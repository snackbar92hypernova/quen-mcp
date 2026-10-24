//! The SQLite ledger (docs/intent.md, "The ledger"): schema and migrations, typed ingest, a read-only query with a
//! time limit, the doc-reference scanner, the cleanup-candidate list, the import of the Python harness's history, and
//! game knowledge: REA's answers as a cache, the RE knowledge base's index, and `about`. It records files by path and
//! sha256 and never deletes one.
//!
//! The schema is `schema/NNNN_*.sql` at the repo root, embedded at build time and applied in order by
//! `PRAGMA user_version`; before the first migration of an existing ledger, a copy at its old version goes to
//! `backups/` beside it. `schema/README.md` describes every table and view for the agents that query them
//! ([`schema_description`]). Every connection sets a busy timeout and `foreign_keys=ON`, and read-write ones use WAL,
//! so several quen processes can share one database.

mod ingest;
mod kb;
mod knowledge;
mod migrate;
pub(crate) mod refs;

pub use ingest::{
    BootEnd, BootModStatus, BootStart, Capture, Claim, Game, LoadStatus, METHOD_FAMILIES,
    Measurement, ModVersion, Outcome, PlayerPose, Pose, Provenance, ReTarget, Row, Save, Sweep,
    SweepFrame, SweepRun, Tag,
};
pub use kb::{KbClaims, KbScan, Section, git_head, sections};
pub use knowledge::{
    About, BundleImport, CaptureRef, Extraction, canonical, extraction_key, profile_digest,
};
pub use migrate::{Harness, Plan, Summary};
pub(crate) use refs::glob;
pub use refs::{Candidate, CaptureRow, Ref, ScanSummary, Scanner};

use std::fmt;
use std::fs::{self, File};
use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::Duration;

use rusqlite::types::{Value as SqlValue, ValueRef};
use rusqlite::{Connection, ErrorCode, OpenFlags, TransactionBehavior};
use serde::Serialize;
use serde_json::Value;

/// The migrations in order: applying entry `i` brings `user_version` to `i + 1`. A shipped one is never edited, only
/// followed by a new one (AGENTS.md; tests/ledger_upgrade.rs pins each file's sha256).
const MIGRATIONS: &[&str] = &[
    include_str!("../../../schema/0001_init.sql"),
    include_str!("../../../schema/0002_mods.sql"),
    include_str!("../../../schema/0003_harness_info.sql"),
    include_str!("../../../schema/0004_claims_strict.sql"),
    include_str!("../../../schema/0005_views.sql"),
    include_str!("../../../schema/0006_sweeps.sql"),
    include_str!("../../../schema/0007_captures_removed.sql"),
    include_str!("../../../schema/0008_captures_missing.sql"),
    include_str!("../../../schema/0009_knowledge.sql"),
    include_str!("../../../schema/0010_saves_made_by_boot.sql"),
    include_str!("../../../schema/0011_play_sessions.sql"),
    include_str!("../../../schema/0012_pose_yaw_optional.sql"),
    include_str!("../../../schema/0013_captures_deleted.sql"),
    include_str!("../../../schema/0014_boot_samplers.sql"),
    include_str!("../../../schema/0015_mod_kinds.sql"),
    include_str!("../../../schema/0016_provenance_other.sql"),
];

/// The schema version this build migrates to.
pub const SCHEMA_VERSION: i64 = MIGRATIONS.len() as i64;

/// `PRAGMA application_id` of a ledger ('quen'), set by the first migration.
const APPLICATION_ID: i64 = 0x7175_656E;

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Every table and view of the ledger for an agent that queries it: what each holds, its columns, how the rows relate,
/// and example queries for common questions (each runs as is through [`Ledger::query`]).
pub fn schema_description() -> &'static str {
    include_str!("../../../schema/README.md")
}

#[derive(Debug)]
pub enum Error {
    /// Refused after checking, before anything was written.
    Refused(String),
    /// A query ran past its time limit and was interrupted.
    TimeLimit,
    Sqlite(rusqlite::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Refused(why) => write!(f, "refused, nothing changed: {why}"),
            Error::TimeLimit => f.write_str("query interrupted at its time limit"),
            // Several agents at once: what SQLite's "database is locked" means here.
            Error::Sqlite(rusqlite::Error::SqliteFailure(e, _))
                if e.code == ErrorCode::DatabaseBusy =>
            {
                write!(
                    f,
                    "busy: another connection had the ledger locked (another quen writing, or a program with a \
                     transaction open on it, e.g. a database viewer with unsaved changes), and quen waits up to {} s \
                     for a lock; try again",
                    BUSY_TIMEOUT.as_secs()
                )
            }
            // The callers name the ledger.
            Error::Sqlite(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Sqlite(e) => Some(e),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Error::Sqlite(e)
    }
}

/// The rows of [`Ledger::query`], as JSON values (a blob becomes lowercase hex).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Rows {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
}

/// One connection to the ledger. Connections from [`Ledger::open_readonly`] can only read: their writes fail in SQLite.
pub struct Ledger {
    conn: Connection,
}

impl Ledger {
    /// Opens the ledger at `path`, creating it if missing, and migrates it to [`SCHEMA_VERSION`]. Before the first
    /// migration of an existing ledger, it copies it to `backups/quen-v<version>-<UTC time>.db` beside it, and never
    /// deletes a backup. Refuses, before changing anything, a path inside a git work tree (the source tree is one:
    /// AGENTS.md), a database that isn't a ledger, a ledger from a newer quen, and a migration whose backup failed. The
    /// parent folder must exist.
    pub fn open(path: impl AsRef<Path>) -> Result<Ledger> {
        let path = path.as_ref();
        let abs = std::path::absolute(path)
            .map_err(|e| Error::Refused(format!("can't resolve {}: {e}", path.display())))?;
        if let Some(tree) = abs
            .ancestors()
            .skip(1)
            .find(|dir| dir.join(".git").exists())
        {
            return Err(Error::Refused(format!(
                "{} is inside the git work tree {}; the ledger belongs under the data root",
                abs.display(),
                tree.display()
            )));
        }
        Ledger::init(Connection::open(path)?, Some(&abs))
    }

    /// A private in-memory ledger with the full schema.
    pub fn open_in_memory() -> Result<Ledger> {
        Ledger::init(Connection::open_in_memory()?, None)
    }

    /// A read-only connection to an existing ledger, for `ledger.query` and for quen processes without the lock.
    /// Refuses a database that isn't a ledger, and a ledger from a newer quen.
    pub fn open_readonly(path: impl AsRef<Path>) -> Result<Ledger> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        configure(&conn)?;
        conn.pragma_update(None, "query_only", true)?;
        let app: i64 = conn.pragma_query_value(None, "application_id", |r| r.get(0))?;
        if app != APPLICATION_ID {
            return Err(Error::Refused("not a quen ledger".into()));
        }
        refuse_newer(conn.pragma_query_value(None, "user_version", |r| r.get(0))?)?;
        Ok(Ledger { conn })
    }

    /// Checks and migrates the database `conn` opened, then fills in extraction rvas (`fill_rvas`); `path` is its file,
    /// where one is to be backed up.
    fn init(mut conn: Connection, path: Option<&Path>) -> Result<Ledger> {
        configure(&conn)?;
        // The checks run under the write lock, so a second quen opening a new ledger at the same time waits, then
        // finds it migrated.
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let app: i64 = tx.pragma_query_value(None, "application_id", |r| r.get(0))?;
        let version: i64 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
        let empty: bool =
            tx.query_row("SELECT count(*) = 0 FROM sqlite_schema", [], |r| r.get(0))?;
        if app != APPLICATION_ID && !(app == 0 && empty) {
            return Err(Error::Refused("the database isn't a quen ledger".into()));
        }
        refuse_newer(version)?;
        // Testers keep their ledger across quen updates, so an existing one is copied before it migrates. Under this
        // lock: a second quen opening it meanwhile waits, then finds it migrated and backed up once.
        if let Some(path) = path.filter(|_| (1..SCHEMA_VERSION).contains(&version)) {
            back_up(&tx, path, version)?;
        }
        for (done, sql) in MIGRATIONS.iter().enumerate().skip(version as usize) {
            tx.execute_batch(sql)?;
            tx.pragma_update(None, "user_version", done as i64 + 1)?;
        }
        tx.commit()?;
        // Turning WAL on needs the database to itself. Among concurrent first opens of a new ledger (several agents at
        // once), SQLite has answered it BUSY at once, not after the busy timeout (seen twice in thousands of opens).
        // The ledger then stays in rollback mode, as safe but with readers holding writers off, until a later open
        // turns WAL on.
        match conn.pragma_update_and_check(None, "journal_mode", "wal", |r| r.get::<_, String>(0)) {
            Err(rusqlite::Error::SqliteFailure(e, _)) if e.code == ErrorCode::DatabaseBusy => {}
            other => {
                other?;
            }
        }
        let mut ledger = Ledger { conn };
        ledger.fill_rvas()?;
        Ok(ledger)
    }

    /// Runs `f` in one write transaction: everything it wrote stays, or after an error nothing does. The transaction
    /// takes the write lock at once (BEGIN IMMEDIATE): a deferred one that reads first fails with BUSY at its first
    /// write while another connection writes, instead of waiting out the busy timeout. Inside another `atomic` it is a
    /// savepoint. Every read-then-write in the ledger goes through here.
    pub fn atomic<T>(&mut self, f: impl FnOnce(&mut Ledger) -> Result<T>) -> Result<T> {
        let outer = self.conn.is_autocommit();
        self.conn.execute_batch(if outer {
            "BEGIN IMMEDIATE"
        } else {
            "SAVEPOINT atomic"
        })?;
        let out = f(self);
        self.conn.execute_batch(match (outer, out.is_ok()) {
            (true, true) => "COMMIT",
            (true, false) => "ROLLBACK",
            (false, true) => "RELEASE atomic",
            (false, false) => "ROLLBACK TO atomic; RELEASE atomic",
        })?;
        out
    }

    /// Runs one read-only `SELECT` or `WITH` statement with positional parameters (`?1`, `?2`, …; null, bool, number or
    /// string) and returns all its rows, or [`Error::TimeLimit`] if it runs longer than `time_limit`. Anything else,
    /// including a `WITH` that writes, is refused unrun.
    pub fn query(&self, sql: &str, params: &[Value], time_limit: Duration) -> Result<Rows> {
        let refused = || {
            Error::Refused("ledger.query runs a single read-only SELECT or WITH statement".into())
        };
        let mut rest = sql.trim_start();
        loop {
            if let Some(r) = rest.strip_prefix("--") {
                rest = r.split_once('\n').map_or("", |(_, r)| r).trim_start();
            } else if let Some(r) = rest.strip_prefix("/*") {
                rest = r.split_once("*/").map_or("", |(_, r)| r).trim_start();
            } else {
                break;
            }
        }
        let keyword: String = rest.chars().take_while(char::is_ascii_alphabetic).collect();
        if !keyword.eq_ignore_ascii_case("select") && !keyword.eq_ignore_ascii_case("with") {
            return Err(refused());
        }
        let mut stmt = match self.conn.prepare(sql) {
            Err(rusqlite::Error::MultipleStatement) => return Err(refused()),
            stmt => stmt?,
        };
        if !stmt.readonly() {
            return Err(refused());
        }
        let params = params
            .iter()
            .map(|p| match p {
                Value::Null => Ok(SqlValue::Null),
                Value::Bool(b) => Ok(SqlValue::Integer(i64::from(*b))),
                Value::Number(n) => Ok(n.as_i64().map_or_else(
                    || SqlValue::Real(n.as_f64().unwrap_or(f64::NAN)),
                    SqlValue::Integer,
                )),
                Value::String(s) => Ok(SqlValue::Text(s.clone())),
                _ => Err(Error::Refused(
                    "query parameters are null, bool, number or string".into(),
                )),
            })
            .collect::<Result<Vec<_>>>()?;
        let columns = stmt.column_names().into_iter().map(String::from).collect();
        let width = stmt.column_count();
        let to_json = |v: ValueRef<'_>| match v {
            ValueRef::Null => Value::Null,
            ValueRef::Integer(n) => n.into(),
            ValueRef::Real(x) => serde_json::Number::from_f64(x).map_or(Value::Null, Value::Number),
            ValueRef::Text(t) => String::from_utf8_lossy(t).into(),
            ValueRef::Blob(b) => b
                .iter()
                .map(|x| format!("{x:02x}"))
                .collect::<String>()
                .into(),
        };

        // rusqlite's progress handler needs its `hooks` feature, so a watchdog thread interrupts the statement at the
        // deadline instead, unless `done` is dropped first. Both end the statement with SQLITE_INTERRUPT.
        let interrupt = self.conn.get_interrupt_handle();
        let (done, wait) = mpsc::channel::<()>();
        let rows: rusqlite::Result<Vec<Vec<Value>>> = thread::scope(|scope| {
            scope.spawn(move || {
                if wait.recv_timeout(time_limit) == Err(RecvTimeoutError::Timeout) {
                    interrupt.interrupt();
                }
            });
            let rows = stmt
                .query_map(rusqlite::params_from_iter(&params), |row| {
                    (0..width).map(|i| row.get_ref(i).map(to_json)).collect()
                })
                .and_then(|rows| rows.collect());
            drop(done);
            rows
        });
        match rows {
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == ErrorCode::OperationInterrupted =>
            {
                Err(Error::TimeLimit)
            }
            rows => Ok(Rows {
                columns,
                rows: rows?,
            }),
        }
    }
}

fn configure(conn: &Connection) -> Result<()> {
    conn.busy_timeout(BUSY_TIMEOUT)?;
    conn.pragma_update(None, "foreign_keys", true)?;
    Ok(())
}

fn refuse_newer(version: i64) -> Result<()> {
    if version > SCHEMA_VERSION {
        return Err(Error::Refused(format!(
            "the ledger has schema version {version}, newer than this quen's {SCHEMA_VERSION}"
        )));
    }
    Ok(())
}

/// Copies the ledger at `path`, at schema version `version`, to `backups/quen-v<version>-<UTC time>.db` beside it, and
/// flushes the copy to disk. `conn` holds the write lock, so the copy is the ledger exactly as the migrations find it;
/// `VACUUM INTO` can't run inside that transaction, so a second connection makes it. The file is created new, so no
/// backup is ever replaced; when the copy fails, the empty or partial file this call created is removed, since it would
/// pass for a backup, and the open is refused.
fn back_up(conn: &Connection, path: &Path, version: i64) -> Result<()> {
    let refused = |e: &dyn fmt::Display| {
        Error::Refused(format!(
            "the ledger at schema version {version} must be backed up before it migrates, and the backup failed: {e}"
        ))
    };
    let dir = path.with_file_name("backups");
    fs::create_dir_all(&dir).map_err(|e| refused(&format!("{}: {e}", dir.display())))?;
    let t: String = conn.query_row("SELECT strftime('%Y%m%dT%H%M%SZ', 'now')", [], |r| r.get(0))?;
    let to = dir.join(format!("quen-v{version}-{t}.db"));
    let name = to
        .to_str()
        .ok_or_else(|| refused(&format!("{} isn't valid Unicode", to.display())))?;
    let file = File::options()
        .write(true)
        .create_new(true)
        .open(&to)
        .map_err(|e| refused(&format!("{}: {e}", to.display())))?;
    let copy = || -> std::result::Result<(), Box<dyn std::error::Error>> {
        let source = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        source.busy_timeout(BUSY_TIMEOUT)?;
        source.execute("VACUUM INTO ?1", [name])?;
        Ok(file.sync_all()?)
    };
    let copied = copy();
    drop(file);
    copied.map_err(|e| {
        let _ = fs::remove_file(&to);
        refused(&format!("{}: {e}", to.display()))
    })
}
