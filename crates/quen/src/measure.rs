//! The measurement tools: `measure_presentmon`, one PresentMon window on the game's process, `measure_trace`, one actor
//! trace window, and `measure_ab`, the within-boot A/B/A/B over the in-game actions, with PresentMon, a trace or both in
//! each window. PresentMon's command line, the stats and the verdict, and the trace's metrics and verdicts, are
//! quen-core's. Each CSV goes to `<capture root>\<session>\`, and each window's numbers become ledger rows
//! (`method_family = presentmon`, and `game_state` for a trace's).

use std::fs::{self, File};
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime};

use quen_core::bridge_log::{VALID, in_photo};
use quen_core::journal::sha256_hex;
use quen_core::ledger::{Capture, Ledger, Measurement, Tag};
use quen_core::protocol::{MAX_ACTORS, MAX_ACTORS_RADIUS, Verb};
use quen_core::stats::{self, Stats};
use quen_core::trace::{self, MetricVerdict, Metrics, Reference, Sample, Sampler};
use serde_json::{Map, Value, json};
use windows::Win32::Foundation::HWND;

use crate::game::{self, Action, Ctx, Cvar, Fields, ctx, flag};
use crate::hands::Hands;
use crate::session::{self, Call, Done, Session, last_line, utc_ms};
use crate::win::Job;
use crate::{channel, doctor, input, notice};

/// Images that skew a CPU-bound measurement: builds, and RE analysis (Ghidra's analyzeHeadless
/// runs as java.exe).
const BUSY: [&str; 6] = [
    "cl.exe",
    "link.exe",
    "cargo.exe",
    "rustc.exe",
    "java.exe",
    "wcc_lite.exe",
];
/// How long PresentMon may run past its window before it is killed (`ini_session.py perf`'s timeout).
const PRESENTMON_GRACE: Duration = Duration::from_secs(30);
/// While the user plays: how long measure_presentmon waits for the game to be in front, and how often it looks
/// during the window whether the game is still there.
const FRONT_MAX: Duration = Duration::from_secs(60);
const FRONT_POLL: Duration = Duration::from_millis(250);
/// The runner design's budget for a sampler inside a timing window (decision 45, Q5): at most this much mean and p99
/// frame time added, judged against its measured cost. The trace's cost is unmeasured until the overhead boot.
const BUDGET_MEAN_MS: f64 = 0.05;
const BUDGET_P99_MS: f64 = 0.5;

pub fn tools() -> Vec<Value> {
    let token = json!({"type": "string", "description": "the token session_launch returned"});
    let tag = json!({"type": "string", "description": "names the CSVs: letters, digits, _ and -; new in this session"});
    let trace = json!({
        "type": "object",
        "description": "the actor trace: each sample reads the player (world_get, unless reference_id) and the \
            subjects (world_actors) through the game's channel, one verb after the other, stamped when the subjects' \
            reply came",
        "properties": {
            "hz": {"type": "number", "minimum": 0, "maximum": trace::MAX_HZ, "description": "samples a second, above 0, default 10"},
            "radius": {"type": "number", "minimum": 0, "maximum": MAX_ACTORS_RADIUS, "description": "metres around the player, above 0, default 8"},
            "max": {"type": "integer", "minimum": 1, "maximum": MAX_ACTORS, "description": "subjects per sample, nearest the player first, default 20"},
            "tag": {"type": "string", "description": "only subjects with this tag: 1 to 64 letters, digits and _ (quen-w3 0.1.12)"},
            "class": {"type": "string", "description": "the gameplay entities of this class instead of the alive actors, e.g. CNewNPC (quen-w3 0.1.12)"},
            "reference_id": {"type": "integer", "description": "measure against the subject with this id (world_actors' id, quen-w3 0.1.12) instead of the player"},
            "contact_m": {"type": "number", "minimum": 0.1, "maximum": 3, "description": "a contact's centre distance in metres, default 0.75 (bodies touched at 0.70 in boot 187)"},
        },
        "additionalProperties": false,
    });
    let strict = json!({"type": "boolean", "description": "refuse, rather than warn, while a compiler, cargo, rustc or java runs, or the devkit is loaded in the game, or a profiler other than tracy is attached"});
    let flags = json!({
        "type": "array",
        "items": {"type": "string", "pattern": "^--(no_)?track_[a-z_]+$"},
        "description": "extra PresentMon switches, --track_* or --no_track_* only: e.g. --track_pc_latency (the column \
            that tells real frames from generated ones) or --track_frame_type",
    });
    vec![
        json!({
            "name": "measure_presentmon",
            "description": "One PresentMon window on the game's process (PresentMon 2.x from the config, its \
                installer's folder or PATH, refused with needs without it; presentmon in the result names the \
                exe that ran): the CSV goes to <capture root>\\<session>\\<tag>.csv; returns mean, median and p95 of frame time \
                (MsBetweenPresents), GPU busy, GPU time and CPU busy, and the PresentMode. Sends no input. Each \
                number is a ledger measurement row (method_family presentmon). Quiet machine: a compiler, cargo, \
                rustc or java running, the RenoDX devkit loaded in the game (it doubles frame time) or a profiler \
                attached gives a warning (on the rows' note; what ran goes on the boot's busy_with), or a refusal \
                with strict (the same for measure_ab). While the user has the controls (a play session), the window \
                starts once the game is in front (up to 60 s) and fails if the game leaves the front.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "token": token.clone(),
                    "tag": tag.clone(),
                    "secs": {"type": "integer", "minimum": 1, "maximum": 600, "description": "default 10"},
                    "strict": strict.clone(),
                    "presentmon_flags": flags.clone(),
                },
                "required": ["token", "tag"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "measure_trace",
            "description": "One actor trace window: samples the subjects within radius of the player (the alive \
                actors; with tag or class, those, quen-w3 0.1.12) and the reference (the player, or the subject \
                reference_id) at hz for secs, through the game's channel (quen-w3 0.1.10 or later; changes nothing in \
                the game). The CSV (<tag>-trace.csv: a row per body per sample) goes to <capture root>\\<session>\\ \
                with a captures row (method actor_trace); the window's behaviour metrics (quen_guide: their \
                definitions) are ledger rows (method_family game_state, capture_id the CSV's), and the sampler goes on \
                the boot's samplers. budget: its frame-time cost, unmeasured. Refused, nothing read: a bad tag or \
                trace, no loaded game, the package missing or older (with needs); while the user has the controls. \
                Failed: a read with no reply (a loading screen), or fewer than 2 samples.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "token": token.clone(),
                    "tag": tag.clone(),
                    "secs": {"type": "integer", "minimum": 1, "maximum": 600, "description": "default 30"},
                    "trace": trace.clone(),
                },
                "required": ["token", "tag"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "measure_ab",
            "description": "Within-boot A/B/A/B (quen_guide: a second method). Per window: apply its state's \
                actions, settle, one PresentMon window (PresentMon 2.x; else needs), and with trace an actor trace \
                beside it, the same each window (presentmon false: alone). Every action is checked before any runs. cvar and asi actions run before each of their state's \
                windows; keys and console lines (toggles) only on a switch of state. Window 1 measures the game as it \
                is: with toggles, the first state must be the current one. Both states name the same cvars and ASIs, \
                and both or neither toggle. Returns per window frame time median / mean / p95, GPU and CPU busy, \
                PresentMode, utc; drift per state (the spread of its windows' means), diff = mean(second state) - \
                mean(first), verdict 'outside drift' only if |diff| exceeds every drift; with trace, sampler and \
                behaviour (quen_guide). Another PresentMode than window 1's stops the run. At the \
                end, and on any failure, cvars go back (game_cvar restore), ASI modes that took and toggles to the \
                first state.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "token": token,
                    "tag": tag,
                    "states": {
                        "type": "array", "minItems": 2, "maxItems": 2,
                        "description": "the two states, the first in order measured first",
                        "items": {
                            "type": "object",
                            "properties": {
                                "label": {"type": "string", "description": "letters, digits, _ and -"},
                                "actions": {
                                    "type": "array",
                                    "description": "what puts the game in this state, each {cvar, set}, {asi, set_mode}, {keys} or {console}",
                                    "items": {
                                        "type": "object",
                                        "properties": {
                                            "cvar": {"type": "string", "description": "a cvar, as game_cvar names it (with set)"},
                                            "set": {"type": "string", "description": "the cvar's value: a number, true / false or text"},
                                            "asi": {"type": "string", "description": "an ASI file name whose *_SetMode export is called (with set_mode)"},
                                            "set_mode": {"type": "integer", "minimum": 0, "maximum": 255, "description": "the mode: our ASIs take 0 (off) and 1 (on), even where their ini writes another value for on"},
                                            "export": {"type": "string", "description": "with asi: which *_SetMode export, when it has several"},
                                            "keys": {"type": "array", "items": {"type": "string"}, "description": "a toggle hotkey, as game_keys tokens"},
                                            "console": {"type": "string", "description": "a console line (a toggle): with quen-w3 0.1.13 or later sent as game_exec sends it (no typing; the note says exec), else typed as game_console types it"},
                                            "expect_log": {
                                                "type": "object",
                                                "description": "with console: a new script-log line on channel holding contains must follow within 10 s, else the run fails",
                                                "properties": {
                                                    "channel": {"type": "string", "description": "its [channel]: letters, digits and _"},
                                                    "contains": {"type": "string", "description": "text the line holds"},
                                                },
                                                "required": ["channel", "contains"],
                                                "additionalProperties": false,
                                            },
                                        },
                                        "additionalProperties": false,
                                    },
                                },
                            },
                            "required": ["label", "actions"],
                            "additionalProperties": false,
                        },
                    },
                    "order": {"type": "array", "items": {"type": "string"}, "description": "labels, each at least twice; default first, second, first, second"},
                    "settle": {"type": "integer", "minimum": 0, "maximum": 300, "description": "seconds after the actions, default 10"},
                    "secs": {"type": "integer", "minimum": 1, "maximum": 600, "description": "seconds measured per window, default 15"},
                    "strict": strict,
                    "presentmon_flags": flags,
                    "trace": trace,
                    "presentmon": {"type": "boolean", "description": "false: no PresentMon, the trace alone (needs trace); default true"},
                    "trace_only": {"type": "string", "description": "the trace only in this state's windows, to measure its own frame-time cost (needs trace and PresentMon; no behaviour verdict)"},
                },
                "required": ["token", "tag", "states"],
                "additionalProperties": false,
            },
        }),
    ]
}

pub(crate) fn call(session: &mut Session, call: &mut Call, name: &str, args: &Value) -> Done {
    let out = match name {
        "measure_presentmon" => presentmon(session, call, args),
        "measure_trace" => trace_window(session, call, args),
        "measure_ab" => ab(session, call, args),
        _ => Err(Done::Refused(format!("unknown tool {name:?}"))),
    };
    out.map_or_else(|done| done, Done::Ok)
}

/// A whole number argument in `range`, `default` when absent.
fn whole(args: &Value, key: &str, default: u32, range: RangeInclusive<u32>) -> Result<u32, String> {
    let Some(v) = args.get(key) else {
        return Ok(default);
    };
    v.as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .filter(|n| range.contains(n))
        .ok_or(format!(
            "{key} must be a whole number {}-{}",
            range.start(),
            range.end()
        ))
}

/// A name that goes into a file name.
fn name<'v>(v: &'v Value, what: &str) -> Result<&'v str, String> {
    let s = v.as_str().ok_or(format!("{what} must be a string"))?;
    if s.is_empty()
        || s.len() > 40
        || !s
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-".contains(c))
    {
        return Err(format!("{what} {s:?}: 1-40 letters, digits, _ and -"));
    }
    Ok(s)
}

/// The busy images among `processes`, sorted and once each.
fn busy(processes: &[(u32, String)]) -> Vec<String> {
    let mut busy: Vec<String> = processes
        .iter()
        .map(|(_, exe)| exe.to_lowercase())
        .filter(|exe| BUSY.contains(&exe.as_str()) || exe.starts_with("analyzeheadless"))
        .collect();
    busy.sort();
    busy.dedup();
    busy
}

/// What skews timing in this session itself: the RenoDX devkit loaded in the game (it doubles frame time), a
/// profiler attached (profile.rs; ngfx's GPU Trace also locks the clocks to base).
fn skewed(cx: &Ctx) -> Vec<String> {
    let mut why = Vec::new();
    if cx.devkit_loaded() {
        why.push("the RenoDX devkit is loaded (it doubles frame time)".to_owned());
    }
    // A tracy boot is its own condition: the boot row's profiler marks it, and claims scoped to it
    // compare only numbers from that boot, so it doesn't warn here.
    if let Some(profiler) = cx
        .live
        .profiler
        .as_ref()
        .filter(|p| !p.starts_with("tracy:"))
    {
        why.push(format!("profiler {profiler} is attached"));
    }
    why
}

/// The quiet-machine check: what runs that skews the numbers, and what skews them in the session itself
/// (`skewed`), refused with `strict`.
fn quiet(strict: bool, skewed: Vec<String>) -> Result<Value, String> {
    let running = game::processes().map_err(|e| format!("process list: {e}"))?;
    let busy = busy(&running);
    if strict && (!busy.is_empty() || !skewed.is_empty()) {
        return Err(format!(
            "strict: {busy:?} running skew CPU-bound numbers (quiet machine); in the session: {skewed:?}"
        ));
    }
    let mut warning: Vec<String> = skewed.clone();
    if !busy.is_empty() {
        warning.push(format!(
            "{busy:?} running: builds and RE analysis skew CPU-bound numbers"
        ));
    }
    let warning = (!warning.is_empty()).then(|| warning.join("; "));
    Ok(json!({"busy": busy, "skewed": skewed, "warning": warning}))
}

/// Where this session's CSVs go, beside its screenshots.
fn captures(cx: &Ctx) -> PathBuf {
    cx.config.capture_root().join(&cx.live.id)
}

/// PresentMon, checked before anything runs: its exe (quen_doctor's check: the config's, else on PATH or where its
/// installer puts it; refused with needs otherwise) and the call's extra
/// switches.
struct PresentMon {
    exe: PathBuf,
    flags: Vec<String>,
}

impl PresentMon {
    fn new(cx: &Ctx, args: &Value) -> Result<PresentMon, Done> {
        let flags = match args.get("presentmon_flags") {
            None => Vec::new(),
            Some(v) => v
                .as_array()
                .and_then(|a| a.iter().map(|f| f.as_str().map(String::from)).collect())
                .ok_or("presentmon_flags must be an array of strings".to_string())?,
        };
        stats::presentmon_args("", 0, 1, "", &flags)?;
        let pm = doctor::require(
            &doctor::PRESENTMON,
            cx.config,
            "measure_presentmon, measure_ab",
        )?;
        Ok(PresentMon {
            exe: pm.path.unwrap_or_default(),
            flags,
        })
    }

    /// One window of `secs` on the game's pid, its CSV straight to `dest` and its output in a numbered log, then the
    /// stats, and when quen started PresentMon and saw it exit (UTC, `presentmon_start` / `presentmon_stop`: feature
    /// tests 4 cut a Tracy capture by them). PresentMon runs in a job of its own, killed if it outlives the window by
    /// [`PRESENTMON_GRACE`]. With `front` (the user plays), the game's window must stay in front throughout: Witcher 3
    /// pauses into its menu behind another window, so a window during which it left fails.
    fn window(
        &self,
        cx: &Ctx,
        call: &mut Call,
        dest: &Path,
        secs: u32,
        front: Option<HWND>,
    ) -> Result<(Stats, Fields), Done> {
        notice::clear(cx.live, call).map_err(Done::Refused)?;
        let pid = cx.game_pid()?;
        let exe = self.exe.to_string_lossy();
        let argv = stats::presentmon_args(&exe, pid, secs, &dest.to_string_lossy(), &self.flags)?;
        let (dir, log) = (dest.parent().unwrap_or(dest), cx.log("presentmon"));
        let job = Job::new().map_err(|e| format!("PresentMon: {e}"))?;
        let failed = |why: String| Done::Failed(why, Map::from_iter([("log".into(), json!(log))]));
        // The folder and the log exist from here: a spawn that fails is Failed.
        let start = SystemTime::now();
        let child = fs::create_dir_all(dir)
            .and_then(|()| File::create(&log))
            .and_then(|out| job.spawn(&argv, &[], dir, &out))
            .map_err(|e| failed(format!("PresentMon {exe} did not start: {e}")))?;
        let (max, t, mut left) = (
            Duration::from_secs(secs.into()) + PRESENTMON_GRACE,
            Instant::now(),
            false,
        );
        // Looked at every FRONT_POLL: the front while the user plays, and a cancel, which kills PresentMon (finding 79:
        // a cancelled window ran on and answered ok).
        let code = loop {
            match child.wait(FRONT_POLL.min(max.saturating_sub(t.elapsed()))) {
                Ok(None) if call.stopped() => {
                    let _ = job.kill();
                    let why = format!(
                        "{} {:.1} s into the {secs} s window: PresentMon was killed; no ledger rows, the CSV may be cut",
                        call.stop_cause(),
                        t.elapsed().as_secs_f32()
                    );
                    return Err(failed(why));
                }
                Ok(None) if t.elapsed() < max => {
                    left |= front.is_some_and(|w| !input::in_front(w));
                }
                done => break done,
            }
        };
        let utc = Fields::from_iter([
            ("presentmon_start".into(), json!(utc_ms(start))),
            ("presentmon_stop".into(), json!(utc_ms(SystemTime::now()))),
        ]);
        call.artifact(&log);
        match code {
            Ok(Some(0)) => {}
            Ok(Some(code)) => {
                // PresentMon's words go beside the reason.
                let said = crate::channel::game_text(&last_line(&log).unwrap_or_default());
                let fields = Map::from_iter([
                    ("log".into(), json!(log)),
                    ("presentmon_said".into(), json!(said)),
                ]);
                let why = format!("PresentMon exit {code}: its last line is in presentmon_said");
                return Err(Done::Failed(why, fields));
            }
            Ok(None) => {
                let _ = job.kill();
                let grace = PRESENTMON_GRACE.as_secs();
                return Err(failed(format!(
                    "PresentMon still ran {grace} s after its {secs} s window: killed"
                )));
            }
            Err(e) => return Err(failed(format!("waiting for PresentMon: {e}"))),
        }
        call.artifact(dest);
        if left {
            return Err(failed(
                "the game left the front during the window (it pauses into its menu there): no ledger rows; the CSV \
                 stays"
                    .into(),
            ));
        }
        let text =
            fs::read_to_string(dest).map_err(|e| failed(format!("{}: {e}", dest.display())))?;
        let stats = stats::parse(&text).map_err(|e| failed(format!("{}: {e}", dest.display())))?;
        Ok((stats, utc))
    }
}

/// The run id of quen's rows for `tag`: `<session>-<tag>`, in a play session `<session>-h<n>-<tag>` with n the
/// hand-overs so far.
fn run_id(cx: &Ctx, tag: &str) -> String {
    match cx.live.play {
        true => format!("{}-h{}-{tag}", cx.live.id, cx.live.handovers.takes.get()),
        false => format!("{}-{tag}", cx.live.id),
    }
}

/// What the world did, as quen sees it when the rows are written (schema 0011's `measurements.world`): the user had
/// the controls, photo mode, quen's pause, or else running with the agent's.
pub(crate) fn world(cx: &Ctx) -> &'static str {
    if cx.hands == Some(Hands::User) {
        "user"
    } else if cx.last_flags().is_some_and(in_photo) {
        "photo"
    } else if cx.live.paused.get() {
        "paused"
    } else {
        "live"
    }
}

/// While the user plays, a window starts once the game is in front, waited for up to [`FRONT_MAX`]: Witcher 3
/// pauses into its menu behind another window. The game's window.
fn in_front(cx: &Ctx, call: &Call) -> Result<HWND, Done> {
    let (_, window) = cx.window()?;
    let t = Instant::now();
    while !input::in_front(window) {
        if t.elapsed() > FRONT_MAX || call.stopped() {
            let why = format!(
                "the game wasn't in front within {} s, where it runs while the user plays: nothing measured",
                FRONT_MAX.as_secs()
            );
            return Err(why.into());
        }
        sleep(FRONT_POLL);
    }
    Ok(window)
}

/// One window: its label, PresentMon's stats and CSV (none without PresentMon), its UTC times, and its trace.
struct Window {
    label: String,
    pm: Option<(Stats, PathBuf)>,
    utc: Fields,
    trace: Option<Traced>,
}

/// A window's trace: its metrics, its CSV and the CSV's captures row.
struct Traced {
    metrics: Metrics,
    csv: PathBuf,
    capture_id: Option<i64>,
}

/// A window's numbers for the result.
fn shown(n: usize, w: &Window) -> Value {
    let mut out = json!({"label": w.label, "window": n, "utc": w.utc});
    if let Some((stats, csv)) = &w.pm {
        out["frame_ms"] = json!(stats.frame_ms);
        out["gpu_busy_ms"] = json!(stats.gpu_busy_ms);
        out["gpu_time_ms"] = json!(stats.gpu_time_ms);
        out["cpu_busy_ms"] = json!(stats.cpu_busy_ms);
        out["present_mode"] = json!(stats.present_mode);
        out["csv"] = json!(csv);
    }
    if let Some(t) = &w.trace {
        out["trace"] = json!({"metrics": t.metrics, "csv": t.csv, "capture_id": t.capture_id});
    }
    out
}

/// A path under the capture root as captures.path has it.
fn relative(cx: &Ctx, path: &Path) -> String {
    let root = cx.config.capture_root();
    let rel = path.strip_prefix(&root).unwrap_or(path);
    rel.to_string_lossy().replace('\\', "/")
}

/// One measurement row per metric and window: PresentMon's (mean, median and p95 of frame time, GPU busy, GPU time and
/// CPU busy, in ms, `method_family = presentmon`) and a trace's (trace.rs's metrics, `method_family = game_state`,
/// with the trace CSV's capture_id), tagged M. The run id groups them. The note names the CSV (relative to the capture
/// root, as captures.path), PresentMon's PresentMode or the trace's sampler and samples, `about` (measure_ab's states,
/// verdict and sampler), a trace metric's own verdict, and what the quiet-machine check said, which also goes on the
/// boot's `busy_with` (feature tests 1: neither was in the ledger); the sampler goes on the boot's `samplers`.
fn record(
    cx: &Ctx,
    run: &str,
    windows: &[Window],
    quiet: Option<&Value>,
    about: &str,
    sampler: Option<&Sampler>,
    behaviour: &[MetricVerdict],
) -> Result<Vec<i64>, String> {
    let path = cx.config.state_dir().join("quen.db");
    let mut ledger = Ledger::open(&path).map_err(|e| e.to_string())?;
    if let Some(quiet) = quiet {
        let busy: Vec<String> = quiet["busy"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|b| b.as_str().map(String::from))
            .collect();
        ledger
            .record_quiet_check(cx.live.boot, &busy)
            .map_err(|e| e.to_string())?;
    }
    if let Some(sampler) = sampler {
        ledger
            .record_sampler(cx.live.boot, &sampler.to_string())
            .map_err(|e| e.to_string())?;
    }
    let checked = match quiet.map(|q| q["warning"].as_str()) {
        None => "no quiet-machine check".into(),
        Some(None) => "a quiet machine".into(),
        Some(Some(w)) => format!("warned: {w}"),
    };
    let world = world(cx);
    let mut ids = Vec::new();
    let mut add =
        |metric: String, value, unit: &str, label: &str, family: &str, capture_id, note| {
            let row = Measurement {
                boot_id: Some(cx.live.boot),
                capture_id,
                metric,
                value: Some(value),
                unit: Some(unit.into()),
                state_label: Some(label.into()),
                method_family: family.into(),
                tag: Tag::M,
                run_id: Some(run.into()),
                note: Some(note),
                extraction_id: None,
                world: Some(world.into()),
            };
            ids.push(ledger.add_measurement(&row).map_err(|e| e.to_string())?);
            Ok::<_, String>(())
        };
    for w in windows {
        if let Some((stats, csv)) = &w.pm {
            let metrics = [
                ("frame_ms", Some(&stats.frame_ms)),
                ("gpu_busy_ms", stats.gpu_busy_ms.as_ref()),
                ("gpu_time_ms", stats.gpu_time_ms.as_ref()),
                ("cpu_busy_ms", stats.cpu_busy_ms.as_ref()),
            ];
            let mode = stats.present_mode.as_deref().unwrap_or("no PresentMode");
            let note = format!("{}; {mode}; {about}{checked}", relative(cx, csv));
            for (metric, summary) in metrics {
                let Some(s) = summary else { continue };
                for (stat, value) in [("mean", s.mean), ("median", s.median), ("p95", s.p95)] {
                    let metric = format!("{metric}_{stat}");
                    add(
                        metric,
                        value,
                        "ms",
                        &w.label,
                        "presentmon",
                        None,
                        note.clone(),
                    )?;
                }
            }
        }
        let (Some(t), Some(sampler)) = (&w.trace, sampler) else {
            continue;
        };
        let m = &t.metrics;
        let sampled = format!(
            "{}; {sampler}; {} samples over {:.1} s",
            relative(cx, &t.csv),
            m.samples,
            m.seconds
        );
        for (metric, unit, _) in trace::METRICS {
            let Some(value) = m.get(metric) else { continue };
            let verdict =
                behaviour
                    .iter()
                    .find(|v| v.metric == metric)
                    .map_or(String::new(), |v| {
                        let v = json!({"verdict": v.verdict, "diff": v.diff, "drift": v.drift});
                        format!("{metric} verdict {v}; ")
                    });
            // The metric's own verdict first, before the run's (finding 85).
            let floor = match (metric, m.push_floor) {
                ("push_m", Some(from)) => format!("jitter floor from {from}; "),
                _ => String::new(),
            };
            let note = format!("{sampled}; {verdict}{floor}{about}{checked}");
            add(
                metric.into(),
                value,
                unit,
                &w.label,
                "game_state",
                t.capture_id,
                note,
            )?;
        }
    }
    Ok(ids)
}

/// Adds the ledger ids (or why there are none) to `fields`.
fn ledger(fields: &mut Fields, recorded: Result<Vec<i64>, String>) {
    match recorded {
        Ok(ids) => fields.insert("ledger".into(), json!({"measurements": ids})),
        Err(why) => fields.insert("ledger_error".into(), json!(why)),
    };
}

fn presentmon(session: &Session, call: &mut Call, args: &Value) -> Result<Fields, Done> {
    let cx = ctx(
        session,
        args,
        &["tag", "secs", "strict", "presentmon_flags"],
    )?;
    let tag = name(&args["tag"], "tag")?;
    let secs = whole(args, "secs", 10, 1..=600)?;
    let dest = captures(&cx).join(format!("{tag}.csv"));
    if dest.exists() {
        return Err(format!("{} exists: pick a tag new in this session", dest.display()).into());
    }
    let pm = PresentMon::new(&cx, args)?;
    let quiet = quiet(flag(args, "strict")?, skewed(&cx))?;
    call.lap("check");
    let notice = notice::measuring(&cx, call, secs.into(), &mut None);
    let front = match cx.hands {
        Some(Hands::User) => Some(in_front(&cx, call)?),
        _ => None,
    };
    let (stats, utc) = pm.window(&cx, call, &dest, secs, front)?;
    call.lap("presentmon");
    let window = Window {
        label: tag.into(),
        pm: Some((stats, dest)),
        utc,
        trace: None,
    };
    let mut fields = Map::new();
    fields.insert("presentmon".into(), json!(pm.exe));
    fields.insert("window".into(), shown(1, &window));
    if let Some(notice) = notice {
        fields.insert("notice".into(), json!(notice));
    }
    ledger(
        &mut fields,
        record(
            &cx,
            &run_id(&cx, tag),
            &[window],
            Some(&quiet),
            "",
            None,
            &[],
        ),
    );
    fields.insert("quiet".into(), quiet);
    Ok(fields)
}

/// A trace's checks before anything is read: quen's package at the version the sampler needs, a loaded game in quen's
/// state; then the pid that serves the channel.
fn trace_ready(cx: &Ctx, sampler: &Sampler, tool: &str) -> Result<u32, Done> {
    doctor::installed(&[&doctor::PACKAGE], cx.config, tool)?;
    doctor::package_at_least(cx.config, doctor::BEFORE_ITEMS, tool, "0.1.10")?;
    if sampler.needs_ids() {
        let what = format!("{tool} with tag, class or reference_id");
        doctor::package_at_least(cx.config, doctor::BEFORE_ENTITIES, &what, "0.1.12")?;
    }
    let last = cx.last_flags();
    if last.is_none_or(|f| f & VALID == 0) {
        let why = format!(
            "quen's state shows {}: a trace needs a loaded game. Nothing read",
            game::shown(last)
        );
        return Err(why.into());
    }
    Ok(cx.channel_server()?)
}

/// What a sampler run gave: its samples, why it stopped early (a read failed, or a stop), and when it started and
/// ended (UTC `trace_start`, `trace_stop`).
struct Sampled {
    samples: Vec<Sample>,
    stopped: Option<String>,
    utc: Fields,
}

/// Samples `sampler` for `secs` from now, one sample every 1 / hz while time allows (a late one runs at once), until the
/// window's end or `stop`: the player (world.get, with the player as reference), then the subjects (world.actors), each
/// on a connection of its own to `channel`, served by `server`. Takes nothing but these, so it runs on a thread of its
/// own beside PresentMon.
fn sample(
    channel: &str,
    server: u32,
    sampler: &Sampler,
    secs: u32,
    stop: &dyn Fn() -> bool,
) -> Sampled {
    let (start, at) = (Instant::now(), SystemTime::now());
    let (every, end) = (
        Duration::from_secs_f64(1.0 / sampler.hz),
        Duration::from_secs(secs.into()),
    );
    let mut samples = Vec::new();
    let mut stopped = None;
    for n in 0u32.. {
        let due = every * n;
        if due > end {
            break;
        }
        if stop() {
            stopped = Some(format!(
                "stopped {:.1} s into the window",
                start.elapsed().as_secs_f32()
            ));
            break;
        }
        sleep(due.saturating_sub(start.elapsed()));
        let world = match sampler.reference {
            Reference::Player => match channel::read(channel, server, Verb::WorldGet {}) {
                Ok(world) => Some(world),
                Err(why) => {
                    stopped = Some(format!("world.get: {why}"));
                    break;
                }
            },
            Reference::Id(_) => None,
        };
        let verb = Verb::WorldActors {
            radius: sampler.radius as f32,
            max: sampler.max,
            tag: sampler.tag.clone(),
            class: sampler.class.clone(),
        };
        let read = channel::read(channel, server, verb)
            .map_err(|why| format!("world.actors: {why}"))
            .and_then(|actors| {
                let t = start.elapsed().as_secs_f64();
                Sample::new(t, &actors, world.as_ref(), &sampler.reference)
                    .map_err(|why| format!("the game's read-back: {why}"))
            });
        match read {
            Ok(s) => samples.push(s),
            Err(why) => {
                stopped = Some(why);
                break;
            }
        }
    }
    let utc = Fields::from_iter([
        ("trace_start".into(), json!(utc_ms(at))),
        ("trace_stop".into(), json!(utc_ms(SystemTime::now()))),
    ]);
    Sampled {
        samples,
        stopped,
        utc,
    }
}

/// What one window measured: PresentMon's stats and UTC times, and the trace's samples, each when it ran.
type Measured = (Option<Result<(Stats, Fields), Done>>, Option<Sampled>);

/// One window of `secs`: PresentMon on `pm_csv` with `pm`, and `trace`'s sampler beside it on a thread of its own, or
/// either alone. A failed PresentMon stops the trace; so does a cancel, looked at every 0.1 s.
fn measure(
    cx: &Ctx,
    call: &mut Call,
    pm: Option<(&PresentMon, &Path)>,
    trace: Option<(&Sampler, u32)>,
    secs: u32,
    front: Option<HWND>,
) -> Measured {
    let stop = AtomicBool::new(false);
    let channel = &cx.live.channel;
    std::thread::scope(|s| {
        let tracer = trace.map(|(sampler, server)| {
            let stop = &stop;
            s.spawn(move || {
                sample(channel, server, sampler, secs, &|| {
                    stop.load(Ordering::Relaxed)
                })
            })
        });
        let measured = pm.map(|(pm, csv)| pm.window(cx, call, csv, secs, front));
        if measured.as_ref().is_some_and(Result::is_err) {
            stop.store(true, Ordering::Relaxed);
        }
        let sampled = tracer.map(|h| {
            while !h.is_finished() {
                if call.stopped() {
                    stop.store(true, Ordering::Relaxed);
                }
                sleep(Duration::from_millis(100));
            }
            h.join().unwrap_or_else(|_| Sampled {
                samples: Vec::new(),
                stopped: Some("the sampler panicked".into()),
                utc: Fields::new(),
            })
        });
        (measured, sampled)
    })
}

/// A finished sampler run: its metrics, its CSV `<name>-trace.csv` in the session's capture folder and the CSV's
/// captures row (method actor_trace). Err: it stopped early or has too few samples; nothing written.
fn traced(
    cx: &Ctx,
    call: &mut Call,
    sampled: &Sampled,
    sampler: &Sampler,
    name: &str,
) -> Result<Traced, String> {
    if let Some(why) = &sampled.stopped {
        return Err(format!(
            "trace: {why}, after {} samples",
            sampled.samples.len()
        ));
    }
    let metrics =
        trace::metrics(&sampled.samples, sampler.contact).map_err(|why| format!("trace: {why}"))?;
    let csv = captures(cx).join(format!("{name}-trace.csv"));
    let text = trace::csv(&sampled.samples);
    fs::create_dir_all(captures(cx))
        .and_then(|()| fs::write(&csv, &text))
        .map_err(|e| format!("{}: {e}", csv.display()))?;
    call.artifact(&csv);
    let row = Capture {
        boot_id: Some(cx.live.boot),
        path: relative(cx, &csv),
        sha256: sha256_hex(text.as_bytes()),
        bytes: text.len() as i64,
        kind: "csv".into(),
        method: Some("actor_trace".into()),
        t: session::utc(SystemTime::now()),
        ..Capture::default()
    };
    let capture_id = session::open_ledger(cx.config)
        .and_then(|mut l| l.add_capture(&row).map_err(session::ledger_error))
        .ok();
    if let Some(id) = capture_id {
        call.row("captures", id);
    }
    Ok(Traced {
        metrics,
        csv,
        capture_id,
    })
}

/// The performance budget of a run with `sampler` (the runner design's §4.4): its cost is unmeasured until the
/// overhead boot, so whether it stays within the cap is unknown; `crowd_mean` per label bounds its bias once it is.
fn budget(sampler: &Sampler, windows: &[Window]) -> Value {
    let mut crowd: Map<String, Value> = Map::new();
    for w in windows {
        let means: Vec<f64> = windows
            .iter()
            .filter(|o| o.label == w.label)
            .filter_map(|o| o.trace.as_ref().map(|t| t.metrics.crowd_mean))
            .collect();
        if !means.is_empty() {
            crowd.insert(
                w.label.clone(),
                json!(means.iter().sum::<f64>() / means.len() as f64),
            );
        }
    }
    json!({
        "sampler": sampler.to_string(), "cost_ms_per_frame": "unmeasured", "evidence": null, "within": "unknown",
        "cap": {"mean_ms": BUDGET_MEAN_MS, "p99_ms": BUDGET_P99_MS, "hz": trace::MAX_HZ, "radius_m": MAX_ACTORS_RADIUS,
                "max": MAX_ACTORS},
        "crowd_mean": crowd,
    })
}

/// measure_trace: one trace window, `secs` long.
fn trace_window(session: &Session, call: &mut Call, args: &Value) -> Result<Fields, Done> {
    let cx = ctx(session, args, &["tag", "secs", "trace"])?;
    let tag = name(&args["tag"], "tag")?;
    let secs = whole(args, "secs", 30, 1..=600)?;
    let sampler = Sampler::parse(
        args.get("trace").unwrap_or(&json!({})),
        f64::from(MAX_ACTORS_RADIUS),
        MAX_ACTORS,
    )?;
    let csv = captures(&cx).join(format!("{tag}-trace.csv"));
    if csv.exists() {
        return Err(format!("{} exists: pick a tag new in this session", csv.display()).into());
    }
    let server = trace_ready(&cx, &sampler, "measure_trace")?;
    call.lap("check");
    let (_, sampled) = measure(&cx, call, None, Some((&sampler, server)), secs, None);
    let sampled = sampled.expect("a trace ran");
    call.lap("trace");
    let mut fields = Map::new();
    fields.insert("utc".into(), json!(sampled.utc));
    fields.insert("samples".into(), json!(sampled.samples.len()));
    let traced = traced(&cx, call, &sampled, &sampler, tag)
        .map_err(|why| Done::Failed(why, fields.clone()))?;
    let window = Window {
        label: tag.into(),
        pm: None,
        utc: sampled.utc,
        trace: Some(traced),
    };
    fields.insert("sampler".into(), json!(sampler.to_string()));
    fields.insert("window".into(), shown(1, &window));
    fields.insert(
        "budget".into(),
        budget(&sampler, std::slice::from_ref(&window)),
    );
    ledger(
        &mut fields,
        record(
            &cx,
            &run_id(&cx, tag),
            &[window],
            None,
            "",
            Some(&sampler),
            &[],
        ),
    );
    Ok(fields)
}

/// A checked measure_ab request.
struct Run {
    tag: String,
    /// The two states: label and actions.
    states: Vec<(String, Vec<Action>)>,
    /// Indices into `states`, one per window.
    order: Vec<usize>,
    settle: Duration,
    secs: u32,
    /// The trace in every window (or only in `trace_only`'s state's), if any; and whether PresentMon runs (else the
    /// trace alone).
    sampler: Option<Sampler>,
    trace_only: Option<usize>,
    presentmon: bool,
}

impl Run {
    fn parse(args: &Value) -> Result<Run, String> {
        let tag = name(&args["tag"], "tag")?.to_string();
        let mut states = Vec::new();
        for state in args["states"]
            .as_array()
            .ok_or("states must be an array of two")?
        {
            let map = state.as_object().ok_or("a state is {label, actions}")?;
            if let Some(key) = map
                .keys()
                .find(|k| !["label", "actions"].contains(&k.as_str()))
            {
                return Err(format!("a state is {{label, actions}}, not {key:?}"));
            }
            let label = name(&state["label"], "label")?.to_string();
            let actions = state["actions"]
                .as_array()
                .ok_or("actions must be an array")?;
            let actions = actions
                .iter()
                .map(Action::parse)
                .collect::<Result<Vec<_>, _>>()?;
            states.push((label, actions));
        }
        let [(a, a_actions), (b, b_actions)] = &states[..] else {
            return Err(format!("{} states: measure_ab compares two", states.len()));
        };
        if a == b {
            return Err(format!("both states are labelled {a:?}"));
        }
        // Both states set the same cvars and ASI modes, and both or neither toggle: else a window would run with the
        // other state's leftovers.
        let names = |actions: &[Action]| {
            let mut names: Vec<String> = actions
                .iter()
                .filter_map(|action| match action {
                    Action::Cvar { name, .. } => Some(format!("cvar {name}")),
                    Action::SetMode { asi, export, .. } => Some(format!(
                        "asi {}!{}",
                        asi.to_lowercase(),
                        export.as_deref().unwrap_or("*_SetMode")
                    )),
                    Action::Keys(_) | Action::Console { .. } => None,
                })
                .collect();
            names.sort();
            names
        };
        if names(a_actions) != names(b_actions) {
            return Err(format!(
                "the states set different cvars or ASIs ({a}: {:?}, {b}: {:?}): each must set what the other does",
                names(a_actions),
                names(b_actions)
            ));
        }
        let toggles = |actions: &[Action]| actions.iter().any(Action::toggles);
        if toggles(a_actions) != toggles(b_actions) {
            return Err(
                "keys and console lines toggle, so both states need them (what switches into each) or neither"
                    .into(),
            );
        }
        let order = match args.get("order") {
            None => vec![0, 1, 0, 1],
            Some(order) => order
                .as_array()
                .ok_or("order must be an array of labels")?
                .iter()
                .map(|l| {
                    let l = l.as_str().unwrap_or_default();
                    states
                        .iter()
                        .position(|(label, _)| label == l)
                        .ok_or(format!("order: no state {l:?}"))
                })
                .collect::<Result<Vec<_>, _>>()?,
        };
        if order.len() > 12 || (0..2).any(|s| order.iter().filter(|&&o| o == s).count() < 2) {
            return Err(
                "order: each state at least twice (drift needs two windows), 12 windows at most"
                    .into(),
            );
        }
        let sampler = args
            .get("trace")
            .map(|t| Sampler::parse(t, f64::from(MAX_ACTORS_RADIUS), MAX_ACTORS))
            .transpose()?;
        let presentmon = args.get("presentmon").is_none_or(|p| p != false);
        if !presentmon && (sampler.is_none() || args.get("presentmon_flags").is_some()) {
            return Err("presentmon false measures the trace alone: it needs trace, and takes no presentmon_flags".into());
        }
        let trace_only = match args.get("trace_only") {
            None => None,
            Some(_) if sampler.is_none() || !presentmon => {
                return Err("trace_only needs trace and PresentMon: it measures the trace's frame-time cost".into());
            }
            Some(l) => Some(
                states
                    .iter()
                    .position(|(label, _)| Some(label.as_str()) == l.as_str())
                    .ok_or(format!("trace_only: no state {l}"))?,
            ),
        };
        Ok(Run {
            tag,
            order,
            states,
            settle: Duration::from_secs(whole(args, "settle", 10, 0..=300)?.into()),
            secs: whole(args, "secs", 15, 1..=600)?,
            sampler,
            trace_only,
            presentmon,
        })
    }

    /// The sampler window `i` (0-based) runs, if any.
    fn traced(&self, i: usize) -> Option<&Sampler> {
        let only = self.trace_only.is_none_or(|s| s == self.order[i]);
        self.sampler.as_ref().filter(|_| only)
    }

    /// Window `i`'s name (0-based): `<tag>_<n><label>`, its PresentMon CSV `<name>.csv`, its trace `<name>-trace.csv`.
    fn window(&self, i: usize) -> String {
        format!("{}_{}{}", self.tag, i + 1, self.states[self.order[i]].0)
    }

    /// The CSV of window `i` (0-based).
    fn csv(&self, cx: &Ctx, i: usize) -> PathBuf {
        captures(cx).join(format!("{}.csv", self.window(i)))
    }

    /// The files window `i` writes.
    fn files(&self, cx: &Ctx, i: usize) -> Vec<PathBuf> {
        let trace = captures(cx).join(format!("{}-trace.csv", self.window(i)));
        let pm = self.csv(cx, i);
        [(self.presentmon, pm), (self.traced(i).is_some(), trace)]
            .into_iter()
            .filter_map(|(on, f)| on.then_some(f))
            .collect()
    }
}

/// A result field as plain text.
fn text(v: &Value) -> String {
    v.as_str().map_or_else(|| v.to_string(), str::to_owned)
}

/// Why a step failed.
fn why(done: Done) -> String {
    match done {
        Done::Refused(why)
        | Done::RefusedWith(why, _)
        | Done::Needs(why, _)
        | Done::Failed(why, _) => why,
        Done::Ok(_) => String::new(),
    }
}

/// Sleeps `d`, reporting progress; false if the call was stopped meanwhile.
pub(crate) fn settle(call: &Call, d: Duration, what: &str) -> bool {
    let start = Instant::now();
    let mut said = 0;
    while start.elapsed() < d {
        if call.stopped() {
            return false;
        }
        let secs = start.elapsed().as_secs();
        if secs >= said + 5 {
            said = secs;
            call.progress(&format!("{what}: settling, {secs} s"));
        }
        sleep((d - start.elapsed()).min(Duration::from_millis(250)));
    }
    true
}

fn ab(session: &Session, call: &mut Call, args: &Value) -> Result<Fields, Done> {
    let cx = ctx(
        session,
        args,
        &[
            "tag",
            "states",
            "order",
            "settle",
            "secs",
            "strict",
            "presentmon_flags",
            "trace",
            "presentmon",
            "trace_only",
        ],
    )?;
    let mut run = Run::parse(args)?;
    let pm = match run.presentmon {
        true => Some(PresentMon::new(&cx, args)?),
        false => None,
    };
    if let Some(csv) = (0..run.order.len())
        .flat_map(|i| run.files(&cx, i))
        .find(|p| p.exists())
    {
        return Err(format!("{} exists: pick a tag new in this session", csv.display()).into());
    }
    let quiet = quiet(flag(args, "strict")?, skewed(&cx))?;
    if cx.live.job.pids().unwrap_or_default().is_empty() {
        return Err("the game isn't running in the session's job"
            .to_string()
            .into());
    }
    let server = match &run.sampler {
        Some(sampler) => Some(trace_ready(&cx, sampler, "measure_ab trace")?),
        None => None,
    };
    // Each action checked before any runs (feature tests 1: five *_SetMode exports failed at window 1): the
    // ASI's export, the cvar's set without its write, the keys' plan and RenderDoc's capture key. A console line goes as
    // an exec verb when the package has it and the line reads as one (game_exec), else typed.
    let exec = doctor::has_exec(doctor::package_version(cx.config));
    for (_, actions) in &mut run.states {
        for action in actions.iter_mut() {
            match action {
                Action::SetMode { asi, export, .. } => {
                    cx.mode_export(asi, export.as_deref())?;
                }
                Action::Cvar { name, value } => {
                    cx.cvar(call, name, Cvar::Check(value))?;
                }
                Action::Keys(tokens) => {
                    crate::profile::capture_key(cx.live, tokens)?;
                    cx.keys(call, tokens, true, game::Focus::Refuse)?;
                }
                Action::Console {
                    line,
                    expect,
                    exec: via,
                } => {
                    if expect.is_some() {
                        cx.script_log()?;
                    }
                    *via = exec && quen_core::protocol::Verb::exec(line).is_ok();
                }
            }
        }
    }
    call.lap("check");
    // About as long as the tool's description says a run takes.
    let secs = run.order.len() as u64 * (run.settle.as_secs() + u64::from(run.secs) + 5);
    let notice = notice::measuring(&cx, call, secs, &mut None);
    // From the first action on, every exit path restores (step 4).
    let mut windows = Vec::new();
    let mut entered = run.order[0];
    let mut accepted = Vec::new();
    let failed = windows_of(
        &cx,
        call,
        &run,
        pm.as_ref(),
        server,
        &mut windows,
        &mut entered,
        &mut accepted,
    )
    .err();
    let restored = restore(&cx, call, &run, entered, &accepted);
    call.lap("restore");

    let mut fields = Map::new();
    if let Some(pm) = &pm {
        fields.insert("presentmon".into(), json!(pm.exe));
    }
    if let Some(notice) = notice {
        fields.insert("notice".into(), json!(notice));
    }
    let shown: Vec<Value> = windows
        .iter()
        .enumerate()
        .map(|(i, window)| shown(i + 1, window))
        .collect();
    fields.insert("states".into(), json!(shown));
    let unrestored: Vec<String> = restored
        .iter()
        .filter(|r| r["ok"] != true)
        .map(|r| format!("{}: {}", text(&r["what"]), text(&r["why"])))
        .collect();
    let restore_note =
        (!unrestored.is_empty()).then(|| format!("restoring failed: {}", unrestored.join("; ")));
    fields.insert("restored".into(), json!(restored));
    let labelled: Vec<(&str, Stats)> = windows
        .iter()
        .filter_map(|w| w.pm.as_ref().map(|(s, _)| (w.label.as_str(), s.clone())))
        .collect();
    let verdict = (failed.is_none() && run.presentmon).then(|| stats::ab_verdict(&labelled));
    let traces: Vec<(&str, &Metrics)> = windows
        .iter()
        .filter_map(|w| w.trace.as_ref().map(|t| (w.label.as_str(), &t.metrics)))
        .collect();
    let behaviour = match failed.is_none() && run.sampler.is_some() && run.trace_only.is_none() {
        true => trace::verdicts(&traces),
        false => Vec::new(),
    };
    // The run on each of its rows: what each state did and the verdict (feature tests 1: a later agent had to guess).
    let mut about: Vec<String> = run
        .states
        .iter()
        .map(|(label, actions)| {
            let did: Vec<String> = actions.iter().map(Action::to_string).collect();
            format!("{label}: {}", did.join(", "))
        })
        .collect();
    if let Some(Ok(v)) = &verdict {
        let v = json!({"verdict": v.verdict, "diff_ms": v.diff_ms, "drift_ms": v.drift_ms});
        about.push(format!("verdict {v}"));
    }
    // The runner design's §4.2: a frame-time verdict with a sampler in its windows is scoped by it.
    let sampled = run.sampler.as_ref().map(|s| match run.trace_only {
        None => format!("sampler {s} in every window: its cost unmeasured"),
        Some(only) => format!("sampler {s} in the {} windows only", run.states[only].0),
    });
    about.extend(sampled.clone());
    let about = format!("{}; ", about.join("; "));
    let run_id = run_id(&cx, &run.tag);
    let sampler = run.sampler.as_ref();
    ledger(
        &mut fields,
        record(
            &cx,
            &run_id,
            &windows,
            Some(&quiet),
            &about,
            sampler,
            &behaviour,
        ),
    );
    fields.insert("quiet".into(), quiet);
    if let Some(sampler) = sampler {
        if run.trace_only.is_none() {
            fields.insert("behaviour".into(), json!(behaviour));
        }
        fields.insert("budget".into(), budget(sampler, &windows));
    }
    if let Some(why) = failed {
        let why = restore_note.map_or(why.clone(), |note| format!("{why}; {note}"));
        return Err(Done::Failed(why, fields));
    }
    match verdict {
        Some(Ok(verdict)) => {
            fields.extend(json!(verdict).as_object().cloned().unwrap_or_default());
            fields.extend(sampled.map(|s| ("sampler".to_string(), json!(s))));
        }
        Some(Err(e)) => return Err(Done::Failed(e.to_string(), fields)),
        None => {}
    }
    match restore_note {
        Some(why) => Err(Done::Failed(format!("measured, but {why}"), fields)),
        None => Ok(fields),
    }
}

/// An ASI's mode switch, whatever the mode: what `accepted` lists.
fn switch_of(asi: &str, export: Option<&str>) -> String {
    format!("{}!{}", asi.to_lowercase(), export.unwrap_or("*_SetMode"))
}

/// Runs the windows in order, pushing each one's label, stats and CSV, trace, and UTC times: when its actions began,
/// its settle ended, PresentMon's (`PresentMon::window`) and the trace's (`sample`). `entered` follows the state the
/// toggles are in; `accepted` gets each ASI whose mode switch took ([`switch_of`]).
#[allow(clippy::too_many_arguments)]
fn windows_of(
    cx: &Ctx,
    call: &mut Call,
    run: &Run,
    pm: Option<&PresentMon>,
    server: Option<u32>,
    windows: &mut Vec<Window>,
    entered: &mut usize,
    accepted: &mut Vec<String>,
) -> Result<(), String> {
    for (i, &s) in run.order.iter().enumerate() {
        let (label, actions) = &run.states[s];
        let at = format!("window {} ({label})", i + 1);
        if call.stopped() {
            return Err(format!(
                "stopped before {at} (a cancel, or quen shutting down)"
            ));
        }
        // Toggles run only on a switch; the first window measures the game as it is.
        let switch = i > 0 && *entered != s;
        let apply = SystemTime::now();
        for action in actions.iter().filter(|a| switch || !a.toggles()) {
            action
                .run(cx, call)
                .map_err(|d| format!("{at}: {}", why(d)))?;
            if let Action::SetMode { asi, export, .. } = action {
                accepted.push(switch_of(asi, export.as_deref()));
            }
        }
        *entered = s;
        call.lap(&format!("{}{label}_apply", i + 1));
        if !settle(call, run.settle, &at) {
            return Err(format!("stopped in {at} (a cancel, or quen shutting down)"));
        }
        call.lap(&format!("{}{label}_settle", i + 1));
        let mut utc = Fields::from_iter([
            ("apply".into(), json!(utc_ms(apply))),
            ("settled".into(), json!(utc_ms(SystemTime::now()))),
        ]);
        let what = match (pm.is_some(), run.traced(i).is_some()) {
            (true, true) => "PresentMon and the trace",
            (true, false) => "PresentMon",
            _ => "the trace",
        };
        call.progress(&format!("{at}: {what} {} s", run.secs));
        let csv = run.csv(cx, i);
        let trace = run.traced(i).zip(server);
        let pm_csv = pm.map(|pm| (pm, csv.as_path()));
        let (measured, sampled) = measure(cx, call, pm_csv, trace, run.secs, None);
        call.lap(&format!("{}{label}_measure", i + 1));
        let pm = measured
            .map(|m| m.map_err(|d| format!("{at}: {}", why(d))))
            .transpose()?;
        let traced = match sampled.as_ref().zip(run.traced(i)) {
            Some((sampled, sampler)) => {
                utc.extend(sampled.utc.clone());
                let name = run.window(i);
                Some(
                    traced(cx, call, sampled, sampler, &name)
                        .map_err(|why| format!("{at}: {why}"))?,
                )
            }
            None => None,
        };
        let pm = pm.map(|(stats, presentmon)| {
            utc.extend(presentmon);
            (stats, csv)
        });
        let mode = pm
            .as_ref()
            .map(|(s, _)| (s.present_mode.clone(), s.present_mode_mixed));
        windows.push(Window {
            label: label.clone(),
            pm,
            utc,
            trace: traced,
        });
        let first = windows[0].pm.as_ref().map(|(s, _)| &s.present_mode);
        if let (Some((mode, mixed)), Some(first)) = (mode, first)
            && (mode.is_none() || mixed || *first != mode)
        {
            let mixed = if mixed { " (mixed)" } else { "" };
            return Err(format!(
                "{at} ran in PresentMode {mode:?}{mixed}, window 1 in {first:?}: the run stopped"
            ));
        }
        if cx.live.job.pids().unwrap_or_default().is_empty() {
            return Err(format!("the game exited during {at}"));
        }
    }
    Ok(())
}

/// Step 4: every cvar the states name goes back to its value before the session's first write (game_cvar restore);
/// ASI modes go back to the first state, and so do the toggles if the run left them in the other. An ASI that accepted
/// no mode in the run changed nothing: nothing to restore (feature tests 2b: the rejected call was retried). Each result
/// is listed; a failure doesn't stop the rest.
fn restore(
    cx: &Ctx,
    call: &mut Call,
    run: &Run,
    entered: usize,
    accepted: &[String],
) -> Vec<Value> {
    let first = run.order[0];
    let mut out = Vec::new();
    for action in &run.states[first].1 {
        if let Action::SetMode { asi, export, .. } = action
            && !accepted.contains(&switch_of(asi, export.as_deref()))
        {
            let skipped = "nothing to restore: the ASI accepted no mode in this run";
            out.push(json!({"what": action.to_string(), "ok": true, "skipped": skipped}));
            continue;
        }
        let (what, done) = match action {
            Action::Cvar { name, .. } => (
                format!("cvar {name} back"),
                cx.cvar(call, name, Cvar::Restore),
            ),
            Action::SetMode { asi, export, mode } => (
                action.to_string(),
                cx.set_mode(asi, export.as_deref(), *mode),
            ),
            Action::Keys(tokens) if entered != first => (
                format!("keys {tokens:?}"),
                cx.keys(call, tokens, false, game::Focus::Refuse),
            ),
            Action::Console { .. } if entered != first => {
                (action.to_string(), action.run(cx, call))
            }
            Action::Keys(_) | Action::Console { .. } => continue,
        };
        out.push(match done {
            Ok(_) => json!({"what": what, "ok": true}),
            Err(d) => json!({"what": what, "ok": false, "why": why(d)}),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::tests::{fake_game, fake_game_until, reply, served, token};
    use crate::game::tests::Fake;
    use quen_core::protocol::Request;
    use std::sync::Arc;

    /// The fake PresentMon, compiled by the tests: appends `presentmon <CSV file name> <secs>` to calls.txt beside it and
    /// writes its argv to argv.txt. Then it exits with the code in exit.txt, writing nothing, or writes the CSV: 5 warm-up
    /// rows and frames whose time is the dummy game's cvar as the journal's last write left it (20 ms, its default, before
    /// any), in the PresentMode of mode.txt (default Independent Flip). A mode_after.txt becomes mode.txt for the next window.
    /// With sleep.txt it first sleeps that many ms.
    const FAKE_PRESENTMON: &str = r#"
use std::{env, fs, io::Write, path::Path, process};
fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let here = env::current_exe().unwrap().parent().unwrap().to_path_buf();
    let read = |f: &str| fs::read_to_string(here.join(f)).ok();
    let arg = |k: &str| args[args.iter().position(|a| a == k).unwrap() + 1].clone();
    let out = arg("--output_file");
    let csv = Path::new(&out).file_name().unwrap().to_string_lossy().into_owned();
    let mut calls = fs::OpenOptions::new().create(true).append(true).open(here.join("calls.txt")).unwrap();
    writeln!(calls, "presentmon {csv} {}", arg("--timed")).unwrap();
    fs::write(here.join("argv.txt"), args.join("\n")).unwrap();
    if let Some(ms) = read("sleep.txt") {
        std::thread::sleep(std::time::Duration::from_millis(ms.trim().parse().unwrap()));
    }
    if let Some(code) = read("exit.txt") {
        eprintln!("error: the fake failed");
        process::exit(code.trim().parse().unwrap());
    }
    let journal = read("../data/state/journal.jsonl").unwrap_or_default();
    let last = journal.lines().filter(|l| l.contains("game_memory")).last().unwrap_or("");
    let v: f64 = last.split(" set ").nth(1).and_then(|r| r.split(" at ").next()).unwrap_or("20").parse().unwrap();
    let mode = read("mode.txt").unwrap_or("Hardware: Independent Flip".into());
    let mut text = String::from("Application,ProcessID,PresentMode,MsBetweenPresents,MsGPUBusy,MsGPUTime,MsCPUBusy\n");
    for d in [99.0, 99.0, 99.0, 99.0, 99.0, -0.5, 0.5, 0.0, 0.0] {
        text += &format!("game.exe,1,{mode},{},{},{},{}\n", v + d, v - 2.0, v - 1.0, v / 2.0);
    }
    fs::write(out, text).unwrap();
    if let Some(next) = read("mode_after.txt") {
        fs::write(here.join("mode.txt"), next).unwrap();
    }
}
"#;

    /// Builds the fake PresentMon where the fixture's config looks for it, beside the fake scripts.
    fn fake_presentmon(fx: &Fake) {
        let src = fx.dir.join("fake_presentmon.rs");
        fs::write(&src, FAKE_PRESENTMON).unwrap();
        let out = std::process::Command::new("rustc")
            .args(["--edition", "2021", "-C", "debuginfo=0", "-o"])
            .arg(fx.dir.join("harness").join("PresentMon.exe"))
            .arg(&src)
            .output()
            .expect("rustc runs");
        let said = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "the fake PresentMon: {said}");
    }

    /// The dummy game's cvar writes the journal holds, in order.
    fn cvar_writes(fx: &Fake) -> Vec<String> {
        let journal = fs::read_to_string(fx.dir.join("data/state/journal.jsonl")).unwrap();
        let what = |l: &str| {
            serde_json::from_str::<Value>(l).unwrap()["action"]["what"]
                .as_str()
                .map(String::from)
        };
        journal
            .lines()
            .filter(|l| l.contains("game_memory"))
            .filter_map(what)
            .collect()
    }

    fn ab_args(order: Value) -> Value {
        json!({
            "token": "tok", "tag": "run", "settle": 0, "secs": 1, "order": order,
            "states": [
                {"label": "A", "actions": [{"cvar": "QuenTest/FrameMs", "set": 20}, {"keys": ["f9"]}]},
                {"label": "B", "actions": [{"cvar": "QuenTest/FrameMs", "set": 23}, {"keys": ["f9"]}]},
            ],
        })
    }

    #[test]
    fn presentmon_runs_on_the_games_pid_and_records_rows() {
        let fx = Fake::new("pm");
        let missing = fx.run("measure_presentmon", json!({"token": "tok", "tag": "base"}));
        assert_eq!(missing["refused"], true, "{missing}");
        assert!(
            missing["reason"]
                .as_str()
                .unwrap()
                .contains("is not a file"),
            "{missing}"
        );
        assert_eq!(
            missing["needs"][0]["dependency"],
            crate::doctor::PRESENTMON.name,
            "{missing}"
        );
        fake_presentmon(&fx);
        let out = fx.run(
            "measure_presentmon",
            json!({"token": "tok", "tag": "base", "secs": 3, "presentmon_flags": ["--track_pc_latency"]}),
        );
        assert_eq!(out["ok"], true, "{out}");
        assert!(
            out["presentmon"]
                .as_str()
                .unwrap()
                .ends_with("PresentMon.exe"),
            "{out}"
        );
        let csv = fx
            .dir
            .join("data")
            .join("captures")
            .join("s1")
            .join("base.csv");
        assert!(csv.is_file());
        // By the game's pid (the dummy game's, the only process in the fixture's job), in quen's own trace session.
        let pid = fx.job.pids().unwrap()[0].to_string();
        let csv_arg = csv.display().to_string();
        let want = [
            "--process_id",
            &pid,
            "--timed",
            "3",
            "--terminate_after_timed",
            "--output_file",
            &csv_arg,
            "--no_console_stats",
            "--session_name",
            "quen",
            "--stop_existing_session",
            "--track_pc_latency",
        ];
        let argv = fs::read_to_string(fx.dir.join("harness/argv.txt")).unwrap();
        assert_eq!(argv.lines().collect::<Vec<_>>(), want);
        assert_eq!(out["window"]["frame_ms"]["mean"], 20.0);
        assert_eq!(out["window"]["present_mode"], "Hardware: Independent Flip");
        assert_eq!(out["ledger"]["measurements"].as_array().unwrap().len(), 12);
        let db =
            quen_core::ledger::Ledger::open_readonly(fx.dir.join("data/state/quen.db")).unwrap();
        let sql = "SELECT DISTINCT boot_id FROM measurements";
        let boots = db.query(sql, &[], Duration::from_secs(5)).unwrap().rows;
        assert_eq!(boots, [[json!(1)]], "the rows name the live session's boot");
        // Feature tests 1: the CSV relative to the capture root, the quiet check on the note and on the boot.
        let sql = "SELECT DISTINCT note FROM measurements";
        let notes = db.query(sql, &[], Duration::from_secs(5)).unwrap().rows;
        let note = notes[0][0].as_str().unwrap();
        assert!(
            note.starts_with("s1/base.csv; Hardware: Independent Flip; ")
                && (note.ends_with("; a quiet machine") || note.contains("; warned: ")),
            "{note}"
        );
        let sql = "SELECT json_type(busy_with) FROM boots WHERE id = 1";
        let busy = db.query(sql, &[], Duration::from_secs(5)).unwrap().rows;
        assert_eq!(busy, [[json!("array")]]);
        assert_eq!(fx.calls(), ["presentmon base.csv 3"]);
        let again = fx.run("measure_presentmon", json!({"token": "tok", "tag": "base"}));
        assert_eq!(again["refused"], true, "{again}");
        let flag = json!({"token": "tok", "tag": "other", "presentmon_flags": ["--process_name", "x.exe"]});
        let flag = fx.run("measure_presentmon", flag);
        assert_eq!(flag["refused"], true, "{flag}");
        assert_eq!(fx.calls().len(), 1, "nothing ran for the refusals");
        fs::write(fx.dir.join("harness/exit.txt"), "5").unwrap();
        let failed = fx.run("measure_presentmon", json!({"token": "tok", "tag": "fail"}));
        let reason = failed["reason"].as_str().unwrap_or_default();
        assert_eq!(failed["ok"], false, "{failed}");
        assert!(
            reason.contains("PresentMon exit 5")
                && !reason.contains("the fake failed")
                && failed["presentmon_said"] == "error: the fake failed",
            "{failed}"
        );

        let db = Ledger::open_readonly(fx.dir.join("data/state/quen.db")).unwrap();
        let rows = db
            .query(
                "SELECT metric, value, state_label, method_family, tag FROM measurements WHERE metric = 'frame_ms_mean'",
                &[],
                Duration::from_secs(5),
            )
            .unwrap();
        assert_eq!(
            rows.rows,
            [[
                json!("frame_ms_mean"),
                json!(20.0),
                json!("base"),
                json!("presentmon"),
                json!("M")
            ]]
        );
    }

    #[test]
    fn ab_alternates_restores_and_gives_the_verdict() {
        let fx = Fake::new("ab");
        fake_presentmon(&fx);
        // Feature tests 1: an ASI export that can't be called refuses before window 1, nothing run.
        let mut asi = ab_args(json!(["A", "B", "A", "B"]));
        asi["states"][0]["actions"] = json!([{"asi": "W3NoSuch.asi", "set_mode": 0}]);
        asi["states"][1]["actions"] = json!([{"asi": "W3NoSuch.asi", "set_mode": 1}]);
        let refused = fx.run("measure_ab", asi);
        assert_eq!(refused["refused"], true, "{refused}");
        assert!(fx.calls().is_empty(), "{:?}", fx.calls());
        // A cvar the game hasn't, or a key token quen doesn't know, is refused before window 1 too.
        let mut typo = ab_args(json!(["A", "B", "A", "B"]));
        typo["states"][0]["actions"][0]["cvar"] = json!("QuenTest/Typo");
        typo["states"][1]["actions"][0]["cvar"] = json!("QuenTest/Typo");
        let mut key = ab_args(json!(["A", "B", "A", "B"]));
        key["states"][1]["actions"][1]["keys"] = json!(["nokey"]);
        for args in [typo, key] {
            let refused = fx.run("measure_ab", args.clone());
            assert_eq!(refused["refused"], true, "{args}: {refused}");
        }
        assert!(fx.calls().is_empty(), "{:?}", fx.calls());
        assert!(cvar_writes(&fx).is_empty());
        let out = fx.run("measure_ab", ab_args(json!(["A", "B", "A", "B"])));
        assert_eq!(out["ok"], true, "{out}");
        assert_eq!(out["verdict"], "outside drift");
        assert_eq!(out["diff_ms"], 3.0);
        assert_eq!(out["drift_ms"]["A"], 0.0);
        let means: Vec<f64> = out["states"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w["frame_ms"]["mean"].as_f64().unwrap())
            .collect();
        assert_eq!(means, [20.0, 23.0, 20.0, 23.0]);
        // Feature tests 4: each window's UTC times, in order, to cut another tool's capture by (ISO times sort as text).
        let times: Vec<String> = out["states"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|w| {
                ["apply", "settled", "presentmon_start", "presentmon_stop"]
                    .map(|k| w["utc"][k].clone())
            })
            .map(|t| t.as_str().unwrap().to_owned())
            .collect();
        assert!(times.len() == 16 && times.is_sorted(), "{times:?}");
        assert!(times[0].len() == 24 && times[0].ends_with('Z'), "{times:?}");
        assert_eq!(out["ledger"]["measurements"].as_array().unwrap().len(), 48);
        // Feature tests 1: every row names what each state did and the verdict.
        let db = Ledger::open_readonly(fx.dir.join("data/state/quen.db")).unwrap();
        let sql = "SELECT note FROM measurements WHERE metric = 'frame_ms_mean' ORDER BY id";
        let notes = db.query(sql, &[], Duration::from_secs(5)).unwrap().rows;
        let note = notes[1][0].as_str().unwrap();
        assert!(
            note.starts_with("s1/run_2B.csv; Hardware: Independent Flip; A: cvar QuenTest/FrameMs set 20, keys f9; B: cvar QuenTest/FrameMs set 23, keys f9; verdict {")
                && note.contains("\"verdict\":\"outside drift\""),
            "{note}"
        );
        // The toggle runs on each switch only, never before the first window; the default comes back at the end, and
        // the toggle goes back to the first state. The first set finds the default already there: no write.
        assert_eq!(
            fx.calls(),
            [
                "presentmon run_1A.csv 1",
                "keys f9",
                "presentmon run_2B.csv 1",
                "keys f9",
                "presentmon run_3A.csv 1",
                "keys f9",
                "presentmon run_4B.csv 1",
                "keys f9",
            ]
        );
        let writes: Vec<String> = cvar_writes(&fx)
            .iter()
            .map(|w| w[..w.find(" at ").unwrap()].into())
            .collect();
        assert_eq!(
            writes,
            [
                "cvar QuenTest/FrameMs set 23",
                "cvar QuenTest/FrameMs set 20",
                "cvar QuenTest/FrameMs set 23",
                "cvar QuenTest/FrameMs back to its value before quen's first write",
            ]
        );
        let now = fx.run(
            "game_cvar",
            json!({"token": "tok", "name": "QuenTest/FrameMs"}),
        );
        assert_eq!(now["var"]["live"], 20.0, "{now}");
    }

    /// Finding 59: console lines toggle like keys, each followed by its script-log line; one that doesn't show (only
    /// an older line holds the text) fails the run.
    #[test]
    fn ab_switches_by_console_line_and_checks_the_script_log() {
        let fx = Fake::new("abconsole");
        fake_presentmon(&fx);
        fs::write(
            fx.dir.join("game/bin/x64_dx12/W3QuenState.log"),
            "push flags=1\n",
        )
        .unwrap();
        let log = fx.dir.join("docs").join(crate::w3::SCRIPTS_LOG);
        fs::write(&log, "[NpcAvoid] on\r\n").unwrap();
        let (to, mut on) = (log.clone(), false);
        fx.on_input(move |what| {
            assert_eq!(what, "console npcavoid()");
            on = !on;
            let mut f = fs::OpenOptions::new().append(true).open(&to).unwrap();
            use std::io::Write as _;
            write!(f, "[npcavoid] aura {}\r\n", if on { "on" } else { "off" }).unwrap();
            Ok(())
        });
        let state = |label, word| {
            json!({"label": label, "actions": [
                {"console": "npcavoid()", "expect_log": {"channel": "NpcAvoid", "contains": format!("aura {word}")}}
            ]})
        };
        let args = json!({"token": "tok", "tag": "con", "settle": 0, "secs": 1,
                          "states": [state("A", "off"), state("B", "on")]});
        let out = fx.run("measure_ab", args.clone());
        assert_eq!(out["ok"], true, "{out}");
        // On each switch, and once more at the end to leave the first state's.
        let console = "console npcavoid()";
        let want = [
            "presentmon con_1A.csv 1",
            console,
            "presentmon con_2B.csv 1",
            console,
        ];
        let want = [
            &want[..],
            &[
                "presentmon con_3A.csv 1",
                console,
                "presentmon con_4B.csv 1",
                console,
            ],
        ]
        .concat();
        assert_eq!(fx.calls(), want);
        let db = Ledger::open_readonly(fx.dir.join("data/state/quen.db")).unwrap();
        let sql = "SELECT note FROM measurements WHERE metric = 'frame_ms_mean' ORDER BY id";
        let notes = db.query(sql, &[], Duration::from_secs(5)).unwrap().rows;
        let note = notes[0][0].as_str().unwrap();
        assert!(
            note.contains("A: console npcavoid() expect [NpcAvoid] \"aura off\"; B: "),
            "{note}"
        );
        // A line that doesn't reach the log: window 2 fails, the old lines don't count, nothing to toggle back.
        fx.on_input(|_| Ok(()));
        let mut args = args;
        args["tag"] = json!("none");
        let out = fx.run("measure_ab", args);
        let reason = out["reason"].as_str().unwrap_or_default();
        assert!(
            out["ok"] == false
                && reason.contains(
                    "window 2 (B): console npcavoid(): no new [NpcAvoid] line holding \"aura on\""
                ),
            "{out}"
        );
        assert_eq!(fx.calls().len(), want.len() + 2, "{:?}", fx.calls());
        // Refused before anything: a bad line, a bad expect_log, a console line in one state only.
        let bad = |actions_a: Value| {
            json!({"token": "tok", "tag": "bad", "settle": 0, "secs": 1,
                   "states": [{"label": "A", "actions": actions_a}, state("B", "on")]})
        };
        for actions in [
            json!([{"console": "say(\"hi\")"}]),
            json!([{"console": "npcavoid()", "expect_log": {"channel": "Npc Avoid", "contains": "x"}}]),
            json!([{"console": "npcavoid()", "expect_log": {"channel": "NpcAvoid", "contains": ""}}]),
            json!([{"console": "npcavoid()", "expect_log": {"channel": "NpcAvoid", "contains": "x", "more": 1}}]),
            json!([]),
        ] {
            let out = fx.run("measure_ab", bad(actions.clone()));
            assert_eq!(out["refused"], true, "{actions}: {out}");
        }
        assert_eq!(fx.calls().len(), want.len() + 2, "ran for a refusal");
    }

    #[test]
    fn ab_stops_on_another_present_mode_and_still_restores() {
        let fx = Fake::new("abmode");
        fake_presentmon(&fx);
        fs::write(fx.dir.join("harness/mode_after.txt"), "Composed: Flip").unwrap();
        let mut args = ab_args(json!(["A", "B", "B", "A"]));
        args["states"][0]["actions"] = json!([{"cvar": "QuenTest/FrameMs", "set": 20}]);
        args["states"][1]["actions"] = json!([{"cvar": "QuenTest/FrameMs", "set": 23}]);
        let out = fx.run("measure_ab", args);
        assert_eq!(
            (out["ok"].clone(), out["refused"].clone()),
            (json!(false), json!(false)),
            "{out}"
        );
        assert!(
            out["reason"]
                .as_str()
                .unwrap()
                .contains("window 2 (B) ran in PresentMode"),
            "{out}"
        );
        assert_eq!(out["states"].as_array().unwrap().len(), 2);
        assert_eq!(out["restored"][0]["ok"], true);
        let last = cvar_writes(&fx).pop().unwrap();
        assert!(
            last.starts_with("cvar QuenTest/FrameMs back to its value before quen's first write"),
            "{last}"
        );
    }

    #[test]
    fn ab_refuses_bad_runs_before_acting() {
        let fx = Fake::new("abrefuse");
        let mut bad = vec![
            ab_args(json!(["A", "B", "A"])),
            ab_args(json!(["A", "B", "C", "A", "B"])),
            ab_args(json!([
                "A", "A", "B", "B", "A", "B", "A", "B", "A", "B", "A", "B", "A"
            ])),
        ];
        let mut other_cvar = ab_args(json!(["A", "B", "A", "B"]));
        other_cvar["states"][0]["actions"] = json!([{"cvar": "QuenTest/Other", "set": 1}]);
        bad.push(other_cvar);
        // A has no keys of its own, so nothing would press them back.
        let mut toggles = ab_args(json!(["A", "B", "A", "B"]));
        toggles["states"][0]["actions"] = json!([{"cvar": "QuenTest/FrameMs", "set": 20}]);
        bad.push(toggles);
        let mut same = ab_args(json!(["A", "B", "A", "B"]));
        same["states"][1]["label"] = json!("A");
        bad.push(same);
        let mut three = ab_args(Value::Null);
        three.as_object_mut().unwrap().remove("order");
        three["states"]
            .as_array_mut()
            .unwrap()
            .push(json!({"label": "C", "actions": []}));
        bad.push(three);
        let mut file = ab_args(json!(["A", "B", "A", "B"]));
        file["tag"] = json!("../x");
        bad.push(file);
        // A good run, but no PresentMon where the config says; and a flag quen doesn't pass on.
        bad.push(ab_args(json!(["A", "B", "A", "B"])));
        let mut flags = ab_args(json!(["A", "B", "A", "B"]));
        flags["presentmon_flags"] = json!(["--output_stdout"]);
        bad.push(flags);
        for args in bad {
            let out = fx.run("measure_ab", args.clone());
            assert_eq!(out["refused"], true, "{args}: {out}");
        }
        assert!(fx.calls().is_empty(), "{:?}", fx.calls());
        assert!(cvar_writes(&fx).is_empty());
    }

    /// Feature tests 2b: an ASI whose SetMode was rejected changed nothing, yet the restore called it again and failed
    /// with the same long reason. An ASI that accepted no mode in the run is skipped; one that did is restored.
    #[test]
    fn the_restore_skips_an_asi_that_took_no_mode() {
        let fx = Fake::new("abskip");
        let session = fx.session();
        let mut call = session.call(crate::mcp::Conn::nobody(), Value::Null, None);
        let cx = ctx(&session, &json!({"token": "tok"}), &[]).unwrap();
        let mut args = ab_args(json!(["A", "B", "A", "B"]));
        args["states"][0]["actions"] = json!([{"asi": "W3NoSuch.asi", "set_mode": 0}]);
        args["states"][1]["actions"] = json!([{"asi": "W3NoSuch.asi", "set_mode": 1}]);
        let run = Run::parse(&args).unwrap();
        let skipped = restore(&cx, &mut call, &run, 0, &[]);
        assert_eq!(
            (skipped[0]["ok"].clone(), skipped[0]["skipped"].is_string()),
            (json!(true), true),
            "{skipped:?}"
        );
        let tried = restore(&cx, &mut call, &run, 0, &[switch_of("W3NoSuch.asi", None)]);
        assert_eq!(tried[0]["ok"], false, "{tried:?}");
    }

    /// A stand-in game for traces, until the flag is set: world.get answers the player standing at the origin, and
    /// world.actors one walker (id 9) passing along y = 0.5 at 1 m/s, from x = -1 at the first read.
    fn tracing(fx: &Fake) -> (std::thread::JoinHandle<Vec<Request>>, Arc<AtomicBool>) {
        let stop = Arc::new(AtomicBool::new(false));
        let start = std::sync::OnceLock::new();
        let answer = Box::new(move |r: &Request| {
            let data = match r.verb {
                Verb::WorldGet {} => json!({"player": {"pos": [0, 0, 0], "heading": 0}}),
                Verb::WorldActors { .. } => {
                    let x = -1.0 + start.get_or_init(Instant::now).elapsed().as_secs_f64();
                    json!({"found": 1, "actors": [{"id": 9, "pos": [x, 0.5, 0], "heading": -90,
                        "velocity": [1, 0, 0], "entity": "walker", "actor": true}]})
                }
                _ => panic!("{:?}", r.verb),
            };
            vec![reply(r.id, true, "read", Some(data))]
        });
        (fake_game_until(&token(fx), answer, stop.clone()), stop)
    }

    fn rows(fx: &Fake, sql: &str) -> Vec<Vec<Value>> {
        let db = Ledger::open_readonly(fx.dir.join("data/state/quen.db")).unwrap();
        db.query(sql, &[], Duration::from_secs(5)).unwrap().rows
    }

    /// Finding 57: a trace window samples the player and the actors, writes its CSV with a captures row, its metrics as
    /// game_state rows citing it, and its sampler on the boot.
    #[test]
    fn a_trace_window_records_its_csv_metrics_and_sampler() {
        let fx = served("trace");
        for bad in [
            json!({"token": "tok", "tag": "x", "trace": {"hz": 11}}),
            json!({"token": "tok", "tag": "x", "trace": {"every": 1}}),
            json!({"token": "tok", "tag": "x", "trace": {"class": "C Actor"}}),
            json!({"token": "tok", "tag": "../x"}),
            json!({"token": "tok", "tag": "x", "secs": 0}),
        ] {
            let out = fx.run("measure_trace", bad.clone());
            assert_eq!(out["refused"], true, "{bad}: {out}");
        }
        let (server, stop) = tracing(&fx);
        let out = fx.run(
            "measure_trace",
            json!({"token": "tok", "tag": "walk", "secs": 2}),
        );
        stop.store(true, Ordering::Relaxed);
        let reads = server.join().unwrap();
        assert_eq!(out["ok"], true, "{out}");
        let m = &out["window"]["trace"]["metrics"];
        assert_eq!(
            (m["passes"].clone(), m["samples"].clone()),
            (json!(1), json!(21)),
            "{out}"
        );
        let closest = m["closest_m_min"].as_f64().unwrap();
        assert!((0.5..0.6).contains(&closest), "{out}");
        assert_eq!(out["budget"]["cost_ms_per_frame"], "unmeasured", "{out}");
        assert_eq!(
            out["sampler"],
            "actor_trace 10 Hz r8 max20 ref player contact 0.75"
        );
        // Per sample world.get, then world.actors with the default radius and max.
        assert_eq!(reads.len(), 42);
        assert_eq!(reads[0].verb, Verb::WorldGet {});
        let actors = Verb::WorldActors {
            radius: 8.0,
            max: 20,
            tag: None,
            class: None,
        };
        assert_eq!(reads[1].verb, actors);
        let csv = fx.dir.join("data/captures/s1/walk-trace.csv");
        let text = fs::read_to_string(&csv).unwrap();
        assert_eq!(text.lines().count(), 1 + 2 * 21, "{text}");
        let captures = rows(&fx, "SELECT id, path, kind, method FROM captures");
        assert_eq!(
            captures,
            [[
                json!(1),
                json!("s1/walk-trace.csv"),
                json!("csv"),
                json!("actor_trace")
            ]]
        );
        let sql =
            "SELECT DISTINCT method_family, capture_id, state_label, run_id FROM measurements";
        assert_eq!(
            rows(&fx, sql),
            [[
                json!("game_state"),
                json!(1),
                json!("walk"),
                json!("s1-walk")
            ]]
        );
        let sql = "SELECT note FROM measurements WHERE metric = 'contacts_per_min'";
        let note = rows(&fx, sql)[0][0].as_str().unwrap().to_owned();
        assert!(
            note.starts_with(
                "s1/walk-trace.csv; actor_trace 10 Hz r8 max20 ref player contact 0.75; 21 samples over "
            ),
            "{note}"
        );
        let samplers = rows(&fx, "SELECT samplers, busy_with FROM boots WHERE id = 1");
        assert_eq!(
            samplers,
            [[
                json!(r#"["actor_trace 10 Hz r8 max20 ref player contact 0.75"]"#),
                Value::Null
            ]]
        );
        let again = fx.run("measure_trace", json!({"token": "tok", "tag": "walk"}));
        assert_eq!(again["refused"], true, "{again}");
    }

    /// The runner design's Q2 and Q4: the same trace beside PresentMon in every window, its sampler named on the frame
    /// rows and in the verdict, per metric a behaviour verdict; or the trace alone, without PresentMon at all.
    /// Finding 79: job_cancel's flag (a cancel of a `wait: false` call: it sets the flag the session thread's call
    /// checks) during the gap after the measuring notice starts no window, and one during the window kills PresentMon;
    /// both fail and say when. The live check saw the window start 3 s after the notice and run on to a plain ok.
    #[test]
    fn a_cancel_in_the_notice_gap_or_the_window_measures_nothing() {
        let cancel_after = |fx: &Fake, ms, tool, args: Value| {
            std::thread::scope(|s| {
                s.spawn(|| {
                    sleep(Duration::from_millis(ms));
                    fx.shared.cancel(0, &Value::Null);
                });
                let t = Instant::now();
                (fx.run(tool, args), t.elapsed())
            })
        };
        let fx = served("pmcancel-gap");
        fake_presentmon(&fx);
        fx.shared.set_hands(Some(Hands::Agent));
        let notice = Box::new(|r: &Request| vec![reply(r.id, true, "notice shown", None)]);
        let server = fake_game(&token(&fx), 1, notice);
        let args = json!({"token": "tok", "tag": "gap", "secs": 30});
        let (out, took) = cancel_after(&fx, 500, "measure_presentmon", args);
        assert_eq!(server.join().unwrap().len(), 1, "the notice went out");
        let reason = out["reason"].as_str().unwrap_or_default();
        assert!(
            out["ok"] == false
                && reason.starts_with("the call was cancelled during the 9 s after quen's notice"),
            "{out}"
        );
        assert!(fx.calls().is_empty(), "a window started: {:?}", fx.calls());
        assert!(took < Duration::from_secs(5), "{took:?}");
        // During the window: PresentMon killed, no rows.
        let fx = served("pmcancel-window");
        fake_presentmon(&fx);
        fs::write(fx.dir.join("harness/sleep.txt"), "20000").unwrap();
        let args = json!({"token": "tok", "tag": "mid", "secs": 10});
        let (out, took) = cancel_after(&fx, 700, "measure_presentmon", args);
        let reason = out["reason"].as_str().unwrap_or_default();
        assert!(
            out["ok"] == false
                && reason.starts_with("the call was cancelled ")
                && reason.contains("s into the 10 s window: PresentMon was killed"),
            "{out}"
        );
        assert!(took < Duration::from_secs(5), "{took:?}");
        assert_eq!(fx.calls(), ["presentmon mid.csv 10"]);
        let db = Ledger::open_readonly(fx.dir.join("data/state/quen.db")).unwrap();
        let none = db
            .query(
                "SELECT count(*) FROM measurements",
                &[],
                Duration::from_secs(5),
            )
            .unwrap();
        assert_eq!(none.rows, [[json!(0)]]);
    }

    #[test]
    fn ab_traces_every_window_beside_or_instead_of_presentmon() {
        let fx = served("abtrace");
        let mut args = ab_args(json!(["A", "B", "A", "B"]));
        args["trace"] = json!({"hz": 5, "radius": 10, "max": 4});
        args["presentmon"] = json!(false);
        // presentmon false needs a trace and takes no PresentMon flags; refused before anything runs.
        let mut alone = args.clone();
        alone.as_object_mut().unwrap().remove("trace");
        let mut flags = args.clone();
        flags["presentmon_flags"] = json!(["--track_pc_latency"]);
        for bad in [alone, flags] {
            assert_eq!(fx.run("measure_ab", bad.clone())["refused"], true, "{bad}");
        }
        let (server, stop) = tracing(&fx);
        let out = fx.run("measure_ab", args.clone());
        assert_eq!(out["ok"], true, "{out}");
        assert!(
            fx.calls().iter().all(|c| !c.starts_with("presentmon")),
            "no PresentMon: {:?}",
            fx.calls()
        );
        assert!(
            out.get("verdict").is_none() && out.get("presentmon").is_none(),
            "{out}"
        );
        let behaviour = out["behaviour"].as_array().unwrap();
        assert_eq!(behaviour.len(), trace::METRICS.len(), "{out}");
        assert_eq!(
            behaviour[0]["verdict"], "too few events",
            "a pass per window: {out}"
        );
        assert_eq!(
            out["budget"]["sampler"],
            "actor_trace 5 Hz r10 max4 ref player contact 0.75"
        );
        let utc = &out["states"][0]["utc"];
        assert!(
            utc["trace_start"].is_string() && utc.get("presentmon_start").is_none(),
            "{out}"
        );
        // Beside PresentMon: both families per window, the frame rows scoped by the sampler.
        fake_presentmon(&fx);
        args["tag"] = json!("both");
        args.as_object_mut().unwrap().remove("presentmon");
        let both = fx.run("measure_ab", args);
        stop.store(true, Ordering::Relaxed);
        server.join().unwrap();
        assert_eq!(both["ok"], true, "{both}");
        assert_eq!(both["verdict"], "outside drift", "{both}");
        assert!(
            both["sampler"]
                .as_str()
                .unwrap()
                .starts_with("sampler actor_trace 5 Hz"),
            "{both}"
        );
        for n in 1..=4 {
            let label = if n % 2 == 1 { "A" } else { "B" };
            let dir = fx.dir.join("data/captures/s1");
            assert!(dir.join(format!("both_{n}{label}.csv")).is_file());
            assert!(dir.join(format!("both_{n}{label}-trace.csv")).is_file());
        }
        let sql = "SELECT method_family, count(DISTINCT capture_id), count(*) FROM measurements \
                   WHERE run_id = 's1-both' GROUP BY method_family ORDER BY method_family";
        let families = rows(&fx, sql);
        assert_eq!(
            (families[0][0].clone(), families[0][1].clone()),
            (json!("game_state"), json!(4))
        );
        assert_eq!(families[1][..], [json!("presentmon"), json!(0), json!(48)]);
        let sql =
            "SELECT note FROM measurements WHERE run_id = 's1-both' AND metric = 'frame_ms_mean'";
        let note = rows(&fx, sql)[0][0].as_str().unwrap().to_owned();
        assert!(
            note.contains(
                "sampler actor_trace 5 Hz r10 max4 ref player contact 0.75 in every window"
            ),
            "{note}"
        );
        let sql = "SELECT note FROM measurements WHERE run_id = 's1-both' AND metric = 'passes'";
        let note = rows(&fx, sql)[0][0].as_str().unwrap().to_owned();
        assert!(note.contains("; passes verdict {\"diff\":0.0,"), "{note}");
        // Finding 85: the metric's own verdict before the run's frame-time one.
        assert!(
            note.find("passes verdict") < note.find("; verdict {"),
            "{note}"
        );
        // The overhead boot's A/B: the trace only in B's windows, its frame-time cost as the verdict; no behaviour.
        let (server, stop) = tracing(&fx);
        // The overhead boot's own call: two states that do nothing, the trace in one.
        let mut only = json!({"token": "tok", "tag": "cost", "settle": 0, "secs": 1, "trace": {"max": 4},
            "trace_only": "B", "states": [{"label": "A", "actions": []}, {"label": "B", "actions": []}]});
        let cost = fx.run("measure_ab", only.clone());
        stop.store(true, Ordering::Relaxed);
        server.join().unwrap();
        assert_eq!(cost["ok"], true, "{cost}");
        assert!(cost.get("behaviour").is_none(), "{cost}");
        let traced: Vec<bool> = cost["states"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w.get("trace").is_some())
            .collect();
        assert_eq!(traced, [false, true, false, true]);
        let sampler = cost["sampler"].as_str().unwrap();
        assert!(
            sampler.ends_with("ref player contact 0.75 in the B windows only"),
            "{cost}"
        );
        for bad in [json!("C"), json!(1)] {
            only["trace_only"] = bad;
            assert_eq!(fx.run("measure_ab", only.clone())["refused"], true);
        }
        only["trace_only"] = json!("B");
        only["presentmon"] = json!(false);
        assert_eq!(fx.run("measure_ab", only)["refused"], true);
    }

    #[test]
    fn busy_images_are_named_once_any_case() {
        let running = [
            (1, "CARGO.EXE".to_string()),
            (2, "cargo.exe".into()),
            (3, "python.exe".into()),
            (4, "analyzeHeadless.bat".into()),
            (5, "java.exe".into()),
            (6, "clang.exe".into()),
        ];
        assert_eq!(
            busy(&running),
            ["analyzeheadless.bat", "cargo.exe", "java.exe"]
        );
        assert!(busy(&[(1, "witcher3.exe".into())]).is_empty());
    }
}
