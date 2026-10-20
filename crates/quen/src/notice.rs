//! What the user sees of the agent's state while a session is live (finding 70), so they know when to keep their hands
//! off and when the agent waits for them. The drawing is the game's own notification, modQuen's `notice` verb
//! (CR4GuiManager.ShowNotification for 8 s, quen-w3 0.1.8 or later), the one hands_give shows: 5.01's scripts draw
//! nothing persistent on screen without a Flash module, and a window over the game makes DWM compose it (capture.rs's
//! pin), which would change its present path during a measurement. The game draws the notification into its frames,
//! so quen's own captures and PresentMon windows wait until one is gone ([`clear`]).
//!
//! - **Your controls** (hands_give .. hands_take): hands_give's notice (hands.rs).
//! - **Measuring:** a measure or sweep call that takes [`MEASURING_MIN`] s or more shows one before its first window
//!   ([`measuring`]).
//! - **The agent waiting:** no call from the connection that launched for [`IDLE`], the agent's controls, the game in
//!   front in gameplay or photo mode: quen brings that connection's client forward as `focus` to client does, unless
//!   the config's `idle_focus` is off or it can't; then the notice ([`idle`]). Once per stretch without calls. The
//!   agent's next call of a tool that needs the game in front brings it back first ([`back`]).
//! - **The agent at work** (a call in flight, or within [`IDLE`] of one): nothing shown.

use std::time::{Duration, Instant};

use quen_core::protocol::Verb;
use serde_json::{Value, json};

use crate::channel::REPLY_MAX;
use crate::game::{self, Ctx, Fields};
use crate::hands::Hands;
use crate::session::{Call, Done, Live, Session};
use crate::{boot, doctor, focus, input, measure};

/// How long the agent may make no call, with the controls and the game in front, before quen counts it as waiting
/// for the user.
// ponytail: a guess between an agent's thinking between calls and a question left unseen; a live session tunes it.
pub(crate) const IDLE: Duration = Duration::from_secs(60);
/// The shortest measuring call, in seconds, that shows a notice: a shorter one would wait longer for it to clear than
/// the user needs it.
pub(crate) const MEASURING_MIN: u64 = 30;
/// How long a notice stays in the way: 8 s on screen, then 1 s for the game to take it down.
// ponytail: the fade's length is unmeasured; a screenshot 9 s after a notice checks it.
pub(crate) const CLEAR: Duration = Duration::from_secs(9);

const WAITING: &str = "quen: the agent is waiting for you in the chat (Alt+Tab).";

/// Shows `text` in the game for 8 s: the game's reply, or why it wasn't shown.
pub(crate) fn show(cx: &Ctx, text: &str) -> Result<String, String> {
    let old = doctor::package_version(cx.config).filter(|v| doctor::BEFORE_NOTICE.contains(v));
    if let Some(old) = old {
        return Err(format!(
            "not shown: {old} has no notice (quen-w3 0.1.8 or later)"
        ));
    }
    let channel = cx.channel()?;
    // Before the call: a reply that times out may still come with the notice on screen.
    cx.live.notice_at.set(Some(Instant::now()));
    let verb = Verb::Notice { text: text.into() };
    channel
        .call(verb, REPLY_MAX)
        .map(|reply| reply.text)
        .map_err(|e| e.why().to_string())
}

/// Waits until [`CLEAR`] after quen's last notice, so none is in a capture or a PresentMon window. A cancel ends the
/// wait; the caller's own checks then stop it.
pub(crate) fn clear(live: &Live, call: &mut Call) {
    let left = live
        .notice_at
        .get()
        .map_or(Duration::ZERO, |at| CLEAR.saturating_sub(at.elapsed()));
    if !left.is_zero() {
        measure::settle(call, left, "quen's notice in the game");
        call.lap("notice_clear");
    }
}

/// The measuring notice, before a call's first window, when the agent has the controls and the call takes `secs` or
/// more: the game's reply, or why it wasn't shown. None: no notice for it.
pub(crate) fn measuring(cx: &Ctx, call: &mut Call, secs: u64) -> Option<String> {
    if cx.hands != Some(Hands::Agent) || secs < MEASURING_MIN {
        return None;
    }
    let text = format!(
        "quen: measuring for about {secs} s. Please keep off the mouse and keys until it ends."
    );
    let shown = show(cx, &text).unwrap_or_else(|why| why);
    call.lap("notice");
    Some(shown)
}

/// The session thread, between messages: the agent waiting ([`IDLE`] without a call from the connection that
/// launched, the agent's controls, the game in front in gameplay or photo mode), once per stretch without calls. What
/// it did goes to the daemon's log.
pub(crate) fn idle(session: &Session) {
    let Some(live) = &session.live else {
        return;
    };
    let Some(last) = session.shared.last_call() else {
        return;
    };
    if session.shared.hands() != Some(Hands::Agent)
        || last.elapsed() < IDLE
        || live.idled.get() == Some(last)
    {
        return;
    }
    live.idled.set(Some(last));
    let Ok(config) = session.config() else {
        return;
    };
    let cx = Ctx {
        config,
        live,
        hands: Some(Hands::Agent),
    };
    let front = cx.window().is_ok_and(|(_, w)| input::in_front(w));
    if !front || !cx.last_flags().is_some_and(game::in_game) {
        return;
    }
    let why = match (config.idle_focus(), &live.client) {
        (true, Ok((pid, name))) => match focus::to_client(*pid, name) {
            Ok(_) => {
                live.away.set(true);
                return eprintln!(
                    "quen: no call for {} s with the game in front: {name} brought forward",
                    IDLE.as_secs()
                );
            }
            Err(done) => boot::reason(&done),
        },
        (true, Err(why)) => why.clone(),
        (false, _) => "idle_focus is off".into(),
    };
    let shown = show(&cx, WAITING).unwrap_or_else(|why| why);
    eprintln!(
        "quen: no call for {} s with the game in front, the client not brought forward ({why}); the notice: {shown}",
        IDLE.as_secs()
    );
}

/// Whether `tool` needs the game in front: it sends input, captures or measures frames, or waits for the world to move.
fn needs_front(tool: &str) -> bool {
    match tool {
        "game_scriptlog" | "game_listsaves" | "game_cvar" => false,
        tool => tool.starts_with("game_") || tool.starts_with("measure_") || tool == "sweep_play",
    }
}

/// After [`idle`] brought the client forward: a call of `tool` that needs the game in front, with the session's token,
/// brings it back first, as `focus` to game does, once; never while the user has the controls. What that did
/// (`game_back` in the result), or why it failed: the call's result then.
pub(crate) fn back(
    session: &Session,
    call: &Call,
    tool: &str,
    args: &Value,
) -> Result<Option<Fields>, Done> {
    let Some(live) = session.live.as_ref().filter(|l| l.away.get()) else {
        return Ok(None);
    };
    if !needs_front(tool)
        || args.get("token").and_then(Value::as_str) != Some(&live.token)
        || session.shared.hands() == Some(Hands::User)
    {
        return Ok(None);
    }
    live.away.set(false);
    let cx = Ctx {
        config: session.config()?,
        live,
        hands: session.shared.hands(),
    };
    focus::to_game(&cx, call).map(Some)
}

/// `done` with what [`back`] did, as `game_back`.
pub(crate) fn with_back(done: Done, back: Option<Fields>) -> Done {
    let Some(back) = back else {
        return done;
    };
    let back = ("game_back".to_string(), json!(back));
    match done {
        Done::Ok(mut fields) => {
            fields.insert(back.0, back.1);
            Done::Ok(fields)
        }
        Done::Refused(why) => Done::RefusedWith(why, Fields::from_iter([back])),
        Done::RefusedWith(why, mut fields) => {
            fields.insert(back.0, back.1);
            Done::RefusedWith(why, fields)
        }
        Done::Failed(why, mut fields) => {
            fields.insert(back.0, back.1);
            Done::Failed(why, fields)
        }
        needs @ Done::Needs(..) => needs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::tests::{fake_game, reply, served, token};

    /// The measuring notice: only with the agent's controls and for a call of MEASURING_MIN s or more, its text as the
    /// game gets it, the game's reply back, and quen's captures waiting until CLEAR after it.
    #[test]
    fn measuring_shows_once_for_long_calls_and_captures_wait_for_it() {
        let fx = served("notice-measuring");
        let server = fake_game(
            &token(&fx),
            1,
            Box::new(|r| vec![reply(r.id, true, "notice shown", None)]),
        );
        let session = fx.session();
        let mut call = session.call(crate::mcp::Conn::nobody(), Value::Null, None);
        let live = session.live.as_ref().unwrap();
        let cx = |hands| Ctx {
            config: session.config().unwrap(),
            live,
            hands,
        };
        assert_eq!(measuring(&cx(Some(Hands::User)), &mut call, 60), None);
        assert_eq!(measuring(&cx(Some(Hands::Agent)), &mut call, 29), None);
        assert!(live.notice_at.get().is_none(), "nothing shown");
        let shown = measuring(&cx(Some(Hands::Agent)), &mut call, 30);
        assert_eq!(shown.as_deref(), Some("notice shown"));
        let sent = server.join().unwrap();
        let want =
            "quen: measuring for about 30 s. Please keep off the mouse and keys until it ends.";
        assert_eq!(sent[0].verb, Verb::Notice { text: want.into() });
        // A notice shown 8.7 s ago: the next capture waits the rest of CLEAR, and none after it.
        live.notice_at
            .set(Some(Instant::now() - CLEAR + Duration::from_millis(300)));
        let t = Instant::now();
        clear(live, &mut call);
        assert!(
            t.elapsed() >= Duration::from_millis(250),
            "{:?}",
            t.elapsed()
        );
        let t = Instant::now();
        clear(live, &mut call);
        assert!(t.elapsed() < Duration::from_millis(50), "{:?}", t.elapsed());
    }

    /// After quen brought the client forward, only a call that needs the game in front, with the session's token and the
    /// agent's controls, brings it back, once; its failure is the call's result (the Fake's game shows no window).
    #[test]
    fn the_next_game_call_brings_the_game_back_once() {
        let fx = served("notice-back");
        let session = fx.session();
        let call = session.call(crate::mcp::Conn::nobody(), Value::Null, None);
        let live = session.live.as_ref().unwrap();
        let ok = json!({"token": "tok"});
        // Not away: nothing.
        assert!(matches!(back(&session, &call, "game_keys", &ok), Ok(None)));
        live.away.set(true);
        for (tool, args) in [
            ("world_get", &ok),
            ("game_scriptlog", &ok),
            ("mem_read", &ok),
            ("game_keys", &json!({"token": "nope"})),
        ] {
            assert!(
                matches!(back(&session, &call, tool, args), Ok(None)),
                "{tool}"
            );
            assert!(live.away.get(), "{tool}");
        }
        // The user has the controls: quen moves no window.
        fx.shared.set_hands(Some(Hands::User));
        assert!(matches!(
            back(&session, &call, "game_screenshot", &ok),
            Ok(None)
        ));
        assert!(live.away.get());
        fx.shared.set_hands(Some(Hands::Agent));
        for tool in ["game_screenshot", "measure_ab", "sweep_play"] {
            live.away.set(true);
            let Err(done) = back(&session, &call, tool, &ok) else {
                panic!("{tool}: brought back a game with no window");
            };
            assert!(boot::reason(&done).contains("no visible window"), "{tool}");
            assert!(!live.away.get(), "{tool}: once");
        }
        let back = Some(Fields::from_iter([("menu".into(), json!("closed"))]));
        let Done::RefusedWith(why, fields) = with_back(Done::Refused("no".into()), back) else {
            panic!("a refusal after the bring-back says what it did");
        };
        assert_eq!(
            (why.as_str(), &fields["game_back"]["menu"]),
            ("no", &json!("closed"))
        );
    }
}
