//! The Witcher 3 boot's file changes, ported from the harness's `ini_session.py boot`, and [`apply`], which makes them
//! through the session journal. [`plan`] reads the user's files and returns every change, or refuses
//! with nothing changed; [`apply`] puts each step on disk before it acts, so a replay undoes whatever part of it ran.
//!
//! The patches are byte-identical to the Python's (its `patch_bytes`, `apply_settings`, `console_on` and ini_writer
//! edits), with two deliberate differences: the console is turned on for every boot, because the save loads through
//! modQuen's console line (the Python turned it on only while mods were parked), and parked mods go to
//! `<game>\quen-parked\{Mods,bin}`, so the harness's own parking folder never holds quen's.

use std::io;
use std::path::{Path, PathBuf};

use crate::journal::{Action, Fs, Journal, sha256_hex};
use crate::park::{self, Entry, Listing};
use crate::reshade::{self, Value};
use crate::settings;

/// The parking folder in the game folder: parked Mods folders go to its `Mods\`, bin files to its `bin\`.
pub const PARKED: &str = "quen-parked";
/// The game's compiled scripts, `content\content0\x64.final.redscripts`. Witcher 3 recompiles them at launch when the
/// script mods change, so parking a mod rewrites this file (first experiment boot, 2026-10-17); every byte and its
/// write time must come back.
pub fn compiled_scripts(game_dir: &Path) -> PathBuf {
    game_dir
        .join("content")
        .join("content0")
        .join("x64.final.redscripts")
}
/// Witcher 3's Steam app id. `steam_appid.txt` with it beside the exe keeps a direct launch from being relaunched by
/// Steam, outside quen's job.
pub const STEAM_APPID: &str = "292030";
/// The `ConfigVersion` of 5.01's dx12user.settings (4.04 writes 5).
const CONFIG_VERSION: &[u8] = b"7";

/// What a boot changes, and where the files are.
pub struct Inputs<'a> {
    /// Holds `bin\x64_dx12` (the exe, ASIs, ReShade), `Mods` and `bin\config\base\general.ini`.
    pub game_dir: &'a Path,
    /// The game's documents folder: dx12user.settings.
    pub documents: &'a Path,
    /// Where the backups go: quen's own folder, never the user's.
    pub backups: &'a Path,
    /// Keep frame generation as the user set it (else `DLSSGMode=0`).
    pub fg: bool,
    /// Leave VSync as the user set it (else `VSync=false`).
    pub vsync: bool,
    /// dx12user.settings overrides: [`settings::apply_flat`] and [`settings::apply_sections`] specs.
    pub flat: &'a str,
    pub sections: &'a str,
    /// What to park ([`park::plan`]), from listings of `Mods` and `bin\x64_dx12` the caller read.
    pub park: &'a str,
    pub keep: &'a str,
    pub never_park: &'a [&'a str],
    pub pairs: &'a [[&'a str; 2]],
    pub mods: &'a [Entry],
    pub bin: &'a [Entry],
    pub renodx: Option<RenoDx<'a>>,
    /// ReShade.ini `[ADDON] LoadFromDllMain`: the devkit's path to load it, `""` not to; `None` leaves it as it is.
    /// Independent of `renodx` (feature tests 2b: with the devkit alone, nothing loaded it).
    pub load_from_dll_main: Option<&'a str>,
    /// A play session: camera auto-rotate stays as the user set it, and input.settings and
    /// dx12user.settings.bak are kept only when dx12user.settings changes. Only what was asked changes.
    pub play: bool,
}

/// The RenoDX switches, for a game with ReShade and the RenoDX add-on: ReShade.ini and ReShadePreset.ini change.
#[derive(Clone, Copy)]
pub struct RenoDx<'a> {
    /// The add-on's file name in bin: its ReShade.ini sections are its stem and `<stem>-preset1`.
    pub addon: &'a str,
    /// Preset-1 values in order, `Key` (must exist) or `+Key` (added when missing), as the harness's boot args.
    pub preset: &'a [(&'a str, f64)],
}

/// One change of a boot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// Back `path` up to `backup`, then write `bytes` over it. `sha` is of the bytes the plan read: a file that has
    /// changed since is refused. `ignore`: the journal step's keys whose lines don't count at the replay
    /// ([`Action::Patch`]).
    Rewrite {
        path: PathBuf,
        sha: String,
        bytes: Vec<u8>,
        backup: PathBuf,
        ignore: Vec<String>,
    },
    /// Rename `from` to `to`: a mod parked. `to`'s folder must exist.
    Move { from: PathBuf, to: PathBuf },
    /// Write `bytes` to `stage`, then rename it to `dest`, where nothing is. `stage` is on `dest`'s volume. `from`: the
    /// file on this PC the bytes are of, for a staged file ([`stage`]).
    Place {
        stage: PathBuf,
        dest: PathBuf,
        bytes: Vec<u8>,
        from: Option<PathBuf>,
    },
    /// A file where none is (a staged mod's log; a file wcc_lite writes): the replay recycles whatever was written there.
    NewLog { path: PathBuf },
    /// Copy the mod folder `from` (on this PC) to `dest` in Mods, where nothing is ([`stage_mods`]): the replay
    /// recycles the copy.
    CopyDir { from: PathBuf, dest: PathBuf },
    /// Back `path` up to `backup` and record its write time: the game rewrites it during the session (its compiled
    /// scripts, once a mod is parked; input.settings, user.settings and dx12user.settings.bak, at every launch), and the
    /// replay puts both back. `keep`: a live file whose lines changed beyond their order is kept beside first
    /// (input.settings: the user may rebind a key in the game's menu meanwhile; user.settings: what a script's
    /// SaveUserSettings may write).
    Rebuilt {
        path: PathBuf,
        sha: String,
        backup: PathBuf,
        keep: bool,
    },
}

impl Change {
    /// One line for a dry run.
    pub fn summary(&self) -> String {
        match self {
            Change::Rewrite { path, backup, .. } => {
                format!("patch {} (backup {})", path.display(), backup.display())
            }
            Change::Move { from, to } => format!("park {} -> {}", from.display(), to.display()),
            Change::Place {
                dest,
                from: Some(from),
                ..
            } => format!("stage {} -> {}", from.display(), dest.display()),
            Change::Place { dest, .. } => format!("place {}", dest.display()),
            Change::NewLog { path } => format!(
                "recycle {} at the end if the game writes it (game_logs keeps a copy)",
                path.display()
            ),
            Change::CopyDir { from, dest } => {
                format!("stage {} -> {}", from.display(), dest.display())
            }
            Change::Rebuilt { path, backup, .. } => {
                format!(
                    "keep {} as it is now (backup {}): the game rewrites it",
                    path.display(),
                    backup.display()
                )
            }
        }
    }
}

/// The game's binaries folder.
pub fn bin_dir(game_dir: &Path) -> PathBuf {
    game_dir.join("bin").join("x64_dx12")
}

/// The game's menu folder, `bin\config\r4game\user_config_matrix\pc`: 5.01 holds the vanilla menus' XMLs and installed
/// mods' menu XMLs side by side there, with no filelist (one install, 2026-10-20).
pub fn menu_dir(game_dir: &Path) -> PathBuf {
    let config = game_dir.join("bin").join("config");
    config.join("r4game").join("user_config_matrix").join("pc")
}

/// Whether [`stage`] copies `file` into [`menu_dir`] (a `.xml`: a mod's menu, finding 49) rather than bin.
pub fn is_menu(file: &Path) -> bool {
    file.extension()
        .is_some_and(|x| x.eq_ignore_ascii_case("xml"))
}

/// Every change the boot makes, read from the files as they are now: dx12user.settings (frame generation off, camera
/// auto-rotate off, the overrides, VSync off if asked), the parking (and with a Mods folder parked, the compiled
/// scripts kept), input.settings, user.settings and dx12user.settings.bak kept, ReShade.ini and ReShadePreset.ini with
/// RenoDX, general.ini's console, and `steam_appid.txt` if it is missing. A file the boot leaves as it is gets no change. A play session ([`Inputs::play`]) forces no setting.
/// Refuses, with nothing changed, on any doubt: a missing file, settings that aren't 5.01's, a bad override or park
/// name, a key the patch needs.
pub fn plan(fs: &dyn Fs, inputs: &Inputs) -> Result<Vec<Change>, String> {
    let bin = bin_dir(inputs.game_dir);
    let read = |path: &Path| {
        fs.read(path)
            .map_err(|e| format!("{}: {e}", path.display()))
    };
    let mut changes = Vec::new();

    let mods_dir = inputs.game_dir.join("Mods");
    let parked = park::plan(
        inputs.park,
        inputs.keep,
        inputs.never_park,
        inputs.pairs,
        Listing {
            dir: &mods_dir,
            entries: inputs.mods,
        },
        Listing {
            dir: &bin,
            entries: inputs.bin,
        },
        &inputs.game_dir.join(PARKED),
    )
    .map_err(|e| format!("park: {e}"))?;
    let script_mods = parked
        .moves
        .iter()
        .any(|m| m.from.parent() == Some(&mods_dir));
    changes.extend(parked.moves.into_iter().map(|m| Change::Move {
        from: m.from,
        to: m.to,
    }));
    let compiled = compiled_scripts(inputs.game_dir);
    if script_mods && fs.exists(&compiled) {
        changes.push(Change::Rebuilt {
            sha: sha256_hex(&read(&compiled)?),
            backup: inputs
                .backups
                .join(compiled.file_name().unwrap_or_default()),
            path: compiled,
            keep: false,
        });
    }
    let path = inputs.documents.join("dx12user.settings");
    let data = read(&path)?;
    let marker = inputs.documents.join("w3ver.active");
    let marker = fs.exists(&marker).then(|| read(&marker)).transpose()?;
    settings_version(&data, marker.as_deref())?;
    let new = dx12_settings(&data, inputs)?;
    // The game rewrites input.settings at every launch, its lines reordered within sections (live boot 1), and
    // dx12user.settings.bak from the settings it starts with, quen's patched ones (feature tests 1): both kept like the
    // compiled scripts, so their bytes come back. A key the user rebound meanwhile is kept beside. user.settings is
    // kept the same way (finding 49), a changed value kept beside; 5.01 wrote a mod menu's values into
    // dx12user.settings, not there (boot 175, finding 53). A play session that changes no setting leaves all three to the game, as a
    // launch without quen does.
    for (name, keep) in [
        ("input.settings", true),
        ("user.settings", true),
        ("dx12user.settings.bak", false),
    ] {
        let path = inputs.documents.join(name);
        if fs.exists(&path) && (!inputs.play || new != data) {
            changes.push(Change::Rebuilt {
                sha: sha256_hex(&read(&path)?),
                backup: inputs.backups.join(name),
                path,
                keep,
            });
        }
    }

    let mut rewrite = |path: PathBuf, old: Vec<u8>, new: Vec<u8>, ignore: Vec<String>| {
        if new != old {
            let name = path.file_name().unwrap_or_default();
            let backup = inputs.backups.join(name);
            let sha = sha256_hex(&old);
            changes.push(Change::Rewrite {
                path,
                sha,
                bytes: new,
                backup,
                ignore,
            });
        }
    };

    // Finding 35: at the end the game's copy differed from the user's only in what quen wrote and the game's version
    // stamp; such a copy isn't kept beside. Finding 82: what quen wrote is the forced keys, every key the patch set
    // (the overrides, VSync) and the headers of the sections it appended.
    let ignore = launch_lines(&data, &new, inputs);
    rewrite(path, data, new, ignore);

    if inputs.renodx.is_some() || inputs.load_from_dll_main.is_some() {
        let path = bin.join("ReShade.ini");
        let data = read(&path)?;
        let new = reshade_ini(&data, inputs.renodx.as_ref(), inputs.load_from_dll_main)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        rewrite(path, data, new, Vec::new());
    }
    if inputs.renodx.is_some() {
        let path = bin.join("ReShadePreset.ini");
        let data = read(&path)?;
        let new = techniques_off(&data).map_err(|e| format!("{}: {e}", path.display()))?;
        rewrite(path, data, new, Vec::new());
    }

    let path = inputs
        .game_dir
        .join("bin")
        .join("config")
        .join("base")
        .join("general.ini");
    let data = read(&path)?;
    let new = console_on(&data).map_err(|e| format!("{}: {e}", path.display()))?;
    rewrite(path, data, new, Vec::new());

    let dest = bin.join("steam_appid.txt");
    if !fs.exists(&dest) {
        changes.push(Change::Place {
            stage: bin.join("steam_appid.txt.quen-stage"),
            dest,
            bytes: STEAM_APPID.into(),
            from: None,
        });
    }
    Ok(changes)
}

/// The changes that copy `files` (on this PC) into bin under their own names for one session, a `.xml` into the menu
/// folder ([`is_menu`]), after `earlier` (the boot's other changes): a file of that name there (`bin` and `menu` list
/// them) is parked first, unless `earlier` parks it, then the copy placed. A file staged into bin whose `<stem>.log`
/// isn't in bin has that log journaled, so the replay recycles it, after the end copied the boot's logs into game_logs.
/// Refuses, with nothing changed: a name staged twice or on `never`, a folder or link of that name where it goes, one
/// `earlier` changes otherwise, a file that can't be read.
pub fn stage(
    fs: &dyn Fs,
    game_dir: &Path,
    files: &[PathBuf],
    never: &[&str],
    bin: &[Entry],
    menu: &[Entry],
    earlier: &[Change],
) -> Result<Vec<Change>, String> {
    let same = |a: &Path, b: &Path| a.as_os_str().eq_ignore_ascii_case(b.as_os_str());
    let in_bin = |name: &str| bin.iter().find(|e| e.name.eq_ignore_ascii_case(name));
    let (mut changes, mut dests) = (Vec::new(), Vec::<PathBuf>::new());
    for file in files {
        let name = file.file_name().unwrap_or_default().to_string_lossy();
        let refused = |why: &str| Err(format!("stage {}: {why}", file.display()));
        if never.iter().any(|n| n.eq_ignore_ascii_case(&name)) {
            return refused("quen never parks that name, so nothing is staged over it");
        }
        let (dir, listed, folder, parking) = match is_menu(file) {
            true => (menu_dir(game_dir), menu, "the menu folder", "menu"),
            false => (bin_dir(game_dir), bin, "bin", "bin"),
        };
        let found = listed.iter().find(|e| e.name.eq_ignore_ascii_case(&name));
        if found.is_some_and(|e| e.is_dir || e.is_link) {
            return refused(&format!("a folder or link of that name is in {folder}"));
        }
        // The name as the folder spells it, so the original comes back as it was.
        let dest = dir.join(found.map_or(&*name, |e| e.name.as_str()));
        if dests.iter().any(|d| same(d, &dest)) {
            return refused("a second file of that name");
        }
        // A park of it is no conflict: then only the copy is placed.
        let parked = |c: &Change| matches!(c, Change::Move { from, .. } if same(from, &dest));
        if let Some(c) = earlier
            .iter()
            .find(|c| !parked(c) && stage_touches(c, &dest))
        {
            return refused(&format!("the launch changes it already ({})", c.summary()));
        }
        let bytes = fs
            .read(file)
            .map_err(|e| format!("stage {}: {e}", file.display()))?;
        if let Some(e) = found.filter(|_| !earlier.iter().any(parked)) {
            changes.push(Change::Move {
                from: dest.clone(),
                to: game_dir.join(PARKED).join(parking).join(&e.name),
            });
        }
        let mut stage = dest.clone().into_os_string();
        stage.push(".quen-stage");
        changes.push(Change::Place {
            stage: stage.into(),
            dest: dest.clone(),
            bytes,
            from: Some(file.clone()),
        });
        dests.push(dest);
    }
    for dest in dests.iter().filter(|d| !is_menu(d)) {
        let log = dest.with_extension("log");
        let name = log.file_name().unwrap_or_default().to_string_lossy();
        let listed = |c: &Change| matches!(c, Change::NewLog { path } if same(path, &log));
        if in_bin(&name).is_none()
            && !dests.iter().any(|d| same(d, &log))
            && !changes.iter().any(listed)
        {
            changes.push(Change::NewLog { path: log });
        }
    }
    Ok(changes)
}

/// The changes that copy the mod folders `dirs` (on this PC) into Mods under their own names for one session, after
/// `earlier` (finding 38): a mod of that name (`mods` lists Mods) is parked first, unless `earlier` parks it, then the
/// folder copied. The game's compiled scripts are kept as a parked script mod keeps them, unless `earlier` keeps them:
/// a staged mod may hold scripts, and the end puts back the cache the launch found. Refuses, with nothing changed: a
/// name staged twice or on `never`, a file or link of that name in Mods, one `earlier` changes otherwise.
pub fn stage_mods(
    fs: &dyn Fs,
    game_dir: &Path,
    dirs: &[PathBuf],
    never: &[&str],
    mods: &[Entry],
    backups: &Path,
    earlier: &[Change],
) -> Result<Vec<Change>, String> {
    let mods_dir = game_dir.join("Mods");
    let same = |a: &Path, b: &Path| a.as_os_str().eq_ignore_ascii_case(b.as_os_str());
    let mut changes = Vec::new();
    let mut dests = Vec::<PathBuf>::new();
    for dir in dirs {
        let name = dir.file_name().unwrap_or_default().to_string_lossy();
        let refused = |why: &str| Err(format!("stage_mods {}: {why}", dir.display()));
        if never.iter().any(|n| n.eq_ignore_ascii_case(&name)) {
            return refused("quen never parks that name, so nothing is staged over it");
        }
        let found = mods.iter().find(|e| e.name.eq_ignore_ascii_case(&name));
        if found.is_some_and(|e| !e.is_dir || e.is_link) {
            return refused("a file or link of that name is in Mods");
        }
        // The name as Mods spells it, so the original comes back as it was.
        let dest = mods_dir.join(found.map_or(&*name, |e| e.name.as_str()));
        if dests.iter().any(|d| same(d, &dest)) {
            return refused("a second folder of that name");
        }
        let parked = |c: &Change| matches!(c, Change::Move { from, .. } if same(from, &dest));
        if let Some(c) = earlier
            .iter()
            .find(|c| !parked(c) && stage_touches(c, &dest))
        {
            return refused(&format!("the launch changes it already ({})", c.summary()));
        }
        if let Some(e) = found.filter(|_| !earlier.iter().any(parked)) {
            changes.push(Change::Move {
                from: dest.clone(),
                to: game_dir.join(PARKED).join("Mods").join(&e.name),
            });
        }
        changes.push(Change::CopyDir {
            from: dir.clone(),
            dest: dest.clone(),
        });
        dests.push(dest);
    }
    let compiled = compiled_scripts(game_dir);
    let kept = |c: &Change| matches!(c, Change::Rebuilt { path, .. } if same(path, &compiled));
    if !dirs.is_empty() && fs.exists(&compiled) && !earlier.iter().any(kept) {
        let data = fs
            .read(&compiled)
            .map_err(|e| format!("{}: {e}", compiled.display()))?;
        changes.push(Change::Rebuilt {
            sha: sha256_hex(&data),
            backup: backups.join(compiled.file_name().unwrap_or_default()),
            path: compiled,
            keep: false,
        });
    }
    Ok(changes)
}

/// Whether change `c` writes, moves or keeps `path`.
fn stage_touches(c: &Change, path: &Path) -> bool {
    let same = |a: &Path| a.as_os_str().eq_ignore_ascii_case(path.as_os_str());
    match c {
        Change::Rewrite { path, .. }
        | Change::Rebuilt { path, .. }
        | Change::NewLog { path }
        | Change::Place { dest: path, .. }
        | Change::CopyDir { dest: path, .. } => same(path),
        Change::Move { from, .. } => same(from),
    }
}

/// Makes `changes` in order, each step journaled before it acts: a [`Change::Rewrite`] is a Backup step (the copy read
/// back and compared), then a Patch step (the same); a [`Change::Rebuilt`] the same Backup, then a Rebuilt step with
/// the file's write time; a move, a place or a new log is one step. `stopped` is asked before each
/// change: once it says yes, this stops with `Interrupted`. On any error the caller unwinds through the journal.
pub fn apply(
    fs: &dyn Fs,
    journal: &mut Journal,
    changes: &[Change],
    stopped: &dyn Fn() -> bool,
) -> io::Result<()> {
    let differs =
        |path: &Path| io::Error::other(format!("{} reads back different", path.display()));
    // A Backup step: the file must still be what the plan read; the copy is read back.
    let back_up = |journal: &mut Journal, path: &PathBuf, sha: &String, backup: &PathBuf| {
        let data = fs.read(path)?;
        if sha256_hex(&data) != *sha {
            let why = format!("{} changed since the boot read it", path.display());
            return Err(io::Error::other(why));
        }
        let id = journal.step(Action::Backup {
            src: path.clone(),
            backup: backup.clone(),
            sha: sha.clone(),
        })?;
        fs.write_atomic(backup, &data)?;
        if fs.read(backup)? != data {
            return Err(differs(backup));
        }
        journal.done(id)
    };
    for change in changes {
        if stopped() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "stopped"));
        }
        match change {
            Change::Rewrite {
                path,
                sha,
                bytes,
                backup,
                ignore,
            } => {
                back_up(journal, path, sha, backup)?;
                let id = journal.step(Action::Patch {
                    path: path.clone(),
                    sha_before: sha.clone(),
                    sha_after: sha256_hex(bytes),
                    mtime: Some(fs.mtime(path)?),
                    ignore: ignore.clone(),
                })?;
                fs.write_atomic(path, bytes)?;
                if fs.read(path)? != *bytes {
                    return Err(differs(path));
                }
                journal.done(id)?;
            }
            Change::Move { from, to } => {
                let id = journal.step(Action::Move {
                    from: from.clone(),
                    to: to.clone(),
                    sha: fs.sha256(from)?,
                    file_id: fs.file_id(from)?,
                })?;
                fs.rename(from, to)?;
                journal.done(id)?;
            }
            Change::Place {
                stage, dest, bytes, ..
            } => {
                if fs.exists(dest) {
                    let why = format!("{} exists: nothing is placed over a file", dest.display());
                    return Err(io::Error::new(io::ErrorKind::AlreadyExists, why));
                }
                let staged = journal.step(Action::Stage {
                    path: stage.clone(),
                    sha: sha256_hex(bytes),
                })?;
                fs.write_atomic(stage, bytes)?;
                journal.done(staged)?;
                let id = journal.step(Action::Place {
                    dest: dest.clone(),
                    sha: sha256_hex(bytes),
                    file_id: fs.file_id(stage)?,
                })?;
                fs.rename(stage, dest)?;
                journal.done(id)?;
            }
            Change::Rebuilt {
                path,
                sha,
                backup,
                keep,
            } => {
                back_up(journal, path, sha, backup)?;
                let id = journal.step(Action::Rebuilt {
                    path: path.clone(),
                    sha: sha.clone(),
                    mtime: fs.mtime(path)?,
                    keep: *keep,
                })?;
                journal.done(id)?;
            }
            Change::NewLog { path } => {
                if fs.exists(path) {
                    let why = format!("{} exists: only a new log is recycled", path.display());
                    return Err(io::Error::new(io::ErrorKind::AlreadyExists, why));
                }
                let id = journal.step(Action::NewLog { path: path.clone() })?;
                journal.done(id)?;
            }
            Change::CopyDir { from, dest } => {
                if fs.exists(dest) {
                    let why = format!("{} exists: nothing is staged over a mod", dest.display());
                    return Err(io::Error::new(io::ErrorKind::AlreadyExists, why));
                }
                let id = journal.step(Action::NewDir { path: dest.clone() })?;
                fs.copy_tree(from, dest)?;
                journal.done(id)?;
            }
        }
    }
    Ok(())
}

/// The harness's `need_501`: the live settings are 5.01's. The switch marker beside them (`w3ver.active`, written by
/// the harness's 4.04 / 5.01 switch), if any, says 501, and the first `ConfigVersion=` line says 7.
fn settings_version(settings: &[u8], marker: Option<&[u8]>) -> Result<(), String> {
    let marker = marker.map(|m| String::from_utf8_lossy(m).trim().to_owned());
    let version = line_starts(settings).find_map(|at| {
        let rest = settings[at..].strip_prefix(b"ConfigVersion=")?;
        let digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
        (digits > 0).then(|| &rest[..digits])
    });
    if marker.as_deref().is_none_or(|m| m == "501") && version == Some(CONFIG_VERSION) {
        return Ok(());
    }
    Err(format!(
        "the settings aren't 5.01's (w3ver.active {marker:?}, ConfigVersion {:?}): switch back to 5.01 first",
        version.map(String::from_utf8_lossy)
    ))
}

/// Where each line starts: 0 and after every `\n`, as a multiline `^`.
fn line_starts(data: &[u8]) -> impl Iterator<Item = usize> + '_ {
    std::iter::once(0).chain(
        data.iter()
            .enumerate()
            .filter(|(i, b)| **b == b'\n' && i + 1 < data.len())
            .map(|(i, _)| i + 1),
    )
}

fn word(b: &u8) -> bool {
    b.is_ascii_alphanumeric() || *b == b'_'
}

fn not_eol(b: &u8) -> bool {
    !matches!(b, b'\r' | b'\n')
}

/// Every line that starts with `key=` (`fold`: the key in any ASCII case) followed by at least `min` bytes of `class`:
/// the key and that run become `key=value`. The harness's `re.sub(rb"(?m)^key=CLASS+", b"key=value", data)`.
fn replace_runs(
    data: &[u8],
    key: &str,
    fold: bool,
    class: fn(&u8) -> bool,
    min: usize,
    value: &str,
) -> Vec<u8> {
    let key_eq = |w: &[u8]| {
        if fold {
            w.eq_ignore_ascii_case(key.as_bytes())
        } else {
            w == key.as_bytes()
        }
    };
    let mut out = Vec::with_capacity(data.len());
    let (mut i, mut start) = (0, true);
    while i < data.len() {
        let k = i + key.len();
        if start && data.get(i..k).is_some_and(key_eq) && data.get(k) == Some(&b'=') {
            let run = data[k + 1..].iter().take_while(|b| class(b)).count();
            if run >= min {
                out.extend(format!("{key}={value}").bytes());
                i = k + 1 + run;
                start = false;
                continue;
            }
        }
        start = data[i] == b'\n';
        out.push(data[i]);
        i += 1;
    }
    out
}

fn contains(data: &[u8], what: &str) -> bool {
    data.windows(what.len()).any(|w| w == what.as_bytes())
}

/// The boot's dx12user.settings, as the harness patches it, in its order: `DLSSGMode=0` unless `fg`,
/// `CameraAutoRotX/Y=false` unless `play`, then the overrides, then `VSync=false` unless `vsync`. Each patched key must
/// be there.
pub fn dx12_settings(data: &[u8], inputs: &Inputs) -> Result<Vec<u8>, String> {
    // Checked right after each patch, as the harness does: an override may change a patched value afterwards.
    let must = |out: &[u8], keys: &[&str]| match keys.iter().find(|m| !contains(out, m)) {
        Some(m) => Err(format!(
            "dx12user.settings: no {} line to patch (fg: true leaves frame generation alone)",
            &m[..=m.find('=').unwrap_or(0)]
        )),
        None => Ok(()),
    };
    let mut out = data.to_vec();
    if !inputs.fg {
        out = replace_runs(&out, "DLSSGMode", false, u8::is_ascii_digit, 1, "0");
    }
    let camera: &[&str] = match inputs.play {
        true => &[],
        false => &["CameraAutoRotX=false", "CameraAutoRotY=false"],
    };
    for line in camera {
        let key = &line[..line.find('=').unwrap_or(0)];
        out = replace_runs(&out, key, false, word, 1, "false");
    }
    let fg: &[&str] = if inputs.fg { &[] } else { &["DLSSGMode=0"] };
    must(&out, fg)?;
    must(&out, camera)?;
    out = settings::validate(&out, inputs.flat, inputs.sections)
        .map_err(|e| format!("settings: {e}"))?;
    if !inputs.vsync {
        out = replace_runs(&out, "VSync", false, word, 1, "false");
        must(&out, &["VSync=false"])?;
    }
    Ok(out)
}

/// What a replay ignores when it compares the game's dx12user.settings with the user's (finding 82): LastGameVersion,
/// the keys the launch forces, every key `new` sets that `old` lacks, and the header of each section `new` appended.
fn launch_lines(old: &[u8], new: &[u8], inputs: &Inputs) -> Vec<String> {
    let mut ignore = vec!["LastGameVersion"];
    if !inputs.fg {
        ignore.push("DLSSGMode");
    }
    if !inputs.play {
        ignore.extend(["CameraAutoRotX", "CameraAutoRotY"]);
    }
    if !inputs.vsync {
        ignore.push("VSync");
    }
    let mut ignore: Vec<String> = ignore.into_iter().map(String::from).collect();
    for line in changed_lines(old, new) {
        let kv = line.split_once(':').map_or(line.as_str(), |(_, kv)| kv);
        ignore.extend(kv.split_once('=').map(|(k, _)| k.trim().to_owned()));
    }
    let headers = |data: &[u8]| -> Vec<String> {
        let text = String::from_utf8_lossy(data);
        let lines = text.lines().map(str::trim);
        lines
            .filter(|l| l.starts_with('['))
            .map(String::from)
            .collect()
    };
    let before = headers(old);
    ignore.extend(headers(new).into_iter().filter(|h| !before.contains(h)));
    let mut seen = std::collections::HashSet::new();
    ignore.retain(|i| seen.insert(i.clone()));
    ignore
}

/// The `Key=Value` lines of the settings `new` that `old` lacks, each as `Section:Key=Value`: what a patch writes, for a
/// plan to show (the forced keys and the overrides; a key already as asked isn't listed).
pub fn changed_lines(old: &[u8], new: &[u8]) -> Vec<String> {
    let lines = |data: &[u8]| {
        let mut section = String::new();
        let mut out = Vec::new();
        for line in String::from_utf8_lossy(data).lines() {
            match line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                Some(name) => section = name.to_owned(),
                None if line.contains('=') => out.push(format!("{section}:{line}")),
                None => {}
            }
        }
        out
    };
    let before: std::collections::HashSet<String> = lines(old).into_iter().collect();
    lines(new)
        .into_iter()
        .filter(|l| !before.contains(l))
        .collect()
}

/// ReShadePreset.ini with every `Techniques=` line emptied (no ReShade effect, such as an HDR overlay, on the
/// swapchain). The emptied line must end in the file's line break.
pub fn techniques_off(data: &[u8]) -> Result<Vec<u8>, String> {
    let eol = if contains(data, "\r\n") { "\r\n" } else { "\n" };
    let out = replace_runs(data, "Techniques", false, not_eol, 0, "");
    if !contains(&out, &format!("Techniques={eol}")) {
        return Err("no Techniques= line to empty".into());
    }
    Ok(out)
}

/// general.ini with the developer console on: every `DBGConsoleOn=` line (any case) set to `DBGConsoleOn=true`, or,
/// without one, `DBGConsoleOn=true` added after each `[General]` header. It must end up in `[General]`.
pub fn console_on(data: &[u8]) -> Result<Vec<u8>, String> {
    const ON: &str = "DBGConsoleOn=true";
    let key = "DBGConsoleOn=";
    let has = line_starts(data).any(|at| {
        data[at..]
            .get(..key.len())
            .is_some_and(|w| w.eq_ignore_ascii_case(key.as_bytes()))
    });
    let out = if has {
        replace_runs(data, "DBGConsoleOn", true, not_eol, 0, "true")
    } else {
        let mut out = Vec::with_capacity(data.len() + ON.len() + 2);
        for line in data.split_inclusive(|&b| b == b'\n') {
            out.extend(line);
            let Some(body) = line.strip_suffix(b"\n") else {
                continue;
            };
            let (body, eol) = match body.strip_suffix(b"\r") {
                Some(body) => (body, "\r\n"),
                None => (body, "\n"),
            };
            if body
                .strip_prefix(b"[General]")
                .is_some_and(|rest| rest.iter().all(|b| matches!(b, b' ' | b'\t')))
            {
                out.extend(format!("{ON}{eol}").bytes());
            }
        }
        out
    };
    // Some `[General]` line, then, before any `[`, a line that is exactly DBGConsoleOn=true (a CR before its break).
    let in_general = line_starts(&out)
        .filter(|&at| out[at..].starts_with(b"[General]"))
        .any(|at| {
            let from = at + "[General]".len();
            let to = out[from..]
                .iter()
                .position(|&b| b == b'[')
                .map_or(out.len(), |n| from + n);
            (from + 1..=to).filter(|&j| out[j - 1] == b'\n').any(|j| {
                let line = out[j..].split(|&b| b == b'\n').next().unwrap_or_default();
                line == ON.as_bytes() || line.strip_suffix(b"\r") == Some(ON.as_bytes())
            })
        });
    if !in_general {
        return Err(format!("{ON} not in [General]"));
    }
    Ok(out)
}

/// ReShade.ini as the harness's boot writes it: `[ADDON] LoadFromDllMain` (if given), then with RenoDX
/// `DevAutoReload=1` in the add-on's section and preset 1's values, existing keys first.
fn reshade_ini(
    data: &[u8],
    renodx: Option<&RenoDx>,
    load_from_dll_main: Option<&str>,
) -> Result<Vec<u8>, String> {
    let mut out = data.to_vec();
    if let Some(path) = load_from_dll_main {
        out = reshade::set_values(
            &out,
            "ADDON",
            &[("LoadFromDllMain", Value::Text(path))],
            false,
        )?;
    }
    let Some(renodx) = renodx else {
        return Ok(out);
    };
    let stem = renodx
        .addon
        .rsplit_once('.')
        .map_or(renodx.addon, |(stem, _)| stem);
    out = reshade::set_values(&out, stem, &[("DevAutoReload", Value::Number(1.0))], true)?;
    // The harness's parse_kv: `+Key` may be added; a key named twice keeps its first place and its last value.
    let mut values: Vec<(&str, f64, bool)> = Vec::new();
    for &(key, value) in renodx.preset {
        let (key, add) = key.strip_prefix('+').map_or((key, false), |k| (k, true));
        match values.iter_mut().find(|(k, ..)| *k == key) {
            Some(entry) => *entry = (key, value, entry.2 || add),
            None => values.push((key, value, add)),
        }
    }
    let preset = format!("{stem}-preset1");
    for add in [false, true] {
        let some: Vec<(&str, Value)> = values
            .iter()
            .filter(|(.., a)| *a == add)
            .map(|&(k, v, _)| (k, Value::Number(v)))
            .collect();
        if !some.is_empty() {
            out = reshade::set_values(&out, &preset, &some, add)?;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::{MemFs, replay};

    /// A synthetic dx12user.settings, CRLF like the game's.
    const SETTINGS: &str = "[Rendering]\r\nDLSSGMode=2\r\nVSync=true\r\nMotionBlur=true\r\n[Gameplay]\r\n\
                            CameraAutoRotX=true\r\nCameraAutoRotY=true\r\n[Version]\r\nConfigVersion=7\r\n";

    fn inputs<'a>(mods: &'a [Entry], bin: &'a [Entry]) -> Inputs<'a> {
        Inputs {
            game_dir: Path::new("GAME"),
            documents: Path::new("DOCS"),
            backups: Path::new("QUEN/backup"),
            fg: false,
            vsync: true,
            flat: "",
            sections: "",
            park: "",
            keep: "",
            never_park: park::W3_NEVER_PARK,
            pairs: park::W3_PAIRS,
            mods,
            bin,
            renodx: None,
            load_from_dll_main: None,
            play: false,
        }
    }

    fn dir(name: &str) -> Entry {
        Entry {
            name: name.into(),
            is_dir: true,
            is_link: false,
        }
    }

    #[test]
    fn settings_get_fg_and_auto_rotate_off_then_overrides_then_vsync() {
        let mut inp = inputs(&[], &[]);
        let out = dx12_settings(SETTINGS.as_bytes(), &inp).unwrap();
        let want = SETTINGS
            .replace("DLSSGMode=2", "DLSSGMode=0")
            .replace("CameraAutoRotX=true", "CameraAutoRotX=false")
            .replace("CameraAutoRotY=true", "CameraAutoRotY=false");
        assert_eq!(String::from_utf8(out).unwrap(), want);
        // The overrides come after the fixed patches, VSync after the overrides: VSync=false wins over an override.
        inp.fg = true;
        inp.vsync = false;
        inp.flat = "VSync=maybe,DLSSGMode=3";
        let out = String::from_utf8(dx12_settings(SETTINGS.as_bytes(), &inp).unwrap()).unwrap();
        assert!(
            out.contains("DLSSGMode=3\r\n") && out.contains("VSync=false\r\n"),
            "{out}"
        );
        // A key the patch needs must be there.
        let no_fg = SETTINGS.replace("DLSSGMode=2\r\n", "");
        inp.fg = false;
        inp.flat = "";
        let err = dx12_settings(no_fg.as_bytes(), &inp).unwrap_err();
        assert!(err.contains("DLSSGMode"), "{err}");
    }

    /// Feature tests 1: a plan names each settings line it writes, the forced keys among them.
    #[test]
    fn a_plan_lists_the_settings_lines_it_writes() {
        let mut inp = inputs(&[], &[]);
        inp.vsync = false;
        inp.sections = "Gameplay:New=1;Rendering:MotionBlur=true";
        let new = dx12_settings(SETTINGS.as_bytes(), &inp).unwrap();
        assert_eq!(
            changed_lines(SETTINGS.as_bytes(), &new),
            [
                "Rendering:DLSSGMode=0",
                "Rendering:VSync=false",
                "Gameplay:New=1",
                "Gameplay:CameraAutoRotX=false",
                "Gameplay:CameraAutoRotY=false",
            ]
        );
    }

    /// In the first experiment boot (2026-10-17), the harness's section override read its value as a regex template, so
    /// a backslash broke it. Both kinds of override here take every value byte for byte.
    #[test]
    fn override_values_are_literal() {
        let mut inp = inputs(&[], &[]);
        inp.flat = r"MotionBlur=C:\x\1\y$0";
        inp.sections = r"Gameplay:CameraAutoRotX=a\1\b;Gameplay:New=\g<0>";
        let out = String::from_utf8(dx12_settings(SETTINGS.as_bytes(), &inp).unwrap()).unwrap();
        for line in [
            r"MotionBlur=C:\x\1\y$0",
            r"CameraAutoRotX=a\1\b",
            r"New=\g<0>",
        ] {
            assert!(out.contains(&format!("{line}\r\n")), "{line}: {out}");
        }
    }

    #[test]
    fn runs_replace_like_the_harness_regexes() {
        let data = b"A=12x\nA=\nXA=1\na=3\nA=4";
        assert_eq!(
            replace_runs(data, "A", false, u8::is_ascii_digit, 1, "0"),
            b"A=0x\nA=\nXA=1\na=3\nA=0"
        );
        assert_eq!(
            replace_runs(data, "A", true, not_eol, 0, "v"),
            b"A=v\nA=v\nXA=1\nA=v\nA=v"
        );
    }

    #[test]
    fn techniques_are_emptied() {
        let out = techniques_off(b"[S]\r\nTechniques=SyntheticA,SyntheticB\r\nX=1\r\n").unwrap();
        assert_eq!(out, b"[S]\r\nTechniques=\r\nX=1\r\n");
        assert!(
            techniques_off(b"Techniques=A").is_err(),
            "no line break after it"
        );
        assert!(techniques_off(b"X=1\n").is_err());
    }

    #[test]
    fn the_console_goes_on_in_general() {
        let on = |data: &str| console_on(data.as_bytes()).map(|b| String::from_utf8(b).unwrap());
        assert_eq!(
            on("[General]\r\nA=1\r\n").unwrap(),
            "[General]\r\nDBGConsoleOn=true\r\nA=1\r\n"
        );
        assert_eq!(
            on("[General] \nA=1\n").unwrap(),
            "[General] \nDBGConsoleOn=true\nA=1\n"
        );
        assert_eq!(
            on("[General]\ndbgconsoleon=false\n").unwrap(),
            "[General]\nDBGConsoleOn=true\n"
        );
        assert_eq!(
            on("[General]\nDBGConsoleOn=true\n").unwrap(),
            "[General]\nDBGConsoleOn=true\n"
        );
        // Elsewhere only, or no [General] at all: refused.
        assert!(on("[Other]\nDBGConsoleOn=false\n[General]\n").is_err());
        assert!(on("[Other]\nA=1\n").is_err());
        assert!(on("[General]").is_err());
    }

    #[test]
    fn the_version_must_be_5_01s() {
        assert!(settings_version(SETTINGS.as_bytes(), None).is_ok());
        assert!(settings_version(SETTINGS.as_bytes(), Some(b"501\r\n")).is_ok());
        assert!(settings_version(SETTINGS.as_bytes(), Some(b"404")).is_err());
        assert!(settings_version(b"[Version]\nConfigVersion=5\n", None).is_err());
        assert!(settings_version(b"[Version]\nConfigVersion=\n", None).is_err());
    }

    #[test]
    fn reshade_ini_gets_the_devkit_reload_and_the_preset() {
        let ini = "[ADDON]\nLoadFromDllMain=x\n[renodx]\nA=1\n[renodx-preset1]\nNits=203\n";
        let renodx = RenoDx {
            addon: "renodx.addon64",
            preset: &[("+New", 2.0), ("Nits", 250.0), ("New", 3.0)],
        };
        let out = reshade_ini(ini.as_bytes(), Some(&renodx), Some("")).unwrap();
        let want = "[ADDON]\nLoadFromDllMain=\n[renodx]\nA=1\nDevAutoReload=1\n[renodx-preset1]\nNits=250\nNew=3\n";
        assert_eq!(String::from_utf8(out).unwrap(), want);
        // A preset key without + must exist.
        let typo = RenoDx {
            preset: &[("Nitz", 1.0)],
            ..renodx
        };
        assert!(reshade_ini(ini.as_bytes(), Some(&typo), None).is_err());
    }

    /// Feature tests 2b: the devkit alone (no RenoDX add-on) gets its `LoadFromDllMain` line, journaled and replayed,
    /// and ReShadePreset.ini stays as it is.
    #[test]
    fn the_devkit_loads_without_the_renodx_add_on() {
        let fs = game();
        let before = fs.files();
        let mut inp = inputs(&[], &[]);
        inp.load_from_dll_main = Some(r"devkit\renodx-devkit.addon64");
        let changes = plan(&fs, &inp).unwrap();
        let ini = Path::new("GAME/bin/x64_dx12/ReShade.ini");
        let Some(Change::Rewrite { bytes, .. }) = changes
            .iter()
            .find(|c| matches!(c, Change::Rewrite { path, .. } if path == ini))
        else {
            panic!("no ReShade.ini change: {changes:?}")
        };
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert!(
            text.contains("LoadFromDllMain=devkit\\renodx-devkit.addon64\n")
                && !text.contains("DevAutoReload"),
            "{text}"
        );
        assert!(
            !changes.iter().any(
                |c| matches!(c, Change::Rewrite { path, .. } if path.ends_with("ReShadePreset.ini"))
            ),
            "{changes:?}"
        );
        let path = Path::new("QUEN/journal.jsonl");
        let mut journal = Journal::create(&fs, path).unwrap();
        apply(&fs, &mut journal, &changes, &|| false).unwrap();
        let report = replay(&fs.read(path).unwrap(), &fs);
        assert!(report.clean(), "{report:#?}");
        let mut after = fs.files();
        after.retain(|p, _| !p.starts_with("QUEN"));
        assert_eq!(after, before);
    }

    /// A synthetic game: settings, general.ini, ReShade and two mods; `steam_appid.txt` missing.
    fn game() -> MemFs {
        let fs = MemFs::default();
        fs.put("DOCS/dx12user.settings", SETTINGS);
        fs.put("GAME/bin/config/base/general.ini", "[General]\r\nX=1\r\n");
        fs.put(
            "GAME/bin/x64_dx12/ReShade.ini",
            "[ADDON]\nLoadFromDllMain=\n[renodx]\n[renodx-preset1]\nNits=203\n",
        );
        fs.put(
            "GAME/bin/x64_dx12/ReShadePreset.ini",
            "Techniques=SyntheticA\n",
        );
        fs.put("GAME/Mods/modAlpha", "alpha");
        fs.put("GAME/Mods/modQuen", "quen");
        fs
    }

    #[test]
    fn a_planned_boot_applies_and_replays_back_to_the_users_files() {
        let fs = game();
        let before = fs.files();
        let mods = [dir("modAlpha"), dir("modQuen")];
        let mut inp = inputs(&mods, &[]);
        inp.park = "ALL";
        inp.renodx = Some(RenoDx {
            addon: "renodx.addon64",
            preset: &[("Nits", 250.0)],
        });
        let changes = plan(&fs, &inp).unwrap();
        let kinds: Vec<String> = changes.iter().map(Change::summary).collect();
        assert_eq!(changes.len(), 6, "{kinds:#?}");
        assert!(
            matches!(&changes[0], Change::Move { from, .. } if from.ends_with("modAlpha")),
            "{kinds:#?}"
        );
        let path = Path::new("QUEN/journal.jsonl");
        let mut journal = Journal::create(&fs, path).unwrap();
        apply(&fs, &mut journal, &changes, &|| false).unwrap();
        assert!(fs.exists(Path::new("GAME/quen-parked/Mods/modAlpha")));
        assert_eq!(
            fs.read(Path::new("GAME/bin/x64_dx12/steam_appid.txt"))
                .unwrap(),
            b"292030"
        );
        let report = replay(&fs.read(path).unwrap(), &fs);
        assert!(report.clean() && report.kept.is_empty(), "{report:#?}");
        let after: Vec<_> = fs
            .files()
            .into_iter()
            .filter(|(p, _)| !p.starts_with("QUEN"))
            .collect();
        assert_eq!(after, before.into_iter().collect::<Vec<_>>());
        // Nothing to change: no change at all.
        let fs = game();
        fs.put("GAME/bin/x64_dx12/steam_appid.txt", STEAM_APPID);
        let done =
            String::from_utf8(dx12_settings(SETTINGS.as_bytes(), &inputs(&[], &[])).unwrap())
                .unwrap();
        fs.put("DOCS/dx12user.settings", &done);
        fs.put(
            "GAME/bin/config/base/general.ini",
            "[General]\nDBGConsoleOn=true\n",
        );
        assert_eq!(plan(&fs, &inputs(&[], &[])).unwrap(), []);
    }

    /// With a Mods folder parked, the game recompiles its scripts. The boot backs the compiled file up and records
    /// its write time; whatever the game wrote, the replay puts the bytes and the time back, keeping nothing beside.
    /// Parking only bin files leaves it alone.
    #[test]
    fn the_compiled_scripts_come_back_with_their_write_time() {
        let fs = game();
        let compiled = compiled_scripts(Path::new("GAME"));
        fs.put(&compiled, "compiled with every mod");
        let before = (fs.read(&compiled).unwrap(), fs.mtime(&compiled).unwrap());
        let mods = [dir("modAlpha")];
        let asi = [Entry {
            name: "Tweaks.asi".into(),
            is_dir: false,
            is_link: false,
        }];
        fs.put("GAME/bin/x64_dx12/Tweaks.asi", "asi");
        let mut inp = inputs(&mods, &asi);
        inp.park = "Tweaks.asi";
        let only_bin = plan(&fs, &inp).unwrap();
        assert!(
            !only_bin.iter().any(|c| matches!(c, Change::Rebuilt { .. })),
            "{only_bin:?}"
        );
        inp.park = "modAlpha";
        let changes = plan(&fs, &inp).unwrap();
        assert!(
            matches!(&changes[1], Change::Rebuilt { path, .. } if *path == compiled),
            "{changes:?}"
        );
        let path = Path::new("QUEN/journal.jsonl");
        let mut journal = Journal::create(&fs, path).unwrap();
        apply(&fs, &mut journal, &changes, &|| false).unwrap();
        // The game recompiles at launch: new bytes, a new write time.
        fs.put(&compiled, "compiled without modAlpha");
        assert_ne!(fs.mtime(&compiled).unwrap(), before.1);
        let report = replay(&fs.read(path).unwrap(), &fs);
        assert!(report.clean() && report.kept.is_empty(), "{report:#?}");
        assert_eq!(
            (fs.read(&compiled).unwrap(), fs.mtime(&compiled).unwrap()),
            before
        );
        assert!(
            report.undone.iter().any(|u| u.contains("rebuilt")),
            "{report:#?}"
        );
        // Only the time moved (the same recompile output): the time goes back too.
        let fs = game();
        fs.put(&compiled, "compiled with every mod");
        let mtime = fs.mtime(&compiled).unwrap();
        let mut journal = Journal::create(&fs, path).unwrap();
        apply(&fs, &mut journal, &plan(&fs, &inp).unwrap(), &|| false).unwrap();
        fs.set_mtime(&compiled, mtime + 1).unwrap();
        let report = replay(&fs.read(path).unwrap(), &fs);
        assert!(report.clean(), "{report:#?}");
        assert_eq!(fs.mtime(&compiled).unwrap(), mtime);
    }

    /// Live boot 1: the game rewrites input.settings at every launch with lines reordered; feature tests 1: and
    /// dx12user.settings.bak from quen's patched settings; finding 49: user.settings, journaled the same way.
    /// Whatever it wrote, the replay puts the user's bytes and write times back; without the files there is nothing to
    /// keep.
    #[test]
    fn input_settings_and_the_settings_bak_come_back_after_the_game_rewrites_them() {
        let fs = game();
        let input = Path::new("DOCS/input.settings");
        let user = Path::new("DOCS/user.settings");
        let bak = Path::new("DOCS/dx12user.settings.bak");
        fs.put(
            input,
            "[Exploration]\r\nIK_W=(Action=A)\r\nIK_S=(Action=B)\r\n",
        );
        fs.put(user, "[ModMenu]\r\nRadius=5\r\nOn=true\r\n");
        fs.put(bak, SETTINGS);
        let now = |p| (fs.read(p).unwrap(), fs.mtime(p).unwrap());
        let before = (now(input), now(user), now(bak));
        let changes = plan(&fs, &inputs(&[], &[])).unwrap();
        assert!(
            matches!(&changes[..3], [Change::Rebuilt { path: a, .. }, Change::Rebuilt { path: u, .. },
                Change::Rebuilt { path: b, .. }] if a == input && u == user && b == bak),
            "{changes:?}"
        );
        let path = Path::new("QUEN/journal.jsonl");
        let mut journal = Journal::create(&fs, path).unwrap();
        apply(&fs, &mut journal, &changes, &|| false).unwrap();
        fs.put(
            input,
            "[Exploration]\r\nIK_S=(Action=B)\r\nIK_W=(Action=A)\r\n",
        );
        fs.put(user, "[ModMenu]\r\nOn=true\r\nRadius=5\r\n");
        fs.put(bak, SETTINGS.replace("VSync=true", "VSync=false"));
        let report = replay(&fs.read(path).unwrap(), &fs);
        assert!(report.clean() && report.kept.is_empty(), "{report:#?}");
        assert_eq!((now(input), now(user), now(bak)), before);
        // A key the user rebound in the game's menu, and a value the game wrote into user.settings, are kept beside;
        // the .bak isn't.
        fs.recycle(path).unwrap();
        let mut journal = Journal::create(&fs, path).unwrap();
        apply(&fs, &mut journal, &changes, &|| false).unwrap();
        let rebound = "[Exploration]\r\nIK_S=(Action=B)\r\nIK_W=(Action=Jump)\r\n";
        let menu = "[ModMenu]\r\nRadius=9\r\nOn=true\r\n";
        fs.put(input, rebound);
        fs.put(user, menu);
        fs.put(bak, "rewritten by the game");
        let report = replay(&fs.read(path).unwrap(), &fs);
        let kept = PathBuf::from("DOCS/input.settings.quen-current");
        let kept_user = PathBuf::from("DOCS/user.settings.quen-current");
        assert!(report.clean(), "{report:#?}");
        assert_eq!(report.kept, [kept_user.clone(), kept.clone()]);
        assert_eq!(fs.read(&kept).unwrap(), rebound.as_bytes());
        assert_eq!(fs.read(&kept_user).unwrap(), menu.as_bytes());
        assert_eq!((now(input), now(user), now(bak)), before);
        let fs = game();
        let none = plan(&fs, &inputs(&[], &[])).unwrap();
        assert!(
            !none.iter().any(|c| matches!(c, Change::Rebuilt { .. })),
            "{none:?}"
        );
    }

    /// A play session's default plan touches only general.ini's console and steam_appid.txt; input.settings,
    /// user.settings and the .bak stay the game's. A setting the user asked for (here VSync off) patches only that line,
    /// and then all three are kept as in a test boot.
    #[test]
    fn a_play_plan_changes_only_what_was_asked() {
        let fs = game();
        fs.put(
            "DOCS/input.settings",
            "[Exploration]\r\nIK_W=(Action=A)\r\n",
        );
        fs.put("DOCS/user.settings", "[ModMenu]\r\nOn=true\r\n");
        fs.put("DOCS/dx12user.settings.bak", SETTINGS);
        let mut inp = inputs(&[], &[]);
        (inp.play, inp.fg) = (true, true);
        let changes = plan(&fs, &inp).unwrap();
        let touched: Vec<&Path> = changes
            .iter()
            .map(|c| match c {
                Change::Rewrite { path, .. }
                | Change::Rebuilt { path, .. }
                | Change::NewLog { path } => path.as_path(),
                Change::Move { from, .. } => from.as_path(),
                Change::Place { dest, .. } | Change::CopyDir { dest, .. } => dest.as_path(),
            })
            .collect();
        let want = [
            Path::new("GAME/bin/config/base/general.ini"),
            Path::new("GAME/bin/x64_dx12/steam_appid.txt"),
        ];
        assert_eq!(touched, want, "{changes:?}");
        inp.vsync = false;
        let changes = plan(&fs, &inp).unwrap();
        let settings = changes.iter().find_map(|c| match c {
            Change::Rewrite { path, bytes, .. } if path.ends_with("dx12user.settings") => {
                Some(bytes)
            }
            _ => None,
        });
        let new = settings.expect("dx12user.settings patched");
        assert_eq!(
            changed_lines(SETTINGS.as_bytes(), new),
            ["Rendering:VSync=false"]
        );
        let kept = changes
            .iter()
            .filter(|c| matches!(c, Change::Rebuilt { .. }))
            .count();
        assert_eq!(kept, 3, "{changes:?}");
    }

    fn file(name: &str, is_link: bool) -> Entry {
        Entry {
            name: name.into(),
            is_dir: false,
            is_link,
        }
    }

    /// Finding 23: test files copied into bin by hand before each boot and recycled after it. Staged, each is a placed
    /// copy; the user's file of that name (here in another case) is parked first and comes back; a log a staged mod
    /// writes where none was is recycled; a file a park moves already isn't moved twice. Finding 49: a mod's menu XML
    /// goes into the menu folder, the same way, with no log journaled.
    #[test]
    fn staged_files_land_park_what_is_there_and_replay_away() {
        let fs = game();
        let game_dir = Path::new("GAME");
        let menu_dir = menu_dir(game_dir);
        fs.put("GAME/bin/x64_dx12/w3fake.INI", "the user's ini");
        fs.put("GAME/bin/x64_dx12/Tweaks.asi", "the user's tweaks");
        fs.put(menu_dir.join("modMenu.xml"), "the user's menu");
        for (name, bytes) in [
            ("W3Test.asi", "test asi"),
            ("W3Test.ini", "test ini"),
            ("W3Fake.ini", "test fake ini"),
            ("Tweaks.asi", "test tweaks"),
            ("modMenu.XML", "test menu"),
        ] {
            fs.put(Path::new("PC").join(name), bytes);
        }
        let before = fs.files();
        let bin = [file("w3fake.INI", false), file("Tweaks.asi", false)];
        let menu = [file("modMenu.xml", false), file("audio.xml", false)];
        let mut inp = inputs(&[], &bin);
        inp.park = "Tweaks.asi";
        let mut changes = plan(&fs, &inp).unwrap();
        let files: Vec<PathBuf> = [
            "W3Test.asi",
            "W3Test.ini",
            "W3Fake.ini",
            "Tweaks.asi",
            "modMenu.XML",
        ]
        .iter()
        .map(|n| Path::new("PC").join(n))
        .collect();
        let never = park::W3_NEVER_PARK;
        let staged = stage(&fs, game_dir, &files, never, &bin, &menu, &changes).unwrap();
        let bin_dir = bin_dir(game_dir);
        let show = |p: &str| bin_dir.join(p).display().to_string();
        let pc = |p: &str| Path::new("PC").join(p).display().to_string();
        let lines: Vec<String> = staged.iter().map(Change::summary).collect();
        let parked = game_dir.join(PARKED).join("bin").join("w3fake.INI");
        let parked_menu = game_dir.join(PARKED).join("menu").join("modMenu.xml");
        let user_menu = menu_dir.join("modMenu.xml");
        assert_eq!(
            lines,
            [
                format!("stage {} -> {}", pc("W3Test.asi"), show("W3Test.asi")),
                format!("stage {} -> {}", pc("W3Test.ini"), show("W3Test.ini")),
                format!("park {} -> {}", show("w3fake.INI"), parked.display()),
                format!("stage {} -> {}", pc("W3Fake.ini"), show("w3fake.INI")),
                // The park moves Tweaks.asi already: only the copy.
                format!("stage {} -> {}", pc("Tweaks.asi"), show("Tweaks.asi")),
                format!("park {} -> {}", user_menu.display(), parked_menu.display()),
                format!("stage {} -> {}", pc("modMenu.XML"), user_menu.display()),
                format!(
                    "recycle {} at the end if the game writes it (game_logs keeps a copy)",
                    show("W3Test.log")
                ),
                format!(
                    "recycle {} at the end if the game writes it (game_logs keeps a copy)",
                    show("w3fake.log")
                ),
                format!(
                    "recycle {} at the end if the game writes it (game_logs keeps a copy)",
                    show("Tweaks.log")
                ),
            ],
        );
        changes.extend(staged);
        let path = Path::new("QUEN/journal.jsonl");
        let mut journal = Journal::create(&fs, path).unwrap();
        apply(&fs, &mut journal, &changes, &|| false).unwrap();
        let read = |p: &str| String::from_utf8(fs.read(&bin_dir.join(p)).unwrap()).unwrap();
        assert_eq!(read("W3Test.asi"), "test asi");
        assert_eq!(read("w3fake.INI"), "test fake ini");
        assert_eq!(read("Tweaks.asi"), "test tweaks");
        assert_eq!(fs.read(&parked).unwrap(), b"the user's ini");
        assert_eq!(fs.read(&user_menu).unwrap(), b"test menu");
        assert_eq!(fs.read(&parked_menu).unwrap(), b"the user's menu");
        // The staged mod logs; the replay recycles its log and the copies, and puts the user's files back.
        fs.put(bin_dir.join("W3Test.log"), "a staged mod's log");
        let report = replay(&fs.read(path).unwrap(), &fs);
        assert!(report.clean() && report.kept.is_empty(), "{report:#?}");
        let mut after = fs.files();
        after.retain(|p, _| !p.starts_with("QUEN"));
        assert_eq!(after, before);
        for name in ["W3Test.log", "W3Test.asi", "w3fake.INI", "Tweaks.asi"] {
            assert!(
                report.recycled.contains(&bin_dir.join(name)),
                "{name}: {report:#?}"
            );
        }
        assert!(report.recycled.contains(&user_menu), "{report:#?}");
    }

    /// Each refusal comes before any change, naming the file: a name quen never parks, a folder or link in bin or the
    /// menu folder, a second file of one name (in any case), a file another change of the launch makes, a file it can't
    /// read.
    #[test]
    fn staging_refuses_before_any_change() {
        let fs = game();
        fs.put("PC/W3Quen.asi", "x");
        fs.put("PC/plugins.asi", "x");
        fs.put("PC/Linked.asi", "x");
        fs.put("PC/a/W3Test.asi", "x");
        fs.put("PC/b/w3test.ASI", "x");
        fs.put("PC/steam_appid.txt", "x");
        fs.put("PC/hud.xml", "x");
        let before = fs.files();
        let bin = [
            Entry {
                name: "plugins.asi".into(),
                is_dir: true,
                is_link: false,
            },
            file("Linked.asi", true),
        ];
        let menu = [file("hud.xml", true)];
        let earlier = plan(&fs, &inputs(&[], &bin)).unwrap();
        for (files, why) in [
            (&["PC/W3Quen.asi"][..], "never parks"),
            (&["PC/plugins.asi"], "a folder or link"),
            (
                &["PC/Linked.asi"],
                "a folder or link of that name is in bin",
            ),
            (
                &["PC/hud.xml"],
                "a folder or link of that name is in the menu folder",
            ),
            (&["PC/a/W3Test.asi", "PC/b/w3test.ASI"], "a second file"),
            (
                &["PC/steam_appid.txt"],
                "the launch changes it already (place ",
            ),
            (&["PC/Missing.asi"], "not found"),
        ] {
            let files: Vec<PathBuf> = files.iter().map(PathBuf::from).collect();
            let got = stage(
                &fs,
                Path::new("GAME"),
                &files,
                park::W3_NEVER_PARK,
                &bin,
                &menu,
                &earlier,
            );
            let err = got.unwrap_err();
            assert!(
                err.contains(why) && err.contains(&files.last().unwrap().display().to_string()),
                "{files:?}: {err}"
            );
        }
        assert_eq!(fs.files(), before);
    }

    /// Finding 38: a test mod copied into Mods by hand and recycled after the boot. Staged, each folder is a copy; the
    /// user's mod of that name (in another case) is parked first and comes back; the compiled scripts are kept and put
    /// back. Refused before any change: a name quen never parks, a file in Mods, a second folder of one name.
    #[test]
    fn staged_mods_land_park_what_is_there_and_replay_away() {
        let fs = game();
        let compiled = compiled_scripts(Path::new("GAME"));
        fs.put(&compiled, "compiled");
        for (dir, tree) in [("PC/modalpha", "test alpha"), ("PC/modNew", "test new")] {
            fs.put(dir, tree);
        }
        let before = fs.files();
        let mods = [dir("modAlpha"), dir("modQuen"), file("modFile", false)];
        let mut changes = plan(&fs, &inputs(&mods, &[])).unwrap();
        let backups = Path::new("QUEN/backup");
        let (game_dir, mods_dir) = (Path::new("GAME"), Path::new("GAME").join("Mods"));
        let staged = |dirs: &[&str]| {
            let dirs: Vec<PathBuf> = dirs.iter().map(PathBuf::from).collect();
            stage_mods(
                &fs,
                game_dir,
                &dirs,
                park::W3_NEVER_PARK,
                &mods,
                backups,
                &changes,
            )
        };
        for (dirs, why) in [
            (&["PC/modQuen"][..], "never parks"),
            (&["PC/modFile"], "a file or link"),
            (&["PC/modNew", "PC/b/modnew"], "a second folder"),
        ] {
            let err = staged(dirs).unwrap_err();
            assert!(
                err.contains(why) && err.contains(dirs[dirs.len() - 1]),
                "{err}"
            );
        }
        let staged = staged(&["PC/modalpha", "PC/modNew"]).unwrap();
        let alpha = mods_dir.join("modAlpha");
        let parked = game_dir.join(PARKED).join("Mods").join("modAlpha");
        let lines: Vec<String> = staged.iter().map(Change::summary).collect();
        assert_eq!(
            lines[..3],
            [
                format!("park {} -> {}", alpha.display(), parked.display()),
                format!("stage PC/modalpha -> {}", alpha.display()),
                format!("stage PC/modNew -> {}", mods_dir.join("modNew").display()),
            ]
        );
        assert!(
            matches!(&staged[3..], [Change::Rebuilt { path, .. }] if *path == compiled),
            "{lines:?}"
        );
        changes.extend(staged);
        let path = Path::new("QUEN/journal.jsonl");
        let mut journal = Journal::create(&fs, path).unwrap();
        apply(&fs, &mut journal, &changes, &|| false).unwrap();
        assert_eq!(fs.read(&alpha).unwrap(), b"test alpha");
        assert_eq!(fs.read(&parked).unwrap(), b"alpha");
        // The game recompiles its scripts with the staged mods; the replay puts everything back.
        fs.put(&compiled, "recompiled");
        let report = replay(&fs.read(path).unwrap(), &fs);
        assert!(report.clean() && report.kept.is_empty(), "{report:#?}");
        let mut after = fs.files();
        after.retain(|p, _| !p.starts_with("QUEN"));
        assert_eq!(after, before);
        assert!(report.recycled.contains(&alpha), "{report:#?}");
    }

    #[test]
    fn a_file_changed_after_the_plan_is_refused_before_its_backup() {
        let fs = game();
        let changes = plan(&fs, &inputs(&[], &[])).unwrap();
        fs.put("DOCS/dx12user.settings", "edited");
        let mut journal = Journal::create(&fs, Path::new("QUEN/journal.jsonl")).unwrap();
        let err = apply(&fs, &mut journal, &changes, &|| false).unwrap_err();
        assert!(err.to_string().contains("changed since"), "{err}");
        assert!(!fs.exists(Path::new("QUEN/backup/dx12user.settings")));
        let stopped = apply(&fs, &mut journal, &changes, &|| true).unwrap_err();
        assert_eq!(stopped.kind(), io::ErrorKind::Interrupted);
    }
}
