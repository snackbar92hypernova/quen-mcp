//! `game_save`: a new manual save, written by the game itself through quen's command channel (the `save` verb,
//! quen-w3 0.1.4), as the menu's "Empty save slot" writes one: the game names it, and the verb can't name a slot, so it
//! never overwrites. quen checks gamesaves before and after: one new `ManualSave_*` and nothing else, which the guard
//! then passes (session.rs); anything else ends the session. Not journaled: quen never deletes a save, so nothing is
//! undone.

use std::thread::sleep;
use std::time::{Duration, Instant};

use quen_core::bridge_log;
use quen_core::journal::sha256_file;
use quen_core::ledger::Save;
use quen_core::protocol::Verb;
use quen_core::saves::{self, W3_AUTO_SAVES, W3_MANUAL};
use serde_json::{Map, Value, json};

use crate::channel::REPLY_MAX;
use crate::doctor;
use crate::game::{Fields, ctx, flag, schema, shown};
use crate::session::{Call, Done, Session, ledger_error, open_ledger};

/// How long the game may take to start writing the save, and how long gamesaves must then stay unchanged: the end's
/// own wait for the folder.
const SAVE_MAX: Duration = Duration::from_secs(30);
const QUIET: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(100);
/// The longest note, in characters.
const NOTE_MAX: usize = 500;

pub(crate) fn tool() -> Value {
    json!({
        "name": "game_save",
        "description": "Write a new manual save, as the game's menu does into its \"Empty save slot\": the game \
            names it (ManualSave_...), never overwrites one, and lists it with the user's own saves. The client asks \
            the user on every call (user_ok is only your word): never route around a denial. quen checks gamesaves \
            before and after: one new save (its .sav, .png, .json) and nothing else. Answers save (the name \
            game_load takes), files, tainted. Refused, nothing sent: no user_ok; quen's state not plain gameplay (the \
            main menu, photo mode, a scene, cutscene, fade or the dialogue flag); a write that taints saves earlier in \
            this boot (tainted_by) without tainted_ok; the game's own save lock (combat, a menu: game_text); with \
            needs: quen-w3 0.1.3 or older. Failed: no new save within 30 s (a dialog may wait for OK), or anything \
            else changed in gamesaves (the session was ended). The save is the user's: never delete it.",
        // Claude Code prompts for this tool on every call, in every permission mode, as for session_launch.
        "_meta": {"anthropic/requiresUserInteraction": true},
        "inputSchema": schema(json!({
            "user_ok": {"type": "boolean", "description": "true only after telling the user in this conversation that the game is about to write a new save into their save list, and they agreed. Required."},
            "tainted_ok": {"type": "boolean", "description": "true to save after a write that taints saves (mem_patch, game_cvar set, game_teleport, game_settime, game_weather), the user having agreed to a tainted save. Default false: refused"},
            "note": {"type": "string", "description": "a note for the save's ledger row, at most 500 characters; it never reaches the game"},
        }), &["user_ok"]),
    })
}

/// game_save's checks, the verb, the wait for the files and their check; then the files go into the session's
/// accepted saves and the .sav into the ledger.
pub(crate) fn call(session: &Session, call: &mut Call, args: &Value) -> Result<Fields, Done> {
    let cx = ctx(session, args, &["user_ok", "tainted_ok", "note"])?;
    let note = args.get("note").map(|n| {
        let text = n.as_str().filter(|n| n.chars().count() <= NOTE_MAX);
        text.ok_or(format!(
            "note must be text of at most {NOTE_MAX} characters"
        ))
    });
    let note = note.transpose()?;
    let tainted_ok = flag(args, "tainted_ok")?;
    if !flag(args, "user_ok")? {
        let why = "user_ok: tell the user the game is about to write a new save into their save list, and get \
                   their OK first";
        return Err(why.to_string().into());
    }
    doctor::installed(&[&doctor::PACKAGE], cx.config, "game_save")?;
    doctor::package_at_least(cx.config, doctor::BEFORE_SAVE, "game_save", "0.1.4")?;
    let last = cx.last_flags();
    if !last.is_some_and(|f| bridge_log::plain(f) && !bridge_log::in_photo(f)) {
        return Err(format!(
            "quen's state shows {}: game_save needs plain gameplay (not the main menu, photo mode, a scene, a \
             cutscene, a fade or the dialogue flag). Nothing sent",
            shown(last)
        )
        .into());
    }
    // finish's rule (session.rs): every save after the boot's first tainting write is tainted.
    let tainted_by: Vec<Value> = cx
        .live
        .patches
        .borrow()
        .iter()
        .filter(|(_, patch)| patch["taints"] != false)
        .map(|(_, patch)| json!({"what": patch["what"], "t": patch["t"]}))
        .collect();
    let tainted = !tainted_by.is_empty();
    let mut fields = Map::new();
    fields.insert("tainted".into(), json!(tainted));
    fields.insert("tainted_by".into(), json!(tainted_by));
    if tainted && !tainted_ok {
        let why = "this boot made a write that taints saves (tainted_by), so a save now carries it: pass \
                   tainted_ok: true, with the user's OK to a tainted save, to save anyway. Nothing sent";
        return Err(Done::RefusedWith(why.into(), fields));
    }
    session.guard_saves()?;
    let channel = cx.channel()?;
    call.lap("check");

    let dir = &cx.live.saves;
    let before = saves::snapshot(dir);
    let reply = channel.call(Verb::Save {}, REPLY_MAX);
    call.lap("reply");
    let reply = reply.map_err(|e| e.done(fields.clone()))?;
    fields.insert("reply".into(), json!(reply.text));
    // The game names the save and writes it after the verb returns (feature tests 3: its .sav, .png and .json).
    let start = Instant::now();
    let begun = |now: &saves::Snapshot| {
        now.keys()
            .any(|n| !before.contains_key(n) && n.starts_with(W3_MANUAL) && n.ends_with(".sav"))
    };
    while !begun(&saves::snapshot(dir)) && start.elapsed() < SAVE_MAX {
        sleep(POLL);
    }
    let (_, quiet) = saves::wait_quiet(dir, QUIET, SAVE_MAX.saturating_sub(start.elapsed()));
    let secs = (start.elapsed().as_secs_f64() * 100.0).round() / 100.0;
    fields.insert("saves_wait".into(), json!({"s": secs, "quiet": quiet}));
    call.lap("files");

    let after = saves::snapshot(dir);
    let files = match saves::new_save(&before, &after, W3_AUTO_SAVES) {
        Ok(files) if files.is_empty() => {
            let why = format!(
                "no new save within {} s: the game may show a dialog that waits for OK (game_screenshot shows it)",
                SAVE_MAX.as_secs()
            );
            return Err(Done::Failed(why, fields));
        }
        Ok(files) if quiet => files,
        Ok(files) => {
            fields.insert("changed".into(), json!(files));
            let why = format!(
                "the new save was still being written after {} s, so quen didn't accept it",
                SAVE_MAX.as_secs()
            );
            return Err(Done::Failed(why, fields));
        }
        Err(changed) => {
            fields.insert("changed".into(), json!(changed));
            let why = "gamesaves changed beyond one new manual save (changed)";
            return Err(Done::Failed(why.into(), fields));
        }
    };
    cx.live
        .quen_saves
        .borrow_mut()
        .extend(files.iter().map(|name| (name.clone(), after[name])));

    let mut listed = Vec::new();
    let mut sav = None;
    for name in &files {
        let sha = sha256_file(&dir.join(name)).map_err(|e| e.to_string());
        if name.ends_with(".sav") {
            sav = Some((name, sha.clone()));
        }
        listed.push(json!({"file": name, "bytes": after[name].0, "sha256": sha.ok()}));
    }
    let (sav, sha) = sav.expect("new_save gives a .sav");
    let row = sha.and_then(|sha256| {
        let save = Save {
            game_id: cx.live.game,
            file: sav.clone(),
            sha256,
            note: note.map(String::from),
            tainted_by_boot: tainted.then_some(cx.live.boot),
            made_by_boot: Some(cx.live.boot),
        };
        open_ledger(cx.config)?
            .add_save(&save)
            .map_err(ledger_error)
    });
    match row {
        Ok(id) => call.row("saves", id),
        Err(e) => {
            fields.insert("ledger_error".into(), json!(e));
        }
    }
    call.lap("ledger");
    let name = sav.strip_suffix(".sav").unwrap_or(sav).to_lowercase();
    fields.insert("save".into(), json!(name));
    fields.insert("files".into(), json!(listed));
    Ok(fields)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::tests::{fake_game, reply, served, token};
    use crate::game::tests::{Fake, PARTS};
    use quen_core::protocol::Request;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::thread::JoinHandle;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// A file of `data`, written at Unix second `secs`.
    fn touch(path: &Path, data: &str, secs: u64) {
        fs::write(path, data).unwrap();
        let file = fs::File::options().write(true).open(path).unwrap();
        file.set_modified(UNIX_EPOCH + Duration::from_secs(secs))
            .unwrap();
    }

    /// The Fake's session over a gamesaves folder holding the user's saves (a manual save, a quicksave, an autosave) as
    /// they were at the launch.
    fn session(fx: &Fake) -> (Session<'_>, PathBuf) {
        let dir = fx.dir.join("gamesaves");
        fs::create_dir_all(&dir).unwrap();
        touch(&dir.join("ManualSave_user.sav"), "user save", 100);
        touch(&dir.join("ManualSave_user.png"), "user png", 100);
        touch(&dir.join("QuickSave.sav"), "quick", 100);
        touch(&dir.join("AutoSave_1.sav"), "auto", 100);
        let mut session = fx.session();
        session.live.as_mut().unwrap().saves_before = saves::snapshot(&dir);
        (session, dir)
    }

    fn run(session: &Session, args: Value) -> Value {
        let mut call = session.call(crate::mcp::Conn::nobody(), Value::Null, None);
        let done = super::call(session, &mut call, &args).map_or_else(|done| done, Done::Ok);
        session.envelope("game_save", call, done)
    }

    /// A stand-in game whose `save` runs `write` on `dir`, then answers as modQuen does.
    fn saving_game(fx: &Fake, dir: PathBuf, write: fn(&Path)) -> JoinHandle<Vec<Request>> {
        fake_game(
            &token(fx),
            1,
            Box::new(move |r| {
                assert_eq!(r.verb, Verb::Save {});
                write(&dir);
                vec![reply(r.id, true, "save requested", None)]
            }),
        )
    }

    /// The menu's "Empty save slot" as seen live: a new ManualSave's .sav, .png and .json; the game's autosave beside.
    fn new_save(dir: &Path) {
        for ext in ["sav", "png", "json"] {
            fs::write(dir.join(format!("ManualSave_new.{ext}")), ext).unwrap();
        }
        fs::write(dir.join("AutoSave_2.sav"), "auto").unwrap();
    }

    fn ledger_rows(fx: &Fake, sql: &str) -> Vec<Vec<Value>> {
        let config = fx.shared.config().as_ref().clone().unwrap();
        let ledger = open_ledger(&config).unwrap();
        ledger.query(sql, &[], Duration::from_secs(5)).unwrap().rows
    }

    #[test]
    fn the_game_writes_one_new_save_and_the_guard_passes_only_its_files() {
        let fx = served("save-ok");
        let (session, dir) = session(&fx);
        let user = saves::snapshot(&dir);
        let server = saving_game(&fx, dir.clone(), new_save);
        let args = json!({"token": "tok", "user_ok": true, "note": "a test save"});
        let out = run(&session, args);
        assert_eq!(out["ok"], true, "{out}");
        assert_eq!(out["save"], "manualsave_new", "{out}");
        assert_eq!(out["tainted"], false, "{out}");
        assert_eq!(out["reply"], "save requested", "{out}");
        assert_eq!(out["saves_wait"]["quiet"], true, "{out}");
        let files: Vec<&str> = out["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["file"].as_str().unwrap())
            .collect();
        let three = [
            "ManualSave_new.json",
            "ManualSave_new.png",
            "ManualSave_new.sav",
        ];
        assert_eq!(files, three);
        let sha = sha256_file(&dir.join(three[2])).unwrap();
        assert_eq!(out["files"][2]["sha256"], sha, "{out}");
        assert_eq!(server.join().unwrap().len(), 1);
        let now = saves::snapshot(&dir);
        assert!(
            user.iter().all(|(name, was)| now.get(name) == Some(was)),
            "nothing the user had changed"
        );
        let sql = "SELECT id, file, made_by_boot, tainted_by_boot, note FROM saves";
        let row = vec![
            out["ledger"]["saves"][0].clone(),
            json!(three[2]),
            json!(1),
            Value::Null,
            json!("a test save"),
        ];
        assert_eq!(ledger_rows(&fx, sql), [row]);

        // The guard passes quen's files while they are as quen found them, and nothing else: a menu save, the menu
        // overwriting quen's save and a quicksave each end the session (session.rs guard_after).
        assert_eq!(session.guard_saves(), Ok(()));
        for (name, data) in [
            ("ManualSave_menu.sav", "the menu's save"),
            ("ManualSave_new.sav", "the menu overwrote quen's"),
            ("QuickSave.sav", "a quicksave"),
        ] {
            let path = dir.join(name);
            let was = now.get(name).copied();
            let kept = fs::read(&path).ok();
            fs::write(&path, data).unwrap();
            let why = session.guard_saves().unwrap_err();
            assert!(why.contains(name), "{name}: {why}");
            match (kept, was) {
                (Some(bytes), Some((_, Some(t)))) => {
                    fs::write(&path, bytes).unwrap();
                    let file = fs::File::options().write(true).open(&path).unwrap();
                    file.set_modified(t).unwrap();
                }
                _ => fs::remove_file(&path).unwrap(), // this test's own synthetic file
            }
            assert_eq!(session.guard_saves(), Ok(()), "{name} put back");
        }
    }

    /// A save that came with any other change (here the stand-in also overwrites the user's save) fails, none of it
    /// accepted, so the guard ends the session.
    #[test]
    fn anything_beside_one_new_save_fails_and_the_guard_ends_the_session() {
        let fx = served("save-overwrite");
        let (session, dir) = session(&fx);
        let server = saving_game(&fx, dir, |dir| {
            new_save(dir);
            fs::write(dir.join("ManualSave_user.sav"), "overwritten").unwrap();
        });
        let out = run(&session, json!({"token": "tok", "user_ok": true}));
        assert_eq!(out["ok"], false, "{out}");
        assert_eq!(out["refused"], false, "{out}");
        let why = out["reason"].as_str().unwrap();
        assert!(why.contains("beyond one new manual save"), "{out}");
        let changed = out["changed"].as_array().unwrap();
        assert!(changed.contains(&json!("ManualSave_user.sav")), "{out}");
        let live = session.live.as_ref().unwrap();
        assert!(live.quen_saves.borrow().is_empty());
        assert!(session.guard_saves().is_err());
        server.join().unwrap();
    }

    #[test]
    fn refuses_before_sending() {
        let fx = served("save-refuse");
        let state = fx.dir.join("game/bin/x64_dx12/W3QuenState.log");
        let (session, dir) = session(&fx);
        let user = saves::snapshot(&dir);
        // The game's own lock: refused with its words in game_text.
        let server = fake_game(
            &token(&fx),
            1,
            Box::new(|r| vec![reply(r.id, false, "the game locks saves now", None)]),
        );
        let ok = json!({"token": "tok", "user_ok": true});
        let locked = run(&session, ok.clone());
        assert_eq!(locked["refused"], true, "{locked}");
        assert_eq!(locked["game_text"], "the game locks saves now", "{locked}");
        for args in [
            json!({"token": "tok"}),
            json!({"token": "tok", "user_ok": false}),
            json!({"token": "nope", "user_ok": true}),
            json!({"token": "tok", "user_ok": true, "slot": 0}),
            json!({"token": "tok", "user_ok": true, "note": 7}),
            json!({"token": "tok", "user_ok": true, "note": "x".repeat(NOTE_MAX + 1)}),
            json!({"token": "tok", "user_ok": true, "tainted_ok": "yes"}),
        ] {
            let out = run(&session, args.clone());
            assert_eq!(out["refused"], true, "{args}: {out}");
        }
        // Every state but plain gameplay: no flags yet, the main menu, photo mode, a scene, a cutscene, a fade, the
        // dialogue flag. Combat (64) is the game's own lock's to refuse.
        for flags in ["", "0", "129", "17", "33", "257", "513"] {
            let line = match flags {
                "" => String::new(),
                f => format!("push flags={f}\n"),
            };
            fs::write(&state, line).unwrap();
            let out = run(&session, ok.clone());
            let why = out["reason"].as_str().unwrap_or_default();
            assert!(
                out["refused"] == true && why.contains("plain gameplay"),
                "{flags:?}: {out}"
            );
        }
        fs::write(&state, "push flags=65\n").unwrap();
        // A write that taints saves: refused without tainted_ok, naming it.
        let patch = json!({"what": "mem_patch cull", "t": "2026-10-19T00:00:00Z", "taints": true});
        let live = session.live.as_ref().unwrap();
        live.patches.borrow_mut().push((SystemTime::now(), patch));
        let tainted = run(&session, ok.clone());
        assert_eq!(tainted["refused"], true, "{tainted}");
        assert_eq!(
            tainted["tainted_by"][0]["what"], "mem_patch cull",
            "{tainted}"
        );
        // Without the package: needs.
        for part in PARTS {
            fs::remove_file(fx.dir.join("game").join(part)).unwrap(); // this test's own synthetic files
        }
        let none = run(
            &session,
            json!({"token": "tok", "user_ok": true, "tainted_ok": true}),
        );
        let need = &none["needs"][0]["dependency"];
        assert_eq!(need, doctor::PACKAGE.name, "{none}");
        assert_eq!(
            server.join().unwrap().len(),
            1,
            "only the first reached the game"
        );
        assert_eq!(saves::snapshot(&dir), user, "nothing written");
        assert!(ledger_rows(&fx, "SELECT id FROM saves").is_empty());
        // No session.
        let out = run(&Session::new(&fx.shared, None), ok);
        let why = out["reason"].as_str().unwrap();
        assert!(why.contains("no live session"), "{out}");
    }

    /// With tainted_ok after a tainting write, the save is made, and tainted in its result and its ledger row; an ASI's
    /// mode switch doesn't taint.
    #[test]
    fn a_tainted_save_needs_tainted_ok_and_is_marked() {
        let fx = served("save-tainted");
        let (session, dir) = session(&fx);
        let live = session.live.as_ref().unwrap();
        for (what, taints) in [("channel teleport", true), ("W3BoneFix.asi mode", false)] {
            let patch = json!({"what": what, "t": "2026-10-19T00:00:00Z", "taints": taints});
            live.patches.borrow_mut().push((SystemTime::now(), patch));
        }
        let server = saving_game(&fx, dir, new_save);
        let args = json!({"token": "tok", "user_ok": true, "tainted_ok": true});
        let out = run(&session, args);
        assert_eq!(out["ok"], true, "{out}");
        assert_eq!(out["tainted"], true, "{out}");
        let by = out["tainted_by"].as_array().unwrap();
        assert_eq!(by.len(), 1, "{out}");
        let sql = "SELECT tainted_by_boot, made_by_boot FROM saves";
        assert_eq!(ledger_rows(&fx, sql), [[json!(1), json!(1)]]);
        server.join().unwrap();
    }
}
