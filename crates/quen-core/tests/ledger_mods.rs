//! Mod fingerprints in the ledger: migration 0002, labels and load status (docs/intent.md, "Mod sets").

mod ledger_common;

use ledger_common::{TempDb, boot, game, rows, sha};
use quen_core::ledger::{
    BootModStatus, Error, Ledger, LoadStatus, ModVersion, Provenance, SCHEMA_VERSION,
};
use quen_core::mods::{
    ConfigSource, HashCache, Kind, MemSource, ModEntry, ModSpec, fingerprint, set_hash,
};
use rusqlite::Connection;
use serde_json::{Value, json};

fn refused(r: Result<(), Error>, needle: &str) {
    match r {
        Err(Error::Refused(why)) => assert!(why.contains(needle), "{why:?} lacks {needle:?}"),
        other => panic!("expected a refusal about {needle:?}, got {other:?}"),
    }
}

#[test]
fn a_0001_ledger_migrates_in_place() {
    let db = TempDb::new();
    let old = Connection::open(&db.0).unwrap();
    old.execute_batch(include_str!("../../../schema/0001_init.sql"))
        .unwrap();
    old.pragma_update(None, "user_version", 1).unwrap();
    old.execute_batch(&format!(
        "INSERT INTO games (id, name, exe_path, exe_sha256) VALUES (1, 'synthetic', '%GAME_DIR%/bin/x64/game.exe', '{}');
         INSERT INTO mod_sets (id, sha256) VALUES (1, '{}');
         INSERT INTO mod_set_entries (mod_set_id, name, kind, sha256, parked)
           VALUES (1, 'modAlpha', 'mod', '{}', 0), (1, 'Tweaks.asi', 'asi', '{}', 1);
         INSERT INTO boots (id, game_id, t_start, mod_set_id) VALUES (1, 1, '2026-10-10T10:00:00Z', 1);",
        sha(1),
        sha(2),
        sha(3),
        sha(4)
    ))
    .unwrap();
    drop(old);

    let mut l = Ledger::open(&db.0).unwrap();
    let version = rows(&l, "SELECT user_version FROM pragma_user_version", &[]);
    assert_eq!(version, vec![vec![json!(SCHEMA_VERSION)]]);
    assert_eq!(SCHEMA_VERSION, 13);
    let entries = rows(
        &l,
        "SELECT mod_set_id, kind, name, parked, content_hash, home, file_count, bytes, config_hash, config_values,
           version_label, provenance
         FROM mod_set_entries ORDER BY name",
        &[],
    );
    let legacy = |kind: &str, name: &str, parked: i64, hash: u32| {
        let null = Value::Null;
        vec![
            json!(1),
            json!(kind),
            json!(name),
            json!(parked),
            json!(sha(hash)),
            null.clone(),
            null.clone(),
            null.clone(),
            null.clone(),
            json!("{}"),
            null,
            json!("unknown"),
        ]
    };
    assert_eq!(
        entries,
        [
            legacy("asi", "Tweaks.asi", 1, 4),
            legacy("mod", "modAlpha", 0, 3)
        ],
        "rows keep their values, free-text kinds included"
    );
    assert_eq!(
        rows(&l, "SELECT mod_set_id FROM boots", &[]),
        vec![vec![json!(1)]]
    );
    assert_eq!(
        rows(&l, "SELECT count(*) FROM pragma_foreign_key_check", &[]),
        vec![vec![json!(0)]]
    );

    // The new key: one name under two kinds.
    let entry = |kind| ModEntry {
        kind,
        name: "W3Perf".into(),
        home: "%GAME_DIR%/bin/x64/W3Perf".into(),
        parked: false,
        content_hash: sha(5),
        file_count: 1,
        bytes: 4,
        config_hash: Some(sha(6)),
        config_values: [("TlasMask".to_string(), "1".to_string())].into(),
    };
    let set = l
        .add_mod_set(&[entry(Kind::Asi), entry(Kind::Addon)])
        .unwrap();
    assert_eq!(set, 2);
    l.add_boot_mod_status(&BootModStatus {
        boot_id: 1,
        kind: Kind::Asi,
        name: "Tweaks".into(),
        status: LoadStatus::NotPresent,
        reason: None,
    })
    .unwrap();
    drop(l);

    let raw = Connection::open(&db.0).unwrap();
    assert!(
        raw.execute(
            "INSERT INTO mod_set_entries (mod_set_id, kind, name, parked, home, content_hash, file_count, bytes)
             VALUES (2, 'mod', 'modBeta', 0, 'x', ?1, 1, 1)",
            [sha(7)],
        )
        .is_err(),
        "only rows from 0001 keep a free-text kind"
    );
    drop(raw);
    let l = Ledger::open(&db.0).unwrap();
    assert_eq!(
        rows(&l, "SELECT count(*) FROM mod_set_entries", &[]),
        vec![vec![json!(4)]],
        "reopening doesn't migrate again"
    );
}

#[test]
fn only_a_known_build_gets_a_label() {
    let mut src = MemSource::default();
    src.put("GAME/bin/x64/W3ElementMap.asi", "element map 1.2");
    src.put(
        "GAME/bin/x64/W3ElementMap.ini",
        "[General]\nMode=raise\nRaisePercent=40\n",
    );
    src.put("GAME/bin/x64/W3Perf.asi", "perf, local build");
    src.put("GAME/Mods/modAlpha/content/blob0.bundle", "bundle");
    src.put("GAME/Mods/modAlpha/modAlpha.log", "boot 1");
    let spec = |kind, name: &str, home: &str, config| ModSpec {
        kind,
        name: name.into(),
        home: home.into(),
        parked: false,
        ignore: vec![],
        config,
    };
    let ini = ConfigSource {
        path: "GAME/bin/x64/W3ElementMap.ini".into(),
        section: None,
        keys: vec!["Mode".into(), "RaisePercent".into()],
    };
    let specs = [
        spec(
            Kind::Asi,
            "W3ElementMap",
            "GAME/bin/x64/W3ElementMap.asi",
            vec![ini],
        ),
        spec(Kind::Asi, "W3Perf", "GAME/bin/x64/W3Perf.asi", vec![]),
        spec(Kind::ModsFolder, "modAlpha", "GAME/Mods/modAlpha", vec![]),
    ];
    let mut cache = HashCache::default();
    let entries: Vec<ModEntry> = specs
        .iter()
        .map(|s| fingerprint(&src, &mut cache, s).unwrap())
        .collect();

    let mut l = Ledger::open_in_memory().unwrap();
    l.add_mod_version(&ModVersion {
        content_hash: entries[0].content_hash.clone(),
        kind: Kind::Asi,
        name: "W3ElementMap".into(),
        version: "1.2".into(),
        provenance: Provenance::NexusFile,
        source: "W3ElementMap-1-2.zip".into(),
    })
    .unwrap();
    let set = l.add_mod_set(&entries).unwrap();
    let reversed: Vec<ModEntry> = entries.iter().rev().cloned().collect();
    assert_eq!(l.add_mod_set(&reversed).unwrap(), set);
    assert_eq!(
        rows(&l, "SELECT sha256 FROM mod_sets", &[]),
        vec![vec![json!(set_hash(&entries).unwrap())]]
    );
    let labels = |l: &Ledger| {
        rows(
            l,
            "SELECT kind || ':' || name, version_label, provenance FROM mod_set_entries ORDER BY 1",
            &[],
        )
    };
    assert_eq!(
        labels(&l),
        vec![
            vec![json!("asi:W3ElementMap"), json!("1.2"), json!("nexus_file")],
            vec![json!("asi:W3Perf"), Value::Null, json!("unknown")],
            vec![json!("mods:modAlpha"), Value::Null, json!("unknown")],
        ],
        "an unknown hash gets no label"
    );
    let stored = rows(
        &l,
        "SELECT home, content_hash, file_count, bytes, config_hash, json_extract(config_values, '$.RaisePercent')
         FROM mod_set_entries WHERE name = 'W3ElementMap'",
        &[],
    );
    let e = &entries[0];
    assert_eq!(
        stored,
        vec![vec![
            json!("GAME/bin/x64/W3ElementMap.asi"),
            json!(e.content_hash),
            json!(1),
            json!(15),
            json!(e.config_hash),
            json!("40"),
        ]]
    );

    // Labeling a build later labels the entries already stored with it.
    let perf = ModVersion {
        content_hash: entries[1].content_hash.clone(),
        kind: Kind::Asi,
        name: "W3Perf".into(),
        version: "0.3.0-dev".into(),
        provenance: Provenance::GitCommit,
        source: "0123abcd".into(),
    };
    l.add_mod_version(&perf).unwrap();
    l.add_mod_version(&perf).unwrap();
    refused(
        l.add_mod_version(&ModVersion {
            version: "0.4.0".into(),
            ..perf.clone()
        }),
        "different label",
    );
    assert_eq!(
        labels(&l)[1],
        [json!("asi:W3Perf"), json!("0.3.0-dev"), json!("git_commit")]
    );
    assert_eq!(
        rows(&l, "SELECT version FROM mod_versions ORDER BY 1", &[]),
        vec![vec![json!("0.3.0-dev")], vec![json!("1.2")]]
    );
    assert!(
        l.add_mod_version(&ModVersion {
            content_hash: "ABC".into(),
            ..perf
        })
        .is_err(),
        "a content hash is 64 lowercase hex"
    );
}

#[test]
fn a_boot_records_what_each_mod_did_at_load() {
    let mut l = Ledger::open_in_memory().unwrap();
    let g = game(&mut l);
    let b = boot(&mut l, g, "2026-10-10T10:00:00Z");
    let status = |name: &str, status, reason: Option<&str>| BootModStatus {
        boot_id: b,
        kind: Kind::Asi,
        name: name.into(),
        status,
        reason: reason.map(String::from),
    };
    l.add_boot_mod_status(&status("W3ElementMap", LoadStatus::Loaded, None))
        .unwrap();
    l.add_boot_mod_status(&status(
        "W3Perf",
        LoadStatus::Refused,
        Some("REFUSED: game code differs"),
    ))
    .unwrap();
    assert!(
        l.add_boot_mod_status(&status("Tweaks", LoadStatus::Refused, None))
            .is_err(),
        "a refusal needs its reason"
    );
    assert!(
        l.add_boot_mod_status(&status("W3ElementMap", LoadStatus::NotPresent, None))
            .is_err(),
        "one status per mod and boot"
    );
    assert!(
        l.add_boot_mod_status(&BootModStatus {
            boot_id: 999,
            ..status("Tweaks", LoadStatus::Loaded, None)
        })
        .is_err(),
        "the boot must exist"
    );
    let parked: BootModStatus = serde_json::from_value(json!({
        "boot_id": b, "kind": "addon", "name": "renodx", "status": "not_present"
    }))
    .unwrap();
    l.add_boot_mod_status(&parked).unwrap();
    assert!(
        serde_json::from_value::<BootModStatus>(json!({
            "boot_id": b, "kind": "asi", "name": "x", "status": "crashed"
        }))
        .is_err()
    );
    assert_eq!(
        rows(
            &l,
            "SELECT kind, name, status, reason FROM boot_mod_status ORDER BY name",
            &[]
        ),
        vec![
            vec![
                json!("asi"),
                json!("W3ElementMap"),
                json!("loaded"),
                Value::Null
            ],
            vec![
                json!("asi"),
                json!("W3Perf"),
                json!("refused"),
                json!("REFUSED: game code differs")
            ],
            vec![
                json!("addon"),
                json!("renodx"),
                json!("not_present"),
                Value::Null
            ],
        ]
    );
}

#[test]
fn a_mod_set_waits_for_another_writer_instead_of_failing() {
    // A deferred transaction that has read can't take the write lock from a busy writer: SQLite returns BUSY at once,
    // without the busy timeout. add_mod_set reads first, so it takes the write lock up front and waits.
    let db = TempDb::new();
    let mut l = Ledger::open(&db.0).unwrap();
    let other = Connection::open(&db.0).unwrap();
    other
        .execute_batch(&format!(
            "BEGIN IMMEDIATE; INSERT INTO mod_sets (sha256) VALUES ('{}');",
            sha(9)
        ))
        .unwrap();
    let writer = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(300));
        other.execute_batch("COMMIT").unwrap();
    });
    let entry = ModEntry {
        kind: Kind::Asi,
        name: "W3Perf".into(),
        home: "%GAME_DIR%/bin/x64/W3Perf".into(),
        parked: false,
        content_hash: sha(5),
        file_count: 1,
        bytes: 4,
        config_hash: None,
        config_values: Default::default(),
    };
    let set = l.add_mod_set(&[entry]);
    writer.join().unwrap();
    assert_eq!(set.unwrap(), 2, "after the other writer's set");
}
