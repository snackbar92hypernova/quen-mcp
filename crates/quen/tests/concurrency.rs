//! Several agents at once (docs/agents.md "Several agents at once"): real `quen mcp` connections to one daemon, each
//! driven from a thread of its own, plus a second daemon over the same data root, the CLI, and plain SQLite
//! connections holding the ledger open as a viewer or script would. Synchronised by barriers, channels and the fake
//! scripts' hold points, never by sleeps; every wait is bounded by a generous [`LIMIT`]. A test binary of its own, on
//! session.rs's fixture (`common`): cargo runs binaries one after another, so these daemons never add to the load
//! session.rs's timing tests run under.
#![cfg(windows)]

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::{Barrier, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use quen_core::ledger::{Ledger, SCHEMA_VERSION};
use rusqlite::Connection;
use serde_json::{Value, json};

mod common;

use common::*;

/// The longest a call that should answer at once may take: generous, so a slow machine never fails it.
const LIMIT: Duration = Duration::from_secs(30);

/// A tool call's envelope, checked to come within [`LIMIT`].
fn timed(quen: &mut Quen, name: &str, args: Value) -> Value {
    let t = Instant::now();
    let out = quen.tool(name, args);
    assert!(t.elapsed() < LIMIT, "{name} took {:?}: {out}", t.elapsed());
    out
}

/// Whether `out` was refused with a reason containing `why`.
fn refused(out: &Value, why: &str) -> bool {
    out["refused"] == true && out["reason"].as_str().is_some_and(|r| r.contains(why))
}

/// Lets a boot held at a hold point go on (FAKE_SCRIPT).
fn go(fx: &Fixture) {
    fs::write(fx.path("harness/go"), "").unwrap();
}

/// A measurement row for ledger_ingest, named by its metric.
fn row(metric: &str) -> String {
    json!({"table": "measurements", "boot_id": null, "capture_id": null, "metric": metric, "value": 1.0,
           "unit": "ms", "state_label": "A", "method_family": "tracy", "tag": "M", "run_id": null, "note": null})
    .to_string()
}

/// The ids a ledger_ingest envelope returned.
fn ids(out: &Value) -> Vec<i64> {
    let ids = out["ledger"].as_array().unwrap_or_else(|| panic!("{out}"));
    ids.iter().map(|id| id.as_i64().unwrap()).collect()
}

/// `quen ARGS` for the fixture's config, with `env` beside it and `stdin` on its input: its exit code and the JSON it
/// printed.
fn cli(fx: &Fixture, args: &[&str], env: &[(&str, PathBuf)], stdin: &str) -> (i32, Value) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_quen"))
        .args(args)
        .env("QUEN_CONFIG", fx.path("config.toml"))
        .envs(env.iter().map(|(k, v)| (*k, v)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let Output { status, stdout, .. } = child.wait_with_output().unwrap();
    let out = serde_json::from_slice(&stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&stdout)));
    (status.code().unwrap(), out)
}

/// The fixture's ledger, opened as a plain SQLite connection from outside quen.
fn outside(fx: &Fixture) -> Connection {
    let conn = Connection::open(fx.path("data/state/quen.db")).unwrap();
    conn.busy_timeout(LIMIT).unwrap();
    conn
}

/// The shipped migrations up to schema 6, applied by hand: a ledger an older quen left.
const OLD: [&str; 6] = [
    include_str!("../../../schema/0001_init.sql"),
    include_str!("../../../schema/0002_mods.sql"),
    include_str!("../../../schema/0003_harness_info.sql"),
    include_str!("../../../schema/0004_claims_strict.sql"),
    include_str!("../../../schema/0005_views.sql"),
    include_str!("../../../schema/0006_sweeps.sql"),
];

/// Writes a schema-6 ledger holding one claim, `kept`, in WAL mode as quen leaves it, and returns a connection to it.
fn old_ledger(fx: &Fixture) -> Connection {
    fs::create_dir_all(fx.path("data/state")).unwrap();
    let conn = outside(fx);
    for (i, sql) in OLD.iter().enumerate() {
        conn.execute_batch(sql).unwrap();
        conn.pragma_update(None, "user_version", i as i64 + 1)
            .unwrap();
    }
    let mode: String = conn
        .pragma_update_and_check(None, "journal_mode", "wal", |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    conn.execute("INSERT INTO claims (text) VALUES ('kept')", [])
        .unwrap();
    conn
}

fn user_version(conn: &Connection) -> i64 {
    conn.pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap()
}

fn integrity(conn: &Connection) -> String {
    conn.pragma_query_value(None, "integrity_check", |r| r.get(0))
        .unwrap()
}

/// While one connection's launch is in flight on the session thread, two others read: status, the ledger, the
/// capture root, the guide and a plan answer at once, each its own (their query echoes their own parameter), and
/// neither sees the launch's answer or its progress.
#[test]
fn reads_answer_during_another_connections_launch_and_are_their_own() {
    let fx = Fixture::new();
    fx.hold("continue");
    let mut a = Quen::start(&fx);
    let launch = a.post(
        "tools/call",
        json!({"name": "session_launch", "arguments": {"user_ok": true}, "_meta": {"progressToken": "a"}}),
    );
    fx.wait_at("continue");
    let (tx, rx) = mpsc::channel();
    thread::scope(|s| {
        for who in ["b", "c"] {
            let (fx, tx) = (&fx, tx.clone());
            s.spawn(move || {
                let mut q = Quen::start(fx);
                for _ in 0..5 {
                    let status = timed(&mut q, "session_status", json!({}));
                    assert_eq!(status["state"]["lock"], "held here", "{status}");
                    assert_eq!(status["state"]["game"]["running"], true, "{status}");
                    let boot = &status["state"]["boot"];
                    assert!(boot.is_i64(), "{status}");
                    let sql = "SELECT ?1 AS who, count(*) FROM boots WHERE id = ?2";
                    let query = json!({"sql": sql, "params": [who, boot.to_string()]});
                    let rows = timed(&mut q, "ledger_query", query);
                    assert_eq!(rows["rows"], json!([[who, 1]]), "{rows}");
                    let usage = timed(&mut q, "capture_usage", json!({}));
                    assert_eq!(usage["ok"], true, "{usage}");
                    let guide = timed(&mut q, "quen_guide", json!({}));
                    assert!(
                        guide["text"]
                            .as_str()
                            .unwrap()
                            .starts_with("# Agent playbook")
                    );
                    let plan = timed(&mut q, "session_plan", json!({}));
                    assert!(refused(&plan, "a session is live"), "{plan}");
                }
                assert!(q.notes.is_empty(), "{who} got {:?}", q.notes);
                tx.send(()).unwrap();
            });
        }
        drop(tx);
        // Both readers finish while the launch is still held, or the hold goes and the test fails.
        let done = (0..2).all(|_| rx.recv_timeout(LIMIT * 2).is_ok());
        go(&fx);
        assert!(done, "a read waited behind the launch");
    });
    let launched = result(&a.response(launch));
    assert_eq!(launched["ok"], true, "{launched}");
    assert!(!a.notes.is_empty(), "the launch sent no progress");
    for note in &a.notes {
        assert_eq!(note["params"]["progressToken"], "a", "{note}");
    }
    let ended = a.tool("session_end", json!({"token": launched["token"]}));
    assert_eq!(ended["ok"], true, "{ended}");
    fx.assert_restored();
}

/// A job belongs to its connection: another one can't see, poll or cancel it, by its job id or by the request id
/// (each connection numbers its own requests from 1).
#[test]
fn only_its_connection_sees_polls_or_cancels_a_job() {
    let fx = Fixture::new();
    fx.hold("continue");
    let mut a = Quen::start(&fx);
    let mut b = Quen::start(&fx);
    let answer = a.call("session_launch", json!({"user_ok": true, "wait": false}));
    let request = a.next_id;
    let job = answer["job"].as_str().unwrap_or_else(|| panic!("{answer}"));
    fx.wait_at("continue");
    for tool in ["job_status", "job_cancel"] {
        let out = timed(&mut b, tool, json!({"job": job}));
        assert!(refused(&out, "another connection"), "{tool}: {out}");
    }
    let cancel = json!({"requestId": request, "reason": "not B's"});
    b.send(&json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": cancel}));
    let status = timed(&mut b, "session_status", json!({}));
    assert_eq!(status["state"]["game"]["running"], true, "{status}");
    go(&fx);
    let launched = a.poll(&answer);
    assert_eq!(
        launched["ok"], true,
        "B's cancel reached A's launch: {launched}"
    );
    let again = timed(&mut a, "job_status", json!({"job": job}));
    assert!(refused(&again, "already returned"), "{again}");
    assert!(b.notes.is_empty(), "{:?}", b.notes);
    let ended = a.tool("session_end", json!({"token": launched["token"]}));
    assert_eq!(ended["ok"], true, "{ended}");
    fx.assert_restored();
}

/// Four connections write at once from a fresh data root: every ingest is all or nothing, every id it returns is the
/// row it wrote, and nothing is lost or written twice.
#[test]
fn concurrent_ingests_are_whole_and_return_their_own_rows() {
    const CONNS: usize = 4;
    const ROUNDS: usize = 15;
    let fx = Fixture::new();
    let start = Barrier::new(CONNS);
    let written: Vec<(i64, String)> = thread::scope(|s| {
        let runs: Vec<_> = (0..CONNS)
            .map(|c| {
                let (fx, start) = (&fx, &start);
                s.spawn(move || {
                    let mut q = Quen::start(fx);
                    start.wait();
                    let mut mine = Vec::new();
                    for i in 0..ROUNDS {
                        let names = [format!("c{c}-{i}-x"), format!("c{c}-{i}-y")];
                        let rows = json!({"rows": [row(&names[0]), row(&names[1])]});
                        let out = timed(&mut q, "ledger_ingest", rows);
                        assert_eq!(out["ok"], true, "{out}");
                        mine.extend(ids(&out).into_iter().zip(names));
                        // A second row naming a boot that doesn't exist: the first row goes too.
                        let mut bad: Value = serde_json::from_str(&row("lost")).unwrap();
                        bad["boot_id"] = json!(999_999);
                        let rows = json!({"rows": [row(&format!("c{c}-{i}-lost")), bad.to_string()]});
                        let out = timed(&mut q, "ledger_ingest", rows);
                        assert!(refused(&out, "row 1"), "{out}");
                        let claim = json!({"action": "open", "text": format!("c{c}-{i}"),
                                           "support": [{"measurement_id": mine[mine.len() - 1].0}]});
                        let out = timed(&mut q, "ledger_claim", claim);
                        assert_eq!(out["claim"]["text"], format!("c{c}-{i}"), "{out}");
                    }
                    mine
                })
            })
            .collect();
        runs.into_iter().flat_map(|r| r.join().unwrap()).collect()
    });
    let mut want: Vec<Vec<Value>> = written
        .into_iter()
        .map(|(id, metric)| vec![json!(id), json!(metric)])
        .collect();
    want.sort_by_key(|r| r[0].as_i64());
    assert_eq!(want.len(), CONNS * ROUNDS * 2);
    assert_eq!(
        fx.ledger("SELECT id, metric FROM measurements ORDER BY id"),
        want
    );
    let claims = "SELECT count(*), count(DISTINCT c.text) FROM claims c JOIN claim_support s ON s.claim_id = c.id \
                  JOIN measurements m ON m.id = s.measurement_id WHERE m.metric = c.text || '-y'";
    let n = (CONNS * ROUNDS) as i64;
    assert_eq!(fx.ledger(claims), [[json!(n), json!(n)]]);
}

/// Several connections rescan one KB and import one REA bundle at once: every call is answered whole, and the
/// ledger holds the bundle once, each record once and the KB's sections once.
#[test]
fn concurrent_imports_of_the_same_knowledge_add_it_once() {
    const CONNS: usize = 4;
    let fx = Fixture::new();
    let exe = "e".repeat(64);
    fs::create_dir_all(fx.path("data/state")).unwrap();
    let mut l = Ledger::open(fx.path("data/state/quen.db")).unwrap();
    l.add_game(&quen_core::ledger::Game {
        name: "synthetic".into(),
        exe_path: "%GAME_DIR%/game.exe".into(),
        exe_sha256: exe.clone(),
        build_stamp: None,
    })
    .unwrap();
    drop(l);
    let (kb, bundle) = knowledge(&fx.0, &exe);
    let start = Barrier::new(CONNS);
    thread::scope(|s| {
        for _ in 0..CONNS {
            let (fx, start, kb, bundle) = (&fx, &start, &kb, &bundle);
            s.spawn(move || {
                let mut q = Quen::start(fx);
                start.wait();
                // The scan first: the bundle's callers become links only between functions the KB names.
                let scan = timed(&mut q, "ledger_import", json!({"kind": "kb", "path": kb}));
                assert_eq!(scan["ok"], true, "{scan}");
                let args = json!({"kind": "rea_bundle", "path": bundle, "rea_version": "6.5.0"});
                let rea = timed(&mut q, "ledger_import", args);
                assert_eq!(rea["ok"], true, "{rea}");
                let about = timed(
                    &mut q,
                    "ledger_about",
                    json!({"kind": "function", "key": "Alpha_Draw"}),
                );
                assert_eq!(about["about"]["extracted"], true, "{about}");
            });
        }
    });
    let counts = "SELECT (SELECT count(*) FROM captures WHERE method = 'rea'), (SELECT count(*) FROM extractions),                   (SELECT count(*) FROM links WHERE rel = 'calls'), (SELECT count(*) FROM kb_sections),                   (SELECT count(*) FROM entities)";
    assert_eq!(
        fx.ledger(counts),
        [[json!(1), json!(2), json!(1), json!(2), json!(3)]]
    );
}

/// Four connections support one claim with the same evidence, and withdraw another, at once: one of each wins
/// whole, the others are refused with a reason that says why, and the claim's status is the winner's.
#[test]
fn claims_on_the_same_evidence_at_once_stay_consistent() {
    const CONNS: usize = 4;
    let fx = Fixture::new();
    let mut seed = Quen::start(&fx);
    let mut pm: Value = serde_json::from_str(&row("pm")).unwrap();
    pm["method_family"] = json!("presentmon");
    let out = seed.tool(
        "ledger_ingest",
        json!({"rows": [pm.to_string(), row("tracy")]}),
    );
    let m = ids(&out);
    let open = |q: &mut Quen, text: &str| {
        let out = q.tool("ledger_claim", json!({"action": "open", "text": text}));
        out["claim"]["id"]
            .as_i64()
            .unwrap_or_else(|| panic!("{out}"))
    };
    let (x, y) = (open(&mut seed, "x"), open(&mut seed, "y"));
    let start = Barrier::new(CONNS);
    let outs: Vec<(Value, Value)> = thread::scope(|s| {
        let runs: Vec<_> = (0..CONNS)
            .map(|c| {
                let (fx, start, m) = (&fx, &start, &m);
                s.spawn(move || {
                    let mut q = Quen::start(fx);
                    start.wait();
                    let support = json!([{"measurement_id": m[0], "supports": c % 2 == 0},
                                         {"measurement_id": m[1]}]);
                    let s = json!({"action": "support", "claim_id": x, "support": support});
                    let w = json!({"action": "withdraw", "claim_id": y, "reason": format!("c{c}")});
                    (
                        timed(&mut q, "ledger_claim", s),
                        timed(&mut q, "ledger_claim", w),
                    )
                })
            })
            .collect();
        runs.into_iter().map(|r| r.join().unwrap()).collect()
    });
    let won: Vec<&Value> = outs
        .iter()
        .map(|o| &o.0)
        .filter(|o| o["ok"] == true)
        .collect();
    assert_eq!(won.len(), 1, "{outs:?}");
    for (lost, _) in outs.iter().filter(|o| o.0["ok"] != true) {
        assert!(refused(lost, "already"), "{lost}");
    }
    let supports = &won[0]["claim"];
    let status = fx.ledger(&format!(
        "SELECT families, proven FROM claims_status WHERE id = {x}"
    ));
    assert_eq!(
        status,
        [[supports["families"].clone(), supports["proven"].clone()]]
    );
    assert_eq!(
        fx.ledger(&format!(
            "SELECT count(*) FROM claim_support WHERE claim_id = {x}"
        )),
        [[json!(2)]]
    );
    let withdrawn: Vec<&Value> = outs
        .iter()
        .map(|o| &o.1)
        .filter(|o| o["ok"] == true)
        .collect();
    assert_eq!(withdrawn.len(), 1, "{outs:?}");
    for (_, lost) in outs.iter().filter(|o| o.1["ok"] != true) {
        assert!(refused(lost, "already withdrawn"), "{lost}");
    }
    let reason = &withdrawn[0]["claim"]["withdrawn_reason"];
    assert_eq!(
        fx.ledger(&format!(
            "SELECT status, withdrawn_reason FROM claims WHERE id = {y}"
        )),
        [[json!("withdrawn"), reason.clone()]]
    );
}

/// While one connection's session is live, only it drives the session and writes on its boot: another connection's
/// session tools are refused at once, its rows stay off the boot, capture_clean is refused for the live boot, and
/// another daemon over the same data root and the CLI can't write rows about a boot.
#[test]
fn only_the_launching_connection_writes_about_its_boot() {
    let fx = Fixture::new();
    let mut a = Quen::start(&fx);
    let mut b = Quen::start(&fx);
    let launched = launch(&mut a);
    let token = &launched["token"];
    let boot = launched["ledger"]["boots"][0].as_i64().unwrap();
    for (tool, args) in [
        ("game_screenshot", json!({"token": token, "tag": "B1"})),
        ("measure_presentmon", json!({"token": token})),
        ("game_console", json!({"token": token, "line": "x"})),
        ("focus", json!({"token": token, "to": "client"})),
        ("game_save", json!({"token": token, "user_ok": true})),
        ("hands_take", json!({"token": token, "user_ok": true})),
        ("world_get", json!({"token": token})),
        ("capture_clean", json!({"boots": [boot], "user_ok": true})),
        ("session_end", json!({"token": token})),
        ("session_recover", json!({})),
    ] {
        let out = timed(&mut b, tool, args);
        assert!(refused(&out, "another connection"), "{tool}: {out}");
    }
    let relaunch = timed(&mut b, "session_launch", json!({"user_ok": true}));
    assert!(refused(&relaunch, "a session is live"), "{relaunch}");
    let clean = timed(
        &mut a,
        "capture_clean",
        json!({"boots": [boot], "user_ok": true}),
    );
    assert!(refused(&clean, "a session is live"), "{clean}");
    let free = ids(&timed(
        &mut b,
        "ledger_ingest",
        json!({"rows": [row("b-free")]}),
    ));
    let wrong = timed(
        &mut b,
        "ledger_ingest",
        json!({"rows": [row("b-wrong")], "token": "not-a"}),
    );
    assert!(refused(&wrong, "wrong token"), "{wrong}");
    let boots = json!({"table": "boots", "game_id": 1}).to_string();
    let out = timed(&mut b, "ledger_ingest", json!({"rows": [boots]}));
    assert_eq!(out["refused"], true, "{out}");
    // Without a boot_id key, the token puts the row on its boot (an explicit null keeps it off).
    let mut mine: Value = serde_json::from_str(&row("a-own")).unwrap();
    mine.as_object_mut().unwrap().remove("boot_id");
    let own = ids(&timed(
        &mut a,
        "ledger_ingest",
        json!({"rows": [mine.to_string()], "token": token}),
    ));
    // Another daemon over the same data root is read-only for rows about a boot, as is the CLI.
    let mut other = Quen::spawn(&fx, fx.second_config(), 0);
    other.init(&fx);
    assert_ne!(other.daemon, a.daemon);
    for (tool, args) in [
        ("ledger_ingest", json!({"rows": [row("other")]})),
        ("capture_clean", json!({"boots": [boot], "user_ok": true})),
        ("session_launch", json!({"user_ok": true})),
    ] {
        let out = timed(&mut other, tool, args);
        assert!(
            refused(&out, "another quen holds the game lock"),
            "{tool}: {out}"
        );
    }
    let (code, out) = cli(&fx, &["ledger", "add"], &[], &row("cli"));
    assert_eq!(code, 1, "{out}");
    assert!(refused(&out, "another quen holds the game lock"), "{out}");
    let ended = a.tool("session_end", json!({"token": token}));
    assert_eq!(ended["ok"], true, "{ended}");
    fx.assert_restored();
    assert_eq!(
        fx.ledger("SELECT id, outcome FROM boots"),
        [[json!(boot), json!("ok")]]
    );
    assert_eq!(
        fx.ledger("SELECT id, boot_id, metric FROM measurements ORDER BY id"),
        [
            vec![json!(free[0]), Value::Null, json!("b-free")],
            vec![json!(own[0]), json!(boot), json!("a-own")],
        ]
    );
}

/// A daemon migrates an older ledger at its start while another process reads it (an older quen, a viewer): the
/// reader keeps its snapshot, the migration and its backup complete, nothing is lost and the file stays sound.
#[test]
fn serve_start_migrates_beside_an_outside_reader() {
    let fx = Fixture::new();
    let reader = old_ledger(&fx);
    reader.execute_batch("BEGIN").unwrap();
    let count = |c: &Connection| -> i64 {
        c.query_row("SELECT count(*) FROM claims", [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(count(&reader), 1);
    let mut q = Quen::start(&fx);
    // A session-thread call queues behind the startup migration.
    let out = timed(&mut q, "session_recover", json!({}));
    assert_eq!(out["ok"], true, "{out}");
    let new = timed(
        &mut q,
        "ledger_query",
        json!({"sql": "SELECT count(removed_to) FROM captures"}),
    );
    assert_eq!(new["ok"], true, "not migrated: {new}");
    assert_eq!(count(&reader), 1);
    assert_eq!(user_version(&reader), 6, "the reader's snapshot moved");
    reader.execute_batch("COMMIT").unwrap();
    assert_eq!(user_version(&reader), SCHEMA_VERSION);
    assert_eq!(integrity(&reader), "ok");
    assert_eq!(count(&reader), 1);
    let backups: Vec<PathBuf> = fs::read_dir(fx.path("data/state/backups"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(backups.len(), 1, "{backups:?}");
    let name = backups[0].file_name().unwrap().to_string_lossy();
    assert!(name.starts_with("quen-v6-"), "{name}");
    let backup = Connection::open(&backups[0]).unwrap();
    assert_eq!(user_version(&backup), 6);
    assert_eq!(count(&backup), 1);
}

/// A daemon starting while another process holds the older ledger's write lock: the start migration waits, gives
/// up with a reason in serve.log and changes nothing; writes are refused with a reason while the lock is held; the
/// first write after it is released migrates, with its backup.
#[test]
fn serve_start_waits_out_an_outside_writer_then_migrates_at_the_next_write() {
    let fx = Fixture::new();
    let writer = old_ledger(&fx);
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();
    let mut q = Quen::start(&fx);
    let out = q.tool("session_recover", json!({}));
    assert_eq!(out["ok"], true, "{out}");
    let log = fx.read("data/logs/serve.log");
    assert!(log.contains("migrates at the next write"), "{log}");
    let read = timed(
        &mut q,
        "ledger_query",
        json!({"sql": "SELECT text FROM claims"}),
    );
    assert_eq!(read["rows"], json!([["kept"]]), "{read}");
    let held = q.tool("ledger_ingest", json!({"rows": [row("held")]}));
    assert!(refused(&held, "busy"), "{held}");
    assert!(!fx.path("data/state/backups").exists());
    assert_eq!(user_version(&writer), 6);
    writer.execute_batch("COMMIT").unwrap();
    let out = timed(&mut q, "ledger_ingest", json!({"rows": [row("after")]}));
    assert_eq!(out["ok"], true, "{out}");
    assert_eq!(user_version(&writer), SCHEMA_VERSION);
    assert_eq!(integrity(&writer), "ok");
    assert_eq!(
        fx.ledger("SELECT metric FROM measurements"),
        [[json!("after")]]
    );
    assert_eq!(
        fs::read_dir(fx.path("data/state/backups")).unwrap().count(),
        1
    );
}

/// The CLI beside a daemon: `quen ledger add` writes alongside two connections' ingests, nothing lost; while a
/// session is live, `quen ledger add|migrate` and `quen recover` are refused and change nothing, and the session goes
/// on.
#[test]
fn the_cli_beside_a_running_daemon_is_refused_or_safe() {
    let fx = Fixture::new();
    let start = Barrier::new(3);
    thread::scope(|s| {
        for c in ["a", "b"] {
            let (fx, start) = (&fx, &start);
            s.spawn(move || {
                let mut q = Quen::start(fx);
                start.wait();
                for i in 0..10 {
                    let out = timed(
                        &mut q,
                        "ledger_ingest",
                        json!({"rows": [row(&format!("{c}{i}"))]}),
                    );
                    assert_eq!(out["ok"], true, "{out}");
                }
            });
        }
        start.wait();
        for i in 0..10 {
            let (code, out) = cli(&fx, &["ledger", "add"], &[], &row(&format!("cli{i}")));
            assert_eq!(code, 0, "{out}");
        }
    });
    let n = fx.ledger("SELECT count(*), count(DISTINCT metric) FROM measurements");
    assert_eq!(n, [[json!(30), json!(30)]]);
    let mut a = Quen::start(&fx);
    let launched = launch(&mut a);
    let journal = fx.read("data/state/journal.jsonl");
    for args in [&["ledger", "add"][..], &["ledger", "migrate"], &["recover"]] {
        let (code, out) = cli(&fx, args, &[], &row("live"));
        assert_eq!(code, 1, "{args:?}: {out}");
        let why = out["reason"].as_str().unwrap_or_default();
        assert!(
            why.contains("another quen holds the game lock"),
            "{args:?}: {out}"
        );
    }
    assert_eq!(fx.read("data/state/journal.jsonl"), journal);
    let status = timed(&mut a, "session_status", json!({}));
    assert_eq!(status["state"]["game"]["running"], true, "{status}");
    let ended = a.tool("session_end", json!({"token": launched["token"]}));
    assert_eq!(ended["ok"], true, "{ended}");
    fx.assert_restored();
    assert_eq!(
        n,
        fx.ledger("SELECT count(*), count(DISTINCT metric) FROM measurements")
    );
}

/// `quen setup --move` while a daemon serves the old folder: refused while its session is live, nothing copied;
/// with the daemon idle, every file is copied and checked, and the old folder, which the daemon's open files keep from
/// being renamed, is named in `left`; the daemon goes on serving it.
#[test]
fn moving_quens_folder_beside_a_running_daemon_is_refused_or_reported() {
    let fx = Fixture::new();
    // quen's older layout: config and data under a synthetic %APPDATA%\quen.
    let old = fx.path(r"roaming\quen");
    fs::create_dir_all(&old).unwrap();
    let config = fx.read("config.toml").replace(
        &format!("data_root = '{}'", fx.path("data").display()),
        &format!("data_root = '{}'", old.display()),
    );
    fs::write(old.join("config.toml"), config).unwrap();
    let env = [
        ("QUEN_CONFIG", old.join("config.toml")),
        ("APPDATA", fx.path("roaming")),
        ("LOCALAPPDATA", fx.path("local")),
        ("USERPROFILE", fx.path("profile")),
    ];
    let home = fx.path(r"profile\.quen");
    let mut a = Quen::spawn(&fx, r"roaming\quen\config.toml", 0);
    a.init(&fx);
    let launched = launch(&mut a);
    let (code, out) = cli(&fx, &["setup", "--move", "--user-ok"], &env, "");
    assert_eq!(code, 1, "{out}");
    assert!(refused(&out, "journal"), "{out}");
    assert!(!home.exists(), "copied while a session was live");
    let ended = a.tool("session_end", json!({"token": launched["token"]}));
    assert_eq!(ended["ok"], true, "{ended}");
    let (code, out) = cli(&fx, &["setup", "--move", "--user-ok"], &env, "");
    assert_eq!(code, 0, "{out}");
    assert!(out["files"].as_u64().unwrap() > 0, "{out}");
    assert_eq!(out["renamed"], json!([]), "{out}");
    assert_eq!(out["left"][0]["path"], json!(old), "{out}");
    let copy = Ledger::open_readonly(home.join(r"state\quen.db")).unwrap();
    let boots = copy.query("SELECT outcome FROM boots", &[], LIMIT).unwrap();
    assert_eq!(boots.rows, [[json!("ok")]]);
    let status = timed(&mut a, "session_status", json!({}));
    let root = status["data_root"].as_str().unwrap_or_default();
    assert!(root.ends_with(r"\roaming\quen"), "{status}");
}

/// A plain SQLite connection holding a read transaction, as a viewer does, never makes quen fail: a whole session
/// and every ledger tool run beside it, and the reader keeps its snapshot until it ends it.
#[test]
fn an_outside_reader_never_makes_quen_fail() {
    let fx = Fixture::new();
    let mut a = Quen::start(&fx);
    let out = a.tool("ledger_ingest", json!({"rows": [row("before")]}));
    assert_eq!(out["ok"], true, "{out}");
    let reader = outside(&fx);
    reader.execute_batch("BEGIN").unwrap();
    let count = |c: &Connection| -> i64 {
        c.query_row("SELECT count(*) FROM measurements", [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(count(&reader), 1);
    let launched = launch(&mut a);
    let token = &launched["token"];
    let out = timed(
        &mut a,
        "ledger_ingest",
        json!({"rows": [row("during")], "token": token}),
    );
    let m = ids(&out)[0];
    let claim =
        json!({"action": "open", "text": "beside a reader", "support": [{"measurement_id": m}]});
    let out = timed(&mut a, "ledger_claim", claim);
    assert_eq!(out["ok"], true, "{out}");
    let out = timed(
        &mut a,
        "ledger_query",
        json!({"sql": "SELECT count(*) FROM measurements"}),
    );
    assert_eq!(out["rows"], json!([[2]]), "{out}");
    let ended = a.tool("session_end", json!({"token": token}));
    assert_eq!(ended["ok"], true, "{ended}");
    assert!(ended.get("ledger_error").is_none(), "{ended}");
    fx.assert_restored();
    assert_eq!(count(&reader), 1, "the reader's snapshot moved");
    reader.execute_batch("COMMIT").unwrap();
    assert_eq!(count(&reader), 2);
    let boot = fx.ledger("SELECT outcome FROM boots");
    assert_eq!(boot, [[json!("ok")]]);
}

/// A plain SQLite connection holding the write lock past quen's busy timeout: each write waits, then is refused with
/// a reason that says the ledger is busy, and nothing of it is written; a launch is refused before it changes anything;
/// reads go on.
#[test]
fn an_outside_writer_past_the_busy_timeout_gets_a_clear_refusal_and_nothing_half_written() {
    let fx = Fixture::new();
    let mut a = Quen::start(&fx);
    let out = a.tool("ledger_ingest", json!({"rows": [row("before")]}));
    let m = ids(&out)[0];
    let writer = outside(&fx);
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();
    let calls = [
        ("ledger_ingest", json!({"rows": [row("x"), row("y")]})),
        (
            "ledger_claim",
            json!({"action": "open", "text": "held", "support": [{"measurement_id": m}]}),
        ),
        ("session_launch", json!({"user_ok": true})),
    ];
    let outs: Vec<Value> = thread::scope(|s| {
        let runs: Vec<_> = calls
            .iter()
            .map(|(tool, args)| {
                let fx = &fx;
                s.spawn(move || timed(&mut Quen::start(fx), tool, args.clone()))
            })
            .collect();
        let read = timed(
            &mut a,
            "ledger_query",
            json!({"sql": "SELECT metric FROM measurements"}),
        );
        assert_eq!(read["rows"], json!([["before"]]), "{read}");
        runs.into_iter().map(|r| r.join().unwrap()).collect()
    });
    for out in &outs {
        assert!(refused(out, "busy"), "{out}");
        let why = out["reason"].as_str().unwrap();
        assert!(
            !why.contains("database is locked"),
            "a raw SQLite error: {why}"
        );
    }
    fx.assert_restored();
    let status = timed(&mut a, "session_status", json!({}));
    assert_eq!(status["state"]["lock"]["free"], true, "{status}");
    writer.execute_batch("COMMIT").unwrap();
    assert_eq!(
        fx.ledger(
            "SELECT (SELECT count(*) FROM measurements), (SELECT count(*) FROM claims), \
                   (SELECT count(*) FROM boots)"
        ),
        [[json!(1), json!(0), json!(0)]]
    );
}

/// The write lock held by another program as a session ends: the end still restores everything and releases the
/// lock, and its result says the ledger was busy. The boot row keeps no end (t_end null): nothing records it later.
#[test]
fn an_end_beside_an_outside_writer_restores_and_says_the_ledger_was_busy() {
    let fx = Fixture::new();
    let mut a = Quen::start(&fx);
    let launched = launch(&mut a);
    let boot = launched["ledger"]["boots"][0].as_i64().unwrap();
    let writer = outside(&fx);
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();
    let ended = a.tool("session_end", json!({"token": launched["token"]}));
    writer.execute_batch("COMMIT").unwrap();
    assert_eq!(ended["ok"], true, "{ended}");
    let why = ended["ledger_error"].as_str().unwrap_or_default();
    assert!(
        why.contains("busy") && !why.contains("database is locked"),
        "{ended}"
    );
    assert_eq!(ended["state"]["lock"]["free"], true, "{ended}");
    fx.assert_restored();
    let row = fx.ledger(&format!(
        "SELECT outcome, t_end FROM boots WHERE id = {boot}"
    ));
    assert_eq!(row, [[Value::Null, Value::Null]]);
}

/// What quen_setup writes reaches every connection of the running daemon at once, with no reconnect, and a new
/// connection finds that same daemon (one per config file). While a session is live the daemon keeps its config, to
/// the session's end, then takes the new one up: a session never sees two.
#[test]
fn quen_setup_s_write_reaches_every_connection_once_no_session_is_live() {
    let fx = Fixture::new();
    let mut a = Quen::start(&fx);
    let mut b = Quen::start(&fx);
    assert_eq!(a.daemon, b.daemon);
    let budget = |q: &mut Quen| timed(q, "capture_usage", json!({}))["budget_gb"].clone();
    assert_eq!(budget(&mut b), json!(20.0));
    let set = |gb: &str| json!({"values": {"capture_budget_gb": gb}, "user_ok": true});
    let out = timed(&mut a, "quen_setup", set("7"));
    assert_eq!(
        (&out["ok"], &out["reloaded"]),
        (&json!(true), &json!(true)),
        "{out}"
    );
    assert_eq!(budget(&mut b), json!(7.0));
    let mut c = Quen::start(&fx);
    assert_eq!(c.daemon, a.daemon, "a second daemon for one config");
    assert_eq!(budget(&mut c), json!(7.0));
    // With a session live: written, and taken up once the session ends.
    let launched = launch(&mut a);
    let out = timed(&mut a, "quen_setup", set("9"));
    assert_eq!(
        (&out["ok"], &out["reloaded"]),
        (&json!(true), &json!(false)),
        "{out}"
    );
    let applies = out["applies"].as_str().unwrap_or_default();
    assert!(applies.contains("a session is live"), "{out}");
    assert_eq!(budget(&mut b), json!(7.0));
    let ended = a.tool("session_end", json!({"token": launched["token"]}));
    assert_eq!(ended["ok"], true, "{ended}");
    assert_eq!(
        ended["captures"]["budget_gb"],
        json!(7.0),
        "the session ran on one config to its end"
    );
    fx.assert_restored();
    assert_eq!(budget(&mut b), json!(9.0));
    assert_eq!(budget(&mut a), json!(9.0));
}

/// An edit by hand (here the CLI's `quen setup`) is taken up too, at the next connection, once no session is live.
#[test]
fn an_edit_outside_the_daemon_is_taken_up_at_the_next_connection() {
    let fx = Fixture::new();
    let mut a = Quen::start(&fx);
    let (code, out) = cli(
        &fx,
        &["setup", "capture_budget_gb=11", "--user-ok"],
        &[],
        "",
    );
    assert_eq!(code, 0, "{out}");
    assert_eq!(out["reloaded"], false, "{out}");
    let mut b = Quen::start(&fx);
    assert_eq!(b.daemon, a.daemon);
    for q in [&mut a, &mut b] {
        let usage = timed(q, "capture_usage", json!({}));
        assert_eq!(usage["budget_gb"], json!(11.0), "{usage}");
    }
}
