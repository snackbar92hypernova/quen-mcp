//! Who has the controls of the game, and the hand-overs between the user and the agent. In a play session (session_launch
//! mode play) the user has them from the launch and hands them to the agent to look into something at that spot; in a
//! test session the agent has them and may hand them to the user to set something up. quen keeps who has them (`hands`,
//! in every envelope's state). While the user has them, it refuses every call that sends input, writes to the game or
//! moves the focus, and lets reads run. `hands_take` (the client asks the user on every call) gives the agent the
//! controls, `hands_give` gives them to the user. quen sorts gamesaves at each hand-over: what changed while the user had
//! the controls is theirs (allowed, listed at the end); a save while the agent had them fails that call, and gives the
//! controls back to the user in a play session or ends a test session.

use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::time::SystemTime;

use quen_core::protocol::Verb;
use quen_core::saves::{self, Snapshot};
use serde_json::{Map, Value, json};

use crate::channel::{self, REPLY_MAX};
use crate::doctor;
use crate::game::{self, Fields, flag, schema};
use crate::session::{Call, Done, Live, Session};

/// What the game shows on screen when the user gets the controls (quen-w3 0.1.8's notice verb).
const NOTICE: &str =
    "quen: you have the controls. To hand them back, tell the agent in the chat (Alt+Tab).";

/// Who has the controls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Hands {
    User,
    Agent,
}

impl Hands {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Hands::User => "user",
            Hands::Agent => "agent",
        }
    }
}

/// A session's hand-overs and the saves sorted at each, on its [`Live`].
pub(crate) struct Handovers {
    /// hands_take calls so far: quen's run ids in a play boot are `<session>-h<n>-<tag>`.
    pub(crate) takes: Cell<u32>,
    /// gamesaves as quen last sorted it (the launch, a hand-over, a save the guard found): what changed beyond it is the
    /// user's while they have the controls, unexpected while the agent has them.
    pub(crate) baseline: RefCell<Snapshot>,
    /// The saves sorted so far: the user's, and the ones written while the agent had the controls.
    pub(crate) user: RefCell<BTreeSet<String>>,
    pub(crate) unexpected: RefCell<BTreeSet<String>>,
    /// The session's patch list's length at the last hands_take: the writes after it are this hand-over's.
    pub(crate) since: Cell<usize>,
}

impl Handovers {
    pub(crate) fn new(saves: Snapshot) -> Handovers {
        Handovers {
            takes: Cell::new(0),
            baseline: RefCell::new(saves),
            user: RefCell::default(),
            unexpected: RefCell::default(),
            since: Cell::new(0),
        }
    }
}

pub(crate) fn tools() -> Vec<Value> {
    vec![
        json!({
            "name": "hands_take",
            "description": "Take the controls of the game from the user, once the user asked for it here: in a play \
                session (session_launch mode play), where the user has them from the launch, or in a test session \
                after hands_give. The client asks the user on every call (user_ok is only your word): never route \
                around a denial. While the user has the controls, quen refuses what sends input, writes to the game or \
                moves the focus (game_keys, game_console, game_load, game_pause, game_settime, game_weather, \
                game_teleport, game_save, game_camera with a pose, game_cvar set or restore, mem_patch, mem_unpatch, \
                measure_ab, sweep_play, focus) and runs reads. Answers hand_over (n: in a play session quen's run ids \
                are <session>-h<n>-<tag>) and user_saves (the saves the user made since the last hand-over, theirs). \
                Refused, nothing changed: no user_ok, the agent has them already, the game not running, a wrong token, \
                another connection's session.",
            // Claude Code prompts for this tool on every call, in every permission mode, as for session_launch.
            "_meta": {"anthropic/requiresUserInteraction": true},
            "inputSchema": schema(json!({
                "user_ok": {"type": "boolean", "description": "true only after the user asked in this conversation for you to take the controls, and agreed. Required."},
            }), &["user_ok"]),
        }),
        json!({
            "name": "hands_give",
            "description": "Give the controls of the game to the user: back in a play session, or in a test session \
                for the user to do something in the game. No prompt; the focus stays where it is. applied: memory \
                patches and cvar writes still applied (in a play session, this hand-over's). A play session refuses \
                while there is one, unless keep_writes with the user's OK; a \
                test session gives the controls with them applied. Then quen's pause goes off and a camera path stops \
                (pause, path: the game's replies), the game shows for 8 s that the user has the controls and hands \
                them back in the chat (notice: its reply, or why not; quen-w3 0.1.8+), and the controls go to the \
                user. kept: this hand-over's teleports, settime and weather; unexpected_saves: saves written while \
                the agent had the controls. A save the user makes while holding them is theirs (hands_take's \
                user_saves, the end's saves.user). Refused: the user has them already, a wrong token. Failed: the \
                pause or the path wasn't undone (left); the controls went to the user.",
            "inputSchema": schema(json!({
                "keep_writes": {"type": "boolean", "description": "play session: give the controls back with this hand-over's memory patches and cvar writes still applied, the user having agreed. Default false: refused while any is"},
            }), &[]),
        }),
    ]
}

/// Whether `tool` with `args` may run while the user has the controls: the session's own tools (each refuses by its
/// own rules) and reads. Everything else sends input, writes to the game or moves the focus.
pub(crate) fn user_may(tool: &str, args: &Value) -> bool {
    let has = |key: &str| args.get(key).is_some();
    match tool {
        "game_cvar" => !has("set") && args.get("restore").is_none_or(|r| r == false),
        "game_camera" => !has("pos") && !has("rot") && !has("fov"),
        "game_screenshot" | "game_listsaves" | "world_get" | "measure_presentmon" | "mem_read"
        | "mem_scan" | "mem_follow" | "sweep_save" | "capture_clean" => true,
        tool => tool.starts_with("session_") || tool.starts_with("hands_"),
    }
}

/// Why `tool` is refused while the user has the controls.
pub(crate) fn user_has(tool: &str) -> String {
    format!(
        "the user has the controls: {tool} sends input, writes to the game or moves the focus. Nothing done. Ask the \
         user in the chat; with their OK, hands_take"
    )
}

/// Sorts what changed in gamesaves since the last sort, beyond the game's own saves and quen's, as the user's saves
/// (`user`) or as unexpected ones, and sorts from now on next time. The names it sorted.
pub(crate) fn sort(live: &Live, user: bool) -> Vec<String> {
    let h = &live.handovers;
    let now = saves::snapshot(&live.saves);
    let (_, found) = live.sorted(&now);
    let to = if user { &h.user } else { &h.unexpected };
    to.borrow_mut().extend(found.iter().cloned());
    *h.baseline.borrow_mut() = now;
    found
}

/// `hands_take`.
pub(crate) fn take(session: &Session, call: &mut Call, args: &Value) -> Done {
    taken(session, call, args).map_or_else(|done| done, Done::Ok)
}

fn taken(session: &Session, call: &mut Call, args: &Value) -> Result<Fields, Done> {
    let cx = game::ctx(session, args, &["user_ok"])?;
    let h = &cx.live.handovers;
    if cx.hands != Some(Hands::User) {
        return Err(
            "the agent has the controls already (hands_give gives them to the user)"
                .to_string()
                .into(),
        );
    }
    if !flag(args, "user_ok")? {
        let why = "user_ok: the user asks in this conversation for you to take the controls, and agrees, first";
        return Err(why.to_string().into());
    }
    if cx.live.job.pids().unwrap_or_default().is_empty() {
        let why = match cx.live.play {
            true => "the game isn't running: the session ends by itself",
            false => "the game isn't running: session_end",
        };
        return Err(why.to_string().into());
    }
    let user_saves = sort(cx.live, true);
    h.takes.set(h.takes.get() + 1);
    h.since.set(cx.live.patches.borrow().len());
    session.shared.set_hands(Some(Hands::Agent));
    call.lap("take");
    Ok(Map::from_iter([
        ("hand_over".into(), json!(h.takes.get())),
        ("user_saves".into(), json!(user_saves)),
    ]))
}

/// `hands_give`.
pub(crate) fn give(session: &Session, call: &mut Call, args: &Value) -> Done {
    given(session, call, args).map_or_else(|done| done, Done::Ok)
}

fn given(session: &Session, call: &mut Call, args: &Value) -> Result<Fields, Done> {
    let cx = game::ctx(session, args, &["keep_writes"])?;
    let h = &cx.live.handovers;
    if cx.hands == Some(Hands::User) {
        return Err("the user has the controls already".to_string().into());
    }
    let keep = flag(args, "keep_writes")?;
    let (applied, kept) = {
        let patches = cx.live.patches.borrow();
        let mine = patches.get(h.since.get()..).unwrap_or_default();
        let kept: Vec<Value> = mine
            .iter()
            .filter(|(_, p)| p["write"].is_null() && p["taints"] == true)
            .map(|(_, p)| p["what"].clone())
            .collect();
        // A test session's writes stay applied across its hand-overs: all of them count.
        let writes = if cx.live.play { mine } else { &patches[..] };
        (applied(writes), kept)
    };
    let mut fields = Map::new();
    if !applied.is_empty() {
        fields.insert("applied".into(), json!(applied));
        if cx.live.play && !keep {
            let why = "a memory patch or cvar write of this hand-over is still applied (applied): undo each \
                       (mem_unpatch, game_cvar restore), or pass keep_writes: true with the user's OK. Nothing done";
            return Err(Done::RefusedWith(why.into(), fields));
        }
    }
    fields.insert("kept".into(), json!(kept));
    // quen's pause and a camera path a sweep left end with the hand-over.
    let mut left = Vec::new();
    let undo = [
        (cx.live.path.get(), "path", Verb::CamStop {}),
        (cx.live.paused.get(), "pause", Verb::Pause { on: false }),
    ];
    for (_, what, verb) in undo.into_iter().filter(|(due, ..)| *due) {
        let sent = cx
            .channel()
            .and_then(|c| c.call(verb, REPLY_MAX).map_err(|e| e.why().to_string()));
        match sent {
            Ok(reply) => {
                match what {
                    "pause" => channel::note_pause(cx.live, &reply),
                    _ => cx.live.path.set(false),
                }
                fields.insert(what.into(), json!(reply.text));
            }
            Err(why) => left.push(format!("{what}: {why}")),
        }
        call.lap(what);
    }
    // The user may be in the game when the controls go to them: it says so, and where to hand them back.
    let shown =
        match doctor::package_version(cx.config).filter(|v| doctor::BEFORE_NOTICE.contains(v)) {
            Some(old) => Err(format!(
                "not shown: {old} has no notice (quen-w3 0.1.8 or later)"
            )),
            None => cx.channel().and_then(|c| {
                let verb = Verb::Notice {
                    text: NOTICE.into(),
                };
                c.call(verb, REPLY_MAX).map_err(|e| e.why().to_string())
            }),
        };
    let notice = shown.map_or_else(|why| why, |reply| reply.text);
    fields.insert("notice".into(), json!(notice));
    call.lap("notice");
    fields.insert("unexpected_saves".into(), json!(sort(cx.live, false)));
    session.shared.set_hands(Some(Hands::User));
    if !left.is_empty() {
        fields.insert("left".into(), json!(left));
        let why = "the controls went to the user, but quen's pause or the camera path wasn't undone (left)";
        return Err(Done::Failed(why.into(), fields));
    }
    Ok(fields)
}

/// The memory writes among `patches` (mem_patch and mem_unpatch, game_cvar's, measure_ab's cvars) still applied: per
/// address, the last write's new bytes differ from what the first found there. Each one's last `what`.
fn applied(patches: &[(SystemTime, Value)]) -> Vec<Value> {
    // (address, the first write's old bytes, the last write's new bytes, its what)
    let mut seen: Vec<[&Value; 4]> = Vec::new();
    for (_, patch) in patches {
        let w = &patch["write"];
        if w.is_null() {
            continue;
        }
        match seen.iter_mut().find(|s| *s[0] == w["address"]) {
            Some(s) => (s[2], s[3]) = (&w["new"], &patch["what"]),
            None => seen.push([&w["address"], &w["old"], &w["new"], &patch["what"]]),
        }
    }
    seen.into_iter()
        .filter(|s| s[1] != s[2])
        .map(|s| s[3].clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::tests::Fake;

    /// With the user's hands, input, writes and focus moves are refused, reads run.
    #[test]
    fn with_the_users_hands_only_reads_run() {
        let none = json!({"token": "t"});
        for tool in [
            "game_keys",
            "game_console",
            "game_load",
            "game_pause",
            "game_settime",
            "game_weather",
            "game_teleport",
            "game_save",
            "sweep_play",
            "measure_ab",
            "mem_patch",
            "mem_unpatch",
            "focus",
        ] {
            assert!(!user_may(tool, &none), "{tool}");
        }
        for (tool, args) in [
            ("game_cvar", json!({"name": "X", "set": "1"})),
            ("game_cvar", json!({"name": "X", "restore": true})),
            ("game_camera", json!({"pos": [0, 0, 0], "rot": [0, 0, 0]})),
            ("game_camera", json!({"fov": 40})),
        ] {
            assert!(!user_may(tool, &args), "{tool} {args}");
        }
        for (tool, args) in [
            ("game_cvar", json!({"name": "X"})),
            ("game_cvar", json!({"name": "X", "restore": false})),
            ("game_camera", none.clone()),
        ] {
            assert!(user_may(tool, &args), "{tool} {args}");
        }
        for tool in [
            "game_screenshot",
            "game_listsaves",
            "world_get",
            "measure_presentmon",
            "mem_read",
            "mem_scan",
            "mem_follow",
            "sweep_save",
            "session_end",
            "hands_take",
            "hands_give",
        ] {
            assert!(user_may(tool, &none), "{tool}");
        }
    }

    /// The Fake's session, a play session or a test one, the agent holding the controls since its first hand-over.
    fn handed_over(fx: &Fake, play: bool) -> Session<'_> {
        let mut session = fx.session();
        let live = session.live.as_mut().unwrap();
        live.play = play;
        live.handovers.takes.set(1);
        fx.shared.set_hands(Some(Hands::Agent));
        session
    }

    fn run(session: &Session, tool: &str, args: Value) -> Value {
        let mut call = session.call(crate::mcp::Conn::nobody(), Value::Null, None);
        let done = match tool {
            "hands_take" => take(session, &mut call, &args),
            _ => give(session, &mut call, &args),
        };
        session.envelope(tool, call, done)
    }

    fn patch(session: &Session, what: &str, write: Value) {
        let taints = !write.is_null() || what.starts_with("channel");
        let p = json!({"what": what, "t": 0, "write": write, "taints": taints});
        let live = session.live.as_ref().unwrap();
        live.patches.borrow_mut().push((SystemTime::now(), p));
    }

    /// In a play session hands_give refuses while a memory patch or cvar write of this hand-over is applied, unless
    /// keep_writes; an undone one doesn't count, an earlier hand-over's isn't its own, and a teleport is listed as kept.
    #[test]
    fn hands_give_in_a_play_session_refuses_while_a_write_of_the_hand_over_is_applied() {
        let fx = Fake::new("play-return");
        let session = handed_over(&fx, true);
        let tok = json!({"token": "tok"});
        patch(
            &session,
            "an earlier hand-over's",
            json!({"address": 7, "old": "00", "new": "01"}),
        );
        session.live.as_ref().unwrap().handovers.since.set(1);
        patch(
            &session,
            "cvar X set 1 at 0x10",
            json!({"address": 16, "old": "00", "new": "01"}),
        );
        patch(&session, "channel {\"verb\":\"teleport\"}", Value::Null);
        patch(
            &session,
            "mem_patch cull at 0x20",
            json!({"address": 32, "old": "aa", "new": "bb"}),
        );
        patch(
            &session,
            "mem_unpatch cull at 0x20",
            json!({"address": 32, "old": "bb", "new": "aa"}),
        );
        let out = run(&session, "hands_give", tok.clone());
        assert_eq!(out["refused"], true, "{out}");
        assert_eq!(out["applied"], json!(["cvar X set 1 at 0x10"]), "{out}");
        assert_eq!(fx.shared.hands(), Some(Hands::Agent), "nothing changed");
        let bad = run(
            &session,
            "hands_give",
            json!({"token": "tok", "keep_writes": "yes"}),
        );
        assert_eq!(bad["refused"], true, "{bad}");
        // Restored: the controls go back, the teleport listed as what the user plays with now.
        patch(
            &session,
            "cvar X back to its value before quen's first write at 0x10",
            json!({"address": 16, "old": "01", "new": "00"}),
        );
        let out = run(&session, "hands_give", tok.clone());
        assert_eq!(out["ok"], true, "{out}");
        assert_eq!(
            out["kept"],
            json!(["channel {\"verb\":\"teleport\"}"]),
            "{out}"
        );
        assert_eq!(out["state"]["hands"], "user", "{out}");
        // The Fake's game serves no channel: the notice says why it isn't shown, and the controls went anyway.
        assert!(
            out["notice"].as_str().unwrap().contains("isn't up"),
            "{out}"
        );
        let again = run(&session, "hands_give", tok.clone());
        assert_eq!(again["refused"], true, "{again}");
        // keep_writes: the controls go back with the write still applied, named.
        fx.shared.set_hands(Some(Hands::Agent));
        patch(
            &session,
            "cvar Y set 2 at 0x30",
            json!({"address": 48, "old": "00", "new": "02"}),
        );
        let out = run(
            &session,
            "hands_give",
            json!({"token": "tok", "keep_writes": true}),
        );
        assert_eq!(out["ok"], true, "{out}");
        assert_eq!(out["applied"][0], "cvar Y set 2 at 0x30", "{out}");
    }

    /// A test session gives the controls with its writes applied, all of the session's named.
    #[test]
    fn hands_give_in_a_test_session_keeps_the_agents_writes() {
        let fx = Fake::new("test-give");
        let session = handed_over(&fx, false);
        patch(
            &session,
            "mem_patch cull at 0x20",
            json!({"address": 32, "old": "aa", "new": "bb"}),
        );
        session.live.as_ref().unwrap().handovers.since.set(1);
        let out = run(&session, "hands_give", json!({"token": "tok"}));
        assert_eq!(out["ok"], true, "{out}");
        assert_eq!(out["applied"], json!(["mem_patch cull at 0x20"]), "{out}");
        assert_eq!(fx.shared.hands(), Some(Hands::User));
    }

    /// hands_take: refused without user_ok and while the agent has the controls; then it sorts the user's saves since
    /// the last hand-over and counts the hand-overs, in a play session and in a test one.
    #[test]
    fn hands_take_takes_the_controls_with_the_users_ok() {
        let fx = Fake::new("play-take");
        let dir = fx.dir.join("gamesaves");
        std::fs::create_dir_all(&dir).unwrap();
        for play in [true, false] {
            let session = handed_over(&fx, play);
            let held = run(
                &session,
                "hands_take",
                json!({"token": "tok", "user_ok": true}),
            );
            assert!(
                held["reason"].as_str().unwrap().contains("already"),
                "{held}"
            );
            fx.shared.set_hands(Some(Hands::User));
            for args in [
                json!({"token": "tok"}),
                json!({"token": "tok", "user_ok": false}),
                json!({"token": "x", "user_ok": true}),
            ] {
                let out = run(&session, "hands_take", args.clone());
                assert_eq!(out["refused"], true, "{args}: {out}");
            }
            assert_eq!(fx.shared.hands(), Some(Hands::User));
            let user = format!("ManualSave_user_{play}.sav");
            std::fs::write(dir.join(&user), "the user's").unwrap();
            std::fs::write(dir.join("AutoSave_1.sav"), format!("the game's {play}")).unwrap();
            let out = run(
                &session,
                "hands_take",
                json!({"token": "tok", "user_ok": true}),
            );
            assert_eq!(out["ok"], true, "{out}");
            assert_eq!(out["hand_over"], 2, "{out}");
            // The session's baseline is the launch's (empty): every manual save so far is the user's.
            assert!(
                out["user_saves"].as_array().unwrap().contains(&json!(user)),
                "{out}"
            );
            assert_eq!(out["state"]["hands"], "agent", "{out}");
        }
    }
}
