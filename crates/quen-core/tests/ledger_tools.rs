//! The core of the ledger tools: row batches for `ledger_ingest` / `quen ledger add`, and the import of the Python
//! harness's history, over a synthetic harness folder this test writes.

mod ledger_common;

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use ledger_common::{TempDb, boot, game, rows, sha};
use quen_core::ledger::{Error, Harness, Ledger, Row, Summary};
use serde_json::{Value, json};

fn parse(rows: Value) -> Vec<Row> {
    serde_json::from_value(rows).unwrap()
}

#[test]
fn a_batch_is_written_whole_or_not_at_all() {
    let mut l = Ledger::open_in_memory().unwrap();
    let g = game(&mut l);
    let b = boot(&mut l, g, "2026-10-10T10:00:00Z");
    let m = json!({"table": "measurements", "boot_id": b, "capture_id": null, "metric": "frame_ms", "value": 23.1,
                   "unit": "ms", "state_label": "A", "method_family": "tracy", "tag": "M", "run_id": null, "note": null});
    let save = json!({"table": "saves", "game_id": g, "file": "Synthetic_1.sav", "sha256": sha(5)});
    let bad = json!({"table": "measurements", "boot_id": b, "capture_id": null, "metric": "frame_ms", "value": 1.0,
                     "unit": null, "state_label": null, "method_family": "Not A Family", "tag": "M", "run_id": null,
                     "note": null});

    let batch = parse(json!([m, save, bad]));
    assert!(batch[0].needs_lock() && !batch[1].needs_lock());
    match l.ingest(&batch) {
        Err(Error::Refused(why)) => assert!(why.starts_with("row 2: "), "{why}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
    let count = "SELECT (SELECT count(*) FROM measurements) + (SELECT count(*) FROM saves)";
    assert_eq!(rows(&l, count, &[]), [[json!(0)]]);

    let label = json!({"table": "mod_versions", "content_hash": sha(7), "kind": "asi", "name": "Synthetic",
                       "version": "1.0", "provenance": "nexus_file", "source": "Synthetic-1-0.zip"});
    let re =
        json!({"table": "re_refs", "target": {"measurement": 1}, "kb_path": "notes/synthetic.md"});
    let ids = l.ingest(&parse(json!([m, save, label, re]))).unwrap();
    assert_eq!(ids, [Some(1), Some(1), None, Some(1)]);

    let unknown =
        json!([{"table": "saves", "game_id": g, "file": "x.sav", "sha256": sha(6), "extra": 1}]);
    assert!(serde_json::from_value::<Vec<Row>>(unknown).is_err());
    assert!(serde_json::from_value::<Vec<Row>>(json!([{"table": "boots", "game_id": g}])).is_err());
}

/// A synthetic harness folder in the temp folder; the test's own files go with it.
struct Fixture(PathBuf);

impl Fixture {
    fn new(name: &str) -> Fixture {
        let dir =
            std::env::temp_dir().join(format!("quen-migrate-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir); // a leftover of this same test
        Fixture(dir)
    }

    fn put(&self, rel: &str, text: &str) {
        let path = self.0.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn harness(&self) -> Harness {
        Harness {
            dir: self.0.join("harness"),
            game_exe: self.0.join("game").join("game.exe"),
            gamesaves: Some(self.0.join("gamesaves")),
            doc_roots: vec![self.0.join("harness")],
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0); // this test's own temp folder
    }
}

/// A local `YYYY-MM-DD HH:MM:SS` for `secs`, as the harness writes its `t` fields.
fn local(l: &Ledger, secs: i64) -> String {
    let t = rows(
        l,
        "SELECT strftime('%Y-%m-%d %H:%M:%S', ?1, 'unixepoch', 'localtime')",
        &[json!(secs)],
    );
    t[0][0].as_str().unwrap().into()
}

fn utc(secs: i64) -> String {
    let l = Ledger::open_in_memory().unwrap();
    let t = rows(
        &l,
        "SELECT strftime('%Y-%m-%dT%H:%M:%SZ', ?1, 'unixepoch')",
        &[json!(secs)],
    );
    t[0][0].as_str().unwrap().into()
}

fn mtime(fx: &Fixture, log: &str) -> i64 {
    let meta = fs::metadata(fx.0.join("harness").join("cap").join(log)).unwrap();
    meta.modified()
        .unwrap()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn traceback(error: &str) -> String {
    format!(
        "boot failed, restored: {{}}\nTraceback (most recent call last):\n  File \"harness.py\", line 9, in boot\n    \
         raise\n{error}\n"
    )
}

#[test]
fn the_harness_history_is_imported_once() {
    let fx = Fixture::new("once");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let (t_ok, t_jsonl, t_failed) = (now - 1200, now - 600, now - 60);
    let scratch = Ledger::open_in_memory().unwrap();
    let ended = local(&scratch, t_jsonl + 120);

    let info = json!({"t_boot": t_ok as f64 + 0.25, "save": null, "quicksave": {"save": "Synthetic_1.sav"},
                      "no_devkit": true, "parked": ["modSynthetic"]});
    fx.put(
        "harness/cap/perf_boot_ok.log",
        &format!("launching\n{info}\nprofiler: bye\n"),
    );
    let linked = json!({"t_boot": t_jsonl as f64 + 0.5, "save": "Synthetic_1.sav"});
    fx.put("harness/cap/perf_boot_linked.log", &format!("{linked}\n"));
    fx.put(
        "harness/cap/perf_boot_failed.log",
        &traceback("RuntimeError: the list was unread"),
    );
    // The same failure again, its own boot: a boot has one log.
    fx.put(
        "harness/cap/perf_boot_failed2.log",
        &traceback("RuntimeError: the list was unread"),
    );
    fx.put("harness/cap/perf_boot_killed.log", "launching\n");
    let jsonl = [
        json!({"event": "boot", "t_boot": t_jsonl as f64 + 0.5, "save": "Synthetic_1.sav", "t": "ignored"}),
        json!({"event": "end", "t": ended, "t_boot": t_jsonl as f64 + 0.5, "restored": {}}),
        json!({"event": "boot", "t_boot": t_failed as f64, "error": "RuntimeError: the list was unread"}),
        json!({"event": "end", "t": "2026-01-01 00:00:00", "t_boot": null}),
    ];
    let jsonl: Vec<String> = jsonl.iter().map(Value::to_string).collect();
    fx.put("harness/cap/boots.jsonl", &(jsonl.join("\n") + "\n"));
    fx.put(
        "harness/cap/ini/perf_A1.csv",
        "Application,MsBetweenPresents\n",
    );
    fx.put("harness/cap/ini/camera_ref_P1.png", "png");
    fx.put(
        "harness/cap/ini/camera_cal.json",
        "{\"deg_per_count\": 0.1}",
    );
    fx.put("harness/cap/run.sh", "python ini_session.py boot\n");
    fx.put("harness/cap/sub/helper.PY", "pass\n");
    fx.put(
        "harness/map/test_saves.md",
        "| # | scenario | file |\n|---|---|---|\n| 1 | hut | `Synthetic_1.sav` |\n| 2 | bog | Missing_2.sav |\n",
    );
    fx.put(
        "harness/PROGRESS.md",
        "see cap/ini/perf_A1.csv and cap/gone.png\n",
    );
    fx.put("gamesaves/Synthetic_1.sav", "save");
    fx.put("game/game.exe", "exe");
    let h = fx.harness();

    let db = TempDb::new();
    let mut l = Ledger::open(&db.0).unwrap();
    let plan = l.plan_migration(&h).unwrap();
    let expected = Summary {
        files: 9,
        scripts_skipped: 2,
        files_new: 9,
        bytes_new: plan.summary.bytes_new,
        files_changed: 0,
        boot_logs: 5,
        boot_logs_new: 5,
        jsonl_boots: 2,
        boots_new: 5,
        boots_new_failed: 3,
        saves_listed: 2,
        saves_new: 1,
        saves_missing: 1,
        docs: 5, // PROGRESS.md, test_saves.md, camera_cal.json and the two scripts
        refs: 2,
        dangling: 1,
    };
    assert_eq!(plan.summary, expected);
    let count = "SELECT (SELECT count(*) FROM captures) + (SELECT count(*) FROM boots) + (SELECT count(*) FROM saves)";
    assert_eq!(rows(&l, count, &[]), [[json!(0)]], "planning wrote rows");

    assert_eq!(l.apply_migration(&h, plan).unwrap(), expected);
    let boots = rows(
        &l,
        "SELECT b.t_start, b.t_end, b.outcome, b.failure, b.devkit, s.file, json_extract(b.harness_info, '$.parked[0]'),
           (SELECT group_concat(c.path) FROM captures c WHERE c.boot_id = b.id)
         FROM boots b LEFT JOIN saves s ON s.id = b.save_id ORDER BY b.t_start, 8",
        &[],
    );
    let (ok_end, failed2, killed) = (
        mtime(&fx, "perf_boot_ok.log"),
        mtime(&fx, "perf_boot_failed2.log"),
        mtime(&fx, "perf_boot_killed.log"),
    );
    let unread = "RuntimeError: the list was unread";
    let no_info = "the log has neither boot's info line nor a traceback (killed?)";
    assert_eq!(
        json!(boots),
        json!([
            [
                utc(t_ok),
                utc(ok_end),
                "ok",
                null,
                0,
                "Synthetic_1.sav",
                "modSynthetic",
                "perf_boot_ok.log"
            ],
            [
                utc(t_jsonl),
                utc(t_jsonl + 120),
                "ok",
                null,
                1,
                "Synthetic_1.sav",
                null,
                "perf_boot_linked.log"
            ],
            [
                utc(t_failed),
                utc(t_failed),
                "failed",
                unread,
                null,
                null,
                null,
                "perf_boot_failed.log"
            ],
            [
                utc(failed2),
                utc(failed2),
                "failed",
                unread,
                null,
                null,
                null,
                "perf_boot_failed2.log"
            ],
            [
                utc(killed),
                utc(killed),
                "failed",
                no_info,
                null,
                null,
                null,
                "perf_boot_killed.log"
            ],
        ])
    );
    let kinds = rows(
        &l,
        "SELECT path, kind, method, pinned FROM captures WHERE boot_id IS NULL ORDER BY path",
        &[],
    );
    assert_eq!(
        json!(kinds),
        json!([
            ["boots.jsonl", "boots_jsonl", null, 0],
            ["ini/camera_cal.json", "camera_cal", null, 1],
            ["ini/camera_ref_P1.png", "camera_ref", null, 1],
            ["ini/perf_A1.csv", "presentmon_csv", "presentmon", 0],
        ])
    );
    let saves = rows(&l, "SELECT file, note, sha256 FROM saves", &[]);
    assert_eq!(saves[0][..2], [json!("Synthetic_1.sav"), json!("1 | hut")]);
    let refs = rows(
        &l,
        "SELECT raw, capture_id IS NOT NULL FROM doc_refs ORDER BY raw",
        &[],
    );
    assert_eq!(
        refs,
        [
            [json!("cap/gone.png"), json!(0)],
            [json!("cap/ini/perf_A1.csv"), json!(1)]
        ]
    );

    // A second run adds nothing; a new boot record is the only addition after that.
    let again = l.plan_migration(&h).unwrap().summary;
    assert_eq!(
        (
            again.files_new,
            again.boots_new,
            again.saves_new,
            again.files_changed
        ),
        (0, 0, 0, 0)
    );
    assert_eq!(
        l.apply_migration(&h, l.plan_migration(&h).unwrap())
            .unwrap()
            .refs,
        2
    );
    assert_eq!(rows(&l, "SELECT count(*) FROM boots", &[]), [[json!(5)]]);
    let more = json!({"event": "boot", "t_boot": now as f64 + 5.0});
    fx.put(
        "harness/cap/boots.jsonl",
        &(jsonl.join("\n") + "\n" + &more.to_string() + "\n"),
    );
    let later = l.plan_migration(&h).unwrap().summary;
    assert_eq!(
        (
            later.files_new,
            later.files_changed,
            later.boots_new,
            later.jsonl_boots
        ),
        (0, 1, 1, 3)
    );

    fx.put("harness/cap/ini/session.lock", "{}");
    match l.apply_migration(&h, l.plan_migration(&h).unwrap()) {
        Err(Error::Refused(why)) => assert!(why.contains("session.lock"), "{why}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(rows(&l, "SELECT count(*) FROM boots", &[]), [[json!(5)]]);
}
