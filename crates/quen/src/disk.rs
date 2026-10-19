//! Capture management: quen keeps its captures from piling up unseen. quen_doctor, session_plan, session_launch
//! and session_end report the capture root's size against `capture_budget_gb`; `capture_usage` (read-only) says what is
//! there and what a cleanup would take; `capture_clean`, with the user's OK, takes it: to the Recycle Bin, or what the
//! bin can't take whole to `<data root>\_to_delete\<date>\` for the user to empty. Nothing is ever deleted, and the
//! ledger rows stay, marked removed. The selection and the plan are quen-core's (`disk`).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use quen_core::disk::{self, File, Select, To};
use quen_core::ledger::Ledger;
use serde_json::{Map, Value, json};

use crate::config::Config;
use crate::session::{self, Call, Done, Session, Shared};
use crate::win;

/// The largest file sent to the Recycle Bin: bigger ones go to `_to_delete`, whatever the bin's size (over about
/// 4 GB, to be safe: a file the bin can't hold whole is deleted at once instead).
const RECYCLE_MAX: u64 = 4_000_000_000;
/// The selection keys both tools take.
const SELECT: [&str; 5] = [
    "boots",
    "older_than_days",
    "kinds",
    "capture_ids",
    "missing",
];

pub fn tools() -> Vec<Value> {
    let ints =
        |what: &str| json!({"type": "array", "items": {"type": "integer"}, "description": what});
    let select = json!({
        "boots": ints("boot ids: their files"),
        "older_than_days": {"type": "number", "minimum": 0, "description": "files older than this many days"},
        "kinds": {"type": "array", "items": {"type": "string"}, "description": "file extensions, e.g. rdc, png, csv"},
        "capture_ids": ints("capture rows, taken even when cited"),
        "missing": {"type": "boolean", "description": "true: the rows in missing too (file gone): capture_clean marks them removed, removed_to missing, and moves no file for them"},
    });
    let mut clean = select.clone();
    clean["user_ok"] = json!({"type": "boolean", "description": "true only after the user saw what goes (capture_usage with the same selection) and agreed. Required."});
    let schema = |props: Value| json!({"type": "object", "properties": props, "additionalProperties": false});
    vec![
        json!({
            "name": "capture_usage",
            "description": "Read-only: what quen's capture root holds (screenshots, PresentMon CSVs, profiler \
                outputs, files you copied there): files and GB by boot, kind and age, every file over 1 GB, the \
                budget (capture_budget_gb in quen's config, default 20), what waits in <data root>\\_to_delete for \
                the user to empty, and the capture rows whose file is gone (missing). With a selection (boots, \
                older_than_days, kinds, capture_ids, missing; every filter given must match, capture_ids are taken \
                anyway): what capture_clean would take, where to, the cited files it keeps, the folders it empties \
                and the rows it marks missing. After session_end, check it and offer the user a cleanup; never \
                clean on your own.",
            "inputSchema": schema(select),
        }),
        json!({
            "name": "capture_clean",
            "description": "Take capture files off the disk, with the user's OK: show them capture_usage with the \
                same selection first. The client asks the user on every call (user_ok is only your word). Takes the \
                files every given filter matches (boots, older_than_days, kinds) and those named in capture_ids. \
                Keeps a cited file (a claim's evidence, a pose's reference image, pinned, or named in a document) \
                unless capture_ids names it. Never deletes: files go to the Recycle Bin, and one it can't take whole \
                (over 4 GB or over the bin's size, or a drive without a bin) to <data root>\\_to_delete\\<date>\\, \
                which the user empties. Ledger rows stay, with removed_at and removed_to. Emptied folders go \
                (folders). missing: true also marks the rows in capture_usage's missing (removed_to missing), moving \
                no file. Refused, nothing moved: no selection, no user_ok, a session live, another quen holding the \
                game lock. Files it couldn't move are in left.",
            // Claude Code prompts for this tool on every call, in every permission mode, as for session_launch.
            "_meta": {"anthropic/requiresUserInteraction": true},
            "inputSchema": schema(clean),
        }),
    ]
}

/// What the capture root holds, joined to the ledger.
struct Found {
    files: Vec<File>,
    /// Capture rows marked on disk whose file isn't under the capture root.
    missing: Vec<i64>,
    /// The folder names that belong to a boot (`Ledger::boot_folders`).
    folders: Vec<(String, i64)>,
    /// The ledger's error, when it can't be read.
    note: Option<String>,
}

/// The files under the capture root, joined to the ledger when it can be read (its error otherwise).
fn files(config: &Config) -> Found {
    let db = config.state_dir().join("quen.db");
    let ledger = match db.exists() {
        true => Ledger::open_readonly(&db).and_then(|l| Ok((l.capture_rows()?, l.boot_folders()?))),
        false => Ok((Vec::new(), Vec::new())),
    };
    let (rows, folders, note) = match ledger {
        Ok((rows, folders)) => (rows, folders, None),
        Err(e) => (
            Vec::new(),
            Vec::new(),
            Some(format!("ledger: {e}: no file counts as cited")),
        ),
    };
    let files = disk::scan(&config.capture_root(), SystemTime::now(), &rows, &folders);
    Found {
        missing: disk::missing(&rows, &files),
        files,
        folders,
        note,
    }
}

impl Found {
    /// The folders a cleanup by `select` taking `plan` empties ([`disk::emptied`]): relative to the capture root.
    fn emptied(&self, root: &Path, select: &Select, plan: &disk::Plan) -> Vec<String> {
        let picked = |boot: &i64| select.boots.as_ref().is_some_and(|b| b.contains(boot));
        let boots: Vec<&str> = self
            .folders
            .iter()
            .filter(|(_, boot)| picked(boot))
            .map(|(folder, _)| folder.as_str())
            .collect();
        disk::emptied(&disk::folders(root), &self.files, plan, &boots)
    }
}

/// The largest file the Recycle Bin takes whole on the capture root's volume, or why it takes none.
fn recycle_max(config: &Config) -> Result<u64, String> {
    let bin = win::recycle_bin(&config.capture_root())?;
    Ok(bin.map_or(RECYCLE_MAX, |size| size.min(RECYCLE_MAX)))
}

fn to_delete(config: &Config) -> PathBuf {
    config.data_root.join("_to_delete")
}

/// The capture root against the budget, and what waits in `_to_delete`: quen_doctor's and session_end's report.
fn budget(config: &Config, bytes: u64) -> Map<String, Value> {
    let budget = config.capture_budget_gb();
    let waiting = to_delete(config);
    let mut out = Map::new();
    out.insert("root".into(), json!(config.capture_root()));
    out.insert("gb".into(), json!(disk::gb(bytes)));
    out.insert("budget_gb".into(), json!(budget));
    out.insert("over_budget".into(), json!(bytes as f64 > budget * 1e9));
    if let Some(why) = warning(config, bytes) {
        out.insert("warning".into(), json!(why));
    }
    let left = disk::total(&waiting);
    if left > 0 {
        let note = "files capture_clean moved there, which the Recycle Bin couldn't take whole: the user empties it";
        out.insert(
            "to_delete".into(),
            json!({"path": waiting, "gb": disk::gb(left), "note": note}),
        );
    }
    out
}

/// The warning when the capture root holds more than the budget.
fn warning(config: &Config, bytes: u64) -> Option<String> {
    let budget = config.capture_budget_gb();
    (bytes as f64 > budget * 1e9).then(|| {
        format!(
            "the capture root holds {} GB, over capture_budget_gb {budget}: capture_usage shows what is there; \
             capture_clean takes what the user agrees to",
            disk::gb(bytes)
        )
    })
}

/// quen_doctor's `captures`.
pub(crate) fn summary(config: &Config) -> Value {
    json!(budget(config, disk::total(&config.capture_root())))
}

/// session_plan's and session_launch's: the warning, when the capture root is over the budget.
pub(crate) fn over_budget(config: &Config) -> Option<String> {
    warning(config, disk::total(&config.capture_root()))
}

/// session_end's `captures`: the budget report, the boot's folders (its id's, for a profiler's output, and its
/// session's) and their files over 1 GB.
pub(crate) fn after_end(config: &Config, boot: i64, session: &str) -> Value {
    let root = config.capture_root();
    let mut out = budget(config, disk::total(&root));
    let folders: Vec<PathBuf> = [boot.to_string(), session.to_owned()]
        .into_iter()
        .map(|f| root.join(f))
        .filter(|f| f.is_dir())
        .collect();
    let bytes: u64 = folders.iter().map(|f| disk::total(f)).sum();
    let mut files = Vec::new();
    for folder in &folders {
        let name = folder.file_name().unwrap_or_default().to_string_lossy();
        let found = disk::scan(folder, SystemTime::now(), &[], &[]);
        // Both folders are the boot's (feature tests 2b: large[].boot was null).
        files.extend(found.into_iter().map(|mut f| {
            f.rel = format!("{name}/{}", f.rel);
            f.boot = Some(boot);
            f
        }));
    }
    let boot =
        json!({"folders": folders, "gb": disk::gb(bytes), "large": disk::large(files.iter())});
    out.insert("boot".into(), boot);
    json!(out)
}

/// `capture_usage`, on the connection's thread: read-only.
pub fn usage(shared: &Shared, args: &Value) -> Value {
    let done = (|| -> Result<Map<String, Value>, String> {
        let config = shared.config();
        let config = config
            .as_ref()
            .as_ref()
            .map_err(|e| format!("quen's config: {e}"))?;
        let select = selection(args, &[])?;
        let found = files(config);
        let (files, missing) = (&found.files, &found.missing);
        let mut out = budget(config, files.iter().map(|f| f.bytes).sum());
        let usage = disk::usage(files);
        out.extend(usage.as_object().cloned().unwrap_or_default());
        let max = recycle_max(config);
        match &max {
            Ok(max) => out.insert("recycle_bin_max_gb".into(), json!(disk::gb(*max))),
            Err(why) => out.insert("recycle_bin".into(), json!(format!("none: {why}"))),
        };
        if !select.is_empty() {
            let plan = disk::plan(files, &select, max.ok());
            let mut cleanup = plan.summary();
            cleanup["folders"] = json!(found.emptied(&config.capture_root(), &select, &plan));
            if select.missing {
                cleanup["missing"] = json!(missing.len());
            }
            out.insert("cleanup".into(), cleanup);
        }
        if !missing.is_empty() {
            let note = "capture rows the ledger says are on disk, with no file under the capture root (removed by \
                        hand, or under another capture root); ids lists the first 50";
            let ids = &missing[..missing.len().min(50)];
            out.insert(
                "missing".into(),
                json!({"rows": missing.len(), "ids": ids, "note": note}),
            );
        }
        if let Some(note) = &found.note {
            out.insert("note".into(), json!(note));
        }
        Ok(out)
    })();
    let done = done.map_or_else(Done::Refused, Done::Ok);
    session::envelope(
        shared,
        "capture_usage",
        Map::new(),
        Vec::new(),
        Map::new(),
        done,
    )
}

/// The selection in `args`, which may hold only its keys and `also`.
fn selection(args: &Value, also: &[&str]) -> Result<Select, String> {
    let map = args.as_object().ok_or("arguments must be an object")?;
    if let Some(key) = map
        .keys()
        .find(|k| !SELECT.contains(&k.as_str()) && !also.contains(&k.as_str()))
    {
        return Err(format!("unknown argument {key:?}"));
    }
    Select::parse(args)
}

/// `capture_clean`, on the session thread, under the game lock: the ledger's writers hold it.
pub(crate) fn clean(session: &mut Session, call: &mut Call, args: &Value) -> Done {
    let config = match session.config() {
        Ok(config) => config.clone(),
        Err(why) => return why.into(),
    };
    let select = match selection(args, &["user_ok"]) {
        Ok(select) if select.is_empty() => {
            return "name what goes: boots, older_than_days, kinds, capture_ids or missing (capture_usage with the same shows it)"
                .to_string()
                .into();
        }
        Ok(select) => select,
        Err(why) => return why.into(),
    };
    if args.get("user_ok") != Some(&json!(true)) {
        return "user_ok: show the user what capture_usage with this selection takes, and get their OK first"
            .to_string()
            .into();
    }
    if session.live.is_some() {
        return "a session is live: session_end first (a boot still writes its captures)"
            .to_string()
            .into();
    }
    if let Err(why) = session.take_lock(&config, "capture_clean") {
        return why.into();
    }
    let done = take(&config, call, &select, recycle_max(&config), &win::recycle);
    session.release_lock();
    done
}

/// Moves what `select` takes, then marks its ledger rows; the lock is held. `max`: [`recycle_max`]; `recycle` sends a
/// file to the Recycle Bin.
fn take(
    config: &Config,
    call: &mut Call,
    select: &Select,
    max: Result<u64, String>,
    recycle: &dyn Fn(&Path) -> std::io::Result<()>,
) -> Done {
    let found = files(config);
    let plan = disk::plan(&found.files, select, max.as_ref().ok().copied());
    let root = config.capture_root();
    let emptied = found.emptied(&root, select, &plan);
    call.lap("plan");
    let now = SystemTime::now();
    let date = &session::utc(now)[..10];
    let bin = to_delete(config).join(date);
    let (mut moved, mut left) = (Vec::new(), Vec::new());
    for (n, (file, to)) in plan.take.iter().enumerate() {
        if call.stopped() {
            left.push(json!({"path": file.rel, "why": "cancelled before it"}));
            continue;
        }
        let path = root.join(file.rel.replace('/', "\\"));
        let went = match to {
            To::RecycleBin => recycle(&path).map_err(|e| e.to_string()),
            To::ToDelete => {
                let dest = bin.join(file.rel.replace('/', "\\"));
                let made = dest.parent().map_or(Ok(()), fs::create_dir_all);
                made.and_then(|()| win::rename_no_replace(&path, &dest))
                    .map_err(|e| format!("not moved to {}: {e}", dest.display()))
            }
        };
        match went {
            Ok(()) => moved.push((file, *to)),
            Err(why) => left.push(json!({"path": file.rel, "why": why})),
        }
        call.progress(&format!("{} of {} files", n + 1, plan.take.len()));
    }
    call.lap("move");
    // The folders the cleanup emptied go, deepest first: only empty ones (remove_dir refuses any other, so a file that
    // didn't move keeps its folders), never a link's target (feature tests 2b: a recovered ngfx boot's folder stayed).
    let folders: Vec<&String> = emptied
        .iter()
        .filter(|dir| fs::remove_dir(root.join(dir.replace('/', "\\"))).is_ok())
        .collect();
    // Rows whose file was gone already, marked in one transaction (feature tests 2c: batches 1-2's 3,887).
    let missing = match select.missing {
        true => found.missing.as_slice(),
        false => &[],
    };
    let mut fields = Map::new();
    let marked = Ledger::open(config.state_dir().join("quen.db")).and_then(|mut ledger| {
        let t = ledger.utc(session::now() as i64)?;
        for (file, to) in &moved {
            if let Some(id) = file.capture {
                ledger.mark_removed(id, &t, to.as_str())?;
                call.row("captures", id);
            }
        }
        ledger.atomic(|l| {
            missing
                .iter()
                .try_for_each(|id| l.mark_removed(*id, &t, "missing"))
        })
    });
    call.lap("ledger");
    if let Err(e) = marked {
        fields.insert("ledger_error".into(), json!(e.to_string()));
    }
    let sum = |to: To| {
        let picked: Vec<&&File> = moved
            .iter()
            .filter(|(_, t)| *t == to)
            .map(|(f, _)| f)
            .collect();
        json!({"files": picked.len(), "gb": disk::gb(picked.iter().map(|f| f.bytes).sum())})
    };
    fields.insert("recycle_bin".into(), sum(To::RecycleBin));
    let mut waiting = sum(To::ToDelete);
    if waiting["files"] != 0 {
        waiting["folder"] = json!(bin);
        waiting["note"] = json!(
            "the Recycle Bin can't take these whole: tell the user to check and empty this folder"
        );
    }
    fields.insert("to_delete".into(), waiting);
    if let Err(why) = max {
        fields.insert("recycle_bin_off".into(), json!(why));
    }
    let kept: Vec<Value> = plan
        .kept
        .iter()
        .map(|f| json!({"path": f.rel, "capture_id": f.capture}))
        .collect();
    fields.insert("kept_cited".into(), json!(kept));
    fields.insert("folders".into(), json!(folders));
    if select.missing {
        let marked = if fields.contains_key("ledger_error") {
            0
        } else {
            missing.len()
        };
        let ids = &missing[..marked.min(50)];
        fields.insert("missing".into(), json!({"rows": marked, "ids": ids}));
    }
    fields.insert("left".into(), json!(left));
    fields.insert("captures".into(), json!(budget(config, disk::total(&root))));
    if let Some(note) = found.note {
        fields.insert("note".into(), json!(note));
    }
    if left.is_empty() {
        Done::Ok(fields)
    } else {
        Done::Failed(
            format!("{} files not moved (left says why)", left.len()),
            fields,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quen_core::ledger::Capture;

    /// The budget's warning names the tools, and only over the budget.
    #[test]
    fn the_budget_warns_over_its_size_and_names_the_tools() {
        let mut config = Config::default();
        assert_eq!(config.capture_budget_gb(), 20.0);
        assert_eq!(warning(&config, 20_000_000_000), None);
        let why = warning(&config, 20_100_000_000).unwrap();
        assert!(
            why.contains("20.1 GB") && why.contains("capture_clean"),
            "{why}"
        );
        config.capture_budget_gb = Some(0.5);
        assert!(warning(&config, 600_000_000).is_some());
    }

    /// capture_clean on a fake session's data root: refused while the session is live, and without a selection or
    /// user_ok. Then (the move itself, with a stand-in Recycle Bin that takes 9 bytes) the small file goes to the bin,
    /// the bigger one to _to_delete, the cited one stays unless named, and the rows are marked.
    #[test]
    fn clean_moves_what_it_takes_and_marks_the_rows() {
        let fx = crate::game::tests::Fake::new("clean");
        let config = fx.shared.config().as_ref().clone().unwrap();
        let root = config.capture_root();
        let put = |rel: &str| {
            let path = root.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, rel).unwrap();
        };
        put(r"1\run.nsys-rep");
        put(r"s1\kf0.png");
        put(r"s1\f1.png");
        let mut ledger = Ledger::open(config.state_dir().join("quen.db")).unwrap();
        let row = |path: &str| Capture {
            boot_id: Some(1),
            path: path.into(),
            sha256: quen_core::journal::sha256_hex(path.as_bytes()),
            bytes: 1,
            kind: "png".into(),
            t: "2026-10-18T00:00:00Z".into(),
            pinned: true,
            ..Capture::default()
        };
        let cited = ledger.add_capture(&row("s1/kf0.png")).unwrap();
        let plain = ledger
            .add_capture(&Capture {
                pinned: false,
                ..row("s1/f1.png")
            })
            .unwrap();
        // Feature tests 2b: a row whose file is gone is counted by capture_usage.
        let gone = ledger.add_capture(&row("old/gone.png")).unwrap();
        drop(ledger);
        let used = usage(&fx.shared, &json!({}));
        assert_eq!(used["missing"]["rows"], 1, "{used}");
        assert_eq!(used["missing"]["ids"], json!([gone]), "{used}");
        // And a selected boot's empty folder goes with the cleanup, as usage says first (feature tests 2c).
        fs::create_dir_all(root.join(r"s1\l3c\deeper")).unwrap();
        let folders = json!(["s1/l3c/deeper", "s1/l3c", "1"]);
        let planned = usage(&fx.shared, &json!({"boots": [1]}));
        assert_eq!(planned["cleanup"]["folders"], folders, "{planned}");

        let mut session = fx.session();
        let run = |session: &mut Session, args: Value| {
            let mut call = session.call(crate::mcp::Conn::nobody(), Value::Null, None);
            let done = clean(session, &mut call, &args);
            session.envelope("capture_clean", call, done)
        };
        let all = json!({"boots": [1], "user_ok": true});
        let live = run(&mut session, all.clone());
        assert!(
            live["reason"].as_str().unwrap().contains("session is live"),
            "{live}"
        );
        session.live = None;
        for (args, why) in [
            (json!({"user_ok": true}), "name what goes"),
            (json!({"boots": [1]}), "user_ok"),
            (
                json!({"boots": [1], "user_ok": true, "x": 1}),
                "unknown argument",
            ),
        ] {
            let out = run(&mut session, args);
            assert!(
                out["reason"].as_str().unwrap().contains(why),
                "{why}: {out}"
            );
        }
        assert!(
            root.join(r"s1\f1.png").exists(),
            "the refusals moved nothing"
        );

        let bin = fx.dir.join("recycled");
        fs::create_dir_all(&bin).unwrap();
        let recycle = |path: &Path| fs::rename(path, bin.join(path.file_name().unwrap()));
        let moved = |select: Value| {
            let mut call = session.call(crate::mcp::Conn::nobody(), Value::Null, None);
            let select = Select::parse(&select).unwrap();
            let done = take(&config, &mut call, &select, Ok(9), &recycle);
            session.envelope("capture_clean", call, done)
        };
        let out = moved(json!({"boots": [1]}));
        assert_eq!(out["ok"], true, "{out}");
        assert_eq!(out["recycle_bin"]["files"], 1, "{out}");
        assert_eq!(out["to_delete"]["files"], 1, "{out}");
        assert_eq!(out["kept_cited"][0]["capture_id"], cited, "{out}");
        assert!(bin.join("f1.png").exists() && root.join(r"s1\kf0.png").exists());
        let date = &session::utc(SystemTime::now())[..10];
        let day = config.data_root.join("_to_delete").join(date);
        assert!(day.join(r"1\run.nsys-rep").exists(), "{out}");
        assert!(!root.join("1").exists(), "the emptied folder went");
        assert_eq!(out["folders"], folders, "{out}");
        assert!(
            !root.join(r"s1\l3c").exists() && root.join("s1").is_dir(),
            "the empty folder went, the one holding a kept file stayed"
        );
        assert_eq!(out["ledger"]["captures"], json!([plain]), "{out}");
        let ledger = Ledger::open_readonly(config.state_dir().join("quen.db")).unwrap();
        let sql = "SELECT id, removed_to FROM captures WHERE removed_at IS NOT NULL";
        let marked = ledger
            .query(sql, &[], std::time::Duration::from_secs(5))
            .unwrap();
        assert_eq!(marked.rows, [[json!(plain), json!("recycle_bin")]]);
        // Named by id, the cited file goes too (10 bytes: over the stand-in bin's 9).
        let named = moved(json!({"capture_ids": [cited]}));
        assert_eq!(named["to_delete"]["files"], 1, "{named}");
        assert!(day.join(r"s1\kf0.png").exists());
        // Feature tests 2c: missing: true marks the row whose file is gone and moves nothing; usage counts it no more.
        let planned = usage(&fx.shared, &json!({"missing": true}));
        assert_eq!(planned["cleanup"]["missing"], 1, "{planned}");
        let marked = moved(json!({"missing": true}));
        assert_eq!(
            marked["missing"],
            json!({"rows": 1, "ids": [gone]}),
            "{marked}"
        );
        assert_eq!(marked["recycle_bin"]["files"], 0, "{marked}");
        let sql =
            format!("SELECT removed_to FROM captures WHERE id = {gone} AND removed_at IS NOT NULL");
        let row = ledger
            .query(&sql, &[], std::time::Duration::from_secs(5))
            .unwrap();
        assert_eq!(row.rows, [[json!("missing")]]);
        let after = usage(&fx.shared, &json!({}));
        assert!(after.get("missing").is_none(), "{after}");
    }
}
