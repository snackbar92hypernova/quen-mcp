//! Typed inserts. The row types deserialize from JSON for `ledger.ingest` and `quen ledger add`, refusing unknown
//! fields. Times are UTC `YYYY-MM-DDTHH:MM:SSZ`; the schema refuses any other spelling.

use rusqlite::types::{ToSql, ToSqlOutput};
use rusqlite::{OptionalExtension, params};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value, json};

use super::{Error, Ledger, Result};
use crate::hdr::Encoding;
use crate::journal::sha256_hex;
use crate::mods::{self, Kind, ModEntry};
use crate::sweep::{self, Mode};

/// A string enum stored as its name; the schema's CHECK lists the same names (tested in tests/ledger_db.rs).
macro_rules! sql_enum {
    ($(#[$m:meta])* $name:ident { $($(#[$vm:meta])* $var:ident = $s:literal,)+ }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        pub enum $name {
            $($(#[$vm])* #[serde(rename = $s)] $var,)+
        }

        impl $name {
            pub const ALL: &[$name] = &[$($name::$var),+];

            pub fn as_str(self) -> &'static str {
                match self {
                    $($name::$var => $s,)+
                }
            }
        }

        impl ToSql for $name {
            fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
                Ok(self.as_str().into())
            }
        }
    };
}

sql_enum! {
    /// How a boot ended.
    Outcome {
        /// Ended normally.
        Ok = "ok",
        /// An error ended it; [`BootEnd::failure`] says what.
        Failed = "failed",
        /// Closed by journal replay after quen or the PC died.
        Recovered = "recovered",
    }
}

sql_enum! {
    /// A measurement's evidence tag. Only M, O and static_re count towards "proven".
    Tag {
        /// Measured.
        M = "M",
        /// Observed in code, files or tool schemas.
        O = "O",
        /// Inferred.
        I = "I",
        /// Unverified.
        U = "U",
        /// A static fact from reverse engineering.
        StaticRe = "static_re",
    }
}

sql_enum! {
    /// Where a build's version label comes from. An entry whose build has no label has provenance `unknown`.
    Provenance {
        /// One of our reproducible builds; the source is its git commit.
        GitCommit = "git_commit",
        /// A third-party download; the source is the Nexus file name.
        NexusFile = "nexus_file",
    }
}

sql_enum! {
    /// What a mod did at load in a boot, from its log or state export.
    LoadStatus {
        Loaded = "loaded",
        /// It refused to load, e.g. because the game code differs; [`BootModStatus::reason`] says why.
        Refused = "refused",
        /// It wasn't in the game: parked, or missing.
        NotPresent = "not_present",
    }
}

/// A capture's encoding is stored by its serde name, which `hdr` also gives as its Display.
impl ToSql for Encoding {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(self.to_string().into())
    }
}

/// A mod kind is stored by its serde name, which the schema's CHECKs list.
impl ToSql for Kind {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(self.as_str().into())
    }
}

/// A sweep run's playback mode, likewise.
impl ToSql for Mode {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(self.as_str().into())
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Game {
    pub name: String,
    pub exe_path: String,
    pub exe_sha256: String,
    pub build_stamp: Option<String>,
}

/// A build's label: the mod and version its content hash stands for, and where that is known from.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModVersion {
    pub content_hash: String,
    pub kind: Kind,
    pub name: String,
    pub version: String,
    pub provenance: Provenance,
    /// The git commit, or the Nexus file name.
    pub source: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BootModStatus {
    pub boot_id: i64,
    pub kind: Kind,
    pub name: String,
    pub status: LoadStatus,
    /// Required for [`LoadStatus::Refused`]: the log line or export field that says why.
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Save {
    pub game_id: i64,
    pub file: String,
    pub sha256: String,
    pub note: Option<String>,
    /// The boot whose memory patch, cvar write or teleport came before the game wrote this save.
    pub tainted_by_boot: Option<i64>,
    /// The boot whose game_save made it (schema 0010). Only quen sets it: ledger_ingest can't.
    #[serde(skip)]
    pub made_by_boot: Option<i64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BootStart {
    pub game_id: i64,
    pub t_start: String,
    pub save_id: Option<i64>,
    pub mod_set_id: Option<i64>,
    pub settings_diff: Option<Map<String, Value>>,
    pub devkit: Option<bool>,
    pub rebar: Option<bool>,
    pub fg: Option<bool>,
    pub vsync: Option<bool>,
    pub profiler: Option<String>,
    pub display_state: Option<Map<String, Value>>,
    pub journal_path: Option<String>,
    /// The Python harness's info dict, for a boot it ran (schema 0003).
    pub harness_info: Option<Map<String, Value>>,
    /// What the quiet-machine check at the launch found running that skews the numbers (schema 0005): empty when the
    /// machine was quiet, None when no check ran. Later checks add to it ([`Ledger::record_quiet_check`]).
    pub busy_with: Option<Vec<String>>,
    /// `play` for a play session (schema 0011); None for a test launch. Only quen sets it: ledger_ingest can't.
    #[serde(skip)]
    pub mode: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BootEnd {
    pub t_end: String,
    pub outcome: Outcome,
    pub failure: Option<String>,
    /// Every memory patch and data write of the boot, as the journal recorded them.
    #[serde(default)]
    pub patches: Vec<Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capture {
    pub boot_id: Option<i64>,
    /// Relative to the capture root, either separator.
    pub path: String,
    pub sha256: String,
    pub bytes: i64,
    pub kind: String,
    pub method: Option<String>,
    pub encoding: Option<Encoding>,
    #[serde(default, deserialize_with = "flag")]
    pub includes_overlay: Option<bool>,
    #[serde(default, deserialize_with = "flag")]
    pub includes_desktop: Option<bool>,
    pub display_state: Option<Map<String, Value>>,
    pub frame_state: Option<Map<String, Value>>,
    pub pose_id: Option<i64>,
    pub t: String,
    #[serde(default, deserialize_with = "flag_or_false")]
    pub pinned: bool,
}

/// A flag column: true or false, or 1 or 0 as `ledger_query` shows it (feature tests 2c: a copied row was refused).
fn flag<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<Option<bool>, D::Error> {
    match Option::<Value>::deserialize(d)? {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(b)),
        Some(v) if v == 0 => Ok(Some(false)),
        Some(v) if v == 1 => Ok(Some(true)),
        Some(v) => Err(D::Error::custom(format!(
            "{v}: expected true, false, 1 or 0"
        ))),
    }
}

fn flag_or_false<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<bool, D::Error> {
    Ok(flag(d)?.unwrap_or(false))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Measurement {
    pub boot_id: Option<i64>,
    pub capture_id: Option<i64>,
    pub metric: String,
    pub value: Option<f64>,
    pub unit: Option<String>,
    pub state_label: Option<String>,
    /// A lowercase identifier (`presentmon`, `tracy`, …); claims count distinct families as independent methods.
    pub method_family: String,
    pub tag: Tag,
    pub run_id: Option<String>,
    pub note: Option<String>,
    /// The extraction a static_re (or decompiler) row was read from (schema 0009).
    #[serde(default)]
    pub extraction_id: Option<i64>,
    /// What the world did during the row's window (schema 0011): user, live, paused or photo.
    #[serde(default)]
    pub world: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Claim {
    pub text: String,
    pub scope: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pose {
    pub name: String,
    pub camera: [f64; 3],
    /// Pitch, yaw, roll in degrees.
    pub rotation: [f64; 3],
    pub fov: f64,
    pub player: Option<PlayerPose>,
    pub save_id: Option<i64>,
    pub ref_capture_id: Option<i64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlayerPose {
    pub position: [f64; 3],
    /// Degrees; none: not given.
    pub yaw: Option<f64>,
}

/// A camera sweep, stored canonical.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sweep {
    pub name: String,
    pub sweep: sweep::Sweep,
    /// The save row its world loads.
    pub save_id: Option<i64>,
    /// The pose row each keyframe was recorded as, in keyframe order and null where none; or empty.
    #[serde(default)]
    pub poses: Vec<Option<i64>>,
    pub note: Option<String>,
}

/// One playback of a sweep.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SweepRun {
    pub sweep_id: i64,
    pub boot_id: Option<i64>,
    pub mode: Mode,
    /// Frame step's frames per second; none for real time.
    pub fps: Option<u32>,
    pub t: String,
}

/// A capture that is frame `frame` of a frame-step run.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SweepFrame {
    pub run_id: i64,
    pub frame: u32,
    pub capture_id: i64,
}

/// What an RE knowledge-base path backs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReTarget {
    Measurement(i64),
    Claim(i64),
}

/// One row for `ledger_ingest` and `quen ledger add`, named by its table: `{"table": "measurements", ...fields}`.
/// Boots, games and mod sets are not here: the front records those itself.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "table", deny_unknown_fields)]
pub enum Row {
    #[serde(rename = "measurements")]
    Measurement(Measurement),
    #[serde(rename = "captures")]
    Capture(Capture),
    #[serde(rename = "saves")]
    Save(Save),
    #[serde(rename = "poses")]
    Pose(Pose),
    #[serde(rename = "mod_versions")]
    ModVersion(ModVersion),
    #[serde(rename = "boot_mod_status")]
    BootModStatus(BootModStatus),
    #[serde(rename = "re_refs")]
    ReRef { target: ReTarget, kb_path: String },
    #[serde(rename = "sweeps")]
    Sweep(Sweep),
    #[serde(rename = "sweep_runs")]
    SweepRun(SweepRun),
    #[serde(rename = "sweep_frames")]
    SweepFrame(SweepFrame),
}

impl Row {
    /// `row` as a row, or why not, naming the field: serde names none for a wrong type (feature tests 2c). Each field
    /// is tried alone with the table: the first refused for more than another field missing is the one (none when the
    /// table alone is).
    pub fn from_json(row: &Value) -> std::result::Result<Row, String> {
        let e = match serde_json::from_value::<Row>(row.clone()) {
            Ok(row) => return Ok(row),
            Err(e) => e.to_string(),
        };
        let (Some(fields), Some(table)) = (row.as_object(), row.get("table")) else {
            return Err(e);
        };
        let refused = |row: Value| {
            serde_json::from_value::<Row>(row)
                .is_err_and(|e| !e.to_string().starts_with("missing field"))
        };
        if refused(json!({"table": table})) {
            return Err(e);
        }
        let field = fields
            .iter()
            .filter(|(k, _)| *k != "table")
            .find(|(k, v)| refused(json!({"table": table, k.as_str(): v})));
        match field {
            Some((k, _)) if !e.contains(&format!("`{k}`")) => Err(format!("{k}: {e}")),
            _ => Err(e),
        }
    }

    /// Only the holder of the game lock writes rows about a boot (captures, measurements, load status, sweep
    /// runs and their frames).
    pub fn needs_lock(&self) -> bool {
        matches!(
            self,
            Row::Measurement(_)
                | Row::Capture(_)
                | Row::BootModStatus(_)
                | Row::SweepRun(_)
                | Row::SweepFrame(_)
        )
    }
}

/// JSON text for a JSON column (serializing serde_json values can't fail).
fn json<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

impl Ledger {
    /// Writes `rows` in order, all or none: each row's id, or None for a table without ids (labels, load status). A
    /// refusal names the first row that failed, counting from 0.
    pub fn ingest(&mut self, rows: &[Row]) -> Result<Vec<Option<i64>>> {
        self.atomic(|l| {
            let mut ids = Vec::with_capacity(rows.len());
            for (i, row) in rows.iter().enumerate() {
                let id = match row {
                    Row::Measurement(m) => l.add_measurement(m).map(Some),
                    Row::Capture(c) => l.add_capture(c).map(Some),
                    Row::Save(s) => l.add_save(s).map(Some),
                    Row::Pose(p) => l.add_pose(p).map(Some),
                    Row::ModVersion(v) => l.add_mod_version(v).map(|()| None),
                    Row::BootModStatus(s) => l.add_boot_mod_status(s).map(|()| None),
                    Row::ReRef { target, kb_path } => l.add_re_ref(*target, kb_path).map(Some),
                    Row::Sweep(s) => l.add_sweep(s).map(Some),
                    Row::SweepRun(r) => l.add_sweep_run(r).map(Some),
                    Row::SweepFrame(f) => l.add_sweep_frame(f).map(|()| None),
                };
                ids.push(id.map_err(|e| {
                    Error::Refused(match e {
                        Error::Refused(why) => format!("row {i}: {why}"),
                        e => format!("row {i}: {e}"),
                    })
                })?);
            }
            Ok(ids)
        })
    }

    /// The id of the game with this exe sha256, added on first sight (later name and path changes are not recorded).
    pub fn add_game(&mut self, g: &Game) -> Result<i64> {
        Ok(self.conn.query_row(
            "INSERT INTO games (name, exe_path, exe_sha256, build_stamp) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (exe_sha256) DO UPDATE SET exe_sha256 = excluded.exe_sha256 RETURNING id",
            params![g.name, g.exe_path, g.exe_sha256, g.build_stamp],
            |r| r.get(0),
        )?)
    }

    /// The id of the mod set with exactly these entries, in any order, added on first sight. [`mods::set_hash`] names
    /// the set; its refusals (a repeated identity, a name it can't hash unambiguously) come back as refusals. A new
    /// entry whose content hash is in `mod_versions` gets that label; any other gets none, with provenance `unknown`.
    pub fn add_mod_set(&mut self, entries: &[ModEntry]) -> Result<i64> {
        let sha = mods::set_hash(entries).map_err(|e| Error::Refused(e.to_string()))?;
        self.atomic(|l| {
            let existing = l.conn.query_row("SELECT id FROM mod_sets WHERE sha256 = ?1", [&sha], |r| {
                r.get(0)
            });
            let id = match existing {
                Ok(id) => id,
                Err(rusqlite::Error::QueryReturnedNoRows) => {
                    l.conn.execute("INSERT INTO mod_sets (sha256) VALUES (?1)", [&sha])?;
                    let id = l.conn.last_insert_rowid();
                    for e in entries {
                        l.conn.execute(
                            "INSERT INTO mod_set_entries (mod_set_id, kind, name, parked, home, content_hash, file_count,
                               bytes, config_hash, config_values, version_label, provenance)
                             SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, v.version, coalesce(v.provenance, 'unknown')
                             FROM (SELECT 1) LEFT JOIN mod_versions v ON v.content_hash = ?6",
                            params![
                                id,
                                e.kind,
                                e.name,
                                e.parked,
                                e.home.to_string_lossy(),
                                e.content_hash,
                                e.file_count,
                                e.bytes,
                                e.config_hash,
                                json(&e.config_values),
                            ],
                        )?;
                    }
                    id
                }
                Err(e) => return Err(e.into()),
            };
            Ok(id)
        })
    }

    /// Labels a build: from now on its content hash stands for this mod and version, and the stored entries with that
    /// hash and no label get it. The same label again changes nothing; a different one for a labeled hash is refused.
    pub fn add_mod_version(&mut self, v: &ModVersion) -> Result<()> {
        let row = params![
            v.content_hash,
            v.kind,
            v.name,
            v.version,
            v.provenance,
            v.source
        ];
        self.atomic(|l| {
            l.conn.execute(
                "INSERT INTO mod_versions (content_hash, kind, name, version, provenance, source)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT (content_hash) DO NOTHING",
                row,
            )?;
            let same: bool = l.conn.query_row(
                "SELECT count(*) > 0 FROM mod_versions WHERE content_hash = ?1 AND kind = ?2 AND name = ?3
                   AND version = ?4 AND provenance = ?5 AND source = ?6",
                row,
                |r| r.get(0),
            )?;
            if !same {
                return Err(Error::Refused(format!(
                    "build {} already has a different label",
                    v.content_hash
                )));
            }
            l.conn.execute(
                "UPDATE mod_set_entries SET version_label = ?2, provenance = ?3
                 WHERE content_hash = ?1 AND provenance = 'unknown'",
                params![v.content_hash, v.version, v.provenance],
            )?;
            Ok(())
        })
    }

    /// Records what a mod did at load in a boot.
    pub fn add_boot_mod_status(&mut self, s: &BootModStatus) -> Result<()> {
        self.conn.execute(
            "INSERT INTO boot_mod_status (boot_id, kind, name, status, reason) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![s.boot_id, s.kind, s.name, s.status, s.reason],
        )?;
        Ok(())
    }

    /// The id of this save (game, file name and sha256), added on first sight. Seeing it again fills a note or taint
    /// the row lacks and never clears one.
    pub fn add_save(&mut self, s: &Save) -> Result<i64> {
        Ok(self.conn.query_row(
            "INSERT INTO saves (game_id, file, sha256, note, tainted_by_boot, made_by_boot)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (game_id, file, sha256) DO UPDATE SET note = coalesce(note, excluded.note),
               tainted_by_boot = coalesce(tainted_by_boot, excluded.tainted_by_boot),
               made_by_boot = coalesce(made_by_boot, excluded.made_by_boot)
             RETURNING id",
            params![
                s.game_id,
                s.file,
                s.sha256,
                s.note,
                s.tainted_by_boot,
                s.made_by_boot
            ],
            |r| r.get(0),
        )?)
    }

    pub fn start_boot(&mut self, b: &BootStart) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO boots (game_id, t_start, save_id, mod_set_id, settings_diff, devkit, rebar, fg, vsync,
               profiler, display_state, journal_path, harness_info, busy_with, mode)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                b.game_id,
                b.t_start,
                b.save_id,
                b.mod_set_id,
                b.settings_diff.as_ref().map(json),
                b.devkit,
                b.rebar,
                b.fg,
                b.vsync,
                b.profiler,
                b.display_state.as_ref().map(json),
                b.journal_path,
                b.harness_info.as_ref().map(json),
                b.busy_with.as_ref().map(json),
                b.mode,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Records a quiet-machine check made during a boot: `busy_with` is what it found running that skews the numbers,
    /// empty when the machine was quiet. The boot's list only grows (sorted, once each), so a boot busy at any check
    /// stays busy. Refuses an unknown boot or one already finished.
    pub fn record_quiet_check(&mut self, boot_id: i64, busy_with: &[String]) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE boots SET busy_with = (
               SELECT json_group_array(value) FROM (
                 SELECT value FROM json_each(coalesce(boots.busy_with, '[]'))
                 UNION SELECT value FROM json_each(?2)
                 ORDER BY value))
             WHERE id = ?1 AND t_end IS NULL",
            params![boot_id, json(&busy_with)],
        )?;
        self.changed(n, "boot", boot_id, "has already finished")
    }

    /// Records how a boot ended. Refuses an unknown boot or one already finished.
    pub fn finish_boot(&mut self, boot_id: i64, end: &BootEnd) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE boots SET t_end = ?2, outcome = ?3, failure = ?4, patches = ?5 WHERE id = ?1 AND t_end IS NULL",
            params![boot_id, end.t_end, end.outcome, end.failure, json(&end.patches)],
        )?;
        self.changed(n, "boot", boot_id, "has already finished")
    }

    /// Records why a boot's end failed and its patches, leaving the boot open: its journal stayed, and the recovery that
    /// replays it closes the boot. Refuses an unknown boot or one already finished.
    pub fn hold_boot(&mut self, boot_id: i64, failure: &str, patches: &[Value]) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE boots SET failure = ?2, patches = ?3 WHERE id = ?1 AND t_end IS NULL",
            params![boot_id, failure, json(&patches)],
        )?;
        self.changed(n, "boot", boot_id, "has already finished")
    }

    /// Records that an unfinished boot's devkit is loaded in the game, with the boot's settings diff, whose conditions
    /// then name it: the launch checks the game's modules first (feature tests 2b). Refuses an unknown or finished boot.
    pub fn set_boot_devkit(
        &mut self,
        boot_id: i64,
        settings_diff: &Map<String, Value>,
    ) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE boots SET devkit = 1, settings_diff = ?2 WHERE id = ?1 AND t_end IS NULL",
            params![boot_id, json(settings_diff)],
        )?;
        self.changed(n, "boot", boot_id, "has already finished")
    }

    /// Records the mod set a started boot loaded, once. It is taken after the parking, so only once the boot
    /// ran. Refuses an unknown boot or one that has its set.
    pub fn set_boot_mod_set(&mut self, boot_id: i64, mod_set_id: i64) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE boots SET mod_set_id = ?2 WHERE id = ?1 AND mod_set_id IS NULL",
            params![boot_id, mod_set_id],
        )?;
        self.changed(n, "boot", boot_id, "has its mod set already")
    }

    /// Records that the user has looked at a finished boot's outputs at `t`, which lets its captures become cleanup
    /// candidates. Refuses an unknown or unfinished boot.
    pub fn review(&mut self, boot_id: i64, t: &str) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE boots SET reviewed_at = ?2 WHERE id = ?1 AND t_end IS NOT NULL",
            params![boot_id, t],
        )?;
        self.changed(n, "boot", boot_id, "hasn't finished")
    }

    /// Adds a capture file. Its path is stored with `/` separators; an absolute path, a drive, `..` or a trailing
    /// separator is refused, since the path must name a file under the capture root.
    pub fn add_capture(&mut self, c: &Capture) -> Result<i64> {
        let path = c.path.replace('\\', "/");
        let segments: Vec<&str> = path
            .split('/')
            .filter(|s| !s.is_empty() && *s != ".")
            .collect();
        if path.starts_with('/')
            || path.ends_with('/')
            || path.contains(':')
            || segments.is_empty()
            || segments.contains(&"..")
        {
            return Err(Error::Refused(format!(
                "capture path {:?} must be a file relative to the capture root",
                c.path
            )));
        }
        self.conn.execute(
            "INSERT INTO captures (boot_id, path, sha256, bytes, kind, method, encoding, includes_overlay,
               includes_desktop, display_state, frame_state, pose_id, t, pinned)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                c.boot_id,
                segments.join("/"),
                c.sha256,
                c.bytes,
                c.kind,
                c.method,
                c.encoding,
                c.includes_overlay,
                c.includes_desktop,
                c.display_state.as_ref().map(json),
                c.frame_state.as_ref().map(json),
                c.pose_id,
                c.t,
                c.pinned,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn add_measurement(&mut self, m: &Measurement) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO measurements (boot_id, capture_id, metric, value, unit, state_label, method_family, tag,
               run_id, note, extraction_id, world)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                m.boot_id,
                m.capture_id,
                m.metric,
                m.value,
                m.unit,
                m.state_label,
                m.method_family,
                m.tag,
                m.run_id,
                m.note,
                m.extraction_id,
                m.world,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Adds an open claim. Whether it is proven is read from the `claims_status` view.
    pub fn add_claim(&mut self, c: &Claim) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO claims (text, scope) VALUES (?1, ?2)",
            params![c.text, c.scope],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Records that a measurement supports (`true`) or contradicts (`false`) a claim. Refuses an unknown one by name,
    /// and a measurement already recorded for the claim (two agents citing the same evidence at once: the first stays).
    pub fn support(&mut self, claim_id: i64, measurement_id: i64, supports: bool) -> Result<()> {
        self.known("claim", claim_id)?;
        self.known("measurement", measurement_id)?;
        let n = self.conn.execute(
            "INSERT INTO claim_support (claim_id, measurement_id, supports) VALUES (?1, ?2, ?3)
             ON CONFLICT DO NOTHING",
            params![claim_id, measurement_id, supports],
        )?;
        if n == 0 {
            return Err(Error::Refused(format!(
                "measurement {measurement_id} is already recorded for claim {claim_id}"
            )));
        }
        Ok(())
    }

    /// Withdraws an open claim, keeping the row and the reason. Refuses an unknown or already withdrawn claim.
    pub fn withdraw(&mut self, claim_id: i64, reason: &str) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE claims SET status = 'withdrawn', withdrawn_reason = ?2 WHERE id = ?1 AND status = 'open'",
            params![claim_id, reason],
        )?;
        self.changed(n, "claim", claim_id, "is already withdrawn")
    }

    /// Refuses an id the `<noun>s` table doesn't hold, by name ("no capture 9"), rather than SQLite's foreign key error.
    fn known(&self, noun: &str, id: i64) -> Result<()> {
        let found: bool = self.conn.query_row(
            &format!("SELECT count(*) > 0 FROM {noun}s WHERE id = ?1"),
            [id],
            |r| r.get(0),
        )?;
        if !found {
            return Err(Error::Refused(format!("no {noun} {id}")));
        }
        Ok(())
    }

    /// Adds a pose. Refuses an unknown reference capture by name.
    pub fn add_pose(&mut self, p: &Pose) -> Result<i64> {
        if let Some(id) = p.ref_capture_id {
            self.known("capture", id)?;
        }
        let player = p.player.as_ref();
        self.conn.execute(
            "INSERT INTO poses (name, cam_x, cam_y, cam_z, cam_pitch, cam_yaw, cam_roll, fov, player_x, player_y,
               player_z, player_yaw, save_id, ref_capture_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                p.name,
                p.camera[0],
                p.camera[1],
                p.camera[2],
                p.rotation[0],
                p.rotation[1],
                p.rotation[2],
                p.fov,
                player.map(|q| q.position[0]),
                player.map(|q| q.position[1]),
                player.map(|q| q.position[2]),
                player.and_then(|q| q.yaw),
                p.save_id,
                p.ref_capture_id,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Links a measurement or claim to a path in the RE knowledge base.
    pub fn add_re_ref(&mut self, target: ReTarget, kb_path: &str) -> Result<i64> {
        let (measurement_id, claim_id) = match target {
            ReTarget::Measurement(id) => (Some(id), None),
            ReTarget::Claim(id) => (None, Some(id)),
        };
        self.conn.execute(
            "INSERT INTO re_refs (measurement_id, claim_id, kb_path) VALUES (?1, ?2, ?3)",
            params![measurement_id, claim_id, kb_path],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// The id of the sweep with this name, added on first sight: canonical and checked, named by the sha256 of its
    /// JSON, with the pose each keyframe was recorded as. The same name with the same sweep again returns that id and
    /// changes nothing; with another sweep it is refused, since a run plays exactly the row it names.
    pub fn add_sweep(&mut self, s: &Sweep) -> Result<i64> {
        let sweep = s
            .sweep
            .clone()
            .canonical()
            .map_err(|e| Error::Refused(e.to_string()))?;
        if !s.poses.is_empty() && s.poses.len() != sweep.keyframes.len() {
            return Err(Error::Refused(format!(
                "{} poses for {} keyframes: one per keyframe (null for none), or none",
                s.poses.len(),
                sweep.keyframes.len()
            )));
        }
        let json = sweep.to_json();
        let sha = sha256_hex(json.as_bytes());
        self.atomic(|l| {
            let existing: Option<(i64, String)> = l
                .conn
                .query_row(
                    "SELECT id, sha256 FROM sweeps WHERE name = ?1",
                    [&s.name],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            match existing {
                Some((id, old)) if old == sha => return Ok(id),
                Some(_) => {
                    return Err(Error::Refused(format!(
                        "a sweep named {:?} has other keyframes or settings; sweeps never change, so name this one anew",
                        s.name
                    )));
                }
                None => {}
            }
            l.conn.execute(
                "INSERT INTO sweeps (name, sha256, sweep, save_id, note) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![s.name, sha, json, s.save_id, s.note],
            )?;
            let id = l.conn.last_insert_rowid();
            for (keyframe, pose) in s.poses.iter().enumerate() {
                if let Some(pose) = pose {
                    l.conn.execute(
                        "INSERT INTO sweep_keyframes (sweep_id, keyframe, pose_id) VALUES (?1, ?2, ?3)",
                        params![id, keyframe, pose],
                    )?;
                }
            }
            Ok(id)
        })
    }

    /// Records one playback of a sweep. Refuses frame step without a frame rate the game takes, and real time
    /// with one.
    pub fn add_sweep_run(&mut self, r: &SweepRun) -> Result<i64> {
        sweep::check_playback(r.mode, r.fps).map_err(|e| Error::Refused(e.to_string()))?;
        self.conn.execute(
            "INSERT INTO sweep_runs (sweep_id, boot_id, mode, fps, t) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![r.sweep_id, r.boot_id, r.mode, r.fps, r.t],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Records a capture as one frame of a frame-step run. Refuses a real-time run, and a frame past the run's last
    /// (`Path::frame_count` of its sweep at its fps).
    pub fn add_sweep_frame(&mut self, f: &SweepFrame) -> Result<()> {
        self.atomic(|l| {
            let run: Option<(Option<u32>, String)> = l
                .conn
                .query_row(
                    "SELECT r.fps, s.sweep FROM sweep_runs r JOIN sweeps s ON s.id = r.sweep_id WHERE r.id = ?1",
                    [f.run_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let Some((fps, json)) = run else {
                return Err(Error::Refused(format!("no sweep run {}", f.run_id)));
            };
            let Some(fps) = fps else {
                return Err(Error::Refused(format!(
                    "sweep run {} played in real time: it has no frames",
                    f.run_id
                )));
            };
            let frames = sweep::Sweep::from_json(&json)
                .and_then(|s| s.path())
                .and_then(|p| p.frame_count(fps))
                .map_err(|e| Error::Refused(format!("sweep run {}: {e}", f.run_id)))?;
            if f.frame >= frames {
                return Err(Error::Refused(format!(
                    "frame {} of sweep run {}: its frames are 0 to {}",
                    f.frame,
                    f.run_id,
                    frames - 1
                )));
            }
            l.conn.execute(
                "INSERT INTO sweep_frames (run_id, frame, capture_id) VALUES (?1, ?2, ?3)",
                params![f.run_id, f.frame, f.capture_id],
            )?;
            Ok(())
        })
    }

    /// Ok when an UPDATE changed its row; otherwise refuses, saying whether the row is missing or `why` it's unchanged.
    pub(super) fn changed(&self, rows: usize, noun: &str, id: i64, why: &str) -> Result<()> {
        if rows > 0 {
            return Ok(());
        }
        let found: bool = self.conn.query_row(
            &format!("SELECT count(*) > 0 FROM {noun}s WHERE id = ?1"),
            [id],
            |r| r.get(0),
        )?;
        Err(Error::Refused(if found {
            format!("{noun} {id} {why}")
        } else {
            format!("no {noun} {id}")
        }))
    }
}
