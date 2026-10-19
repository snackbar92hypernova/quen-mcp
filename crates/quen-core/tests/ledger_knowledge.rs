//! Game knowledge (schema 0009) on synthetic fixtures: the KB scan, the KB claim import, the REA
//! bundle import (the extraction cache), `about` and the KB export. Every import, repeated, adds nothing.

mod ledger_common;

use std::fs;

use ledger_common::{TempDb, commit, ev, fake_bundle, fake_kb, profile, rows, sha};
use quen_core::ledger::{About, Error, Game, Ledger, extraction_key};
use serde_json::{Value, json};

/// A ledger with the synthetic game `exe` (sha(1)), and the fixture's folder.
fn ledger() -> (TempDb, Ledger) {
    let db = TempDb::new();
    let mut l = Ledger::open(&db.0).unwrap();
    l.add_game(&Game {
        name: "synthetic".into(),
        exe_path: "%GAME_DIR%/bin/x64/game.exe".into(),
        exe_sha256: sha(1),
        build_stamp: None,
    })
    .unwrap();
    (db, l)
}

fn refused<T: std::fmt::Debug>(r: Result<T, Error>, needle: &str) {
    match r {
        Err(Error::Refused(why)) => assert!(why.contains(needle), "{why}"),
        other => panic!("expected a refusal with {needle:?}, got {other:?}"),
    }
}

/// Every row of the knowledge tables, in a fixed order: a rescan or re-import must leave them as they were.
fn snapshot(l: &Ledger) -> Vec<Vec<Vec<Value>>> {
    [
        "SELECT id, game, kind, key, label FROM entities ORDER BY id",
        "SELECT entity_id, game_id, rva, offset, kb_source, confidence FROM entity_addresses ORDER BY 1, 2",
        "SELECT id, src_id, rel, dst_id, claim_id, extraction_id, kb_source FROM links ORDER BY id",
        "SELECT id, path, anchor, heading, level, parent_id, line_start, line_end, sha256, kb_commit, gone_at
         FROM kb_sections ORDER BY id",
        "SELECT section_id, entity_id, n FROM kb_mentions ORDER BY 1, 2",
        "SELECT id, key, text, scope, confidence, review FROM claims ORDER BY id",
        "SELECT claim_id, entity_id FROM claim_subjects ORDER BY 1, 2",
        "SELECT id, claim_id, kb_path FROM re_refs ORDER BY id",
        "SELECT id, path, sha256, kind, method, pinned FROM captures ORDER BY id",
        "SELECT * FROM extraction_profiles ORDER BY digest",
        "SELECT id, evidence_id, key_sha256, rva, entity_id, capture_id FROM extractions ORDER BY id",
    ]
    .iter()
    .map(|sql| rows(l, sql, &[]))
    .collect()
}

#[test]
fn a_kb_scan_mirrors_the_tables_indexes_the_notes_and_repeats_as_a_no_op() {
    let (db, mut l) = ledger();
    let kb = fake_kb(db.dir(), &sha(1));
    let scan = l.scan_kb(&kb).unwrap();
    assert_eq!(scan.commit, commit());
    assert_eq!(
        (scan.game.as_str(), scan.build.as_str()),
        ("synthetic", sha(1).as_str())
    );
    assert_eq!(
        (
            scan.names,
            scan.hooks,
            scan.patches,
            scan.cvar_layouts,
            scan.rows_unknown_build
        ),
        (4, 2, 1, 1, 1)
    );
    assert_eq!((scan.notes, scan.links), (2, 1));

    let entities = rows(
        &l,
        "SELECT kind, key, label FROM entities ORDER BY kind, key",
        &[],
    );
    let e =
        |kind: &str, key: &str, label: Option<&str>| vec![json!(kind), json!(key), json!(label)];
    let mut want = vec![
        e("function", "Alpha_Draw", None),
        e("function", "Beta_Setup", None),
        e("function", "fn_0BADF00D_3000", None),
        e("hook", "ModA:hook:Alpha_Draw", Some("draw hook")),
        e("hook", "ModB:mid-hook:0BADF00D_1234", Some("inside alpha")),
        e("label", "Gamma_Table", None),
        e("patch", "cull_off", None),
    ];
    want.extend(
        ["default", "flags", "group", "live", "name", "value"]
            .map(|f| e("field", &format!("cvar_object.{f}"), None)),
    );
    want.sort_by_key(|r| (r[0].to_string(), r[1].to_string()));
    assert_eq!(entities, want);
    let source = |file: &str| json!(format!("{file}@{}", commit()));
    assert_eq!(
        rows(
            &l,
            "SELECT e.key, a.rva, a.offset, a.kb_source, a.confidence FROM entity_addresses a
             JOIN entities e ON e.id = a.entity_id WHERE e.key IN ('Alpha_Draw', 'Beta_Setup', 'cvar_object.value')
             ORDER BY e.key",
            &[]
        ),
        [
            vec![json!("Alpha_Draw"), json!(0x1000), Value::Null, source("names.tsv"), json!("O")],
            vec![json!("Beta_Setup"), json!(0x2000), Value::Null, source("names.tsv"), json!("I")],
            vec![json!("cvar_object.value"), Value::Null, json!(0x20), source("cvar_layout.tsv"), Value::Null],
        ]
    );
    // A hook at a function's start hooks it; one inside a function is keyed by its site, unlinked.
    assert_eq!(
        rows(
            &l,
            "SELECT src_key, rel, dst_key, basis, kb_source FROM links_evidence",
            &[]
        ),
        [[
            json!("ModA:hook:Alpha_Draw"),
            json!("hooks"),
            json!("Alpha_Draw"),
            json!("kb"),
            source("hooks.tsv")
        ]]
    );

    // Sections: the preamble, H2s and H3s with their parents; a fenced "##" is no heading.
    let sections = rows(
        &l,
        "SELECT s.path, s.anchor, s.level, p.anchor, s.line_start, s.line_end, s.kb_commit FROM kb_sections s
         LEFT JOIN kb_sections p ON p.id = s.parent_id ORDER BY s.path, s.line_start",
        &[],
    );
    let sec = |path: &str, anchor: &str, level: i64, parent: Option<&str>, a: i64, b: i64| {
        vec![
            json!(path),
            json!(anchor),
            json!(level),
            json!(parent),
            json!(a),
            json!(b),
            json!(commit()),
        ]
    };
    assert_eq!(
        sections,
        [
            sec("frame/MAP.md", "", 1, None, 1, 2),
            sec("frame/MAP.md", "2-colour-chain", 2, None, 3, 4),
            sec(
                "frame/MAP.md",
                "2-colour-chain/2d-tonemap-seg-9",
                3,
                Some("2-colour-chain"),
                5,
                11
            ),
            sec(
                "frame/MAP.md",
                "2-colour-chain/2f-composite-0x2b3c4d5e-v-c-all-h-for-code",
                3,
                Some("2-colour-chain"),
                12,
                18
            ),
            sec(
                "frame/MAP.md",
                "2-colour-chain/2g-limits",
                3,
                Some("2-colour-chain"),
                19,
                24
            ),
            sec("frame/MAP.md", "3-controls", 2, None, 25, 26),
            sec("notes/alpha.md", "", 1, None, 1, 4),
            sec("notes/alpha.md", "q1-allocation", 2, None, 5, 7),
            sec(
                "notes/alpha.md",
                "q1-allocation/detail-v",
                3,
                Some("q1-allocation"),
                8,
                12
            ),
        ]
    );
    // Mentions: names, RVAs, VAs and Ghidra names of known functions, a site-keyed hook as written; unknown hex never.
    let mentions = rows(
        &l,
        "SELECT s.anchor, e.key, m.n FROM kb_mentions m JOIN kb_sections s ON s.id = m.section_id
         JOIN entities e ON e.id = m.entity_id WHERE s.path = 'notes/alpha.md' ORDER BY s.line_start, e.key",
        &[],
    );
    assert_eq!(
        mentions,
        [
            vec![json!(""), json!("Alpha_Draw"), json!(3)],
            vec![json!("q1-allocation"), json!("Beta_Setup"), json!(3)],
            vec![
                json!("q1-allocation/detail-v"),
                json!("Alpha_Draw"),
                json!(1)
            ],
            vec![
                json!("q1-allocation/detail-v"),
                json!("ModB:mid-hook:0BADF00D_1234"),
                json!(1)
            ],
        ]
    );
    assert_eq!(
        rows(
            &l,
            "SELECT count(*) FROM entities WHERE key LIKE '%CAFEBABE%'",
            &[]
        ),
        [[json!(0)]]
    );

    // The same KB again: nothing changes.
    let before = snapshot(&l);
    let again = l.scan_kb(&kb).unwrap();
    assert_eq!(
        (again.sections_new, again.sections_gone, again.entities_new),
        (0, 0, 0)
    );
    assert_eq!(snapshot(&l), before);

    // An edit at a new commit: a renamed heading is a new section, the old one gone; a removed hook loses its link.
    fs::write(db.dir().join("repo/.git/refs/heads/main"), "d".repeat(40)).unwrap();
    let note = kb.join("notes/alpha.md");
    fs::write(
        &note,
        fs::read_to_string(&note)
            .unwrap()
            .replace("Q1 Allocation", "Q1 Pools"),
    )
    .unwrap();
    let hooks = kb.join("hooks.tsv");
    let text = fs::read_to_string(&hooks).unwrap();
    fs::write(
        &hooks,
        text.lines()
            .filter(|l| !l.contains("ModA"))
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();
    let edited = l.scan_kb(&kb).unwrap();
    assert_eq!(
        (edited.sections_new, edited.sections_gone, edited.links),
        (2, 2, 0)
    );
    assert_eq!(rows(&l, "SELECT count(*) FROM links", &[]), [[json!(0)]]);
    let gone = rows(
        &l,
        "SELECT anchor FROM kb_sections WHERE gone_at IS NOT NULL ORDER BY anchor",
        &[],
    );
    assert_eq!(
        gone,
        [[json!("q1-allocation")], [json!("q1-allocation/detail-v")]]
    );
    assert_eq!(
        rows(
            &l,
            "SELECT DISTINCT kb_commit FROM kb_sections WHERE gone_at IS NULL",
            &[]
        ),
        [[json!("d".repeat(40))]]
    );
    assert_eq!(l.kb_index().unwrap().unwrap().0, "d".repeat(40));
}

#[test]
fn a_kb_scan_refuses_before_writing_anything() {
    let (db, mut l) = ledger();
    let kb = fake_kb(db.dir(), &sha(2));
    refused(l.scan_kb(&kb), "no game row");
    let kb = fake_kb(db.dir(), &sha(1));
    let names = kb.join("names.tsv");
    let good = fs::read_to_string(&names).unwrap();
    fs::write(
        &names,
        format!("{good}0x5000\tAlpha_Draw\tfunction\tO\ttest\t2026-10-18\n"),
    )
    .unwrap();
    refused(l.scan_kb(&kb), "Alpha_Draw is defined at line");
    fs::write(&names, good.replace(&sha(1), &sha(3))).unwrap();
    refused(l.scan_kb(&kb), "one KB, one build");
    fs::write(&names, &good).unwrap();
    fs::remove_dir_all(db.dir().join("repo/.git")).unwrap();
    refused(l.scan_kb(&kb), "git work tree");
    assert_eq!(
        rows(
            &l,
            "SELECT (SELECT count(*) FROM entities) + (SELECT count(*) FROM kb_sections)",
            &[]
        ),
        [[json!(0)]]
    );
}

#[test]
fn table_rows_of_a_kb_note_become_keyed_graded_claims_once() {
    let (db, mut l) = ledger();
    let kb = fake_kb(db.dir(), &sha(1));
    let wanted = ["2d", "2f", "2g"].map(String::from);
    let scope = Some("HDR on; synthetic capture");
    let first = l
        .import_kb_claims(&kb, "frame/MAP.md", &wanted, scope)
        .unwrap();
    assert_eq!(
        (first.rows, first.claims_new.len(), first.already),
        (5, 5, 0)
    );
    let claims = rows(
        &l,
        "SELECT key, text, scope, confidence, review FROM claims ORDER BY id",
        &[],
    );
    let at = |anchor: &str, text: &str| {
        let key = format!(
            "frame/MAP.md#2-colour-chain/{anchor}/{}",
            &quen_core::journal::sha256_hex(text.as_bytes())[..12]
        );
        json!(key)
    };
    let c = |anchor: &str, text: &str, confidence: Option<&str>, review: Option<&str>| {
        vec![
            at(anchor, text),
            json!(text),
            json!(scope),
            json!(confidence),
            json!(review),
        ]
    };
    let composite = "2f-composite-0x2b3c4d5e-v-c-all-h-for-code";
    assert_eq!(
        claims,
        [
            c(
                "2d-tonemap-seg-9",
                "10 | **0x1A2B3C4D** tonemap | clips above c8.w | (s1)",
                None,
                Some("C")
            ),
            c(
                "2d-tonemap-seg-9",
                "11 | 0x00C0FFEE glare | darkens >1",
                Some("H"),
                Some("V")
            ),
            c(
                composite,
                "0 | `min(rgb*c0.x, c0.w)` per channel | hard clip",
                Some("H"),
                Some("C")
            ),
            c(composite, "1 | PQ encode, `|x|` | -", Some("H"), Some("C")),
            c(
                "2g-limits",
                "1 | 0x1A2B3C4D | above c8.w discarded",
                None,
                Some("V")
            ),
        ]
    );
    // Subjects: the shader hashes each row and its section's heading name; a re_refs row to the section.
    let subjects = rows(
        &l,
        "SELECT s.claim_id, e.key FROM claim_subjects s JOIN entities e ON e.id = s.entity_id ORDER BY 1, 2",
        &[],
    );
    let ids = &first.claims_new;
    assert_eq!(
        subjects,
        [
            vec![json!(ids[0]), json!("0x1A2B3C4D")],
            vec![json!(ids[1]), json!("0x00C0FFEE")],
            vec![json!(ids[2]), json!("0x2B3C4D5E")],
            vec![json!(ids[3]), json!("0x2B3C4D5E")],
            vec![json!(ids[4]), json!("0x1A2B3C4D")],
        ]
    );
    assert_eq!(
        rows(
            &l,
            "SELECT kb_path FROM re_refs WHERE claim_id = ?1",
            &[json!(ids[4])]
        ),
        [[json!("frame/MAP.md#2-colour-chain/2g-limits")]]
    );
    // Imported, never proven: no support.
    assert_eq!(
        rows(
            &l,
            "SELECT sum(proven), sum(families) FROM claims_status",
            &[]
        ),
        [[json!(0), json!(0)]]
    );

    // Again: nothing added. An edited row: a new claim, and the old one listed stale (left open).
    let before = snapshot(&l);
    let again = l
        .import_kb_claims(&kb, "frame/MAP.md", &wanted, scope)
        .unwrap();
    assert_eq!((again.claims_new.len(), again.already), (0, 5));
    assert_eq!(snapshot(&l), before);
    let map = kb.join("frame/MAP.md");
    fs::write(
        &map,
        fs::read_to_string(&map)
            .unwrap()
            .replace("darkens >1", "darkens >1.0"),
    )
    .unwrap();
    let edited = l
        .import_kb_claims(&kb, "frame/MAP.md", &wanted, scope)
        .unwrap();
    assert_eq!((edited.claims_new.len(), edited.already), (1, 4));
    assert_eq!(
        edited.stale,
        [at("2d-tonemap-seg-9", "11 | 0x00C0FFEE glare | darkens >1")
            .as_str()
            .unwrap()]
    );

    refused(
        l.import_kb_claims(&kb, "frame/MAP.md", &["9z".into()], None),
        "no section \"9z\"",
    );
    refused(
        l.import_kb_claims(&kb, "../MAP.md", &wanted, None),
        "relative to the KB root",
    );
    refused(
        l.import_kb_claims(&kb, "frame/MAP.md", &["3".into()], None),
        "no table rows",
    );
}

#[test]
fn an_rea_bundle_is_copied_pinned_indexed_and_imported_once() {
    let (db, mut l) = ledger();
    let kb = fake_kb(db.dir(), &sha(1));
    l.scan_kb(&kb).unwrap();
    let root = db.dir().join("captures");
    let outside = fake_bundle(&db.dir().join("export/rea-s1.json"), &sha(1), 1, 10);
    let import = l.import_rea_bundle(&root, &outside, "6.5.0").unwrap();
    let bytes = fs::read(&outside).unwrap();
    let file_sha = quen_core::journal::sha256_hex(&bytes);
    assert_eq!(import.path, format!("re/rea-{}.json", &file_sha[..16]));
    assert!(import.copied);
    assert_eq!(
        fs::read(root.join(&import.path)).unwrap(),
        bytes,
        "the copy is the file"
    );
    assert_eq!(
        (
            import.records,
            import.extractions_new,
            import.skipped,
            import.profiles_new,
            import.links_new
        ),
        (3, 2, 1, 1, 1)
    );
    assert_eq!(
        rows(
            &l,
            "SELECT path, sha256, bytes, kind, method, pinned, boot_id FROM captures",
            &[]
        ),
        [[
            json!(import.path),
            json!(file_sha),
            json!(bytes.len()),
            json!("json"),
            json!("rea"),
            json!(1),
            Value::Null
        ]]
    );
    let params = json!({"document": "game.exe", "procedure": "0x140001000"});
    let key = extraction_key(&sha(1), &profile(1), "procedure_pseudo_code", &params);
    assert_eq!(
        rows(
            &l,
            "SELECT x.evidence_id, x.tool, x.tool_version, x.operation, x.params, x.key_sha256, x.rva, e.key, x.confidence
             FROM extractions x LEFT JOIN entities e ON e.id = x.entity_id ORDER BY x.id",
            &[]
        ),
        [
            vec![
                json!(ev(10)),
                json!("rea"),
                json!("6.5.0"),
                json!("procedure_pseudo_code"),
                json!(r#"{"document":"game.exe","procedure":"0x140001000"}"#),
                json!(key),
                json!(0x1000),
                json!("Alpha_Draw"),
                json!("observed"),
            ],
            vec![
                json!(ev(11)),
                json!("rea"),
                json!("6.5.0"),
                json!("procedure_callers"),
                json!(r#"{"document":"game.exe","procedure":"0x140001000"}"#),
                json!(extraction_key(&sha(1), &profile(1), "procedure_callers", &params)),
                json!(0x1000),
                json!("Alpha_Draw"),
                json!("observed"),
            ],
        ]
    );
    // Callers: the known caller only; the unknown address stays in the record.
    assert_eq!(
        rows(
            &l,
            "SELECT src_key, rel, dst_key, basis, evidence_id, confidence FROM links_evidence WHERE basis = 'extraction'",
            &[]
        ),
        [[
            json!("Beta_Setup"),
            json!("calls"),
            json!("Alpha_Draw"),
            json!("extraction"),
            json!(ev(11)),
            json!("observed")
        ]]
    );
    // No decompiled text in the ledger: only its hash.
    let text = rows(
        &l,
        "SELECT count(*) FROM extractions WHERE params LIKE '%synthetic%' OR operation LIKE '%void%'",
        &[],
    );
    assert_eq!(text, [[json!(0)]]);

    // Again, from outside or from its copy: nothing added, nothing copied.
    let before = snapshot(&l);
    for path in [outside.clone(), root.join(&import.path)] {
        let again = l.import_rea_bundle(&root, &path, "6.5.1").unwrap();
        assert_eq!(
            (again.extractions_new, again.copied, again.capture_id),
            (0, false, import.capture_id)
        );
    }
    assert_eq!(snapshot(&l), before);
    assert_eq!(fs::read_dir(root.join("re")).unwrap().count(), 1);

    // A bundle exported under the capture root stays where it is.
    let inside = fake_bundle(&root.join("re/s2/rea-a.json"), &sha(1), 2, 20);
    let second = l.import_rea_bundle(&root, &inside, "6.5.0").unwrap();
    assert_eq!(
        (second.path.as_str(), second.copied, second.profiles_new),
        ("re/s2/rea-a.json", false, 1)
    );

    // Refused, nothing written.
    let n = rows(&l, "SELECT count(*) FROM extractions", &[]);
    let bad = db.dir().join("bad.json");
    for (text, why) in [
        ("[]", "no records list"),
        (
            r#"{"records": [{"evidence_id": "ev_x"}]}"#,
            "record 0: evidence_id",
        ),
    ] {
        fs::write(&bad, text).unwrap();
        refused(l.import_rea_bundle(&root, &bad, "6.5.0"), why);
    }
    let twice = fs::read_to_string(&outside)
        .unwrap()
        .replace(&ev(11), &ev(10));
    fs::write(&bad, twice).unwrap();
    refused(l.import_rea_bundle(&root, &bad, "6.5.0"), "appears twice");
    refused(l.import_rea_bundle(&root, &outside, " "), "REA version");
    assert_eq!(rows(&l, "SELECT count(*) FROM extractions", &[]), n);
}

#[test]
fn about_gathers_everything_known_and_says_whether_it_was_extracted() {
    let (db, mut l) = ledger();
    let kb = fake_kb(db.dir(), &sha(1));
    let root = db.dir().join("captures");
    // The bundle first: its extractions find their function through its address once the KB names it.
    l.import_rea_bundle(
        &root,
        &fake_bundle(&db.dir().join("a.json"), &sha(1), 1, 10),
        "6.5.0",
    )
    .unwrap();
    l.scan_kb(&kb).unwrap();
    l.import_rea_bundle(
        &root,
        &fake_bundle(&db.dir().join("b.json"), &sha(1), 2, 20),
        "6.5.0",
    )
    .unwrap();
    let claims = l
        .import_kb_claims(&kb, "frame/MAP.md", &["2g".into()], None)
        .unwrap();

    let ask = |q: Value| {
        l.about(&serde_json::from_value::<About>(q).unwrap())
            .unwrap()
    };
    let about = ask(json!({"kind": "function", "key": "Alpha_Draw"}));
    assert_eq!(about["entity"]["key"], "Alpha_Draw");
    assert_eq!(about["addresses"][0]["rva"], "0x1000");
    assert_eq!(about["addresses"][0]["basis"], "kb");
    let links: Vec<(&str, &str, &str)> = about["links"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| {
            (
                l["rel"].as_str().unwrap(),
                l["direction"].as_str().unwrap(),
                l["other"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        links,
        [
            ("calls", "in", "function:Beta_Setup"),
            ("hooks", "in", "hook:ModA:hook:Alpha_Draw")
        ]
    );
    let sections: Vec<&str> = about["sections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["anchor"].as_str().unwrap())
        .collect();
    assert_eq!(sections, ["", "q1-allocation/detail-v"]);
    assert_eq!(about["extracted"], true);
    // Four records at its address: two per profile; no profile asked, so none exact.
    let n = |group: &str| about["extractions"][group].as_array().unwrap().len();
    assert_eq!((n("exact"), n("near"), n("gone")), (0, 4, 0));

    // The cache key asked for: one exact hit, the other profile's same query near.
    let params = json!({"document": "game.exe", "procedure": "0x140001000"});
    let hit = ask(
        json!({"build": sha(1), "rva": "0x1000", "operation": "procedure_pseudo_code", "params": params,
                         "profile": profile(1)}),
    );
    assert_eq!(hit["entity"]["key"], "Alpha_Draw");
    let key = extraction_key(&sha(1), &profile(1), "procedure_pseudo_code", &params);
    assert_eq!(hit["extractions"]["key_sha256"], json!(key));
    let exact = &hit["extractions"]["exact"];
    assert_eq!(
        (
            exact[0]["evidence_id"].clone(),
            exact[0]["key_sha256"].clone()
        ),
        (json!(ev(10)), json!(key))
    );
    assert_eq!(
        exact[0]["seeds"],
        json!(json!({"path": "%KB%/seeds/synthetic.tsv", "sha256": sha(0x5eed + 1)}).to_string())
    );
    assert_eq!(hit["extractions"]["near"][0]["evidence_id"], json!(ev(20)));
    assert_eq!(exact.as_array().unwrap().len(), 1);

    // A shader the claim import named: its claim with its grades and standing.
    let shader = ask(json!({"kind": "shader", "key": "0x1A2B3C4D"}));
    assert_eq!(shader["claims"][0]["id"], json!(claims.claims_new[0]));
    assert_eq!(
        (
            shader["claims"][0]["review"].clone(),
            shader["claims"][0]["proven"].clone()
        ),
        (json!("V"), json!(0))
    );
    // Nothing known is an empty answer; asking about nothing is refused.
    let none = ask(json!({"kind": "function", "key": "Nobody"}));
    assert_eq!(
        (none["entity"].clone(), none["extracted"].clone()),
        (Value::Null, json!(false))
    );
    refused(l.about(&About::default()), "kind and key, or build and rva");
    refused(
        l.about(&About {
            rva: Some("0x1000".into()),
            ..Default::default()
        }),
        "rva needs build",
    );
}

#[test]
fn the_kb_export_lists_keyed_claims_and_their_links_sorted() {
    let (db, mut l) = ledger();
    let kb = fake_kb(db.dir(), &sha(1));
    l.scan_kb(&kb).unwrap();
    l.import_kb_claims(
        &kb,
        "frame/MAP.md",
        &["2g".into(), "2d".into()],
        Some("a\tscope"),
    )
    .unwrap();
    l.add_claim(&quen_core::ledger::Claim {
        text: "unkeyed".into(),
        scope: None,
    })
    .unwrap();
    let (claims, links) = l.kb_export().unwrap();
    let lines: Vec<&str> = claims.lines().collect();
    assert_eq!(
        lines[0],
        "key\ttext\tscope\tstatus\twithdrawn_reason\tconfidence\treview\tproven\tfamilies\tsubjects\tkb_refs"
    );
    assert_eq!(lines.len(), 4, "the three keyed claims only:\n{claims}");
    let mut keys: Vec<&str> = lines[1..]
        .iter()
        .map(|l| l.split('\t').next().unwrap())
        .collect();
    let sorted = {
        let mut k = keys.clone();
        k.sort();
        k
    };
    assert_eq!(keys, sorted);
    keys.dedup();
    assert_eq!(keys.len(), 3);
    let row: Vec<&str> = lines
        .iter()
        .find(|l| l.contains("2g-limits"))
        .unwrap()
        .split('\t')
        .collect();
    assert_eq!(
        &row[1..],
        [
            "1 | 0x1A2B3C4D | above c8.w discarded",
            r"a\tscope",
            "open",
            "",
            "",
            "V",
            "0",
            "0",
            "shader:0x1A2B3C4D",
            "frame/MAP.md#2-colour-chain/2g-limits"
        ]
    );
    assert_eq!(links, "src\trel\tdst\tclaim\n", "no claim-backed links yet");
}
