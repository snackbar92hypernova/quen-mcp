//! Crash injection for the session journal (docs/intent.md, "The session journal"). A scripted session (back up 4
//! files, patch 2, park 3 mods, place 2 files) is cut at every byte of every step's lines; each replay must give the
//! user's files back exactly, or report every difference it leaves. All files are synthetic.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::path::{Path, PathBuf};

use quen_core::journal::{Action, Fs, Journal, MemFs, ReplayReport, replay, sha256_hex};
use quen_core::park::{self, Entry, Listing};
use quen_core::settings;

/// quen's own data root: the journal, backups and staged files, none of them the user's.
const DATA: &str = "QUEN";
const JOURNAL: &str = "QUEN/state/journal.jsonl";
const SETTINGS: &str = "DOCS/dx12user.settings";
const PRESET: &str = "GAME/bin/x64/ReShadePreset.ini";
const PARKED_GAMMA: &str = "GAME/quen-parked/Mods/modGamma";
const PLACED_A: &str = "GAME/bin/x64/quen_pack_a.asi";

enum Op {
    Backup(PathBuf, PathBuf),
    Patch(PathBuf, Vec<u8>),
    Move(PathBuf, PathBuf),
    /// Stage the bytes, then rename the staged file to the destination.
    Place(PathBuf, PathBuf, &'static str),
}

fn backup_of(path: &str) -> PathBuf {
    Path::new(DATA)
        .join("backup")
        .join(Path::new(path).file_name().unwrap())
}

/// The user's files before the session: 4 config files, 3 mods (one with a non-ASCII name, so cuts split a UTF-8
/// character), and a bin file nobody touches.
fn user_state() -> MemFs {
    let fs = MemFs::default();
    fs.put(
        "GAME/bin/x64/ReShade.ini",
        "[GENERAL]\nPresetPath=.\\ReShadePreset.ini\n",
    );
    fs.put(PRESET, "Techniques=SyntheticA,SyntheticB\n");
    fs.put(
        SETTINGS,
        "[Rendering]\r\nMotionBlur=true\r\n[Gameplay]\r\nAutoRotate=true\r\n",
    );
    fs.put("DOCS/general.ini", "[General]\nDBGConsoleOn=false\n");
    fs.put("GAME/Mods/modAlpha", "alpha tree");
    fs.put("GAME/Mods/modBeta", "beta tree");
    fs.put("GAME/Mods/modGamma", "gamma tree");
    fs.put("GAME/bin/x64/untouched.dll", "not ours");
    fs
}

/// The scripted session over [`user_state`]: back up 4 files, patch 2, park 3 mods, place 2 files.
fn session() -> (MemFs, Vec<Op>) {
    let fs = user_state();
    let configs = [
        "GAME/bin/x64/ReShade.ini",
        PRESET,
        SETTINGS,
        "DOCS/general.ini",
    ];
    let mut ops: Vec<Op> = configs
        .iter()
        .map(|c| Op::Backup(c.into(), backup_of(c)))
        .collect();
    let settings = fs.read(Path::new(SETTINGS)).unwrap();
    let settings = settings::validate(
        &settings,
        "MotionBlur=false",
        "Gameplay:AutoRotate=false;Rendering:Hdr=1",
    );
    ops.push(Op::Patch(SETTINGS.into(), settings.unwrap()));
    let preset = settings::apply_flat(&fs.read(Path::new(PRESET)).unwrap(), "Techniques=").unwrap();
    ops.push(Op::Patch(PRESET.into(), preset));
    let mods: Vec<Entry> = ["modAlpha", "modBeta", "modGamma", "modQuen"]
        .map(|name| Entry {
            name: name.into(),
            is_dir: true,
            is_link: false,
        })
        .into();
    let mods = Listing {
        dir: Path::new("GAME/Mods"),
        entries: &mods,
    };
    let bin = Listing {
        dir: Path::new("GAME/bin/x64"),
        entries: &[],
    };
    let plan = park::plan(
        "ALL",
        "",
        park::W3_NEVER_PARK,
        park::W3_PAIRS,
        mods,
        bin,
        Path::new("GAME/quen-parked"),
    )
    .unwrap();
    ops.extend(plan.moves.into_iter().map(|m| Op::Move(m.from, m.to)));
    ops.push(Op::Place(
        Path::new(DATA).join("stage/a.asi"),
        PLACED_A.into(),
        "pack a",
    ));
    ops.push(Op::Place(
        Path::new(DATA).join("stage/b.ini"),
        "GAME/bin/x64/quen_pack_b.ini".into(),
        "pack b",
    ));
    (fs, ops)
}

/// The journal step for `op`, as the session writes it before acting (a place stages its file first).
fn action(fs: &MemFs, op: &Op) -> Action {
    match op {
        Op::Backup(src, backup) => Action::Backup {
            src: src.clone(),
            backup: backup.clone(),
            sha: fs.sha256(src).unwrap(),
        },
        Op::Patch(path, bytes) => Action::Patch {
            path: path.clone(),
            sha_before: fs.sha256(path).unwrap(),
            sha_after: sha256_hex(bytes),
            mtime: Some(fs.mtime(path).unwrap()),
            // As a launch journals dx12user.settings: its forced keys and the game's version stamp (finding 35).
            ignore: match path == Path::new(SETTINGS) {
                true => vec!["AutoRotate".into(), "LastGameVersion".into()],
                false => Vec::new(),
            },
        },
        Op::Move(from, to) => Action::Move {
            from: from.clone(),
            to: to.clone(),
            sha: fs.sha256(from).unwrap(),
            file_id: fs.file_id(from).unwrap(),
        },
        Op::Place(staged, dest, bytes) => {
            fs.write_atomic(staged, bytes.as_bytes()).unwrap();
            Action::Place {
                dest: dest.clone(),
                sha: sha256_hex(bytes.as_bytes()),
                file_id: fs.file_id(staged).unwrap(),
            }
        }
    }
}

fn perform(fs: &MemFs, op: &Op) {
    match op {
        Op::Backup(src, backup) => fs.write_atomic(backup, &fs.read(src).unwrap()),
        Op::Patch(path, bytes) => fs.write_atomic(path, bytes),
        Op::Move(from, to) => fs.rename(from, to),
        Op::Place(staged, dest, _) => fs.rename(staged, dest),
    }
    .unwrap()
}

/// A session stopped at step `k` (0-based): steps before it ran in full; step `k` has its Step and Done lines in the
/// journal, and its action ran only if `perform`.
struct Run {
    fs: MemFs,
    journal: Vec<u8>,
    step: Range<usize>,
    done: Range<usize>,
}

fn run(make: fn() -> (MemFs, Vec<Op>), k: usize, perform_k: bool) -> Run {
    let (fs, ops) = make();
    let path = Path::new(JOURNAL);
    let mut journal = Journal::create(&fs, path).unwrap();
    let len = || fs.read(path).map_or(0, |j| j.len());
    for op in &ops[..k] {
        let id = journal.step(action(&fs, op)).unwrap();
        perform(&fs, op);
        journal.done(id).unwrap();
    }
    let start = len();
    let id = journal.step(action(&fs, &ops[k])).unwrap();
    let mid = len();
    if perform_k {
        perform(&fs, &ops[k]);
    }
    journal.done(id).unwrap();
    let journal = fs.read(path).unwrap();
    Run {
        step: start..mid,
        done: mid..journal.len(),
        journal,
        fs,
    }
}

/// The user's files: everything outside quen's data root.
fn user_files(fs: &MemFs) -> BTreeMap<PathBuf, Vec<u8>> {
    fs.files()
        .into_iter()
        .filter(|(path, _)| !path.starts_with(DATA))
        .collect()
}

fn backups(fs: &MemFs) -> Vec<PathBuf> {
    fs.files()
        .into_keys()
        .filter(|path| path.starts_with(Path::new(DATA).join("backup")))
        .collect()
}

/// Cuts the journal of `make`'s session at every byte of every step's lines, crashing before or after the action as
/// the cut allows, and checks that each replay restores the user's files exactly. Returns the number of replays.
fn crash_everywhere(make: fn() -> (MemFs, Vec<Op>)) -> usize {
    let (fs, ops) = make();
    let original = user_files(&fs);
    let mut replays = 0;
    for k in 0..ops.len() {
        for perform_k in [false, true] {
            let run = run(make, k, perform_k);
            // Not performed: the crash came while the Step line was written, or just after.
            // Performed: just after the action, or while its Done line was written.
            let cuts = if perform_k {
                run.step.end..=run.done.end
            } else {
                run.step.start..=run.step.end
            };
            for cut in cuts {
                let fs = run.fs.clone();
                let report = replay(&run.journal[..cut], &fs);
                let at = format!(
                    "step {} of {}, performed {perform_k}, cut at byte {cut}",
                    k + 1,
                    ops.len()
                );
                assert_eq!(user_files(&fs), original, "{at}: {report:#?}");
                assert!(
                    report.clean() && report.kept.is_empty(),
                    "{at}: {report:#?}"
                );
                assert_eq!(
                    backups(&fs),
                    Vec::<PathBuf>::new(),
                    "{at}: backups not recycled"
                );
                // A cut inside a line (not just before its line break) tears it.
                let torn = cut > 0
                    && run.journal[cut - 1] != b'\n'
                    && run.journal.get(cut) != Some(&b'\n');
                assert_eq!(
                    report.notes.iter().any(|n| n.contains("torn")),
                    torn,
                    "{at}: {report:#?}"
                );
                replays += 1;
            }
        }
    }
    replays
}

#[test]
fn every_crash_point_of_the_session_restores_the_user_files() {
    assert_eq!(session().1.len(), 11);
    let replays = crash_everywhere(session);
    assert!(replays > 11 * 100, "{replays} replays");
}

#[test]
fn every_crash_point_of_a_file_patched_twice_restores_it() {
    fn twice() -> (MemFs, Vec<Op>) {
        let fs = user_state();
        let once = settings::apply_flat(&fs.read(Path::new(SETTINGS)).unwrap(), "MotionBlur=false")
            .unwrap();
        let twice = settings::apply_sections(&once, "Gameplay:AutoRotate=false").unwrap();
        let ops = vec![
            Op::Backup(SETTINGS.into(), backup_of(SETTINGS)),
            Op::Patch(SETTINGS.into(), once),
            Op::Patch(SETTINGS.into(), twice),
        ];
        (fs, ops)
    }
    assert!(crash_everywhere(twice) > 3 * 100);
}

/// The whole session run, its journal complete.
fn ended() -> (MemFs, Vec<u8>) {
    let (_, ops) = session();
    let run = run(session, ops.len() - 1, true);
    (run.fs, run.journal)
}

/// Every user file that differs from `original` is named by the report: kept beside, or in a step it left.
fn assert_reported(original: &BTreeMap<PathBuf, Vec<u8>>, fs: &MemFs, report: &ReplayReport) {
    let now = user_files(fs);
    let named: BTreeSet<&Path> = report
        .kept
        .iter()
        .map(PathBuf::as_path)
        .chain(report.left.iter().flat_map(|left| match &left.action {
            Action::Backup { src, backup, .. } => vec![src.as_path(), backup.as_path()],
            Action::Patch { path, .. } => vec![path.as_path()],
            Action::Move { from, to, .. } => vec![from.as_path(), to.as_path()],
            Action::Place { dest, .. } => vec![dest.as_path()],
            Action::Rebuilt { path, .. }
            | Action::Stage { path, .. }
            | Action::NewLog { path }
            | Action::NewDir { path } => {
                vec![path.as_path()]
            }
            Action::GameMemory { .. } => vec![],
        }))
        .collect();
    for path in original.keys().chain(now.keys()) {
        if original.get(path) != now.get(path) {
            assert!(
                named.contains(path.as_path()),
                "{} differs, unreported: {report:#?}",
                path.display()
            );
        }
    }
}

#[test]
fn a_full_replay_ends_the_session() {
    let (fs, journal) = ended();
    assert_ne!(user_files(&fs), user_files(&user_state()));
    let report = replay(&journal, &fs);
    assert_eq!(user_files(&fs), user_files(&user_state()));
    assert!(
        report.clean() && report.kept.is_empty() && report.notes.is_empty(),
        "{report:#?}"
    );
    assert_eq!(report.undone.len(), 2 + 3 + 2, "{report:#?}");
    // The placed files and the four spent backups went to the Recycle Bin, nothing was deleted.
    assert_eq!(fs.recycled(), report.recycled);
    assert_eq!(report.recycled.len(), 2 + 4);
    assert!(backups(&fs).is_empty());
    // A second replay of the same journal finds nothing left to do.
    let again = replay(&journal, &fs);
    assert!(
        again.clean() && again.undone.is_empty() && again.recycled.is_empty(),
        "{again:#?}"
    );
    assert_eq!(user_files(&fs), user_files(&user_state()));
}

#[test]
fn a_config_the_game_rewrote_is_kept_beside_then_restored() {
    let (fs, journal) = ended();
    fs.put(
        SETTINGS,
        "[Rendering]\r\nMotionBlur=false\r\nWrittenByTheGame=1\r\n",
    );
    let report = replay(&journal, &fs);
    let current = PathBuf::from(format!("{SETTINGS}.quen-current"));
    assert_eq!(report.kept, std::slice::from_ref(&current));
    assert!(report.clean(), "{report:#?}");
    let files = fs.files();
    assert_eq!(
        files[&current],
        b"[Rendering]\r\nMotionBlur=false\r\nWrittenByTheGame=1\r\n"
    );
    assert_eq!(
        files[Path::new(SETTINGS)],
        user_state().read(Path::new(SETTINGS)).unwrap()
    );
    assert_reported(&user_files(&user_state()), &fs, &report);
}

/// Finding 35: a copy that differs from the user's only in the step's ignored keys isn't kept beside; a note says so.
#[test]
fn a_rewrite_only_in_ignored_keys_is_not_kept() {
    let (fs, journal) = ended();
    fs.put(
        SETTINGS,
        "[Rendering]\r\nMotionBlur=true\r\n[Gameplay]\r\nAutoRotate=false\r\nLastGameVersion=5.01\r\n",
    );
    let report = replay(&journal, &fs);
    assert!(report.kept.is_empty() && report.clean(), "{report:#?}");
    assert!(
        report
            .notes
            .iter()
            .any(|n| n.contains("only in AutoRotate, LastGameVersion: not kept")),
        "{report:#?}"
    );
    assert_eq!(user_files(&fs), user_files(&user_state()));
}

/// Finding 82: at the end the game's copy differs from the user's only in what the launch wrote (forced keys, a
/// `settings` override, VSync, an appended section the game moved) and LastGameVersion: not kept. A value the game
/// changed besides (a mod menu's) is kept.
#[test]
fn a_copy_holding_only_the_launchs_own_settings_is_not_kept() {
    use quen_core::boot::{self, Inputs};
    let user = "[Rendering]\r\nDLSSGMode=2\r\nVSync=true\r\nMotionBlur=true\r\n[Gameplay]\r\nCameraAutoRotX=true\r\n\
                CameraAutoRotY=true\r\n[Version]\r\nConfigVersion=7\r\nLastGameVersion=5.00\r\n";
    let inputs = Inputs {
        game_dir: Path::new("GAME"),
        documents: Path::new("DOCS"),
        backups: &Path::new(DATA).join("backup"),
        fg: false,
        vsync: false,
        flat: "MotionBlur=false",
        sections: "QuenLiveCheck:Probe=1",
        park: "",
        keep: "",
        never_park: park::W3_NEVER_PARK,
        pairs: park::W3_PAIRS,
        mods: &[],
        bin: &[],
        renodx: None,
        load_from_dll_main: None,
        play: false,
    };
    for (game_changed, kept) in [(false, 0), (true, 1)] {
        let fs = MemFs::default();
        fs.put(SETTINGS, user);
        fs.put("GAME/bin/config/base/general.ini", "[General]\r\nX=1\r\n");
        let changes = boot::plan(&fs, &inputs).unwrap();
        let mut journal = Journal::create(&fs, Path::new(JOURNAL)).unwrap();
        boot::apply(&fs, &mut journal, &changes, &|| false).unwrap();
        // The game rewrites it: the appended section moved to the top, its version stamp, and maybe a menu value.
        let written = String::from_utf8(fs.read(Path::new(SETTINGS)).unwrap()).unwrap();
        assert!(written.contains("[QuenLiveCheck]\r\nProbe=1"), "{written}");
        let rest = written.replace("[QuenLiveCheck]\r\nProbe=1\r\n", "");
        let mut by_game = format!("[QuenLiveCheck]\r\nProbe=1\r\n{rest}").replace("5.00", "5.01");
        if game_changed {
            by_game = by_game.replace(
                "CameraAutoRotX=false",
                "CameraAutoRotX=false\r\nMenuOption=2",
            );
        }
        fs.put(SETTINGS, by_game);
        let report = replay(&fs.read(Path::new(JOURNAL)).unwrap(), &fs);
        assert!(report.clean(), "{report:#?}");
        assert_eq!(
            report.kept.len(),
            kept,
            "game changed {game_changed}: {report:#?}"
        );
        assert_eq!(fs.read(Path::new(SETTINGS)).unwrap(), user.as_bytes());
    }
}

/// An atomic write a crash cut off leaves `<file>.quen-tmp` beside a file the journal names; the replay sends it to
/// the Recycle Bin.
#[test]
fn a_cut_off_atomic_write_leaves_no_temp_file() {
    let (fs, journal) = ended();
    let tmp = PathBuf::from(format!("{SETTINGS}.quen-tmp"));
    fs.put(&tmp, "cut off mid-write");
    let report = replay(&journal, &fs);
    assert!(report.clean(), "{report:#?}");
    assert!(
        !fs.exists(&tmp) && report.recycled.contains(&tmp),
        "{report:#?}"
    );
    assert_eq!(user_files(&fs), user_files(&user_state()));
}

/// An earlier session's `.quen-current` with other bytes once left the step, the journal and every launch
/// blocked. The live file now goes beside under the next free name, the earlier copy untouched, and the replay is
/// clean; a second replay reuses the copy that holds the same bytes.
#[test]
fn an_older_quen_current_is_never_overwritten() {
    let (fs, journal) = ended();
    fs.put(SETTINGS, "written by the game");
    fs.put(
        format!("{SETTINGS}.quen-current"),
        "kept by an earlier session",
    );
    let report = replay(&journal, &fs);
    assert!(report.clean(), "{report:#?}");
    let second = PathBuf::from(format!("{SETTINGS}.quen-current-2"));
    assert_eq!(report.kept, std::slice::from_ref(&second));
    let files = fs.files();
    assert_eq!(files[&second], b"written by the game");
    assert_eq!(
        files[&PathBuf::from(format!("{SETTINGS}.quen-current"))],
        b"kept by an earlier session"
    );
    assert_eq!(
        files[Path::new(SETTINGS)],
        user_state().read(Path::new(SETTINGS)).unwrap()
    );
    assert!(backups(&fs).is_empty());
}

#[test]
fn a_parked_mod_changed_since_is_left_parked() {
    let (fs, journal) = ended();
    fs.put(PARKED_GAMMA, "gamma tree, edited while parked");
    let report = replay(&journal, &fs);
    assert_eq!(report.left.len(), 1, "{report:#?}");
    assert!(fs.exists(Path::new(PARKED_GAMMA)) && !fs.exists(Path::new("GAME/Mods/modGamma")));
    assert!(
        fs.exists(Path::new("GAME/Mods/modAlpha")) && fs.exists(Path::new("GAME/Mods/modBeta"))
    );
    assert!(
        report
            .notes
            .iter()
            .any(|n| n.contains("not every step was undone")),
        "{report:#?}"
    );
    assert_reported(&user_files(&user_state()), &fs, &report);
}

#[test]
fn a_mod_reinstalled_while_parked_blocks_its_unpark() {
    let (fs, journal) = ended();
    fs.put("GAME/Mods/modGamma", "a fresh install");
    let report = replay(&journal, &fs);
    assert_eq!(report.left.len(), 1, "{report:#?}");
    assert_eq!(
        fs.read(Path::new("GAME/Mods/modGamma")).unwrap(),
        b"a fresh install"
    );
    assert_eq!(fs.read(Path::new(PARKED_GAMMA)).unwrap(), b"gamma tree");
    assert_reported(&user_files(&user_state()), &fs, &report);
}

#[test]
fn a_placed_file_replaced_by_an_identical_copy_is_left() {
    let (fs, journal) = ended();
    // Same bytes, but a new file: the file id no longer matches, so it isn't the file quen placed.
    fs.put(PLACED_A, fs.read(Path::new(PLACED_A)).unwrap());
    let report = replay(&journal, &fs);
    assert!(
        matches!(&report.left[..], [left] if left.why.contains("sha or file id")),
        "{report:#?}"
    );
    assert!(fs.exists(Path::new(PLACED_A)));
    assert_reported(&user_files(&user_state()), &fs, &report);
}

#[test]
fn a_backup_that_fails_its_sha_is_never_restored() {
    let (fs, journal) = ended();
    fs.put(backup_of(SETTINGS), "corrupted");
    let patched = fs.read(Path::new(SETTINGS)).unwrap();
    let report = replay(&journal, &fs);
    assert!(
        matches!(&report.left[..], [left] if left.why.contains("fails its sha")),
        "{report:#?}"
    );
    assert_eq!(fs.read(Path::new(SETTINGS)).unwrap(), patched);
    assert_eq!(
        fs.read(Path::new(PRESET)).unwrap(),
        user_state().read(Path::new(PRESET)).unwrap()
    );
    assert_reported(&user_files(&user_state()), &fs, &report);
}

#[test]
fn an_unreadable_line_is_reported_and_the_rest_undone() {
    let (fs, journal) = ended();
    let mut lines: Vec<&[u8]> = journal.split_inclusive(|&b| b == b'\n').collect();
    lines.insert(
        3,
        b"{\"rec\":\"step\",\"id\":99,\"action\":{\"op\":\"teleport\"}}\n",
    );
    let report = replay(&lines.concat(), &fs);
    assert_eq!(report.unreadable, [4]);
    assert!(!report.clean());
    assert_eq!(user_files(&fs), user_files(&user_state()));
    assert_eq!(
        backups(&fs).len(),
        4,
        "backups are kept when the journal can't be fully read"
    );
}

/// A file system that dies before its `limit`-th write (a write, rename or recycle): that call and every later one
/// fails, as if quen were killed right there, and the files stay as they were.
struct Dying<'a> {
    fs: &'a MemFs,
    writes: std::cell::Cell<usize>,
    limit: usize,
}

impl Dying<'_> {
    fn write(&self) -> std::io::Result<()> {
        let n = self.writes.get();
        self.writes.set(n + 1);
        if n >= self.limit {
            return Err(std::io::Error::other("killed"));
        }
        Ok(())
    }
}

impl Fs for Dying<'_> {
    fn read(&self, path: &Path) -> std::io::Result<Vec<u8>> {
        self.fs.read(path)
    }
    fn write_atomic(&self, path: &Path, data: &[u8]) -> std::io::Result<()> {
        self.write()?;
        self.fs.write_atomic(path, data)
    }
    fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()> {
        self.write()?;
        self.fs.rename(from, to)
    }
    fn exists(&self, path: &Path) -> bool {
        self.fs.exists(path)
    }
    fn file_id(&self, path: &Path) -> std::io::Result<String> {
        self.fs.file_id(path)
    }
    fn fsync(&self, path: &Path) -> std::io::Result<()> {
        self.fs.fsync(path)
    }
    fn recycle(&self, path: &Path) -> std::io::Result<()> {
        self.write()?;
        self.fs.recycle(path)
    }
    fn mtime(&self, path: &Path) -> std::io::Result<u64> {
        self.fs.mtime(path)
    }
    fn set_mtime(&self, path: &Path, nanos: u64) -> std::io::Result<()> {
        self.write()?;
        self.fs.set_mtime(path, nanos)
    }
}

/// The native boot (`boot::plan` + `boot::apply`) over a synthetic game, killed before each of its writes in turn
/// (journal lines included); every replay gives the user's files back exactly, its spent backups recycled.
#[test]
fn a_native_boot_killed_before_any_write_replays_back() {
    use quen_core::boot::{self, Inputs, RenoDx};
    let game = || {
        let fs = MemFs::default();
        fs.put(
            "DOCS/dx12user.settings",
            "[Rendering]\r\nDLSSGMode=2\r\nVSync=true\r\n[Gameplay]\r\nCameraAutoRotX=true\r\nCameraAutoRotY=true\r\n\
             [Version]\r\nConfigVersion=7\r\n",
        );
        fs.put("GAME/bin/config/base/general.ini", "[General]\r\nX=1\r\n");
        fs.put(
            "GAME/bin/x64_dx12/ReShade.ini",
            "[ADDON]\nLoadFromDllMain=\n[renodx]\n[renodx-preset1]\nNits=203\n",
        );
        fs.put(
            "GAME/bin/x64_dx12/ReShadePreset.ini",
            "Techniques=SyntheticA\n",
        );
        fs.put("GAME/bin/x64_dx12/Tweaks.asi", "asi");
        fs.put("GAME/bin/x64_dx12/Tweaks.ini", "the user's ini");
        fs.put("PC/W3Test.asi", "a test build");
        fs.put("PC/Tweaks.ini", "a test ini");
        fs.put("GAME/Mods/modAlpha", "alpha tree");
        fs.put("GAME/Mods/modBeta", "beta tree");
        fs.put(
            boot::compiled_scripts(Path::new("GAME")),
            "compiled scripts",
        );
        fs.put(
            "DOCS/input.settings",
            "[Exploration]\r\nIK_W=(Action=A)\r\n",
        );
        fs
    };
    let entry = |name: &str, is_dir| Entry {
        name: name.into(),
        is_dir,
        is_link: false,
    };
    let mods = [entry("modAlpha", true), entry("modBeta", true)];
    let bin = [entry("Tweaks.asi", false), entry("Tweaks.ini", false)];
    let staged = [
        PathBuf::from("PC/W3Test.asi"),
        PathBuf::from("PC/Tweaks.ini"),
    ];
    let inputs = Inputs {
        game_dir: Path::new("GAME"),
        documents: Path::new("DOCS"),
        backups: &Path::new(DATA).join("backup"),
        fg: false,
        vsync: false,
        flat: "",
        sections: "Rendering:Hdr=1",
        park: "ALL",
        keep: "",
        never_park: park::W3_NEVER_PARK,
        pairs: park::W3_PAIRS,
        mods: &mods,
        bin: &bin,
        renodx: Some(RenoDx {
            addon: "renodx.addon64",
            preset: &[("Nits", 250.0)],
        }),
        load_from_dll_main: Some(r"devkit\renodx-devkit.addon64"),
        play: false,
    };
    let original = user_files(&game());
    let mut limit = 0;
    loop {
        let fs = game();
        let mut changes = boot::plan(&fs, &inputs).unwrap();
        let stage = boot::stage(
            &fs,
            Path::new("GAME"),
            &staged,
            park::W3_NEVER_PARK,
            &bin,
            &[],
            &changes,
        );
        changes.extend(stage.unwrap());
        assert_eq!(
            changes.len(),
            3 + 2 + 4 + 1 + 5,
            "3 parked, the compiled scripts and input.settings kept, 4 files patched, steam_appid.txt placed, 2 files              staged (Tweaks.ini parked first), 2 new logs journaled"
        );
        let dying = Dying {
            fs: &fs,
            writes: Default::default(),
            limit,
        };
        let applied = Journal::create(&dying, Path::new(JOURNAL))
            .and_then(|mut journal| boot::apply(&dying, &mut journal, &changes, &|| false));
        if applied.is_ok() {
            fs.put("GAME/bin/x64_dx12/W3Test.log", "the staged mod's log");
        }
        let journal = fs.read(Path::new(JOURNAL)).unwrap_or_default();
        let report = replay(&journal, &fs);
        let at = format!("killed before write {limit}");
        assert!(
            report.clean() && report.kept.is_empty(),
            "{at}: {report:#?}"
        );
        let mut now = user_files(&fs);
        // A file staged but not yet journaled is quen's own: only its name says so.
        now.retain(|path, _| !path.to_string_lossy().ends_with(".quen-stage"));
        assert_eq!(now, original, "{at}: {report:#?}");
        assert!(backups(&fs).is_empty(), "{at}: backups not recycled");
        if applied.is_ok() {
            assert!(limit > 20, "{limit} writes");
            // The untouched compiled scripts and input.settings need nothing; the staged mod wrote one log.
            assert!(report.undone.len() == 3 + 4 + 1 + 3 + 1, "{report:#?}");
            break;
        }
        limit += 1;
    }
}
