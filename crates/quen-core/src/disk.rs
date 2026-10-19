//! The capture root on disk: every file there, which boot it belongs to, what its ledger row says, the usage
//! by boot, kind and age, and what a cleanup takes and where to. It only reads and plans: quen's capture_clean
//! (crates/quen disk.rs) moves the files, to the Recycle Bin or `_to_delete`, never deleting one.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Serialize;
use serde_json::{Value, json};

use crate::ledger::CaptureRow;

/// A file over this is listed on its own (profiler outputs: a RenderDoc capture is 3-5 GB).
pub const LARGE: u64 = 1_000_000_000;
const GB: f64 = 1e9;

/// A file under the capture root.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct File {
    /// Relative to the capture root, `/` separators.
    pub rel: String,
    pub bytes: u64,
    pub age_days: f64,
    /// From its folder: a boot's id, or its session (see `Ledger::boot_folders`), else its ledger row's.
    pub boot: Option<i64>,
    /// The lowercase extension, as `captures.kind` has it (`file` without one).
    pub kind: String,
    /// Its ledger row, if it has one.
    pub capture: Option<i64>,
    /// Kept by a cleanup unless named by its capture id (`CaptureRow::cited`).
    pub cited: bool,
}

/// GB with two decimals.
pub fn gb(bytes: u64) -> f64 {
    (bytes as f64 / GB * 100.0).round() / 100.0
}

/// Every file under `dir`, recursively: its path and size. A folder that can't be read is skipped.
fn walk(dir: &Path, out: &mut Vec<(PathBuf, fs::Metadata)>) {
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        match entry.metadata() {
            Ok(m) if m.is_dir() => walk(&entry.path(), out),
            Ok(m) => out.push((entry.path(), m)),
            Err(_) => {}
        }
    }
}

/// `path` under `root`, its names joined with `/`. Split by the platform's own separators: on Linux a `\` is part of
/// a name.
fn relative(root: &Path, path: &Path) -> Option<String> {
    let names: Vec<_> = path
        .strip_prefix(root)
        .ok()?
        .iter()
        .map(|n| n.to_string_lossy())
        .collect();
    Some(names.join("/"))
}

/// Every folder under `root`, relative with `/` separators; a link isn't followed.
pub fn folders(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut todo = vec![root.to_path_buf()];
    while let Some(dir) = todo.pop() {
        for entry in fs::read_dir(&dir).into_iter().flatten().flatten() {
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                let path = entry.path();
                out.extend(relative(root, &path));
                todo.push(path);
            }
        }
    }
    out
}

/// The bytes under `dir`, recursively (0 when it doesn't exist).
pub fn total(dir: &Path) -> u64 {
    let mut files = Vec::new();
    walk(dir, &mut files);
    files.iter().map(|(_, m)| m.len()).sum()
}

/// `path` lowercase with `/` separators, for comparing ledger paths with files.
fn norm(path: &str) -> String {
    path.replace('\\', "/").to_lowercase()
}

/// Every file under `root` at `now`, joined to the ledger: `rows` (its captures on disk) and `folders` (the folder
/// names that belong to a boot, lowercase).
pub fn scan(
    root: &Path,
    now: SystemTime,
    rows: &[CaptureRow],
    folders: &[(String, i64)],
) -> Vec<File> {
    let mut found = Vec::new();
    walk(root, &mut found);
    let mut files: Vec<File> = found
        .into_iter()
        .filter_map(|(path, meta)| {
            let rel = relative(root, &path)?;
            let age = meta
                .modified()
                .ok()
                .and_then(|t| now.duration_since(t).ok());
            let row = rows.iter().find(|r| norm(&r.path) == rel.to_lowercase());
            let first = rel.split('/').next().unwrap_or_default().to_lowercase();
            let folder = folders.iter().find(|(f, _)| *f == first).map(|(_, b)| *b);
            let kind = Path::new(&rel)
                .extension()
                .map_or("file".into(), |x| x.to_string_lossy().to_lowercase());
            Some(File {
                boot: folder.or(row.and_then(|r| r.boot_id)),
                kind,
                capture: row.map(|r| r.id),
                cited: row.is_some_and(|r| r.cited),
                age_days: age.map_or(0.0, |a| a.as_secs_f64() / 86_400.0),
                bytes: meta.len(),
                rel,
            })
        })
        .collect();
    files.sort_by(|a, b| a.rel.cmp(&b.rel));
    files
}

/// The ids of `rows` (captures the ledger says are on disk) that no file of `files` matches: removed by hand, or kept
/// under another capture root (feature tests 2b). Sorted.
pub fn missing(rows: &[CaptureRow], files: &[File]) -> Vec<i64> {
    let found: std::collections::HashSet<i64> = files.iter().filter_map(|f| f.capture).collect();
    let mut ids: Vec<i64> = rows
        .iter()
        .map(|r| r.id)
        .filter(|id| !found.contains(id))
        .collect();
    ids.sort_unstable();
    ids
}

/// The usage: totals by boot (largest first), by kind and by age, and every file over [`LARGE`].
pub fn usage(files: &[File]) -> Value {
    let sum = |f: &dyn Fn(&File) -> bool| {
        let picked: Vec<&File> = files.iter().filter(|x| f(x)).collect();
        let bytes: u64 = picked.iter().map(|x| x.bytes).sum();
        (picked.len(), bytes)
    };
    let mut boots: Vec<Option<i64>> = files.iter().map(|f| f.boot).collect();
    boots.sort();
    boots.dedup();
    let mut by_boot: Vec<(u64, Value)> = boots
        .into_iter()
        .map(|boot| {
            let (n, bytes) = sum(&|f| f.boot == boot);
            let newest = files
                .iter()
                .filter(|f| f.boot == boot)
                .map(|f| f.age_days)
                .fold(f64::INFINITY, f64::min);
            let days = (newest * 10.0).round() / 10.0;
            (
                bytes,
                json!({"boot": boot, "files": n, "gb": gb(bytes), "newest_age_days": days}),
            )
        })
        .collect();
    by_boot.sort_by(|a, b| b.0.cmp(&a.0));
    let mut kinds: Vec<&str> = files.iter().map(|f| f.kind.as_str()).collect();
    kinds.sort();
    kinds.dedup();
    let by_kind: Vec<Value> = kinds
        .into_iter()
        .map(|kind| {
            let (n, bytes) = sum(&|f| f.kind == kind);
            json!({"kind": kind, "files": n, "gb": gb(bytes)})
        })
        .collect();
    let ages = [
        ("under 1 day", 0.0, 1.0),
        ("1 to 7 days", 1.0, 7.0),
        ("7 to 30 days", 7.0, 30.0),
        ("over 30 days", 30.0, f64::INFINITY),
    ];
    let by_age: Vec<Value> = ages
        .into_iter()
        .map(|(age, from, to)| {
            let (n, bytes) = sum(&|f| f.age_days >= from && f.age_days < to);
            json!({"age": age, "files": n, "gb": gb(bytes)})
        })
        .collect();
    let (n, bytes) = sum(&|_| true);
    json!({
        "files": n,
        "gb": gb(bytes),
        "by_boot": by_boot.into_iter().map(|(_, v)| v).collect::<Vec<_>>(),
        "by_kind": by_kind,
        "by_age": by_age,
        "large": large(files.iter()),
    })
}

/// The files over [`LARGE`].
pub fn large<'a>(files: impl Iterator<Item = &'a File>) -> Vec<Value> {
    files
        .filter(|f| f.bytes > LARGE)
        .map(|f| json!({"path": f.rel, "gb": gb(f.bytes), "boot": f.boot, "kind": f.kind}))
        .collect()
}

/// Which files a cleanup takes: those every given filter matches (boot ids, older than so many days, kinds), and
/// those named by capture id. A cited file is kept unless named. `missing`: the rows whose file is gone ([`missing`])
/// are marked removed too, to `missing`; no file moves for them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Select {
    pub boots: Option<Vec<i64>>,
    pub older_than_days: Option<f64>,
    pub kinds: Option<Vec<String>>,
    pub capture_ids: Vec<i64>,
    pub missing: bool,
}

impl Select {
    /// From a tool's arguments `boots`, `older_than_days`, `kinds`, `capture_ids` and `missing`; the caller checks for
    /// others.
    pub fn parse(args: &Value) -> Result<Select, String> {
        let ints = |key: &str| -> Result<Option<Vec<i64>>, String> {
            let Some(v) = args.get(key) else {
                return Ok(None);
            };
            let ids = v
                .as_array()
                .and_then(|a| a.iter().map(Value::as_i64).collect());
            ids.map(Some)
                .ok_or(format!("{key} must be an array of whole numbers"))
        };
        let kinds =
            match args.get("kinds") {
                None => None,
                Some(v) => {
                    let kinds: Option<Vec<String>> = v.as_array().and_then(|a| {
                        a.iter()
                            .map(|k| Some(k.as_str()?.trim_start_matches('.').to_lowercase()))
                            .collect()
                    });
                    Some(kinds.ok_or(
                        "kinds must be an array of file extensions, e.g. [\"rdc\", \"png\"]",
                    )?)
                }
            };
        let older_than_days = match args.get("older_than_days") {
            None => None,
            Some(v) => Some(
                v.as_f64()
                    .filter(|d| *d >= 0.0)
                    .ok_or("older_than_days must be a number from 0")?,
            ),
        };
        let missing = match args.get("missing") {
            None => false,
            Some(v) => v.as_bool().ok_or("missing must be true or false")?,
        };
        Ok(Select {
            boots: ints("boots")?,
            older_than_days,
            kinds,
            capture_ids: ints("capture_ids")?.unwrap_or_default(),
            missing,
        })
    }

    pub fn is_empty(&self) -> bool {
        *self == Select::default()
    }

    /// Whether every given filter matches `f` (never without one).
    fn matches(&self, f: &File) -> bool {
        let any = self.boots.is_some() || self.older_than_days.is_some() || self.kinds.is_some();
        any && self
            .boots
            .as_ref()
            .is_none_or(|b| f.boot.is_some_and(|x| b.contains(&x)))
            && self.older_than_days.is_none_or(|d| f.age_days > d)
            && self.kinds.as_ref().is_none_or(|k| k.contains(&f.kind))
    }
}

/// Where a cleanup sends a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum To {
    RecycleBin,
    /// `<data root>\_to_delete\<date>\`, for the user to empty: what the Recycle Bin can't take whole.
    ToDelete,
}

impl To {
    /// `captures.removed_to`.
    pub fn as_str(self) -> &'static str {
        match self {
            To::RecycleBin => "recycle_bin",
            To::ToDelete => "to_delete",
        }
    }
}

/// What a cleanup takes, and the cited files it keeps.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Plan {
    pub take: Vec<(File, To)>,
    pub kept: Vec<File>,
}

/// The cleanup `select` asks for. `recycle_max`: the largest file the Recycle Bin takes whole, None when it takes
/// nothing (it would delete instead); bigger files go to `_to_delete`.
pub fn plan(files: &[File], select: &Select, recycle_max: Option<u64>) -> Plan {
    let mut out = Plan::default();
    for f in files {
        let named = f.capture.is_some_and(|id| select.capture_ids.contains(&id));
        if !named && !select.matches(f) {
            continue;
        }
        if f.cited && !named {
            out.kept.push(f.clone());
            continue;
        }
        let to = match recycle_max {
            Some(max) if f.bytes <= max => To::RecycleBin,
            _ => To::ToDelete,
        };
        out.take.push((f.clone(), to));
    }
    out
}

/// The folders of `dirs` (every folder under the capture root, [`folders`]) that a cleanup taking `plan` empties,
/// deepest first: each that holds no file but taken ones, at any depth, and held a taken file or lies in a selected
/// boot's folder (`boots`: their names, lowercase), with its subfolders (feature tests 2b, 2c).
pub fn emptied(dirs: &[String], files: &[File], plan: &Plan, boots: &[&str]) -> Vec<String> {
    let taken: Vec<&str> = plan.take.iter().map(|(f, _)| f.rel.as_str()).collect();
    let beneath = |dir: &str, rel: &str| {
        rel.strip_prefix(dir)
            .is_some_and(|rest| rest.starts_with('/'))
    };
    let goes = |dir: &str| {
        let first = dir.split('/').next().unwrap_or_default().to_lowercase();
        let held = taken.iter().any(|t| beneath(dir, t)) || boots.contains(&first.as_str());
        let left = files
            .iter()
            .any(|f| beneath(dir, &f.rel) && !taken.contains(&f.rel.as_str()));
        held && !left
    };
    let mut out: Vec<String> = dirs
        .iter()
        .filter(|d| {
            std::iter::successors(Some(d.as_str()), |a| a.rsplit_once('/').map(|(up, _)| up))
                .any(goes)
        })
        .cloned()
        .collect();
    out.sort_by(|a, b| b.len().cmp(&a.len()).then(a.cmp(b)));
    out
}

impl Plan {
    /// Its summary: files and GB by destination, and the cited files kept.
    pub fn summary(&self) -> Value {
        let to = |dest: To| {
            let picked: Vec<&File> = self
                .take
                .iter()
                .filter(|(_, t)| *t == dest)
                .map(|(f, _)| f)
                .collect();
            json!({"files": picked.len(), "gb": gb(picked.iter().map(|f| f.bytes).sum())})
        };
        let kept: Vec<Value> = self
            .kept
            .iter()
            .map(|f| json!({"path": f.rel, "capture_id": f.capture}))
            .collect();
        json!({
            "files": self.take.len(),
            "gb": gb(self.take.iter().map(|(f, _)| f.bytes).sum()),
            "recycle_bin": to(To::RecycleBin),
            "to_delete": to(To::ToDelete),
            "kept_cited": kept,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// A capture root of this test's own: a profiler boot's folder, a session folder, a file of no boot.
    struct Temp(PathBuf);

    impl Temp {
        fn new(name: &str) -> Temp {
            let dir = std::env::temp_dir().join(format!("quen-disk-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir); // this test's own temp folder, from an earlier run
            Temp(dir)
        }

        fn put(&self, rel: &str, bytes: usize) {
            let path = self.0.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, vec![7u8; bytes]).unwrap();
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0); // this test's own temp folder
        }
    }

    fn row(id: i64, path: &str, boot: Option<i64>, cited: bool) -> CaptureRow {
        CaptureRow {
            id,
            path: path.into(),
            boot_id: boot,
            cited,
        }
    }

    #[test]
    fn files_join_their_boot_and_row_and_sum_by_boot_kind_and_age() {
        let t = Temp::new("scan");
        t.put("132/l1.rdc", 500);
        t.put("ab12cd/shot.png", 30);
        t.put("ab12cd/run.csv", 20);
        t.put("loose.txt", 5);
        let rows = [
            row(1, r"132\L1.rdc", Some(132), false),
            row(2, "ab12cd/shot.png", Some(130), true),
        ];
        let folders = [("132".to_owned(), 132), ("ab12cd".to_owned(), 130)];
        let now = SystemTime::now() + Duration::from_secs(10 * 86_400);
        let files = scan(&t.0, now, &rows, &folders);
        let got: Vec<String> = files
            .iter()
            .map(|f| {
                format!(
                    "{} {:?} {} {:?} {}",
                    f.rel, f.boot, f.kind, f.capture, f.cited
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                "132/l1.rdc Some(132) rdc Some(1) false",
                "ab12cd/run.csv Some(130) csv None false",
                "ab12cd/shot.png Some(130) png Some(2) true",
                "loose.txt None txt None false",
            ]
        );
        assert!(
            files.iter().all(|f| (f.age_days - 10.0).abs() < 0.1),
            "{files:?}"
        );
        // Feature tests 2b: rows whose file is gone are counted.
        let gone = [
            rows[0].clone(),
            row(9, "old/x.png", Some(9), false),
            row(5, "y.png", None, true),
        ];
        assert_eq!(missing(&gone, &files), [5, 9]);
        assert_eq!(total(&t.0), 555);
        let mut dirs = super::folders(&t.0);
        dirs.sort();
        assert_eq!(dirs, ["132", "ab12cd"]);
        let u = usage(&files);
        assert_eq!(u["files"], 4);
        assert_eq!(u["by_boot"][0]["boot"], 132, "largest first: {u}");
        assert_eq!(u["by_boot"][1]["files"], 2);
        assert_eq!(u["by_age"][2]["files"], 4, "7 to 30 days: {u}");
        assert_eq!(u["large"], json!([]));
        // On Linux a `\` is part of a name, not a separator (CI, 2026-10-18).
        #[cfg(unix)]
        {
            t.put(r"x\y.png", 1);
            let files = scan(&t.0, now, &[], &[]);
            assert!(files.iter().any(|f| f.rel == r"x\y.png"), "{files:?}");
            assert!(!super::folders(&t.0).iter().any(|d| d == "x"));
        }
    }

    #[test]
    fn a_cleanup_takes_what_every_filter_matches_keeps_the_cited_and_routes_by_size() {
        let file = |rel: &str, bytes, age_days, boot, kind: &str, capture, cited| File {
            rel: rel.into(),
            bytes,
            age_days,
            boot,
            kind: kind.into(),
            capture,
            cited,
        };
        let files = [
            file(
                "132/l1.rdc",
                5_000_000_000,
                2.0,
                Some(132),
                "rdc",
                Some(1),
                false,
            ),
            file("s1/kf0.png", 9, 40.0, Some(130), "png", Some(2), true),
            file("s1/f1.png", 9, 40.0, Some(130), "png", Some(3), false),
            file("s1/ab.csv", 9, 40.0, Some(130), "csv", None, false),
            file("old/x.png", 9, 40.0, None, "png", None, false),
        ];
        let take = |s: &Select, max| -> Vec<(String, To)> {
            plan(&files, s, max)
                .take
                .into_iter()
                .map(|(f, to)| (f.rel, to))
                .collect()
        };
        let pngs = Select::parse(&json!({"kinds": [".PNG"], "older_than_days": 30})).unwrap();
        assert_eq!(
            take(&pngs, Some(4_000_000_000)),
            [
                ("s1/f1.png".into(), To::RecycleBin),
                ("old/x.png".into(), To::RecycleBin)
            ]
        );
        assert_eq!(
            plan(&files, &pngs, None).kept[0].rel,
            "s1/kf0.png",
            "cited: kept"
        );
        // Named by id, a cited file goes too; boot 130's files only; an rdc too big for the bin, or no bin.
        let named = Select::parse(&json!({"boots": [130], "capture_ids": [2]})).unwrap();
        assert_eq!(take(&named, Some(1)).len(), 3);
        assert!(plan(&files, &named, Some(1)).kept.is_empty());
        let rdc = Select::parse(&json!({"boots": [132]})).unwrap();
        assert_eq!(
            take(&rdc, Some(4_000_000_000)),
            [("132/l1.rdc".into(), To::ToDelete)]
        );
        let all = Select::parse(&json!({"older_than_days": 0})).unwrap();
        assert!(take(&all, None).iter().all(|(_, to)| *to == To::ToDelete));
        let summary = plan(&files, &all, None).summary();
        assert_eq!(
            (
                summary["files"].as_u64(),
                summary["to_delete"]["files"].as_u64()
            ),
            (Some(4), Some(4))
        );
        // Nothing without a filter or a name; missing alone takes no file; bad shapes refused.
        assert!(Select::parse(&json!({})).unwrap().is_empty());
        assert!(take(&Select::default(), Some(1)).is_empty());
        let missing = Select::parse(&json!({"missing": true})).unwrap();
        assert!(missing.missing && !missing.is_empty() && take(&missing, Some(1)).is_empty());
        for bad in [
            json!({"boots": ["x"]}),
            json!({"older_than_days": -1}),
            json!({"kinds": "png"}),
            json!({"missing": 1}),
        ] {
            assert!(Select::parse(&bad).is_err(), "{bad}");
        }
    }

    /// Feature tests 2c: the folders a cleanup empties, which capture_usage shows before and capture_clean removes.
    #[test]
    fn a_cleanup_empties_the_folders_it_leaves_with_no_file() {
        let file = |rel: &str| File {
            rel: rel.into(),
            bytes: 1,
            age_days: 0.0,
            boot: None,
            kind: "png".into(),
            capture: None,
            cited: false,
        };
        let files = [
            file("1/a/x.png"),
            file("1/b.png"),
            file("s1/kf0.png"),
            file("other/y.png"),
        ];
        let dirs = [
            "1", "1/a", "1/a/deep", "1/c", "s1", "s1/l3c", "other", "empty",
        ]
        .map(String::from);
        let taking = |n: usize| Plan {
            take: files[..n]
                .iter()
                .map(|f| (f.clone(), To::RecycleBin))
                .collect(),
            kept: Vec::new(),
        };
        // 1/a held the taken file: it goes with its empty subfolder; 1 still holds b.png; no unrelated folder goes.
        assert_eq!(emptied(&dirs, &files, &taking(1), &[]), ["1/a/deep", "1/a"]);
        // A selected boot's folder keeps kf0.png; its empty l3c goes.
        assert_eq!(
            emptied(&dirs, &files, &taking(1), &["s1"]),
            ["1/a/deep", "s1/l3c", "1/a"]
        );
        // Both of 1's files taken: 1 goes, with every folder in it.
        assert_eq!(
            emptied(&dirs, &files, &taking(2), &[]),
            ["1/a/deep", "1/a", "1/c", "1"]
        );
    }
}
