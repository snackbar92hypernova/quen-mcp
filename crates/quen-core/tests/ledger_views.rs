//! The investigation views (schema 0005) and `ledger::schema_description`: the description names
//! every table, view and column, and each of its example queries answers its question on a synthetic ledger.

mod ledger_common;

use std::collections::BTreeSet;

use ledger_common::{rows, sha};
use quen_core::ledger::{
    BootEnd, BootStart, Capture, Claim, Game, Ledger, Measurement, ModVersion, Outcome, Pose,
    Provenance, ReTarget, Tag, schema_description,
};
use quen_core::mods::{Kind, ModEntry};
use quen_core::stats::{self, Stats, Summary};
use serde_json::{Value, json};

/// The description's sections for tables and views: (name, "table" or "view", the section's text).
fn sections() -> Vec<(String, String, String)> {
    let mut out: Vec<(String, String, String)> = Vec::new();
    for line in schema_description().lines() {
        if line.starts_with('#') {
            let heading = line.strip_prefix("### ").and_then(|h| h.strip_suffix(')'));
            if let Some((name, kind)) = heading.and_then(|h| h.split_once(" (")) {
                out.push((name.into(), kind.into(), String::new()));
                continue;
            }
            out.push((String::new(), String::new(), String::new()));
        } else if let Some(last) = out.last_mut() {
            last.2 += line;
            last.2 += "\n";
        }
    }
    out.retain(|(name, _, _)| !name.is_empty());
    out
}

/// The description's example queries, in order.
fn examples() -> Vec<String> {
    let mut out = Vec::new();
    let mut open: Option<String> = None;
    for line in schema_description().lines() {
        match (&mut open, line) {
            (None, "```sql") => open = Some(String::new()),
            (Some(sql), "```") => {
                out.push(std::mem::take(sql));
                open = None;
            }
            (Some(sql), line) => {
                sql.push_str(line);
                sql.push('\n');
            }
            (None, _) => {}
        }
    }
    out
}

#[test]
fn the_description_names_every_table_view_and_column() {
    let l = Ledger::open_in_memory().unwrap();
    let schema: BTreeSet<(String, String)> = rows(
        &l,
        "SELECT name, type FROM sqlite_schema WHERE type IN ('table', 'view')",
        &[],
    )
    .into_iter()
    .map(|r| (r[0].as_str().unwrap().into(), r[1].as_str().unwrap().into()))
    .collect();
    let described: BTreeSet<(String, String)> = sections()
        .into_iter()
        .map(|(name, kind, _)| (name, kind))
        .collect();
    assert_eq!(
        described, schema,
        "one '### name (table)' or '### name (view)' section each"
    );
    for (name, _, text) in sections() {
        for column in rows(&l, "SELECT name FROM pragma_table_info(?1)", &[json!(name)]) {
            let column = column[0].as_str().unwrap();
            assert!(
                text.contains(&format!("`{column}`")),
                "the section on {name} doesn't name its column `{column}`"
            );
        }
    }
}

fn entry(kind: Kind, name: &str, build: u32, config: Option<u32>) -> ModEntry {
    ModEntry {
        kind,
        name: name.into(),
        home: format!("%GAME_DIR%/{name}").into(),
        parked: false,
        content_hash: sha(build),
        file_count: 1,
        bytes: 1,
        config_hash: config.map(sha),
        config_values: Default::default(),
    }
}

/// A boot on the morning of day `day` of October 2026, finished ok unless `outcome` says otherwise.
#[derive(Default)]
struct Launch {
    day: u32,
    game: i64,
    set: i64,
    devkit: bool,
    busy_with: &'static [&'static str],
    some_cvar: Option<&'static str>,
    patches: Vec<Value>,
    outcome: Option<Outcome>,
}

fn launch(l: &mut Ledger, b: Launch) -> i64 {
    let id = l
        .start_boot(&BootStart {
            game_id: b.game,
            t_start: format!("2026-10-{:02}T10:00:00Z", b.day),
            settings_diff: b.some_cvar.map(|v| {
                json!({"Rendering": {"SomeCvar": v}})
                    .as_object()
                    .cloned()
                    .unwrap()
            }),
            devkit: Some(b.devkit),
            busy_with: Some(b.busy_with.iter().map(|s| s.to_string()).collect()),
            ..Default::default()
        })
        .unwrap();
    l.set_boot_mod_set(id, b.set).unwrap();
    l.finish_boot(
        id,
        &BootEnd {
            t_end: format!("2026-10-{:02}T11:00:00Z", b.day),
            outcome: b.outcome.unwrap_or(Outcome::Ok),
            failure: None,
            patches: b.patches,
        },
    )
    .unwrap();
    id
}

/// One measurement row.
fn row(
    l: &mut Ledger,
    boot: Option<i64>,
    (metric, value): (&str, f64),
    (run, label): (Option<&str>, Option<&str>),
    (family, tag): (&str, Tag),
) -> i64 {
    l.add_measurement(&Measurement {
        boot_id: boot,
        capture_id: None,
        metric: metric.into(),
        value: Some(value),
        unit: Some("ms".into()),
        state_label: label.map(String::from),
        method_family: family.into(),
        tag,
        run_id: run.map(String::from),
        note: None,
        extraction_id: None,
        world: None,
    })
    .unwrap()
}

/// Frame-time windows, `label` value pairs in order, as measure_ab records them.
fn windows(l: &mut Ledger, boot: i64, run: &str, windows: &[(&str, f64)]) -> Vec<i64> {
    windows
        .iter()
        .map(|&(label, ms)| {
            row(
                l,
                Some(boot),
                ("frame_ms_mean", ms),
                (Some(run), Some(label)),
                ("presentmon", Tag::M),
            )
        })
        .collect()
}

fn claim(l: &mut Ledger, text: &str, support: &[(i64, bool)]) -> i64 {
    let id = l
        .add_claim(&Claim {
            text: text.into(),
            scope: Some("5.01".into()),
        })
        .unwrap();
    for &(m, supports) in support {
        l.support(id, m, supports).unwrap();
    }
    id
}

/// An example query's cases: (its params, the rows it must return).
type Cases = Vec<(Vec<Value>, Vec<Vec<Value>>)>;

#[test]
fn the_example_queries_answer_their_questions() {
    let mut l = Ledger::open_in_memory().unwrap();
    let game = |l: &mut Ledger, n| {
        l.add_game(&Game {
            name: "synthetic".into(),
            exe_path: "%GAME_DIR%/bin/x64/game.exe".into(),
            exe_sha256: sha(n),
            build_stamp: None,
        })
        .unwrap()
    };
    let (g1, g2) = (game(&mut l, 1), game(&mut l, 2));
    l.add_mod_version(&ModVersion {
        content_hash: sha(10),
        kind: Kind::Asi,
        name: "W3ElementMap".into(),
        version: "v1".into(),
        provenance: Provenance::GitCommit,
        source: "abc1234".into(),
    })
    .unwrap();
    let set_a = l
        .add_mod_set(&[
            entry(Kind::Asi, "W3ElementMap", 10, Some(20)),
            entry(Kind::ModsFolder, "modX", 30, None),
            entry(Kind::ModsFolder, "modGone", 32, None),
        ])
        .unwrap();
    let set_b = l
        .add_mod_set(&[
            entry(Kind::Asi, "W3ElementMap", 11, Some(20)),
            entry(Kind::ModsFolder, "modX", 30, None),
            entry(Kind::ModsFolder, "modNew", 31, None),
        ])
        .unwrap();
    let base = |day, some_cvar| Launch {
        day,
        game: g1,
        set: set_a,
        some_cvar,
        ..Default::default()
    };
    let b1 = launch(
        &mut l,
        Launch {
            patches: vec![json!({"cvar": "SomeCvar"})],
            ..base(10, Some("0"))
        },
    );
    let b2 = launch(&mut l, base(11, Some("0")));
    let b3 = launch(&mut l, base(12, Some("1")));
    let b4 = launch(
        &mut l,
        Launch {
            devkit: true,
            ..base(13, Some("1"))
        },
    );
    let b5 = launch(
        &mut l,
        Launch {
            game: g2,
            set: set_b,
            ..base(14, None)
        },
    );
    let b6 = launch(
        &mut l,
        Launch {
            set: set_b,
            busy_with: &["cargo.exe"],
            ..base(15, Some("0"))
        },
    );
    let b7 = launch(&mut l, base(16, Some("0")));
    let b8 = launch(
        &mut l,
        Launch {
            outcome: Some(Outcome::Recovered),
            ..base(17, Some("0"))
        },
    );

    // The cvar's A/B run on b1, b4 and b7, and another run on b1; two plain windows on each other boot.
    let ab = [("off", 20.0), ("on", 21.0), ("off", 20.25), ("on", 21.125)];
    windows(&mut l, b1, "sess1-somecvar", &ab);
    let noise = [("a", 20.0), ("b", 20.5), ("a", 20.75), ("b", 20.25)];
    windows(&mut l, b1, "sess1-noise", &noise);
    let ab = [("off", 40.0), ("on", 42.0), ("off", 40.5), ("on", 42.25)];
    windows(&mut l, b4, "sess4-somecvar", &ab);
    let ab = [("off", 20.0), ("on", 20.5), ("off", 20.75), ("on", 20.25)];
    windows(&mut l, b7, "sess7-somecvar", &ab);
    let base_windows = |l: &mut Ledger, boot, ms: f64, second: f64| {
        windows(
            l,
            boot,
            &format!("sess{boot}-base"),
            &[("base", ms), ("base", second)],
        )
    };
    base_windows(&mut l, b2, 20.0, 20.125);
    let b3_frame = base_windows(&mut l, b3, 21.0, 21.25)[0];
    base_windows(&mut l, b4, 40.0, 40.125);
    let b5_frame = base_windows(&mut l, b5, 22.0, 22.125)[0];
    base_windows(&mut l, b6, 19.0, 19.125);
    base_windows(&mut l, b7, 20.0, 20.25);
    let b2_frame = rows(
        &l,
        "SELECT min(id) FROM measurements WHERE boot_id = ?1",
        &[json!(b2)],
    )[0][0]
        .as_i64()
        .unwrap();
    let tracy = row(
        &mut l,
        Some(b3),
        ("gpu_pass_ms", 3.5),
        (None, None),
        ("tracy", Tag::M),
    );
    let files = row(
        &mut l,
        Some(b3),
        ("cpu_ms", 7.0),
        (None, None),
        ("files", Tag::I),
    );
    let nsight = row(
        &mut l,
        Some(b3),
        ("gpu_frame_ms", 12.0),
        (None, None),
        ("nsight", Tag::M),
    );
    let disasm = row(
        &mut l,
        None,
        ("pool_size", 4096.0),
        (None, None),
        ("static_re", Tag::StaticRe),
    );
    let devkit = row(
        &mut l,
        Some(b4),
        ("pool_size", 4096.0),
        (None, None),
        ("renodx_devkit", Tag::O),
    );
    let tracy_b2 = row(
        &mut l,
        Some(b2),
        ("gpu_pass_ms", 3.0),
        (None, None),
        ("tracy", Tag::M),
    );

    let c1 = claim(
        &mut l,
        "SomeCvar=1 costs about 1 ms",
        &[(b3_frame, true), (tracy, true), (files, true)],
    );
    let c2 = claim(
        &mut l,
        "the pool holds 4096 entries",
        &[(disasm, true), (devkit, true)],
    );
    l.add_re_ref(ReTarget::Claim(c2), "re/notes/pools.md")
        .unwrap();
    claim(
        &mut l,
        "W3ElementMap's second build is faster",
        &[(b5_frame, true)],
    );
    let c4 = claim(&mut l, "withdrawn", &[(b2_frame, true), (tracy, true)]);
    l.withdraw(c4, "measured the wrong scene").unwrap();
    claim(
        &mut l,
        "contradicted later",
        &[(b2_frame, true), (tracy_b2, true), (nsight, false)],
    );

    let pose = |l: &mut Ledger, name: &str| {
        l.add_pose(&Pose {
            name: name.into(),
            fov: 60.0,
            ..Default::default()
        })
        .unwrap()
    };
    let (p1, p2) = (pose(&mut l, "P1"), pose(&mut l, "P2"));
    let mut shot = |boot, pose, path: &str, t: &str| {
        l.add_capture(&Capture {
            boot_id: boot,
            path: path.into(),
            sha256: sha(40),
            bytes: 1,
            kind: "screenshot".into(),
            encoding: Some(quen_core::hdr::Encoding::Srgb8),
            pose_id: Some(pose),
            t: t.into(),
            ..Default::default()
        })
        .unwrap()
    };
    let cap1 = shot(Some(b2), p1, "2/P1.png", "2026-10-11T10:30:00Z");
    let cap2 = shot(Some(b5), p1, "5/P1.png", "2026-10-14T10:30:00Z");
    let cap3 = shot(None, p1, "old/P1.png", "2026-10-01T10:30:00Z");
    shot(Some(b2), p2, "2/P2.png", "2026-10-11T10:31:00Z");
    let cap5 = shot(Some(b3), p1, "3/P1.png", "2026-10-12T10:30:00Z");

    // Game knowledge on g1's build: the synthetic KB and an REA bundle (tests/ledger_knowledge.rs has the details).
    let dir = ledger_common::TempDb::new();
    l.scan_kb(&ledger_common::fake_kb(dir.dir(), &sha(1)))
        .unwrap();
    let rea = ledger_common::fake_bundle(&dir.dir().join("rea.json"), &sha(1), 1, 10);
    let bundle = l
        .import_rea_bundle(&dir.dir().join("captures"), &rea, "6.5.0")
        .unwrap()
        .path;
    let extracted = |op: &str, ev: u32| {
        vec![
            json!(op),
            json!(r#"{"document":"game.exe","procedure":"0x140001000"}"#),
            json!(ledger_common::ev(ev)),
            json!("observed"),
            json!("6.5.0"),
            json!(ledger_common::profile(1)),
            json!(
                json!({"path": "%KB%/seeds/synthetic.tsv", "sha256": sha(0x5eed + 1)}).to_string()
            ),
            json!(bundle),
            Value::Null,
            json!(1),
        ]
    };
    let null = Value::Null;
    let s = |n| json!(sha(n));
    let day = |d: u32| json!(format!("2026-10-{d:02}T10:00:00Z"));
    let text =
        |id: i64| rows(&l, "SELECT text FROM claims WHERE id = ?1", &[json!(id)])[0][0].clone();
    let ab_row = |boot: i64,
                  run: &str,
                  first: (&str, f64, f64),
                  second: (&str, f64, f64),
                  diff: f64,
                  devkit: i64| {
        let verdict = if diff.abs() > first.2.max(second.2) {
            "outside drift"
        } else {
            "inside drift"
        };
        vec![
            json!(boot),
            json!(run),
            json!(first.0),
            json!(first.1),
            json!(first.2),
            json!(second.0),
            json!(second.1),
            json!(second.2),
            json!(diff),
            json!(verdict),
            json!(devkit),
            Value::Null,
            json!(0),
        ]
    };
    let expected: Vec<Cases> = vec![
        // A cvar's A/B runs, found by their run ids in any case; another run, inside its drift.
        vec![
            (
                vec![json!("SOMECVAR")],
                vec![
                    ab_row(
                        b1,
                        "sess1-somecvar",
                        ("off", 20.125, 0.25),
                        ("on", 21.0625, 0.125),
                        0.9375,
                        0,
                    ),
                    ab_row(
                        b4,
                        "sess4-somecvar",
                        ("off", 40.25, 0.5),
                        ("on", 42.125, 0.25),
                        1.875,
                        1,
                    ),
                    ab_row(
                        b7,
                        "sess7-somecvar",
                        ("off", 20.375, 0.75),
                        ("on", 20.375, 0.25),
                        0.0,
                        0,
                    ),
                ],
            ),
            (
                vec![json!("noise")],
                vec![ab_row(
                    b1,
                    "sess1-noise",
                    ("a", 20.375, 0.75),
                    ("b", 20.375, 0.25),
                    0.0,
                    0,
                )],
            ),
        ],
        // The same runs beside how each boot compares with b1's.
        vec![(
            vec![json!("somecvar")],
            vec![
                vec![
                    json!(b1),
                    json!("sess1-somecvar"),
                    json!(0.9375),
                    json!("outside drift"),
                    json!(1),
                    null.clone(),
                    null.clone(),
                    null.clone(),
                    null.clone(),
                    null.clone(),
                ],
                vec![
                    json!(b4),
                    json!("sess4-somecvar"),
                    json!(1.875),
                    json!("outside drift"),
                    json!(0),
                    json!("devkit"),
                    json!(""),
                    json!(""),
                    json!(""),
                    json!("settings,patches"),
                ],
                vec![
                    json!(b7),
                    json!("sess7-somecvar"),
                    json!(0.0),
                    json!("inside drift"),
                    json!(1),
                    json!(""),
                    json!(""),
                    json!(""),
                    json!(""),
                    json!("patches"),
                ],
            ],
        )],
        // Proven claims and the tools behind them.
        vec![(
            vec![],
            vec![
                vec![
                    json!(c1),
                    text(c1),
                    json!("5.01"),
                    json!("presentmon,tracy"),
                    json!("files"),
                    json!(""),
                    json!(""),
                ],
                vec![
                    json!(c2),
                    text(c2),
                    json!("5.01"),
                    json!("renodx_devkit,static_re"),
                    json!(""),
                    json!(""),
                    json!("re/notes/pools.md"),
                ],
            ],
        )],
        // One claim's evidence.
        vec![(
            vec![json!(c1)],
            [
                (
                    b3_frame,
                    "presentmon",
                    "M",
                    1,
                    "frame_ms_mean",
                    21.0,
                    json!("base"),
                ),
                (tracy, "tracy", "M", 1, "gpu_pass_ms", 3.5, null.clone()),
                (files, "files", "I", 0, "cpu_ms", 7.0, null.clone()),
            ]
            .into_iter()
            .map(|(id, family, tag, strong, metric, value, label)| {
                vec![
                    json!(1),
                    json!(id),
                    json!(family),
                    json!(tag),
                    json!(strong),
                    json!(metric),
                    json!(value),
                    json!("ms"),
                    label,
                    json!(b3),
                    day(12),
                    json!(0),
                    null.clone(),
                    json!(0),
                ]
            })
            .collect(),
        )],
        // Captures of P1 across W3ElementMap's builds: none known for the file without a boot.
        vec![(
            vec![json!("P1"), json!("asi"), json!("W3ElementMap")],
            [
                (cap3, "old/P1.png", "2026-10-01T10:30:00Z", None, None, None),
                (
                    cap1,
                    "2/P1.png",
                    "2026-10-11T10:30:00Z",
                    Some(b2),
                    Some(10),
                    Some("v1"),
                ),
                (
                    cap5,
                    "3/P1.png",
                    "2026-10-12T10:30:00Z",
                    Some(b3),
                    Some(10),
                    Some("v1"),
                ),
                (
                    cap2,
                    "5/P1.png",
                    "2026-10-14T10:30:00Z",
                    Some(b5),
                    Some(11),
                    Some("unknown build"),
                ),
            ]
            .into_iter()
            .map(|(id, path, t, boot, build, version)| {
                let known = boot.is_some();
                vec![
                    json!(id),
                    json!(path),
                    json!(t),
                    json!(boot),
                    build.map_or(null.clone(), s),
                    json!(version),
                    if known { s(20) } else { null.clone() },
                    if known { json!(0) } else { null.clone() },
                    json!("srgb8"),
                    if known { json!(0) } else { null.clone() },
                    match boot {
                        Some(b) if b == b5 => s(2),
                        Some(_) => s(1),
                        None => null.clone(),
                    },
                ]
            })
            .collect(),
        )],
        // Whether two boots compare: comparable, differs, unknown, busy, failed, varies.
        [
            (b2, b3, 1, "", "", "", "settings"),
            (b2, b4, 0, "devkit", "", "", "settings"),
            (b2, b6, 0, "", "b", "", "mods"),
            (b5, b2, 0, "game_build", "", "", "mods,settings"),
            (b2, b8, 0, "", "", "b", ""),
        ]
        .into_iter()
        .map(|(a, b, comparable, differs, busy, failed, varies)| {
            (
                vec![json!(a), json!(b)],
                vec![vec![
                    json!(comparable),
                    json!(differs),
                    json!(""),
                    json!(busy),
                    json!(failed),
                    json!(varies),
                ]],
            )
        })
        .collect(),
        // Baselines for b2.
        vec![(
            vec![json!(b2)],
            vec![
                vec![json!(b7), day(16), json!("")],
                vec![json!(b3), day(12), json!("settings")],
                vec![json!(b1), day(10), json!("patches")],
            ],
        )],
        // The mods that differ between b2 and b5.
        vec![(
            vec![json!(b2), json!(b5)],
            vec![
                vec![
                    json!("asi"),
                    json!("W3ElementMap"),
                    s(10),
                    s(11),
                    s(20),
                    s(20),
                    json!(0),
                    json!(0),
                ],
                vec![
                    json!("mods"),
                    json!("modGone"),
                    s(32),
                    null.clone(),
                    null.clone(),
                    null.clone(),
                    json!(0),
                    null.clone(),
                ],
                vec![
                    json!("mods"),
                    json!("modNew"),
                    null.clone(),
                    s(31),
                    null.clone(),
                    null.clone(),
                    null.clone(),
                    json!(0),
                ],
            ],
        )],
        // What was measured in b3, by which tools.
        vec![(
            vec![json!(b3)],
            [
                ("files", "cpu_ms", null.clone(), 1, 7.0),
                ("nsight", "gpu_frame_ms", null.clone(), 1, 12.0),
                ("presentmon", "frame_ms_mean", json!("base"), 2, 21.125),
                ("tracy", "gpu_pass_ms", null.clone(), 1, 3.5),
            ]
            .into_iter()
            .map(|(family, metric, label, n, mean)| {
                vec![
                    json!(family),
                    json!(metric),
                    label,
                    json!(n),
                    json!(mean),
                    json!("ms"),
                ]
            })
            .collect(),
        )],
        // What REA answered at 0x1000 of g1's build: by RVA, any operation; by name, one.
        vec![
            (
                vec![json!(sha(1)), json!("0x1000"), json!("")],
                vec![
                    extracted("procedure_callers", 11),
                    extracted("procedure_pseudo_code", 10),
                ],
            ),
            (
                vec![
                    json!(sha(1)),
                    json!("Alpha_Draw"),
                    json!("procedure_pseudo_code"),
                ],
                vec![extracted("procedure_pseudo_code", 10)],
            ),
            (vec![json!(sha(2)), json!("0x1000"), json!("")], vec![]),
        ],
        // From a function to the hooks that touch it: through the caller REA found, then the KB's hook.
        vec![
            (
                vec![json!("synthetic"), json!("function"), json!("Beta_Setup")],
                vec![vec![
                    json!(2),
                    json!(
                        "function:Beta_Setup calls function:Alpha_Draw hooks hook:ModA:hook:Alpha_Draw"
                    ),
                ]],
            ),
            (
                vec![json!("synthetic"), json!("function"), json!("Alpha_Draw")],
                vec![vec![
                    json!(1),
                    json!("function:Alpha_Draw hooks hook:ModA:hook:Alpha_Draw"),
                ]],
            ),
        ],
    ];

    let examples = examples();
    assert_eq!(
        examples.len(),
        expected.len(),
        "every example has its expected answers here"
    );
    for (i, (sql, cases)) in examples.iter().zip(&expected).enumerate() {
        for (params, want) in cases {
            let got = l.query(sql, params, std::time::Duration::from_secs(10));
            let got = got.unwrap_or_else(|e| panic!("example {i} with {params:?}: {e}\n{sql}"));
            assert_eq!(&got.rows, want, "example {i} with {params:?}:\n{sql}");
        }
    }
}

#[test]
fn ab_runs_gives_measure_abs_verdict() {
    let mut l = Ledger::open_in_memory().unwrap();
    let g = ledger_common::game(&mut l);
    let b = ledger_common::boot(&mut l, g, "2026-10-10T10:00:00Z");
    let runs: &[&[(&str, f64)]] = &[
        &[("A", 20.0), ("B", 21.0), ("A", 20.3), ("B", 21.2)],
        &[("A", 20.0), ("B", 20.1), ("A", 20.4), ("B", 20.2)],
        &[("A", 20.0), ("B", 21.0), ("B", 21.5), ("A", 20.9)],
        &[
            ("A", 10.0),
            ("A", 10.1),
            ("B", 10.5),
            ("B", 10.6),
            ("B", 10.55),
        ],
        // |diff| equal to a drift is inside it.
        &[("A", 0.0), ("B", 1.5), ("A", 1.0), ("B", 1.5)],
        &[("B", 5.0), ("A", 4.0), ("B", 5.25), ("A", 4.5)],
    ];
    for (i, run) in runs.iter().enumerate() {
        windows(&mut l, b, &format!("run-{i}"), run);
        let labelled: Vec<(&str, Stats)> = run
            .iter()
            .map(|&(label, mean)| {
                let summary = Summary {
                    count: 1,
                    mean,
                    median: mean,
                    p95: mean,
                };
                let stats = Stats {
                    frame_ms: summary,
                    gpu_busy_ms: None,
                    gpu_time_ms: None,
                    cpu_busy_ms: None,
                    present_mode: Some("Hardware: Independent Flip".into()),
                    present_mode_mixed: false,
                };
                (label, stats)
            })
            .collect();
        let want = stats::ab_verdict(&labelled).unwrap();
        let got = &rows(
            &l,
            "SELECT verdict, diff, first_label, first_drift, second_label, second_drift FROM ab_runs WHERE run_id = ?1",
            &[json!(format!("run-{i}"))],
        )[0];
        assert_eq!(got[0], json!(want.verdict), "run {i}");
        assert!(
            (got[1].as_f64().unwrap() - want.diff_ms).abs() < 1e-9,
            "run {i}"
        );
        for (label, drift) in [(&got[2], &got[3]), (&got[4], &got[5])] {
            let want = want.drift_ms[label.as_str().unwrap()];
            assert!((drift.as_f64().unwrap() - want).abs() < 1e-9, "run {i}");
        }
    }

    // One state, or one window in a state: no verdict.
    windows(&mut l, b, "single", &[("A", 20.0), ("A", 20.1)]);
    windows(&mut l, b, "short", &[("A", 20.0), ("B", 21.0), ("A", 20.1)]);
    let none = rows(
        &l,
        "SELECT run_id, states, second_label, verdict FROM ab_runs WHERE run_id IN ('single', 'short') ORDER BY run_id",
        &[],
    );
    assert_eq!(
        none,
        [
            vec![json!("short"), json!(2), json!("B"), Value::Null],
            vec![json!("single"), json!(1), Value::Null, Value::Null],
        ]
    );
}
