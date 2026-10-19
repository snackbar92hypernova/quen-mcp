//! Camera sweeps in the ledger (schema 0006): stored canonical once by name, their keyframes'
//! poses, and their runs frame by frame.

mod ledger_common;

use ledger_common::{TempDb, boot, capture, game, rows, sha};
use quen_core::journal::sha256_hex;
use quen_core::ledger::{
    Error, Ledger, Pose, Row, SCHEMA_VERSION, Save, Sweep, SweepFrame, SweepRun,
};
use quen_core::sweep::{self, Mode};
use rusqlite::Connection;
use serde_json::{Value, json};

#[track_caller]
fn refused<T: std::fmt::Debug>(r: Result<T, Error>, needle: &str) {
    match r {
        Err(Error::Refused(why)) => assert!(why.contains(needle), "{why:?} lacks {needle:?}"),
        other => panic!("expected a refusal about {needle:?}, got {other:?}"),
    }
}

/// A 7 s sweep through a synthetic room, written as an agent might: yaw 350 and roll -180 are canonical -10 and 180.
fn room() -> sweep::Sweep {
    serde_json::from_value(json!({
        "keyframes": [
            {"t": 0, "pos": [100, 200, 10], "rot": [-5, 350, -180], "fov": 60},
            {"t": 2, "pos": [103, 201, 10.5], "rot": [0, 10, -180], "fov": 60, "ease": "ease_in_out"},
            {"t": 7, "pos": [98, 205, 11], "rot": [0, 60, -180], "fov": 50}
        ],
        "world": {"save": "synthetic save", "time": {"day": 2, "hour": 22}, "paused": true}
    }))
    .unwrap()
}

fn pose(l: &mut Ledger, name: &str) -> i64 {
    l.add_pose(&Pose {
        name: name.into(),
        camera: [100.0, 200.0, 10.0],
        rotation: [-5.0, -10.0, 180.0],
        fov: 60.0,
        ..Default::default()
    })
    .unwrap()
}

fn count(l: &Ledger, table: &str) -> Value {
    rows(l, &format!("SELECT count(*) FROM {table}"), &[])[0][0].clone()
}

#[test]
fn a_sweep_is_stored_canonical_once_by_name() {
    let mut l = Ledger::open_in_memory().unwrap();
    let g = game(&mut l);
    let save = l
        .add_save(&Save {
            game_id: g,
            file: "synthetic save.sav".into(),
            sha256: sha(5),
            ..Default::default()
        })
        .unwrap();
    let (p0, p2) = (pose(&mut l, "room 0"), pose(&mut l, "room 2"));
    let row = Sweep {
        name: "room pan".into(),
        sweep: room(),
        save_id: Some(save),
        poses: vec![Some(p0), None, Some(p2)],
        note: Some("synthetic".into()),
    };
    let id = l.add_sweep(&row).unwrap();

    let canonical = room().canonical().unwrap().to_json();
    assert!(
        canonical.contains(r#""rot":[-5.0,-10.0,180.0]"#),
        "{canonical}"
    );
    assert_eq!(
        rows(
            &l,
            "SELECT name, sha256, sweep, save_id, note FROM sweeps",
            &[]
        ),
        vec![vec![
            json!("room pan"),
            json!(sha256_hex(canonical.as_bytes())),
            json!(canonical),
            json!(save),
            json!("synthetic")
        ]]
    );
    assert_eq!(
        rows(
            &l,
            "SELECT sweep_id, keyframe, pose_id FROM sweep_keyframes ORDER BY keyframe",
            &[]
        ),
        vec![
            vec![json!(id), json!(0), json!(p0)],
            vec![json!(id), json!(2), json!(p2)]
        ]
    );

    // The same sweep under its name again, even spelled differently: the same row, nothing added.
    let again = Sweep {
        sweep: room().canonical().unwrap(),
        poses: vec![],
        ..row.clone()
    };
    assert_eq!(l.add_sweep(&again).unwrap(), id);
    assert_eq!(
        (count(&l, "sweeps"), count(&l, "sweep_keyframes")),
        (json!(1), json!(2))
    );

    // Another sweep under that name, a broken sweep, or a pose list that doesn't match: refused, nothing written.
    let mut other = row.clone();
    other.sweep.keyframes[2].fov = 55.0;
    refused(l.add_sweep(&other), "name this one anew");
    let mut broken = row.clone();
    broken.name = "broken".into();
    broken.sweep.keyframes.truncate(1);
    refused(l.add_sweep(&broken), "1 keyframes");
    let mut short = row.clone();
    short.name = "short".into();
    short.poses.pop();
    refused(l.add_sweep(&short), "2 poses for 3 keyframes");
    assert_eq!(
        (count(&l, "sweeps"), count(&l, "sweep_keyframes")),
        (json!(1), json!(2))
    );

    // The same path under a second name shares the hash.
    let twin = l
        .add_sweep(&Sweep {
            name: "room pan, again".into(),
            poses: vec![],
            ..row
        })
        .unwrap();
    assert_ne!(twin, id);
    let shas = rows(&l, "SELECT count(DISTINCT sha256) FROM sweeps", &[]);
    assert_eq!(shas, vec![vec![json!(1)]]);
}

#[test]
fn runs_hold_their_frames_and_nothing_past_the_end() {
    let mut l = Ledger::open_in_memory().unwrap();
    let g = game(&mut l);
    let b = boot(&mut l, g, "2026-10-17T20:00:00Z");
    let s = l
        .add_sweep(&Sweep {
            name: "room pan".into(),
            sweep: room(),
            save_id: None,
            poses: vec![],
            note: None,
        })
        .unwrap();
    let run = |l: &mut Ledger, mode: Mode, fps: Option<u32>| {
        l.add_sweep_run(&SweepRun {
            sweep_id: s,
            boot_id: Some(b),
            mode,
            fps,
            t: "2026-10-17T20:05:00Z".into(),
        })
    };
    let live = run(&mut l, Mode::Realtime, None).unwrap();
    let step = run(&mut l, Mode::Step, Some(30)).unwrap();
    refused(run(&mut l, Mode::Step, None), "step with fps None");
    refused(run(&mut l, Mode::Realtime, Some(30)), "realtime with fps");
    refused(
        run(&mut l, Mode::Step, Some(sweep::MAX_FPS + 1)),
        "1 to 240",
    );
    assert!(
        l.add_sweep_run(&SweepRun {
            sweep_id: s + 1,
            boot_id: None,
            mode: Mode::Realtime,
            fps: None,
            t: "2026-10-17T20:05:00Z".into()
        })
        .is_err(),
        "no such sweep"
    );
    assert_eq!(count(&l, "sweep_runs"), json!(2));

    // 7 s at 30 fps: frames 0 to 210.
    let c: Vec<i64> = (0..4)
        .map(|i| {
            capture(
                &mut l,
                Some(b),
                &format!("1/frame{i}.png"),
                "2026-10-17T20:06:00Z",
            )
        })
        .collect();
    let frame = |l: &mut Ledger, run_id: i64, frame: u32, capture_id: i64| {
        l.add_sweep_frame(&SweepFrame {
            run_id,
            frame,
            capture_id,
        })
    };
    frame(&mut l, step, 0, c[0]).unwrap();
    frame(&mut l, step, 210, c[1]).unwrap();
    refused(frame(&mut l, step, 211, c[2]), "its frames are 0 to 210");
    refused(frame(&mut l, live, 0, c[2]), "real time");
    refused(frame(&mut l, step + 5, 0, c[2]), "no sweep run");
    assert!(
        frame(&mut l, step, 1, c[0]).is_err(),
        "a capture is one frame"
    );
    assert!(
        frame(&mut l, step, 0, c[3]).is_err(),
        "a frame has one capture"
    );
    assert_eq!(
        rows(
            &l,
            "SELECT run_id, frame, capture_id FROM sweep_frames ORDER BY frame",
            &[]
        ),
        vec![
            vec![json!(step), json!(0), json!(c[0])],
            vec![json!(step), json!(210), json!(c[1])]
        ]
    );
}

#[test]
fn sweep_rows_ingest_from_json_and_runs_need_the_lock() {
    let mut l = Ledger::open_in_memory().unwrap();
    let parse = |v: Value| serde_json::from_value::<Row>(v).unwrap();
    let sweep = parse(json!({"table": "sweeps", "name": "room pan", "sweep": room()}));
    let run = parse(
        json!({"table": "sweep_runs", "sweep_id": 1, "boot_id": null, "mode": "step", "fps": 24,
        "t": "2026-10-17T20:05:00Z"}),
    );
    assert!(!sweep.needs_lock() && run.needs_lock());
    let ids = l.ingest(&[sweep, run]).unwrap();
    assert_eq!(ids, [Some(1), Some(1)]);
    let c = capture(&mut l, None, "frame0.png", "2026-10-17T20:06:00Z");
    let frame = parse(json!({"table": "sweep_frames", "run_id": 1, "frame": 168, "capture_id": c}));
    assert!(frame.needs_lock());
    assert_eq!(l.ingest(&[frame]).unwrap(), [None]);
    let late = parse(json!({"table": "sweep_frames", "run_id": 1, "frame": 169, "capture_id": c}));
    refused(l.ingest(&[late]), "row 0: frame 169 of sweep run 1");

    for bad in [
        json!({"table": "sweeps", "name": "x", "sweep": room(), "pose": []}),
        json!({"table": "sweep_runs", "sweep_id": 1, "boot_id": null, "mode": "slow", "fps": null, "t": "2026-10-17T20:05:00Z"}),
        json!({"table": "sweep_frames", "run_id": 1, "frame": -1, "capture_id": c}),
    ] {
        assert!(serde_json::from_value::<Row>(bad.clone()).is_err(), "{bad}");
    }
}

#[test]
fn the_schema_holds_without_the_rust_checks() {
    let db = TempDb::new();
    drop(Ledger::open(&db.0).unwrap());
    let c = Connection::open(&db.0).unwrap();
    assert_eq!(
        c.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        SCHEMA_VERSION
    );
    let two = room().canonical().unwrap().to_json();
    c.execute(
        "INSERT INTO sweeps (id, name, sha256, sweep) VALUES (1, 'room', ?1, ?2)",
        [sha(1), two],
    )
    .unwrap();
    for sql in [
        // One keyframe, not JSON, a blank name.
        format!(
            "INSERT INTO sweeps (name, sha256, sweep) VALUES ('one', '{}', '{{\"keyframes\":[{{}}]}}')",
            sha(2)
        ),
        format!("INSERT INTO sweeps (name, sha256, sweep) VALUES ('bad', '{}', 'nope')", sha(2)),
        format!("INSERT INTO sweeps (name, sha256, sweep) VALUES (' ', '{}', '{{\"keyframes\":[1,2]}}')", sha(2)),
        // Step without fps, real time with one, an unknown mode, an fps out of range.
        "INSERT INTO sweep_runs (sweep_id, mode, t) VALUES (1, 'step', '2026-10-17T20:05:00Z')".into(),
        "INSERT INTO sweep_runs (sweep_id, mode, fps, t) VALUES (1, 'realtime', 30, '2026-10-17T20:05:00Z')".into(),
        "INSERT INTO sweep_runs (sweep_id, mode, t) VALUES (1, 'slow', '2026-10-17T20:05:00Z')".into(),
        "INSERT INTO sweep_runs (sweep_id, mode, fps, t) VALUES (1, 'step', 241, '2026-10-17T20:05:00Z')".into(),
        "INSERT INTO sweep_runs (sweep_id, mode, t) VALUES (1, 'realtime', '2026-10-17 20:05')".into(),
    ] {
        assert!(c.execute(&sql, []).is_err(), "{sql}");
    }
    c.execute(
        "INSERT INTO sweep_runs (sweep_id, mode, fps, t) VALUES (1, 'step', 240, '2026-10-17T20:05:00Z')",
        [],
    )
    .unwrap();
}
