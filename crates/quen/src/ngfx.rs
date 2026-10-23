//! Nsight Graphics frame captures without Nsight's window (nsight-capture design §4, phase 1). `ngfx_extract` reads a
//! capture file only: ngfx-replay's print-and-exit options, and quen's reader of the file itself (quen-core's
//! `ngfx_capture`), checked against ngfx-replay's lists of the same capture.
//! `ngfx_replay` replays it on the GPU (a perf report, or GPU Trace around the replay), which the client asks the user
//! about on every call. Only the configured Nsight install's ngfx-replay runs, never the replayer a capture bundles,
//! each in a job of its own, without a window. A capture made outside quen is imported by a hard link into
//! `<capture root>\imports\<its sha256's first 12>\`; outputs go beside the capture, in a folder named by its stem.
//! Each output file is a captures row; each file-only answer an extraction, read back instead of run again; counts
//! and replay times are measurements.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, BufReader, Read};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::OwnedHandle;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use quen_core::journal::sha256_file;
use quen_core::ledger::{Capture, CaptureRef, Extraction, Ledger, Measurement, Tag};
use quen_core::{ngfx, ngfx_calls, ngfx_capture};
use serde_json::{Map, Value, json};

use crate::config::Config;
use crate::game::Fields;
use crate::session::{self, Call, Done, Session};
use crate::win::{self, Child, Job};
use crate::{doctor, profile};

/// The file-only operations, in their default order: the output file and ngfx-replay's option (none: quen's reader).
const OPS: [(&str, &str, Option<&str>); 10] = [
    ("metadata", "metadata.json", Some("--metadata")),
    ("screenshot", "final.png", Some("--metadata-screenshot")),
    ("functions", "functions.json", Some("--metadata-functions")),
    ("objects", "objects.json", Some("--metadata-objects")),
    ("errors", "errors.txt", Some("--metadata-logs-errors")),
    ("shaders", "shaders.tsv", None),
    ("object_table", "object_table.tsv", None),
    ("pipelines", "pipelines.tsv", None),
    ("calls", "calls.tsv", None),
    ("raytracing", "blas_builds.tsv", None),
];
/// The ops a reader op checks against or reads from (ngfx-replay's lists of the same capture, the metadata's Nsight
/// version): run first, in the same call.
fn inputs(op: &str) -> &'static [&'static str] {
    match op {
        "object_table" | "pipelines" => &["metadata", "objects"],
        "calls" | "raytracing" => &["metadata", "functions", "objects"],
        _ => &[],
    }
}
/// How long one print-and-exit run of ngfx-replay may take, and a replay.
const PRINT_LIMIT: Duration = Duration::from_secs(600);
const REPLAY_LIMIT: Duration = Duration::from_secs(900);
/// quen's shader decoder and capture reader: their revisions are part of their profiles, so a fix misses the cache.
const DECODER_REVISION: u32 = 2;
const READER_REVISION: u32 = 1;
/// The longest capture path ngfx-replay opens: Windows' MAX_PATH less its NUL. At 263 characters it printed "File does
/// not exist" (2026-10-23).
const MAX_PATH: usize = 259;

/// Why ngfx-replay can't open `path`, if it is too long.
fn too_long(path: &Path) -> Option<String> {
    let n = path.as_os_str().encode_wide().count();
    (n > MAX_PATH).then(|| {
        format!(
            "{} is {n} characters: ngfx-replay opens at most {MAX_PATH} (Windows' MAX_PATH); set a shorter \
             capture_root in quen's config (quen_setup)",
            path.display()
        )
    })
}

pub fn tools() -> Vec<Value> {
    let capture_id = json!({"type": "integer", "description": "a captures row of kind ngfx-capture (an earlier result's capture.id)"});
    vec![
        json!({
            "name": "ngfx_extract",
            "description": "Reads an Nsight Graphics frame capture (.ngfx-capture), the file only: no GPU, no window, \
                no prompt. ops (default all, each in ops' description): ngfx-replay's print-and-exit lists, and quen's \
                reader of the file itself, checked against those lists (added to ops when missing); a file that \
                doesn't line up fails that op: layout not recognised. Outputs go beside the capture in a folder named \
                by its stem, each a captures row; an op answered before for the same file is read back (cached). \
                path: a capture outside the capture root is hard-linked into imports\\ (same volume only), never \
                moved or changed. Results: counts, paths, no raw metadata. Refused: another quen holds the game lock; \
                with needs: no ngfx-replay beside ngfx in the config.",
            "inputSchema": {"type": "object", "properties": {
                "capture_id": capture_id,
                "path": {"type": "string", "description": "instead of capture_id: an absolute path to a .ngfx-capture (under the capture root, or imported by a hard link)"},
                "ops": {"type": "array", "items": {"type": "string", "enum": OPS.map(|(op, ..)| op)}, "description": "default all. metadata: metadata.json, cleaned (variable names, Nsight's own knobs, user paths as %USERPROFILE%, no computer or user name); facts: frame, resolution, API, features, Nsight version. screenshot: final.png, the frame as presented. functions: functions.json; counts of events, draws, indirect calls, dispatches, acceleration-structure builds, ray dispatches. objects: objects.json; counts by type. errors: errors.txt, logged errors. shaders: shaders\\<hash>.dxbc, shaders.tsv (the shader chunks); found, decoded, failed. quen's reader: object_table: object_table.tsv, every object with its type, creator and creation record. pipelines: pipelines.tsv, each pipeline state and state object with the shaders its creation record names (stage, chunk, hash). calls: calls.tsv, every event of functions with its this, CPU time, a draw's or dispatch's pipeline and its arguments (decoded for the draws, dispatches, pipeline and root bindings, barriers, render targets, copies, ray-tracing builds and dispatches, submits, signals and waits; raw hex for the rest). raytracing: blas_builds.tsv (each bottom-level build's geometry) and tlas_<n>.tsv (each top-level build's instances: id, mask, hit group, flags, BLAS, transform), when the file holds them."},
            }, "additionalProperties": false},
        }),
        json!({
            "name": "ngfx_replay",
            "description": "Replays a capture on the GPU, with the user's OK: the client asks them on every call \
                (user_ok is only your word). op perf: ngfx-replay 20 times, hidden (perf-<time>\\: \
                iteration_times.csv and Nsight's images); rows replay_image_error (iteration 0: how complete the \
                capture is), replay_gpu_frame_ms and replay_cpu_frame_ms (medians of the later iterations). op \
                gputrace: Nsight GPU Trace around 10 replays, may show a window (gputrace-<time>\\: .ngfx-gputrace, \
                .xls tables); row gpu_frame_ms, GPU clocks locked to base: relative only. Each takes the GPU for \
                about a minute. A replay is not the game: compare only replays of one capture. Every file becomes a \
                captures row with no boot. Refused, nothing run: no user_ok, a session live, the game running, \
                another quen holding the game lock, no metadata extraction yet (ngfx_extract), a capture newer than \
                the installed Nsight; with needs: no ngfx-replay.",
            // Claude Code prompts for this tool on every call, in every permission mode, as for session_launch.
            "_meta": {"anthropic/requiresUserInteraction": true},
            "inputSchema": {"type": "object", "properties": {
                "capture_id": capture_id,
                "op": {"type": "string", "enum": ["perf", "gputrace"], "description": "perf: replay timings and completeness; gputrace: GPU Trace of the replay"},
                "user_ok": {"type": "boolean", "description": "true only after telling the user in this conversation that the GPU replays the capture for about a minute, and they agreed. Required."},
            }, "required": ["capture_id", "op"], "additionalProperties": false},
        }),
    ]
}

/// The session-thread Nsight tools.
pub(crate) fn call(session: &mut Session, call: &mut Call, name: &str, args: &Value) -> Done {
    let out = match name {
        "ngfx_extract" => extract(session, call, args),
        "ngfx_replay" => replay(session, call, args),
        _ => Err(Done::Refused(format!("unknown tool {name:?}"))),
    };
    out.map_or_else(|done| done, Done::Ok)
}

fn open(config: &Config) -> Result<Ledger, String> {
    let dir = config.state_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    Ledger::open(dir.join("quen.db")).map_err(|e| format!("ledger: {e}"))
}

/// The configured install's ngfx-replay.exe and its version, or refused with needs.
fn replayer(config: &Config, tool: &str) -> Result<(PathBuf, Option<String>), Done> {
    let exe = doctor::require(&doctor::NGFX_REPLAY, config, tool)?
        .path
        .unwrap_or_default();
    let version = win::file_version(&exe);
    Ok((exe, version))
}

/// `rel` (relative to the capture root, `/` separators) as a path under `root`.
fn under(root: &Path, rel: &str) -> PathBuf {
    root.join(rel.replace('/', "\\"))
}

/// `path` relative to `root`, `/` separators.
fn rel(root: &Path, path: &Path) -> String {
    let parts: Vec<_> = path
        .strip_prefix(root)
        .unwrap_or(path)
        .iter()
        .map(|p| p.to_string_lossy())
        .collect();
    parts.join("/")
}

/// `name` in `dir`, else `<stem>-2.<ext>`, `-3`, ...: the first no captures row has (a path stays the row's after
/// capture_clean took its file).
fn free(ledger: &Ledger, root: &Path, dir: &Path, name: &str) -> Result<PathBuf, String> {
    let (stem, ext) = name.rsplit_once('.').unwrap_or((name, ""));
    for n in 1.. {
        let file = match n {
            1 => name.to_owned(),
            n if ext.is_empty() => format!("{stem}-{n}"),
            n => format!("{stem}-{n}.{ext}"),
        };
        let path = dir.join(file);
        let taken = ledger.capture_ref(None, Some(&rel(root, &path)));
        if taken.map_err(|e| e.to_string())?.is_none() {
            return Ok(path);
        }
    }
    unreachable!("an unbounded range")
}

/// The lowercase extension a captures row's kind is (`file` without one).
fn kind(path: &Path) -> String {
    path.extension()
        .map_or("file".into(), |e| e.to_string_lossy().to_lowercase())
}

/// A captures row for a file under the capture root (`sha`: its sha256, when known already).
fn row(
    root: &Path,
    path: &Path,
    sha: Option<String>,
    boot: Option<i64>,
    method: &str,
    t: SystemTime,
) -> Result<Capture, String> {
    let failed = |e: io::Error| format!("{}: {e}", path.display());
    let bytes = fs::metadata(path).map_err(failed)?.len();
    let sha256 = match sha {
        Some(sha) => sha,
        None => sha256_file(path).map_err(failed)?,
    };
    Ok(Capture {
        boot_id: boot,
        path: rel(root, path),
        sha256,
        bytes: bytes as i64,
        kind: kind(path),
        method: Some(method.into()),
        t: session::utc(t),
        ..Capture::default()
    })
}

/// The capture a call names: its row and file, and where its outputs go.
struct Target {
    row: CaptureRef,
    file: PathBuf,
    /// Beside the capture, named by its stem.
    out: PathBuf,
    /// This call recorded it: hard-linked in, or a file already under the capture root that had no row.
    imported: bool,
}

/// The capture `args` names: `capture_id`, or (with `import`) `path`, recorded when it has no row ([`import`]).
fn target(
    config: &Config,
    ledger: &mut Ledger,
    call: &mut Call,
    args: &Value,
    import: bool,
) -> Result<Target, String> {
    let root = config.capture_root();
    let (row, imported) = match (args.get("capture_id"), args.get("path")) {
        (Some(id), None) => {
            let id = id.as_i64().ok_or("capture_id must be an integer")?;
            let row = ledger
                .capture_ref(Some(id), None)
                .map_err(|e| e.to_string())?;
            (row.ok_or(format!("no captures row {id}"))?, false)
        }
        (None, Some(path)) if import => {
            let path = path.as_str().ok_or("path must be a string")?;
            self::import(config, ledger, call, Path::new(path))?
        }
        _ if import => return Err("give capture_id or path".into()),
        _ => return Err("give capture_id (ngfx_extract imports a path)".into()),
    };
    let file = under(&root, &row.path);
    if row.kind != "ngfx-capture" {
        return Err(format!(
            "captures row {} is a {}, not an ngfx-capture",
            row.id, row.kind
        ));
    }
    if row.removed {
        return Err(format!(
            "captures row {}'s file was removed (capture_clean): import it again by path",
            row.id
        ));
    }
    let bytes = fs::metadata(&file).map(|m| m.len() as i64);
    if bytes.as_ref().ok() != Some(&row.bytes) {
        return Err(format!(
            "{} isn't the file captures row {} recorded ({} bytes): gone or changed",
            file.display(),
            row.id,
            row.bytes
        ));
    }
    Ok(Target {
        out: file.with_extension(""),
        row,
        file,
        imported,
    })
}

/// The capture at `path`: its row when it is under the capture root (recorded now if it has none); else hard-linked
/// into `imports\<sha256 12>\` (a link made before is found by its file id, not hashed again) and recorded, with no
/// boot. Refused: not an absolute path to a .ngfx-capture, or on another volume than the capture root (a link can't
/// cross volumes). The original is never moved or changed.
fn import(
    config: &Config,
    ledger: &mut Ledger,
    call: &mut Call,
    path: &Path,
) -> Result<(CaptureRef, bool), String> {
    let named = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("ngfx-capture"));
    if !path.is_absolute() || !named || !path.is_file() {
        return Err(format!(
            "path {}: not an absolute path to a .ngfx-capture file",
            path.display()
        ));
    }
    let root = config.capture_root();
    fs::create_dir_all(&root).map_err(|e| format!("{}: {e}", root.display()))?;
    let canon = |p: &Path| fs::canonicalize(p).map_err(|e| format!("{}: {e}", p.display()));
    let (root_c, source) = (canon(&root)?, canon(path)?);
    let id = |p: &Path| win::file_id(p).map_err(|e| format!("{}: {e}", p.display()));
    let found = |rel: &str| {
        ledger
            .capture_ref(None, Some(rel))
            .map_err(|e| e.to_string())
    };
    let (dest, sha) = match source.strip_prefix(&root_c) {
        Ok(inside) => {
            let rel = rel(Path::new(""), inside);
            if let Some(row) = found(&rel)? {
                return Ok((row, false));
            }
            if let Some(why) = too_long(&under(&root, &rel)) {
                return Err(why);
            }
            (under(&root, &rel), None)
        }
        Err(_) => {
            let file_id = id(&source)?;
            let volume = |id: &str| id.split('-').next().map(str::to_owned);
            if volume(&file_id) != volume(&id(&root_c)?) {
                return Err(format!(
                    "{} is on another volume than the capture root {}: a hard link can't cross volumes; copy it \
                     into the capture root first",
                    path.display(),
                    root.display()
                ));
            }
            let imports = root.join("imports");
            let earlier = fs::read_dir(&imports)
                .into_iter()
                .flatten()
                .flatten()
                .flat_map(|d| fs::read_dir(d.path()).into_iter().flatten().flatten())
                .map(|f| f.path())
                .find(|f| win::file_id(f).is_ok_and(|i| i == file_id));
            if let Some(row) = earlier
                .map(|f| found(&rel(&root, &f)))
                .transpose()?
                .flatten()
            {
                return Ok((row, false));
            }
            call.progress("hashing the capture");
            let sha = sha256_file(&source).map_err(|e| format!("{}: {e}", path.display()))?;
            let name = source
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let dir = (1..)
                .map(|n| match n {
                    1 => imports.join(&sha[..12]),
                    n => imports.join(format!("{}-{n}", &sha[..12])),
                })
                .find(|d| !found(&rel(&root, &d.join(&name))).is_ok_and(|r| r.is_some()))
                .unwrap_or_default();
            let dest = dir.join(&name);
            if let Some(why) = too_long(&dest) {
                return Err(why);
            }
            match win::file_id(&dest) {
                Ok(i) if i == file_id => {}
                Ok(_) => return Err(format!("{} exists and is another file", dest.display())),
                Err(_) => {
                    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
                    fs::hard_link(&source, &dest)
                        .map_err(|e| format!("linking {}: {e}", dest.display()))?;
                }
            }
            (dest, Some(sha))
        }
    };
    let t = fs::metadata(&dest)
        .and_then(|m| m.modified())
        .unwrap_or_else(|_| SystemTime::now());
    if sha.is_none() {
        call.progress("hashing the capture");
    }
    let capture = row(&root, &dest, sha, None, "ngfx_capture", t)?;
    let id = ledger.add_capture(&capture).map_err(|e| e.to_string())?;
    call.row("captures", id);
    let row = ledger
        .capture_ref(Some(id), None)
        .map_err(|e| e.to_string())?;
    Ok((row.ok_or("the capture's new row is gone")?, true))
}

/// Runs `f` under the game lock: the live session's, else taken for the call (only its holder writes rows about a
/// boot), then released.
fn locked<T>(
    session: &mut Session,
    config: &Config,
    tool: &str,
    f: impl FnOnce() -> Result<T, Done>,
) -> Result<T, Done> {
    let held = session.live.is_some();
    if !held {
        session.take_lock(config, tool)?;
    }
    let out = f();
    if !held {
        session.release_lock();
    }
    out
}

fn extract(session: &mut Session, call: &mut Call, args: &Value) -> Result<Fields, Done> {
    crate::ledger::object(args, &["capture_id", "path", "ops"])?;
    let config = session.config()?.clone();
    let asked: Vec<&str> = match args.get("ops") {
        None => OPS.iter().map(|(op, ..)| *op).collect(),
        Some(Value::Array(asked)) => {
            let mut ops = Vec::new();
            for op in asked {
                let op = op
                    .as_str()
                    .and_then(|op| OPS.iter().find(|(o, ..)| *o == op))
                    .ok_or(format!(
                        "ops: {op} is not one of {}",
                        OPS.map(|(o, ..)| o).join(", ")
                    ))?;
                ops.push(op.0);
            }
            ops
        }
        Some(_) => return Err("ops must be a list".to_string().into()),
    };
    // In OPS' order, each op's inputs before it.
    let ops: Vec<&str> = OPS
        .iter()
        .map(|(op, ..)| *op)
        .filter(|op| asked.contains(op) || asked.iter().any(|a| inputs(a).contains(op)))
        .collect();
    let needs_replayer = ops
        .iter()
        .any(|op| OPS.iter().any(|(o, _, flag)| o == op && flag.is_some()));
    let replay = needs_replayer
        .then(|| replayer(&config, "ngfx_extract"))
        .transpose()?;
    locked(session, &config, "ngfx_extract", || {
        let mut ledger = open(&config)?;
        let t = target(&config, &mut ledger, call, args, true)?;
        if let Some(why) = too_long(&t.file).filter(|_| replay.is_some()) {
            return Err(why.into());
        }
        call.lap("capture");
        fs::create_dir_all(&t.out).map_err(|e| format!("{}: {e}", t.out.display()))?;
        let (mut results, mut failed) = (Vec::new(), Vec::new());
        // The outputs so far, for the ops that read them, and the capture's Nsight version from its metadata.
        let (mut files, mut nsight) = (HashMap::new(), None);
        for op in ops {
            let missing = inputs(op).iter().find(|i| !files.contains_key(*i));
            let result = match (call.stopped(), missing) {
                (true, _) => Err(call.stop_cause().to_owned()),
                (_, Some(i)) => Err(format!("needs op {i}, which failed")),
                _ => one(
                    &config,
                    &mut ledger,
                    call,
                    &t,
                    op,
                    replay.as_ref(),
                    (&files, nsight.as_deref()),
                ),
            };
            match result {
                Ok(v) => {
                    if let Some(file) = v["file"].as_str() {
                        files.insert(op, PathBuf::from(file));
                    }
                    if let Some(v) = v["facts"]["nsight_version"].as_str() {
                        nsight = Some(v.to_owned());
                    }
                    results.push(v);
                }
                Err(why) => {
                    failed.push(op);
                    results.push(json!({"op": op, "error": why}));
                }
            }
            call.lap(op);
        }
        let links = win::file_info(&t.file).map(|(_, n)| n).ok();
        let fields = Fields::from_iter([
            (
                "capture".into(),
                json!({"id": t.row.id, "path": t.file, "sha256": t.row.sha256, "bytes": t.row.bytes,
                       "boot_id": t.row.boot_id, "imported": t.imported, "links": links, "outputs": t.out}),
            ),
            (
                "ngfx_replay_version".into(),
                json!(replay.and_then(|r| r.1)),
            ),
            ("ops".into(), json!(results)),
        ]);
        match failed.is_empty() {
            true => Ok(fields),
            false => Err(Done::Failed(
                format!("{} failed (each op says why)", failed.join(", ")),
                fields,
            )),
        }
    })
}

/// The extraction `op` of a capture whose sha256 is `target`: under ngfx-replay `version`'s flags, or quen's decoder,
/// or quen's reader (checked against that ngfx-replay's lists).
fn extraction<'a>(op: &'a str, version: Option<&'a str>, target: &'a str) -> Extraction<'a> {
    let flag = OPS
        .iter()
        .find(|(o, ..)| *o == op)
        .and_then(|(.., flag)| *flag);
    let quen = Some(env!("CARGO_PKG_VERSION"));
    let (tool, tool_version, provider, parameters) = match flag {
        Some(flag) => (
            "ngfx_replay",
            version,
            json!({"id": "ngfx_replay", "name": "NVIDIA Nsight Graphics ngfx-replay"}),
            json!({"args": [flag]}),
        ),
        None if op == "shaders" => (
            "quen",
            quen,
            json!({"id": "quen", "name": "quen's shader decoder"}),
            json!({"decoder": "lz4_dxbc", "revision": DECODER_REVISION}),
        ),
        None => (
            "quen",
            quen,
            json!({"id": "quen", "name": "quen's capture reader"}),
            json!({"reader": "ngfx_capture", "revision": READER_REVISION, "ngfx_replay": version}),
        ),
    };
    Extraction {
        tool,
        tool_version,
        provider,
        parameters,
        target_sha256: target,
        operation: op,
        result_sha256: "",
        capture_id: 0,
    }
}

/// The family of an op's counts: facts read from decoded bytecode are static_re (playbook §2), the rest nsight's.
fn family(op: &str) -> (&'static str, Tag) {
    match op {
        "shaders" | "pipelines" => ("static_re", Tag::StaticRe),
        _ => ("nsight", Tag::M),
    }
}

/// One file-only op on `t`: read back when answered before for the same file, else run and recorded (its output's
/// captures row, the extraction, its counts). What the result says of it. `earlier`: the outputs of this call's
/// earlier ops, and the capture's Nsight version.
fn one(
    config: &Config,
    ledger: &mut Ledger,
    call: &mut Call,
    t: &Target,
    op: &str,
    replay: Option<&(PathBuf, Option<String>)>,
    earlier: (&HashMap<&str, PathBuf>, Option<&str>),
) -> Result<Value, String> {
    let (_, name, flag) = OPS
        .into_iter()
        .find(|(o, ..)| *o == op)
        .ok_or("unknown op")?;
    let x = extraction(op, replay.and_then(|r| r.1.as_deref()), &t.row.sha256);
    let root = config.capture_root();
    let hit = ledger.extraction_hit(&x.key()).map_err(|e| e.to_string())?;
    let (cached, extraction, output, file, mut out) = match hit {
        Some((id, row)) if under(&root, &row.path).is_file() => {
            let file = under(&root, &row.path);
            (true, id, row.id, file, json!({}))
        }
        _ => {
            let file = free(ledger, &root, &t.out, name)?;
            let replay_exe = replay.map(|r| r.0.as_path());
            let (out, counts) = run(op, &file, t, replay_exe, earlier.0, call)?;
            let output = row(
                &root,
                &file,
                None,
                t.row.boot_id,
                "ngfx_extract",
                SystemTime::now(),
            )?;
            let note = format!("ngfx_extract {op} of capture {}", t.row.id);
            let ids = ledger
                .atomic(|l| {
                    let capture_id = l.add_capture(&output)?;
                    let x = Extraction {
                        result_sha256: &output.sha256,
                        capture_id,
                        ..x.clone()
                    };
                    let extraction = l.add_extraction(&x)?;
                    let (family, tag) = family(op);
                    let mut measured = Vec::new();
                    for (metric, value) in &counts {
                        measured.push(l.add_measurement(&Measurement {
                            boot_id: t.row.boot_id,
                            capture_id: Some(capture_id),
                            metric: (*metric).into(),
                            value: Some(*value as f64),
                            unit: Some("count".into()),
                            state_label: None,
                            method_family: family.into(),
                            tag,
                            run_id: None,
                            note: Some(note.clone()),
                            extraction_id: (family == "static_re").then_some(extraction),
                            world: None,
                        })?);
                    }
                    Ok((capture_id, extraction, measured))
                })
                .map_err(|e| e.to_string())?;
            call.row("captures", ids.0);
            call.row("extractions", ids.1);
            for id in ids.2 {
                call.row("measurements", id);
            }
            (false, ids.1, ids.0, file, out)
        }
    };
    call.artifact(&file);
    out["op"] = json!(op);
    out["cached"] = json!(cached);
    out["file"] = json!(file);
    out["capture_id"] = json!(output);
    out["extraction_id"] = json!(extraction);
    match op {
        "metadata" => {
            let text = fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
            let cleaned: Value =
                serde_json::from_str(&text).map_err(|e| format!("{}: {e}", file.display()))?;
            let facts = ngfx::metadata_facts(&cleaned);
            ledger
                .fill_frame_state(t.row.id, &facts)
                .map_err(|e| e.to_string())?;
            out["facts"] = json!(facts);
        }
        _ => {
            let sql = "SELECT metric, value FROM measurements WHERE capture_id = ?1 ORDER BY id";
            let rows = ledger
                .query(sql, &[json!(output)], Duration::from_secs(10))
                .map_err(|e| e.to_string())?;
            let counts: Map<String, Value> = rows
                .rows
                .into_iter()
                .filter_map(|r| Some((r.first()?.as_str()?.to_owned(), r.get(1)?.clone())))
                .collect();
            if !counts.is_empty() {
                out["counts"] = json!(counts);
            }
        }
    }
    if flag.is_none() {
        out["decoder_proven_on"] = json!(ngfx::DECODER_PROVEN_ON);
        if earlier.1.is_some_and(|v| v != ngfx::DECODER_PROVEN_ON) {
            out["note"] = json!(format!(
                "decoder proven on Nsight {} only",
                ngfx::DECODER_PROVEN_ON
            ));
        }
    }
    Ok(out)
}

/// The capture's own file, open for quen's reader.
fn capture(t: &Target) -> Result<ngfx_capture::Capture<BufReader<File>>, String> {
    let file = File::open(&t.file).map_err(|e| format!("{}: {e}", t.file.display()))?;
    ngfx_capture::Capture::open(BufReader::new(file))
}

/// The text of an earlier op's output.
fn input(earlier: &HashMap<&str, PathBuf>, op: &str) -> Result<String, String> {
    let file = earlier.get(op).ok_or(format!("no {op} output"))?;
    fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))
}

/// This machine's values of quen-core's `IDENTITY` (a quen launch's allow-list leaves some out of a game's
/// environment, so a capture's own environment can't name them all).
fn local() -> Vec<(&'static str, String)> {
    ngfx::IDENTITY
        .iter()
        .filter_map(|k| Some((*k, std::env::var(k).ok()?)))
        .collect()
}

/// `text` with this machine's identity scrubbed, as `%USERPROFILE%`, ...
fn scrubbed(text: &str) -> String {
    let local = local();
    let local: Vec<(&str, &str)> = local.iter().map(|(k, v)| (*k, v.as_str())).collect();
    ngfx::scrub_identity(text, &ngfx::identity(&local, &[]))
}

/// Runs `op` on `t`'s capture into `file`: the result's own fields and the counts to record.
fn run(
    op: &str,
    file: &Path,
    t: &Target,
    replay: Option<&Path>,
    earlier: &HashMap<&str, PathBuf>,
    call: &mut Call,
) -> Result<(Value, ngfx::Counts), String> {
    let write =
        |bytes: &[u8]| fs::write(file, bytes).map_err(|e| format!("{}: {e}", file.display()));
    let log = t.out.join(format!("ngfx-replay-{op}.log"));
    let printed = |args: &[String], call: &mut Call| {
        let exe = replay.ok_or("no ngfx-replay")?;
        call.progress(&format!("ngfx-replay {}", args[0]));
        print(call, exe, &t.file, args, &log)
    };
    match op {
        "metadata" => {
            let raw = printed(&["--metadata".into()], call)?;
            let local = local();
            let local: Vec<(&str, &str)> = local.iter().map(|(k, v)| (*k, v.as_str())).collect();
            let cleaned = ngfx::clean_metadata(&String::from_utf8_lossy(&raw), &local)?;
            drop(raw);
            write(
                serde_json::to_string_pretty(&cleaned.json)
                    .unwrap_or_default()
                    .as_bytes(),
            )?;
            Ok((
                json!({"unknown_fields": cleaned.unknown_fields}),
                Vec::new(),
            ))
        }
        "screenshot" => {
            printed(
                &["--metadata-screenshot".into(), file.display().to_string()],
                call,
            )?;
            match fs::metadata(file) {
                Ok(m) if m.len() > 0 => Ok((json!({}), Vec::new())),
                _ => Err(format!(
                    "ngfx-replay wrote no {} (its log: {})",
                    file.display(),
                    log.display()
                )),
            }
        }
        "functions" | "objects" => {
            let raw = printed(&[format!("--metadata-{op}")], call)?;
            let text = String::from_utf8_lossy(&raw);
            let counts = match op {
                "functions" => ngfx::count_functions(&text)?,
                _ => ngfx::count_objects(&text)?,
            };
            write(&raw)?;
            Ok((json!({}), counts))
        }
        "errors" => {
            let raw = printed(&["--metadata-logs-errors".into()], call)?;
            write(&raw)?;
            Ok((json!({}), Vec::new()))
        }
        "object_table" | "pipelines" | "calls" | "raytracing" => {
            call.progress(&format!("reading the capture's {op}"));
            let mut capture = capture(t)?;
            let objects = capture.objects()?;
            let listed = ngfx_capture::listed(&input(earlier, "objects")?)?;
            let names = ngfx_capture::check_objects(&objects, &listed)?;
            match op {
                "object_table" => {
                    let table = ngfx_capture::object_table(&mut capture, &objects, &names)?;
                    write(scrubbed(&table).as_bytes())?;
                    Ok((
                        json!({}),
                        vec![("capture_object_records", objects.len() as u64)],
                    ))
                }
                "pipelines" => {
                    let shaders = capture.shaders()?;
                    let j = ngfx_capture::pipelines(&mut capture, &objects, &names, &shaders)?;
                    write(scrubbed(&j.tsv).as_bytes())?;
                    let counts = vec![
                        ("capture_pipelines", j.pipelines as u64),
                        ("capture_pipelines_with_shaders", j.with_shaders as u64),
                        ("capture_pipeline_shader_links", j.links as u64),
                    ];
                    Ok((json!({}), counts))
                }
                "raytracing" => {
                    let events = ngfx_calls::events(&input(earlier, "functions")?)?;
                    let rt = ngfx_calls::raytracing(&mut capture, &events, &objects, &names)?;
                    write(scrubbed(&rt.blas).as_bytes())?;
                    let mut tables = Vec::new();
                    for (k, tlas) in rt.tlas.iter().enumerate() {
                        let path = match &tlas.tsv {
                            Some(tsv) => {
                                let path = t.out.join(format!("tlas_{}.tsv", k + 1));
                                fs::write(&path, scrubbed(tsv))
                                    .map_err(|e| format!("{}: {e}", path.display()))?;
                                Some(path)
                            }
                            None => None,
                        };
                        tables.push(
                            json!({"tlas": k + 1, "event": tlas.event, "instances": tlas.instances,
                                           "named": tlas.named, "file": path}),
                        );
                    }
                    let instances: u32 = rt.tlas.iter().map(|t| t.instances).sum();
                    let named: usize = rt.tlas.iter().map(|t| t.named).sum();
                    let unread = rt.tlas.iter().filter(|t| t.tsv.is_none()).count();
                    let counts = vec![
                        ("capture_blas_builds", rt.blas_builds as u64),
                        ("capture_blas_geometries", rt.geometries as u64),
                        ("capture_tlas_builds", rt.tlas.len() as u64),
                        ("capture_tlas_instances", u64::from(instances)),
                        ("capture_tlas_instances_named", named as u64),
                        ("capture_tlas_builds_unread", unread as u64),
                        ("capture_as_builds_undecoded", rt.undecoded as u64),
                    ];
                    Ok((json!({"tlas": tables}), counts))
                }
                _ => {
                    let events = ngfx_calls::events(&input(earlier, "functions")?)?;
                    let c = ngfx_calls::calls(&mut capture, &events, &names)?;
                    write(scrubbed(&c.tsv).as_bytes())?;
                    let counts = vec![
                        ("capture_calls", c.calls as u64),
                        ("capture_call_args_decoded", c.decoded as u64),
                        ("capture_call_args_raw", c.raw as u64),
                        ("capture_call_args_unfit", c.unfit as u64),
                    ];
                    let out = match c.unfit_by.is_empty() {
                        true => json!({}),
                        false => json!({"unfit_by_function": c.unfit_by}),
                    };
                    Ok((out, counts))
                }
            }
        }
        _ => {
            call.progress("decoding the capture's shaders");
            let scan = capture(t)?.shaders()?;
            let dir = t.out.join("shaders");
            fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            let mut tsv = String::from("hash\tstage\tmodel\tbytes\tparts\tchunks\n");
            for s in &scan.shaders {
                let path = dir.join(format!("{}.dxbc", s.hash));
                if fs::metadata(&path).map(|m| m.len()).ok() != Some(s.bytes.len() as u64) {
                    fs::write(&path, &s.bytes).map_err(|e| format!("{}: {e}", path.display()))?;
                }
                let parts = s.parts.join(",");
                let chunks: Vec<String> = s.chunks.iter().map(u64::to_string).collect();
                tsv += &format!(
                    "{}\t{}\t{}\t{}\t{parts}\t{}\n",
                    s.hash,
                    s.stage,
                    s.model,
                    s.bytes.len(),
                    chunks.join(",")
                );
            }
            write(tsv.as_bytes())?;
            let counts = vec![
                ("capture_shader_blobs", scan.found as u64),
                ("capture_shaders_decoded", scan.shaders.len() as u64),
                ("capture_shader_blobs_failed", scan.failed as u64),
            ];
            Ok((
                json!({"duplicates": scan.duplicates, "folder": dir}),
                counts,
            ))
        }
    }
}

/// ngfx-replay `capture` `args` in a job of its own, without a window: its stdout, kept in memory (the metadata holds
/// the game's whole environment: never written raw), and its stderr in `log`. Killed when the call stops or past
/// [`PRINT_LIMIT`].
fn print(
    call: &Call,
    exe: &Path,
    capture: &Path,
    args: &[String],
    log: &Path,
) -> Result<Vec<u8>, String> {
    let failed = |e: io::Error| format!("ngfx-replay didn't start: {e}");
    let (mut reader, writer) = io::pipe().map_err(failed)?;
    let writer = File::from(OwnedHandle::from(writer));
    let err = File::create(log).map_err(|e| format!("{}: {e}", log.display()))?;
    let mut argv = vec![exe.display().to_string(), capture.display().to_string()];
    argv.extend(args.iter().cloned());
    let job = Job::new().map_err(failed)?;
    let child = job
        .spawn_split(&argv, &[], exe.parent().unwrap_or(exe), (&writer, &err))
        .map_err(failed)?;
    drop(writer);
    let reading = thread::spawn(move || {
        let mut out = Vec::new();
        reader.read_to_end(&mut out).map(|_| out)
    });
    let code = wait(call, &job, &child, PRINT_LIMIT, "ngfx-replay")?;
    let out = reading
        .join()
        .map_err(|_| "reading ngfx-replay's output failed".to_owned())?
        .map_err(|e| format!("reading ngfx-replay's output: {e}"))?;
    match code {
        0 => Ok(out),
        code => Err(format!(
            "ngfx-replay exited {code} (its log: {})",
            log.display()
        )),
    }
}

/// Waits for `child`, killing its job when the call stops or past `limit`: its exit code.
fn wait(call: &Call, job: &Job, child: &Child, limit: Duration, what: &str) -> Result<u32, String> {
    let t = Instant::now();
    loop {
        let why = match child.wait(Duration::from_millis(250)) {
            Ok(Some(code)) => return Ok(code),
            Ok(None) if call.stopped() => call.stop_cause().to_owned(),
            Ok(None) if t.elapsed() > limit => format!("it ran past {} s", limit.as_secs()),
            Ok(None) => {
                call.progress(&format!("{what} running, {} s", t.elapsed().as_secs()));
                continue;
            }
            Err(e) => format!("waiting for it: {e}"),
        };
        let _ = job.kill();
        return Err(format!("{what} killed: {why}"));
    }
}

/// Every file under `dir`, recursively.
fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        match entry.file_type() {
            Ok(t) if t.is_dir() => out.extend(files_under(&entry.path())),
            Ok(_) => out.push(entry.path()),
            Err(_) => {}
        }
    }
    out.sort();
    out
}

fn replay(session: &mut Session, call: &mut Call, args: &Value) -> Result<Fields, Done> {
    crate::ledger::object(args, &["capture_id", "op", "user_ok"])?;
    let config = session.config()?.clone();
    let op = match args["op"].as_str() {
        Some(op @ ("perf" | "gputrace")) => op,
        _ => return Err("op is perf or gputrace".to_string().into()),
    };
    if args.get("user_ok") != Some(&json!(true)) {
        let why = "user_ok: tell the user that the GPU replays the capture for about a minute, and get their OK first";
        return Err(why.to_string().into());
    }
    if session.live.is_some() {
        return Err(
            "a session is live: replay a capture only without one (after session_end)"
                .to_string()
                .into(),
        );
    }
    if !session::outside(&config.game_exe, None)?.is_empty() {
        return Err(format!(
            "{} runs: replay a capture only while no game does",
            config.game_exe
        )
        .into());
    }
    let (exe, version) = replayer(&config, "ngfx_replay")?;
    let ngfx = config.ngfx.clone().unwrap_or_default();
    if op == "gputrace" {
        profile::ngfx_args(config.ngfx_args.as_deref().unwrap_or_default())?;
    }
    locked(session, &config, "ngfx_replay", || {
        let mut ledger = open(&config)?;
        let t = target(&config, &mut ledger, call, args, false)?;
        if let Some(why) = too_long(&t.file) {
            return Err(why.into());
        }
        // The capture's Nsight version, from its metadata extraction: a replayer older than the capture may fail.
        let metadata = extraction("metadata", None, &t.row.sha256);
        let hit = ledger
            .extraction_hit(&metadata.key())
            .map_err(|e| e.to_string())?;
        let facts = hit
            .and_then(|(_, row)| fs::read_to_string(under(&config.capture_root(), &row.path)).ok())
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .map(|json| ngfx::metadata_facts(&json))
            .ok_or_else(|| "no metadata extraction of this capture yet: ngfx_extract with ops [\"metadata\"] first".to_owned())?;
        let captured = facts["nsight_version"].as_str().unwrap_or_default();
        if let Some(v) = version.as_deref().filter(|v| ngfx::newer(captured, v)) {
            return Err(format!(
                "the capture is from Nsight {captured}, newer than the installed ngfx-replay {v}"
            )
            .into());
        }
        let dir = t.out.join(format!("{op}-{}", session::now()));
        let tmp = config.data_root.join("tmp").join("ngfx");
        for d in [&dir, &tmp] {
            fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
        }
        let capture = t.file.display().to_string();
        let path = |p: &Path| p.display().to_string();
        let mut argv: Vec<String> = match op {
            "perf" => vec![
                path(&exe),
                capture,
                "--no-user-input".into(),
                "--perf-report-dir".into(),
                path(&dir),
                "-n".into(),
                "20".into(),
                "--present-hidden".into(),
                "--no-block-on-incompatibility".into(),
                "--collect-gpu-frametimes".into(),
                "--temp-resource-dir".into(),
                path(&tmp),
            ],
            _ => vec![
                path(&ngfx),
                "--activity".into(),
                "GPU Trace Profiler".into(),
                "--exe".into(),
                path(&exe),
                "--args".into(),
                format!("\"{capture}\" -n 10 --no-block-on-incompatibility --no-user-input"),
                "--start-on-replay-begin".into(),
                "--stop-on-replay-end".into(),
                "--auto-export".into(),
                "--output-dir".into(),
                path(&dir),
            ],
        };
        if op == "gputrace" {
            // Only the user's config adds ngfx arguments: none come from the agent.
            argv.extend(profile::words(
                config.ngfx_args.as_deref().unwrap_or_default(),
            ));
        }
        let log = dir.with_extension("log");
        let job = Job::new().map_err(|e| format!("the replay's job: {e}"))?;
        let started = SystemTime::now();
        let child = File::create(&log)
            .and_then(|f| job.spawn(&argv, &[], exe.parent().unwrap_or(&exe), &f))
            .map_err(|e| format!("{} didn't start: {e}", argv[0]))?;
        let code = wait(call, &job, &child, REPLAY_LIMIT, op);
        drop(job);
        call.artifact(&log);
        call.lap(op);
        let mut fields = Fields::from_iter([
            ("capture_id".into(), json!(t.row.id)),
            ("op".into(), json!(op)),
            ("folder".into(), json!(dir)),
            ("log".into(), json!(log)),
        ]);
        match code {
            Ok(0) => {}
            Ok(code) => {
                return Err(Done::Failed(
                    format!("{} exited {code}: its log says why", argv[0]),
                    fields,
                ));
            }
            Err(why) => return Err(Done::Failed(why, fields)),
        }
        // Every file it wrote, a captures row with no boot: a replay ran now, outside any boot.
        let root = config.capture_root();
        let files = files_under(&dir);
        let rows: Vec<Capture> = files
            .iter()
            .map(|f| row(&root, f, None, None, "ngfx_replay", started))
            .collect::<Result<_, _>>()?;
        let named = |name: &str| {
            files
                .iter()
                .position(|f| f.file_name().is_some_and(|n| n.eq_ignore_ascii_case(name)))
        };
        let read = |i: usize| {
            fs::read_to_string(&files[i]).map_err(|e| format!("{}: {e}", files[i].display()))
        };
        let (source, numbers) = match op {
            "perf" => {
                let i = named("iteration_times.csv")
                    .ok_or_else(|| "the replay wrote no iteration_times.csv".to_owned())?;
                let it = ngfx::iteration_times(&read(i)?)?;
                fields.insert("iterations".into(), json!(it.iterations));
                let rows = vec![
                    ("replay_image_error", it.image_error, "mae"),
                    ("replay_gpu_frame_ms", it.gpu_frame_ms, "ms"),
                    ("replay_cpu_frame_ms", it.cpu_frame_ms, "ms"),
                ];
                (Some(i), rows)
            }
            _ => match named("FRAME.xls") {
                Some(i) => (
                    Some(i),
                    vec![("gpu_frame_ms", ngfx::gpu_frame_ms(&read(i)?), "ms")],
                ),
                None => (None, Vec::new()),
            },
        };
        let note = match op {
            "perf" => format!(
                "ngfx_replay perf of capture {}: a replay, not the game",
                t.row.id
            ),
            _ => format!(
                "ngfx_replay gputrace of capture {}: a replay, not the game; clocks locked to base",
                t.row.id
            ),
        };
        let (ids, measured) = ledger
            .atomic(|l| {
                let ids: Vec<i64> = rows
                    .iter()
                    .map(|r| l.add_capture(r))
                    .collect::<Result<_, _>>()?;
                let mut measured = Vec::new();
                for (metric, value, unit) in numbers.iter().filter(|n| n.1.is_some()) {
                    measured.push(l.add_measurement(&Measurement {
                        boot_id: None,
                        capture_id: source.map(|i| ids[i]),
                        metric: (*metric).into(),
                        value: *value,
                        unit: Some((*unit).into()),
                        state_label: None,
                        method_family: "nsight".into(),
                        tag: Tag::M,
                        run_id: None,
                        note: Some(note.clone()),
                        extraction_id: None,
                        world: None,
                    })?);
                }
                Ok((ids, measured))
            })
            .map_err(|e| e.to_string())?;
        for id in &ids {
            call.row("captures", *id);
        }
        for id in measured {
            call.row("measurements", id);
        }
        let numbers: Map<String, Value> = numbers
            .iter()
            .map(|(m, v, _)| ((*m).to_owned(), json!(v)))
            .collect();
        fields.insert("files".into(), json!(files.len()));
        fields.insert("numbers".into(), json!(numbers));
        fields.insert("note".into(), json!(note));
        Ok(fields)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::Conn;
    use crate::session::Shared;
    use quen_core::journal::sha256_hex;

    /// A capture outside the capture root (a synthetic file with one shader block): ngfx_extract shaders imports it by
    /// a hard link, decodes the shader and records the rows; a second call reads the extraction back, finding the
    /// import by its file id; a capture on no row, a bad op and a replay without user_ok are refused, nothing changed.
    #[test]
    fn extract_imports_by_link_and_reads_the_cache_back() {
        let dir = std::env::temp_dir().join(format!("quen-ngfx-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir); // this test's own temp folder, from an earlier run
        fs::create_dir_all(dir.join("elsewhere")).unwrap();
        let config = Config {
            data_root: dir.join("data"),
            game_exe: "quen-test-no-such-game.exe".into(),
            ..Config::default()
        };
        // One SHEX container (compute 5_1), stored as chunk 7 of a container: `nlyp`, the chunk after its header, the
        // footer repeating the header, the trailer.
        let mut c = b"DXBC".to_vec();
        c.extend([3u8; 16]);
        c.extend(1u32.to_le_bytes());
        c.extend(52u32.to_le_bytes());
        c.extend(1u32.to_le_bytes());
        c.extend(36u32.to_le_bytes());
        c.extend(b"SHEX");
        c.extend(8u32.to_le_bytes());
        c.extend(0x0005_0051u32.to_le_bytes());
        c.extend([0u8; 4]);
        let mut header = b"elif".to_vec();
        for v in [1u32, 0, 0] {
            header.extend(v.to_le_bytes());
        }
        for v in [0u64, 52, 7, 4] {
            header.extend(v.to_le_bytes());
        }
        let mut file = b"nlyp".to_vec();
        file.extend(&header);
        file.extend(&c);
        let footer = file.len() as u64;
        file.extend(&header);
        file.extend(b"enod");
        file.extend(41u32.to_le_bytes());
        file.extend(1u64.to_le_bytes());
        file.extend(footer.to_le_bytes());
        let source = dir.join(r"elsewhere\synthetic frame.ngfx-capture");
        fs::write(&source, &file).unwrap();

        let shared = Shared::new(Ok(config.clone()));
        let mut session = Session::new(&shared, None);
        let mut run = |tool: &str, args: Value| {
            let mut c = session.call(Conn::nobody(), Value::Null, None);
            let done = call(&mut session, &mut c, tool, &args);
            session.envelope(tool, c, done)
        };
        let args = json!({"path": source, "ops": ["shaders"]});
        let first = run("ngfx_extract", args.clone());
        assert_eq!(first["ok"], true, "{first}");
        assert_eq!(first["capture"]["imported"], true, "{first}");
        assert_eq!(first["capture"]["links"], 2, "{first}");
        let op = &first["ops"][0];
        assert_eq!(op["cached"], false, "{first}");
        let counts = json!({"capture_shader_blobs": 1.0, "capture_shaders_decoded": 1.0, "capture_shader_blobs_failed": 0.0});
        assert_eq!(op["counts"], counts, "{first}");
        let sha = sha256_hex(&file);
        let out = config
            .capture_root()
            .join(format!(r"imports\{}\synthetic frame", &sha[..12]));
        let shader = out.join(format!(r"shaders\{}.dxbc", "03".repeat(16)));
        assert_eq!(fs::read(shader).unwrap(), c);
        let tsv = fs::read_to_string(out.join("shaders.tsv")).unwrap();
        assert!(
            tsv.ends_with(&format!("{}\tcompute\t5_1\t52\tSHEX\t7\n", "03".repeat(16))),
            "{tsv}"
        );
        assert_eq!(fs::read(&source).unwrap(), file, "the original unchanged");

        let again = run("ngfx_extract", args);
        assert_eq!(again["ok"], true, "{again}");
        assert_eq!(again["capture"]["imported"], false, "{again}");
        assert_eq!(again["ops"][0]["cached"], true, "{again}");
        assert_eq!(again["ops"][0]["counts"], counts, "{again}");
        assert_eq!(
            again["ops"][0]["extraction_id"], op["extraction_id"],
            "{again}"
        );
        let by_id = run(
            "ngfx_extract",
            json!({"capture_id": first["capture"]["id"], "ops": ["shaders"]}),
        );
        assert_eq!(by_id["ops"][0]["cached"], true, "{by_id}");

        for (tool, args, why) in [
            (
                "ngfx_extract",
                json!({"capture_id": 99, "ops": ["shaders"]}),
                "no captures row 99",
            ),
            (
                "ngfx_extract",
                json!({"path": source, "ops": ["nope"]}),
                "not one of",
            ),
            (
                "ngfx_extract",
                json!({"path": "relative.ngfx-capture", "ops": ["shaders"]}),
                "not an absolute path",
            ),
            (
                "ngfx_replay",
                json!({"capture_id": 1, "op": "perf"}),
                "user_ok",
            ),
            (
                "ngfx_replay",
                json!({"capture_id": 1, "op": "other", "user_ok": true}),
                "perf or gputrace",
            ),
        ] {
            let out = run(tool, args);
            assert!(
                out["reason"].as_str().is_some_and(|r| r.contains(why)),
                "{why}: {out}"
            );
        }
        // A capture whose path in the capture root would pass MAX_PATH: refused before any link.
        let long = dir
            .join("elsewhere")
            .join(format!("{}.ngfx-capture", "a".repeat(230)));
        fs::write(&long, b"x").unwrap();
        let out = run("ngfx_extract", json!({"path": long, "ops": ["shaders"]}));
        assert!(
            out["reason"]
                .as_str()
                .is_some_and(|r| r.contains("MAX_PATH")),
            "{out}"
        );
        let imports = fs::read_dir(config.capture_root().join("imports"))
            .unwrap()
            .count();
        assert_eq!(imports, 1, "no second import folder");
        // Without ngfx in the config, the replayer's ops are refused with needs, and a reader op that checks against
        // them too.
        for ops in [json!(["metadata"]), json!(["object_table"])] {
            let out = run("ngfx_extract", json!({"capture_id": 1, "ops": ops}));
            assert_eq!(
                out["needs"][0]["dependency"],
                doctor::NGFX_REPLAY.name,
                "{out}"
            );
        }
        let _ = fs::remove_dir_all(&dir); // this test's own temp folder
    }

    /// A hand check of quen's reader on a real capture, read-only: `QUEN_NGFX_CAPTURE` (the file),
    /// `QUEN_NGFX_FUNCTIONS` and `QUEN_NGFX_OBJECTS` (ngfx-replay's `--metadata-functions` and `--metadata-objects`
    /// of it), `QUEN_NGFX_OUT` (a folder outside the repo for the tables). Prints counts only:
    /// `cargo test --release -p quen reader_hand_check -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn reader_hand_check() {
        let var = |k: &str| PathBuf::from(std::env::var(k).unwrap_or_else(|_| panic!("set {k}")));
        let out = var("QUEN_NGFX_OUT");
        let earlier = HashMap::from([
            ("functions", var("QUEN_NGFX_FUNCTIONS")),
            ("objects", var("QUEN_NGFX_OBJECTS")),
        ]);
        let row = CaptureRef {
            id: 0,
            boot_id: None,
            path: String::new(),
            sha256: String::new(),
            bytes: 0,
            kind: "ngfx-capture".into(),
            removed: false,
        };
        let t = Target {
            row,
            file: var("QUEN_NGFX_CAPTURE"),
            out: out.clone(),
            imported: false,
        };
        let shared = Shared::new(Ok(Config::default()));
        let session = Session::new(&shared, None);
        let mut call = session.call(Conn::nobody(), Value::Null, None);
        for (op, name, _) in OPS.iter().filter(|(.., flag)| flag.is_none()) {
            let started = Instant::now();
            let (json, counts) = run(op, &out.join(name), &t, None, &earlier, &mut call).unwrap();
            let secs = started.elapsed().as_secs_f64();
            println!("{op} {secs:.1} s {counts:?}");
            for tlas in json["tlas"].as_array().into_iter().flatten() {
                let file = tlas["file"].is_string();
                println!(
                    "  tlas {} instances {} named {} file {file}",
                    tlas["tlas"], tlas["instances"], tlas["named"]
                );
            }
        }
    }
}
