//! The append-only session journal and its replay, against a file-system trait so both run on any OS.
//!
//! A `Step` line is on disk before each action starts and a `Done` line follows it. [`replay`] walks the journal
//! backwards and undoes every started step, but only what still verifies: anything changed since is reported and left
//! as found. A normal `end` is a replay of the live journal, so there is one undo path.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The file operations the journal and its replay need. A path may name a file or (a parked mod) a folder.
pub trait Fs {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;
    /// Writes a temp file beside `path`, fsyncs it and renames it over `path`: readers see the old bytes or the new.
    fn write_atomic(&self, path: &Path, data: &[u8]) -> io::Result<()>;
    /// Renames on one volume, keeping the file id. Fails if `to` exists.
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
    fn exists(&self, path: &Path) -> bool;
    /// Lowercase hex sha256 of a file's bytes. A real implementation defines it over a folder's whole tree.
    fn sha256(&self, path: &Path) -> io::Result<String> {
        Ok(sha256_hex(&self.read(path)?))
    }
    /// An opaque identity that a rename on one volume keeps (volume serial and file id on Windows).
    fn file_id(&self, path: &Path) -> io::Result<String>;
    fn fsync(&self, path: &Path) -> io::Result<()>;
    /// Sends `path` to the Recycle Bin: nothing is ever deleted for good.
    fn recycle(&self, path: &Path) -> io::Result<()>;
    /// Copies the folder `from` (on this PC) to `to`, where nothing is, with its whole tree. A flat file system (no
    /// folders, as [`MemFs`]) copies the one entry.
    fn copy_tree(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.write_atomic(to, &self.read(from)?)
    }
    /// A file's last write time, in nanoseconds since 1970.
    fn mtime(&self, path: &Path) -> io::Result<u64>;
    fn set_mtime(&self, path: &Path, nanos: u64) -> io::Result<()>;
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

/// [`sha256_hex`] of a file, read in 1 MiB pieces: a capture may be gigabytes.
pub fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0; 1 << 20];
    loop {
        let n = io::Read::read(&mut file, &mut buf)?;
        if n == 0 {
            return Ok(hex(&hasher.finalize()));
        }
        hasher.update(&buf[..n]);
    }
}

fn hex(digest: &[u8]) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// One journal line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "rec", rename_all = "snake_case")]
pub enum Record {
    /// On disk before its action starts.
    Step { id: u64, action: Action },
    /// Written after the action with this id finished.
    Done { id: u64 },
}

/// An action, with what replay needs to undo it safely.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Action {
    /// Copies `src` to `backup`; `sha` is of the bytes copied. Nothing to undo: patches restore from it, and a fully
    /// undone session recycles it.
    Backup {
        src: PathBuf,
        backup: PathBuf,
        sha: String,
    },
    /// Rewrites `path`, backed up earlier in the journal; `mtime` is its write time then (journals before 2026-10-18
    /// have none). Undo: restore the backup, and that write time (feature tests 2: NVIDIA's signed interposer came back
    /// with a new one).
    Patch {
        path: PathBuf,
        sha_before: String,
        sha_after: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mtime: Option<u64>,
        /// Keys whose `Key=Value` lines don't count when the live file is compared with the one put back: a live
        /// file that differs only in them isn't kept beside, and a note says so (dx12user.settings: the keys the
        /// launch forced and LastGameVersion, finding 35). Journals before quen 0.1.8 have none.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        ignore: Vec<String>,
    },
    /// Renames `from` (a mod being parked) to `to`. Undo: rename it back.
    Move {
        from: PathBuf,
        to: PathBuf,
        sha: String,
        file_id: String,
    },
    /// A new file about to be written at `path` (a Place's stage, `<dest>.quen-stage`), before it is (a crash
    /// between the write and the Place step left it unjournaled). Undo: recycle it if it is still there with these
    /// bytes; once placed, it is gone.
    Stage { path: PathBuf, sha: String },
    /// Renames a staged new file (this sha and file id) to `dest`, where nothing was. Undo: recycle it.
    Place {
        dest: PathBuf,
        sha: String,
        file_id: String,
    },
    /// The log a staged mod may write at `path`, where no file was at the launch. Undo: recycle it if it is there (the
    /// end copies the boot's logs into the session's game-logs first).
    NewLog { path: PathBuf },
    /// A mod folder about to be copied to `path` (`Mods\<name>`) from this PC, where nothing was at the launch
    /// (finding 38). Undo: recycle it if it is there, a copy cut off by a crash included: its source stays on the PC.
    NewDir { path: PathBuf },
    /// A file the game rewrites by itself during the session, backed up earlier in the journal, with its sha and write
    /// time then: Witcher 3 recompiles its scripts into `content\content0\x64.final.redscripts` when a script mod is
    /// parked (first experiment boot), and reorders the lines of input.settings at every launch (live boot 1). Undo:
    /// the backup back, bytes and write time. With `keep` (input.settings, where the user rebinds keys in the game's
    /// menu), a live file whose lines differ from the backup's in more than their order is first kept beside, as a
    /// patch's is (journals before 2026-10-18 have no `keep`).
    Rebuilt {
        path: PathBuf,
        sha: String,
        mtime: u64,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        keep: bool,
    },
    /// A write into the running game's memory (a cvar's live value, a `mem_patch`). Nothing to undo: the value
    /// lives only in the game process, which every replay path has ended first (it dies with the session's job). The
    /// step is the session's record of it: saves written after `t` (seconds since 1970) are tainted.
    GameMemory { what: String, t: u64 },
}

/// Writes the journal. Each record rewrites the whole file through [`Fs::write_atomic`], so a crash leaves the journal
/// with or without the record, never a torn line.
pub struct Journal<'a> {
    fs: &'a dyn Fs,
    path: PathBuf,
    text: Vec<u8>,
    next: u64,
}

impl<'a> Journal<'a> {
    /// Starts a journal at `path`. Refuses if one is there: replay it first.
    pub fn create(fs: &'a dyn Fs, path: &Path) -> io::Result<Self> {
        if fs.exists(path) {
            let why = format!("{} exists: replay it first", path.display());
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, why));
        }
        Ok(Self {
            fs,
            path: path.to_owned(),
            text: Vec::new(),
            next: 1,
        })
    }

    /// Continues the journal at `path`, which this process writes (a live session's): new steps are numbered after the
    /// ids already there. Refuses a journal whose last line is cut off.
    pub fn open(fs: &'a dyn Fs, path: &Path) -> io::Result<Self> {
        let text = fs.read(path)?;
        if text.last().is_some_and(|&b| b != b'\n') {
            let why = format!("{}: the last line is cut off", path.display());
            return Err(io::Error::new(io::ErrorKind::InvalidData, why));
        }
        let last = text
            .split(|&b| b == b'\n')
            .filter_map(|line| serde_json::from_slice::<Record>(line).ok())
            .map(|r| match r {
                Record::Step { id, .. } | Record::Done { id } => id,
            })
            .max();
        Ok(Self {
            fs,
            path: path.to_owned(),
            text,
            next: last.unwrap_or(0) + 1,
        })
    }

    /// Puts `action`'s step on disk and returns its id for [`Journal::done`]. Start the action only after this returns.
    pub fn step(&mut self, action: Action) -> io::Result<u64> {
        let id = self.next;
        self.append(&Record::Step { id, action })?;
        self.next += 1;
        Ok(id)
    }

    pub fn done(&mut self, id: u64) -> io::Result<()> {
        self.append(&Record::Done { id })
    }

    /// Takes step `id` back off, before its done line, when its action never happened (the game refused a verb:
    /// feature tests 2c). Refused unless it is the journal's last line, so nothing after it can depend on it.
    pub fn retract(&mut self, id: u64) -> io::Result<()> {
        let body = self.text.strip_suffix(b"\n").unwrap_or_default();
        let start = body.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
        match serde_json::from_slice(&body[start..]) {
            Ok(Record::Step { id: last, .. }) if last == id => {}
            _ => {
                return Err(io::Error::other(format!(
                    "step {id} isn't the journal's last line"
                )));
            }
        }
        self.fs.write_atomic(&self.path, &self.text[..start])?;
        self.text.truncate(start);
        Ok(())
    }

    fn append(&mut self, record: &Record) -> io::Result<()> {
        let len = self.text.len();
        serde_json::to_writer(&mut self.text, record)?;
        self.text.push(b'\n');
        let written = self.fs.write_atomic(&self.path, &self.text);
        if written.is_err() {
            self.text.truncate(len);
        }
        written
    }
}

/// What a replay did and what it left, newest step first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ReplayReport {
    /// One line per action undone.
    pub undone: Vec<String>,
    /// Live files that differed from what quen wrote, kept beside as `<name>.quen-current` (`-2`, `-3`, ... when that
    /// name holds other bytes) before the restore.
    pub kept: Vec<PathBuf>,
    /// Steps not undone because their files no longer verify; those files are left exactly as found.
    pub left: Vec<Left>,
    /// Placed files and spent backups, sent to the Recycle Bin.
    pub recycled: Vec<PathBuf>,
    /// Complete journal lines (1-based) that don't parse: any step on them was not undone.
    pub unreadable: Vec<usize>,
    /// A torn last line, interrupted steps, backups kept.
    pub notes: Vec<String>,
}

impl ReplayReport {
    /// Every started step was undone (live files may still have been kept beside: see `kept`).
    pub fn clean(&self) -> bool {
        self.left.is_empty() && self.unreadable.is_empty()
    }
}

/// A step replay left alone, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Left {
    pub step: u64,
    pub action: Action,
    pub why: String,
}

/// Undoes every started step of `journal`, newest first. A torn last line is ignored with a note: a step's line is on
/// disk before its action starts, so that action never ran. Backups are recycled only when every step was undone.
pub fn replay(journal: &[u8], fs: &dyn Fs) -> ReplayReport {
    let mut report = ReplayReport::default();
    let mut steps = Vec::new();
    let mut done = Vec::new();
    let lines: Vec<&[u8]> = journal.split(|&b| b == b'\n').collect();
    for (i, line) in lines.iter().enumerate() {
        if line.trim_ascii().is_empty() {
            continue;
        }
        match serde_json::from_slice(line) {
            Ok(Record::Step { id, action }) => steps.push((id, action)),
            Ok(Record::Done { id }) => done.push(id),
            Err(_) if i + 1 == lines.len() => {
                report.notes.push(format!(
                    "line {} is torn (cut off mid-write): ignored",
                    i + 1
                ));
            }
            Err(_) => report.unreadable.push(i + 1),
        }
    }
    for (id, _) in steps.iter().filter(|(id, _)| !done.contains(id)) {
        report
            .notes
            .push(format!("step {id} has no done line: it was interrupted"));
    }
    for (i, (id, action)) in steps.iter().enumerate().rev() {
        if let Err(Why(why)) = undo(fs, &steps[..i], action, &mut report) {
            report.left.push(Left {
                step: *id,
                action: action.clone(),
                why,
            });
        }
    }
    // An atomic write a crash cut off leaves `<path>.quen-tmp` beside a file the journal names.
    for (_, action) in &steps {
        let (Action::Patch { path, .. }
        | Action::Rebuilt { path, .. }
        | Action::Stage { path, .. }) = action
        else {
            continue;
        };
        let mut tmp = OsString::from(path);
        tmp.push(".quen-tmp");
        let tmp = PathBuf::from(tmp);
        if fs.exists(&tmp) {
            match fs.recycle(&tmp) {
                Ok(()) => report.recycled.push(tmp),
                Err(e) => report.notes.push(format!("kept {}: {e}", tmp.display())),
            }
        }
    }
    for (_, action) in &steps {
        let Action::Backup { backup, sha, .. } = action else {
            continue;
        };
        if !fs.exists(backup) {
            continue;
        }
        let spent = report.clean() && fs.sha256(backup).is_ok_and(|s| s == *sha);
        match spent.then(|| fs.recycle(backup)) {
            Some(Ok(())) => report.recycled.push(backup.clone()),
            Some(Err(e)) => report
                .notes
                .push(format!("kept backup {}: {e}", backup.display())),
            None if report.clean() => report
                .notes
                .push(format!("kept backup {}: fails its sha", backup.display())),
            None => report.notes.push(format!(
                "kept backup {}: not every step was undone",
                backup.display()
            )),
        }
    }
    report
}

/// Why a step was left as found.
struct Why(String);

impl From<String> for Why {
    fn from(why: String) -> Self {
        Self(why)
    }
}

impl From<io::Error> for Why {
    fn from(e: io::Error) -> Self {
        Self(format!("I/O error: {e}"))
    }
}

/// Undoes one action if its files still verify. `earlier` are the steps before it, where a patch finds its backup.
fn undo(
    fs: &dyn Fs,
    earlier: &[(u64, Action)],
    action: &Action,
    report: &mut ReplayReport,
) -> Result<(), Why> {
    match action {
        Action::Backup { .. } => {}
        Action::Patch {
            path,
            sha_before,
            sha_after,
            mtime,
            ignore,
        } => {
            let (backup, original) = backup_of(earlier, path)?;
            let live = if fs.exists(path) {
                Some(fs.sha256(path)?)
            } else {
                None
            };
            if live.as_ref() == Some(original) {
                return Ok(());
            }
            let bytes = backed_up(fs, backup, original)?;
            // A state this journal didn't write (the game rewrote the file): keep it beside, so neither is lost.
            if let Some(live) = live
                && live != *sha_after
                && live != *sha_before
            {
                let current = fs.read(path)?;
                let counted = |data: &[u8]| {
                    let mut lines = sorted_lines(data);
                    lines.retain(|l| {
                        !l.split_once('=')
                            .is_some_and(|(k, _)| ignore.iter().any(|i| i == k.trim()))
                    });
                    lines
                };
                if !ignore.is_empty() && counted(&current) == counted(&bytes) {
                    report.notes.push(format!(
                        "{}: the game's copy differed from the one put back only in {}: not kept",
                        path.display(),
                        ignore.join(", ")
                    ));
                } else {
                    keep_beside(fs, path, &current, &live, report)?;
                }
            }
            fs.write_atomic(path, &bytes)?;
            if let Some(mtime) = mtime {
                fs.set_mtime(path, *mtime)?;
            }
            report.undone.push(format!(
                "restored {} from {}",
                path.display(),
                backup.display()
            ));
        }
        Action::Move {
            from,
            to,
            sha,
            file_id,
        } => {
            if verifies(fs, to, sha, file_id)? {
                if fs.exists(from) {
                    return Err(format!(
                        "{} is taken again, the parked copy stays",
                        from.display()
                    )
                    .into());
                }
                fs.rename(to, from)?;
                if let Err(e) = fs.fsync(from) {
                    report.notes.push(format!("fsync {}: {e}", from.display()));
                }
                report
                    .undone
                    .push(format!("moved {} back to {}", to.display(), from.display()));
            } else if fs.exists(to) {
                return Err(format!(
                    "{} changed since it was moved (sha or file id)",
                    to.display()
                )
                .into());
            } else if !verifies(fs, from, sha, file_id)? {
                let why = format!(
                    "neither {} nor {} is what was moved",
                    from.display(),
                    to.display()
                );
                return Err(why.into());
            }
        }
        Action::Rebuilt {
            path,
            sha,
            mtime,
            keep,
        } => {
            let same = fs.exists(path) && fs.sha256(path)? == *sha;
            if same && fs.mtime(path)? == *mtime {
                return Ok(());
            }
            if !same {
                let (backup, original) = backup_of(earlier, path)?;
                if original != sha {
                    return Err(
                        format!("the backup of {} is of other bytes", path.display()).into(),
                    );
                }
                let bytes = backed_up(fs, backup, original)?;
                if *keep && fs.exists(path) {
                    let live = fs.read(path)?;
                    if sorted_lines(&live) != sorted_lines(&bytes) {
                        keep_beside(fs, path, &live, &sha256_hex(&live), report)?;
                    }
                }
                fs.write_atomic(path, &bytes)?;
            }
            fs.set_mtime(path, *mtime)?;
            report.undone.push(format!(
                "put {} back as it was before the game rebuilt it",
                path.display()
            ));
        }
        Action::Stage { path, sha } => {
            if fs.exists(path) {
                if fs.sha256(path)? != *sha {
                    let why = format!("{} holds other bytes than quen staged", path.display());
                    return Err(why.into());
                }
                fs.recycle(path)?;
                report.recycled.push(path.clone());
                report.undone.push(format!(
                    "removed the staged {} (Recycle Bin)",
                    path.display()
                ));
            }
        }
        Action::GameMemory { what, .. } => report
            .notes
            .push(format!("{what}: nothing to undo, it ended with the game")),
        Action::NewLog { path } => {
            if fs.exists(path) {
                fs.recycle(path)?;
                report.recycled.push(path.clone());
                report.undone.push(format!(
                    "removed {}, a staged mod's log (Recycle Bin)",
                    path.display()
                ));
            }
        }
        Action::NewDir { path } => {
            if fs.exists(path) {
                fs.recycle(path)?;
                report.recycled.push(path.clone());
                report.undone.push(format!(
                    "removed the staged mod {} (Recycle Bin)",
                    path.display()
                ));
            }
        }
        Action::Place { dest, sha, file_id } => {
            if verifies(fs, dest, sha, file_id)? {
                fs.recycle(dest)?;
                report.recycled.push(dest.clone());
                report
                    .undone
                    .push(format!("removed {} (Recycle Bin)", dest.display()));
            } else if fs.exists(dest) {
                return Err(format!(
                    "{} changed since it was placed (sha or file id)",
                    dest.display()
                )
                .into());
            }
        }
    }
    Ok(())
}

/// Keeps `live` (the bytes at `path` now, `sha` theirs) beside it as `<name>.quen-current`, or `-2`, `-3`, ... where
/// that name holds other bytes: a kept copy, an earlier session's included, is never replaced. A copy that
/// holds these bytes already is reused.
fn keep_beside(
    fs: &dyn Fs,
    path: &Path,
    live: &[u8],
    sha: &str,
    report: &mut ReplayReport,
) -> Result<(), Why> {
    for n in 1..1000 {
        let mut current = OsString::from(path);
        current.push(".quen-current");
        if n > 1 {
            current.push(format!("-{n}"));
        }
        let current = PathBuf::from(current);
        if !fs.exists(&current) {
            fs.write_atomic(&current, live)?;
        } else if fs.sha256(&current)? != sha {
            continue;
        }
        report.kept.push(current);
        return Ok(());
    }
    Err(format!("{}: 999 kept copies beside it already", path.display()).into())
}

/// A text file's non-empty lines, trimmed and sorted: two files with the same lines in another order compare equal.
fn sorted_lines(data: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(data);
    let mut lines: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect();
    lines.sort();
    lines
}

/// The backup of `path` earlier in the journal, and the sha of what it holds.
fn backup_of<'a>(earlier: &'a [(u64, Action)], path: &Path) -> Result<(&'a Path, &'a String), Why> {
    let backup = earlier.iter().rev().find_map(|(_, a)| match a {
        Action::Backup { src, backup, sha } if src == path => Some((backup.as_path(), sha)),
        _ => None,
    });
    backup.ok_or_else(|| format!("no backup of {} earlier in the journal", path.display()).into())
}

/// A backup's bytes, if they still have the sha the journal recorded.
fn backed_up(fs: &dyn Fs, backup: &Path, sha: &str) -> Result<Vec<u8>, Why> {
    match fs.read(backup) {
        Ok(bytes) if sha256_hex(&bytes) == sha => Ok(bytes),
        _ => Err(format!("backup {} is missing or fails its sha", backup.display()).into()),
    }
}

/// Whether `path` exists with this sha and file id.
fn verifies(fs: &dyn Fs, path: &Path, sha: &str, file_id: &str) -> io::Result<bool> {
    Ok(fs.exists(path) && fs.sha256(path)? == sha && fs.file_id(path)? == file_id)
}

/// An in-memory [`Fs`] for tests. Every path is a leaf holding bytes, so a parked mod folder is one entry.
/// `write_atomic` and [`MemFs::put`] make a new file (a new id, and its write time a newer one); `rename` keeps both.
#[derive(Debug, Default, Clone)]
pub struct MemFs {
    state: RefCell<MemState>,
}

#[derive(Debug, Default, Clone)]
struct MemState {
    files: BTreeMap<PathBuf, (u64, Vec<u8>, u64)>,
    recycled: Vec<PathBuf>,
    next_id: u64,
}

impl MemFs {
    /// Creates or replaces `path` as a new file, as another program would.
    pub fn put(&self, path: impl AsRef<Path>, data: impl AsRef<[u8]>) {
        let mut state = self.state.borrow_mut();
        let id = state.next_id;
        state.next_id += 1;
        state
            .files
            .insert(path.as_ref().to_owned(), (id, data.as_ref().to_vec(), id));
    }

    /// Every file and its bytes.
    pub fn files(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        self.state
            .borrow()
            .files
            .iter()
            .map(|(path, (_, data, _))| (path.clone(), data.clone()))
            .collect()
    }

    /// What went to the Recycle Bin, in order.
    pub fn recycled(&self) -> Vec<PathBuf> {
        self.state.borrow().recycled.clone()
    }

    fn file<T>(&self, path: &Path, get: impl FnOnce(&(u64, Vec<u8>, u64)) -> T) -> io::Result<T> {
        self.state
            .borrow()
            .files
            .get(path)
            .map(get)
            .ok_or_else(|| missing(path))
    }
}

fn missing(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        format!("{} not found", path.display()),
    )
}

impl Fs for MemFs {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.file(path, |(_, data, _)| data.clone())
    }

    fn write_atomic(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        self.put(path, data);
        Ok(())
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        let mut state = self.state.borrow_mut();
        if state.files.contains_key(to) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} exists", to.display()),
            ));
        }
        let file = state.files.remove(from).ok_or_else(|| missing(from))?;
        state.files.insert(to.to_owned(), file);
        Ok(())
    }

    fn exists(&self, path: &Path) -> bool {
        self.state.borrow().files.contains_key(path)
    }

    fn file_id(&self, path: &Path) -> io::Result<String> {
        self.file(path, |(id, ..)| id.to_string())
    }

    fn fsync(&self, path: &Path) -> io::Result<()> {
        self.file(path, |_| ())
    }

    fn recycle(&self, path: &Path) -> io::Result<()> {
        let mut state = self.state.borrow_mut();
        state.files.remove(path).ok_or_else(|| missing(path))?;
        state.recycled.push(path.to_owned());
        Ok(())
    }

    fn mtime(&self, path: &Path) -> io::Result<u64> {
        self.file(path, |&(.., mtime)| mtime)
    }

    fn set_mtime(&self, path: &Path, nanos: u64) -> io::Result<()> {
        let mut state = self.state.borrow_mut();
        let file = state.files.get_mut(path).ok_or_else(|| missing(path))?;
        file.2 = nanos;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_are_one_json_object_per_line() {
        let fs = MemFs::default();
        let path = Path::new("J/journal.jsonl");
        let mut journal = Journal::create(&fs, path).unwrap();
        let action = Action::Move {
            from: "M/a".into(),
            to: "P/a".into(),
            sha: "00".into(),
            file_id: "7".into(),
        };
        let id = journal.step(action.clone()).unwrap();
        journal.done(id).unwrap();
        let text = String::from_utf8(fs.read(path).unwrap()).unwrap();
        let want = concat!(
            r#"{"rec":"step","id":1,"action":{"op":"move","from":"M/a","to":"P/a","sha":"00","file_id":"7"}}"#,
            "\n",
            r#"{"rec":"done","id":1}"#,
            "\n",
        );
        assert_eq!(text, want);
        let first: Record = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(first, Record::Step { id: 1, action });
    }

    #[test]
    fn journal_refuses_to_start_over_an_old_one() {
        let fs = MemFs::default();
        fs.put("J/journal.jsonl", "");
        let err = Journal::create(&fs, Path::new("J/journal.jsonl"))
            .err()
            .unwrap();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn mem_fs_keeps_ids_across_renames_only() {
        let fs = MemFs::default();
        fs.put("a", "x");
        let id = fs.file_id(Path::new("a")).unwrap();
        fs.rename(Path::new("a"), Path::new("b")).unwrap();
        assert_eq!(fs.file_id(Path::new("b")).unwrap(), id);
        fs.write_atomic(Path::new("b"), b"x").unwrap();
        assert_ne!(fs.file_id(Path::new("b")).unwrap(), id);
        fs.put("c", "y");
        let err = fs.rename(Path::new("b"), Path::new("c")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs.sha256(Path::new("c")).unwrap(), sha256_hex(b"y"));
        fs.recycle(Path::new("c")).unwrap();
        assert!(!fs.exists(Path::new("c")));
        assert_eq!(fs.recycled(), [PathBuf::from("c")]);
    }

    #[test]
    fn sha256_is_lowercase_hex() {
        let want = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        assert_eq!(sha256_hex(b""), want);
    }

    #[test]
    fn an_open_journal_continues_its_ids_and_game_writes_undo_nothing() {
        let fs = MemFs::default();
        fs.put("M/a", "mod");
        let path = Path::new("J/journal.jsonl");
        let (from, to) = (PathBuf::from("M/a"), PathBuf::from("P/a"));
        let mut journal = Journal::create(&fs, path).unwrap();
        let moved = journal
            .step(Action::Move {
                from: from.clone(),
                to: to.clone(),
                sha: fs.sha256(&from).unwrap(),
                file_id: fs.file_id(&from).unwrap(),
            })
            .unwrap();
        fs.rename(&from, &to).unwrap();
        journal.done(moved).unwrap();
        drop(journal);
        let mut journal = Journal::open(&fs, path).unwrap();
        let what = "cvar Rendering/X set 1".to_string();
        let write = journal
            .step(Action::GameMemory {
                what: what.clone(),
                t: 7,
            })
            .unwrap();
        assert_eq!(write, 2);
        journal.done(write).unwrap();
        // A step the game refused goes back off; only the last line can.
        let before = fs.read(path).unwrap();
        let refused = journal
            .step(Action::GameMemory {
                what: "channel weather".into(),
                t: 8,
            })
            .unwrap();
        assert!(journal.retract(write).is_err(), "not the last line");
        journal.retract(refused).unwrap();
        assert_eq!(fs.read(path).unwrap(), before);
        assert!(journal.retract(refused).is_err(), "gone already");
        let report = replay(&fs.read(path).unwrap(), &fs);
        assert!(report.clean(), "{report:#?}");
        assert_eq!(report.undone, ["moved P/a back to M/a"]);
        assert_eq!(
            report.notes,
            [format!("{what}: nothing to undo, it ended with the game")]
        );

        fs.put(path, "{\"rec\":\"done\",\"id\":1}\n{\"rec\":\"st");
        let err = Journal::open(&fs, path).err().unwrap();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn empty_and_blank_journals_undo_nothing() {
        let fs = MemFs::default();
        fs.put("a", "x");
        for journal in [&b""[..], b"\n", b"\n\r\n  \n"] {
            assert_eq!(replay(journal, &fs), ReplayReport::default());
        }
        assert_eq!(fs.files().len(), 1);
    }
}
