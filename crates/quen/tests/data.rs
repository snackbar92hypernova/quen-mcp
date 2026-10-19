//! script_compile end to end: the real binary's daemon runs a stand-in wcc_lite (`examples/fake_wcc_lite.rs`) over the
//! synthetic game of the session tests' fixture, with what it sends to the Recycle Bin kept in the fixture.
#![cfg(windows)]

mod common;

use std::fs;
use std::path::PathBuf;

use serde_json::json;

use common::*;

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
    let bin = fx.path(r"redkit\bin\x64_RedKit");
    fs::create_dir_all(&bin).unwrap();
    fs::copy(fake_wcc_lite(), bin.join("wcc_lite.exe")).unwrap();
    fs::write(
        bin.join("fake_docs.txt"),
        fx.path("docs").display().to_string(),
    )
    .unwrap();
    fx.config(&format!("redkit = '{}'\n", fx.path("redkit").display()));
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
    // Shown as %USERPROFILE% when the fixture is under the user's folder (TEMP is, here).
    let home = std::env::var("USERPROFILE").unwrap_or_default();
    let scratch = PathBuf::from(
        out["scratch"]
            .as_str()
            .unwrap()
            .replace("%USERPROFILE%", &home),
    );
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
