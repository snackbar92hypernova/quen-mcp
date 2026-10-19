//! The user's saves. A snapshot of the gamesaves folder (names, sizes, write times) and what changed between two,
//! the guard ported from ini_session.py's `guard_saves`, the wait for the folder to go quiet before a kill, and its
//! copy into the data root before a launch. Only files at the folder's top level count.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::Path;
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The Witcher 3 saves the game writes by itself (and Steam's cloud file): the guard lets them change. The menu's Save
/// Game always writes a `ManualSave_*`, so letting `CheckPoint_*` pass loses no guard (ini_session.py).
pub const W3_AUTO_SAVES: &[&str] = &["AutoSave", "CheckPoint", "steam_autocloud"];

/// A folder's files by name: size and write time.
pub type Snapshot = BTreeMap<String, (u64, Option<SystemTime>)>;

/// The files in `dir` now. A missing or unreadable folder is empty.
pub fn snapshot(dir: &Path) -> Snapshot {
    let entries = fs::read_dir(dir).into_iter().flatten().flatten();
    entries
        .filter_map(|e| {
            let meta = e.metadata().ok().filter(|m| m.is_file())?;
            let name = e.file_name().to_string_lossy().into_owned();
            Some((name, (meta.len(), meta.modified().ok())))
        })
        .collect()
}

/// The names that are new, changed or gone between two snapshots, sorted.
pub fn changed(before: &Snapshot, after: &Snapshot) -> Vec<String> {
    let names: BTreeSet<&String> = before
        .keys()
        .chain(after.keys())
        .filter(|name| before.get(*name) != after.get(*name))
        .collect();
    names.into_iter().cloned().collect()
}

/// The guard: the changed names that aren't the game's own saves (names starting with one of `auto`, case as given).
/// Any of them means a key confirmed something it shouldn't have (a manual save written, the quicksave replaced), and
/// the session must end at once.
pub fn unexpected<'a>(changed: &'a [String], auto: &[&str]) -> Vec<&'a str> {
    changed
        .iter()
        .map(String::as_str)
        .filter(|name| !auto.iter().any(|prefix| name.starts_with(prefix)))
        .collect()
}

/// The Witcher 3 manual saves (the menu's Save Game, and game_save): `ManualSave_<hex>_<hex>_<hex>`, its `.sav` with a
/// `.png` and, for some, a `.json` (feature tests 3).
pub const W3_MANUAL: &str = "ManualSave_";
const W3_MANUAL_FILES: &[&str] = &[".sav", ".png", ".json"];

/// game_save's check: what changed from `before` to `after`, beside the game's own saves (`auto`), must be one new
/// manual save and nothing else: a new `<base>.sav` whose base starts with [`W3_MANUAL`], and new files of that base
/// only. Ok: those files, sorted (none: nothing changed). Err: every changed name, when anything else changed (an
/// existing file, a second save, a quicksave).
pub fn new_save(
    before: &Snapshot,
    after: &Snapshot,
    auto: &[&str],
) -> Result<Vec<String>, Vec<String>> {
    let all = changed(before, after);
    let names = unexpected(&all, auto);
    let new = |name: &str| !before.contains_key(name) && after.contains_key(name);
    let savs: Vec<&str> = names
        .iter()
        .copied()
        .filter(|n| new(n) && n.starts_with(W3_MANUAL) && n.ends_with(".sav"))
        .collect();
    let ours = |base: &str, name: &str| {
        new(name)
            && W3_MANUAL_FILES
                .iter()
                .any(|ext| name.strip_prefix(base) == Some(ext))
    };
    let owned = || names.iter().map(|n| n.to_string()).collect();
    match savs[..] {
        [] if names.is_empty() => Ok(Vec::new()),
        [sav] if names.iter().all(|n| ours(&sav[..sav.len() - 4], n)) => Ok(owned()),
        _ => Err(owned()),
    }
}

/// Waits until `dir` has stayed unchanged for `quiet`, at most `max`. The seconds waited, and whether it went quiet.
pub fn wait_quiet(dir: &Path, quiet: Duration, max: Duration) -> (f64, bool) {
    let start = Instant::now();
    let (mut last, mut since) = (snapshot(dir), Instant::now());
    loop {
        let waited = (start.elapsed().as_secs_f64() * 100.0).round() / 100.0;
        if since.elapsed() >= quiet {
            return (waited, true);
        }
        if start.elapsed() >= max {
            return (waited, false);
        }
        sleep(Duration::from_millis(100));
        let now = snapshot(dir);
        if now != last {
            (last, since) = (now, Instant::now());
        }
    }
}

/// What [`backup`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Backup {
    /// Files copied, and their bytes.
    pub copied: usize,
    pub bytes: u64,
    /// Older copies moved aside into `older/` before a newer one replaced them.
    pub kept: usize,
}

/// Copies every file of `src` that `dst` lacks, or holds with another size or write time, into `dst`, keeping its
/// write time, so `dst` mirrors `src` as it is now. A copy it replaces is first moved to
/// `dst/older/<name>.<its write time in Unix seconds>`, and files gone from `src` stay: nothing is ever lost. Unchanged
/// files aren't read, so only the first backup copies everything.
pub fn backup(src: &Path, dst: &Path) -> io::Result<Backup> {
    let mut done = Backup::default();
    let have = snapshot(dst);
    for (name, state) in snapshot(src) {
        if have.get(&name) == Some(&state) {
            continue;
        }
        let to = dst.join(&name);
        if let Some((_, old)) = have.get(&name) {
            let secs = old
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs());
            let older = dst.join("older");
            fs::create_dir_all(&older)?;
            let mut aside = older.join(format!("{name}.{secs}"));
            for n in 1.. {
                if !aside.exists() {
                    break;
                }
                aside = older.join(format!("{name}.{secs}-{n}"));
            }
            fs::rename(&to, aside)?;
            done.kept += 1;
        }
        fs::create_dir_all(dst)?;
        done.bytes += fs::copy(src.join(&name), &to)?;
        if let Some(t) = state.1 {
            fs::File::options().write(true).open(&to)?.set_modified(t)?;
        }
        done.copied += 1;
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temp folder for one test, gone afterwards (only what the test made in it).
    struct Temp(std::path::PathBuf);

    impl Temp {
        fn new(name: &str) -> Temp {
            let dir =
                std::env::temp_dir().join(format!("quen-saves-{name}-{}", std::process::id()));
            fs::create_dir_all(&dir).unwrap();
            Temp(dir)
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0); // this test's own temp folder
        }
    }

    fn touch(path: &Path, data: &str, secs: u64) {
        fs::write(path, data).unwrap();
        let t = UNIX_EPOCH + Duration::from_secs(secs);
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(t)
            .unwrap();
    }

    #[test]
    fn changes_and_the_guard() {
        let tmp = Temp::new("guard");
        let dir = &tmp.0;
        touch(&dir.join("QuickSave.sav"), "q", 100);
        touch(&dir.join("AutoSave_1.sav"), "a", 100);
        touch(&dir.join("ManualSave_1.sav"), "m", 100);
        fs::create_dir(dir.join("sub")).unwrap();
        let before = snapshot(dir);
        assert_eq!(before.len(), 3, "folders don't count");
        touch(&dir.join("AutoSave_1.sav"), "a2", 200);
        touch(&dir.join("CheckPoint_2.sav"), "c", 200);
        assert!(unexpected(&changed(&before, &snapshot(dir)), W3_AUTO_SAVES).is_empty());
        touch(&dir.join("QuickSave.sav"), "q", 300);
        fs::remove_file(dir.join("ManualSave_1.sav")).unwrap();
        let all = changed(&before, &snapshot(dir));
        assert_eq!(
            all,
            [
                "AutoSave_1.sav",
                "CheckPoint_2.sav",
                "ManualSave_1.sav",
                "QuickSave.sav"
            ]
        );
        assert_eq!(
            unexpected(&all, W3_AUTO_SAVES),
            ["ManualSave_1.sav", "QuickSave.sav"]
        );
        assert!(snapshot(&dir.join("missing")).is_empty());
    }

    /// game_save accepts one new manual save (its .sav with a .png and maybe a .json) beside the game's own saves, and
    /// nothing else: an overwrite, a quicksave, two saves or a stray file next to the new one are refused whole.
    #[test]
    fn one_new_manual_save_and_nothing_else() {
        let tmp = Temp::new("new-save");
        let dir = &tmp.0;
        touch(&dir.join("ManualSave_user.sav"), "old", 100);
        touch(&dir.join("ManualSave_user.png"), "old", 100);
        let before = snapshot(dir);
        let check = || new_save(&before, &snapshot(dir), W3_AUTO_SAVES);
        assert_eq!(check(), Ok(vec![]), "nothing yet");
        touch(&dir.join("AutoSave_9.sav"), "a", 200);
        assert_eq!(check(), Ok(vec![]), "the game's own saves pass");
        touch(&dir.join("ManualSave_new.sav"), "new", 200);
        touch(&dir.join("ManualSave_new.png"), "new", 200);
        touch(&dir.join("ManualSave_new.json"), "new", 200);
        let three = [
            "ManualSave_new.json",
            "ManualSave_new.png",
            "ManualSave_new.sav",
        ];
        assert_eq!(check(), Ok(three.map(String::from).to_vec()));
        for (bad, data) in [
            ("ManualSave_user.sav", "overwritten"),
            ("QuickSave.sav", "q"),
            ("ManualSave_menu.sav", "a second save"),
            ("ManualSave_new.sav.bak", "a stray file"),
            ("notes.txt", "a stray file"),
        ] {
            let path = dir.join(bad);
            let existed = path.exists();
            touch(&path, data, 300);
            let names = check().expect_err(bad);
            assert!(names.iter().any(|n| n == bad), "{bad}: {names:?}");
            match existed {
                true => touch(&path, "old", 100),
                false => fs::remove_file(&path).unwrap(),
            }
            assert!(check().is_ok(), "{bad} put back");
        }
        fs::remove_file(dir.join("ManualSave_new.sav")).unwrap();
        assert!(check().is_err(), "a .png without its new .sav");
        fs::remove_file(dir.join("ManualSave_user.png")).unwrap();
        touch(&dir.join("ManualSave_new.sav"), "new", 200);
        assert!(check().is_err(), "an old file gone");
    }

    #[test]
    fn backup_mirrors_and_keeps_what_it_replaces() {
        let tmp = Temp::new("backup");
        let (src, dst) = (
            tmp.0.join("gamesaves"),
            tmp.0.join("data").join("gamesaves"),
        );
        fs::create_dir_all(&src).unwrap();
        touch(&src.join("QuickSave.sav"), "q1", 100);
        touch(&src.join("ManualSave_1.sav"), "m1", 100);
        let first = backup(&src, &dst).unwrap();
        assert_eq!((first.copied, first.bytes, first.kept), (2, 4, 0));
        assert_eq!(snapshot(&dst), snapshot(&src), "sizes and write times kept");
        assert_eq!(
            backup(&src, &dst).unwrap(),
            Backup::default(),
            "nothing changed"
        );

        touch(&src.join("QuickSave.sav"), "q2!", 200);
        fs::remove_file(src.join("ManualSave_1.sav")).unwrap();
        let second = backup(&src, &dst).unwrap();
        assert_eq!((second.copied, second.kept), (1, 1));
        assert_eq!(
            fs::read_to_string(dst.join("QuickSave.sav")).unwrap(),
            "q2!"
        );
        let older = dst.join("older").join("QuickSave.sav.100");
        assert_eq!(fs::read_to_string(older).unwrap(), "q1");
        assert_eq!(
            fs::read_to_string(dst.join("ManualSave_1.sav")).unwrap(),
            "m1",
            "a save gone from gamesaves stays in the copy"
        );
    }

    #[test]
    fn waits_until_the_folder_stops_changing() {
        let tmp = Temp::new("quiet");
        let dir = tmp.0.clone();
        let writer = std::thread::spawn({
            let dir = dir.clone();
            move || {
                for i in 0..8 {
                    fs::write(dir.join("save.sav"), vec![0u8; i + 1]).unwrap();
                    sleep(Duration::from_millis(100));
                }
            }
        });
        let (secs, quiet) = wait_quiet(&dir, Duration::from_millis(400), Duration::from_secs(10));
        writer.join().unwrap();
        assert!(quiet && secs >= 0.8, "{secs} s, quiet {quiet}");
        let (_, quiet) = wait_quiet(&dir, Duration::from_secs(5), Duration::from_millis(300));
        assert!(!quiet, "returned quiet before `quiet` passed");
    }
}
