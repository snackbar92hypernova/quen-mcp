//! The import of the Python harness's history (docs/intent.md, "The ledger"), mechanical only: every file under its
//! `cap` folder except the scripts kept there (`*.sh`, `*.py`, `*.ps1`), one boot per `perf_boot_*.log` and per boot in
//! `cap/boots.jsonl`, the test saves `map/test_saves.md` lists, and one doc-reference scan. Planning reads files and
//! the ledger without hashing or writing, so a dry run is quick; applying hashes, then writes in one transaction.
//!
//! A second run adds nothing: a capture path already recorded is skipped (a changed file is only counted), as is a
//! boot that starts within one already recorded and a save whose file name is. Boot times are UTC seconds; the
//! harness's own `t` fields are local times, converted by SQLite with this machine's time zone rules for that date.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::Serialize;
use serde_json::{Map, Value};

use super::{
    BootEnd, BootStart, Capture, Error, Game, Ledger, Outcome, Result, Save, Scanner, glob,
};

/// Where the harness keeps its history, and what the import needs beside it.
pub struct Harness {
    /// The harness folder, holding `cap\` and `map\test_saves.md`.
    pub dir: PathBuf,
    /// The game's exe: the game row every imported boot and save belongs to. Hashed only when applying.
    pub game_exe: PathBuf,
    /// The game's `gamesaves` folder, where the listed test saves are hashed; None leaves them out.
    pub gamesaves: Option<PathBuf>,
    /// The folders whose `.md`, `.py`, `.sh`, `.ps1`, `.json` and `.toml` files the doc-reference scan reads.
    pub doc_roots: Vec<PathBuf>,
}

impl Harness {
    fn cap(&self) -> PathBuf {
        self.dir.join("cap")
    }
}

/// What an import adds (dry run) or added.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Summary {
    /// Files under `cap`, the scripts left out.
    pub files: usize,
    pub scripts_skipped: usize,
    pub files_new: usize,
    pub bytes_new: u64,
    /// Recorded before, with another size or time now: counted, never re-recorded.
    pub files_changed: usize,
    pub boot_logs: usize,
    pub boot_logs_new: usize,
    /// Boot records in `cap/boots.jsonl`.
    pub jsonl_boots: usize,
    pub boots_new: usize,
    pub boots_new_failed: usize,
    pub saves_listed: usize,
    pub saves_new: usize,
    /// Listed, but not in the gamesaves folder (or no folder given).
    pub saves_missing: usize,
    pub docs: usize,
    pub refs: usize,
    pub dangling: usize,
}

/// A boot already in the ledger, or the plan's `n`th new one.
#[derive(Debug, Clone, Copy)]
enum BootRef {
    Old(i64),
    New(usize),
}

struct NewFile {
    rel: String,
    abs: PathBuf,
    bytes: u64,
    mtime: i64,
    boot: Option<BootRef>,
}

struct NewBoot {
    start: i64,
    end: i64,
    failure: Option<String>,
    info: Option<Map<String, Value>>,
}

/// A boot's span for matching: a record or log starting within it is that boot. A boot has at most one log.
struct Span {
    start: i64,
    end: i64,
    failure: Option<String>,
    boot: BootRef,
    logged: bool,
}

/// What an import would do: [`Plan::summary`], and the rows to write.
pub struct Plan {
    pub summary: Summary,
    files: Vec<NewFile>,
    boots: Vec<NewBoot>,
    saves: Vec<(String, String, PathBuf)>,
    docs: Vec<PathBuf>,
}

/// How long after a recorded failed boot's start its log may end and still be that boot's (a log without boot's info
/// line has no start time of its own).
const FAILED_LOG_WITHIN_S: i64 = 3600;
const DOC_EXTENSIONS: &[&str] = &["md", "py", "sh", "ps1", "json", "toml"];
const SCRIPT_EXTENSIONS: &[&str] = &["sh", "py", "ps1"];

impl Ledger {
    /// Plans the import: reads the harness's files and the ledger, hashes nothing and writes nothing. Works on a
    /// read-only ledger.
    pub fn plan_migration(&self, h: &Harness) -> Result<Plan> {
        let cap = h.cap();
        let mut found = Vec::new();
        walk(&cap, &mut found).map_err(|e| refused(&cap, e))?;
        found.sort();
        let mut known: HashMap<String, (u64, i64)> = HashMap::new();
        {
            let mut stmt = self
                .conn
                .prepare("SELECT path, bytes, CAST(strftime('%s', t) AS INTEGER) FROM captures")?;
            let rows =
                stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, (r.get(1)?, r.get(2)?))))?;
            for row in rows {
                let (path, seen) = row?;
                known.insert(path.to_lowercase(), seen);
            }
        }
        let mut s = Summary::default();
        let mut files = Vec::new();
        for (abs, bytes, mtime) in found {
            let rel = rel_path(&cap, &abs);
            if SCRIPT_EXTENSIONS.contains(&extension(&rel).as_str()) {
                s.scripts_skipped += 1;
                continue;
            }
            s.files += 1;
            let log = is_boot_log(&rel);
            s.boot_logs += usize::from(log);
            match known.get(&rel.to_lowercase()) {
                Some(&seen) => s.files_changed += usize::from(seen != (bytes, mtime)),
                None => {
                    s.files_new += 1;
                    s.bytes_new += bytes;
                    s.boot_logs_new += usize::from(log);
                    files.push(NewFile {
                        rel,
                        abs,
                        bytes,
                        mtime,
                        boot: None,
                    });
                }
            }
        }

        let mut spans: Vec<Span> = {
            let mut stmt = self.conn.prepare(
                "SELECT id, CAST(strftime('%s', t_start) AS INTEGER),
                   CAST(strftime('%s', coalesce(t_end, t_start)) AS INTEGER), failure,
                   EXISTS (SELECT 1 FROM captures c WHERE c.boot_id = boots.id AND c.kind = 'boot_log')
                 FROM boots",
            )?;
            stmt.query_map([], |r| {
                Ok(Span {
                    boot: BootRef::Old(r.get(0)?),
                    start: r.get(1)?,
                    end: r.get(2)?,
                    failure: r.get(3)?,
                    logged: r.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?
        };
        let mut boots = Vec::new();
        let mut add = |spans: &mut Vec<Span>, boot: NewBoot, logged: bool| {
            spans.push(Span {
                start: boot.start,
                end: boot.end,
                failure: boot.failure.clone(),
                boot: BootRef::New(boots.len()),
                logged,
            });
            boots.push(boot);
            BootRef::New(boots.len() - 1)
        };

        // boots.jsonl first: its records have exact start times, so a log of the same boot links to it.
        let jsonl = cap.join("boots.jsonl");
        let text = match fs::read(&jsonl) {
            Ok(data) => String::from_utf8_lossy(&data).into_owned(),
            Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(refused(&jsonl, e)),
        };
        let mut ends = HashMap::new();
        let mut records = Vec::new();
        for line in text.lines() {
            let Ok(mut rec) = serde_json::from_str::<Map<String, Value>>(line) else {
                continue;
            };
            match rec.remove("event").as_ref().and_then(Value::as_str) {
                Some("boot") => records.push(rec),
                Some("end") => {
                    let t0 = rec.get("t_boot").and_then(Value::as_f64);
                    if let (Some(t0), Some(t)) = (t0, rec.get("t").and_then(Value::as_str)) {
                        ends.insert(t0.to_bits(), t.to_owned());
                    }
                }
                _ => {}
            }
        }
        for rec in records {
            s.jsonl_boots += 1;
            let Some(t0) = rec.get("t_boot").and_then(Value::as_f64) else {
                continue;
            };
            let start = t0.floor() as i64;
            if spans.iter().any(|b| b.start <= start && start <= b.end) {
                continue;
            }
            let end = match ends.get(&t0.to_bits()) {
                Some(t) => self.local_secs(t)?.unwrap_or(start).max(start),
                None => start,
            };
            let failure = rec.get("error").and_then(Value::as_str).map(str::to_owned);
            add(
                &mut spans,
                NewBoot {
                    start,
                    end,
                    failure,
                    info: Some(rec),
                },
                false,
            );
        }

        for f in files.iter_mut().filter(|f| is_boot_log(&f.rel)) {
            let text = fs::read(&f.abs).map_err(|e| refused(&f.abs, e))?;
            let (info, failure) = read_boot_log(&String::from_utf8_lossy(&text));
            let t0 = info
                .as_ref()
                .and_then(|i| i.get("t_boot"))
                .and_then(Value::as_f64);
            let t0 = t0.map(|t| t.floor() as i64);
            let found = (0..spans.len())
                .filter(|&i| !spans[i].logged)
                .filter(|&i| {
                    let b = &spans[i];
                    match (t0, &failure) {
                        (Some(t0), _) => b.start <= t0 && t0 <= b.end,
                        (None, Some(failure)) => {
                            b.failure.as_ref() == Some(failure)
                                && b.start <= f.mtime
                                && f.mtime <= b.start + FAILED_LOG_WITHIN_S
                        }
                        (None, None) => false,
                    }
                })
                .max_by_key(|&i| spans[i].start);
            f.boot = Some(match found {
                Some(i) => {
                    spans[i].logged = true;
                    spans[i].boot
                }
                None => {
                    let start = t0.unwrap_or(f.mtime);
                    let failure = failure.or_else(|| {
                        info.is_none().then(|| {
                            "the log has neither boot's info line nor a traceback (killed?)".into()
                        })
                    });
                    let boot = NewBoot {
                        start,
                        end: f.mtime.max(start),
                        failure,
                        info,
                    };
                    add(&mut spans, boot, true)
                }
            });
        }
        s.boots_new = boots.len();
        s.boots_new_failed = boots.iter().filter(|b| b.failure.is_some()).count();

        let mut saves: Vec<(String, String, PathBuf)> = Vec::new();
        let listed = h.dir.join("map").join("test_saves.md");
        if let Ok(text) = fs::read(&listed) {
            for (file, note) in test_saves(&String::from_utf8_lossy(&text)) {
                s.saves_listed += 1;
                let path = h.gamesaves.as_ref().map(|d| d.join(&file));
                let Some(path) = path.filter(|p| p.is_file()) else {
                    s.saves_missing += 1;
                    continue;
                };
                let present: bool = self.conn.query_row(
                    "SELECT count(*) > 0 FROM saves WHERE file = ?1 COLLATE NOCASE",
                    [&file],
                    |r| r.get(0),
                )?;
                if !present && !saves.iter().any(|(f, _, _)| f.eq_ignore_ascii_case(&file)) {
                    saves.push((file, note, path));
                }
            }
        }
        s.saves_new = saves.len();

        let mut docs = Vec::new();
        for root in &h.doc_roots {
            let mut found = Vec::new();
            walk(root, &mut found).map_err(|e| refused(root, e))?;
            docs.extend(
                found
                    .into_iter()
                    .map(|(p, _, _)| p)
                    .filter(|p| DOC_EXTENSIONS.contains(&extension(&p.to_string_lossy()).as_str())),
            );
        }
        docs.sort();
        docs.dedup();
        let mut paths: Vec<&str> = known.keys().map(String::as_str).collect();
        paths.extend(files.iter().map(|f| f.rel.as_str()));
        let scanner = Scanner::new(&cap.to_string_lossy(), &paths)?;
        s.docs = docs.len();
        for doc in &docs {
            let text = fs::read(doc).map_err(|e| refused(doc, e))?;
            for r in scanner.refs(&String::from_utf8_lossy(&text)) {
                s.refs += 1;
                s.dangling += usize::from(r.matches.is_empty());
            }
        }
        Ok(Plan {
            summary: s,
            files,
            boots,
            saves,
            docs,
        })
    }

    /// Applies a plan from [`Ledger::plan_migration`] on this ledger: hashes every new file, the test saves and the
    /// game's exe first, then writes everything in one transaction, so a failure leaves the ledger as it was. Refused
    /// while a harness session runs or was not ended (its `cap/ini/session.lock`): its boot log would be half written.
    pub fn apply_migration(&mut self, h: &Harness, plan: Plan) -> Result<Summary> {
        let lock = h.cap().join("ini").join("session.lock");
        if lock.exists() {
            return Err(Error::Refused(format!(
                "{} exists: a harness session runs or was not ended (`python ini_session.py end` or `recover`)",
                lock.display()
            )));
        }
        let exe_sha256 = sha256_file(&h.game_exe)?;
        let shas = plan
            .files
            .iter()
            .map(|f| sha256_file(&f.abs))
            .collect::<Result<Vec<_>>>()?;
        let save_shas = plan
            .saves
            .iter()
            .map(|(_, _, p)| sha256_file(p))
            .collect::<Result<Vec<_>>>()?;
        let mut texts = Vec::with_capacity(plan.docs.len());
        for doc in &plan.docs {
            let text = fs::read(doc).map_err(|e| refused(doc, e))?;
            texts.push((
                doc.to_string_lossy().into_owned(),
                String::from_utf8_lossy(&text).into_owned(),
            ));
        }
        let mut s = plan.summary;
        self.atomic(|l| {
            let game_id = l.add_game(&Game {
                name: h
                    .game_exe
                    .file_stem()
                    .map_or("game".into(), |n| n.to_string_lossy().into_owned()),
                exe_path: h.game_exe.to_string_lossy().into_owned(),
                exe_sha256,
                build_stamp: None,
            })?;
            for ((file, note, _), sha256) in plan.saves.iter().zip(save_shas) {
                l.add_save(&Save {
                    game_id,
                    file: file.clone(),
                    sha256,
                    note: Some(note.clone()),
                    ..Save::default()
                })?;
            }
            let mut ids = Vec::with_capacity(plan.boots.len());
            for b in plan.boots {
                let save = b.info.as_ref().and_then(|i| {
                    i.get("quicksave")
                        .and_then(|q| q.get("save"))
                        .or_else(|| i.get("save"))
                        .and_then(Value::as_str)
                });
                let save_id: Option<i64> = l.conn.query_row(
                    "SELECT max(id) FROM saves WHERE game_id = ?1 AND file = ?2 COLLATE NOCASE",
                    rusqlite::params![game_id, save],
                    |r| r.get(0),
                )?;
                // Boot sets `no_devkit` late, so only a boot that got through says which it was.
                let devkit = b
                    .info
                    .as_ref()
                    .filter(|_| b.failure.is_none())
                    .map(|i| i.get("no_devkit") != Some(&Value::Bool(true)));
                let (t_start, t_end) = (l.utc(b.start)?, l.utc(b.end)?);
                let id = l.start_boot(&BootStart {
                    game_id,
                    t_start,
                    save_id,
                    devkit,
                    harness_info: b.info,
                    ..Default::default()
                })?;
                let outcome = if b.failure.is_some() {
                    Outcome::Failed
                } else {
                    Outcome::Ok
                };
                l.finish_boot(
                    id,
                    &BootEnd {
                        t_end,
                        outcome,
                        failure: b.failure,
                        patches: Vec::new(),
                    },
                )?;
                ids.push(id);
            }
            for (f, sha256) in plan.files.iter().zip(shas) {
                let (kind, method, pinned) = kind(&f.rel);
                let t = l.utc(f.mtime)?;
                l.add_capture(&Capture {
                    boot_id: f.boot.map(|b| match b {
                        BootRef::Old(id) => id,
                        BootRef::New(n) => ids[n],
                    }),
                    path: f.rel.clone(),
                    sha256,
                    bytes: f.bytes as i64,
                    kind,
                    method: method.map(str::to_owned),
                    t,
                    pinned,
                    ..Default::default()
                })?;
            }
            let now: String =
                l.conn
                    .query_row("SELECT strftime('%Y-%m-%dT%H:%M:%SZ', 'now')", [], |r| {
                        r.get(0)
                    })?;
            let docs: Vec<(&str, &str)> = texts
                .iter()
                .map(|(p, t)| (p.as_str(), t.as_str()))
                .collect();
            let scan = l.scan_docs(&docs, &h.cap().to_string_lossy(), &now)?;
            (s.docs, s.refs, s.dangling) = (scan.docs, scan.refs, scan.dangling);
            Ok(())
        })?;
        Ok(s)
    }

    /// The ledger's time text, UTC `YYYY-MM-DDTHH:MM:SSZ`, for seconds since 1970, by SQLite's own calendar.
    pub fn utc(&self, secs: i64) -> Result<String> {
        Ok(self.conn.query_row(
            "SELECT strftime('%Y-%m-%dT%H:%M:%SZ', ?1, 'unixepoch')",
            [secs],
            |r| r.get(0),
        )?)
    }

    /// Seconds since 1970 of a local `YYYY-MM-DD HH:MM:SS`; None for another spelling.
    fn local_secs(&self, t: &str) -> Result<Option<i64>> {
        Ok(self.conn.query_row(
            "SELECT CAST(strftime('%s', ?1, 'utc') AS INTEGER)",
            [t],
            |r| r.get(0),
        )?)
    }
}

/// From a boot log: boot's info dict (its last line that is a JSON object; under a profiler, the profiler's own lines
/// may follow) and, after the last traceback, the exception line.
fn read_boot_log(text: &str) -> (Option<Map<String, Value>>, Option<String>) {
    let lines: Vec<&str> = text.lines().collect();
    let info = lines
        .iter()
        .rev()
        .filter(|l| l.trim_start().starts_with('{'))
        .find_map(|l| serde_json::from_str(l.trim()).ok());
    let failure = lines
        .iter()
        .rposition(|l| l.starts_with("Traceback (most recent call last)"))
        .map(|at| {
            lines[at + 1..]
                .iter()
                .find(|l| !l.trim().is_empty() && !l.starts_with(char::is_whitespace))
                .map_or("a traceback without its exception line".into(), |l| {
                    l.trim().to_owned()
                })
        });
    (info, failure)
}

/// The rows of the Markdown tables in `map/test_saves.md` that name a `.sav` file: (file, the row's other cells).
fn test_saves(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let row = line.trim().strip_prefix('|')?.strip_suffix('|')?;
            let cells: Vec<&str> = row.split('|').map(|c| c.trim().trim_matches('`')).collect();
            let at = cells.iter().position(|c| {
                c.to_ascii_lowercase().ends_with(".sav") && !c.contains(char::is_whitespace)
            })?;
            let note: Vec<&str> = (0..cells.len())
                .filter(|&i| i != at && !cells[i].is_empty())
                .map(|i| cells[i])
                .collect();
            Some((cells[at].to_owned(), note.join(" | ")))
        })
        .collect()
}

fn is_boot_log(rel: &str) -> bool {
    glob("perf_boot_*.log", &rel.to_ascii_lowercase())
}

/// A capture's kind, method and pinned flag. ORB references for camera poses and the camera calibration are pinned.
fn kind(rel: &str) -> (String, Option<&'static str>, bool) {
    let lower = rel.to_ascii_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    if is_boot_log(rel) {
        ("boot_log".into(), None, false)
    } else if lower == "boots.jsonl" {
        ("boots_jsonl".into(), None, false)
    } else if glob("ini/perf_*.csv", &lower) {
        ("presentmon_csv".into(), Some("presentmon"), false)
    } else if glob("camera_ref_*.png", name) {
        ("camera_ref".into(), None, true)
    } else if name == "camera_cal.json" {
        ("camera_cal".into(), None, true)
    } else {
        (extension(&lower), None, false)
    }
}

/// The lowercase extension of a file name or path, or `file` when it has none.
fn extension(path: &str) -> String {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    name.rsplit_once('.')
        .map_or("file".into(), |(_, ext)| ext.to_ascii_lowercase())
}

/// `abs` relative to `root`, with `/` separators.
fn rel_path(root: &Path, abs: &Path) -> String {
    let rel = abs.strip_prefix(root).unwrap_or(abs);
    let parts: Vec<_> = rel.iter().map(|p| p.to_string_lossy()).collect();
    parts.join("/")
}

/// Every file under `dir` at any depth as (path, size, mtime in seconds since 1970). Links are not followed.
fn walk(dir: &Path, out: &mut Vec<(PathBuf, u64, i64)>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        if ty.is_dir() {
            walk(&entry.path(), out)?;
        } else if ty.is_file() {
            let meta = entry.metadata()?;
            let mtime = meta.modified()?.duration_since(UNIX_EPOCH);
            out.push((
                entry.path(),
                meta.len(),
                mtime.map_or(0, |d| d.as_secs() as i64),
            ));
        }
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
    crate::journal::sha256_file(path).map_err(|e| refused(path, e))
}

fn refused(path: &Path, e: io::Error) -> Error {
    Error::Refused(format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_boot_log_gives_its_info_line_and_the_last_exception() {
        let ok = "starting\n{\"t_boot\": 1.5, \"save\": \"a.sav\"}\nprofiler: done\n\n";
        let (info, failure) = read_boot_log(ok);
        assert_eq!(info.unwrap()["save"], "a.sav");
        assert_eq!(failure, None);
        let failed = "boot failed, restored: {}\nTraceback (most recent call last):\n  File \"x.py\", line 1, in f\n    \
                      g()\n    ~^^\nValueError: first\n\nDuring handling of the above exception, another exception \
                      occurred:\n\nTraceback (most recent call last):\n  File \"x.py\", line 2, in g\nRuntimeError: \
                      the menu never showed\n";
        let (info, failure) = read_boot_log(failed);
        assert!(info.is_none());
        assert_eq!(
            failure.as_deref(),
            Some("RuntimeError: the menu never showed")
        );
    }

    #[test]
    fn test_saves_are_the_table_rows_that_name_a_save() {
        let md = "# Saves\n\n| # | scenario | file | npc |\n|---|---|---|---|\n| 1 | a hut | `Manual_1.sav` | no |\n\
                  | 2 | a bog | Quick_2.SAV |  |\nnot a table: Other.sav\n| 3 | no file | none | x |\n";
        assert_eq!(
            test_saves(md),
            [
                ("Manual_1.sav".into(), "1 | a hut | no".into()),
                ("Quick_2.SAV".into(), "2 | a bog".into())
            ]
        );
    }

    #[test]
    fn kinds_follow_the_harness_names() {
        assert_eq!(kind("perf_boot_x.log"), ("boot_log".into(), None, false));
        assert_eq!(kind("old/perf_boot_x.log").0, "log");
        assert_eq!(
            kind("ini/perf_A1.csv"),
            ("presentmon_csv".into(), Some("presentmon"), false)
        );
        assert_eq!(
            kind("ini/camera_ref_P1.png"),
            ("camera_ref".into(), None, true)
        );
        assert_eq!(
            kind("ini/camera_cal.json"),
            ("camera_cal".into(), None, true)
        );
        assert_eq!(kind("trace/run.TRACY").0, "tracy");
        assert_eq!(kind("README").0, "file");
    }
}
