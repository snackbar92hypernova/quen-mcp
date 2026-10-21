//! `run_plan` (the runner design's phase 5): a plan of quen's own tool calls, every step checked against its tool's
//! schema before any runs, then run in order on the session thread as one call. A plan does what a test session's agent
//! does without the client's prompt (the owner's Q7), never a launch, a save, a hand-over, a cleanup or a memory patch.
//! Its setup steps that succeed are noted for `session_launch again`; its teardown runs on every way out once a step
//! ran. The plan (canonical JSON) and the result are files in the session's capture folder, each with a captures row
//! (Q14). A/B comparisons are `measure_ab` steps: its windows, restore and rows as they are.

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use quen_core::input::{Event, GAP_MS, GRAVE, HOLD_MS};
use quen_core::journal::sha256_hex;
use quen_core::ledger::Capture;
use serde_json::{Map, Value, json};

use crate::game::{self, Ctx, Fields, ctx, flag};
use crate::session::{self, Call, Done, Session};
use crate::{doctor, measure, status};

/// The tools a step may call: what a test session's agent calls without the client's prompt (the owner's Q7), sweeps
/// aside. Never a launch, a save, a hand-over, a cleanup, a memory patch, a focus move or a ledger write.
const TOOLS: [&str; 28] = [
    "game_console",
    "game_exec",
    "game_cvar",
    "game_keys",
    "game_teleport",
    "game_place",
    "game_settime",
    "game_weather",
    "game_photo",
    "game_camera",
    "game_frame",
    "game_screenshot",
    "game_load",
    "game_items",
    "game_pause",
    "game_scriptlog",
    "game_listsaves",
    "world_get",
    "world_actors",
    "world_frame",
    "world_ground",
    "world_ray",
    "world_nav",
    "world_map",
    "measure_trace",
    "measure_presentmon",
    "measure_ab",
    "measure_image",
];
/// A plan's estimate cap by default and at most, and the estimate over which it runs only as a job (the owner's Q8).
const MAX_S: f64 = 600.0;
const MAX_S_CAP: f64 = 3600.0;
const JOB_S: f64 = 300.0;
/// The longest `wait` step, and the most steps in a plan.
const WAIT_MAX_S: f64 = 600.0;
const MAX_STEPS: usize = 200;
/// What a step's tool takes, in seconds, for the estimate: a read is 0.02-0.04 s quen-side (boot 175), a game_frame side
/// 0.75-0.86 s with its 250 ms settle (boot 190), a load 33.84 s (boot 191's setup), a screenshot under 1 s (inside
/// those sides); any other verb one channel round trip and its checks.
// ponytail: per-tool constants from single boots; replace with each tool's median once plans have run.
const READ_S: f64 = 0.05;
const SIDE_S: f64 = 0.55;
const LOAD_S: f64 = 34.0;
const SCREENSHOT_S: f64 = 1.0;
const VERB_S: f64 = 0.2;
/// A world_map step: per verb, and once (its files and maths).
// ponytail: the design's guesses until a live map is timed; replace with the tool's median once plans have run it.
const MAP_VERB_S: f64 = 0.03;
const MAP_S: f64 = 0.3;
/// A measure_image metric: 0.03-1.35 s each on a synthetic 4K frame, a file's decode 0.05-0.11 s (release build).
// ponytail: one timing of synthetic frames; replace with the tool's median once plans have run it.
const IMAGE_S: f64 = 1.5;
/// What a step result's table row keeps of a text field (the whole is in the result file).
const TEXT_MAX: usize = 200;

pub fn tool() -> Value {
    json!({
        "name": "run_plan",
        "description": "Run a plan of quen's own tool calls as one call (quen_guide: run_plan). plan: JSON text (an \
            object serves too) {tag, setup, steps, teardown, max_s}; each step {\"<tool>\": {its arguments, no \
            token}} or {\"wait\": seconds}. Tools: the game_, world_ and measure_ tools a test session calls without \
            the client's prompt; no launch, save, hand-over, cleanup, memory patch or focus. Every step is checked \
            against its tool's schema before any runs. Setup steps that succeed are replayed by session_launch again. \
            A step that doesn't succeed ends the run (aborted); teardown then runs, also after a cancel. estimate_s: \
            from measured costs, fade waits left out; refused over max_s (default 600, at most 3600), and over 300 \
            unless wait: false. check: the checks and estimate, nothing run. Files: <tag>-plan.json and <tag>-result.json (every step's \
            envelope), each with a captures row. Refused, nothing run: a bad plan or step, a tag taken, the user's \
            controls, a play session.",
        "inputSchema": game::schema(json!({
            "plan": {"type": "string", "description": "the plan: JSON text of {tag, setup, steps, teardown, max_s}"},
            "check": {"type": "boolean", "description": "true: check and estimate only, run nothing"},
        }), &["plan"]),
    })
}

/// One step: its tool (or `wait`), arguments and estimate.
struct Step {
    tool: String,
    args: Value,
    est: f64,
}

/// A checked plan: its tag, its phases in order, its cap, and its canonical JSON.
struct Plan {
    tag: String,
    phases: [(&'static str, Vec<Step>); 3],
    max_s: f64,
    canonical: String,
}

impl Plan {
    /// The steps' estimates, and what is left of the plan's measuring line's [`status::MEASURING_SHOWN`] at its first
    /// capture or window, after the game's answer that it is up (one [`status::CONFIRM_EVERY`]).
    fn estimate(&self) -> f64 {
        let steps = || self.phases.iter().flat_map(|(_, s)| s);
        let hides = |s: &Step| {
            status::captures(&s.tool) && (s.tool != "game_frame" || s.args.get("tag").is_some())
        };
        let shown = steps().position(hides).map_or(0.0, |first| {
            let before: f64 = steps().take(first).map(|s| s.est).sum();
            status::CONFIRM_EVERY.as_secs_f64()
                + (status::MEASURING_SHOWN.as_secs_f64() - before).max(0.0)
        });
        steps().map(|s| s.est).sum::<f64>() + shown
    }
}

/// The plan in `v` (JSON text or an object), each step checked against its tool's schema in `schemas` (mcp.rs's
/// tools) and estimated; `exec`: console lines go as exec verbs (measure_ab's).
fn parse(v: Option<&Value>, schemas: &Value, exec: bool) -> Result<Plan, String> {
    let plan = match v {
        Some(Value::String(text)) => {
            serde_json::from_str(text).map_err(|e| format!("plan: {e}"))?
        }
        Some(v @ Value::Object(_)) => v.clone(),
        _ => return Err("plan must be JSON text of {tag, setup, steps, teardown, max_s}".into()),
    };
    let map = plan.as_object().ok_or("plan must be an object")?;
    let keys = ["tag", "setup", "steps", "teardown", "max_s"];
    if let Some(key) = map.keys().find(|k| !keys.contains(&k.as_str())) {
        return Err(format!("plan: unknown key {key:?} (it takes {keys:?})"));
    }
    let tag = plan["tag"].as_str().filter(|t| {
        (1..=40).contains(&t.len())
            && t.chars()
                .all(|c| c.is_ascii_alphanumeric() || "_-".contains(c))
    });
    let tag = tag
        .ok_or("plan: tag is 1-40 letters, digits, _ and -")?
        .to_string();
    let max_s = match plan.get("max_s") {
        None => MAX_S,
        Some(v) => v
            .as_f64()
            .filter(|s| *s > 0.0 && *s <= MAX_S_CAP)
            .ok_or(format!(
                "plan: max_s is seconds above 0, at most {MAX_S_CAP}"
            ))?,
    };
    let mut phases = [
        ("setup", Vec::new()),
        ("steps", Vec::new()),
        ("teardown", Vec::new()),
    ];
    let mut tags = Vec::new();
    for (phase, steps) in &mut phases {
        let Some(list) = plan.get(*phase) else {
            continue;
        };
        let list = list
            .as_array()
            .ok_or(format!("plan: {phase} is an array of steps"))?;
        for (i, v) in list.iter().enumerate() {
            let at = format!("{phase} step {}", i + 1);
            let step = step(v, schemas, exec).map_err(|why| format!("{at}: {why}"))?;
            if *phase == "teardown" && step.tool == "wait" {
                return Err(format!(
                    "{at}: no wait in teardown (it runs after a cancel too)"
                ));
            }
            if let Some(t) = step.args.get("tag").and_then(Value::as_str) {
                if tags.contains(&t.to_string()) {
                    return Err(format!(
                        "{at}: tag {t:?} twice in the plan: its files would clash"
                    ));
                }
                tags.push(t.to_string());
            }
            steps.push(step);
        }
    }
    let n: usize = phases.iter().map(|(_, s)| s.len()).sum();
    if phases[0].1.is_empty() && phases[1].1.is_empty() || n > MAX_STEPS {
        return Err(format!(
            "plan: 1 to {MAX_STEPS} steps, at least one in setup or steps"
        ));
    }
    Ok(Plan {
        tag,
        phases,
        max_s,
        // serde_json's maps keep their keys sorted: the same plan always gives the same text.
        canonical: plan.to_string(),
    })
}

/// One step: `{"<tool>": {arguments}}` or `{"wait": seconds}`.
fn step(v: &Value, schemas: &Value, exec: bool) -> Result<Step, String> {
    let map = v.as_object().filter(|m| m.len() == 1);
    let (tool, args) = map
        .and_then(|m| m.iter().next())
        .ok_or("a step is one {\"<tool>\": {arguments}} or {\"wait\": seconds}")?;
    if tool == "wait" {
        let s = args
            .as_f64()
            .filter(|s| *s > 0.0 && *s <= WAIT_MAX_S)
            .ok_or(format!("wait is seconds above 0, at most {WAIT_MAX_S}"))?;
        return Ok(Step {
            tool: tool.clone(),
            args: json!(s),
            est: s,
        });
    }
    if !TOOLS.contains(&tool.as_str()) {
        return Err(format!(
            "{tool:?} isn't a tool a plan calls: one of {}",
            TOOLS.join(", ")
        ));
    }
    let args = match args {
        Value::Null => json!({}),
        args => args.clone(),
    };
    let obj = args
        .as_object()
        .ok_or(format!("{tool}: its arguments are an object"))?;
    if let Some(key) = ["token", "wait"].into_iter().find(|k| obj.contains_key(*k)) {
        return Err(format!(
            "{tool}: no {key} in a step (the plan's call has it)"
        ));
    }
    let schema = schemas
        .as_array()
        .and_then(|t| t.iter().find(|t| t["name"] == *tool))
        .map(|t| &t["inputSchema"])
        .ok_or(format!("{tool}: no such tool"))?;
    valid(schema, &args, tool)?;
    Ok(Step {
        est: estimate(tool, &args, exec)?,
        tool: tool.clone(),
        args,
    })
}

/// `v` against `schema`, the portable subset quen's schemas use (mcp.rs: type, properties, required,
/// additionalProperties, items, minItems, maxItems, minimum, maximum, enum; `pattern` is left to the tool), no stricter
/// than the tools themselves. `token`, which the plan's call carries, isn't required.
fn valid(schema: &Value, v: &Value, at: &str) -> Result<(), String> {
    let ty = schema["type"].as_str();
    let ok = match ty {
        // The tools take a number or true / false as text where their schema says string (a cvar's set).
        Some("string") => v.is_string() || v.is_number() || v.is_boolean(),
        Some("integer") => v.is_i64() || v.is_u64(),
        Some("number") => v.is_number(),
        Some("boolean") => v.is_boolean(),
        Some("array") => v.is_array(),
        Some("object") => v.is_object(),
        _ => true,
    };
    if !ok {
        return Err(format!("{at}: not of type {}", ty.unwrap_or_default()));
    }
    if let Some(of) = schema["enum"].as_array()
        && !of.contains(v)
    {
        return Err(format!("{at}: one of {}", json!(of)));
    }
    if let Some(n) = v.as_f64() {
        let low = schema["minimum"].as_f64().filter(|m| n < *m);
        let high = schema["maximum"].as_f64().filter(|m| n > *m);
        if let Some(m) = low.or(high) {
            let side = if low.is_some() { "under" } else { "over" };
            return Err(format!("{at}: {n} is {side} {m}"));
        }
    }
    if let Some(items) = v.as_array() {
        let n = items.len() as u64;
        if schema["minItems"].as_u64().is_some_and(|m| n < m)
            || schema["maxItems"].as_u64().is_some_and(|m| n > m)
        {
            return Err(format!(
                "{at}: {n} items, not {} to {}",
                schema["minItems"].as_u64().unwrap_or(0),
                schema["maxItems"]
                    .as_u64()
                    .map_or("any".into(), |m| m.to_string())
            ));
        }
        for (i, item) in items.iter().enumerate() {
            valid(&schema["items"], item, &format!("{at}[{i}]"))?;
        }
    }
    if let Some(obj) = v.as_object() {
        let props = schema["properties"].as_object();
        for (key, item) in obj {
            match props.and_then(|p| p.get(key)) {
                Some(s) => valid(s, item, &format!("{at}.{key}"))?,
                None if schema["additionalProperties"] == false => {
                    return Err(format!("{at}: unknown argument {key:?}"));
                }
                None => {}
            }
        }
        let required = schema["required"].as_array().into_iter().flatten();
        if let Some(key) = required
            .filter_map(Value::as_str)
            .find(|k| *k != "token" && !obj.contains_key(*k))
        {
            return Err(format!("{at}: {key} is required"));
        }
    }
    Ok(())
}

/// About how long `tool` takes with `args`, in seconds ([`READ_S`] and the other constants; measure_ab's own estimate;
/// a window its secs plus 0.3 s, the design's §3.2; game_keys its holds, gaps and waits; a capture or window the status
/// line's hide wait too).
fn estimate(tool: &str, args: &Value, exec: bool) -> Result<f64, String> {
    let num = |key: &str, default: f64| args.get(key).and_then(Value::as_f64).unwrap_or(default);
    let hide = status::HIDE_WAIT.as_secs_f64();
    Ok(match tool {
        "game_console" => measure::CONSOLE_S,
        "game_keys" => {
            let tokens: Vec<&str> = args["tokens"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            let plan = quen_core::input::plan(&tokens, HOLD_MS, GAP_MS, GRAVE)
                .map_err(|e| format!("game_keys: {e}"))?;
            let ms: u64 = plan
                .iter()
                .map(|e| match e {
                    Event::Sleep { ms, .. } => *ms,
                    Event::Key { .. } => 0,
                })
                .sum();
            ms as f64 / 1000.0
        }
        "game_weather" => VERB_S + num("settle_s", 0.0),
        "game_load" => LOAD_S,
        "game_screenshot" => SCREENSHOT_S + hide,
        "game_frame" => {
            let sides = args["sides"].as_array().map_or(1, Vec::len) as f64;
            let captured = if args.get("tag").is_some() { hide } else { 0.0 };
            VERB_S + sides * (SIDE_S + num("settle_ms", 250.0) / 1000.0) + captured
        }
        "measure_trace" => num("secs", 30.0) + 0.3 + hide,
        "measure_presentmon" => num("secs", 10.0) + 1.0 + hide,
        "measure_ab" => measure::ab_secs(args, exec).map_err(|e| format!("measure_ab: {e}"))?,
        "measure_image" => IMAGE_S * args["metrics"].as_array().map_or(1, Vec::len) as f64,
        "world_map" => crate::scene::map_calls(args) as f64 * MAP_VERB_S + MAP_S,
        t if t.starts_with("world_") || matches!(t, "game_scriptlog" | "game_listsaves") => READ_S,
        _ => VERB_S,
    })
}

pub(crate) fn call(session: &mut Session, call: &mut Call, args: &Value) -> Done {
    run(session, call, args).map_or_else(|done| done, Done::Ok)
}

/// A step's row of the result's table: where it ran, how it ended, its time against its estimate, and its own text and
/// number fields (a text cut at [`TEXT_MAX`]); its whole envelope goes to the result file.
fn row(phase: &str, n: usize, step: &Step, envelope: &Value, secs: f64) -> Value {
    let mut out = json!({"phase": phase, "n": n, "tool": step.tool, "ok": envelope["ok"],
                         "s": (secs * 100.0).round() / 100.0, "est_s": (step.est * 100.0).round() / 100.0});
    if envelope["ok"] != true {
        out["refused"] = envelope["refused"].clone();
        out["reason"] = envelope["reason"].clone();
    }
    if envelope["ledger"]
        .as_object()
        .is_some_and(|l| !l.is_empty())
    {
        out["ledger"] = envelope["ledger"].clone();
    }
    let envelope_keys = [
        "ok",
        "tool",
        "refused",
        "reason",
        "state",
        "timing_s",
        "artifacts",
        "ledger",
    ];
    for (key, v) in envelope.as_object().into_iter().flatten() {
        if envelope_keys.contains(&key.as_str()) {
            continue;
        }
        match v {
            Value::String(s) if s.chars().count() > TEXT_MAX => {
                let cut: String = s.chars().take(TEXT_MAX).collect();
                out[key] = json!(format!("{cut}... (whole in the result file)"));
            }
            Value::String(_) | Value::Number(_) | Value::Bool(_) => out[key] = v.clone(),
            _ => {}
        }
    }
    out
}

/// Writes `text` to `path` with a captures row (kind json, method run_plan): its id, if the ledger took it.
fn file(cx: &Ctx, call: &mut Call, path: &Path, text: &str) -> Result<Option<i64>, String> {
    fs::create_dir_all(measure::captures(cx))
        .and_then(|()| fs::write(path, text))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    call.artifact(path);
    let capture = Capture {
        boot_id: Some(cx.live.boot),
        path: measure::relative(cx, path),
        sha256: sha256_hex(text.as_bytes()),
        bytes: text.len() as i64,
        kind: "json".into(),
        method: Some("run_plan".into()),
        t: session::utc(SystemTime::now()),
        ..Capture::default()
    };
    let id = session::open_ledger(cx.config)
        .and_then(|mut l| l.add_capture(&capture).map_err(session::ledger_error))
        .ok();
    if let Some(id) = id {
        call.row("captures", id);
    }
    Ok(id)
}

/// Why the plan stops before its next step, if it must: a cancel, `max_s` passed, the game gone, quen's state off
/// gameplay and photo mode; else, before a step that needs the game in front, its menu closed (finding 88), what that
/// did.
fn before(
    cx: &Ctx,
    call: &Call,
    tool: &str,
    start: Instant,
    max_s: f64,
) -> Result<Option<Fields>, String> {
    if call.stopped() {
        return Err(format!("{} before it", call.stop_cause()));
    }
    if start.elapsed().as_secs_f64() > max_s {
        return Err(format!("max_s ({max_s} s) passed before it"));
    }
    if cx.live.job.pids().unwrap_or_default().is_empty() {
        return Err("the game exited".into());
    }
    // A long teleport fades (flags 257 after a 215 m game_place, the 0.1.12 live check): waited out.
    if let Err(last) = cx.wait_for(call, game::READY_MAX, game::in_game) {
        return Err(format!(
            "quen's state showed {} for {} s, neither gameplay nor photo mode",
            game::shown(last),
            game::READY_MAX.as_secs()
        ));
    }
    // The status line back after a step's capture, or sent again when due; not just before a capture, which takes it
    // down again.
    if !status::captures(tool) {
        status::tick(cx);
    }
    if tool == "wait" || !status::needs_front(tool) {
        return Ok(None);
    }
    measure::game_back(cx, call).map_err(|done| crate::boot::reason(&done))
}

fn run(session: &mut Session, call: &mut Call, args: &Value) -> Result<Fields, Done> {
    let schemas = crate::mcp::tools();
    let token = json!({"token": args["token"]});
    let (plan, folder) = {
        let cx = ctx(session, args, &["plan", "check"])?;
        if cx.live.play {
            let why = "run_plan runs in a test session: in a play session the game is the user's";
            return Err(why.to_string().into());
        }
        let exec = doctor::has_exec(doctor::package_version(cx.config));
        (
            parse(args.get("plan"), &schemas, exec)?,
            measure::captures(&cx),
        )
    };
    let estimate = (plan.estimate() * 10.0).round() / 10.0;
    // A fade can't be foreseen: only a long move fades (7.5 s after a 215 m game_place, the 0.1.12 live check).
    let excludes = format!(
        "fade waits: up to {} s before a step (before_s in its row)",
        game::READY_MAX.as_secs()
    );
    let mut fields = Map::from_iter([
        ("tag".to_string(), json!(plan.tag)),
        ("estimate_s".into(), json!(estimate)),
        ("estimate_excludes".into(), json!(excludes)),
        ("max_s".into(), json!(plan.max_s)),
    ]);
    let listed: Vec<Value> = plan
        .phases
        .iter()
        .flat_map(|(phase, steps)| {
            steps.iter().enumerate().map(move |(i, s)| {
                json!({"phase": phase, "n": i + 1, "tool": s.tool, "est_s": (s.est * 100.0).round() / 100.0})
            })
        })
        .collect();
    let plan_path = folder.join(format!("{}-plan.json", plan.tag));
    let result_path = folder.join(format!("{}-result.json", plan.tag));
    if let Some(taken) = [&plan_path, &result_path].into_iter().find(|p| p.exists()) {
        return Err(format!("{} exists: pick a tag new in this session", taken.display()).into());
    }
    if estimate > plan.max_s {
        let why = format!(
            "the plan's estimate is {estimate} s, over max_s {} s: nothing run",
            plan.max_s
        );
        return Err(Done::RefusedWith(
            why,
            Map::from_iter([("steps".into(), json!(listed))]),
        ));
    }
    let sha = sha256_hex(plan.canonical.as_bytes());
    if flag(args, "check")? {
        fields.insert("steps".into(), json!(listed));
        fields.insert("sha256".into(), json!(sha));
        return Ok(fields);
    }
    if estimate > JOB_S && !call.detached() {
        let why = format!(
            "the plan's estimate is {estimate} s: a plan over {JOB_S} s runs as a job (wait: false, then job_status). \
             Nothing run"
        );
        return Err(why.into());
    }
    let start = Instant::now();
    let mut aborted = None;
    {
        let cx = ctx(session, &token, &[])?;
        // The checks before every step that need no change, before the plan's file is written.
        before(&cx, call, "wait", start, plan.max_s)
            .map_err(|why| format!("{why}: nothing run"))?;
        call.lap("check");
        let plan_id = file(&cx, call, &plan_path, &plan.canonical).map_err(Done::Refused)?;
        fields.insert(
            "plan".into(),
            json!({"path": plan_path, "sha256": sha, "capture_id": plan_id}),
        );
        eprintln!(
            "{} quen: run_plan {} ({sha}) started, estimate {estimate} s",
            session::utc_ms(SystemTime::now()),
            plan.tag
        );
        if let Some(shown) = status::measuring(&cx, call, estimate.ceil() as u64, &mut None) {
            fields.insert("status".into(), json!(shown));
        }
        cx.live.in_plan.set(true);
    }
    let (mut table, mut envelopes, mut ran) = (Vec::new(), Vec::new(), false);
    'run: for (phase, steps) in plan.phases[..2].iter() {
        for (i, step) in steps.iter().enumerate() {
            let at = format!("{phase} step {} ({})", i + 1, step.tool);
            let ready = Instant::now();
            let checked = ctx(session, &token, &[])
                .and_then(|cx| before(&cx, call, &step.tool, start, plan.max_s));
            let waited = ready.elapsed().as_secs_f64();
            let back = match checked {
                Ok(back) => back,
                Err(why) => {
                    aborted = Some(format!("{at}: {why}"));
                    break 'run;
                }
            };
            ran = true;
            call.progress(&format!("run_plan {at}"));
            let t = Instant::now();
            let mut envelope = match step.tool.as_str() {
                "wait" => {
                    let waited = measure::settle(call, Duration::from_secs_f64(step.est), &at);
                    let why = (!waited).then(|| format!("{} during it", call.stop_cause()));
                    json!({"ok": waited, "tool": "wait", "refused": false, "reason": why})
                }
                tool => {
                    let mut step_args = step.args.clone();
                    step_args["token"] = token["token"].clone();
                    let (done, each) =
                        session.nested(call, tool, &step_args, *phase == "setup", true);
                    let envelope = session.envelope(tool, each, done);
                    call.merge(&envelope);
                    envelope
                }
            };
            if let Some(back) = back {
                envelope["game_back"] = json!(back);
            }
            let mut row = row(phase, i + 1, step, &envelope, t.elapsed().as_secs_f64());
            // The checks before it: a fade waited out, the game brought back (not in its s).
            if waited >= 0.1 {
                row["before_s"] = json!((waited * 100.0).round() / 100.0);
            }
            envelope["phase"] = json!(phase);
            envelope["n"] = json!(i + 1);
            envelopes.push(envelope);
            let failed =
                (row["ok"] != true).then(|| row["reason"].as_str().unwrap_or_default().to_string());
            table.push(row);
            if let Some(why) = failed {
                aborted = Some(format!("{at}: {why}"));
                break 'run;
            }
        }
    }
    let took = (start.elapsed().as_secs_f64() * 10.0).round() / 10.0;
    call.lap("steps");
    // The teardown, on every way out once a step ran: each step whatever the one before did, a cancel of the plan
    // stopping none (a shutdown or the connection closing still do).
    let mut torn = Vec::new();
    for (i, step) in plan.phases[2].1.iter().enumerate().filter(|_| ran) {
        if session.live.is_none() {
            torn.push(
                json!({"phase": "teardown", "n": i + 1, "tool": step.tool, "ok": false,
                             "reason": "the session ended"}),
            );
            continue;
        }
        // A fade is waited out here too (the 0.1.12 live check: the teardown was refused under one), on a call of its
        // own so a cancel of the plan doesn't end the wait; then the step runs whatever quen's state shows.
        let t = Instant::now();
        if let Ok(cx) = ctx(session, &token, &[]) {
            let own = session.call(call.conn.clone(), Value::Null, None);
            let _ = cx.wait_for(&own, game::READY_MAX, game::in_game);
        }
        let mut step_args = step.args.clone();
        step_args["token"] = token["token"].clone();
        let (done, each) = session.nested(call, &step.tool, &step_args, false, false);
        let mut envelope = session.envelope(&step.tool, each, done);
        call.merge(&envelope);
        torn.push(row(
            "teardown",
            i + 1,
            step,
            &envelope,
            t.elapsed().as_secs_f64(),
        ));
        envelope["phase"] = json!("teardown");
        envelope["n"] = json!(i + 1);
        envelopes.push(envelope);
    }
    if ran {
        call.lap("teardown");
    }
    if let Some(live) = &session.live {
        live.in_plan.set(false);
    }
    fields.insert("took_s".into(), json!(took));
    fields.insert("steps".into(), json!(table));
    if !torn.is_empty() {
        fields.insert("teardown".into(), json!(torn));
    }
    if let Some(why) = &aborted {
        fields.insert("aborted".into(), json!(why));
    }
    let result = json!({"tag": plan.tag, "plan_sha256": sha, "estimate_s": estimate, "took_s": took,
                        "aborted": aborted, "steps": envelopes});
    let text = serde_json::to_string_pretty(&result).unwrap_or_default();
    let written = ctx(session, &token, &[]).and_then(|cx| file(&cx, call, &result_path, &text));
    match written {
        Ok(id) => fields.insert(
            "result".into(),
            json!({"path": result_path, "capture_id": id}),
        ),
        Err(why) => fields.insert("result_error".into(), json!(why)),
    };
    eprintln!(
        "{} quen: run_plan {} ({sha}) ended after {took} s: {}",
        session::utc_ms(SystemTime::now()),
        plan.tag,
        aborted.as_deref().unwrap_or("every step ok")
    );
    let torn_failed = torn.iter().filter(|r| r["ok"] != true).map(|r| {
        let tool = r["tool"].as_str().unwrap_or_default();
        format!(
            "teardown step {} ({tool}): {}",
            r["n"],
            r["reason"].as_str().unwrap_or_default()
        )
    });
    let failed: Vec<String> = aborted.into_iter().chain(torn_failed).collect();
    match failed.is_empty() {
        true => Ok(fields),
        false => Err(Done::Failed(failed.join("; "), fields)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::tests::{fake_game_until, reply, served, token};
    use crate::game::tests::Fake;
    use quen_core::protocol::{Request, Verb};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    /// A stand-in game for plans until the flag is set, the player at the origin: world.get reads the last settime
    /// back, exec calls any function but `nope`, scene.frame / scene.ray / cam.* serve game_frame, and world.actors
    /// one walker standing by.
    fn game(fx: &Fake) -> (std::thread::JoinHandle<Vec<Request>>, Arc<AtomicBool>) {
        let stop = Arc::new(AtomicBool::new(false));
        let time = Mutex::new(json!({"day": 1, "hour": 0}));
        let answer = Box::new(move |r: &Request| {
            let body = json!({"id": -7, "name": "Geralt", "entity": "geralt", "player": true, "pos": [0, 0, 0],
                "heading": 0, "head": [0, 0, 1.7], "torso": [0, 0, 1.2], "in_front": true, "x": 0, "y": 0,
                "share": 0.14, "box_whole": true, "distance": 3, "visible_points": 3,
                "visible": {"head": true, "torso": true, "origin": true}});
            let (ok, data) = match &r.verb {
                Verb::SetTime { day, hour } => {
                    *time.lock().unwrap() = json!({"day": day, "hour": hour});
                    (true, None)
                }
                Verb::WorldGet {} => (
                    true,
                    Some(
                        json!({"player": {"pos": [0, 0, 0], "heading": 0}, "weather": "WT_Clear",
                        "time": *time.lock().unwrap()}),
                    ),
                ),
                Verb::Exec { function, .. } => (
                    function != "nope",
                    Some(json!({"line": format!("{function}()"), "log": ""})),
                ),
                Verb::SceneFrame { .. } => (true, Some(json!({"found": 1, "entities": [body]}))),
                Verb::SceneRay { segments, .. } => {
                    let clear =
                        json!({"hit": false, "pos": null, "normal": null, "distance": null});
                    (true, Some(json!(vec![clear; segments.len() / 6])))
                }
                Verb::CamSet { pos, rot, fov } => (
                    true,
                    Some(json!({"camera": "photomode", "pos": pos, "rot": rot, "fov": fov})),
                ),
                Verb::CamNoclip { .. } | Verb::Notice { .. } | Verb::NoticeHide {} => (true, None),
                Verb::WorldActors { .. } => (
                    true,
                    Some(
                        json!({"found": 1, "actors": [{"id": 9, "pos": [1, 0.5, 0], "heading": 0,
                        "velocity": [0, 0, 0], "entity": "walker", "actor": true}]}),
                    ),
                ),
                v => panic!("{v:?}"),
            };
            vec![reply(r.id, ok, "ok", data)]
        });
        (fake_game_until(&token(fx), answer, stop.clone()), stop)
    }

    /// A served Fake whose state shows photo mode (valid and photo).
    fn photo(test: &str) -> Fake {
        let fx = served(test);
        let log = fx.dir.join("game/bin/x64_dx12/W3QuenState.log");
        fs::write(log, "push flags=129\n").unwrap();
        fx
    }

    fn run(session: &mut Session, args: Value) -> Value {
        let mut call = session.call(crate::mcp::Conn::nobody(), Value::Null, None);
        let done = super::call(session, &mut call, &args);
        session.envelope("run_plan", call, done)
    }

    /// run_plan's arguments for `plan`, as JSON text (a string: that text).
    fn args(plan: Value) -> Value {
        let text = match plan {
            Value::String(text) => text,
            plan => plan.to_string(),
        };
        json!({"token": "tok", "plan": text})
    }

    /// The design's accept: setup, a framed set (game_frame), a trace and an image metric as one call. The setup is
    /// noted for `again`, the steps and the teardown aren't; the plan and the result are files with captures rows, and
    /// the envelope lists every step's rows. The estimate stays within 20 % of the run for a plan of a wait, a trace
    /// window and verbs (here game_frame's sides take no screenshot, which its estimate counts).
    #[test]
    fn a_plan_runs_setup_a_framing_and_a_trace_as_one_call() {
        let fx = photo("plan-run");
        let (server, stop) = game(&fx);
        let mut session = fx.session();
        session.noted().clear();
        let shot = crate::image::tests::srgb8(1, 1, &[[255, 255, 255]]);
        let shot = crate::image::tests::stored(
            &fx,
            "A1.png",
            &shot,
            "printwindow",
            Some(quen_core::hdr::Encoding::Srgb8),
        );
        let plan = json!({
            "tag": "p1",
            "setup": [{"game_settime": {"day": 3, "hour": 12}}, {"game_exec": {"line": "preset(2)"}}],
            "steps": [
                {"game_frame": {"sides": ["front", "left"], "distance": 3}},
                {"measure_trace": {"tag": "walk", "secs": 1}},
                {"world_get": {}},
                {"measure_image": {"capture": "s1/A1.png", "metrics": ["luminance"]}},
            ],
            "teardown": [{"game_exec": {"line": "preset(0)"}}],
        });
        let out = run(&mut session, args(plan.clone()));
        assert_eq!(out["ok"], true, "{out}");
        let rows = out["steps"].as_array().unwrap();
        assert_eq!(rows.len(), 6, "{out}");
        assert!(rows.iter().all(|r| r["ok"] == true), "{out}");
        assert_eq!(rows[3]["tool"], "measure_trace");
        assert!(rows[3]["ledger"]["measurements"].is_array(), "{out}");
        assert_eq!(
            rows[5]["ledger"]["measurements"].as_array().unwrap().len(),
            4,
            "{out}"
        );
        assert_eq!(rows[5]["est_s"], 1.5, "{out}");
        assert!(
            out["estimate_excludes"]
                .as_str()
                .unwrap()
                .starts_with("fade waits"),
            "{out}"
        );
        assert_eq!(out["teardown"][0]["ok"], true, "{out}");
        // Only the setup is noted for again, in order, without the token.
        let noted: Vec<Value> = session.noted().clone();
        assert_eq!(
            noted,
            [
                json!({"tool": "game_settime", "args": {"day": 3, "hour": 12}}),
                json!({"tool": "game_exec", "args": {"line": "preset(2)"}}),
            ]
        );
        assert!(!session.live.as_ref().unwrap().in_plan.get());
        // The plan's canonical JSON and the result, each with a captures row, beside the trace's CSV.
        let dir = fx.dir.join("data/captures/s1");
        let saved = fs::read_to_string(dir.join("p1-plan.json")).unwrap();
        assert_eq!(saved, plan.to_string());
        assert_eq!(out["plan"]["sha256"], sha256_hex(saved.as_bytes()));
        let result = fs::read_to_string(dir.join("p1-result.json")).unwrap();
        let result: Value = serde_json::from_str(&result).unwrap();
        let steps = result["steps"].as_array().unwrap();
        assert_eq!(steps.len(), 7, "every envelope, the teardown's too");
        assert_eq!(steps[2]["sides"][1]["side"], "left");
        let db =
            quen_core::ledger::Ledger::open_readonly(fx.dir.join("data/state/quen.db")).unwrap();
        let sql = "SELECT path, method, id FROM captures WHERE id > ?1 ORDER BY id";
        let captures = db
            .query(sql, &[json!(shot)], Duration::from_secs(5))
            .unwrap()
            .rows;
        let named: Vec<&[Value]> = captures.iter().map(|r| &r[..2]).collect();
        assert_eq!(
            named,
            [
                [json!("s1/p1-plan.json"), json!("run_plan")],
                [json!("s1/walk-trace.csv"), json!("actor_trace")],
                [json!("s1/p1-result.json"), json!("run_plan")],
            ]
        );
        // The envelope lists them all, the trace step's CSV too.
        let ids: Vec<Value> = captures.iter().map(|r| r[2].clone()).collect();
        assert_eq!(out["ledger"]["captures"], json!(ids), "{out}");
        let sql = "SELECT DISTINCT capture_id, method_family FROM measurements WHERE metric LIKE 'luminance%'";
        let image = db.query(sql, &[], Duration::from_secs(5)).unwrap().rows;
        assert_eq!(image, [[json!(shot), json!("screenshot")]]);
        // A tag taken refuses the next plan before anything.
        let again = run(&mut session, args(plan));
        assert_eq!(again["refused"], true, "{again}");
        // The estimate against the run: a wait, a window and verbs, with the agent's controls (the status line up, so
        // the window waits for its hide).
        fx.shared.set_hands(Some(crate::hands::Hands::Agent));
        let timed = json!({"tag": "p2", "setup": [{"game_settime": {"day": 4, "hour": 9}}],
            "steps": [{"measure_trace": {"tag": "t2", "secs": 3}}, {"wait": 2}, {"game_exec": {"line": "x()"}},
                      {"world_get": {}}]});
        let out = run(&mut session, args(timed));
        stop.store(true, Ordering::Relaxed);
        server.join().unwrap();
        assert_eq!(out["ok"], true, "{out}");
        let (est, took) = (
            out["estimate_s"].as_f64().unwrap(),
            out["took_s"].as_f64().unwrap(),
        );
        assert!(
            (est - took).abs() <= 0.2 * took,
            "estimate {est} s, ran {took} s"
        );
    }

    /// The 0.1.12 live check (lc1b): a long move fades the game (flags 257), and the next step and the teardown were
    /// refused. Each waits for gameplay or photo mode first: here each exec line fades the game for 1 s.
    #[test]
    fn a_fade_is_waited_out_before_a_step_and_the_teardown() {
        let fx = photo("plan-fade");
        let log = fx.dir.join("game/bin/x64_dx12/W3QuenState.log");
        let stop = Arc::new(AtomicBool::new(false));
        let answer = Box::new(move |r: &Request| {
            let Verb::Exec { function, .. } = &r.verb else {
                panic!("{:?}", r.verb)
            };
            fs::write(&log, "push flags=257\n").unwrap();
            let log = log.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(1));
                fs::write(log, "push flags=1\n").unwrap();
            });
            let data = json!({"line": format!("{function}()"), "log": ""});
            vec![reply(r.id, function != "nope", "ok", Some(data))]
        });
        let server = fake_game_until(&token(&fx), answer, stop.clone());
        let mut session = fx.session();
        let plan = json!({"tag": "fade", "setup": [{"game_exec": {"line": "on()"}}],
            "steps": [{"game_exec": {"line": "nope()"}}],
            "teardown": [{"game_exec": {"line": "off()"}}]});
        let t = Instant::now();
        let out = run(&mut session, args(plan));
        stop.store(true, Ordering::Relaxed);
        let sent: Vec<String> = server
            .join()
            .unwrap()
            .into_iter()
            .filter_map(|r| match r.verb {
                Verb::Exec { function, .. } => Some(function),
                _ => None,
            })
            .collect();
        assert_eq!(sent, ["on", "nope", "off"], "{out}");
        assert!(
            out["aborted"]
                .as_str()
                .unwrap()
                .starts_with("steps step 1 (game_exec): "),
            "{out}"
        );
        assert_eq!(out["teardown"][0]["ok"], true, "{out}");
        assert!(t.elapsed() >= Duration::from_secs(2), "{:?}", t.elapsed());
        // The wait before the step, outside its own s.
        let before = out["steps"][1]["before_s"].as_f64().unwrap_or_default();
        assert!(before >= 0.5, "{out}");
    }

    /// A step that doesn't succeed ends the run there; the teardown still runs, every step of it; the steps after it
    /// never do.
    #[test]
    fn an_abort_runs_the_teardown_and_no_later_step() {
        let fx = photo("plan-abort");
        let (server, stop) = game(&fx);
        let mut session = fx.session();
        let plan = json!({"tag": "ab", "setup": [{"game_exec": {"line": "on()"}}],
            "steps": [{"game_exec": {"line": "nope()"}}, {"game_exec": {"line": "never()"}}],
            "teardown": [{"game_exec": {"line": "nope()"}}, {"game_exec": {"line": "off()"}}]});
        let out = run(&mut session, args(plan));
        stop.store(true, Ordering::Relaxed);
        let sent: Vec<String> = server
            .join()
            .unwrap()
            .into_iter()
            .filter_map(|r| match r.verb {
                Verb::Exec { function, .. } => Some(function),
                _ => None,
            })
            .collect();
        assert_eq!(sent, ["on", "nope", "nope", "off"]);
        let reason = out["reason"].as_str().unwrap_or_default();
        assert!(
            out["ok"] == false
                && out["refused"] == false
                && reason.starts_with("steps step 1 (game_exec): ")
                && reason.contains("; teardown step 1 (game_exec): "),
            "{out}"
        );
        let aborted = reason.split("; teardown").next().unwrap();
        assert_eq!(out["aborted"], aborted, "{out}");
        assert_eq!(out["teardown"][1]["ok"], true, "{out}");
        assert!(fx.dir.join("data/captures/s1/ab-result.json").is_file());
    }

    /// Refused before any step runs, nothing written: a bad plan, a step its tool's schema refuses or no plan may make,
    /// a clash, an estimate over max_s, one over 300 s outside a job. check runs nothing either.
    #[test]
    fn a_plan_is_checked_before_any_step_runs() {
        let fx = photo("plan-refuse");
        let mut session = fx.session();
        let step = |s: Value| json!({"tag": "x", "steps": [s]});
        for (plan, why) in [
            (json!("{"), "plan: "),
            (
                json!({"tag": "x", "steps": [], "after": []}),
                "unknown key \"after\"",
            ),
            (
                json!({"tag": "a b", "steps": [{"world_get": {}}]}),
                "tag is",
            ),
            (
                json!({"tag": "x", "steps": []}),
                "at least one in setup or steps",
            ),
            (
                step(json!({"session_launch": {}})),
                "isn't a tool a plan calls",
            ),
            (step(json!({"game_save": {}})), "isn't a tool a plan calls"),
            (step(json!({"mem_patch": {}})), "isn't a tool a plan calls"),
            (step(json!({"world_get": {}, "wait": 1})), "a step is one"),
            (
                step(json!({"game_settime": {"day": 1}})),
                "hour is required",
            ),
            (
                step(json!({"game_exec": {"line": "x()", "tok": 1}})),
                "unknown argument \"tok\"",
            ),
            (
                step(json!({"game_settime": {"day": "x", "hour": 1}})),
                "game_settime.day: not of type integer",
            ),
            (
                step(json!({"game_frame": {"sides": []}})),
                "game_frame.sides: 0 items",
            ),
            (
                step(json!({"game_frame": {"distance": 0.1}})),
                "0.1 is under 0.5",
            ),
            (
                step(json!({"game_photo": {"action": "fly"}})),
                "game_photo.action: one of",
            ),
            (
                step(json!({"world_get": {"token": "tok"}})),
                "no token in a step",
            ),
            (
                step(json!({"game_keys": {"tokens": ["nokey"]}})),
                "game_keys: ",
            ),
            (
                step(json!({"measure_ab": {"tag": "ab", "states": [
                    {"label": "A", "actions": []}, {"label": "A", "actions": []}]}})),
                "measure_ab: both states are labelled",
            ),
            (step(json!({"wait": 0})), "wait is seconds"),
            (
                json!({"tag": "x", "steps": [{"world_get": {}}], "teardown": [{"wait": 1}]}),
                "no wait in teardown",
            ),
            (
                json!({"tag": "x", "steps": [{"measure_trace": {"tag": "t"}}, {"game_screenshot": {"tag": "t"}}]}),
                "tag \"t\" twice",
            ),
            (
                json!({"tag": "x", "steps": [{"wait": 100}], "max_s": 50}),
                "over max_s 50 s",
            ),
            (
                json!({"tag": "x", "steps": [{"wait": 400}]}),
                "runs as a job",
            ),
        ] {
            let out = run(&mut session, args(plan.clone()));
            let reason = out["reason"].as_str().unwrap_or_default();
            assert!(
                out["refused"] == true && reason.contains(why),
                "{plan}: {out}"
            );
        }
        // check: the steps and the estimate, nothing run or written.
        let plan = json!({"tag": "c", "setup": [{"game_settime": {"day": 1, "hour": 2}}],
            "steps": [{"measure_ab": {"tag": "ab", "settle": 2, "secs": 3, "states": [
                {"label": "A", "actions": [{"cvar": "X/Y", "set": 1}]},
                {"label": "B", "actions": [{"cvar": "X/Y", "set": 2}]}]}}, {"wait": 1}]});
        let mut check = args(plan);
        check["check"] = json!(true);
        let out = run(&mut session, check);
        assert_eq!(out["ok"], true, "{out}");
        // settime 0.2, 4 windows of 2 + 3 + 0.5 + the status line's 0.5 s hide, the wait 1; and the plan's measuring
        // line, confirmed after 0.5 s, up 5 s before its first window: 0.5 + 4.8 s more after the settime.
        assert_eq!(out["estimate_s"], 30.5, "{out}");
        assert_eq!(out["steps"][1]["est_s"], 24.0, "{out}");
        assert!(!fx.dir.join("data/captures/s1").exists(), "nothing written");
    }
}
