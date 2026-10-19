//! Upgrades keep a tester's ledger across quen updates: shipped migrations are pinned, an existing
//! ledger is backed up before its first migration, and a ledger from every older version comes through with its rows.

mod ledger_common;

use std::fs;
use std::path::{Path, PathBuf};

use ledger_common::{TempDb, rows, sha};
use quen_core::journal::sha256_hex;
use quen_core::ledger::{Error, Ledger, SCHEMA_VERSION};
use rusqlite::types::Value as Sql;
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};

/// The sha256 of each shipped migration. Shipped migrations are never edited, only added (AGENTS.md): a fix is a new
/// migration, since testers' ledgers already ran the old text.
const SHIPPED: &[(&str, &str)] = &[
    (
        "0001_init.sql",
        "d59e2bfe01d4e424b4b762b6b856a34ddcac609c6f7784d6c58f7050a129176e",
    ),
    (
        "0002_mods.sql",
        "d582f92826a3aeff0799a4ad2b0271cab710a5b9b84f537ea62a0f184103b823",
    ),
    (
        "0003_harness_info.sql",
        "2d1e749a596a6b7d7f1368bb558677b78e5ca83e883238adeeee29e17348c7ba",
    ),
    (
        "0004_claims_strict.sql",
        "858b23a26b8e941369a44131aad9334c67b7d91a96ed9ad56097c5388435a9c3",
    ),
    (
        "0005_views.sql",
        "93b0da48782b248cfb85e3165c1ffb699f4733c8315a74a28f9de7264c7e0df9",
    ),
    (
        "0006_sweeps.sql",
        "6ee92f09deaab8daf0dc3687a54b09b9c88f70a2f00fedfc2f521d063f4194d3",
    ),
    (
        "0007_captures_removed.sql",
        "b5f0799a033c883f4605d41da5a9fb5ddd212c079dcaea89fd760caa03d044a5",
    ),
    (
        "0008_captures_missing.sql",
        "24c18d726a6629db17c77adb36ffda2b52119be55654745273504334bc5acc8f",
    ),
    (
        "0009_knowledge.sql",
        "66459063e3b5d04f4cde2cb734c6928de272c53f2f750637cf3448432dce0898",
    ),
    (
        "0010_saves_made_by_boot.sql",
        "8b1310514ed525f54c7a27102b031989cff06dfbafdb1eaf3c7eb1cbd1c2f323",
    ),
    (
        "0011_play_sessions.sql",
        "681ebdee3fd14f168e3c9fd9f95bb6dd3d9eba9b908c158dde665beeb564d254",
    ),
];

/// The migrations in `schema/`, in order: (file name, text).
fn migrations() -> Vec<(String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schema");
    let mut files: Vec<(String, String)> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.ends_with(".sql"))
        .map(|name| {
            let text = fs::read_to_string(dir.join(&name)).unwrap();
            (name, text)
        })
        .collect();
    files.sort();
    files
}

#[test]
fn shipped_migrations_are_never_edited() {
    let files = migrations();
    for (i, (name, text)) in files.iter().enumerate() {
        assert!(
            name.starts_with(&format!("{:04}_", i + 1)),
            "schema/{name}: migrations are numbered 0001, 0002, ... without gaps"
        );
        let sha = sha256_hex(text.as_bytes());
        match SHIPPED.iter().find(|(pinned, _)| pinned == name) {
            Some((_, pin)) => assert_eq!(
                &sha, pin,
                "schema/{name} changed after it shipped: restore it and put the change in a new migration"
            ),
            None => panic!(
                "schema/{name} has no pin: add (\"{name}\", \"{sha}\") to SHIPPED; from then on it never changes"
            ),
        }
    }
    assert_eq!(files.len(), SHIPPED.len(), "a pinned migration is gone");
    assert_eq!(
        files.len() as i64,
        SCHEMA_VERSION,
        "ledger.rs's MIGRATIONS embeds every file in schema/"
    );
}

/// A ledger at schema version `k` as a quen of that version left it: the first `k` migrations, WAL, and synthetic rows
/// in every table it has. The connection stays open, so the rows may still be in the WAL.
fn old_ledger(path: &Path, k: usize) -> Connection {
    let c = Connection::open(path).unwrap();
    for (_, sql) in &migrations()[..k] {
        c.execute_batch(sql).unwrap();
    }
    c.pragma_update(None, "user_version", k as i64).unwrap();
    c.pragma_update_and_check(None, "journal_mode", "wal", |r| r.get::<_, String>(0))
        .unwrap();
    let entries = if k == 1 {
        // 0001's shape: name, a free-text kind and sha256.
        format!(
            "INSERT INTO mod_set_entries (mod_set_id, name, kind, sha256, parked)
               VALUES (1, 'W3Perf', 'asi', '{}', 0), (1, 'modAlpha', 'mod', '{}', 1);",
            sha(5),
            sha(6)
        )
    } else {
        format!(
            "INSERT INTO mod_set_entries (mod_set_id, kind, name, parked, home, content_hash, file_count, bytes,
               config_hash, config_values, version_label, provenance)
               VALUES (1, 'asi', 'W3Perf', 0, '%GAME_DIR%/bin/x64/W3Perf.asi', '{0}', 1, 4, '{1}', '{{\"Mode\":\"1\"}}',
                       '1.0', 'git_commit'),
                      (1, 'mods', 'modAlpha', 1, '%GAME_DIR%/Mods/modAlpha', '{2}', 3, 30, NULL, '{{}}', NULL, 'unknown');
             INSERT INTO mod_versions (content_hash, kind, name, version, provenance, source)
               VALUES ('{0}', 'asi', 'W3Perf', '1.0', 'git_commit', 'abc1234');
             INSERT INTO boot_mod_status (boot_id, kind, name, status, reason)
               VALUES (1, 'asi', 'W3Perf', 'loaded', NULL), (2, 'asi', 'W3Perf', 'refused', 'game code differs');",
            sha(5),
            sha(7),
            sha(6)
        )
    };
    c.execute_batch(&format!(
        "INSERT INTO games (id, name, exe_path, exe_sha256, build_stamp)
           VALUES (1, 'synthetic', '%GAME_DIR%/bin/x64/game.exe', '{g}', '0x00000001');
         INSERT INTO saves (id, game_id, file, sha256, note) VALUES (1, 1, 'slot_1.sav', '{s}', 'synthetic');
         INSERT INTO mod_sets (id, sha256) VALUES (1, '{m}');
         INSERT INTO boots (id, game_id, t_start, t_end, save_id, mod_set_id, settings_diff, devkit, profiler, patches,
             outcome, journal_path)
           VALUES (1, 1, '2026-10-10T10:00:00Z', '2026-10-10T11:00:00Z', 1, 1, '{{\"Rendering\":{{\"SomeCvar\":\"0\"}}}}',
                   0, NULL, '[]', 'ok', '%LOCALAPPDATA%/quen/logs/1/journal.jsonl'),
                  (2, 1, '2026-10-11T10:00:00Z', '2026-10-11T11:00:00Z', 1, 1, '{{\"Rendering\":{{\"SomeCvar\":\"1\"}}}}',
                   0, NULL, '[]', 'ok', NULL);
         INSERT INTO poses (id, name, cam_x, cam_y, cam_z, cam_pitch, cam_yaw, cam_roll, fov, save_id)
           VALUES (1, 'P1', 1, 2, 3, -10, 90, 0, 60, 1);
         INSERT INTO captures (id, boot_id, path, sha256, bytes, kind, method, encoding, pose_id, t)
           VALUES (1, 1, 'shots/P1_a.png', '{c}', 10, 'screenshot', 'printwindow', 'srgb8', 1, '2026-10-10T10:30:00Z'),
                  (2, 1, 'ini/perf_A.csv', '{c}', 20, 'presentmon_csv', 'presentmon', NULL, NULL, '2026-10-10T10:40:00Z');
         INSERT INTO measurements (id, boot_id, capture_id, metric, value, unit, state_label, method_family, tag, run_id)
           VALUES (1, 1, 2, 'frame_ms_mean', 20.0, 'ms', 'A', 'presentmon', 'M', 'run-1'),
                  (2, 1, 2, 'frame_ms_mean', 21.0, 'ms', 'B', 'presentmon', 'M', 'run-1'),
                  (3, 1, 2, 'frame_ms_mean', 20.1, 'ms', 'A', 'presentmon', 'M', 'run-1'),
                  (4, 1, 2, 'frame_ms_mean', 21.1, 'ms', 'B', 'presentmon', 'M', 'run-1'),
                  (5, NULL, NULL, 'pool_size', 4096, NULL, NULL, 'static_re', 'static_re', NULL);
         INSERT INTO claims (id, text, scope) VALUES (1, 'state B costs about 1 ms', '5.01');
         INSERT INTO claim_support (claim_id, measurement_id, supports) VALUES (1, 2, 1), (1, 5, 1);
         INSERT INTO doc_refs (id, doc_path, line, raw, capture_id, dangling, t_scanned)
           VALUES (1, 'notes/a.md', 3, 'cap/shots/P1_a.png', 1, 0, '2026-10-12T10:00:00Z');
         INSERT INTO re_refs (id, measurement_id, claim_id, kb_path) VALUES (1, NULL, 1, 're/notes/pools.md');
         {entries}",
        g = sha(1),
        s = sha(2),
        m = sha(3),
        c = sha(4),
    ))
    .unwrap();
    if k >= 3 {
        c.execute_batch(
            r#"UPDATE boots SET harness_info = '{"parked": ["modAlpha"]}' WHERE id = 2"#,
        )
        .unwrap();
    }
    if k >= 7 {
        c.execute_batch(
            "UPDATE captures SET removed_at = '2026-10-18T00:00:00Z', removed_to = 'to_delete' WHERE id = 2",
        )
        .unwrap();
    }
    if k >= 6 {
        c.execute_batch(&format!(
            r#"INSERT INTO sweeps (id, name, sha256, sweep, save_id, note)
                 VALUES (1, 'pan', '{h}', '{{"keyframes": [{{"t": 0}}, {{"t": 1}}]}}', 1, 'synthetic');
               INSERT INTO sweep_keyframes (sweep_id, keyframe, pose_id) VALUES (1, 0, 1), (1, 1, 1);
               INSERT INTO sweep_runs (id, sweep_id, boot_id, mode, fps, t)
                 VALUES (1, 1, 1, 'step', 30, '2026-10-10T10:50:00Z');
               INSERT INTO sweep_frames (run_id, frame, capture_id) VALUES (1, 0, 1);"#,
            h = sha(8)
        ))
        .unwrap();
    }
    if k >= 9 {
        c.execute_batch(&format!(
            "INSERT INTO entities (id, game, kind, key, t)
               VALUES (1, 'synthetic', 'function', 'F1', '2026-10-18T00:00:00Z'),
                      (2, 'synthetic', 'function', 'F2', '2026-10-18T00:00:00Z');
             INSERT INTO extraction_profiles (digest, tool, provider, parameters) VALUES ('{p}', 'rea', '{{}}', '{{}}');
             INSERT INTO extractions (id, tool, target_sha256, profile_digest, operation, params, key_sha256,
                 result_sha256, rva, entity_id, capture_id, t)
               VALUES (1, 'rea', '{g}', '{p}', 'decompile', '{{}}', '{x}', '{r}', 4096, 1, 1, '2026-10-18T00:00:00Z');
             INSERT INTO entity_addresses (entity_id, game_id, rva, extraction_id) VALUES (1, 1, 4096, 1);
             INSERT INTO links (id, src_id, dst_id, rel, extraction_id, t)
               VALUES (1, 1, 2, 'calls', 1, '2026-10-18T00:00:00Z');
             INSERT INTO claim_subjects (claim_id, entity_id) VALUES (1, 1);
             INSERT INTO kb_sections (id, path, anchor, level, line_start, line_end, sha256, scanned_at)
               VALUES (1, 're/notes/pools.md', '', 1, 1, 2, '{r}', '2026-10-18T00:00:00Z');
             INSERT INTO kb_mentions (section_id, entity_id, n) VALUES (1, 1, 1);",
            p = sha(9),
            g = sha(1),
            x = sha(10),
            r = sha(11),
        ))
        .unwrap();
    }
    c
}

/// Every row of some tables: (table, its rows ordered by the columns asked for).
type Snapshot = Vec<(String, Vec<Vec<Sql>>)>;

/// The tables of `c` and their column names.
fn columns(c: &Connection) -> Vec<(String, Vec<String>)> {
    let mut stmt = c
        .prepare(
            "SELECT m.name, group_concat(p.name, ',' ORDER BY p.cid) FROM sqlite_schema m, pragma_table_info(m.name) p
             WHERE m.type = 'table' GROUP BY m.name ORDER BY m.name",
        )
        .unwrap();
    stmt.query_map([], |r| {
        let cols: String = r.get(1)?;
        Ok((r.get(0)?, cols.split(',').map(String::from).collect()))
    })
    .unwrap()
    .map(Result::unwrap)
    .collect()
}

fn snapshot(c: &Connection, tables: &[(String, Vec<String>)]) -> Snapshot {
    tables
        .iter()
        .map(|(table, cols)| {
            let list = cols.join(", ");
            let mut stmt = c
                .prepare(&format!("SELECT {list} FROM {table} ORDER BY {list}"))
                .unwrap();
            let rows = stmt
                .query_map([], |r| {
                    (0..cols.len()).map(|i| r.get::<_, Sql>(i)).collect()
                })
                .unwrap()
                .map(Result::unwrap)
                .collect();
            (table.clone(), rows)
        })
        .collect()
}

/// The files in the ledger's backups folder.
fn backups(db: &TempDb) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = match fs::read_dir(db.dir().join("backups")) {
        Ok(dir) => dir.map(|e| e.unwrap().path()).collect(),
        Err(_) => vec![],
    };
    files.sort();
    files
}

#[test]
fn a_ledger_from_every_older_version_upgrades_after_a_backup() {
    const { assert!(SCHEMA_VERSION >= 2) };
    for k in 1..SCHEMA_VERSION as usize {
        let db = TempDb::new();
        let old = old_ledger(&db.0, k);
        let tables = columns(&old);
        let before = snapshot(&old, &tables);
        assert!(
            before.iter().all(|(_, rows)| !rows.is_empty()),
            "v{k}: every table has rows"
        );

        let l = Ledger::open(&db.0).unwrap();
        drop(old);
        let version = rows(&l, "SELECT user_version FROM pragma_user_version", &[]);
        assert_eq!(version, vec![vec![json!(SCHEMA_VERSION)]], "v{k}");

        // The backup: one file, named for the old version and the time, holding the ledger as it was.
        let files = backups(&db);
        assert_eq!(files.len(), 1, "v{k}: {files:?}");
        let name = files[0].file_name().unwrap().to_str().unwrap().to_string();
        let stamp = name
            .strip_prefix(&format!("quen-v{k}-"))
            .and_then(|s| s.strip_suffix(".db"))
            .unwrap_or_else(|| panic!("v{k}: backup named {name}"));
        assert!(
            stamp.len() == 16
                && stamp.as_bytes()[8] == b'T'
                && stamp.ends_with('Z')
                && stamp.chars().filter(char::is_ascii_digit).count() == 14,
            "v{k}: {stamp} is a UTC time"
        );
        let backup = Ledger::open_readonly(&files[0]).unwrap();
        let backed_up = rows(&backup, "SELECT user_version FROM pragma_user_version", &[]);
        assert_eq!(
            backed_up,
            vec![vec![json!(k)]],
            "v{k}: the backup opens at its version"
        );
        drop(backup);
        let raw = Connection::open_with_flags(&files[0], OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        assert_eq!(
            snapshot(&raw, &tables),
            before,
            "v{k}: the backup holds every row"
        );
        drop(raw);

        // Every row survives the migrations; 0002 renamed mod_set_entries.sha256 to content_hash.
        let now = Connection::open(&db.0).unwrap();
        let renamed: Vec<(String, Vec<String>)> = tables
            .iter()
            .map(|(table, cols)| {
                let cols = cols
                    .iter()
                    .map(|c| match (table.as_str(), c.as_str()) {
                        ("mod_set_entries", "sha256") => "content_hash".to_string(),
                        _ => c.clone(),
                    })
                    .collect();
                (table.clone(), cols)
            })
            .collect();
        assert_eq!(snapshot(&now, &renamed), before, "v{k}: rows survive");
        let fk: i64 = now
            .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(fk, 0, "v{k}");
        // 0008: removed_to takes 'missing' too (feature tests 2c), and still no value outside its list.
        now.execute(
            "UPDATE captures SET removed_at = '2026-10-18T00:00:00Z', removed_to = 'missing' WHERE id = 1",
            [],
        )
        .unwrap();
        let other = "UPDATE captures SET removed_to = 'elsewhere' WHERE id = 1";
        assert!(now.execute(other, []).is_err(), "v{k}");
        drop(now);

        // The views work over the old rows.
        let conditions = rows(
            &l,
            "SELECT boot_id, exe_sha256, mod_set_sha256, devkit, profiler, busy, busy_with FROM boot_conditions
             ORDER BY boot_id",
            &[],
        );
        assert_eq!(
            conditions[0],
            [
                json!(1),
                json!(sha(1)),
                json!(sha(3)),
                json!(0),
                Value::Null,
                Value::Null,
                Value::Null
            ],
            "v{k}: a boot from before 0005 has busy unknown"
        );
        let comparison = rows(
            &l,
            "SELECT comparable, differs, unknown, busy, varies FROM boot_comparison WHERE a_id = 1 AND b_id = 2",
            &[],
        );
        let varies = if k >= 2 {
            "mod_status,settings"
        } else {
            "settings"
        };
        assert_eq!(
            comparison,
            vec![vec![
                json!(0),
                json!(""),
                json!("busy"),
                json!(""),
                json!(varies)
            ]],
            "v{k}"
        );
        let measured = rows(
            &l,
            "SELECT count(*), sum(capture_path = 'ini/perf_A.csv'), sum(exe_sha256 IS NULL) FROM measurements_with_boot",
            &[],
        );
        assert_eq!(measured, vec![vec![json!(5), json!(4), json!(1)]], "v{k}");
        let captured = rows(
            &l,
            "SELECT pose, boot_t_start FROM captures_with_boot WHERE id = 1",
            &[],
        );
        assert_eq!(
            captured,
            vec![vec![json!("P1"), json!("2026-10-10T10:00:00Z")]],
            "v{k}"
        );
        let claim = rows(
            &l,
            "SELECT proven, supported_by, kb_paths, measurements, boots FROM claims_evidence",
            &[],
        );
        assert_eq!(
            claim,
            vec![vec![
                json!(1),
                json!("presentmon,static_re"),
                json!("re/notes/pools.md"),
                json!(2),
                json!(1)
            ]],
            "v{k}"
        );
        // 0009: an older claim has no key and no grades, and the knowledge tables start empty (a v9 ledger's hold its
        // two entities and one link).
        let graded = rows(
            &l,
            "SELECT key, confidence, review, (SELECT count(*) FROM entities) + (SELECT count(*) FROM links_evidence)
             FROM claims",
            &[],
        );
        let knowledge = if k >= 9 { 3 } else { 0 };
        assert_eq!(
            graded,
            vec![vec![
                Value::Null,
                Value::Null,
                Value::Null,
                json!(knowledge)
            ]],
            "v{k}"
        );
        // 0010: an older save was made by nobody quen knows of; a new one can name its boot.
        let made = rows(&l, "SELECT made_by_boot FROM saves", &[]);
        assert_eq!(made, vec![vec![Value::Null]], "v{k}");
        let now = Connection::open(&db.0).unwrap();
        now.pragma_update(None, "foreign_keys", true).unwrap();
        now.execute("UPDATE saves SET made_by_boot = 2 WHERE id = 1", [])
            .unwrap();
        let unknown = "UPDATE saves SET made_by_boot = 99 WHERE id = 1";
        assert!(now.execute(unknown, []).is_err(), "v{k}: a boot that isn't");
        now.execute("UPDATE saves SET made_by_boot = NULL WHERE id = 1", [])
            .unwrap();
        // 0011: an older boot was a test launch and an older row's world unknown; a play boot never compares with one.
        let old = rows(
            &l,
            "SELECT (SELECT count(*) FROM boots WHERE mode IS NOT NULL),
                    (SELECT count(*) FROM measurements WHERE world IS NOT NULL)",
            &[],
        );
        assert_eq!(old, vec![vec![json!(0), json!(0)]], "v{k}");
        now.execute("UPDATE boots SET mode = 'play' WHERE id = 2", [])
            .unwrap();
        now.execute("UPDATE measurements SET world = 'photo' WHERE id = 3", [])
            .unwrap();
        for bad in [
            "UPDATE boots SET mode = 'observe' WHERE id = 2",
            "UPDATE measurements SET world = 'moving' WHERE id = 3",
        ] {
            assert!(now.execute(bad, []).is_err(), "v{k}: {bad}");
        }
        let play = rows(
            &l,
            "SELECT differs, comparable FROM boot_comparison WHERE a_id = 1 AND b_id = 2",
            &[],
        );
        assert_eq!(play, vec![vec![json!("mode"), json!(0)]], "v{k}");
        let shown = rows(
            &l,
            "SELECT m.world, m.boot_mode, c.boot_mode, b.mode FROM measurements_with_boot m, captures_with_boot c,
               boot_conditions b WHERE m.id = 3 AND c.id = 1 AND b.boot_id = 2",
            &[],
        );
        let want = vec![json!("photo"), Value::Null, Value::Null, json!("play")];
        assert_eq!(shown, vec![want], "v{k}");
        drop(now);
        let run = rows(
            &l,
            "SELECT run_id, first_label, second_label, verdict FROM ab_runs",
            &[],
        );
        assert_eq!(
            run,
            vec![vec![
                json!("run-1"),
                json!("A"),
                json!("B"),
                json!("outside drift")
            ]],
            "v{k}"
        );
        drop(l);

        drop(Ledger::open(&db.0).unwrap());
        assert_eq!(
            backups(&db),
            files,
            "v{k}: a current ledger isn't backed up again"
        );
    }
}

#[test]
fn a_new_or_current_ledger_is_not_backed_up() {
    let db = TempDb::new();
    drop(Ledger::open(&db.0).unwrap());
    drop(Ledger::open(&db.0).unwrap());
    assert!(!db.dir().join("backups").exists());
}

fn refused(r: Result<Ledger, Error>, needle: &str) {
    match r {
        Err(Error::Refused(why)) => assert!(why.contains(needle), "{why:?} lacks {needle:?}"),
        Err(e) => panic!("expected a refusal about {needle:?}, got {e}"),
        Ok(_) => panic!("expected a refusal about {needle:?}, got a ledger"),
    }
}

#[test]
fn a_failed_backup_refuses_and_changes_nothing() {
    let k = SCHEMA_VERSION as usize - 1;
    let db = TempDb::new();
    let old = old_ledger(&db.0, k);
    let tables = columns(&old);
    let before = snapshot(&old, &tables);
    drop(old);

    // The backups folder can't be made: a file has its name.
    let blocker = db.dir().join("backups");
    fs::write(&blocker, "not a folder").unwrap();
    refused(Ledger::open(&db.0), "backed up");
    assert_eq!(fs::read_to_string(&blocker).unwrap(), "not a folder");
    let raw = Connection::open(&db.0).unwrap();
    let version: i64 = raw
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, k as i64, "not migrated");
    assert_eq!(snapshot(&raw, &tables), before);
    drop(raw);
    fs::remove_file(&blocker).unwrap();

    // A backup is never replaced: a file already named for this second is kept, and the open refused. The names are
    // taken for the next minute, so the open below falls inside them.
    fs::create_dir(&blocker).unwrap();
    let clock = Connection::open_in_memory().unwrap();
    let names: Vec<PathBuf> = (0..60)
        .map(|s| {
            let t: String = clock
                .query_row(
                    "SELECT strftime('%Y%m%dT%H%M%SZ', 'now', ?1 || ' seconds')",
                    [s],
                    |r| r.get(0),
                )
                .unwrap();
            blocker.join(format!("quen-v{k}-{t}.db"))
        })
        .collect();
    for name in &names {
        fs::write(name, "an earlier backup").unwrap();
    }
    refused(Ledger::open(&db.0), "backed up");
    for name in &names {
        assert_eq!(fs::read_to_string(name).unwrap(), "an earlier backup");
    }
    assert_eq!(backups(&db).len(), names.len(), "no other file");
    let raw = Connection::open(&db.0).unwrap();
    assert_eq!(snapshot(&raw, &tables), before);
    drop(raw);

    // Once the backup can be made, the upgrade goes ahead.
    for name in &names {
        fs::remove_file(name).unwrap();
    }
    let l = Ledger::open(&db.0).unwrap();
    let version = rows(&l, "SELECT user_version FROM pragma_user_version", &[]);
    assert_eq!(version, vec![vec![json!(SCHEMA_VERSION)]]);
    assert_eq!(backups(&db).len(), 1);
}

#[test]
fn readonly_connections_refuse_a_newer_ledger_and_read_an_older_one() {
    let db = TempDb::new();
    drop(old_ledger(&db.0, 1));
    let r = Ledger::open_readonly(&db.0).unwrap();
    assert_eq!(
        rows(&r, "SELECT user_version FROM pragma_user_version", &[]),
        vec![vec![json!(1)]],
        "read as it is, not migrated"
    );
    drop(r);
    assert!(backups(&db).is_empty());
    Connection::open(&db.0)
        .unwrap()
        .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
        .unwrap();
    refused(Ledger::open_readonly(&db.0), "newer");
}
