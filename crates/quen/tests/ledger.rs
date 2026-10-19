//! The ledger tools end to end: the real binary's `quen ledger add` / `migrate` and the MCP
//! tools `ledger_ingest`, `ledger_query` and `ledger_claim`, and game knowledge (`ledger_import`, `ledger_about`,
//! `quen kb`, `quen re`), on a synthetic data root and harness in a temp folder.
#![cfg(windows)]

mod common;

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};

struct Fixture(PathBuf);

impl Fixture {
    fn new(name: &str) -> Fixture {
        let dir =
            std::env::temp_dir().join(format!("quen-ledger-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir); // a leftover of this same test
        for (rel, text) in [
            ("harness/ini_session.py", "# not run here\n"),
            (
                "harness/cap/perf_boot_a.log",
                "{\"t_boot\": 1760000000.5}\n",
            ),
            ("harness/cap/ini/perf_A1.csv", "MsBetweenPresents\n16.6\n"),
            ("harness/cap/run.sh", "echo\n"),
            ("harness/notes.md", "cap/ini/perf_A1.csv\n"),
            ("game/bin/x64_dx12/game.exe", "exe"),
        ] {
            let path = dir.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        let config = format!(
            "harness = '{}'\ngame_dir = '{}'\ngame_exe = 'game.exe'\ndata_root = '{}'\n",
            dir.join("harness").join("ini_session.py").display(),
            dir.join("game").display(),
            dir.join("data").display(),
        );
        fs::write(dir.join("config.toml"), config).unwrap();
        Fixture(dir)
    }

    fn quen(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_quen"));
        cmd.args(args)
            .env("QUEN_CONFIG", self.0.join("config.toml"));
        cmd
    }

    /// Runs `quen ledger ARGS` with `stdin`: its exit code and JSON line (null when it printed none).
    fn cli(&self, args: &[&str], stdin: &str) -> (i32, Value) {
        let mut child = self
            .quen(&[&["ledger"], args].concat())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        let line = String::from_utf8(out.stdout).unwrap();
        (
            out.status.code().unwrap(),
            serde_json::from_str(&line).unwrap_or(Value::Null),
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0); // this test's own temp folder
    }
}

fn measurement(family: &str, value: f64) -> Value {
    json!({"table": "measurements", "boot_id": null, "capture_id": null, "metric": "frame_ms", "value": value,
           "unit": "ms", "state_label": "B", "method_family": family, "tag": "M", "run_id": null, "note": null})
}

#[test]
fn the_cli_records_rows_and_imports_the_harness_once() {
    let fx = Fixture::new("cli");
    let (code, out) = fx.cli(
        &["add"],
        &format!(
            "{}\n{}\n",
            measurement("tracy", 23.1),
            measurement("perf_func", 23.4)
        ),
    );
    assert_eq!((code, &out["ids"]), (0, &json!([1, 2])), "{out}");
    let (code, out) = fx.cli(
        &["add"],
        &json!([measurement("tracy", 1.0), {"table": "measurements"}]).to_string(),
    );
    assert_eq!(code, 1, "{out}");
    assert!(
        out["reason"].as_str().unwrap().starts_with("row 1: "),
        "{out}"
    );

    // Another quen holds the game lock (share mode 0, as quen opens it): boot rows are refused, others are not.
    fs::create_dir_all(fx.0.join("data/state")).unwrap();
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(0)
        .open(fx.0.join("data/state/game.lock"))
        .unwrap();
    let (code, out) = fx.cli(&["add"], &measurement("tracy", 2.0).to_string());
    assert_eq!(code, 1);
    assert!(
        out["reason"].as_str().unwrap().contains("game lock"),
        "{out}"
    );
    let save =
        json!({"table": "saves", "game_id": 1, "file": "x.sav", "sha256": format!("{:064x}", 1)});
    let (_, out) = fx.cli(&["add"], &save.to_string());
    assert!(
        out["reason"].as_str().unwrap().contains("FOREIGN KEY"),
        "not the lock: {out}"
    );
    let (code, out) = fx.cli(&["migrate"], "");
    assert_eq!(code, 1, "migrate writes boots: {out}");
    drop(lock);

    let (code, dry) = fx.cli(&["migrate", "--dry-run"], "");
    assert_eq!(code, 0, "{dry}");
    let s = &dry["summary"];
    assert_eq!(
        (
            &s["files_new"],
            &s["scripts_skipped"],
            &s["boots_new"],
            &s["docs"],
            &s["refs"],
            &s["dangling"]
        ),
        (
            &json!(2),
            &json!(1),
            &json!(1),
            &json!(3),
            &json!(1),
            &json!(0)
        ),
        "{dry}"
    );
    let (code, done) = fx.cli(&["migrate"], "");
    assert_eq!((code, &done["summary"]), (0, s), "{done}");
    let (_, again) = fx.cli(&["migrate", "--dry-run"], "");
    assert_eq!(
        (
            &again["summary"]["files_new"],
            &again["summary"]["boots_new"]
        ),
        (&json!(0), &json!(0))
    );
    let (code, _) = fx.cli(&["nonsense"], "");
    assert_eq!(code, 2);
}

/// `quen mcp` for a fixture's config. The proxy starts a `quen serve` for it, inside cargo's own job (which refuses
/// breakaway).
struct Mcp {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    id: u64,
}

impl Mcp {
    fn start(fx: &Fixture) -> Mcp {
        let mut child = fx
            .quen(&["mcp"])
            .env("QUEN_TEST_IN_JOB", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Mcp {
            child,
            stdin,
            stdout,
            id: 0,
        }
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.id += 1;
        let line = json!({"jsonrpc": "2.0", "id": self.id, "method": method, "params": params});
        writeln!(self.stdin.as_ref().unwrap(), "{line}").unwrap();
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }

    fn tool(&mut self, name: &str, args: Value) -> Value {
        let r = self.request("tools/call", json!({"name": name, "arguments": args}));
        serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
    }

    /// Closes the proxy, then waits for its daemon to exit: it keeps its log in the data root until then, a few
    /// seconds after its last connection.
    fn close(mut self) {
        let daemon = self.tool("session_status", json!({}))["serve_pid"].to_string();
        drop(self.stdin.take());
        self.child.wait().unwrap();
        let gone = || {
            let out = Command::new("tasklist")
                .args(["/FI", &format!("PID eq {daemon}"), "/NH", "/FO", "CSV"])
                .output()
                .unwrap();
            !String::from_utf8_lossy(&out.stdout).contains(&format!("\"{daemon}\""))
        };
        let t = std::time::Instant::now();
        while !gone() {
            assert!(
                t.elapsed().as_secs() < 30,
                "quen serve {daemon} did not exit"
            );
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }
}

#[test]
fn the_mcp_tools_ingest_query_and_judge_claims() {
    let fx = Fixture::new("mcp");
    let mut quen = Mcp::start(&fx);
    let mut tool = |name: &str, args: Value| quen.tool(name, args);

    let q = tool("ledger_query", json!({"sql": "SELECT 1"}));
    assert_eq!(q["refused"], true, "no ledger yet: {q}");
    let r = tool(
        "ledger_ingest",
        // A row as JSON text, as the schema has it (docs/clients.md), or as an object.
        json!({"rows": [measurement("tracy", 23.1), measurement("presentmon", 23.3).to_string()]}),
    );
    assert_eq!(
        (&r["ok"], &r["ledger"]),
        (&json!(true), &json!([1, 2])),
        "{r}"
    );

    // A number as a number or as text: the schema offers text, and SQLite reads it as the column's type.
    for param in [json!(23.2), json!("23.2")] {
        let sql = "SELECT method_family FROM measurements WHERE value > ?1";
        let q = tool("ledger_query", json!({"sql": sql, "params": [param]}));
        assert_eq!(
            (&q["rows"], &q["columns"]),
            (&json!([["presentmon"]]), &json!(["method_family"])),
            "{q}"
        );
    }
    let big = tool(
        "ledger_query",
        json!({"sql": "WITH RECURSIVE c(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM c WHERE i < 5000) SELECT i, 'padding' FROM c"}),
    );
    assert_eq!(
        (&big["truncated"], &big["row_count"]),
        (&json!(true), &json!(5000)),
        "{big}"
    );
    // A path under the user's folder comes back as %USERPROFILE%, which an agent expands to open it.
    let home = std::env::var("USERPROFILE").unwrap_or_default();
    let path = big["file"]
        .as_str()
        .unwrap()
        .replace("%USERPROFILE%", &home);
    let file: Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(file["rows"].as_array().unwrap().len(), 5000);
    assert!(big["rows"].as_array().unwrap().len() < 5000);
    let w = tool("ledger_query", json!({"sql": "DELETE FROM measurements"}));
    assert_eq!(w["refused"], true, "{w}");

    let support = json!([{"measurement_id": 1}, {"measurement_id": 2, "supports": true}]);
    let c = tool(
        "ledger_claim",
        json!({"action": "open", "text": "B is 0.2 ms slower", "support": support}),
    );
    assert_eq!(
        (&c["claim"]["proven"], &c["claim"]["families"], &c["ledger"]),
        (&json!(1), &json!(2), &json!([1])),
        "{c}"
    );
    let bad = tool(
        "ledger_claim",
        json!({"action": "support", "claim_id": 1, "support": [{"measurement_id": 99}]}),
    );
    assert_eq!(bad["refused"], true, "{bad}");
    let w = tool(
        "ledger_claim",
        json!({"action": "withdraw", "claim_id": 1, "reason": "drift was larger"}),
    );
    assert_eq!(
        (&w["claim"]["status"], &w["claim"]["proven"]),
        (&json!("withdrawn"), &json!(0)),
        "{w}"
    );
    let n = tool(
        "ledger_query",
        json!({"sql": "SELECT count(*) FROM claim_support"}),
    );
    assert_eq!(
        n["rows"],
        json!([[2]]),
        "the refused support wrote nothing: {n}"
    );

    let list = quen.request("tools/list", json!({}));
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"ledger_claim") && names.contains(&"session_launch"),
        "{names:?}"
    );
    quen.close();
}

/// Game knowledge end to end: ledger_import of a KB and an REA bundle, ledger_about, and the CLI twins (quen kb
/// scan and export, quen re import, quen ledger about); every import repeated adds nothing.
#[test]
fn the_knowledge_tools_import_once_and_answer() {
    let fx = Fixture::new("knowledge");
    let exe = "e".repeat(64);
    fs::create_dir_all(fx.0.join("data/state")).unwrap();
    let mut l = quen_core::ledger::Ledger::open(fx.0.join("data/state/quen.db")).unwrap();
    l.add_game(&quen_core::ledger::Game {
        name: "synthetic".into(),
        exe_path: "%GAME_DIR%/game.exe".into(),
        exe_sha256: exe.clone(),
        build_stamp: None,
    })
    .unwrap();
    drop(l);
    let (kb, bundle) = common::knowledge(&fx.0, &exe);
    let (kb_path, bundle_path) = (kb.to_str().unwrap(), bundle.to_str().unwrap());

    let mut quen = Mcp::start(&fx);
    let scan = quen.tool("ledger_import", json!({"kind": "kb", "path": kb_path}));
    let got = (
        &scan["ok"],
        &scan["imported"]["names"],
        &scan["imported"]["links"],
    );
    assert_eq!(got, (&json!(true), &json!(2), &json!(1)), "{scan}");
    let args = json!({"kind": "rea_bundle", "path": bundle_path, "rea_version": "6.5.0"});
    let rea = quen.tool("ledger_import", args);
    let got = (
        &rea["imported"]["extractions_new"],
        &rea["imported"]["links_new"],
    );
    assert_eq!(got, (&json!(2), &json!(1)), "{rea}");
    let no_version = quen.tool(
        "ledger_import",
        json!({"kind": "rea_bundle", "path": bundle_path}),
    );
    let why = no_version["reason"].as_str().unwrap_or_default();
    assert!(
        no_version["refused"] == true && why.contains("rea_version"),
        "{no_version}"
    );
    let about = quen.tool(
        "ledger_about",
        json!({"kind": "function", "key": "Alpha_Draw", "operation": "procedure_pseudo_code",
               "params": "{\"procedure\": \"0x140001000\"}", "profile": "7".repeat(64)}),
    );
    let exact = &about["about"]["extractions"]["exact"];
    assert_eq!(exact.as_array().map(Vec::len), Some(1), "{about}");
    assert_eq!(exact[0]["evidence_id"], json!(format!("ev_{:064x}", 1)));
    let stored =
        fx.0.join("data/captures")
            .join(exact[0]["bundle"].as_str().unwrap());
    assert_eq!(
        fs::read(stored).unwrap(),
        fs::read(&bundle).unwrap(),
        "the pinned copy"
    );
    quen.close();

    let run = |args: &[&str]| {
        let out = fx.quen(args).output().unwrap();
        let json = serde_json::from_slice::<Value>(&out.stdout).unwrap_or(Value::Null);
        (out.status.code().unwrap(), json)
    };
    let (code, again) = run(&["kb", "scan", kb_path]);
    assert_eq!((code, &again["sections_new"]), (0, &json!(0)), "{again}");
    let (code, again) = run(&["re", "import", bundle_path, "--rea-version", "6.5.0"]);
    assert_eq!((code, &again["extractions_new"]), (0, &json!(0)), "{again}");
    let (code, about) = run(&[
        "ledger",
        "about",
        r#"{"kind": "hook", "key": "ModA:hook:Alpha_Draw"}"#,
    ]);
    assert_eq!(
        (code, &about["about"]["links"][0]["other"]),
        (0, &json!("function:Alpha_Draw")),
        "{about}"
    );
    let (code, export) = run(&["kb", "export", kb_path]);
    assert_eq!(code, 0, "{export}");
    let claims = fs::read_to_string(kb.join("ledger").join("claims.tsv")).unwrap();
    assert!(
        claims.starts_with("key	text	") && claims.lines().count() == 1,
        "no keyed claims yet: {claims}"
    );
    assert_eq!(run(&["kb", "nonsense"]).0, 2);
    assert_eq!(run(&["re", "import", bundle_path]).0, 2);
}

/// Feature tests 2: after a quen update the ledger kept its old schema until the first write, so ledger_query found
/// none of the newer tables and views. quen serve now migrates it at start, under the game lock, with the backup.
#[test]
fn serve_migrates_an_older_ledger_at_start() {
    let fx = Fixture::new("migrate");
    // A schema-1 ledger, as the first quen left it: the shipped first migration, through Python's sqlite3.
    let state = fx.0.join("data/state");
    fs::create_dir_all(&state).unwrap();
    let init = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schema/0001_init.sql");
    let script = "import sqlite3, sys; c = sqlite3.connect(sys.argv[1]); \
                  c.executescript(open(sys.argv[2], encoding='utf-8').read()); \
                  c.execute('PRAGMA user_version = 1'); c.commit()";
    let made = Command::new("python")
        .args(["-c", script])
        .arg(state.join("quen.db"))
        .arg(init)
        .status()
        .unwrap();
    assert!(made.success());
    let mut quen = Mcp::start(&fx);
    // A call on the session thread, which runs only after the migration.
    let recover = quen.tool("session_recover", json!({}));
    assert_eq!(recover["replayed"], false, "{recover}");
    let version = quen.tool(
        "ledger_query",
        json!({"sql": "SELECT user_version FROM pragma_user_version"}),
    );
    assert_eq!(
        version["rows"],
        json!([[quen_core::ledger::SCHEMA_VERSION]]),
        "{version}"
    );
    let backups: Vec<String> = fs::read_dir(state.join("backups"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        matches!(&backups[..], [one] if one.starts_with("quen-v1-")),
        "{backups:?}"
    );
    quen.close();
}
