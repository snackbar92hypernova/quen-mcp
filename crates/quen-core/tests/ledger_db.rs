//! Opening, migrations, the read-only query and typed ingest (docs/intent.md, "The ledger").

mod ledger_common;

use std::time::{Duration, Instant};

use ledger_common::{TempDb, boot, capture, game, measurement, rows, sha};
use quen_core::hdr::Encoding;
use quen_core::ledger::{
    BootEnd, BootModStatus, BootStart, Capture, Claim, Error, Ledger, LoadStatus, Measurement,
    ModVersion, Outcome, PlayerPose, Pose, Provenance, ReTarget, Row, SCHEMA_VERSION, Save, Tag,
};
use quen_core::mods::{Kind, ModEntry};
use rusqlite::Connection;
use serde_json::{Value, json};

fn pragma(path: &std::path::Path, name: &str) -> Value {
    let c = Connection::open(path).unwrap();
    c.query_row(&format!("PRAGMA {name}"), [], |r| {
        Ok(match r.get_ref(0)? {
            rusqlite::types::ValueRef::Integer(n) => json!(n),
            v => json!(v.as_str().unwrap()),
        })
    })
    .unwrap()
}

fn refused(r: Result<impl std::fmt::Debug, Error>, needle: &str) {
    match r {
        Err(Error::Refused(why)) => assert!(why.contains(needle), "{why:?} lacks {needle:?}"),
        other => panic!("expected a refusal about {needle:?}, got {other:?}"),
    }
}

#[test]
fn open_migrates_once_with_wal_and_foreign_keys() {
    let db = TempDb::new();
    let mut l = Ledger::open(&db.0).unwrap();
    let g = game(&mut l);
    let fk = rows(&l, "SELECT foreign_keys FROM pragma_foreign_keys", &[]);
    assert_eq!(fk, vec![vec![json!(1)]]);
    drop(l);

    assert_eq!(pragma(&db.0, "user_version"), json!(SCHEMA_VERSION));
    assert_eq!(pragma(&db.0, "journal_mode"), json!("wal"));
    let l = Ledger::open(&db.0).unwrap();
    assert_eq!(
        rows(&l, "SELECT id FROM games", &[]),
        vec![vec![json!(g)]],
        "reopening keeps the rows"
    );
    let tables = rows(
        &l,
        "SELECT name FROM sqlite_schema WHERE type IN ('table', 'view') ORDER BY name",
        &[],
    );
    let names: Vec<&str> = tables.iter().map(|r| r[0].as_str().unwrap()).collect();
    assert_eq!(
        names,
        [
            "ab_runs",
            "boot_comparison",
            "boot_conditions",
            "boot_mod_status",
            "boots",
            "captures",
            "captures_with_boot",
            "claim_subjects",
            "claim_support",
            "claims",
            "claims_evidence",
            "claims_status",
            "doc_refs",
            "entities",
            "entity_addresses",
            "extraction_profiles",
            "extractions",
            "games",
            "kb_mentions",
            "kb_sections",
            "links",
            "links_evidence",
            "measurements",
            "measurements_with_boot",
            "mod_set_entries",
            "mod_sets",
            "mod_versions",
            "poses",
            "re_refs",
            "saves",
            "sweep_frames",
            "sweep_keyframes",
            "sweep_runs",
            "sweeps"
        ]
    );
}

#[test]
fn open_refuses_a_newer_ledger_and_foreign_databases_untouched() {
    let db = TempDb::new();
    drop(Ledger::open(&db.0).unwrap());
    Connection::open(&db.0)
        .unwrap()
        .pragma_update(None, "user_version", 99)
        .unwrap();
    refused(Ledger::open(&db.0).map(|_| ()), "newer");
    refused(Ledger::open_readonly(&db.0).map(|_| ()), "newer");
    assert_eq!(pragma(&db.0, "user_version"), json!(99));
    assert!(
        !db.dir().join("backups").exists(),
        "a refusal backs nothing up"
    );

    let other = TempDb::new();
    Connection::open(&other.0)
        .unwrap()
        .execute_batch("CREATE TABLE notes (x TEXT)")
        .unwrap();
    refused(Ledger::open(&other.0).map(|_| ()), "isn't a quen ledger");
    refused(
        Ledger::open_readonly(&other.0).map(|_| ()),
        "not a quen ledger",
    );
    assert_eq!(
        pragma(&other.0, "journal_mode"),
        json!("delete"),
        "not switched to WAL"
    );
    assert_eq!(pragma(&other.0, "user_version"), json!(0));
}

#[test]
fn open_refuses_a_path_inside_a_git_work_tree() {
    // The crate folder is inside this repository's work tree.
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("quen-ledger-test-refused.db");
    refused(Ledger::open(&path).map(|_| ()), "git work tree");
    assert!(!path.exists(), "nothing was created");
}

#[test]
fn readonly_connections_read_but_cannot_write() {
    let db = TempDb::new();
    let mut l = Ledger::open(&db.0).unwrap();
    game(&mut l);
    drop(l);

    let mut r = Ledger::open_readonly(&db.0).unwrap();
    assert_eq!(
        rows(&r, "SELECT count(*) FROM games", &[]),
        vec![vec![json!(1)]]
    );
    assert_eq!(
        rows(&r, "SELECT foreign_keys FROM pragma_foreign_keys", &[]),
        vec![vec![json!(1)]]
    );
    assert!(matches!(
        r.add_claim(&Claim {
            text: "x".into(),
            scope: None
        }),
        Err(Error::Sqlite(_))
    ));
    let w = Ledger::open(&db.0).unwrap();
    assert_eq!(
        rows(&w, "SELECT count(*) FROM claims", &[]),
        vec![vec![json!(0)]]
    );

    let missing = TempDb::new();
    assert!(Ledger::open_readonly(&missing.0).is_err());
    assert!(!missing.0.exists());
}

#[test]
fn query_runs_one_read_only_statement_with_params() {
    let l = Ledger::open_in_memory().unwrap();
    let out = l
        .query(
            "-- a comment\n/* and another */ select ?1 AS n, ?2 AS s, ?3 AS b, ?4 AS x, 0.5 AS r, NULL AS z, x'ab' AS blob;",
            &[json!(7), json!("text"), json!(true), json!(1.25)],
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(out.columns, ["n", "s", "b", "x", "r", "z", "blob"]);
    assert_eq!(
        out.rows,
        vec![vec![
            json!(7),
            json!("text"),
            json!(1),
            json!(1.25),
            json!(0.5),
            Value::Null,
            json!("ab")
        ]]
    );
    let with = rows(
        &l,
        "WITH RECURSIVE c(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM c WHERE i < 3) SELECT sum(i) FROM c",
        &[],
    );
    assert_eq!(with, vec![vec![json!(6)]]);

    for sql in [
        "INSERT INTO claims (text) VALUES ('x')",
        "PRAGMA user_version = 5",
        "SELECT 1; SELECT 2",
        "SELECT 1; DELETE FROM claims",
        "WITH x AS (SELECT 1) DELETE FROM claims",
        "-- SELECT\nDELETE FROM claims",
        "VALUES (1)",
        "ATTACH ':memory:' AS other",
        "",
    ] {
        refused(l.query(sql, &[], Duration::from_secs(1)), "SELECT or WITH");
    }
    refused(
        l.query("SELECT ?1", &[json!([1])], Duration::from_secs(1)),
        "parameters",
    );
    assert!(matches!(
        l.query("SELECT ?1, ?2", &[json!(1)], Duration::from_secs(1)),
        Err(Error::Sqlite(_))
    ));
}

#[test]
fn query_stops_at_its_time_limit() {
    let l = Ledger::open_in_memory().unwrap();
    let start = Instant::now();
    let endless =
        "WITH RECURSIVE c(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM c) SELECT count(*) FROM c";
    assert!(matches!(
        l.query(endless, &[], Duration::from_millis(200)),
        Err(Error::TimeLimit)
    ));
    assert!(start.elapsed() < Duration::from_secs(5));
    assert_eq!(
        rows(&l, "SELECT 1", &[]),
        vec![vec![json!(1)]],
        "the connection still works"
    );
}

#[test]
fn games_mod_sets_and_saves_are_added_on_first_sight() {
    let mut l = Ledger::open_in_memory().unwrap();
    let g = game(&mut l);
    assert_eq!(game(&mut l), g);

    let entry = |name: &str, parked| ModEntry {
        kind: Kind::ModsFolder,
        name: name.into(),
        home: format!("%GAME_DIR%/Mods/{name}").into(),
        parked,
        content_hash: sha(5),
        file_count: 1,
        bytes: 10,
        config_hash: None,
        config_values: Default::default(),
    };
    let a = l
        .add_mod_set(&[entry("modA", false), entry("modB", true)])
        .unwrap();
    assert_eq!(
        l.add_mod_set(&[entry("modB", true), entry("modA", false)])
            .unwrap(),
        a,
        "order doesn't matter"
    );
    let b = l
        .add_mod_set(&[entry("modA", false), entry("modB", false)])
        .unwrap();
    assert_ne!(a, b, "parking is part of the set");
    let parked = rows(
        &l,
        "SELECT name FROM mod_set_entries WHERE mod_set_id = ?1 AND parked = 1",
        &[json!(a)],
    );
    assert_eq!(parked, vec![vec![json!("modB")]]);
    refused(
        l.add_mod_set(&[entry("modA", false), entry("modA", true)]),
        "twice",
    );
    refused(l.add_mod_set(&[entry("mod\tA", false)]), "tab");

    let save = |by| Save {
        game_id: g,
        file: "slot_1.sav".into(),
        sha256: sha(7),
        note: None,
        tainted_by_boot: by,
        made_by_boot: by,
    };
    let s = l.add_save(&save(None)).unwrap();
    let b = boot(&mut l, g, "2026-10-10T10:00:00Z");
    assert_eq!(l.add_save(&save(Some(b))).unwrap(), s);
    assert_eq!(l.add_save(&save(None)).unwrap(), s);
    assert_eq!(
        rows(&l, "SELECT tainted_by_boot, made_by_boot FROM saves", &[]),
        vec![vec![json!(b), json!(b)]],
        "taint and maker are kept"
    );
    assert!(
        l.add_save(&Save {
            game_id: 999,
            ..save(None)
        })
        .is_err(),
        "foreign keys are on"
    );
}

#[test]
fn a_boot_starts_finishes_and_is_reviewed_once_finished() {
    let mut l = Ledger::open_in_memory().unwrap();
    let g = game(&mut l);
    let settings = json!({"Rendering": {"MtRender": "true"}});
    let display = json!({"hdr": true, "sdr_white_nits": 240});
    let b = l
        .start_boot(&BootStart {
            game_id: g,
            t_start: "2026-10-10T10:00:00Z".into(),
            settings_diff: settings.as_object().cloned(),
            display_state: display.as_object().cloned(),
            devkit: Some(false),
            fg: Some(false),
            profiler: Some("nsys".into()),
            journal_path: Some(r"%LOCALAPPDATA%\quen\state\journal.jsonl".into()),
            ..Default::default()
        })
        .unwrap();
    refused(l.review(b, "2026-10-10T12:00:00Z"), "hasn't finished");

    let end = |t: &str, outcome, failure: Option<&str>| BootEnd {
        t_end: t.into(),
        outcome,
        failure: failure.map(String::from),
        patches: vec![json!({"name": "tlas_mask", "rva": "0x1000"})],
    };
    assert!(
        l.finish_boot(b, &end("2026-10-10T09:00:00Z", Outcome::Ok, None))
            .is_err(),
        "ends before it started"
    );
    assert!(
        l.finish_boot(b, &end("2026-10-10 11:00:00", Outcome::Ok, None))
            .is_err(),
        "not UTC ISO-8601"
    );
    assert!(
        l.finish_boot(b, &end("2026-10-10T11:00:00Z", Outcome::Failed, None))
            .is_err(),
        "a failure needs its text"
    );
    // An end that left its journal: why and the patches go on the boot, which stays open for the recovery.
    l.hold_boot(b, "the replay left steps", &[json!({"name": "x"})])
        .unwrap();
    let held = l
        .query(
            &format!("SELECT t_end, failure, patches FROM boots WHERE id = {b}"),
            &[],
            Duration::from_secs(5),
        )
        .unwrap()
        .rows;
    assert_eq!(
        held[0],
        [
            Value::Null,
            json!("the replay left steps"),
            json!(r#"[{"name":"x"}]"#)
        ]
    );
    l.finish_boot(b, &end("2026-10-10T11:00:00Z", Outcome::Ok, None))
        .unwrap();
    refused(
        l.finish_boot(b, &end("2026-10-10T11:30:00Z", Outcome::Ok, None)),
        "already finished",
    );
    refused(l.hold_boot(b, "late", &[]), "already finished");
    refused(
        l.finish_boot(999, &end("2026-10-10T11:30:00Z", Outcome::Ok, None)),
        "no boot 999",
    );

    assert!(
        l.review(b, "2026-10-10T10:30:00Z").is_err(),
        "reviewed before it ended"
    );
    l.review(b, "2026-10-11T08:00:00Z").unwrap();
    refused(l.review(999, "2026-10-11T08:00:00Z"), "no boot 999");
    let r = rows(
        &l,
        "SELECT json_extract(settings_diff, '$.Rendering.MtRender'), json_extract(display_state, '$.sdr_white_nits'),
           json_extract(patches, '$[0].name'), devkit, rebar, outcome, reviewed_at FROM boots",
        &[],
    );
    assert_eq!(
        r,
        vec![vec![
            json!("true"),
            json!(240),
            json!("tlas_mask"),
            json!(0),
            Value::Null,
            json!("ok"),
            json!("2026-10-11T08:00:00Z")
        ]]
    );
}

#[test]
fn a_boot_records_what_kept_the_machine_busy() {
    let mut l = Ledger::open_in_memory().unwrap();
    let g = game(&mut l);
    let start = |busy_with: Option<Vec<String>>| BootStart {
        game_id: g,
        t_start: "2026-10-10T10:00:00Z".into(),
        busy_with,
        ..Default::default()
    };
    let unchecked = l.start_boot(&start(None)).unwrap();
    let quiet = l.start_boot(&start(Some(vec![]))).unwrap();
    let busy = l
        .start_boot(&start(Some(vec!["rustc.exe".into()])))
        .unwrap();
    let flags = |l: &Ledger| {
        rows(
            l,
            "SELECT boot_id, busy, busy_with FROM boot_conditions ORDER BY boot_id",
            &[],
        )
    };
    assert_eq!(
        flags(&l),
        [
            vec![json!(unchecked), Value::Null, Value::Null],
            vec![json!(quiet), json!(0), json!("[]")],
            vec![json!(busy), json!(1), json!(r#"["rustc.exe"]"#)],
        ]
    );

    // Later checks add what they found, sorted and once each; a quiet check never clears a busy one.
    l.record_quiet_check(unchecked, &[]).unwrap();
    l.record_quiet_check(quiet, &["java.exe".into(), "cargo.exe".into()])
        .unwrap();
    l.record_quiet_check(busy, &["cargo.exe".into(), "rustc.exe".into()])
        .unwrap();
    l.record_quiet_check(busy, &[]).unwrap();
    assert_eq!(
        flags(&l),
        [
            vec![json!(unchecked), json!(0), json!("[]")],
            vec![json!(quiet), json!(1), json!(r#"["cargo.exe","java.exe"]"#)],
            vec![json!(busy), json!(1), json!(r#"["cargo.exe","rustc.exe"]"#)],
        ]
    );
    l.finish_boot(
        busy,
        &BootEnd {
            t_end: "2026-10-10T11:00:00Z".into(),
            outcome: Outcome::Ok,
            failure: None,
            patches: vec![],
        },
    )
    .unwrap();
    refused(l.record_quiet_check(busy, &[]), "already finished");
    refused(l.record_quiet_check(999, &[]), "no boot 999");
}

#[test]
fn a_boot_gets_its_mod_set_once_and_times_are_utc_text() {
    let mut l = Ledger::open_in_memory().unwrap();
    assert_eq!(l.utc(1_792_242_900).unwrap(), "2026-10-17T13:15:00Z");
    let g = game(&mut l);
    let b = boot(&mut l, g, "2026-10-10T10:00:00Z");
    let entry = ModEntry {
        kind: Kind::Asi,
        name: "W3Synthetic".into(),
        home: "GAME/bin/x64/W3Synthetic.asi".into(),
        parked: false,
        content_hash: sha(7),
        file_count: 1,
        bytes: 1,
        config_hash: None,
        config_values: Default::default(),
    };
    let set = l.add_mod_set(&[entry]).unwrap();
    l.set_boot_mod_set(b, set).unwrap();
    refused(l.set_boot_mod_set(b, set), "has its mod set already");
    refused(l.set_boot_mod_set(999, set), "no boot 999");
    let r = rows(&l, "SELECT mod_set_id FROM boots", &[]);
    assert_eq!(r, vec![vec![json!(set)]]);
    // Feature tests 2b: the devkit goes on an unfinished boot's row once the launch saw it loaded.
    let diff = serde_json::Map::from_iter([("conditions".into(), json!(["devkit"]))]);
    refused(l.set_boot_devkit(b, &diff), "has already finished");
    let start = BootStart {
        game_id: g,
        t_start: "2026-10-10T12:00:00Z".into(),
        devkit: Some(false),
        ..Default::default()
    };
    let live = l.start_boot(&start).unwrap();
    l.set_boot_devkit(live, &diff).unwrap();
    let sql = "SELECT devkit, settings_diff FROM boots WHERE id = ?1";
    let r = rows(&l, sql, &[json!(live)]);
    assert_eq!(
        r,
        vec![vec![json!(1), json!(r#"{"conditions":["devkit"]}"#)]]
    );
    refused(l.set_boot_devkit(999, &diff), "no boot 999");
}

#[test]
fn every_enum_value_is_accepted_by_the_schema() {
    let mut l = Ledger::open_in_memory().unwrap();
    let g = game(&mut l);
    for &tag in Tag::ALL {
        measurement(&mut l, None, "presentmon", tag);
    }
    for (i, encoding) in [
        Encoding::Srgb8,
        Encoding::PqBt2020U16,
        Encoding::PqCodesF16,
        Encoding::ScrgbF16,
    ]
    .into_iter()
    .enumerate()
    {
        l.add_capture(&Capture {
            path: format!("enc/{i}.png"),
            sha256: sha(3),
            kind: "png".into(),
            encoding: Some(encoding),
            t: "2026-10-10T10:00:00Z".into(),
            ..Default::default()
        })
        .unwrap();
    }
    for &outcome in Outcome::ALL {
        let b = l
            .start_boot(&BootStart {
                game_id: g,
                t_start: "2026-10-10T10:00:00Z".into(),
                ..Default::default()
            })
            .unwrap();
        let failure =
            (outcome == Outcome::Failed).then(|| "witcher3.exe exited during launch".to_string());
        l.finish_boot(
            b,
            &BootEnd {
                t_end: "2026-10-10T10:05:00Z".into(),
                outcome,
                failure,
                patches: vec![],
            },
        )
        .unwrap();
    }
    let stored = rows(&l, "SELECT group_concat(tag, ' ') FROM measurements", &[]);
    assert_eq!(stored, vec![vec![json!("M O I U static_re")]]);

    let b = boot(&mut l, g, "2026-10-10T10:00:00Z");
    let mut n = 0;
    for &kind in Kind::ALL {
        for &provenance in Provenance::ALL {
            n += 1;
            l.add_mod_version(&ModVersion {
                content_hash: sha(n),
                kind,
                name: format!("mod{n}"),
                version: "1.0".into(),
                provenance,
                source: "synthetic".into(),
            })
            .unwrap();
        }
        for &status in LoadStatus::ALL {
            l.add_boot_mod_status(&BootModStatus {
                boot_id: b,
                kind,
                name: format!("{status:?}"),
                status,
                reason: Some("synthetic".into()),
            })
            .unwrap();
        }
    }
    let entries: Vec<ModEntry> = Kind::ALL
        .iter()
        .map(|&kind| ModEntry {
            kind,
            name: "x".into(),
            home: "x".into(),
            parked: false,
            content_hash: sha(1),
            file_count: 1,
            bytes: 1,
            config_hash: None,
            config_values: Default::default(),
        })
        .collect();
    l.add_mod_set(&entries).unwrap();
    let stored = rows(
        &l,
        "SELECT (SELECT count(*) FROM mod_versions), (SELECT count(*) FROM boot_mod_status),
           (SELECT group_concat(kind, ' ') FROM mod_set_entries)",
        &[],
    );
    assert_eq!(
        stored,
        vec![vec![
            json!(10),
            json!(15),
            json!("addon asi dlc mods reshade")
        ]]
    );
}

#[test]
fn captures_are_stored_relative_to_the_capture_root() {
    let mut l = Ledger::open_in_memory().unwrap();
    let t = "2026-10-10T10:00:00Z";
    let c = capture(&mut l, None, r"ngfx\TAG\.\frame1.png", t);
    capture(&mut l, None, "./ini//perf_A.csv", t);
    let paths = rows(&l, "SELECT path FROM captures ORDER BY id", &[]);
    assert_eq!(
        paths,
        vec![
            vec![json!("ngfx/TAG/frame1.png")],
            vec![json!("ini/perf_A.csv")]
        ]
    );
    for bad in [
        r"D:\fixture\cap\x.png",
        "/fixture/x.png",
        "../x.png",
        "ngfx/../../x.png",
        "ngfx/",
        "",
    ] {
        refused(
            l.add_capture(&Capture {
                path: bad.into(),
                sha256: sha(2),
                t: t.into(),
                ..Default::default()
            }),
            "relative",
        );
    }
    assert!(
        l.add_capture(&Capture {
            path: "NGFX/tag/FRAME1.png".into(),
            sha256: sha(2),
            t: t.into(),
            ..Default::default()
        })
        .is_err(),
        "paths are unique ignoring case"
    );
    assert!(
        l.add_capture(&Capture {
            path: "x.png".into(),
            sha256: "ABC".into(),
            t: t.into(),
            ..Default::default()
        })
        .is_err(),
        "sha256 is 64 lowercase hex"
    );

    let p = l
        .add_pose(&Pose {
            name: "P1".into(),
            camera: [1.0, 2.0, 3.0],
            rotation: [-10.0, 90.0, 0.0],
            fov: 60.0,
            player: Some(PlayerPose {
                position: [1.5, 2.5, 0.0],
                yaw: Some(45.0),
            }),
            ref_capture_id: Some(c),
            ..Default::default()
        })
        .unwrap();
    l.add_pose(&Pose {
        name: "P2".into(),
        fov: 50.0,
        ..Default::default()
    })
    .unwrap();
    assert!(
        l.add_pose(&Pose {
            name: "P1".into(),
            ..Default::default()
        })
        .is_err(),
        "pose names are unique"
    );
    let r = rows(
        &l,
        "SELECT cam_yaw, player_yaw, ref_capture_id FROM poses WHERE id = ?1",
        &[json!(p)],
    );
    assert_eq!(r, vec![vec![json!(90.0), json!(45.0), json!(c)]]);

    // Schema 0012: a player position without a yaw.
    let row = json!({"table": "poses", "name": "P3", "camera": [0, 0, 0], "rotation": [0, 0, 0], "fov": 60,
                     "player": {"position": [4, 5, 6]}});
    let ids = l.ingest(&[Row::from_json(&row).unwrap()]).unwrap();
    let r = rows(
        &l,
        "SELECT player_x, player_y, player_z, player_yaw FROM poses WHERE id = ?1",
        &[json!(ids[0])],
    );
    assert_eq!(
        r,
        vec![vec![json!(4.0), json!(5.0), json!(6.0), Value::Null]]
    );
}

#[test]
fn measurements_need_a_lowercase_method_family() {
    let mut l = Ledger::open_in_memory().unwrap();
    let m = |family: &str| Measurement {
        boot_id: None,
        capture_id: None,
        metric: "gpu_busy_ms".into(),
        value: Some(1.0),
        unit: None,
        state_label: None,
        method_family: family.into(),
        tag: Tag::M,
        run_id: None,
        note: None,
        extraction_id: None,
        world: None,
    };
    for bad in ["PresentMon", "present mon", ""] {
        assert!(l.add_measurement(&m(bad)).is_err(), "{bad:?}");
    }
    let id = l.add_measurement(&m("perf_func")).unwrap();
    let claim = l
        .add_claim(&Claim {
            text: "pool size is 4096".into(),
            scope: None,
        })
        .unwrap();
    l.add_re_ref(ReTarget::Measurement(id), "re/notes/pools.md")
        .unwrap();
    l.add_re_ref(ReTarget::Claim(claim), "re/names.tsv")
        .unwrap();
    assert!(l.add_re_ref(ReTarget::Claim(999), "re/names.tsv").is_err());
    assert!(
        l.add_claim(&Claim {
            text: " ".into(),
            scope: None
        })
        .is_err()
    );
}

#[test]
fn rows_deserialize_from_json_and_refuse_unknown_fields() {
    let m: Measurement = serde_json::from_value(json!({
        "boot_id": null, "capture_id": null, "metric": "rva_writes", "value": 1.0, "unit": null, "state_label": null,
        "method_family": "rea", "tag": "static_re", "run_id": null, "note": "synthetic"
    }))
    .unwrap();
    assert_eq!(m.tag, Tag::StaticRe);
    let c: Capture = serde_json::from_value(json!({
        "path": "a.png", "sha256": sha(1), "bytes": 3, "kind": "png", "t": "2026-10-10T10:00:00Z", "encoding": "pq_bt2020_u16"
    }))
    .unwrap();
    assert_eq!((c.encoding, c.pinned), (Some(Encoding::PqBt2020U16), false));
    assert!(serde_json::from_value::<Claim>(json!({"text": "x", "scoep": "typo"})).is_err());
    assert!(serde_json::from_value::<ReTarget>(json!({"claim": 4})).unwrap() == ReTarget::Claim(4));

    // Feature tests 2c: a flag as ledger_query shows it (0 or 1) is taken; a refusal names its field.
    let row = |extra: Value| {
        let mut row = json!({"table": "captures", "path": "a.png", "sha256": sha(1), "bytes": 3, "kind": "png",
                             "t": "2026-10-10T10:00:00Z"});
        row.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        Row::from_json(&row)
    };
    let Ok(Row::Capture(c)) =
        row(json!({"includes_overlay": 0, "includes_desktop": 1, "pinned": 1}))
    else {
        panic!("0 and 1 are flags");
    };
    assert_eq!(
        (c.includes_overlay, c.includes_desktop, c.pinned),
        (Some(false), Some(true), true)
    );
    for (extra, why) in [
        (json!({"includes_overlay": 2}), "includes_overlay: "),
        (
            json!({"frame_state": "{}"}),
            "frame_state: invalid type: string",
        ),
        (json!({"bytes": "3"}), "bytes: invalid type: string"),
        (json!({"encoding": "rgb"}), "encoding: unknown variant"),
        (json!({"scoep": 1}), "unknown field `scoep`"),
    ] {
        let e = row(extra).err().unwrap();
        assert!(e.starts_with(why), "{why}: {e}");
    }
    let e = Row::from_json(&json!({"table": "captures", "path": "a.png"}))
        .err()
        .unwrap();
    assert!(e.starts_with("missing field"), "{e}");
    let e = Row::from_json(&json!({"table": "boots", "path": "a.png"}))
        .err()
        .unwrap();
    assert!(e.starts_with("unknown variant `boots`"), "{e}");
}

#[test]
fn several_processes_can_create_the_same_ledger_at_once() {
    let db = TempDb::new();
    let opened: Vec<bool> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..4)
            .map(|_| s.spawn(|| Ledger::open(&db.0).is_ok()))
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert_eq!(opened, [true; 4]);
    assert_eq!(pragma(&db.0, "user_version"), json!(SCHEMA_VERSION));
}
