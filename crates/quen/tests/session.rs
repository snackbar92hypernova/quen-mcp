//! The quen front end to end: the real `quen mcp` proxy over stdio and the `quen serve`
//! daemon it starts, booting a synthetic game natively: a stand-in game exe (`examples/fake_game.rs`, copied under a
//! name of this fixture's own) in a synthetic game folder, and fake input scripts in a fake harness folder (the game_*
//! tools still type through scripts) that write what quen's state log would. All files are synthetic, in a temp folder;
//! each fixture's config gets its own daemon. `cargo test` builds the fake game (an example); `--test session` alone
//! doesn't, so build it first then.
#![cfg(windows)]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, TryRecvError};
use std::thread::{self, sleep};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, GenerateConsoleCtrlEvent};
use windows_sys::Win32::System::JobObjects::{
    JOB_OBJECT_LIMIT_BREAKAWAY_OK, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW};

mod common;

use common::*;

#[test]
fn launch_then_end_restores_and_kills_the_game() {
    let fx = Fixture::new();
    let mut quen = Quen::start(&fx);
    let launched = launch(&mut quen);
    // The boot's changes: frame generation and camera auto-rotate off, the console on; the game in the job.
    let settings = fx.settings();
    assert!(
        settings.contains("DLSSGMode=0\r\n") && settings.contains("CameraAutoRotX=false\r\n"),
        "{settings}"
    );
    assert!(
        fx.read("game/bin/config/base/general.ini")
            .contains("DBGConsoleOn=true")
    );
    assert_eq!(launched["state"]["lock"], "held here");
    assert_eq!(launched["state"]["game"]["running"], true);
    let game = fx.game_pid();
    assert!(launched["pids"].as_array().unwrap().contains(&json!(game)));
    assert_eq!(launched["job_covers_game"], true);
    for step in ["files", "launch", "window", "continue", "photo"] {
        assert!(launched["timing_s"][step].is_number(), "{step}: {launched}");
    }
    // input.settings kept, dx12user.settings and general.ini patched.
    assert_eq!(
        launched["changes"].as_array().unwrap().len(),
        3,
        "{launched}"
    );
    // Nothing typed at the main menu (letters reach the menu too). Continue (E) loaded the newest save, which is the
    // one asked for (the newest QuickSave), so nothing was typed at all; then photo mode. Live boot 1: no key of
    // Continue's after the first push, so none came near gameplay (the pushes carry ticks to compare with).
    assert_eq!(launched["continued"], "QuickSave.sav", "{launched}");
    assert!(launched.get("load").is_none(), "{launched}");
    assert_eq!(fx.calls(), ["keys e", "keys u:200"]);
    assert_eq!(launched["continue_keys"], 1, "{launched}");
    assert_eq!(launched["keys_after_gameplay"], json!([]), "{launched}");

    let wrong = quen.tool("session_end", json!({"token": "not-it"}));
    assert_eq!(
        (wrong["ok"].clone(), wrong["refused"].clone()),
        (json!(false), json!(true))
    );
    assert!(alive(game));

    // gamesaves was copied into the data root before the launch.
    assert_eq!(launched["saves_copy"]["copied"], 1, "{launched}");
    assert_eq!(fx.read("data/gamesaves/QuickSave.sav"), "quick");
    // The boot's row names the game, the save it loads and the mods that loaded.
    let boot = launched["ledger"]["boots"][0].as_i64().unwrap();
    for table in ["games", "saves", "mod_sets"] {
        assert!(launched["ledger"][table][0].is_i64(), "{table}: {launched}");
    }

    // A row without boot_id goes on the live boot only with the session's token; a wrong token writes nothing.
    let row = json!({"table": "measurements", "metric": "frame_ms", "value": 1.0, "unit": "ms",
                     "method_family": "tracy", "tag": "M"});
    let free = quen.tool("ledger_ingest", json!({"rows": [row]}));
    assert_eq!(free["ok"], true, "{free}");
    let owned = quen.tool(
        "ledger_ingest",
        json!({"rows": [row], "token": launched["token"]}),
    );
    assert_eq!(owned["ok"], true, "{owned}");
    let wrong = quen.tool("ledger_ingest", json!({"rows": [row], "token": "not-it"}));
    assert_eq!(wrong["refused"], true, "{wrong}");
    let rows = fx.ledger("SELECT boot_id FROM measurements ORDER BY id");
    assert_eq!(rows, [[Value::Null], [json!(boot)]]);

    // The game writes an AutoSave: end waits for gamesaves to stay unchanged before the kill, then lists it.
    fs::write(fx.path("docs/gamesaves/AutoSave_1.sav"), "auto").unwrap();
    let ended = quen.tool("session_end", json!({"token": launched["token"]}));
    assert_eq!(ended["ok"], true, "{ended}");
    assert_eq!(ended["saves_wait"]["quiet"], true, "{ended}");
    assert_eq!(ended["saves"]["changed"], json!(["AutoSave_1.sav"]));
    assert_eq!(ended["saves"]["unexpected"], json!([]));
    assert_eq!(ended["saves"]["tainted"], json!([]));
    // The boot's own logs in the game folder, copied beside its journal (the stand-in's state log among them).
    let logs = ended["artifacts"].to_string();
    assert!(
        ended["game_logs"]["files"].as_u64() >= Some(1) && logs.contains("W3QuenState.log"),
        "{ended}"
    );
    // What the session left in the capture root, against the budget.
    let captures = &ended["captures"];
    assert_eq!(
        (
            &captures["budget_gb"],
            &captures["over_budget"],
            &captures["boot"]["large"]
        ),
        (&json!(20.0), &json!(false), &json!([])),
        "{ended}"
    );
    // The two patches back, and input.settings as it was before the game reordered it.
    assert_eq!(
        ended["replay"]["undone"].as_array().unwrap().len(),
        3,
        "{ended}"
    );
    assert_eq!(ended["ledger"]["boots"], json!([boot]));
    assert!(dies(game));
    fx.assert_restored();
    assert_eq!(ended["state"]["lock"]["free"], true);
    // The spent backups went to the Recycle Bin (here, the fixture's stand-in for it).
    assert_eq!(fs::read_dir(fx.path("recycled")).unwrap().count(), 3);

    let rows = fx.ledger(
        "SELECT b.outcome, b.devkit, b.fg, b.vsync, json_remove(b.settings_diff, '$.build'), s.file,
                b.mod_set_id IS NOT NULL, b.t_end >= b.t_start, b.journal_path, b.settings_diff ->> '$.build.status'
           FROM boots b JOIN saves s ON s.id = b.save_id",
    );
    let journal = rows[0][8].as_str().unwrap();
    assert!(Path::new(journal).exists(), "the row names the journal");
    assert_eq!(
        rows[0][..8],
        [
            json!("ok"),
            json!(0),
            json!(0),
            Value::Null,
            json!("{}"),
            json!("QuickSave.sav"),
            json!(1),
            json!(1)
        ]
    );
    assert_eq!(rows[0][9], "known", "the build, on the boot row");
    let entries = fx.ledger(
        "SELECT kind, name, parked, json_extract(config_values, '$.Mode') FROM mod_set_entries ORDER BY kind, name",
    );
    assert_eq!(
        entries,
        [
            [json!("asi"), json!("W3Fake"), json!(0), json!("raise")],
            [json!("asi"), json!("W3Quen"), json!(0), Value::Null],
            [
                json!("asi"),
                json!("WitcherNativeBridge"),
                json!(0),
                Value::Null
            ],
            [json!("mods"), json!("modAlpha"), json!(0), Value::Null],
            [
                json!("mods"),
                json!("modNativeBridge"),
                json!(0),
                Value::Null
            ],
            [json!("mods"), json!("modQuen"), json!(0), Value::Null]
        ]
    );
    let saves = fx.ledger("SELECT file, tainted_by_boot FROM saves ORDER BY file");
    assert_eq!(
        saves,
        [
            [json!("AutoSave_1.sav"), Value::Null],
            [json!("QuickSave.sav"), Value::Null]
        ]
    );
}

/// A game-memory write (here a mem_patch) is journaled and goes on the boot's patch list: the boot row
/// records it, and the saves the game writes after it are tainted, listed first at the end and marked in the ledger.
#[test]
fn saves_written_after_a_memory_write_are_tainted() {
    let fx = Fixture::new();
    // The patch: 4 bytes of the fake game's DOS stub message, which nothing reads once it is loaded (the headers lie in
    // memory as in the file).
    let kb = fx.path("kb");
    fx.config(&format!("kb = '{}'\n", kb.display()));
    let file = fs::read(fx.path("game/bin/x64_dx12").join(&fx.2)).unwrap();
    let word = |at: usize| u32::from_le_bytes(file[at..at + 4].try_into().unwrap());
    let stamp = word(word(0x3c) as usize + 8);
    let hex = |b: &[u8]| {
        b.iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let (old, new) = (
        &file[0x50..0x54],
        file[0x50..0x54].iter().map(|b| !b).collect::<Vec<_>>(),
    );
    let row = format!(
        "stub\t0x50\t{}\t{}\t0x{stamp:08X}\ttest\n",
        hex(old),
        hex(&new)
    );
    fs::create_dir_all(&kb).unwrap();
    fs::write(
        kb.join("patches.tsv"),
        format!("name\trva\told\tnew\tbuild\tboundaries-checked\n{row}"),
    )
    .unwrap();

    let mut quen = Quen::start(&fx);
    let launched = launch(&mut quen);
    let token = &launched["token"];
    assert_eq!(launched["state"]["boot"], launched["ledger"]["boots"][0]);
    fs::write(fx.path("docs/gamesaves/AutoSave_1.sav"), "before").unwrap();
    sleep(Duration::from_millis(50));
    let read = json!({"token": token, "rva": "0x50", "type": "bytes", "len": 4});
    assert_eq!(quen.tool("mem_read", read.clone())["value"], hex(old));
    let patched = quen.tool("mem_patch", json!({"token": token, "name": "stub"}));
    assert_eq!(patched["ok"], true, "{patched}");
    assert_eq!(quen.tool("mem_read", read)["value"], hex(&new));
    sleep(Duration::from_millis(50));
    fs::write(fx.path("docs/gamesaves/AutoSave_2.sav"), "after").unwrap();
    let ended = quen.tool("session_end", json!({"token": token}));
    assert_eq!(ended["ok"], true, "{ended}");
    assert_eq!(ended["saves"]["tainted"], json!(["AutoSave_2.sav"]));
    let boot = &ended["ledger"]["boots"][0];
    let saves = fx.ledger(
        "SELECT file, tainted_by_boot FROM saves WHERE file LIKE 'AutoSave%' ORDER BY file",
    );
    assert_eq!(
        saves,
        [
            [json!("AutoSave_1.sav"), Value::Null],
            [json!("AutoSave_2.sav"), boot.clone()]
        ]
    );
    let patches = fx.ledger("SELECT json_extract(patches, '$[0].what'), json_extract(patches, '$[0].write.new') FROM boots");
    let what = patches[0][0].as_str().unwrap();
    assert!(what.starts_with("mem_patch stub at 0x"), "{patches:?}");
    assert_eq!(patches[0][1], hex(&new));
}

/// A save that isn't the newest, named as the game's own list names it (lowercase, no `.sav`): Continue loads the
/// newest first, then the one asked for is typed from gameplay, under its name on disk.
#[test]
fn a_save_that_isnt_the_newest_is_loaded_from_gameplay() {
    let fx = Fixture::new();
    let manual = fx.path("docs/gamesaves/ManualSave_10ad3f.sav");
    fs::write(&manual, "manual").unwrap();
    let long_ago = std::time::UNIX_EPOCH + Duration::from_secs(1_600_000_000);
    fs::File::options()
        .write(true)
        .open(&manual)
        .unwrap()
        .set_modified(long_ago)
        .unwrap();
    let mut quen = Quen::start(&fx);
    let dry = quen.tool("session_plan", json!({"save": "MANUALSAVE_10AD3F.SAV"}));
    assert!(
        dry["save"]
            .as_str()
            .unwrap()
            .ends_with("ManualSave_10ad3f.sav"),
        "{dry}"
    );
    let missing = quen.tool("session_plan", json!({"save": "manualsave_10ad3"}));
    assert_eq!(missing["refused"], true, "{missing}");
    let launched = launch_with(&mut quen, json!({"save": "manualsave_10ad3f"}));
    // The stand-in game serves no command channel: the menu load is tried once, then Continue, then the typed load.
    assert_eq!(launched["first_load"], "continue", "{launched}");
    let tried = launched["menu_load"][0]["why"].as_str().unwrap();
    assert!(tried.contains("isn't up"), "{launched}");
    assert_eq!(launched["continued"], "QuickSave.sav", "{launched}");
    assert_eq!(launched["load"]["save"], "ManualSave_10ad3f", "{launched}");
    assert_eq!(launched["load"]["via"], "console", "{launched}");
    assert_eq!(
        fx.calls(),
        [
            "keys e",
            "console quenload(ManualSave_10ad3f)",
            "close",
            "keys u:200"
        ]
    );
    let rows = fx.ledger("SELECT s.file FROM boots b JOIN saves s ON s.id = b.save_id");
    assert_eq!(rows, [[json!("ManualSave_10ad3f.sav")]]);
    let ended = quen.tool("session_end", json!({"token": launched["token"]}));
    assert_eq!(ended["ok"], true, "{ended}");
    fx.assert_restored();
}

/// A manual save written during the boot (a key confirmed something it shouldn't have) ends the session after
/// the load, everything given back, and the boot's row says why. The failed launch keeps its Continue keys (live boot
/// 2: a failed boot's key count was lost).
#[test]
fn a_manual_save_during_the_boot_ends_the_session() {
    let fx = Fixture::new();
    let saves = fx.path("docs/gamesaves").display().to_string();
    fs::write(fx.path("game/bin/x64_dx12/manual_save"), saves).unwrap();
    let mut quen = Quen::start(&fx);
    let launch = quen.tool("session_launch", json!({"user_ok": true}));
    let why = launch["reason"].as_str().unwrap();
    assert!(
        launch["refused"] == false && why.contains("ManualSave_1.sav"),
        "{launch}"
    );
    assert_eq!(launch["continue_keys"], 1, "{launch}");
    assert_eq!(launch["keys_after_gameplay"], json!([]), "{launch}");
    assert_eq!(launch["continued"], "ManualSave_1.sav", "{launch}");
    assert_eq!(launch["load"]["save"], "QuickSave", "{launch}");
    assert!(dies(fx.game_pid()));
    fx.assert_restored();
    assert_eq!(
        launch["end"]["fields"]["saves"]["unexpected"],
        json!(["ManualSave_1.sav"])
    );
    let rows = fx.ledger("SELECT outcome, failure FROM boots");
    assert_eq!(rows[0][0], "failed");
    assert!(rows[0][1].as_str().unwrap().contains("ManualSave_1.sav"));
}

/// After every input batch: a save other than the game's own, written while an agent drives, ends the session
/// at the next game_keys / game_console / game_load, not only at session_end.
#[test]
fn a_manual_save_after_keys_ends_the_session_at_once() {
    let fx = Fixture::new();
    let mut quen = Quen::start(&fx);
    let launched = launch(&mut quen);
    let token = launched["token"].clone();
    let calm = quen.tool("game_keys", json!({"token": token, "tokens": ["e"]}));
    assert_eq!(calm["ok"], true, "{calm}");
    fs::write(fx.path("docs/gamesaves/ManualSave_2.sav"), "a key saved").unwrap();
    let out = quen.tool("game_keys", json!({"token": token, "tokens": ["e"]}));
    let why = out["reason"].as_str().unwrap();
    assert!(
        out["refused"] == false && why.contains("ManualSave_2.sav"),
        "{out}"
    );
    assert_eq!(out["end"]["ok"], true, "{out}");
    assert!(dies(fx.game_pid()));
    fx.assert_restored();
    let after = quen.tool("game_keys", json!({"token": token, "tokens": ["e"]}));
    assert_eq!(after["refused"], true, "{after}");
}

/// A test session hands the controls to the user and takes them back: while the user has them, input is refused and
/// their save is theirs, not an end; the end lists it as saves.user.
#[test]
fn a_test_session_hands_the_controls_to_the_user_and_back() {
    let fx = Fixture::new();
    let mut quen = Quen::start(&fx);
    let launched = launch(&mut quen);
    let token = launched["token"].clone();
    assert_eq!(launched["state"]["hands"], "agent", "{launched}");
    let given = quen.tool("hands_give", json!({"token": token}));
    assert_eq!(given["ok"], true, "{given}");
    assert_eq!(given["state"]["hands"], "user", "{given}");
    let keys = quen.tool("game_keys", json!({"token": token, "tokens": ["e"]}));
    assert_eq!(keys["refused"], true, "{keys}");
    fs::write(fx.path("docs/gamesaves/ManualSave_user.sav"), "the user's").unwrap();
    let taken = quen.tool("hands_take", json!({"token": token, "user_ok": true}));
    assert_eq!(taken["ok"], true, "{taken}");
    assert_eq!(
        taken["user_saves"],
        json!(["ManualSave_user.sav"]),
        "{taken}"
    );
    let keys = quen.tool("game_keys", json!({"token": token, "tokens": ["e"]}));
    assert_eq!(keys["ok"], true, "{keys}");
    let end = quen.tool("session_end", json!({"token": token}));
    assert_eq!(end["ok"], true, "{end}");
    assert_eq!(
        end["saves"]["user"],
        json!(["ManualSave_user.sav"]),
        "{end}"
    );
    assert_eq!(end["saves"]["unexpected"], json!([]), "{end}");
    fx.assert_restored();
}

/// A play session: the launch changes only general.ini's console (the fixture has steam_appid.txt), sends no key
/// without a save, and its boot row says play. The user has the controls: input, writes and focus moves are refused;
/// hands_take (with the user's OK) gives them to the agent, hands_give gives them back, and the user's save is theirs,
/// not an end. The launching connection closing leaves the game running; the session ends by itself when it exits.
#[test]
fn a_play_session_runs_on_without_the_client_until_the_game_exits() {
    let fx = Fixture::new();
    let mut quen = Quen::start(&fx);
    let planned = quen.tool("session_plan", json!({"mode": "play"}));
    assert_eq!(planned["mode"], "play", "{planned}");
    let changes = planned["changes"].as_array().unwrap();
    assert!(
        changes.len() == 1 && changes[0].as_str().unwrap().contains("general.ini"),
        "{planned}"
    );
    for args in [
        json!({"mode": "play", "park": "ALL"}),
        json!({"mode": "play", "profiler": "nsys:x"}),
        json!({"mode": "observe"}),
    ] {
        let out = quen.tool("session_plan", args.clone());
        assert_eq!(out["refused"], true, "{args}: {out}");
    }
    let launched = launch_with(&mut quen, json!({"mode": "play"}));
    let token = launched["token"].clone();
    assert_eq!(launched["state"]["hands"], "user", "{launched}");
    assert!(fx.calls().is_empty(), "no key: {:?}", fx.calls());
    assert_eq!(fx.settings(), SETTINGS, "the user's settings");
    let general = fx.read("game/bin/config/base/general.ini");
    assert!(general.contains("DBGConsoleOn=true"), "{general}");
    let game = fx.game_pid();
    let rows = fx.ledger("SELECT mode, fg, vsync, save_id FROM boots");
    assert_eq!(
        rows,
        [[json!("play"), Value::Null, Value::Null, Value::Null]]
    );
    for (tool, args) in [
        ("game_keys", json!({"token": token, "tokens": ["e"]})),
        ("focus", json!({"token": token, "to": "game"})),
        (
            "game_cvar",
            json!({"token": token, "name": "X", "set": "1"}),
        ),
        ("hands_give", json!({"token": token})),
    ] {
        let out = quen.tool(tool, args);
        assert_eq!(out["refused"], true, "{tool}: {out}");
    }
    // The user saves; the hand-over sorts it as theirs.
    fs::write(fx.path("docs/gamesaves/ManualSave_user.sav"), "the user's").unwrap();
    let taken = quen.tool("hands_take", json!({"token": token, "user_ok": true}));
    assert_eq!(taken["ok"], true, "{taken}");
    assert_eq!(taken["user_saves"], json!(["ManualSave_user.sav"]));
    assert_eq!(taken["state"]["hands"], "agent");
    let keys = quen.tool("game_keys", json!({"token": token, "tokens": ["e"]}));
    assert_eq!(keys["ok"], true, "{keys}");
    let back = quen.tool("hands_give", json!({"token": token}));
    assert_eq!(back["ok"], true, "{back}");
    assert_eq!(back["state"]["hands"], "user");
    let end = quen.tool("session_end", json!({"token": token}));
    assert_eq!(end["refused"], true, "{end}");
    // The client closes: the game runs on, the session with it.
    drop(quen.stdin.take());
    quen.child.wait().unwrap();
    sleep(Duration::from_secs(2));
    assert!(alive(game), "the game closed with the client");
    assert!(fx.path("data/state/journal.jsonl").exists());
    // The user quits the game: quen ends the session by itself and gives the files back.
    let out = Command::new("taskkill")
        .args(["/F", "/PID", &game.to_string()])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let t = Instant::now();
    while fx.ledger("SELECT outcome FROM boots") != [[json!("ok")]] {
        assert!(t.elapsed() < Duration::from_secs(30), "never ended");
        sleep(Duration::from_millis(100));
    }
    assert!(!fx.path("data/state/journal.jsonl").exists());
    assert_eq!(fx.read("game/bin/config/base/general.ini"), GENERAL);
    assert_eq!(fx.settings(), SETTINGS);
    // The boot row lands before serve.log's line is written whole (CI's Windows runner read it cut off, 2026-10-20).
    let done = |log: &str| {
        log.contains("the game exited") && log.contains(r#""user":["ManualSave_user.sav"]"#)
    };
    let t = Instant::now();
    let log = loop {
        let log = fx.read("data/logs/serve.log");
        if done(&log) || t.elapsed() > Duration::from_secs(10) {
            break log;
        }
        sleep(Duration::from_millis(100));
    };
    assert!(done(&log), "{log}");
}

/// What the boot parked is in the mod set, as parked: it is taken after the parking. Status shows the parked mods,
/// the last push of quen's state log, and each file the journal backed up.
#[test]
fn status_and_the_mod_set_see_the_parked_boot() {
    let fx = Fixture::new();
    let mut quen = Quen::start(&fx);
    let launched = launch_with(&mut quen, json!({"park": "modAlpha,W3Fake.asi"}));
    assert!(
        fx.path("game/quen-parked/Mods/modAlpha/blob0.bundle")
            .exists()
    );
    assert!(fx.path("game/quen-parked/bin/W3Fake.asi").exists());
    let status = quen.tool("session_status", json!({}));
    assert_eq!(
        status["parked"],
        json!(["modAlpha", "W3Fake.asi"]),
        "{status}"
    );
    let state = &status["game_state"];
    assert_eq!(
        (&state["flags"], &state["pushes"], &state["photo"]),
        (&json!(129), &json!(3), &json!(true)),
        "{state}"
    );
    assert_eq!(state["bits"], json!(["valid", "photo"]));
    let files = status["files"].as_array().unwrap();
    let settings = files
        .iter()
        .find(|f| f["file"].as_str().unwrap().ends_with("dx12user.settings"))
        .unwrap();
    assert!(
        settings["backup"].is_string() && settings["same"] == false,
        "{settings}"
    );
    let rows = fx.ledger(
        "SELECT e.kind, e.name, e.parked FROM boots b JOIN mod_set_entries e ON e.mod_set_id = b.mod_set_id
           ORDER BY e.kind, e.name",
    );
    assert_eq!(
        rows,
        [
            [json!("asi"), json!("W3Fake"), json!(1)],
            [json!("asi"), json!("W3Quen"), json!(0)],
            [json!("asi"), json!("WitcherNativeBridge"), json!(0)],
            [json!("mods"), json!("modAlpha"), json!(1)],
            [json!("mods"), json!("modNativeBridge"), json!(0)],
            [json!("mods"), json!("modQuen"), json!(0)]
        ]
    );

    let ended = quen.tool("session_end", json!({"token": launched["token"]}));
    assert_eq!(ended["ok"], true, "{ended}");
    fx.assert_restored();
    assert_eq!(fx.read("game/bin/x64_dx12/W3Fake.asi"), "asi");
    let status = quen.tool("session_status", json!({}));
    assert_eq!(status["files"], json!([]), "{status}");
    assert_eq!(status["parked"], json!([]));
}

/// Finding 23: test files were copied into bin by hand before each boot and recycled after it. Staged, the plan lists
/// each copy and the parking of the user's file it covers; the launch puts the copies in bin, a staged ASI in the
/// boot's mod set in place of the one it parked; the end copies the staged mod's new log into game_logs, then recycles
/// it and the copies and puts the user's files back. Each bad entry, and stage in a play session or through a bin
/// that leads out of the game folder, is refused with nothing changed.
#[test]
fn staged_files_load_with_the_boot_and_go_at_its_end() {
    let fx = Fixture::new();
    let mut quen = Quen::start(&fx);
    let pc = fx.path("pc");
    fs::create_dir_all(pc.join("folder.asi")).unwrap();
    for name in [
        "W3Test.asi",
        "W3Fake.asi",
        "W3Fake.ini",
        "W3Quen.asi",
        "bad name.asi",
        "modMenu.xml",
        fx.2.as_str(),
    ] {
        fs::write(pc.join(name), format!("a test {name}")).unwrap();
    }
    let at = |name: &str| pc.join(name).display().to_string();
    let bin = fx.path("game/bin/x64_dx12");
    let refused = |quen: &mut Quen, bad: &Value, why: &str| {
        for tool in ["session_plan", "session_launch"] {
            let mut args = bad.clone();
            if tool == "session_launch" {
                args["user_ok"] = json!(true);
            }
            let out = quen.tool(tool, args);
            assert_eq!(out["refused"], true, "{bad}: {out}");
            assert!(out["reason"].to_string().contains(why), "{why}: {out}");
            assert_eq!(out["state"]["lock"]["free"], true);
        }
    };
    for (bad, why) in [
        (json!({"stage": at("W3Test.asi")}), "an array"),
        (json!({"stage": ["W3Test.asi"]}), "not an absolute path"),
        (json!({"stage": [at("Missing.asi")]}), "no such file"),
        (json!({"stage": [at("folder.asi")]}), "no such file"),
        (json!({"stage": [at("bad name.asi")]}), "its name must be"),
        (json!({"stage": [at("W3Quen.asi")]}), "never parks"),
        (json!({"stage": [at(&fx.2)]}), "never parks"),
        (
            json!({"stage": [at("W3Test.asi"), at("W3Test.asi")]}),
            "a second file",
        ),
        (
            json!({"stage": [at("W3Test.asi")], "mode": "play"}),
            "stage isn't taken in a play session",
        ),
        // Finding 49: no menu folder, no menu XML staged.
        (
            json!({"stage": [at("modMenu.xml")]}),
            "is missing or leads out of the game folder",
        ),
    ] {
        refused(&mut quen, &bad, why);
    }
    // Finding 49: a mod's menu XML goes into the menu folder, the user's of that name parked; user.settings, where a
    // menu writes its values, comes back at the end with the game's copy kept beside.
    let menu = fx.path(r"game\bin\config\r4game\user_config_matrix\pc");
    fs::create_dir_all(&menu).unwrap();
    fs::write(menu.join("modMenu.xml"), "the user's menu").unwrap();
    let user_settings = "[ModMenu]\r\nOn=true\r\n";
    fs::write(fx.path("docs/user.settings"), user_settings).unwrap();
    assert!(!fx.path("data/state/journal.jsonl").exists());
    assert!(!bin.join("W3Test.asi").exists() && !fx.path("game/quen-parked").exists());

    // Finding 50: debugscripts ends the game's command line with -debugscripts, a condition on the boot row.
    let plan = quen.tool("session_plan", json!({"debugscripts": true}));
    let argv = plan["command"]["argv"].as_array().unwrap();
    assert_eq!(argv.last().unwrap(), "-debugscripts", "{plan}");
    assert_eq!(plan["conditions"], json!(["debugscripts"]), "{plan}");
    let stage = json!([
        at("W3Test.asi"),
        at("W3Fake.asi"),
        at("W3Fake.ini"),
        at("modMenu.xml")
    ]);
    let plan = quen.tool("session_plan", json!({"stage": stage.clone()}));
    let changes: Vec<&str> = plan["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap())
        .collect();
    let has = |start: &str, end: &str| {
        changes
            .iter()
            .any(|c| c.starts_with(start) && c.ends_with(end))
    };
    for (start, end) in [
        ("stage ", r"bin\x64_dx12\W3Test.asi"),
        ("park ", r"quen-parked\bin\W3Fake.asi"),
        ("stage ", r"bin\x64_dx12\W3Fake.asi"),
        ("park ", r"quen-parked\bin\W3Fake.ini"),
        ("stage ", r"bin\x64_dx12\W3Fake.ini"),
        ("park ", r"quen-parked\menu\modMenu.xml"),
        ("stage ", r"user_config_matrix\pc\modMenu.xml"),
        (
            "recycle ",
            r"W3Test.log at the end if the game writes it (game_logs keeps a copy)",
        ),
        (
            "recycle ",
            r"W3Fake.log at the end if the game writes it (game_logs keeps a copy)",
        ),
    ] {
        assert!(has(start, end), "{start}...{end}: {changes:#?}");
    }
    let launched = launch_with(&mut quen, json!({"stage": stage}));
    assert_eq!(fx.read("game/bin/x64_dx12/W3Test.asi"), "a test W3Test.asi");
    assert_eq!(fx.read("game/bin/x64_dx12/W3Fake.ini"), "a test W3Fake.ini");
    assert_eq!(fx.read("game/quen-parked/bin/W3Fake.asi"), "asi");
    assert_eq!(
        fs::read_to_string(menu.join("modMenu.xml")).unwrap(),
        "a test modMenu.xml"
    );
    assert_eq!(
        fx.read("game/quen-parked/menu/modMenu.xml"),
        "the user's menu"
    );
    assert!(launched.get("mod_set_error").is_none(), "{launched}");
    let rows = fx.ledger(
        "SELECT e.name, e.parked, e.content_hash FROM boots b JOIN mod_set_entries e ON e.mod_set_id = b.mod_set_id
          WHERE e.kind = 'asi' ORDER BY e.name",
    );
    let sha = |text: &str| json!(quen_core::journal::sha256_hex(text.as_bytes()));
    assert_eq!(
        rows,
        [
            [json!("W3Fake"), json!(0), sha("a test W3Fake.asi")],
            [json!("W3Quen"), json!(0), sha("synthetic")],
            [json!("W3Test"), json!(0), sha("a test W3Test.asi")],
            [json!("WitcherNativeBridge"), json!(0), sha("synthetic")],
        ],
    );
    // The staged mod writes its log and its menu a value; the end keeps a copy of each, then the folders go back as
    // they were.
    fs::write(bin.join("W3Test.log"), "the staged mod's log").unwrap();
    let menu_set = "[ModMenu]\r\nOn=false\r\n";
    fs::write(fx.path("docs/user.settings"), menu_set).unwrap();
    let ended = quen.tool("session_end", json!({"token": launched["token"]}));
    assert_eq!(ended["ok"], true, "{ended}");
    let session = launched["session"].as_str().unwrap();
    let logs = fs::read_dir(fx.path("data/logs")).unwrap();
    let logs = logs
        .map(|e| e.unwrap().path())
        .find(|p| p.to_string_lossy().ends_with(session));
    let copy = logs.unwrap().join(r"game-logs\W3Test.log");
    assert_eq!(fs::read_to_string(copy).unwrap(), "the staged mod's log");
    let undone = ended["replay"]["undone"].to_string();
    assert!(
        undone.contains("W3Test.log, new since the journal began"),
        "{ended}"
    );
    fx.assert_restored();
    assert_eq!(
        fs::read_to_string(menu.join("modMenu.xml")).unwrap(),
        "the user's menu"
    );
    assert_eq!(fx.read("docs/user.settings"), user_settings);
    assert_eq!(fx.read("docs/user.settings.quen-current"), menu_set);
    for gone in ["W3Test.asi", "W3Test.log", "W3Fake.log"] {
        assert!(!bin.join(gone).exists(), "{gone}");
    }
    assert_eq!(fx.read("game/bin/x64_dx12/W3Fake.asi"), "asi");
    assert_eq!(
        fx.read("game/bin/x64_dx12/W3Fake.ini"),
        "[General]\nMode=raise\n"
    );
    let left = fs::read_dir(&bin).unwrap().map(|e| e.unwrap().file_name());
    assert!(
        !left
            .into_iter()
            .any(|n| n.to_string_lossy().contains(".quen-"))
    );

    // bin through a junction out of the game folder: refused before anything changes.
    fs::rename(fx.path(r"game\bin"), fx.path("elsewhere")).unwrap();
    let out = Command::new("cmd")
        .args(["/c", "mklink", "/J"])
        .arg(fx.path(r"game\bin"))
        .arg(fx.path("elsewhere"))
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let bad = json!({"stage": [at("W3Test.asi")]});
    refused(&mut quen, &bad, "leads out of the game folder");
    assert!(!bin.join("W3Test.asi").exists());
}

/// Finding 61: session_launch again ends the session and launches its spec again, the staged file copied anew from its
/// path (a rebuilt one is taken), then makes the setup calls that succeeded after the last launch. Refused, nothing
/// changed and the session live: no launch to repeat, another argument, a wrong token, a staged path gone.
#[test]
fn again_relaunches_the_last_spec_with_rebuilt_files_and_its_setup() {
    let fx = Fixture::new();
    let mut quen = Quen::start(&fx);
    let again = |token: &Value| json!({"again": true, "user_ok": true, "token": token});
    let none = quen.tool("session_launch", again(&Value::Null));
    assert_eq!(none["refused"], true, "{none}");
    let asi = fx.path("pc/W3Test.asi");
    fs::create_dir_all(asi.parent().unwrap()).unwrap();
    fs::write(&asi, "build 1").unwrap();
    let launched = launch_with(&mut quen, json!({"stage": [asi.display().to_string()]}));
    let token = launched["token"].clone();
    let load = quen.tool("game_load", json!({"token": token, "save": "QuickSave"}));
    assert_eq!(load["ok"], true, "{load}");
    fs::write(&asi, "build 2").unwrap();
    let mut extra = again(&token);
    extra["save"] = json!("QuickSave");
    let moved = fx.path("pc/moved.asi");
    for (args, why) in [
        (extra, "\"save\" with again"),
        (again(&json!("not-it")), "wrong or missing token"),
        (again(&token), "can't be repeated: stage"),
    ] {
        if why.starts_with("can't") {
            fs::rename(&asi, &moved).unwrap();
        }
        let out = quen.tool("session_launch", args);
        assert!(
            out["refused"] == true && out["reason"].as_str().unwrap().contains(why),
            "{why}: {out}"
        );
        assert_eq!(out["state"]["game"]["running"], true, "{out}");
    }
    fs::rename(&moved, &asi).unwrap();
    let relaunched = quen.tool("session_launch", again(&token));
    assert_eq!(relaunched["ok"], true, "{relaunched}");
    assert_eq!(relaunched["ended"]["ok"], true, "{relaunched}");
    assert_ne!(relaunched["token"], token);
    assert_eq!(fx.read("game/bin/x64_dx12/W3Test.asi"), "build 2");
    let setup = &relaunched["setup"];
    assert_eq!(
        (&setup[0]["tool"], &setup[0]["args"], &setup[0]["ok"]),
        (
            &json!("game_load"),
            &json!({"save": "QuickSave"}),
            &json!(true)
        ),
        "{relaunched}"
    );
    let loads = fx.calls();
    let loads = loads.iter().filter(|c| *c == "console quenload(QuickSave)");
    assert_eq!(loads.count(), 2, "{:?}", fx.calls());
    let ended = quen.tool("session_end", json!({"token": relaunched["token"]}));
    assert_eq!(ended["ok"], true, "{ended}");
    fx.assert_restored();
    let boots = fx.ledger("SELECT outcome FROM boots ORDER BY id");
    assert_eq!(boots, [[json!("ok")], [json!("ok")]]);
    // After the end the plan shows what again would do: the same save, the staged file, the setup.
    let plan = quen.tool("session_plan", json!({"again": true}));
    assert_eq!(plan["ok"], true, "{plan}");
    assert_eq!(plan["setup"].as_array().unwrap().len(), 1, "{plan}");
}

/// The daemon killed mid-session leaves the game to die with the job; the next daemon to start takes the lock,
/// replays the journal and gives everything back, a staged file and its mod's log included (the log copied first).
/// Kill points: the boot half done (files changed, the game running, the intros being skipped), and the boot done.
#[test]
fn a_killed_daemon_is_replayed_at_the_next_start() {
    for hold in [true, false] {
        let fx = Fixture::new();
        let mut quen = Quen::start(&fx);
        fs::create_dir_all(fx.path("pc")).unwrap();
        fs::write(fx.path("pc/W3Test.asi"), "a test build").unwrap();
        let args =
            json!({"user_ok": true, "park": "modAlpha", "stage": [fx.path("pc/W3Test.asi")]});
        if hold {
            fx.hold("continue");
            quen.post(
                "tools/call",
                json!({"name": "session_launch", "arguments": args}),
            );
            fx.wait_at("continue");
        } else {
            launch_with(&mut quen, args);
        }
        let staged = fx.path("game/bin/x64_dx12/W3Test.asi");
        assert_eq!(fs::read_to_string(&staged).unwrap(), "a test build");
        fs::write(
            fx.path("game/bin/x64_dx12/W3Test.log"),
            "the staged mod's log",
        )
        .unwrap();
        let game = fx.game_pid();
        let changed = fx.settings();
        assert_ne!(changed, SETTINGS);
        quen.kill_daemon();
        assert!(dies(game), "the game outlived quen's job");
        assert_eq!(
            fx.settings(),
            changed,
            "nothing restores before a quen takes the lock"
        );
        assert!(fx.path("game/quen-parked/Mods/modAlpha").exists());
        assert!(fx.path("data/state/journal.jsonl").exists());

        let mut next = Quen::start(&fx);
        // Calls queue behind the startup replay on the session thread: this one finds nothing left.
        let recovered = next.tool("session_recover", json!({}));
        assert_eq!(recovered["replayed"], false, "{recovered}");
        let status = next.tool("session_status", json!({}));
        assert_eq!(status["startup_replay"]["ok"], true, "{status}");
        fx.assert_restored();
        assert_eq!(status["state"]["lock"]["free"], true);
        // The dead quen's boot is closed as recovered, and its journal is where the row says.
        let rows = fx.ledger("SELECT id, outcome, journal_path FROM boots");
        assert_eq!(rows.len(), 1);
        assert_eq!(
            status["startup_replay"]["ledger"]["boots"],
            json!([rows[0][0]])
        );
        assert_eq!(rows[0][1], "recovered");
        let journal = Path::new(rows[0][2].as_str().unwrap());
        assert!(journal.exists(), "{rows:?}");
        assert!(!staged.exists() && !staged.with_extension("log").exists());
        let copy = journal.with_file_name(r"game-logs\W3Test.log");
        assert_eq!(fs::read_to_string(copy).unwrap(), "the staged mod's log");
    }
}

/// A cancel only sets a flag. The step in flight (here the intros' keys) runs to its end, then the session is
/// unwound, and the cancelled request gets no response.
#[test]
fn a_cancelled_launch_finishes_its_step_then_unwinds() {
    let fx = Fixture::new();
    let mut quen = Quen::start(&fx);
    fx.hold("continue");
    let id = quen.post(
        "tools/call",
        json!({"name": "session_launch", "arguments": {"user_ok": true}}),
    );
    fx.wait_at("continue");
    let game = fx.game_pid();
    quen.send(&json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": id}}));
    fs::write(fx.path("harness/go"), "").unwrap();
    // Queued behind the launch; a response to the cancelled launch would fail `response`.
    let recovered = quen.tool("session_recover", json!({}));
    assert_eq!(recovered["replayed"], false, "{recovered}");
    assert!(
        fx.path("harness/continue_done").exists(),
        "the keys step was cut short"
    );
    assert_eq!(fx.calls(), ["keys e"], "it went on after the cancel");
    assert!(dies(game));
    fx.assert_restored();
}

/// A client with a short tool timeout: `wait: false` answers a job at once, job_status returns the call's own
/// envelope once it ended (then forgets the job), and a call that outlives the client's budget (QUEN_JOB_BUDGET_S,
/// here 1 s: the end waits 2 s for quiet saves) answers a job too.
#[test]
fn a_client_with_a_short_timeout_polls_long_calls_as_jobs() {
    let fx = Fixture::new();
    let mut quen = Quen::spawn_with(&fx, "config.toml", 0, &[("QUEN_JOB_BUDGET_S", "1")]);
    quen.init_as(&fx, "another-client");
    fx.hold("continue");
    let job = quen.call("session_launch", json!({"user_ok": true, "wait": false}));
    assert_eq!(job["tool"], "session_launch", "{job}");
    fx.wait_at("continue");
    let running = quen.call("job_status", json!({"job": job["job"]}));
    assert_eq!(running["state"], "running", "{running}");
    assert_eq!(running["job"], job["job"]);
    let message = running["message"].as_str().unwrap();
    assert!(message.ends_with(" done"), "{running}");
    fs::write(fx.path("harness/go"), "").unwrap();
    let launched = quen.poll(&job);
    assert_eq!(launched["ok"], true, "{launched}");
    assert_eq!(launched["tool"], "session_launch");
    assert_eq!(launched["state"]["lock"], "held here");
    let gone = quen.call("job_status", json!({"job": job["job"]}));
    assert_eq!(gone["refused"], true, "{gone}");
    assert!(
        gone["reason"].as_str().unwrap().contains("no job"),
        "{gone}"
    );

    let end = quen.call("session_end", json!({"token": launched["token"]}));
    assert!(end.get("ok").is_none(), "the end answered at once: {end}");
    let ended = quen.poll(&end);
    assert_eq!(ended["ok"], true, "{ended}");
    assert_eq!(ended["tool"], "session_end");
    fx.assert_restored();
    // A quick call is answered as before.
    let recovered = quen.call("session_recover", json!({}));
    assert_eq!(recovered["replayed"], false, "{recovered}");
}

/// job_cancel is the cancel flag: the step in flight ends, then the launch unwinds, and job_status returns the
/// envelope of that failure. Progress for the call stops once it answered a job: its request is done (MCP).
#[test]
fn a_cancelled_job_unwinds_and_its_progress_stops_at_the_answer() {
    let fx = Fixture::new();
    let mut quen = Quen::spawn_with(&fx, "config.toml", 0, &[("QUEN_JOB_BUDGET_S", "1")]);
    quen.init_as(&fx, "another-client");
    fx.hold("continue");
    let params = json!({"name": "session_launch", "arguments": {"user_ok": true},
                        "_meta": {"progressToken": "p1"}});
    let id = quen.post("tools/call", params);
    let job = result(&quen.response(id));
    assert_eq!(job["state"], "running", "{job}");
    let progress = |notes: &[Value]| {
        let p1 = notes
            .iter()
            .filter(|n| n["params"]["progressToken"] == "p1");
        p1.count()
    };
    assert!(progress(&quen.notes) > 0, "no progress before the answer");
    let answered = quen.notes.len();
    fx.wait_at("continue");
    let cancelling = quen.call("job_cancel", json!({"job": job["job"]}));
    assert_eq!(cancelling["cancelling"], true, "{cancelling}");
    fs::write(fx.path("harness/go"), "").unwrap();
    let failed = quen.poll(&cancelling);
    assert_eq!(failed["ok"], false, "{failed}");
    assert_eq!(failed["tool"], "session_launch");
    assert_eq!(fx.calls(), ["keys e"], "it went on after the cancel");
    fx.assert_restored();
    assert_eq!(
        progress(&quen.notes[answered..]),
        0,
        "progress after the answer"
    );
}

/// A game the user started (one of the configured image name, outside quen's job) blocks the launch, and blocks every
/// replay while it runs: it holds the files a replay restores. The journal stays until it is gone.
#[test]
fn a_game_outside_the_job_blocks_launch_and_replay() {
    let fx = Fixture::new();
    let mut quen = Quen::start(&fx);
    let mut outside = outside_game(&fx);
    let refused = quen.tool("session_launch", json!({"user_ok": true}));
    let why = refused["reason"].as_str().unwrap();
    assert!(
        refused["refused"] == true && why.contains("close the game first"),
        "{refused}"
    );
    assert_eq!(refused["state"]["lock"]["free"], true);
    outside.kill().unwrap();
    outside.wait().unwrap();

    // One starts during the session: the end kills quen's game, but its replay waits.
    let launched = launch(&mut quen);
    let mut outside = outside_game(&fx);
    let ended = quen.tool("session_end", json!({"token": launched["token"]}));
    let why = ended["reason"].as_str().unwrap();
    assert!(
        ended["ok"] == false && why.contains("outside quen's job"),
        "{ended}"
    );
    assert!(dies(fx.game_pid()), "the job's game outlived the end");
    assert!(fx.path("data/state/journal.jsonl").exists());
    assert_ne!(fx.settings(), SETTINGS);
    outside.kill().unwrap();
    outside.wait().unwrap();
    let recovered = quen.tool("session_recover", json!({}));
    assert_eq!(recovered["ok"], true, "{recovered}");
    fx.assert_restored();

    // A killed quen's journal: the next quen's startup replay waits for the outside game too.
    launch(&mut quen);
    quen.kill_daemon();
    let mut outside = outside_game(&fx);
    let mut next = Quen::start(&fx);
    let blocked = next.tool("session_recover", json!({}));
    assert_eq!(blocked["refused"], false, "{blocked}");
    let status = next.tool("session_status", json!({}));
    let startup = &status["startup_replay"];
    assert!(
        startup["reason"]
            .as_str()
            .unwrap()
            .contains("outside quen's job"),
        "{startup}"
    );
    assert!(fx.path("data/state/journal.jsonl").exists());
    outside.kill().unwrap();
    outside.wait().unwrap();
    let recovered = next.tool("session_recover", json!({}));
    assert_eq!(recovered["ok"], true, "{recovered}");
    fx.assert_restored();
}

/// Live boot 1: an end whose replay was refused (the game still exiting) left the journal, and the daemon took its idle
/// exit with the mods still parked. Such an end now leaves its boot open with why; before the daemon exits it waits for
/// the game outside its job to go, then replays, and the recovery closes the boot as recovered, the journal where the
/// row says.
#[test]
fn an_end_that_left_its_journal_is_replayed_before_the_daemon_exits() {
    let fx = Fixture::new();
    let mut quen = Quen::start(&fx);
    let launched = launch_with(&mut quen, json!({"park": "modAlpha"}));
    let mut outside = outside_game(&fx);
    let ended = quen.tool("session_end", json!({"token": launched["token"]}));
    let why = ended["reason"].as_str().unwrap();
    assert!(
        ended["ok"] == false && why.contains("outside quen's job"),
        "{ended}"
    );
    let open = fx.ledger("SELECT id, t_end, outcome, failure FROM boots");
    assert_eq!(
        open[0][..3],
        [
            launched["ledger"]["boots"][0].clone(),
            Value::Null,
            Value::Null
        ]
    );
    assert!(
        open[0][3].as_str().unwrap().contains("outside quen's job"),
        "{open:?}"
    );
    // The client goes. The idle daemon waits for the outside game, replays, then exits.
    let daemon = quen.daemon;
    drop(quen.stdin.take());
    quen.wait_exit();
    let log = fx.path("data/logs/serve.log");
    let t = Instant::now();
    while !fs::read_to_string(&log)
        .unwrap_or_default()
        .contains("before exit, replaying")
    {
        assert!(
            t.elapsed() < Duration::from_secs(30),
            "no retry in serve.log"
        );
        sleep(Duration::from_millis(100));
    }
    assert!(alive(daemon), "the daemon exited with the journal left");
    outside.kill().unwrap();
    outside.wait().unwrap();
    assert!(dies_within(daemon, Duration::from_secs(30)));
    fx.assert_restored();
    let rows = fx.ledger("SELECT outcome, failure, journal_path FROM boots");
    assert_eq!(rows[0][0], "recovered", "{rows:?}");
    assert!(
        rows[0][1].as_str().unwrap().contains("outside quen's job"),
        "{rows:?}"
    );
    assert!(Path::new(rows[0][2].as_str().unwrap()).exists(), "{rows:?}");
}

/// A replay undoes only what still is as quen left it. A mod reinstalled while its old copy was parked is left
/// alone, both copies kept: the replay reports the step, the journal stays (so no launch), and once the user has
/// sorted it out a recovery finishes.
#[test]
fn a_replay_that_leaves_a_step_keeps_the_journal() {
    let fx = Fixture::new();
    let mut quen = Quen::start(&fx);
    let launched = launch_with(&mut quen, json!({"park": "modAlpha"}));
    fs::create_dir_all(fx.path("game/Mods/modAlpha")).unwrap();
    fs::write(fx.path("game/Mods/modAlpha/new.bundle"), "reinstalled").unwrap();
    let ended = quen.tool("session_end", json!({"token": launched["token"]}));
    let why = ended["reason"].as_str().unwrap();
    assert!(
        ended["ok"] == false && why.contains("replay left"),
        "{ended}"
    );
    let left = &ended["replay"]["left"][0]["why"];
    assert!(left.as_str().unwrap().contains("taken again"), "{ended}");
    assert_eq!(fx.settings(), SETTINGS, "the other steps were undone");
    assert!(
        fx.path("game/quen-parked/Mods/modAlpha/blob0.bundle")
            .exists()
    );
    assert!(fx.path("data/state/journal.jsonl").exists());
    let blocked = quen.tool("session_launch", json!({"user_ok": true}));
    assert_eq!(blocked["refused"], true, "{blocked}");
    fs::remove_dir_all(fx.path("game/Mods/modAlpha")).unwrap(); // the test's own synthetic reinstall
    let recovered = quen.tool("session_recover", json!({}));
    assert_eq!(recovered["ok"], true, "{recovered}");
    fx.assert_restored();
}

/// steam_appid.txt keeps Steam from relaunching the game outside quen's job: a launch places it when it is
/// missing, and the end takes it away again (to the Recycle Bin).
#[test]
fn a_missing_steam_appid_is_placed_for_the_session() {
    let fx = Fixture::new();
    let appid = fx.path("game/bin/x64_dx12/steam_appid.txt");
    fs::remove_file(&appid).unwrap(); // the fixture's own synthetic file
    let mut quen = Quen::start(&fx);
    let launched = launch(&mut quen);
    assert_eq!(fs::read_to_string(&appid).unwrap(), "292030");
    let ended = quen.tool("session_end", json!({"token": launched["token"]}));
    assert_eq!(ended["ok"], true, "{ended}");
    assert!(!appid.exists());
    fx.assert_restored();
}

/// The harness's launch check, ported: a dialog of the game (a script compile error) ends the launch before any key.
#[test]
fn a_dialog_of_the_game_ends_the_launch() {
    let fx = Fixture::new();
    fs::write(fx.path("game/bin/x64_dx12/dialog"), "").unwrap();
    let mut quen = Quen::start(&fx);
    let launch = quen.tool("session_launch", json!({"user_ok": true}));
    let why = launch["reason"].as_str().unwrap();
    assert!(
        launch["refused"] == false && why.contains("Script Compilation Errors"),
        "{launch}"
    );
    assert!(fx.calls().is_empty(), "a key was sent: {:?}", fx.calls());
    fx.assert_restored();
}

/// A game exe that can't start: the boot's file changes are undone, the lock is free, and its row says why.
#[test]
fn a_game_that_cannot_start_is_unwound() {
    let fx = Fixture::new();
    // A PE header and nothing more: the build gate reads it (an unknown build, allowed here); Windows can't start it.
    let mut stand_in = vec![0u8; 0x98];
    stand_in[..2].copy_from_slice(b"MZ");
    stand_in[0x3C..0x40].copy_from_slice(&0x80u32.to_le_bytes());
    stand_in[0x80..0x84].copy_from_slice(b"PE  ");
    fs::write(fx.path("game/bin/x64_dx12").join(&fx.2), stand_in).unwrap();
    let mut quen = Quen::start(&fx);
    let launch = quen.tool(
        "session_launch",
        json!({"user_ok": true, "allow_unknown_build": true}),
    );
    assert!(
        launch["refused"] == false && launch["reason"].as_str().unwrap().contains("did not start"),
        "{launch}"
    );
    assert_eq!(launch["state"]["lock"]["free"], true);
    fx.assert_restored();
    let rows = fx.ledger("SELECT outcome, failure FROM boots");
    assert_eq!(rows[0][0], "failed");
    assert!(rows[0][1].as_str().unwrap().contains("did not start"));
}

/// A launch needs the game folder and the documents folder, absolute and there (without them the saves guard or the
/// parking would quietly be off), and every optional feature it asks for configured.
#[test]
fn a_launch_needs_the_configured_paths_and_features() {
    let fx = Fixture::new();
    let config = fx.read("config.toml");
    let path = |rel: &str| fx.path(rel).display().to_string();
    for (from, to, why) in [
        (path("game"), "game".to_owned(), "not an absolute path"),
        (path("docs"), "docs".to_owned(), "not an absolute path"),
        (path("game"), path("no-such-game"), "an existing folder"),
        (path("docs"), path("no-such-docs"), "an existing folder"),
    ] {
        fs::write(fx.path("config.toml"), config.replace(&from, &to)).unwrap();
        let mut quen = Quen::start(&fx);
        let refused = quen.tool("session_plan", json!({}));
        let reason = refused["reason"].as_str().unwrap_or_default();
        assert!(
            refused["refused"] == true && reason.contains(why),
            "{to}: {refused}"
        );
    }
    fs::write(fx.path("config.toml"), config).unwrap();
    let mut quen = Quen::start(&fx);
    for (args, why) in [
        (json!({"preset": "Nits=1"}), "renodx_addon"),
        (json!({"devkit": true}), "devkit in the local config"),
        (json!({"profiler": "nsys:run"}), "nsys in the local config"),
        (json!({"profiler": "ngfx:run"}), "ngfx in the local config"),
    ] {
        let refused = quen.tool("session_plan", args.clone());
        let reason = refused["reason"].as_str().unwrap_or_default();
        assert!(
            refused["refused"] == true && reason.contains(why),
            "{args}: {refused}"
        );
    }
}

/// The launch profiles (profile.rs), as dry runs through the real daemon. renderdoc swaps only NVIDIA's signed
/// interposer: unpinned bytes are refused, and pinned bytes without a signature fail the real Authenticode check. nsys
/// and ngfx park ReShade by themselves and refuse the devkit or a kept ReShade; ngfx's run gets its own output folder,
/// which the plan lists. Each plan carries the run's conditions; nothing changes.
#[test]
fn launch_profiles_check_park_and_flag_before_anything_changes() {
    let fx = Fixture::new();
    let tool = |rel: &str| {
        let path = fx.path(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "tool").unwrap();
        path.display().to_string()
    };
    let bin = fx.path("game/bin/x64_dx12");
    fs::write(bin.join("dxgi.dll"), "reshade").unwrap();
    fs::write(fx.path("game").join(INTERPOSER), "not NVIDIA's").unwrap();
    fx.config(&format!(
        "renderdoc = '{}'\nstreamline_dev_interposer = '{}'\nnsys = '{}'\nngfx = '{}'\ndevkit = 'devkit\\x.addon64'\n",
        tool(r"tools\rd\renderdoccmd.exe"),
        tool(r"tools\sl\sl.interposer.dll"),
        tool(r"tools\nsys\nsys.exe"),
        tool(r"tools\ngfx\ngfx.exe"),
    ));
    let mut quen = Quen::start(&fx);
    let dry = |quen: &mut Quen, args: Value| quen.tool("session_plan", args);
    let reason = |e: &Value| e["reason"].as_str().unwrap_or_default().to_owned();
    let refused = dry(&mut quen, json!({"profiler": "renderdoc:f1"}));
    assert!(
        reason(&refused).contains("not NVIDIA's signed original"),
        "{refused}"
    );
    fs::write(fx.path("game").join(INTERPOSER), "synthetic").unwrap();
    let refused = dry(&mut quen, json!({"profiler": "renderdoc:f1"}));
    assert!(
        reason(&refused).contains("Authenticode signature doesn't verify"),
        "{refused}"
    );

    let nsys = dry(&mut quen, json!({"profiler": "nsys:run"}));
    assert_eq!(nsys["ok"], true, "{nsys}");
    assert_eq!(nsys["conditions"], json!(["nsys", "reshade_parked"]));
    assert_eq!(
        nsys["devkit"], false,
        "ReShade parked takes the devkit with it"
    );
    let changes = nsys["changes"].to_string();
    assert!(changes.contains("dxgi.dll"), "{changes}");
    let staged = tool(r"pc\dxgi.dll");
    for profiler in ["nsys:run", "ngfx:trace"] {
        for (args, why) in [
            (
                json!({"profiler": profiler, "devkit": true}),
                "devkit: false",
            ),
            (
                json!({"profiler": profiler, "park": "ALL", "keep": "dxgi.dll"}),
                "leave it out of keep",
            ),
            (
                json!({"profiler": profiler, "stage": [&staged]}),
                "leave it out of stage",
            ),
        ] {
            let refused = dry(&mut quen, args.clone());
            assert!(reason(&refused).contains(why), "{args}: {refused}");
        }
    }

    // Feature tests 2: under ngfx with ReShade loaded, the game exited 4 s into the launch.
    let ngfx = dry(&mut quen, json!({"profiler": "ngfx:trace"}));
    assert_eq!(ngfx["ok"], true, "{ngfx}");
    assert_eq!(
        ngfx["conditions"],
        json!(["ngfx", "reshade_parked", "clocks_locked_to_base"])
    );
    let changes = ngfx["changes"].to_string();
    assert!(
        changes.contains("dxgi.dll")
            && changes.contains(r"create ")
            && changes.contains(r"<boot>\\trace"),
        "{changes}"
    );
    let argv = ngfx["command"]["argv"].to_string();
    assert!(
        argv.contains(r"<boot>\\trace") && !argv.contains("--platform"),
        "{argv}"
    );
    assert_eq!(fx.settings(), SETTINGS, "a dry run changed the settings");
    assert!(bin.join("dxgi.dll").exists() && !fx.path("data/state/journal.jsonl").exists());
}

/// A tracy launch end to end (profile.rs), with a stand-in for Tracy's capture tool and the fake game answering the
/// trigger as W3Quen.asi does. The doctor checks the tool's version; the plan parks the mod the KB's hooks.tsv names at a
/// function W3Quen hooks and lists the tool's command; the launch gives the game the trigger and the port and starts the
/// tool in the job first; the boot waits until a capture connected; the end lets the game go first, so the tool writes
/// its capture, records it, and puts the parked mod back.
#[test]
fn a_tracy_launch_captures_from_the_launch_to_the_end() {
    let fx = Fixture::new();
    let tool = fx.path(r"tools\tracy\tracy-capture.exe");
    fs::create_dir_all(tool.parent().unwrap()).unwrap();
    fs::copy(fake_tracy_capture(), &tool).unwrap();
    let kb = fx.path("kb");
    fs::create_dir_all(&kb).unwrap();
    let hooks = "site\tkind\tmod\tversion\tnote\n0x1C224A0\thook\tW3Fake\t1.0\tsynthetic\n";
    fs::write(kb.join("hooks.tsv"), hooks).unwrap();
    fx.config(&format!(
        "tracy_capture = '{}'\nkb = '{}'\n",
        tool.display(),
        kb.display()
    ));
    let mut quen = Quen::start(&fx);
    let doctor = quen.tool("quen_doctor", json!({}));
    let deps = doctor["dependencies"].as_array().unwrap();
    let tracy = deps
        .iter()
        .find(|d| d["name"].as_str().unwrap().starts_with("Tracy's"))
        .unwrap();
    assert_eq!(
        (&tracy["status"], &tracy["version"]),
        (&json!("found"), &json!("0.14.1")),
        "{tracy}"
    );

    let plan = quen.tool("session_plan", json!({"profiler": "tracy:run"}));
    assert_eq!(plan["ok"], true, "{plan}");
    assert_eq!(plan["conditions"], json!(["tracy"]));
    let changes = plan["changes"].to_string();
    assert!(
        changes.contains("W3Fake.asi") && !changes.contains("dxgi"),
        "{changes}"
    );
    let profile = &plan["settings_diff"]["profile"];
    assert_eq!(profile["park"], json!(["W3Fake.asi"]), "{profile}");
    assert_eq!(profile["collisions"][0]["hook"], "RT_UpdateRayTracingData");
    let capture = plan["capture_tool"]["argv"].as_array().unwrap();
    assert!(
        capture[1..5]
            == [
                json!("-a"),
                json!("127.0.0.1"),
                json!("-p"),
                profile["port"].to_string().into()
            ]
            && capture[6].as_str().unwrap().ends_with(r"<boot>\run.tracy"),
        "{capture:?}"
    );
    let keep = quen.tool(
        "session_plan",
        json!({"profiler": "tracy:run", "park": "ALL", "keep": "W3Fake.asi"}),
    );
    let why = keep["reason"].as_str().unwrap_or_default();
    assert!(
        why.contains("parks W3Fake.asi: leave it out of keep"),
        "{keep}"
    );
    assert!(
        fx.path("game/bin/x64_dx12/W3Fake.asi").exists(),
        "a plan parks nothing"
    );

    let launched = launch_with(&mut quen, json!({"profiler": "tracy:run"}));
    let mut tracy = launched["tracy"].clone();
    let started = tracy.as_object_mut().and_then(|t| t.remove("started_utc"));
    assert_eq!(
        tracy,
        json!({"hooks_installed": "75 of 75", "skipped": [], "capture": "connected"}),
        "{launched}"
    );
    // W3Quen.log's `tracy: started` line as UTC, to the ms (feature tests 4).
    let started = started
        .as_ref()
        .and_then(|t| t.as_str())
        .unwrap_or_default();
    assert!(started.len() == 24 && started.ends_with('Z'), "{launched}");
    assert!(launched["timing_s"]["tracy"].is_number());
    assert!(
        !fx.path("game/bin/x64_dx12/W3Fake.asi").exists(),
        "parked for the boot"
    );
    let boot = launched["ledger"]["boots"][0].as_i64().unwrap();
    let ended = quen.tool("session_end", json!({"token": launched["token"]}));
    assert_eq!(ended["ok"], true, "{ended}");
    let file = ended["tracy"]["capture"].as_str().unwrap();
    assert!(file.ends_with(&format!(r"{boot}\run.tracy")), "{ended}");
    assert_eq!(
        fs::read_to_string(file.replace("%USERPROFILE%", &std::env::var("USERPROFILE").unwrap()))
            .unwrap(),
        "synthetic tracy capture, 5 bytes from the game\n",
        "the tool wrote it once the game went"
    );
    assert!(ended["ledger"]["captures"][0].is_i64(), "{ended}");
    let rows = fx.ledger("SELECT kind, method, path, boot_id, bytes > 0 FROM captures");
    assert_eq!(
        rows,
        [[
            json!("tracy"),
            json!("tracy"),
            json!(format!("{boot}/run.tracy")),
            json!(boot),
            json!(1)
        ]]
    );
    let boots = fx.ledger("SELECT profiler, settings_diff ->> '$.conditions' FROM boots");
    assert_eq!(boots, [[json!("tracy:run"), json!(r#"["tracy"]"#)]]);
    assert!(
        fx.path("game/bin/x64_dx12/W3Fake.asi").exists(),
        "back after the end"
    );
    fx.assert_restored();
}

/// A launch checks what it needs before anything changes, a dry run too: the Native Bridge, and quen's package
/// exactly (its state is the readiness), whatever the save. Refused with every missing part in `needs`; else
/// the game's build is reported.
#[test]
fn a_launch_needs_the_native_bridge_and_quen_s_package_whole() {
    let fx = Fixture::new();
    let mut quen = Quen::start(&fx);
    let planned = quen.tool("session_plan", json!({}));
    assert_eq!(planned["ok"], true, "{planned}");
    let build = &planned["game_build"];
    assert!(
        build["sha256"].as_str().is_some_and(|s| s.len() == 64),
        "{planned}"
    );
    assert_eq!(
        (&build["status"], &build["label"]),
        (&json!("known"), &json!("synthetic (the fake game)")),
        "the fixture's config knows the fake game"
    );
    off_in(&fx, PARTS[0]); // WitcherNativeBridge.asi
    let refused = quen.tool("session_plan", json!({}));
    let needs: Vec<&str> = refused["needs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["dependency"].as_str().unwrap())
        .collect();
    assert_eq!(needs, ["Witcher Native Bridge 2.0"], "{refused}");
    assert_eq!(refused["refused"], true);
    // A broken package fails every boot's script compile: a launch of the newest save is refused too, unchanged.
    let fx = Fixture::new();
    let mut quen = Quen::start(&fx);
    let mut broken = |why: &str| {
        let out = quen.tool("session_launch", json!({"user_ok": true}));
        let need = &out["needs"][0];
        assert_eq!(need["dependency"], "quen in-game package", "{out}");
        assert_eq!(need["status"], "wrong_version", "{out}");
        assert!(out["reason"].as_str().unwrap().contains(why), "{out}");
        fx.assert_restored();
        assert!(!fx.path("data/gamesaves").exists(), "saves copied");
    };
    fs::create_dir_all(fx.path(r"game\Mods\modQuenRpc")).unwrap();
    broken(r"Mods\modQuenRpc is left from quen's old layout");
    off_in(&fx, r"Mods\modQuenRpc");
    fs::write(fx.path("game").join(PARTS[4]), "another release").unwrap();
    broken(
        "quen.ws is not the version quen expects (quen-w3 0.1.14, quen-w3 0.1.13, quen-w3 0.1.12, quen-w3 0.1.11, quen-w3 0.1.10, quen-w3 0.1.9, quen-w3 0.1.8, quen-w3 0.1.7, quen-w3 0.1.6, quen-w3 0.1.5, quen-w3 0.1.4, quen-w3 0.1.3, quen-w3 0.1.2, quen-w3 0.1.1, test): unzip one release package",
    );
    off_in(&fx, PARTS[4]); // quen.ws
    broken(r"W3Quen.asi without Mods\modQuen, half the package");
    // Without the package: refused, the newest save too (its state is how a launch sees gameplay).
    off_in(&fx, PARTS[3]); // W3Quen.asi
    let newest = quen.tool("session_plan", json!({}));
    let need = &newest["needs"][0];
    assert_eq!(need["dependency"], "quen in-game package", "{newest}");
    assert_eq!(need["status"], "missing", "{newest}");
}

/// A Native Bridge that didn't register quen's script functions (a game update broke it) ends the launch before
/// any key, everything given back, and the boot's row says why.
#[test]
fn a_launch_whose_natives_failed_ends_before_any_key() {
    let fx = Fixture::new();
    fs::write(fx.path("game/bin/x64_dx12/natives_failed"), "").unwrap();
    let mut quen = Quen::start(&fx);
    let launch = quen.tool("session_launch", json!({"user_ok": true}));
    let why = launch["reason"].as_str().unwrap();
    assert!(
        launch["refused"] == false && why.contains("QUEN_State=FAILED"),
        "{launch}"
    );
    assert!(fx.calls().is_empty(), "a key was sent: {:?}", fx.calls());
    fx.assert_restored();
    let rows = fx.ledger("SELECT outcome, failure FROM boots");
    assert_eq!(rows[0][0], "failed");
}

/// quen works the same with the W3 Game State Bridge installed (for RenoDX) and without it: it reads
/// nothing the bridge writes (here its log says "loading" at every push quen's state makes: see FAKE_SCRIPT), needs it
/// for no tool, and never parks either half of it, even under ALL.
#[test]
fn quen_works_the_same_with_or_without_the_w3_game_state_bridge() {
    let run = |bridge: bool| {
        let fx = Fixture::new();
        let manual = fx.path("docs/gamesaves/ManualSave_old.sav");
        fs::write(&manual, "manual").unwrap();
        let long_ago = std::time::UNIX_EPOCH + Duration::from_secs(1_600_000_000);
        let file = fs::File::options().write(true).open(&manual).unwrap();
        file.set_modified(long_ago).unwrap();
        if bridge {
            for part in W3GSB {
                let path = fx.path("game").join(part);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, "the bridge, another build").unwrap();
            }
        }
        let mut quen = Quen::start(&fx);
        let doctor = quen.tool("quen_doctor", json!({}));
        let names: Vec<&str> = doctor["dependencies"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["name"].as_str().unwrap())
            .collect();
        assert!(!names.iter().any(|n| n.contains("Game State")), "{names:?}");
        let launched = launch_with(&mut quen, json!({"save": "ManualSave_old", "park": "ALL"}));
        let token = launched["token"].clone();
        for part in W3GSB.iter().filter(|_| bridge) {
            assert!(fx.path("game").join(part).exists(), "{part} was parked");
        }
        let status = quen.tool("session_status", json!({}));
        let keys = quen.tool(
            "game_keys",
            json!({"token": token, "tokens": ["escape"], "focus": true}),
        );
        let console = quen.tool(
            "game_console",
            json!({"token": token, "line": "settime(1,22)"}),
        );
        let load = quen.tool("game_load", json!({"token": token, "save": "QuickSave"}));
        let ended = quen.tool("session_end", json!({"token": token}));
        assert_eq!(ended["ok"], true, "{ended}");
        fx.assert_restored();
        for part in W3GSB.iter().filter(|_| bridge) {
            assert_eq!(
                fs::read_to_string(fx.path("game").join(part)).unwrap(),
                "the bridge, another build"
            );
        }
        json!({
            "launch": [launched["ok"].clone(), launched["continued"].clone(), launched["load"]["flags_after"].clone()],
            "state": status["game_state"]["flags"],
            "keys": keys["ok"],
            "console": console["ok"],
            "load": [load["ok"].clone(), load["flags_after"].clone()],
            "calls": fx.calls(),
        })
    };
    let without = run(false);
    assert_eq!(without["launch"][0], true, "{without}");
    assert_eq!(without["load"][0], true, "{without}");
    assert_eq!(run(true), without);
}

/// The build gate: an exe whose sha256 isn't a known build (the fake game with bytes added: an update) is
/// refused before anything changes, session_plan too, naming its sha256 and PE stamp, and the doctor says the same;
/// allow_unknown_build launches it, and the boot row records the build and the condition.
#[test]
fn an_unknown_build_is_refused_unless_allowed() {
    let fx = Fixture::new();
    let exe = fx.path("game/bin/x64_dx12").join(&fx.2);
    let mut file = fs::OpenOptions::new().append(true).open(&exe).unwrap();
    file.write_all(b"a synthetic update").unwrap();
    drop(file);
    let sha = quen_core::journal::sha256_hex(&fs::read(&exe).unwrap());
    let mut quen = Quen::start(&fx);
    for (tool, args) in [
        ("session_plan", json!({})),
        ("session_launch", json!({"user_ok": true})),
    ] {
        let out = quen.tool(tool, args);
        assert_eq!(out["refused"], true, "{out}");
        let why = out["reason"].as_str().unwrap();
        assert!(
            why.contains(&sha)
                && why.contains("(PE stamp 0x")
                && why.contains("allow_unknown_build"),
            "{why}"
        );
        assert_eq!(out["state"]["lock"]["free"], true);
    }
    fx.assert_restored();
    assert!(!fx.path("data/gamesaves").exists(), "saves copied");
    let doctor = quen.tool("quen_doctor", json!({}));
    let build = &doctor["game_build"];
    assert_eq!(
        (&build["status"], &build["sha256"]),
        (&json!("unknown"), &json!(sha)),
        "{doctor}"
    );

    let launched = launch_with(&mut quen, json!({"allow_unknown_build": true}));
    assert_eq!(launched["game_build"]["status"], "allowed_unknown");
    let ended = quen.tool("session_end", json!({"token": launched["token"]}));
    assert_eq!(ended["ok"], true, "{ended}");
    fx.assert_restored();
    let rows = fx.ledger(
        "SELECT json_extract(b.settings_diff, '$.build.sha256'), json_extract(b.settings_diff, '$.build.status'),
                json_extract(b.settings_diff, '$.conditions'), g.exe_sha256, g.build_stamp
           FROM boots b JOIN games g ON g.id = b.game_id",
    );
    let row = &rows[0];
    assert_eq!(
        row[..4],
        [
            json!(sha),
            json!("allowed_unknown"),
            json!(r#"["unknown_build"]"#),
            json!(sha)
        ]
    );
    assert_eq!(row[4], launched["game_build"]["stamp"]);
}

/// The W3 Game State Bridge is parked both halves or neither. One half alone is refused before anything changes;
/// both named are parked for the session and given back at its end.
#[test]
fn the_w3_game_state_bridge_is_parked_only_as_a_pair() {
    let fx = Fixture::new();
    for part in W3GSB {
        let path = fx.path("game").join(part);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "the bridge").unwrap();
    }
    let mut quen = Quen::start(&fx);
    for one in ["W3GameStateBridge.asi", "modW3GameStateBridge"] {
        let out = quen.tool("session_launch", json!({"user_ok": true, "park": one}));
        assert_eq!(out["refused"], true, "{out}");
        assert!(
            out["reason"]
                .as_str()
                .unwrap()
                .contains("is parked only together with"),
            "{out}"
        );
    }
    fx.assert_restored();
    let launched = launch_with(
        &mut quen,
        json!({"park": "W3GameStateBridge.asi,modW3GameStateBridge"}),
    );
    for part in W3GSB {
        assert!(!fx.path("game").join(part).exists(), "{part} is in place");
    }
    let status = quen.tool("session_status", json!({}));
    assert_eq!(
        status["parked"],
        json!(["modW3GameStateBridge", "W3GameStateBridge.asi"]),
        "{status}"
    );
    let ended = quen.tool("session_end", json!({"token": launched["token"]}));
    assert_eq!(ended["ok"], true, "{ended}");
    fx.assert_restored();
    for part in W3GSB {
        assert_eq!(fx.read(&format!("game/{part}")), "the bridge");
    }
}

/// Takes one of the game's parts away, as if never installed.
fn off_in(fx: &Fixture, rel: &str) {
    let path = fx.path("game").join(rel);
    fs::rename(&path, path.with_extension("off")).unwrap();
}

/// With RenoDX configured: ReShade.ini gets the devkit and DevAutoReload and preset 1's values, ReShadePreset.ini its
/// techniques emptied, the launch waits for the add-on's line in ReShade.log, and an add-on that isn't the build is
/// refused unless W3_ADDON_ANY. The end gives both files back.
#[test]
fn renodx_patches_reshade_and_waits_for_its_log() {
    let fx = Fixture::new();
    let bin = fx.path("game/bin/x64_dx12");
    let ini = "[ADDON]\r\nLoadFromDllMain=\r\n[renodx-thewitcher3remastered]\r\nSettingsMode=1\r\n\
               [renodx-thewitcher3remastered-preset1]\r\nToneMapGameNits=203\r\n";
    let preset = "Techniques=SyntheticHdr\r\n";
    fs::write(bin.join("ReShade.ini"), ini).unwrap();
    fs::write(bin.join("ReShadePreset.ini"), preset).unwrap();
    fs::write(bin.join("dxgi.dll"), "reshade").unwrap();
    fs::write(bin.join("renodx-thewitcher3remastered.addon64"), "addon").unwrap();
    fs::write(fx.path("build.addon64"), "a newer build").unwrap();
    fx.config(&format!(
        "renodx_addon = 'renodx-thewitcher3remastered.addon64'\nrenodx_build = '{}'\ndevkit = 'devkit\\renodx-devkit.addon64'\n",
        fx.path("build.addon64").display()
    ));
    let mut quen = Quen::start(&fx);
    let refused = quen.tool("session_launch", json!({"user_ok": true}));
    assert!(
        refused["refused"] == true
            && refused["reason"]
                .as_str()
                .unwrap()
                .contains("differs from the build"),
        "{refused}"
    );
    let launched = launch_with(
        &mut quen,
        json!({"devkit": false, "preset": "ToneMapGameNits=250", "extra_env": {"W3_ADDON_ANY": "1"}}),
    );
    assert!(launched["timing_s"]["reshade"].is_number(), "{launched}");
    let patched = fs::read_to_string(bin.join("ReShade.ini")).unwrap();
    let want = ini
        .replace(
            "SettingsMode=1\r\n",
            "SettingsMode=1\r\nDevAutoReload=1\r\n",
        )
        .replace("ToneMapGameNits=203", "ToneMapGameNits=250");
    assert_eq!(patched, want);
    assert_eq!(
        fs::read_to_string(bin.join("ReShadePreset.ini")).unwrap(),
        "Techniques=\r\n"
    );
    let ended = quen.tool("session_end", json!({"token": launched["token"]}));
    assert_eq!(ended["ok"], true, "{ended}");
    assert_eq!(fs::read_to_string(bin.join("ReShade.ini")).unwrap(), ini);
    assert_eq!(
        fs::read_to_string(bin.join("ReShadePreset.ini")).unwrap(),
        preset
    );
    fx.assert_restored();
    let rows = fx.ledger(
        "SELECT devkit, json_extract(settings_diff, '$.preset.ToneMapGameNits') FROM boots",
    );
    assert_eq!(rows, [[json!(0), json!(250)]]);
}

/// After a crash or a power loss nothing replays a left journal until a client starts quen: `quen recover`
/// does it from a terminal, as session_recover would, and prints the envelope.
#[test]
fn quen_recover_replays_a_left_journal_from_a_terminal() {
    let fx = Fixture::new();
    fs::create_dir_all(fx.path("data/state")).unwrap();
    fs::write(fx.path("data/state/journal.jsonl"), "").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_quen"))
        .arg("recover")
        .env("QUEN_CONFIG", fx.path("config.toml"))
        .env("QUEN_TEST_RECYCLE_TO", fx.path("recycled"))
        .output()
        .unwrap();
    let envelope: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(out.status.success(), "{envelope}");
    assert_eq!(
        (envelope["tool"].clone(), envelope["replayed"].clone()),
        (json!("session_recover"), json!(true)),
        "{envelope}"
    );
    assert!(!fx.path("data/state/journal.jsonl").exists());
}

/// Mods in quen's parking folder with no journal in this data root are another data root's session's; a plan
/// or launch from the game as it is would take them for the user's state. Both are refused, and nothing moves.
#[test]
fn a_launch_is_refused_while_another_sessions_mods_are_parked() {
    let fx = Fixture::new();
    let parked = fx.path(r"game\quen-parked\Mods\modOther");
    fs::create_dir_all(&parked).unwrap();
    let mut quen = Quen::start(&fx);
    for (tool, args) in [
        ("session_plan", json!({})),
        ("session_launch", json!({"user_ok": true})),
    ] {
        let out = quen.tool(tool, args);
        let why = out["reason"].as_str().unwrap_or_default();
        assert!(
            out["refused"] == true && why.contains("modOther") && why.contains("no journal"),
            "{tool}: {out}"
        );
    }
    assert!(parked.is_dir() && !fx.path("data/state/journal.jsonl").exists());
}

/// Feature tests 2b: with the devkit in the config and no RenoDX add-on, a launch wrote nothing that loads it, and the
/// boot row still said devkit. Now ReShade.ini gets `LoadFromDllMain` (the fake game loads what it names, as ReShade
/// does), the launch checks the module is in the game, the row says devkit only then, and the end gives the ini back.
/// Without the devkit's file a default launch leaves it off, and one that asks for it is refused.
#[test]
fn the_devkit_loads_without_renodx_and_the_row_says_so_once_it_is_in_the_game() {
    let fx = Fixture::new();
    let bin = fx.path("game/bin/x64_dx12");
    let ini = "[ADDON]\r\nLoadFromDllMain=\r\n";
    fs::write(bin.join("ReShade.ini"), ini).unwrap();
    fs::write(bin.join("dxgi.dll"), "reshade").unwrap();
    fx.config("devkit = 'devkit\\renodx-devkit.addon64'\n");
    let mut quen = Quen::start(&fx);
    let plan = quen.tool("session_plan", json!({}));
    assert_eq!(plan["devkit"], false, "no devkit file: {plan}");
    let refused = quen.tool("session_plan", json!({"devkit": true}));
    assert!(
        refused["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("renodx-devkit.addon64"),
        "{refused}"
    );
    // A DLL the test builds stands in for the devkit.
    let src = fx.path("devkit.rs");
    fs::write(
        &src,
        "#[no_mangle]\npub extern \"C\" fn quen_fake_devkit() {}\n",
    )
    .unwrap();
    fs::create_dir_all(bin.join("devkit")).unwrap();
    let out = std::process::Command::new("rustc")
        .args([
            "--edition",
            "2021",
            "--crate-type",
            "cdylib",
            "-C",
            "debuginfo=0",
            "-o",
        ])
        .arg(bin.join(r"devkit\renodx-devkit.addon64"))
        .arg(&src)
        .output()
        .expect("rustc runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let plan = quen.tool("session_plan", json!({}));
    assert_eq!(
        (plan["devkit"].clone(), plan["conditions"].clone()),
        (json!(true), json!(["devkit"])),
        "{plan}"
    );
    assert!(
        plan["changes"].to_string().contains("ReShade.ini"),
        "{plan}"
    );
    let launched = launch(&mut quen);
    assert!(launched["timing_s"]["devkit"].is_number(), "{launched}");
    // A path under the user's folder comes back as %USERPROFILE% (the fixture is in TEMP, under it here).
    let home = std::env::var_os("USERPROFILE").map(PathBuf::from);
    if home.is_some_and(|home| fx.0.starts_with(home)) {
        let journal = launched["state"]["journal"].as_str().unwrap_or_default();
        assert!(journal.starts_with(r"%USERPROFILE%\"), "{launched}");
    }
    assert_eq!(
        fs::read_to_string(bin.join("ReShade.ini")).unwrap(),
        "[ADDON]\r\nLoadFromDllMain=devkit\\renodx-devkit.addon64\r\n"
    );
    let ended = quen.tool("session_end", json!({"token": launched["token"]}));
    assert_eq!(ended["ok"], true, "{ended}");
    assert_eq!(fs::read_to_string(bin.join("ReShade.ini")).unwrap(), ini);
    fx.assert_restored();
    let rows = fx.ledger("SELECT devkit, json_extract(settings_diff, '$.conditions') FROM boots");
    assert_eq!(rows, [[json!(1), json!(r#"["devkit"]"#)]]);
}

/// The client kills a stdio server's tree 300 ms after closing its stdin. The proxy is gone well before that;
/// the daemon sees the connection close and ends the session the normal way.
#[test]
fn stdin_eof_exits_the_proxy_at_once_and_the_daemon_ends_the_session() {
    let fx = Fixture::new();
    let mut quen = Quen::start(&fx);
    launch(&mut quen);
    let game = fx.game_pid();
    let t = Instant::now();
    drop(quen.stdin.take());
    quen.child.wait().unwrap();
    let took = t.elapsed();
    assert!(took < Duration::from_millis(300), "the proxy took {took:?}");
    assert!(alive(quen.daemon));
    fx.wait_restored();
    assert!(dies(game));
    assert_eq!(fx.ledger("SELECT outcome FROM boots"), [[json!("ok")]]);
}

/// `taskkill /T /F` on the proxy, as the client does after 300 ms: the daemon isn't in its tree, so it survives, waits
/// for the save the game is writing, then kills the game through its job and replays the journal.
#[test]
fn killing_the_proxy_tree_leaves_the_daemon_to_end_the_session() {
    let fx = Fixture::new();
    let mut quen = Quen::start(&fx);
    launch(&mut quen);
    let game = fx.game_pid();
    let saves = fx.path("docs/gamesaves");
    // The game writes a save until the test has seen it outlive the proxy, however long the kill took.
    let (stop, stopped) = mpsc::channel::<()>();
    let writer = thread::spawn(move || {
        let mut size = 0;
        while stopped.try_recv() == Err(TryRecvError::Empty) {
            size += 1;
            fs::write(saves.join("QuickSave.sav"), vec![0u8; size]).unwrap();
            sleep(Duration::from_millis(200));
        }
    });
    let pid = quen.child.id().to_string();
    let out = Command::new("taskkill")
        .args(["/T", "/F", "/PID", &pid])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    quen.wait_exit();
    sleep(Duration::from_millis(500));
    assert!(alive(quen.daemon), "the daemon died with the proxy's tree");
    assert!(alive(game), "the game died while a save was being written");
    drop(stop);
    writer.join().unwrap();
    let end = fx.wait_restored();
    assert!(dies(game));
    assert_eq!(
        end["saves_wait"]["quiet"], true,
        "no quiet saves wait before the kill: {end}"
    );
}

/// The client runs its stdio servers in a job that kills them when it closes and allows breakaway (not silently).
/// The proxy starts the daemon from inside it; closing the job kills the proxy only.
#[test]
fn closing_the_clients_job_leaves_the_daemon() {
    let fx = Fixture::new();
    let mut quen = Quen::spawn(&fx, "config.toml", 0);
    let job = job_holding(
        &quen.child,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_BREAKAWAY_OK,
    );
    quen.init(&fx);
    launch(&mut quen);
    let game = fx.game_pid();
    // SAFETY: the job's only handle, closed once.
    unsafe { CloseHandle(job) };
    quen.wait_exit();
    assert!(alive(quen.daemon), "the daemon died with the client's job");
    fx.wait_restored();
    assert!(dies(game));
}

/// A client job that refuses breakaway and kills its processes on close: the proxy says so and exits; it never runs the
/// daemon inside it.
#[test]
fn a_job_that_refuses_breakaway_fails_loudly() {
    let fx = Fixture::new();
    let mut child = Command::new(env!("CARGO_BIN_EXE_quen"))
        .arg("mcp")
        .env("QUEN_CONFIG", fx.path("config.toml"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let job = job_holding(&child, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE);
    let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}});
    writeln!(child.stdin.as_mut().unwrap(), "{init}").unwrap();
    let out = child.wait_with_output().unwrap();
    // SAFETY: the job's only handle, closed once.
    unsafe { CloseHandle(job) };
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success() && out.stdout.is_empty(), "{out:?}");
    assert!(stderr.contains("refused breakaway"), "{stderr}");
    assert!(stderr.contains("quen serve --stay"), "{stderr}");
}

/// Codex's way (docs/clients.md): the user starts `quen serve --stay` in a terminal first. It outlives LINGER with no
/// connection, a second one for the same config refuses, and a relay in a job that kills on close and refuses
/// breakaway (Codex's) connects to it instead of starting one. Closing that job leaves the daemon.
#[test]
fn a_relay_in_a_no_breakaway_job_uses_a_stay_daemon() {
    /// A `--stay` daemon never exits by itself: killed when the test ends, however it ends.
    struct Stay(std::process::Child);
    impl Drop for Stay {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let fx = Fixture::new();
    let stay = || {
        let child = Command::new(env!("CARGO_BIN_EXE_quen"))
            .args(["serve", "--stay"])
            .env("QUEN_CONFIG", fx.path("config.toml"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .unwrap();
        Stay(child)
    };
    let mut serve = stay();
    fx.1.lock().unwrap().push(serve.0.id());
    let mut log = BufReader::new(serve.0.stderr.take().unwrap());
    let mut first = String::new();
    log.read_line(&mut first).unwrap();
    assert!(
        first.contains(r"on \\.\pipe\quen-") && first.contains("Ctrl+C"),
        "{first}"
    );
    // The rest of its log, read so it never meets a full pipe.
    thread::spawn(move || std::io::read_to_string(log));
    sleep(Duration::from_secs(7)); // past LINGER, 5 s
    assert!(serve.0.try_wait().unwrap().is_none(), "--stay exited idle");
    let mut second = stay();
    assert!(dies(second.0.id()), "a second --stay for the config ran");
    let stderr = std::io::read_to_string(second.0.stderr.take().unwrap()).unwrap();
    assert!(
        !second.0.wait().unwrap().success() && stderr.contains("already listens"),
        "{stderr}"
    );
    let mut relay = Command::new(env!("CARGO_BIN_EXE_quen"))
        .arg("mcp")
        .env("QUEN_CONFIG", fx.path("config.toml"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .unwrap();
    let job = job_holding(&relay, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE);
    let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                      "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                                 "clientInfo": {"name": "codex-mcp-client", "version": "0"}}});
    writeln!(relay.stdin.as_mut().unwrap(), "{init}").unwrap();
    let mut answer = String::new();
    BufReader::new(relay.stdout.take().unwrap())
        .read_line(&mut answer)
        .unwrap();
    // SAFETY: the job's only handle, closed once: the relay dies with it.
    unsafe { CloseHandle(job) };
    assert!(!answer.is_empty(), "{:?}", relay.wait_with_output());
    let answer: Value = serde_json::from_str(&answer).unwrap();
    assert_eq!(answer["result"]["serverInfo"]["name"], "quen", "{answer}");
    relay.wait().unwrap();
    sleep(Duration::from_millis(500));
    assert!(
        serve.0.try_wait().unwrap().is_none(),
        "the daemon died with the job"
    );
}

/// Feature tests 1: a Windows PowerShell 5.1 / .NET Framework client writes a UTF-8 BOM before its first line. quen skips
/// it, so that client's initialize isn't lost to a parse error.
#[test]
fn a_bom_before_the_first_line_is_skipped() {
    let fx = Fixture::new();
    let mut quen = Quen::spawn(&fx, "config.toml", 0);
    let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                      "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                                 "clientInfo": {"name": "powershell", "version": "5.1"}}});
    let stdin = quen.stdin.as_mut().unwrap();
    writeln!(stdin, "\u{feff}{init}").unwrap();
    stdin.flush().unwrap();
    quen.next_id = 1;
    let answer = quen.response(1);
    assert_eq!(answer["result"]["serverInfo"]["name"], "quen", "{answer}");
    let status = quen.tool("session_status", json!({}));
    fx.1.lock()
        .unwrap()
        .push(status["serve_pid"].as_u64().unwrap() as u32);
}

/// Two proxies started at once find one daemon. The second reads status, can't drive or end the first one's session
/// even with its token, and closing it leaves that session alone.
#[test]
fn two_proxies_share_one_daemon_and_only_the_launcher_ends_its_session() {
    let fx = Fixture::new();
    let (mut first, mut second) = thread::scope(|s| {
        let a = s.spawn(|| Quen::start(&fx));
        let b = s.spawn(|| Quen::start(&fx));
        (a.join().unwrap(), b.join().unwrap())
    });
    assert_eq!(first.daemon, second.daemon, "two daemons for one config");
    let launched = launch(&mut first);
    let game = fx.game_pid();
    let status = second.tool("session_status", json!({}));
    assert_eq!(status["state"]["lock"], "held here");
    assert_eq!(status["state"]["game"]["running"], true);
    for (tool, args) in [
        ("session_end", json!({"token": launched["token"]})),
        (
            "game_console",
            json!({"token": launched["token"], "line": "x"}),
        ),
    ] {
        let stolen = second.tool(tool, args);
        assert_eq!(stolen["refused"], true, "{stolen}");
        let reason = stolen["reason"].as_str().unwrap();
        assert!(reason.contains("another connection"), "{reason}");
    }
    drop(second);
    sleep(Duration::from_millis(500));
    assert!(alive(game), "another connection closing ended the session");
    let ended = first.tool("session_end", json!({"token": launched["token"]}));
    assert_eq!(ended["ok"], true, "{ended}");
    assert!(dies(game));
    fx.assert_restored();
    assert_eq!(fx.ledger("SELECT outcome FROM boots"), [[json!("ok")]]);
}

/// Ctrl+Break at the proxy, which has no handler: it dies, and the daemon ends the session as for any closed
/// connection. A process in its own console process group can be sent only Ctrl+Break (Windows turns Ctrl+C off in a
/// new group).
#[test]
fn ctrl_break_at_the_proxy_ends_the_session_through_the_daemon() {
    let fx = Fixture::new();
    let mut quen = Quen::spawn(&fx, "config.toml", CREATE_NEW_PROCESS_GROUP);
    quen.init(&fx);
    launch(&mut quen);
    let game = fx.game_pid();
    // SAFETY: a plain call; the group id is the proxy's pid, the root of its new process group.
    let sent = unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, quen.child.id()) };
    assert_ne!(sent, 0, "{}", std::io::Error::last_os_error());
    quen.wait_exit();
    fx.wait_restored();
    assert!(dies(game));
    // An end that left its journal says why in its failure.
    let rows = fx.ledger("SELECT outcome, failure FROM boots");
    assert_eq!(rows, [[json!("ok"), Value::Null]]);
}

/// Ctrl+C, Ctrl+Break and the console closing at `quen serve` itself (run by hand, here in its own process group): its
/// handler ends the session the normal way, saves first, then the daemon exits.
#[test]
fn ctrl_break_at_the_daemon_ends_the_session_before_exit() {
    let fx = Fixture::new();
    let mut serve = Command::new(env!("CARGO_BIN_EXE_quen"))
        .arg("serve")
        .env("QUEN_CONFIG", fx.path("config.toml"))
        .env("QUEN_TEST_PINS", test_pins())
        .env("QUEN_TEST_RECYCLE_TO", fx.path("recycled"))
        .env("QUEN_TEST_INPUT", fx.path("harness/fake_input.py"))
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NEW_PROCESS_GROUP)
        .spawn()
        .unwrap();
    // Its first line comes once it listens; the rest is read to its end, so its log never meets a closed pipe.
    let mut log = BufReader::new(serve.stderr.take().unwrap());
    let mut first = String::new();
    log.read_line(&mut first).unwrap();
    assert!(first.contains(r"on \\.\pipe\quen-"), "{first}");
    let log = thread::spawn(move || std::io::read_to_string(log).unwrap());
    let mut quen = Quen::start(&fx);
    assert_eq!(
        quen.daemon,
        serve.id(),
        "the proxy started a daemon of its own"
    );
    launch(&mut quen);
    let game = fx.game_pid();
    // SAFETY: a plain call; the group id is the daemon's pid, the root of its new process group.
    let sent = unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, serve.id()) };
    assert_ne!(sent, 0, "{}", std::io::Error::last_os_error());
    assert!(serve.wait().unwrap().success());
    assert!(dies(game));
    fx.assert_restored();
    assert_eq!(fx.ledger("SELECT outcome FROM boots"), [[json!("ok")]]);
    let log = log.join().unwrap();
    assert!(log.contains(r#""saves_wait":{"quiet":true"#), "{log}");
    quen.wait_exit();
}

#[test]
fn refuses_before_acting() {
    let fx = Fixture::new();
    let mut quen = Quen::start(&fx);
    let refused = |e: &Value| e["ok"] == false && e["refused"] == true;

    let no_ok = quen.tool("session_launch", json!({}));
    assert!(refused(&no_ok) && no_ok["reason"].as_str().unwrap().contains("user_ok"));
    // Each refused as a dry run and as a launch, before the lock or the journal.
    for bad in [
        json!({"frobnicate": 1}),
        json!({"profiler": "nsys"}),
        json!({"profiler": "perf:x"}),
        json!({"preset": "Bad Key=1"}),
        json!({"save": "NoSuchSave.sav"}),
        // The harness's own: never parked (its park_names), our add-on only with W3_PARK_RENODX=1.
        json!({"park": "modRenoDXTest"}),
        json!({"park": "renodx-devkit.addon64"}),
        json!({"park": "renodx-thewitcher3remastered.addon64"}),
        json!({"park": "modQuen"}),
        json!({"settings": {"flat": "NoSuchKey=1"}}),
        json!({"keep": "modAlpha"}),
        // Switches the native boot doesn't have, or that aren't a number.
        json!({"extra_env": {"W3_SIGNALS": "dinput8"}}),
        json!({"extra_env": {"PATH": "x"}}),
        json!({"extra_env": {"W3_NGFX_FRAMES": "3; x"}}),
        // ngfx arguments come only from the config, never from the agent.
        json!({"extra_env": {"W3_NGFX_ARGS": "--time-every-action"}}),
        json!({"extra_env": {"W3_NGFX_FRAMES": "3\u{0}"}}),
    ] {
        for plan in [true, false] {
            let mut args = bad.clone();
            if !plan {
                args["user_ok"] = json!(true);
            }
            let tool = if plan {
                "session_plan"
            } else {
                "session_launch"
            };
            let envelope = quen.tool(tool, args);
            assert!(refused(&envelope), "{bad}: {envelope}");
            assert_eq!(envelope["state"]["lock"]["free"], true);
            assert!(!fx.path("data/state/journal.jsonl").exists());
        }
    }
    assert_eq!(fx.settings(), SETTINGS);
    let addon = "renodx-thewitcher3remastered.addon64";
    fs::write(fx.path("game/bin/x64_dx12").join(addon), "addon").unwrap();
    let asked = quen.tool(
        "session_plan",
        json!({"park": addon, "extra_env": {"W3_PARK_RENODX": "1"}}),
    );
    assert_eq!(asked["ok"], true, "{asked}");

    fs::write(fx.path("docs/gamesaves/a b.sav"), "manual").unwrap();
    let dry = quen.tool(
        "session_plan",
        json!({"save": "a b.sav", "park": "ALL", "keep": "modAlpha", "vsync": false,
               "settings": {"flat": "MotionBlur=false", "sections": "Gameplay:Hdr=1"}}),
    );
    assert_eq!(dry["ok"], true, "{dry}");
    assert!(dry["save"].as_str().unwrap().ends_with("a b.sav"));
    let changes: Vec<&str> = dry["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap())
        .collect();
    // ALL takes the ASI, keeps modAlpha and never takes our add-on (no W3_PARK_RENODX).
    assert_eq!(changes.len(), 4, "{changes:?}");
    assert!(
        changes[0].starts_with("park ") && changes[0].contains("W3Fake.asi"),
        "{changes:?}"
    );
    assert!(
        changes[1].contains("input.settings")
            && changes[2].contains("dx12user.settings")
            && changes[3].contains("general.ini"),
        "{changes:?}"
    );
    let exe = dry["command"]["argv"][0].as_str().unwrap();
    assert!(exe.ends_with(&fx.2), "{dry}");
    assert_eq!(dry["state"]["lock"]["free"], true);
    assert_eq!(fx.settings(), SETTINGS, "a dry run changed the settings");
    // Feature tests 1: each line the launch would write, the forced keys and the overrides.
    let written: Vec<&str> = dry["settings_written"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l.as_str().unwrap())
        .collect();
    assert_eq!(
        written,
        [
            "Rendering:DLSSGMode=0",
            "Rendering:VSync=false",
            "Rendering:MotionBlur=false",
            "Gameplay:Hdr=1",
            "Gameplay:CameraAutoRotX=false",
            "Gameplay:CameraAutoRotY=false",
        ],
        "{dry}"
    );
    assert_eq!(dry["settings_diff"]["flat"], "MotionBlur=false", "{dry}");
    // The plan takes no user_ok, and the launch no dry_run (the plan's old place): refused, nothing changed.
    for (tool, key) in [("session_plan", "user_ok"), ("session_launch", "dry_run")] {
        let mut args = json!({});
        args[key] = json!(true);
        let out = quen.tool(tool, args);
        assert!(
            refused(&out) && out["reason"].to_string().contains(key),
            "{out}"
        );
    }
    assert_eq!(fx.settings(), SETTINGS);
    // Without a save named, the newest QuickSave, as the harness picks it.
    let quick = quen.tool("session_plan", json!({}));
    assert!(
        quick["save"].as_str().unwrap().ends_with("QuickSave.sav"),
        "{quick}"
    );
    // Gemini CLI adds wait_for_previous to every tool and passes it on: ignored, not an unknown argument.
    let gemini = quen.tool("session_plan", json!({"wait_for_previous": true}));
    assert_eq!(gemini["ok"], true, "{gemini}");

    // A Python session's leftovers (with the harness configured): refused, naming the Python recovery.
    fs::create_dir_all(fx.path("harness/cap/ini/orig")).unwrap();
    fs::write(fx.path("harness/cap/ini/orig/settings"), SETTINGS).unwrap();
    let left = quen.tool("session_launch", json!({"user_ok": true}));
    assert!(
        refused(&left)
            && left["reason"]
                .as_str()
                .unwrap()
                .contains("ini_session.py recover")
    );
    let recover = quen.tool("session_recover", json!({}));
    assert!(refused(&recover), "{recover}");
    assert_eq!(fx.settings(), SETTINGS);
    assert!(!fx.path("data/state/journal.jsonl").exists());
}

/// Without a harness in the config there is no interlock to keep: its folder's leftovers mean nothing to quen.
#[test]
fn without_the_harness_there_is_no_interlock() {
    let fx = Fixture::new();
    fs::create_dir_all(fx.path("harness/cap/ini/orig")).unwrap();
    fs::write(fx.path("harness/cap/ini/orig/settings"), SETTINGS).unwrap();
    let config = fx.read("config.toml");
    let config: String = config
        .lines()
        .filter(|l| !l.starts_with("harness"))
        .map(|l| format!("{l}\n"))
        .collect();
    fs::write(fx.path("config.toml"), config).unwrap();
    let mut quen = Quen::start(&fx);
    let dry = quen.tool("session_plan", json!({}));
    assert_eq!(dry["ok"], true, "{dry}");
    let status = quen.tool("session_status", json!({}));
    assert_eq!(status["harness_leftovers"], json!([]), "{status}");
}

/// game_screenshot runs on the session: without a session, with a wrong token or a bad tag it is refused and writes
/// nothing. The grabs themselves are tested on a window shown on screen (`--ignored` below, and capture.rs).
#[test]
fn a_screenshot_needs_the_session_token() {
    let fx = Fixture::new();
    let mut quen = Quen::start(&fx);
    let tools = quen.request("tools/list", json!({}));
    let listed = tools["result"]["tools"].as_array().unwrap();
    assert!(listed.iter().any(|t| t["name"] == "game_screenshot"));
    // Finding 38: session_status fingerprints the list a client got.
    let status = quen.tool("session_status", json!({}));
    let sha = quen_core::journal::sha256_hex(tools["result"].to_string().as_bytes());
    let fingerprint = json!({"count": listed.len(), "sha256": sha});
    assert_eq!(status["tools"], fingerprint, "{status}");
    let refused =
        |e: &Value, why: &str| e["refused"] == true && e["reason"].as_str().unwrap().contains(why);
    let early = quen.tool("game_screenshot", json!({"token": "x", "tag": "A1"}));
    assert!(refused(&early, "no live session"), "{early}");
    let launched = launch(&mut quen);
    let token = &launched["token"];
    for (args, why) in [
        (json!({"token": "not-it", "tag": "A1"}), "token"),
        (json!({"token": token, "tag": "..\\A1"}), "tag"),
    ] {
        let shot = quen.tool("game_screenshot", args);
        assert!(refused(&shot, why), "{shot}");
    }
    assert!(!fx.path("data/captures").exists());
    let ended = quen.tool("session_end", json!({"token": token}));
    assert_eq!(ended["ok"], true, "{ended}");
}

/// game_screenshot end to end on the fake game's window shown on screen (sRGB 200, 100, 50): found through the job,
/// written under the boot's capture folder, one ledger row per file. It shows a small topmost window for a few
/// seconds, so it runs only when asked: `cargo test -p quen --test session -- --ignored`.
#[test]
#[ignore = "shows a window on the desktop"]
fn a_screenshot_of_the_game_window_lands_in_its_boot_folder_and_the_ledger() {
    let fx = Fixture::new();
    fs::write(fx.path("game/bin/x64_dx12/on_screen"), "").unwrap();
    let mut quen = Quen::start(&fx);
    let launched = launch(&mut quen);
    let (token, boot) = (&launched["token"], &launched["ledger"]["boots"][0]);
    assert!(boot.is_i64(), "{launched}");
    let sdr = quen.tool("game_screenshot", json!({"token": token, "tag": "A1"}));
    assert_eq!(sdr["ok"], true, "{sdr}");
    assert_eq!(sdr["method"], "printwindow");
    assert!(sdr["display_state"]["color_mode"].is_string(), "{sdr}");
    let hdr = quen.tool(
        "game_screenshot",
        json!({"token": token, "tag": "B2", "method": "hdr"}),
    );
    assert_eq!(hdr["ok"], true, "{hdr}");
    let again = quen.tool("game_screenshot", json!({"token": token, "tag": "A1"}));
    assert_eq!(again["refused"], true, "{again}");

    let mut ids = Vec::new();
    for (shot, want) in [
        (&sdr, &["srgb8"][..]),
        (&hdr, &["scrgb_f16", "pq_bt2020_u16"]),
    ] {
        let captures = shot["captures"].as_array().unwrap();
        let encodings: Vec<&str> = captures
            .iter()
            .map(|c| c["encoding"].as_str().unwrap())
            .collect();
        assert_eq!(encodings, want);
        for (capture, artifact) in captures.iter().zip(shot["artifacts"].as_array().unwrap()) {
            let path = Path::new(capture["path"].as_str().unwrap());
            assert!(
                path.starts_with(fx.path("data/captures")) && path.exists(),
                "{capture}"
            );
            assert_eq!(capture["path"], artifact["path"]);
            ids.push(capture["id"].as_i64().unwrap());
        }
        assert_eq!(
            shot["ledger"]["captures"],
            json!(ids[ids.len() - want.len()..])
        );
    }
    let rows = fx
        .ledger("SELECT id, method, encoding, includes_desktop, boot_id FROM captures ORDER BY id");
    let want: Vec<Vec<Value>> = ids
        .iter()
        .zip(["printwindow", "wgc_fp16", "wgc_fp16"])
        .zip(["srgb8", "scrgb_f16", "pq_bt2020_u16"])
        .map(|((id, method), encoding)| {
            vec![
                json!(id),
                json!(method),
                json!(encoding),
                json!(0),
                boot.clone(),
            ]
        })
        .collect();
    assert_eq!(rows, want);

    // The PNG holds the window's fill.
    let png = fs::read(sdr["captures"][0]["path"].as_str().unwrap()).unwrap();
    let mut reader = png::Decoder::new(std::io::Cursor::new(png))
        .read_info()
        .unwrap();
    let mut rgb = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut rgb).unwrap();
    let centre = ((info.height / 2 * info.width + info.width / 2) * 3) as usize;
    assert_eq!(rgb[centre..centre + 3], [200, 100, 50]);
    let ended = quen.tool("session_end", json!({"token": token}));
    assert_eq!(ended["ok"], true, "{ended}");
}

/// Two configs over one data root: two daemons, and the one without the lock is read-only.
#[test]
fn a_second_daemon_is_read_only_while_the_first_drives() {
    let fx = Fixture::new();
    let mut first = Quen::start(&fx);
    let launched = launch(&mut first);
    let mut second = Quen::spawn(&fx, fx.second_config(), 0);
    second.init(&fx);
    assert_ne!(first.daemon, second.daemon);
    // Calls queue behind the startup replay on the session thread, so it is over when this returns.
    let refused = second.tool("session_recover", json!({}));
    assert_eq!(refused["refused"], true, "{refused}");
    let status = second.tool("session_status", json!({}));
    assert_eq!(
        status["state"]["lock"],
        "held by another quen (read-only here)"
    );
    let startup = &status["startup_replay"];
    assert_eq!(
        startup["refused"], true,
        "a quen without the lock replayed: {startup}"
    );
    // Feature tests 1: the other quen's live session, not "a journal a dead quen left", and its game shows.
    for tool in ["session_plan", "session_launch"] {
        let out = second.tool(tool, json!({}));
        let why = out["reason"].as_str().unwrap_or_default();
        assert!(why.contains("a session is live there"), "{tool}: {out}");
    }
    let outside = status["game_outside_job"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let ours = first.tool("session_status", json!({}))["state"]["game"]["pids"].clone();
    let ours = ours.as_array().unwrap();
    assert!(
        !outside.is_empty() && outside.iter().all(|p| ours.contains(p)),
        "{status}"
    );
    let ended = first.tool("session_end", json!({"token": launched["token"]}));
    assert_eq!(ended["ok"], true);
    fx.assert_restored();
}

/// The game, focus and measure tools are listed and run on the session thread; without a live session they refuse.
#[test]
fn game_and_measure_tools_need_a_live_session() {
    let fx = Fixture::new();
    let mut quen = Quen::start(&fx);
    let list = quen.request("tools/list", json!({}));
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for tool in [
        "game_load",
        "game_console",
        "game_cvar",
        "game_keys",
        "game_listsaves",
        "game_pause",
        "game_settime",
        "game_weather",
        "game_teleport",
        "game_save",
        "focus",
        "hands_take",
        "hands_give",
        "world_get",
        "world_frame",
        "world_ground",
        "world_ray",
        "world_nav",
        "game_frame",
        "game_place",
        "game_exec",
        "measure_presentmon",
        "measure_trace",
        "measure_ab",
    ] {
        assert!(names.contains(&tool), "{names:?}");
        let out = quen.tool(tool, json!({"token": "x"}));
        assert_eq!(out["refused"], true, "{tool}: {out}");
    }
    let unknown = quen.request("tools/call", json!({"name": "game_nope", "arguments": {}}));
    assert_eq!(unknown["error"]["code"], -32602);
}
