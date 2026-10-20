//! script_compile and quen_setup licence end to end: the real binary's daemon runs a stand-in wcc_lite
//! (`examples/fake_wcc_lite.rs`) over the synthetic game of the session tests' fixture, with what it sends to the
//! Recycle Bin kept in the fixture. `QUEN_TEST_EULA` stands in for REDkit's licence answer (accepted unless a test says
//! otherwise): no test reads or writes the user's.
#![cfg(windows)]

mod common;

use std::fs;
use std::path::PathBuf;

use serde_json::{Value, json};

use common::*;

/// A synthetic REDkit in the fixture, its wcc_lite the stand-in, writing the fixture's documents folder; in the config.
fn redkit(fx: &Fixture) {
    let bin = fx.path(r"redkit\bin\x64_RedKit");
    fs::create_dir_all(&bin).unwrap();
    fs::copy(fake_wcc_lite(), bin.join("wcc_lite.exe")).unwrap();
    fs::write(
        bin.join("fake_docs.txt"),
        fx.path("docs").display().to_string(),
    )
    .unwrap();
    fx.config(&format!("redkit = '{}'\n", fx.path("redkit").display()));
}

/// The scratch folder a result names, shown as %USERPROFILE% when the fixture is under the user's folder (TEMP is).
fn scratch(out: &Value) -> PathBuf {
    let home = std::env::var("USERPROFILE").unwrap_or_default();
    PathBuf::from(
        out["scratch"]
            .as_str()
            .unwrap()
            .replace("%USERPROFILE%", &home),
    )
}

/// The tree holds the game's scripts and the mods' (stubbed, the first in name order winning a conflict), the switches
/// are passed, the log's errors and warnings are parsed, and the documents folder's settings are put back (bytes and
/// write time), the file wcc_lite made recycled, the journal retired. Without REDkit: refused with needs.
#[test]
fn script_compile_runs_wcc_lite_and_puts_the_settings_back() {
    let fx = Fixture::new();
    let put = |rel: &str, text: &str| {
        let path = fx.path(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        path
    };
    put(r"game\content\content0\scripts\game\x.ws", "class X {}\n");
    put(
        r"game\Mods\modA\content\scripts\local\a.ws",
        "import function A() : int;\n",
    );
    put(r"game\Mods\modB\content\scripts\local\a.ws", "// modB\n");
    let profile = put(r"docs\profile.settings", "[Telemetry]\noriginal=1\n");
    let mtime = fs::metadata(&profile).unwrap().modified().unwrap();
    let mut quen = Quen::start(&fx);
    let without = quen.tool("script_compile", json!({}));
    assert_eq!(
        without["needs"][0]["dependency"], "The Witcher 3 REDkit",
        "{without}"
    );
    drop(quen);
    redkit(&fx);
    let mut quen = Quen::start(&fx);
    let out = quen.tool("script_compile", json!({"stubs": true}));
    assert_eq!(out["ok"], true, "{out}");
    assert_eq!(out["exit_code"], 1);
    assert_eq!(out["error_count"], 2);
    let first = json!({"file": r"local\a.ws", "line": 1, "msg": "Cannot find global function 'A' to import."});
    assert_eq!(out["errors"][0], first);
    assert_eq!(out["warning_count"], 1);
    assert_eq!(out["conflicts"][0]["file"], r"local\a.ws");
    assert_eq!(out["conflicts"][0]["used"], "modA");
    assert_eq!(
        fs::read_to_string(&profile).unwrap(),
        "[Telemetry]\noriginal=1\n"
    );
    assert_eq!(fs::metadata(&profile).unwrap().modified().unwrap(), mtime);
    assert!(!fx.path(r"docs\editoruser.settings").exists());
    assert!(!fx.path(r"data\state\journal.jsonl").exists());
    let scratch = scratch(&out);
    assert!(scratch.join("journal.jsonl").is_file());
    let a = fs::read_to_string(scratch.join(r"scripts\local\a.ws")).unwrap();
    assert_eq!(a, "function A() : int { return 0; }\n");
    assert!(scratch.join(r"scripts\game\x.ws").is_file());
    let said = fs::read_to_string(scratch.join("wcc_lite.log")).unwrap();
    assert!(
        said.contains("-agreetoterms -noninteractivecrash"),
        "{said}"
    );
    assert!(scratch.join("wcc.log").is_file());
    assert!(fx.path("recycled").read_dir().unwrap().count() >= 1);
}

/// REDkit's licence is the user's answer in its own dialog. Until it is 1, quen_doctor says so and script_compile is
/// refused with needs, nothing changed. quen_setup licence is refused as script_compile is (no REDkit, a journal left,
/// the game running, a live session); otherwise it runs wcc_lite once without -agreetoterms and with no command, puts
/// the documents folder's settings back, and reports the answer read again.
#[test]
fn the_licence_is_the_users_answer_in_redkits_dialog() {
    let fx = Fixture::new();
    let profile = fx.path(r"docs\profile.settings");
    fs::write(&profile, "[Telemetry]\noriginal=1\n").unwrap();
    let start = |fx: &Fixture| {
        let mut quen = Quen::spawn_with(fx, "config.toml", 0, &[("QUEN_TEST_EULA", "0")]);
        quen.init(fx);
        quen
    };
    let licence = json!({"licence": true});
    let mut quen = start(&fx);
    let without = quen.tool("quen_setup", licence.clone());
    assert_eq!(
        without["needs"][0]["dependency"], "The Witcher 3 REDkit",
        "{without}"
    );
    quen.kill_daemon();
    redkit(&fx);
    let mut quen = start(&fx);
    let doctor = quen.tool("quen_doctor", json!({}));
    let deps = doctor["dependencies"].as_array().unwrap();
    let row = deps
        .iter()
        .find(|d| d["name"] == "The Witcher 3 REDkit")
        .unwrap();
    let note = row["note"].as_str().unwrap();
    assert_eq!(row["status"], "found", "{row}");
    assert!(
        note.contains("not accepted (DoNotShowEula = 0)") && note.contains("quen_setup"),
        "{note}"
    );
    let refused = quen.tool("script_compile", json!({}));
    assert_eq!(refused["refused"], true, "{refused}");
    assert_eq!(refused["needs"][0]["status"], "licence_not_accepted");
    assert_eq!(refused["needs"][0]["needed_for"], "script_compile");
    assert!(!fx.path(r"docs\editoruser.settings").exists());

    let reason = |out: Value| out["reason"].as_str().unwrap_or_default().to_owned();
    let journal = fx.path(r"data\state\journal.jsonl");
    fs::create_dir_all(journal.parent().unwrap()).unwrap();
    fs::write(&journal, "").unwrap();
    let left = reason(quen.tool("quen_setup", licence.clone()));
    fs::remove_file(&journal).unwrap(); // the test's own
    assert!(left.contains("session_recover first"), "{left}");
    let mut game = outside_game(&fx);
    let running = reason(quen.tool("quen_setup", licence.clone()));
    let _ = game.kill();
    let _ = game.wait();
    assert!(running.contains("the game runs"), "{running}");
    let launched = launch(&mut quen);
    let live = reason(quen.tool("quen_setup", licence.clone()));
    assert!(live.contains("a session is live"), "{live}");
    let ended = quen.tool("session_end", json!({"token": launched["token"]}));
    assert_eq!(ended["ok"], true, "{ended}");
    assert!(!fx.path(r"docs\editoruser.settings").exists());

    let out = quen.tool("quen_setup", licence);
    assert_eq!(out["ok"], true, "{out}");
    assert_eq!(
        (&out["accepted"], &out["do_not_show_eula"]),
        (&json!(false), &json!(0))
    );
    assert_eq!(
        (&out["exit_code"], &out["dialog_shown"]),
        (&json!(0), &json!(false))
    );
    let said = fs::read_to_string(scratch(&out).join("wcc_lite.log")).unwrap();
    assert_eq!(said.trim(), "", "no command and no switch");
    assert_eq!(
        fs::read_to_string(&profile).unwrap(),
        "[Telemetry]\noriginal=1\n"
    );
    assert!(!fx.path(r"docs\editoruser.settings").exists());
    assert!(!journal.exists());
}
