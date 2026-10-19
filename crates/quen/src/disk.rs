//! Capture management: quen keeps its captures, and its other leftovers under the data root, from piling up unseen.
//! quen_doctor, session_plan, session_launch and session_end report the capture root's size against
//! `capture_budget_gb`; `capture_usage` (read-only) says what is there and what a cleanup would take; `capture_clean`,
//! with the user's OK, takes it: to the Recycle Bin, or, a file the bin can't take whole, through Windows' own dialog,
//! which asks the user whether to delete it permanently (the owner's rule, 2026-10-19: never a silent permanent
//! delete; no file moves to `_to_delete` any more). The ledger rows stay, marked removed. The selection and the plan
//! are quen-core's (`disk`).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, SystemTime};

use quen_core::disk::{self, File, Select, To};
use quen_core::ledger::Ledger;
use serde_json::{Map, Value, json};
use windows::Win32::Foundation::HWND;

use crate::config::Config;
use crate::session::{self, Call, Done, Session, Shared};
use crate::{capture, doctor, game, input, win};

/// The largest file sent to the Recycle Bin: for bigger ones Windows asks the user, whatever the bin's size (over
/// about 4 GB, to be safe: a file the bin can't hold whole is deleted at once instead).
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
        "boots": ints("boot ids: their files, and their sessions' logs"),
        "older_than_days": {"type": "number", "minimum": 0, "description": "files older than this many days"},
        "kinds": {"type": "array", "items": {"type": "string"}, "description": "file extensions, e.g. rdc, png, csv, or leftover kinds: session_logs, settings_backups, query_spills, ledger_backups, packages, to_delete"},
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
                budget (capture_budget_gb in quen's config, default 20), and the capture rows whose file is gone \
                (missing). leftovers: quen's other files under the data root, the same way, by kind: session_logs, \
                settings_backups, query_spills, ledger_backups, packages (zips but the installed package's) and \
                to_delete (what earlier cleanups moved there); their paths are relative to the data root. With a \
                selection (boots, older_than_days, kinds, capture_ids, missing; every filter given must match, \
                capture_ids are taken anyway): what capture_clean would take (recycle_bin, ask_delete), the cited \
                files it keeps, the folders it empties and the rows it marks missing. After session_end, check it \
                and offer the user a cleanup; never clean on your own.",
            "inputSchema": schema(select),
        }),
        json!({
            "name": "capture_clean",
            "description": "Take quen's files off the disk, with the user's OK: show them capture_usage with the \
                same selection first. The client asks the user on every call (user_ok is only your word). Takes the \
                files every given filter matches (boots, older_than_days, kinds) and those named in capture_ids. \
                Keeps a cited file (a claim's evidence, a pose's image, pinned, or named in a document) unless \
                capture_ids names it. Files go to the Recycle Bin. For one it can't take whole (over 4 GB or the \
                bin's size, or no bin on the drive), Windows' own dialog asks the user whether to delete it for \
                good, and the call waits: yes deletes it (deleted), no keeps it in place (kept). Ledger rows stay, \
                with removed_at and removed_to. Emptied folders go (folders, leftover_folders). missing: true also \
                marks the rows in capture_usage's missing (removed_to missing). Refused, nothing moved: no \
                selection, no user_ok, a session live, a journal a dead quen left, PresentMon running, another quen \
                holding the game lock. What it couldn't take: left.",
            // Claude Code prompts for this tool on every call, in every permission mode, as for session_launch.
            "_meta": {"anthropic/requiresUserInteraction": true},
            "inputSchema": schema(clean),
        }),
    ]
}

/// What the capture root and the data root hold, joined to the ledger.
struct Found {
    /// The capture root's files.
    files: Vec<File>,
    /// quen's other leftovers under the data root ([`disk::leftovers`]).
    leftovers: Vec<File>,
    /// Capture rows marked on disk whose file isn't under the capture root, and how many are of quen's boots, of
    /// imported boots and of no boot ([`disk::missing`]).
    missing: (Vec<i64>, [usize; 3]),
    /// The folder names that belong to a boot (`Ledger::boot_folders`).
    folders: Vec<(String, i64)>,
    /// The ledger's error, when it can't be read.
    note: Option<String>,
}

/// The files under the capture root and quen's leftovers, joined to the ledger when it can be read (its error
/// otherwise).
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
    let now = SystemTime::now();
    let files = disk::scan(&config.capture_root(), now, &rows, &folders);
    // The zips of this quen and of the package in the game stay: they are no leftovers.
    let package = doctor::package_version(config).map(|v| format!("{}.zip", v.replace(' ', "-")));
    let quen = format!("quen-{}-windows-x64.zip", env!("CARGO_PKG_VERSION"));
    let keep: Vec<String> = [Some(quen), package].into_iter().flatten().collect();
    Found {
        missing: disk::missing(&rows, &files),
        leftovers: disk::leftovers(&config.data_root, now, &folders, &keep),
        files,
        folders,
        note,
    }
}

impl Found {
    /// The cleanup `select` asks for: of the capture root's files, by `max.0` (its volume's [`recycle_max`]), and of
    /// the leftovers, by `max.1` (the data root's).
    fn plan(&self, select: &Select, max: (Option<u64>, Option<u64>)) -> disk::Plan {
        let mut plan = disk::plan(&self.files, select, max.0);
        plan.take
            .extend(disk::plan(&self.leftovers, select, max.1).take);
        plan
    }

    /// The folders a cleanup by `select` taking `plan` empties ([`disk::emptied`]): the capture root's, relative to it,
    /// and the leftovers' (in `logs` and `_to_delete`), relative to the data root.
    fn emptied(
        &self,
        config: &Config,
        select: &Select,
        plan: &disk::Plan,
    ) -> (Vec<String>, Vec<String>) {
        let taken = |leftover: bool| -> Vec<&str> {
            let taken = plan.take.iter().filter(|(f, _)| f.leftover == leftover);
            taken.map(|(f, _)| f.rel.as_str()).collect()
        };
        let picked = |boot: &i64| select.boots.as_ref().is_some_and(|b| b.contains(boot));
        let boots: Vec<&str> = self
            .folders
            .iter()
            .filter(|(_, boot)| picked(boot))
            .map(|(folder, _)| folder.as_str())
            .collect();
        let root = config.capture_root();
        let captures = disk::emptied(&disk::folders(&root), &self.files, &taken(false), &boots);
        let dirs: Vec<String> = ["logs", "_to_delete"]
            .into_iter()
            .flat_map(|area| {
                let dirs = disk::folders(&config.data_root.join(area));
                dirs.into_iter().map(move |d| format!("{area}/{d}"))
            })
            .collect();
        let leftovers = disk::emptied(&dirs, &self.leftovers, &taken(true), &[]);
        (captures, leftovers)
    }
}

/// The largest file the Recycle Bin takes whole on `path`'s volume, or why it takes none.
fn recycle_max(path: &Path) -> Result<u64, String> {
    let bin = win::recycle_bin(path)?;
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
        let note = "files quen 0.1.5 and older moved there in a cleanup: capture_clean with kinds [\"to_delete\"] \
                    takes them, with the user's OK";
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
        let (files, (missing, by)) = (&found.files, &found.missing);
        let mut out = budget(config, files.iter().map(|f| f.bytes).sum());
        let usage = disk::usage(files);
        out.extend(usage.as_object().cloned().unwrap_or_default());
        let max = recycle_max(&config.capture_root());
        match &max {
            Ok(max) => out.insert("recycle_bin_max_gb".into(), json!(disk::gb(*max))),
            Err(why) => out.insert("recycle_bin".into(), json!(format!("none: {why}"))),
        };
        let mut leftovers = disk::usage(&found.leftovers);
        leftovers["root"] = json!(config.data_root);
        out.insert("leftovers".into(), leftovers);
        if !select.is_empty() {
            let data = recycle_max(&config.data_root).ok();
            let plan = found.plan(&select, (max.ok(), data));
            let mut cleanup = plan.summary();
            let (folders, leftover_folders) = found.emptied(config, &select, &plan);
            cleanup["folders"] = json!(folders);
            cleanup["leftover_folders"] = json!(leftover_folders);
            if select.missing {
                cleanup["missing"] = json!(missing.len());
            }
            out.insert("cleanup".into(), cleanup);
        }
        if !missing.is_empty() {
            let note = "capture rows the ledger says are on disk, with no file under the capture root (removed by \
                        hand, or under another capture root), by origin: quen_boots, imported_boots (harness_info) \
                        and no_boot (recorded without a boot: the harness import records each file of its cap \
                        folder so, its boot logs aside, by its path in that folder); ids lists the first 50, \
                        quen's boots' first";
            let ids = &missing[..missing.len().min(50)];
            out.insert(
                "missing".into(),
                json!({"rows": missing.len(), "quen_boots": by[0], "imported_boots": by[1],
                       "no_boot": by[2], "ids": ids, "note": note}),
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
    // Windows' question before a permanent delete takes the focus, and the game pauses without it. PresentMon's
    // capture exe runs only while something measures; PresentMonService.exe, its app's service, runs all the time.
    let procs = game::tree().unwrap_or_default();
    let measuring = |name: &str| name.starts_with("presentmon") && !name.contains("service");
    if let Some((.., name)) = procs
        .iter()
        .find(|(.., name)| measuring(&name.to_lowercase()))
    {
        return format!(
            "{name} runs (a measurement): a cleanup may open a Windows dialog, which takes the focus; clean once it \
             ends"
        )
        .into();
    }
    if let Err(why) = session.take_lock(&config, "capture_clean") {
        return why.into();
    }
    // With the lock, a journal is a dead quen's: its backups are among the leftovers, and its replay needs them.
    if session::journal_path(&config).exists() {
        session.release_lock();
        return "a journal a dead quen left exists: session_recover first"
            .to_string()
            .into();
    }
    let max = (
        recycle_max(&config.capture_root()),
        recycle_max(&config.data_root),
    );
    let done = take(&config, call, &select, max, &remove);
    session.release_lock();
    done
}

/// What became of a file a cleanup took.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Went {
    Recycled,
    /// Deleted for good, after the user said yes to Windows' question.
    Deleted,
    /// The user said no: it stays where it was.
    Kept,
}

/// What became of a file, and whether Windows' question came to the front (None: none showed).
type Removed = io::Result<(Went, Option<bool>)>;

/// Takes `path` off the disk through the Windows shell ([`win::shell_delete`]): `to` the Recycle Bin, or for good with
/// Windows asking the user first. `quen serve` shows no window of its own, so one of its process that shows meanwhile
/// is the shell's question: a watcher brings it to the front ([`input::focus`]), as a window a windowless background
/// process opens may come up behind the client's (memory-design §4.2; checked live).
fn remove(path: &Path, to: To) -> Removed {
    let done = AtomicBool::new(false);
    let (gone, front) = thread::scope(|s| {
        let watcher = s.spawn(|| {
            while !done.load(Ordering::Relaxed) {
                if let Some(window) = own_window() {
                    return Some(input::focus(window).is_ok());
                }
                thread::sleep(Duration::from_millis(50));
            }
            None
        });
        let gone = win::shell_delete(path, to == To::RecycleBin);
        done.store(true, Ordering::Relaxed);
        (gone, watcher.join().unwrap_or(None))
    });
    let went = match (gone?, front) {
        (false, _) => Went::Kept,
        (true, None) if to == To::RecycleBin => Went::Recycled,
        (true, _) => Went::Deleted,
    };
    Ok((went, front))
}

/// A window this process shows ([`capture::shown`]).
fn own_window() -> Option<HWND> {
    let me = std::process::id();
    let all = capture::top_windows().ok()?;
    all.into_iter()
        .find(|&w| capture::shown(w).is_some_and(|(pid, ..)| pid == me))
}

/// Takes what `select` takes, then marks its ledger rows; the lock is held. `max`: [`recycle_max`] of the capture
/// root's volume and of the data root's; `remove` takes a file off the disk ([`remove`]).
fn take(
    config: &Config,
    call: &mut Call,
    select: &Select,
    max: (Result<u64, String>, Result<u64, String>),
    remove: &dyn Fn(&Path, To) -> Removed,
) -> Done {
    let found = files(config);
    let plan = found.plan(select, (max.0.clone().ok(), max.1.clone().ok()));
    let root = config.capture_root();
    let (emptied, leftovers_emptied) = found.emptied(config, select, &plan);
    call.lap("plan");
    let (mut gone, mut deleted, mut kept, mut left) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for (n, (file, to)) in plan.take.iter().enumerate() {
        if call.stopped() {
            left.push(json!({"path": file.rel, "why": "cancelled before it"}));
            continue;
        }
        let under = if file.leftover {
            &config.data_root
        } else {
            &root
        };
        let path = under.join(file.rel.replace('/', "\\"));
        match remove(&path, *to) {
            Ok((went, in_front)) => {
                let entry = json!({"path": file.rel, "kind": file.kind, "gb": disk::gb(file.bytes), "in_front": in_front});
                match went {
                    Went::Recycled => gone.push((file, "recycle_bin")),
                    Went::Deleted => {
                        gone.push((file, "deleted"));
                        deleted.push(entry);
                    }
                    Went::Kept => kept.push(entry),
                }
            }
            Err(e) => left.push(json!({"path": file.rel, "why": e.to_string()})),
        }
        call.progress(&format!("{} of {} files", n + 1, plan.take.len()));
    }
    call.lap("remove");
    // The folders the cleanup emptied go, deepest first: only empty ones (remove_dir refuses any other, so a file that
    // stayed keeps its folders), never a link's target (feature tests 2b: a recovered ngfx boot's folder stayed).
    let removed = |under: &Path, dirs: Vec<String>| -> Vec<String> {
        dirs.into_iter()
            .filter(|dir| fs::remove_dir(under.join(dir.replace('/', "\\"))).is_ok())
            .collect()
    };
    let folders = removed(&root, emptied);
    let leftover_folders = removed(&config.data_root, leftovers_emptied);
    // Rows whose file was gone already, marked in one transaction (feature tests 2c: batches 1-2's 3,887).
    let missing = match select.missing {
        true => found.missing.0.as_slice(),
        false => &[],
    };
    let mut fields = Map::new();
    let marked = Ledger::open(config.state_dir().join("quen.db")).and_then(|mut ledger| {
        let t = ledger.utc(session::now() as i64)?;
        for (file, to) in &gone {
            if let Some(id) = file.capture {
                ledger.mark_removed(id, &t, to)?;
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
    let recycled: Vec<&&File> = gone
        .iter()
        .filter(|(_, to)| *to == "recycle_bin")
        .map(|(f, _)| f)
        .collect();
    let bytes = recycled.iter().map(|f| f.bytes).sum();
    fields.insert(
        "recycle_bin".into(),
        json!({"files": recycled.len(), "gb": disk::gb(bytes)}),
    );
    fields.insert("deleted".into(), json!(deleted));
    fields.insert("kept".into(), json!(kept));
    if let Err(why) = max.0.as_ref().and(max.1.as_ref()) {
        fields.insert("recycle_bin_off".into(), json!(why));
    }
    let kept_cited: Vec<Value> = plan
        .kept
        .iter()
        .map(|f| json!({"path": f.rel, "capture_id": f.capture}))
        .collect();
    fields.insert("kept_cited".into(), json!(kept_cited));
    fields.insert("folders".into(), json!(folders));
    fields.insert("leftover_folders".into(), json!(leftover_folders));
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
            format!("{} files not taken (left says why)", left.len()),
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

    /// capture_clean on a fake session's data root: refused while the session is live, without a selection or user_ok,
    /// and with a dead quen's journal. Then (with a stand-in Recycle Bin that takes 9 bytes, and a stand-in user who
    /// answers Windows' question) the small files go to the bin, the session's logs among them, the bigger one is
    /// deleted on a yes, the cited one stays unless named, and kept on a no; the rows are marked.
    #[test]
    fn clean_takes_what_it_selects_and_marks_the_rows() {
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
        // Session s1's logs, and the daemon's own log, which no cleanup takes.
        for rel in [
            r"logs\1792-s1\game.log",
            r"logs\1792-s1\game-logs\a.log",
            r"logs\serve.log",
        ] {
            let path = config.data_root.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "log").unwrap();
        }
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
        let rep = ledger
            .add_capture(&Capture {
                pinned: false,
                ..row("1/run.nsys-rep")
            })
            .unwrap();
        // Feature tests 2b: a row whose file is gone is counted by capture_usage.
        let gone = ledger.add_capture(&row("old/gone.png")).unwrap();
        drop(ledger);
        let used = usage(&fx.shared, &json!({}));
        assert_eq!(used["missing"]["rows"], 1, "{used}");
        assert_eq!(used["missing"]["ids"], json!([gone]), "{used}");
        // Finding 36: counted by origin; this row is a quen boot's.
        let by = ["quen_boots", "imported_boots", "no_boot"].map(|k| used["missing"][k].clone());
        assert_eq!(by, [json!(1), json!(0), json!(0)], "{used}");
        let kinds = used["leftovers"]["by_kind"].as_array().unwrap();
        assert!(
            kinds.contains(&json!({"kind": "session_logs", "files": 2, "gb": 0.0})),
            "{used}"
        );
        // And a selected boot's empty folder goes with the cleanup, as usage says first (feature tests 2c), its
        // session's log folder too.
        fs::create_dir_all(root.join(r"s1\l3c\deeper")).unwrap();
        let folders = json!(["s1/l3c/deeper", "s1/l3c", "1"]);
        let logs = json!(["logs/1792-s1/game-logs", "logs/1792-s1"]);
        let planned = usage(&fx.shared, &json!({"boots": [1]}));
        assert_eq!(planned["cleanup"]["folders"], folders, "{planned}");
        assert_eq!(planned["cleanup"]["leftover_folders"], logs, "{planned}");
        assert_eq!(planned["cleanup"]["ask_delete"]["files"], 0, "{planned}");

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
        // A dead quen's journal: its backups are among the leftovers, and its replay needs them.
        let journal = session::journal_path(&config);
        fs::write(&journal, "").unwrap();
        let out = run(&mut session, all.clone());
        let why = out["reason"].as_str().unwrap();
        assert!(why.contains("session_recover"), "{out}");
        fs::remove_file(&journal).unwrap(); // this test's own file
        assert!(
            root.join(r"s1\f1.png").exists(),
            "the refusals moved nothing"
        );

        let bin = fx.dir.join("recycled");
        fs::create_dir_all(&bin).unwrap();
        // The stand-in user answers Windows' question: no for the key frame, yes for the rest.
        let remove = |path: &Path, to: To| match to {
            To::RecycleBin => fs::rename(path, bin.join(path.file_name().unwrap()))
                .map(|()| (Went::Recycled, None)),
            To::AskDelete if path.ends_with("kf0.png") => Ok((Went::Kept, Some(true))),
            To::AskDelete => fs::remove_file(path).map(|()| (Went::Deleted, Some(true))),
        };
        let moved = |select: Value| {
            let mut call = session.call(crate::mcp::Conn::nobody(), Value::Null, None);
            let select = Select::parse(&select).unwrap();
            let done = take(&config, &mut call, &select, (Ok(9), Ok(9)), &remove);
            session.envelope("capture_clean", call, done)
        };
        let out = moved(json!({"boots": [1]}));
        assert_eq!(out["ok"], true, "{out}");
        assert_eq!(out["recycle_bin"]["files"], 3, "f1.png and the logs: {out}");
        let deleted =
            json!([{"path": "1/run.nsys-rep", "kind": "nsys-rep", "gb": 0.0, "in_front": true}]);
        assert_eq!(out["deleted"], deleted, "{out}");
        assert_eq!(out["kept_cited"][0]["capture_id"], cited, "{out}");
        assert!(bin.join("f1.png").exists() && bin.join("game.log").exists());
        assert!(root.join(r"s1\kf0.png").exists());
        assert!(config.data_root.join(r"logs\serve.log").exists());
        assert!(!root.join("1").exists(), "the emptied folder went");
        assert_eq!(out["folders"], folders, "{out}");
        assert_eq!(out["leftover_folders"], logs, "{out}");
        assert!(
            !root.join(r"s1\l3c").exists() && root.join("s1").is_dir(),
            "the empty folder went, the one holding a kept file stayed"
        );
        assert_eq!(out["ledger"]["captures"], json!([rep, plain]), "{out}");
        let ledger = Ledger::open_readonly(config.state_dir().join("quen.db")).unwrap();
        let sql = "SELECT id, removed_to FROM captures WHERE removed_at IS NOT NULL ORDER BY id";
        let marked = ledger
            .query(sql, &[], std::time::Duration::from_secs(5))
            .unwrap();
        let want = [
            [json!(plain), json!("recycle_bin")],
            [json!(rep), json!("deleted")],
        ];
        assert_eq!(marked.rows, want);
        // Named by id, the cited file is taken too (10 bytes: over the stand-in bin's 9); on the user's no, it stays.
        let named = moved(json!({"capture_ids": [cited]}));
        assert_eq!(named["ok"], true, "{named}");
        assert_eq!(named["kept"][0]["path"], "s1/kf0.png", "{named}");
        assert!(root.join(r"s1\kf0.png").exists());
        assert_eq!(named["ledger"]["captures"], json!(null), "{named}");
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

    /// The owner's rule, on the real shell: a permanent delete only on the user's yes in Windows' own dialog, which
    /// comes to the front from a process with no window of its own. It takes the foreground, so it runs only when
    /// asked, with the PC free: `cargo test -p quen windows_asks -- --ignored`. A stand-in user answers: no, then yes.
    #[test]
    #[ignore]
    fn windows_asks_before_a_permanent_delete_and_its_question_comes_to_the_front() {
        use windows::Win32::Foundation::{LPARAM, WPARAM};
        use windows::Win32::UI::WindowsAndMessaging::{IDYES, PostMessageW, WM_CLOSE, WM_COMMAND};
        let dir = std::env::temp_dir().join(format!("quen-ask-delete-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("synthetic.bin");
        for yes in [false, true] {
            fs::write(&file, b"synthetic").unwrap();
            // The question, once in front: no closes it, yes is its Yes button.
            let user = thread::spawn(move || {
                for _ in 0..200 {
                    if let Some(w) = own_window().filter(|&w| input::in_front(w)) {
                        let (msg, button) = match yes {
                            true => (WM_COMMAND, IDYES.0 as usize),
                            false => (WM_CLOSE, 0),
                        };
                        // SAFETY: a message to a live window of this process.
                        return unsafe { PostMessageW(Some(w), msg, WPARAM(button), LPARAM(0)) }
                            .is_ok();
                    }
                    thread::sleep(Duration::from_millis(50));
                }
                false
            });
            let (went, front) = remove(&file, To::AskDelete).unwrap();
            assert!(
                user.join().unwrap(),
                "yes {yes}: no question came to the front"
            );
            assert_eq!(front, Some(true), "yes {yes}");
            let want = if yes { Went::Deleted } else { Went::Kept };
            assert_eq!((went, file.exists()), (want, !yes), "yes {yes}");
        }
        fs::remove_dir(&dir).unwrap(); // this test's own folder, empty now
    }
}
