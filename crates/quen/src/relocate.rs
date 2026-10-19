//! Where quen keeps its config and data: `%USERPROFILE%\.quen`, never AppData. Claude Desktop is an MSIX app
//! package, and Windows redirects AppData writes from a package, and from every process it starts (`quen mcp`, `quen
//! serve`, shells), into `%LOCALAPPDATA%\Packages\<package>\LocalCache\{Roaming,Local}`. A client outside the package
//! then saw another config, another ledger and another game lock (measured 2026-10-18). The doctor warns while quen's
//! AppData is redirected (GetCurrentPackageFullName, or one file under a package's copy and AppData alike: the first
//! says nothing in Claude Desktop's processes) and while it finds quen's old folders; `quen_setup` `move` (and
//! `quen setup --move`) copies them
//! into `.quen` once, checks every copy by sha256, rewrites what pointed at them, and renames the old folders
//! `quen.moved-<date>`. Nothing is deleted.

use std::env;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use quen_core::journal::sha256_file;
use quen_core::ledger::Ledger;
use serde_json::{Map, Value, json};

use crate::config;
use crate::session::{self, Done};
use crate::win;

/// The folders the move looks at and writes to.
pub(crate) struct Places {
    /// `%USERPROFILE%\.quen`.
    pub(crate) home: PathBuf,
    /// `%APPDATA%` and `%LOCALAPPDATA%`.
    pub(crate) roaming: Option<PathBuf>,
    pub(crate) local: Option<PathBuf>,
    /// The app package whose private folder this process's AppData is redirected into, if any.
    pub(crate) package: Option<String>,
}

impl Places {
    pub(crate) fn here() -> Result<Places, String> {
        let roaming = env::var_os("APPDATA").map(PathBuf::from);
        let local = env::var_os("LOCALAPPDATA").map(PathBuf::from);
        let id = |p: &Path| win::file_id(p).ok();
        // GetCurrentPackageFullName says nothing for the processes Claude Desktop starts, though their AppData is
        // redirected (feature tests 2b, measured twice): the redirection itself is the test.
        let package =
            win::package().or_else(|| redirected(local.as_deref(), roaming.as_deref(), &id));
        Ok(Places {
            home: config::home().ok_or("USERPROFILE is not set")?,
            roaming,
            local,
            package,
        })
    }

    /// The default places of an older quen, `%APPDATA%\quen` and `%LOCALAPPDATA%\quen`: what its config may name.
    fn defaults(&self) -> Vec<PathBuf> {
        [&self.roaming, &self.local]
            .into_iter()
            .flatten()
            .map(|d| d.join("quen"))
            .collect()
    }

    /// quen's old folders that hold anything: each app package's private `LocalCache\{Roaming,Local}\quen`, and the
    /// defaults, but those only outside a package (inside one, their paths show the package's private copy). A folder
    /// seen under two paths (the same file id: a redirection this process can't tell) counts once.
    pub(crate) fn old(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let packages = self.local.iter().map(|l| l.join("Packages"));
        for package in packages.flat_map(fs::read_dir).flatten().flatten() {
            for kind in ["Roaming", "Local"] {
                out.push(package.path().join("LocalCache").join(kind).join("quen"));
            }
        }
        if self.package.is_none() {
            out.extend(self.defaults());
        }
        out.retain(|d| fs::read_dir(d).is_ok_and(|mut e| e.next().is_some()));
        let mut seen = std::collections::HashSet::new();
        out.retain(|d| win::file_id(d).map_or(true, |id| seen.insert(id)));
        out
    }
}

/// The app package whose private copy this process's `%LOCALAPPDATA%` (`local`) and `%APPDATA%` (`roaming`) show: a
/// name in a package's `LocalCache\Local` (or `Roaming`) with the same file id (`id`) under them. Reads only.
fn redirected(
    local: Option<&Path>,
    roaming: Option<&Path>,
    id: &dyn Fn(&Path) -> Option<String>,
) -> Option<String> {
    for package in fs::read_dir(local?.join("Packages")).ok()?.flatten() {
        for (kind, base) in [("Local", local), ("Roaming", roaming)] {
            let Some(base) = base else { continue };
            let cache = package.path().join("LocalCache").join(kind);
            // ponytail: the first 20 names of each copy; a redirected copy shows from its first.
            for entry in fs::read_dir(&cache)
                .into_iter()
                .flatten()
                .flatten()
                .take(20)
            {
                let theirs = id(&entry.path());
                if theirs.is_some() && theirs == id(&base.join(entry.file_name())) {
                    return Some(package.file_name().to_string_lossy().into_owned());
                }
            }
        }
    }
    None
}

/// The doctor's warnings: quen's AppData is redirected into an app package, or an older quen's folders are there to
/// move.
pub(crate) fn warnings(places: &Places) -> Vec<String> {
    let home = places.home.display();
    let mut out: Vec<String> = places
        .package
        .iter()
        .map(|p| {
            format!(
                "Windows redirects this quen's AppData into the app package {p}'s private folder, which other \
                 clients don't see. quen keeps its config and data in {home} instead"
            )
        })
        .collect();
    out.extend(places.old().iter().map(|old| {
        format!(
            "an older quen's files are at {}: move them into {home} once (quen_setup with move: true, then user_ok; \
             or quen setup --move --user-ok in a terminal)",
            old.display()
        )
    }));
    out
}

/// A file to copy: where from, where to, its sha256.
struct Copy {
    from: PathBuf,
    to: PathBuf,
    sha: String,
    bytes: u64,
}

/// What the move would do, after every check that needs no lock. Err: refused, nothing changed.
struct Survey {
    old: Vec<PathBuf>,
    copies: Vec<Copy>,
    /// Destinations that exist already (a log the new quen wrote): the old file goes beside, `<name>.moved`.
    beside: Vec<PathBuf>,
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out);
        } else {
            out.push(path);
        }
    }
}

fn survey(places: &Places) -> Result<Survey, String> {
    let old = places.old();
    if old.is_empty() {
        return Err(format!(
            "no older quen folders found: nothing to move into {}",
            places.home.display()
        ));
    }
    let home = &places.home;
    for dir in old.iter().chain([home]) {
        let journal = dir.join("state").join("journal.jsonl");
        if journal.exists() {
            return Err(format!(
                "{} is a session's journal: replay it first (session_recover, with the quen that wrote it), then move",
                journal.display()
            ));
        }
    }
    for taken in ["config.toml", r"state\quen.db"] {
        if home.join(taken).exists() {
            return Err(format!(
                "{} exists already: quen won't merge two configs or ledgers. Move one aside yourself first",
                home.join(taken).display()
            ));
        }
    }
    let mut copies: Vec<Copy> = Vec::new();
    let mut beside = Vec::new();
    for dir in &old {
        let mut files = Vec::new();
        walk(dir, &mut files);
        for from in files {
            let rel = from.strip_prefix(dir).unwrap_or(&from).to_owned();
            // The lock holds only who held it last; the new quen has its own.
            if rel == Path::new(r"state\game.lock") {
                continue;
            }
            let sha = sha256_file(&from).map_err(|e| format!("{}: {e}", from.display()))?;
            let bytes = fs::metadata(&from).map_or(0, |m| m.len());
            let mut to = home.join(&rel);
            if let Some(other) = copies.iter().find(|c| c.to == to) {
                if other.sha == sha {
                    continue;
                }
                return Err(format!(
                    "{} and {} differ: both would be {}. Keep one (rename the other folder), then move",
                    other.from.display(),
                    from.display(),
                    to.display()
                ));
            }
            if to.exists() {
                let mut name = to.as_os_str().to_owned();
                name.push(".moved");
                to = PathBuf::from(name);
                beside.push(to.clone());
            }
            copies.push(Copy {
                from,
                to,
                sha,
                bytes,
            });
        }
    }
    Ok(Survey {
        old,
        copies,
        beside,
    })
}

/// `quen_setup` `move`: without `user_ok` what it would do; with it, the move: refused while `busy` (this quen's
/// session or its lock), a journal is left or another quen holds a lock; then every file copied into `.quen` and checked
/// by sha256, the config's paths into the old folders rewritten, the ledger's journal paths rebased, and the old
/// folders renamed `quen.moved-<date>`.
pub(crate) fn run(places: &Places, user_ok: bool, busy: Option<&str>) -> Done {
    let survey = match survey(places) {
        Ok(survey) => survey,
        Err(why) => return why.into(),
    };
    let bytes: u64 = survey.copies.iter().map(|c| c.bytes).sum();
    let mut fields = Map::new();
    fields.insert("from".into(), json!(survey.old));
    fields.insert("to".into(), json!(places.home));
    fields.insert("files".into(), json!(survey.copies.len()));
    fields.insert("gb".into(), json!(quen_core::disk::gb(bytes)));
    fields.insert("beside".into(), json!(survey.beside));
    if !user_ok {
        let next =
            "tell the user what moves (from, to, files, gb), then call again with user_ok: true";
        fields.insert("next".into(), json!(next));
        return Done::Ok(fields);
    }
    if let Some(why) = busy {
        return why.to_string().into();
    }
    // Every lock, old and new, held through the copy: no quen starts a session meanwhile.
    let holder = json!({"pid": std::process::id(), "session": "move", "t_start": session::now()});
    let mut locks: Vec<File> = Vec::new();
    for dir in survey.old.iter().chain([&places.home]) {
        let state = dir.join("state");
        let path = state.join("game.lock");
        if dir != &places.home && !path.exists() {
            continue;
        }
        let taken = fs::create_dir_all(&state).and_then(|()| win::try_lock(&path, &holder));
        match taken {
            Ok(Some(file)) => locks.push(file),
            Ok(None) => {
                return format!(
                    "another quen holds {}: a session is live there; end it first",
                    path.display()
                )
                .into();
            }
            Err(e) => return format!("{}: {e}", path.display()).into(),
        }
    }
    for copy in &survey.copies {
        let copied = copy
            .to
            .parent()
            .map_or(Ok(()), fs::create_dir_all)
            .and_then(|()| fs::copy(&copy.from, &copy.to))
            .and_then(|_| sha256_file(&copy.to));
        match copied {
            Ok(sha) if sha == copy.sha => {}
            other => {
                let why = other.map_or_else(
                    |e| e.to_string(),
                    |sha| format!("the copy's sha256 is {sha}"),
                );
                let why = format!(
                    "{} didn't copy to {} ({why}): the old folders are untouched; the copy in {} is incomplete: move \
                     it aside, then move again",
                    copy.from.display(),
                    copy.to.display(),
                    places.home.display()
                );
                return Done::Failed(why, fields);
            }
        }
    }
    let mut prefixes: Vec<PathBuf> = survey.old.clone();
    prefixes.extend(places.defaults());
    fields.insert(
        "config_rewritten".into(),
        json!(rewrite_config(places, &prefixes)),
    );
    fields.insert(
        "ledger_journal_paths".into(),
        json!(rebase_ledger(places, &prefixes)),
    );
    drop(locks);
    let date = &session::utc(SystemTime::now())[..10];
    let (mut renamed, mut left) = (Vec::new(), Vec::new());
    for dir in &survey.old {
        let to = (1..)
            .map(|n| match n {
                1 => dir.with_file_name(format!("quen.moved-{date}")),
                n => dir.with_file_name(format!("quen.moved-{date}-{n}")),
            })
            .find(|p| !p.exists())
            .unwrap_or_default();
        match win::rename_no_replace(dir, &to) {
            Ok(()) => renamed.push(json!({"from": dir, "to": to})),
            Err(e) => left.push(json!({"path": dir, "why": format!(
                "copied and checked, but not renamed ({e}): an older quen serve may still have a file open there. \
                 Rename it to {} yourself once no quen runs",
                to.display()
            )})),
        }
    }
    fields.insert("renamed".into(), json!(renamed));
    fields.insert("left".into(), json!(left));
    let applies = "a quen serve on .quen\\config.toml takes the moved config up for every connection at its next \
                   connection or session tool call; any other quen serve still running (an older quen's, or one whose \
                   QUEN_CONFIG names another file) keeps its config until the MCP clients restart (Claude Code: /mcp)";
    fields.insert("applies".into(), json!(applies));
    Done::Ok(fields)
}

/// Rewrites the copied config's values that point into an old folder (`prefixes`) to the same place under `.quen`: the
/// keys rewritten, or the error.
fn rewrite_config(places: &Places, prefixes: &[PathBuf]) -> Value {
    let path = places.home.join("config.toml");
    let Ok(text) = fs::read_to_string(&path) else {
        return json!([]);
    };
    /// What follows the old folder `value` is in, if it is in one.
    fn under<'a>(value: &'a str, prefixes: &[PathBuf]) -> Option<&'a str> {
        prefixes.iter().find_map(|old| {
            let old = old.to_string_lossy().to_lowercase();
            let head = value.get(..old.len())?;
            let rest = value.get(old.len()..)?;
            (head.to_lowercase() == old && (rest.is_empty() || rest.starts_with('\\')))
                .then_some(rest)
        })
    }
    let home = places.home.display();
    let moved: Vec<(&str, String)> = config::pairs(&text)
        .into_iter()
        .filter_map(|(key, value)| Some((key, format!("{home}{}", under(value, prefixes)?))))
        .collect();
    if moved.is_empty() {
        return json!([]);
    }
    let values: Vec<(&str, &str)> = moved.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let new = config::with_values(&text, &values);
    match config::parse(&new, &path).and_then(|_| config::write(&path, &new)) {
        Ok(()) => json!(values.iter().map(|(k, _)| *k).collect::<Vec<_>>()),
        Err(e) => json!({"error": e}),
    }
}

/// Rebases the copied ledger's journal paths from the old folders to `.quen`: how many rows, or the error.
fn rebase_ledger(places: &Places, prefixes: &[PathBuf]) -> Value {
    let db = places.home.join("state").join("quen.db");
    if !db.exists() {
        return json!(0);
    }
    let home = places.home.to_string_lossy();
    let rebased = Ledger::open(&db).and_then(|mut ledger| {
        prefixes
            .iter()
            .map(|old| ledger.rebase_journal_paths(&old.to_string_lossy(), &home))
            .sum::<Result<usize, _>>()
    });
    rebased.map_or_else(|e| json!({"error": e.to_string()}), |n| json!(n))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A user profile of this test's own: AppData, a fake app package's private copy, and the new home.
    struct Temp(PathBuf);

    impl Temp {
        fn new(name: &str) -> Temp {
            let dir = env::temp_dir().join(format!("quen-relocate-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir); // this test's own temp folder, from an earlier run
            Temp(dir)
        }

        fn put(&self, rel: &str, text: &str) -> PathBuf {
            let path = self.0.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, text).unwrap();
            path
        }

        fn places(&self, package: Option<&str>) -> Places {
            Places {
                home: self.0.join(r"profile\.quen"),
                roaming: Some(self.0.join(r"profile\AppData\Roaming")),
                local: Some(self.0.join(r"profile\AppData\Local")),
                package: package.map(String::from),
            }
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0); // this test's own temp folder
        }
    }

    const PKG: &str = r"profile\AppData\Local\Packages\Claude_test\LocalCache";

    /// The old layout as Claude Desktop left it: the config in the package's private Roaming copy, the data in its
    /// Local copy, and the deny-list written by hand into the real AppData.
    fn old_layout(t: &Temp) {
        let local = t.0.join(r"profile\AppData\Local\quen");
        t.put(
            &format!(r"{PKG}\Roaming\quen\config.toml"),
            &format!(
                "data_root = '{}'\ngame_exe = 'witcher3.exe'\n",
                local.display()
            ),
        );
        t.put(r"profile\AppData\Roaming\quen\deny.txt", "# synthetic\n");
        t.put(&format!(r"{PKG}\Local\quen\logs\serve.log"), "old log\n");
        t.put(&format!(r"{PKG}\Local\quen\captures\1\shot.png"), "png");
        let db = t.0.join(format!(r"{PKG}\Local\quen\state\quen.db"));
        fs::create_dir_all(db.parent().unwrap()).unwrap();
        let mut ledger = Ledger::open(&db).unwrap();
        let game = quen_core::ledger::Game {
            name: "synthetic".into(),
            exe_path: "game.exe".into(),
            exe_sha256: "0".repeat(64),
            build_stamp: None,
        };
        ledger.add_game(&game).unwrap();
        let journal = local.join(r"logs\1-abc\journal.jsonl");
        let boot = quen_core::ledger::BootStart {
            game_id: 1,
            t_start: "2026-10-17T13:15:00Z".into(),
            journal_path: Some(journal.display().to_string()),
            ..Default::default()
        };
        ledger.start_boot(&boot).unwrap();
    }

    #[test]
    fn the_doctor_finds_a_package_copy_and_the_old_defaults() {
        let t = Temp::new("find");
        old_layout(&t);
        let outside = t.places(None);
        let found = outside.old();
        assert_eq!(found.len(), 3, "{found:?}");
        assert!(found.iter().any(|p| p.ends_with(r"LocalCache\Local\quen")));
        // Inside a package, AppData shows the package's copy: only the real package paths count.
        let inside = t.places(Some("Claude_test"));
        assert_eq!(inside.old().len(), 2);
        let warned = warnings(&inside);
        assert!(warned[0].contains("app package Claude_test"), "{warned:?}");
        assert!(warned[1].contains("quen_setup with move"), "{warned:?}");
    }

    /// Feature tests 2b: GetCurrentPackageFullName said nothing for a process Claude Desktop started, though its AppData
    /// was redirected. A name in a package's private copy with the same file id under %LOCALAPPDATA% shows the
    /// redirection; distinct files don't.
    #[test]
    fn a_redirection_shows_as_one_file_under_two_names() {
        let t = Temp::new("redirect");
        let local = t.0.join(r"profile\AppData\Local");
        let cached = t.put(&format!(r"{PKG}\Local\Synthetic_a.txt"), "copy");
        t.put(r"profile\AppData\Local\Synthetic_a.txt", "other");
        let real = |p: &Path| win::file_id(p).ok();
        assert_eq!(redirected(Some(&local), None, &real), None, "two files");
        let seen = local.join("Synthetic_a.txt");
        let same = |p: &Path| (p == cached || p == seen).then(|| "1".to_string());
        assert_eq!(
            redirected(Some(&local), None, &same).as_deref(),
            Some("Claude_test")
        );
    }

    #[test]
    fn the_move_refuses_with_a_live_lock_then_copies_checks_rewrites_and_renames() {
        let t = Temp::new("move");
        old_layout(&t);
        let places = t.places(None);
        // A plan first: nothing changes.
        let Done::Ok(plan) = run(&places, false, None) else {
            panic!("no plan")
        };
        // config.toml, deny.txt, serve.log, shot.png, quen.db (and its WAL files, if SQLite left them).
        assert!(plan["files"].as_u64().unwrap() >= 5, "{plan:?}");
        assert!(!places.home.exists());
        // Another quen holds the old data's lock: refused, nothing copied.
        let lock = t.0.join(format!(r"{PKG}\Local\quen\state\game.lock"));
        let held = win::try_lock(&lock, &json!({"pid": 1})).unwrap().unwrap();
        let Done::Refused(why) = run(&places, true, None) else {
            panic!("not refused")
        };
        assert!(why.contains("another quen holds"), "{why}");
        assert!(!places.home.join("config.toml").exists());
        drop(held);
        let Done::Refused(why) = run(&places, true, Some("a session is live")) else {
            panic!("not refused")
        };
        assert_eq!(why, "a session is live");

        // The new quen already wrote its own log.
        t.put(r"profile\.quen\logs\serve.log", "new log\n");
        let Done::Ok(moved) = run(&places, true, None) else {
            panic!("not moved")
        };
        let home = &places.home;
        assert_eq!(
            fs::read_to_string(home.join(r"logs\serve.log")).unwrap(),
            "new log\n"
        );
        assert_eq!(
            fs::read_to_string(home.join(r"logs\serve.log.moved")).unwrap(),
            "old log\n"
        );
        assert!(home.join("deny.txt").exists() && home.join(r"captures\1\shot.png").exists());
        let config = fs::read_to_string(home.join("config.toml")).unwrap();
        assert_eq!(
            config,
            format!(
                "data_root = '{}'\ngame_exe = 'witcher3.exe'\n",
                home.display()
            )
        );
        assert_eq!(moved["config_rewritten"], json!(["data_root"]), "{moved:?}");
        assert_eq!(moved["ledger_journal_paths"], 1, "{moved:?}");
        let ledger = Ledger::open_readonly(home.join(r"state\quen.db")).unwrap();
        let rows = ledger
            .query(
                "SELECT journal_path FROM boots",
                &[],
                std::time::Duration::from_secs(5),
            )
            .unwrap()
            .rows;
        assert_eq!(rows[0][0], json!(home.join(r"logs\1-abc\journal.jsonl")));
        assert_eq!(moved["renamed"].as_array().unwrap().len(), 3, "{moved:?}");
        assert!(places.old().is_empty(), "nothing left to warn about");
        let date = &session::utc(SystemTime::now())[..10];
        let kept = t.0.join(format!(
            r"{PKG}\Local\quen.moved-{date}\captures\1\shot.png"
        ));
        assert!(kept.exists(), "the old files stay, renamed");
        // .quen has a ledger now: a second move is refused.
        t.put(&format!(r"{PKG}\Local\quen\x.txt"), "x");
        let Done::Refused(why) = run(&places, true, None) else {
            panic!("not refused")
        };
        assert!(why.contains("won't merge"), "{why}");
    }
}
