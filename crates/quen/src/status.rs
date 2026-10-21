//! What the user sees of the agent's state while a session is live (finding 70), so they know when to keep their hands
//! off and when the agent waits for them. The drawing is the game's own notification, modQuen's `notice` verb (the one
//! hands_give showed first): in the probe boot (finding 94) it showed in gameplay, in photo mode and over the pause menu.
//! Every state is set at quen's own transitions ([`State`]), none read from the game.
//!
//! **Lasting** (a test session, quen-w3 0.1.15 or later, the config's `indicator` on): one line, up for an hour, sent
//! at each transition and again [`RESEND`] after it was last sent while a state is set: the game's own notifications
//! and its hub menu's key clear it without quen seeing it (the probe boot). modQuen keeps a line that is still up with
//! the same text, so then a re-send changes nothing on screen.
//! - **working:** a call of the connection that launched starts, with the agent's controls ([`start`]);
//! - **measuring:** a measure call, a sweep or a run_plan, from its checks to its end ([`measuring`], [`poll`]);
//! - **yours:** hands_give .. hands_take;
//! - **waiting:** no call from that connection for [`IDLE`] with the agent's controls, until its next call ([`poll`]).
//!
//! **Transitions only** (a play session, `indicator = 'off'`, or a package before 0.1.15): one notice for 8 s at three
//! transitions: hands_give, a measuring call of [`MEASURING_MIN`] s or more, and the agent waiting when quen doesn't
//! bring its client forward.
//!
//! Either way the game draws the notice into its frames: before every capture and window quen takes it down and waits
//! [`HIDE_WAIT`] ([`hide`]); the line comes back at quen's next send, which is never inside one: a transition, the
//! session thread between calls ([`poll`]), a run_plan between steps, measure_ab between windows. With a package before
//! 0.1.15 (no `notice.hide`) a capture waits until 9 s after the notice instead.
//!
//! Idle focus: [`IDLE`] without a call, the agent's controls, the game in front in gameplay or photo mode: quen brings
//! that connection's client forward as `focus` to client does, unless the config's `idle_focus` is off or it can't. The
//! agent's next call of a tool that needs the game in front brings it back first ([`back`]).

use std::cell::{Cell, OnceCell};
use std::time::{Duration, Instant, SystemTime};

use quen_core::protocol::{MAX_NOTICE_MS, Verb};
use serde_json::{Value, json};

use crate::channel::{self, Channel, REPLY_MAX};
use crate::game::{self, Ctx, Fields};
use crate::hands::Hands;
use crate::session::{Call, Done, Session, utc_ms};
use crate::{boot, doctor, focus, input, measure};

/// How long the agent may make no call, with the controls and the game in front, before quen counts it as waiting
/// for the user.
// ponytail: a guess between an agent's thinking between calls and a question left unseen; a live session tunes it.
pub(crate) const IDLE: Duration = Duration::from_secs(60);
/// Transitions only: the shortest measuring call, in seconds, that shows a notice.
pub(crate) const MEASURING_MIN: u64 = 30;
/// How often the lasting line is sent again while a state is set (the owner's ~10 s, after the probe boot).
pub(crate) const RESEND: Duration = Duration::from_secs(10);
/// How long a capture or window waits after the game took quen's notice down: its fade read hidden 0.312 s after the
/// hide, a screenshot about 0.8 s after showed none, and windows started about 0.6 s after a hide were no slower than
/// plain ones (the probe boot, checks 1 and 7; one run each).
pub(crate) const HIDE_WAIT: Duration = Duration::from_millis(500);
/// A transition's notice: the package's own 8 s.
const ONCE: Duration = Duration::from_secs(8);
/// After a notice's time, its 0.3 s fade and a margin: until then it may be on screen (the 9 s quen waited before 0.1.15).
const FADE: Duration = Duration::from_secs(1);

/// What the line says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum State {
    Working,
    /// For about this many seconds.
    Measuring(u64),
    Yours,
    Waiting,
}

impl State {
    fn name(self) -> &'static str {
        match self {
            State::Working => "working",
            State::Measuring(_) => "measuring",
            State::Yours => "yours",
            State::Waiting => "waiting",
        }
    }

    fn text(self) -> String {
        match self {
            State::Working => "quen: agent at work. Please keep off the mouse and keys.".into(),
            State::Measuring(secs) => {
                format!("quen: measuring for about {secs} s. Hands off until it ends.")
            }
            State::Yours => {
                "quen: you have the controls. Tell the agent in the chat (Alt+Tab) to take them back.".into()
            }
            State::Waiting => "quen: the agent is waiting for you in the chat (Alt+Tab).".into(),
        }
    }
}

/// The line, on a session's [`crate::session::Live`].
#[derive(Default)]
pub(crate) struct Status {
    /// The lasting line's state; None: no lasting line.
    state: Cell<Option<State>>,
    /// When quen last sent its lasting line. None: not yet, or taken down since.
    sent: Cell<Option<Instant>>,
    /// quen's last notice that may be on screen: its state, when it went out, its time there. None: taken down.
    up: Cell<Option<(State, Instant, Duration)>>,
    /// quen's package in the game, read once ([`doctor::package_version`]).
    package: OnceCell<Option<&'static str>>,
}

/// Tools that capture or measure frames: each takes the line down first, so none is sent just before one.
const CAPTURES: &[&str] = &[
    "game_screenshot",
    "game_frame",
    "measure_presentmon",
    "measure_ab",
    "measure_trace",
    "sweep_play",
];

pub(crate) fn captures(tool: &str) -> bool {
    CAPTURES.contains(&tool)
}

fn package(cx: &Ctx) -> Option<&'static str> {
    *cx.live
        .status
        .package
        .get_or_init(|| doctor::package_version(cx.config))
}

/// Whether the package lacks notice's duration and notice.hide (before quen-w3 0.1.15).
fn old(cx: &Ctx) -> bool {
    package(cx).is_some_and(|v| doctor::BEFORE_STATUS.contains(&v))
}

/// Whether the line lasts: a test session, the config's `indicator` on, and a package with the duration and the hide.
fn lasting(cx: &Ctx) -> bool {
    !cx.live.play && cx.config.indicator() && !old(cx)
}

/// Sends `state`'s line on `held` (a connection the caller holds: the ASI serves one at a time, finding 80) or a new
/// one, for an hour when `lasting`, else the package's 8 s: the game's reply, or why it wasn't shown.
fn show(
    cx: &Ctx,
    held: &mut Option<Channel>,
    state: State,
    lasting: bool,
) -> Result<String, String> {
    if let Some(old) = package(cx).filter(|v| doctor::BEFORE_NOTICE.contains(v)) {
        return Err(format!(
            "not shown: {old} has no notice (quen-w3 0.1.8 or later)"
        ));
    }
    let st = &cx.live.status;
    st.sent.set(Some(Instant::now()));
    let channel = match held.take() {
        Some(channel) => channel,
        None => cx.channel()?,
    };
    let (duration_ms, time) = match lasting {
        true => (
            Some(MAX_NOTICE_MS),
            Duration::from_millis(MAX_NOTICE_MS.into()),
        ),
        false => (None, ONCE),
    };
    let verb = Verb::Notice {
        text: state.text(),
        duration_ms,
    };
    let sent = channel.call(verb, REPLY_MAX);
    // After the call: the game shows the notice on the tick that replies, or, when no reply came, may still show it
    // up to then. A refusal showed nothing; its words are the game's when it gave some.
    let up = Some((state, Instant::now(), time));
    match sent {
        Ok(reply) => {
            st.up.set(up);
            Ok(reply.text)
        }
        Err(e @ channel::Error::Refused(..)) => Err(e.game_text().unwrap_or(e.why()).to_string()),
        Err(e) => {
            st.up.set(up);
            Err(e.why().to_string())
        }
    }
}

/// quen's transition to `state`: lasting, the line says it from now, sent at once when it changed or is due; otherwise
/// the transition's own notice, if it has one (yours and waiting; measuring from [`MEASURING_MIN`] s). None: nothing
/// sent; else the game's reply, or why it wasn't shown. With `call`, its time is `status` in `timing_s`.
fn send(
    cx: &Ctx,
    call: Option<&mut Call>,
    state: State,
    held: &mut Option<Channel>,
) -> Option<Result<String, String>> {
    let lasting = lasting(cx);
    let st = &cx.live.status;
    let due = match lasting {
        true => {
            st.state.replace(Some(state)) != Some(state)
                || st.sent.get().is_none_or(|t| t.elapsed() >= RESEND)
        }
        false => match state {
            State::Working => false,
            State::Measuring(secs) => secs >= MEASURING_MIN,
            State::Yours | State::Waiting => true,
        },
    };
    if !due {
        return None;
    }
    let shown = show(cx, held, state, lasting);
    if let Some(call) = call {
        call.lap("status");
    }
    Some(shown)
}

/// [`send`], its outcome as a result's `status` takes it.
pub(crate) fn set(cx: &Ctx, call: Option<&mut Call>, state: State) -> Option<String> {
    send(cx, call, state, &mut None).map(|r| r.unwrap_or_else(|why| why))
}

/// The lasting line sent again when due: [`RESEND`] after it was last sent, or at once after [`hide`].
pub(crate) fn tick(cx: &Ctx) -> Option<Result<String, String>> {
    send(cx, None, cx.live.status.state.get()?, &mut None)
}

/// A measure call's, a sweep's or a run_plan's transition to measuring for about `secs`, after its checks: only with
/// the agent's controls, and none for a run_plan's step (the plan's covers it). On `held` when the caller holds the
/// connection (taking it), else on a connection of its own.
pub(crate) fn measuring(
    cx: &Ctx,
    call: &mut Call,
    secs: u64,
    held: &mut Option<Channel>,
) -> Option<String> {
    if cx.hands != Some(Hands::Agent) || cx.live.in_plan.get() {
        return None;
    }
    send(cx, Some(call), State::Measuring(secs), held).map(|r| r.unwrap_or_else(|why| why))
}

/// Before `tool` runs on the session thread for the connection that launched, with the agent's controls: it is at
/// work. Not before hands_give, session_end, run_plan or a capture, which set or take down the line themselves.
pub(crate) fn start(session: &Session, call: &mut Call, tool: &str) -> Option<String> {
    let live = session.live.as_ref().filter(|l| l.owner == call.conn.id)?;
    let skip = captures(tool) || matches!(tool, "hands_give" | "session_end" | "run_plan");
    if session.shared.hands() != Some(Hands::Agent) || skip {
        return None;
    }
    let cx = Ctx {
        config: session.config().ok()?,
        live,
        hands: Some(Hands::Agent),
    };
    set(&cx, Some(call), State::Working)
}

/// Before a capture or a window (`window`: a measuring window, which also gets the config's sound cue): takes quen's
/// notice down when it may be on screen and waits [`HIDE_WAIT`] (with a package before 0.1.15, until 9 s after the
/// notice); `status_hide` in `timing_s` and the budget line in the result (`indicator`, [`note`]). Err, nothing to
/// capture or measure: the game didn't confirm the hide, or the call was stopped in the wait.
pub(crate) fn hide(cx: &Ctx, call: &mut Call, window: bool) -> Result<(), String> {
    let st = &cx.live.status;
    let up = st
        .up
        .get()
        .filter(|(_, at, time)| at.elapsed() < *time + FADE);
    if window {
        cue(cx, call);
    }
    let not_hidden = |why: String| {
        format!("quen's line in the game wasn't taken down ({why}): nothing captured or measured")
    };
    let wait = match up {
        Some((_, at, time)) if old(cx) => (time + FADE).saturating_sub(at.elapsed()),
        Some(_) => {
            cx.channel()
                .and_then(|c| {
                    c.call(Verb::NoticeHide {}, REPLY_MAX)
                        .map_err(|e| e.why().to_string())
                })
                .map_err(not_hidden)?;
            HIDE_WAIT
        }
        // The cue comes as long before a window as the hide does.
        None if window && cx.config.window_sound.is_some() => HIDE_WAIT,
        None => Duration::ZERO,
    };
    if up.is_some() {
        st.up.set(None);
        st.sent.set(None);
    }
    note(call, up.map(|(state, ..)| state), wait);
    if wait.is_zero() {
        return Ok(());
    }
    let waited = measure::settle(call, wait, "quen's line in the game");
    call.lap("status_hide");
    match waited {
        true => Ok(()),
        false => Err(format!(
            "{} during the {:.1} s wait for quen's line in the game to be gone: nothing captured or measured",
            call.stop_cause(),
            wait.as_secs_f64()
        )),
    }
}

/// The call's budget line (the design's §7.4): the state shown before its first hide, when the game took it down, how
/// many hides and their waits, the sound cue's reply; the line's own frame cost when shown (the probe boot saw none at
/// 0.1-0.4 ms resolution, so the 0.05 ms cap is unconfirmed), and no claim for it.
fn line<'c>(call: &'c mut Call) -> &'c mut Value {
    call.indicator.get_or_insert_with(|| {
        json!({"shown_before": null, "hidden_at": null, "hides": 0, "hide_wait_s": 0.0,
               "shown_cost_ms_per_frame": "unconfirmed", "evidence": null})
    })
}

fn note(call: &mut Call, before: Option<State>, wait: Duration) {
    let line = line(call);
    if let Some(state) = before {
        if line["hidden_at"].is_null() {
            line["shown_before"] = json!(state.name());
            line["hidden_at"] = json!(utc_ms(SystemTime::now()));
        }
        line["hides"] = json!(line["hides"].as_u64().unwrap_or(0) + 1);
    }
    let total = line["hide_wait_s"].as_f64().unwrap_or(0.0) + wait.as_secs_f64();
    line["hide_wait_s"] = json!((total * 100.0).round() / 100.0);
}

/// The config's sound cue (`window_sound`), at a window's start (with [`hide`]) and end, never inside it: its reply as
/// `sound` in the budget line.
pub(crate) fn cue(cx: &Ctx, call: &mut Call) {
    let Some(event) = &cx.config.window_sound else {
        return;
    };
    let verb = Verb::Sound {
        event: event.clone(),
    };
    let played = cx
        .channel()
        .and_then(|c| c.call(verb, REPLY_MAX).map_err(|e| e.why().to_string()))
        .map_or_else(|why| why, |reply| reply.text);
    line(call)["sound"] = json!(played);
}

/// The session thread, between messages: a measuring call has ended, or a call came since the agent was waiting
/// (working); the agent waiting ([`idle`]); the lasting line sent again when due. A failed send goes to the daemon's
/// log, with the time (UTC).
pub(crate) fn poll(session: &Session) {
    let (Some(live), Ok(config)) = (&session.live, session.config()) else {
        return;
    };
    let cx = Ctx {
        config,
        live,
        hands: session.shared.hands(),
    };
    let last = session.shared.last_call();
    let state = live.status.state.get();
    let measured = matches!(state, Some(State::Measuring(_)));
    if measured || (state == Some(State::Waiting) && last != live.idled.get()) {
        log(send(&cx, None, State::Working, &mut None));
    }
    idle(&cx, last);
    log(tick(&cx));
}

fn log(sent: Option<Result<String, String>>) {
    if let Some(Err(why)) = sent {
        eprintln!(
            "{} quen: the status line in the game wasn't shown: {why}",
            utc_ms(SystemTime::now())
        );
    }
}

/// The agent waiting: [`IDLE`] without a call from the connection that launched (`last`), the agent's controls, once
/// per stretch without calls. Lasting, the line says so. With the game in front in gameplay or photo mode, quen brings
/// that connection's client forward, unless `idle_focus` is off or it can't; then, transitions only, the notice. What
/// it did goes to the daemon's log, with the time (UTC).
fn idle(cx: &Ctx, last: Option<Instant>) {
    let live = cx.live;
    let Some(last) = last else {
        return;
    };
    if cx.hands != Some(Hands::Agent) || last.elapsed() < IDLE || live.idled.get() == Some(last) {
        return;
    }
    live.idled.set(Some(last));
    let line = lasting(cx).then(|| set(cx, None, State::Waiting));
    let front = cx.window().is_ok_and(|(_, w)| input::in_front(w));
    if !front || !cx.last_flags().is_some_and(game::in_game) {
        return;
    }
    let why = match (cx.config.idle_focus(), &live.client) {
        (true, Ok((pid, name))) => match focus::to_client(*pid, name) {
            Ok(_) => {
                live.away.set(true);
                return eprintln!(
                    "{} quen: no call for {} s with the game in front: {name} brought forward",
                    utc_ms(SystemTime::now()),
                    IDLE.as_secs()
                );
            }
            Err(done) => boot::reason(&done),
        },
        (true, Err(why)) => why.clone(),
        (false, _) => "idle_focus is off".into(),
    };
    let shown = line
        .unwrap_or_else(|| set(cx, None, State::Waiting))
        .unwrap_or_else(|| "nothing sent: the line says so already".into());
    eprintln!(
        "{} quen: no call for {} s with the game in front, the client not brought forward ({why}); the notice: {shown}",
        utc_ms(SystemTime::now()),
        IDLE.as_secs()
    );
}

/// Whether `tool` needs the game in front: it sends input, captures or measures frames, or waits for the world to move.
pub(crate) fn needs_front(tool: &str) -> bool {
    match tool {
        "game_scriptlog" | "game_listsaves" | "game_cvar" | "measure_image" => false,
        tool => {
            tool.starts_with("game_")
                || tool.starts_with("measure_")
                || matches!(tool, "sweep_play" | "run_plan")
        }
    }
}

/// After [`idle`] brought the client forward: a call of `tool` that needs the game in front, with the session's token,
/// brings it back first, as `focus` to game does, once; never while the user has the controls. What that did
/// (`game_back` in the result), or why it failed: the call's result then. The daemon's log says which, with the time.
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
    let back = focus::to_game(&cx, call);
    let how = match &back {
        Ok(fields) => format!("the game brought back: {}", json!(fields)),
        Err(done) => format!("bringing the game back failed: {}", boot::reason(done)),
    };
    eprintln!(
        "{} quen: {tool} after idle focus: {how}",
        utc_ms(SystemTime::now())
    );
    back.map(Some)
}

/// `done` with `value` as its field `key` (what [`back`] did as `game_back`, a send's `status`); nothing for None.
pub(crate) fn with(done: Done, key: &str, value: Option<Value>) -> Done {
    let Some(value) = value else {
        return done;
    };
    let field = (key.to_string(), value);
    match done {
        Done::Ok(mut fields) => {
            fields.insert(field.0, field.1);
            Done::Ok(fields)
        }
        Done::Refused(why) => Done::RefusedWith(why, Fields::from_iter([field])),
        Done::RefusedWith(why, mut fields) => {
            fields.insert(field.0, field.1);
            Done::RefusedWith(why, fields)
        }
        Done::Failed(why, mut fields) => {
            fields.insert(field.0, field.1);
            Done::Failed(why, fields)
        }
        needs @ Done::Needs(..) => needs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::tests::{fake_game, fake_game_until, reply, served, token};
    use crate::game::tests::Fake;
    use quen_core::protocol::Request;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread::JoinHandle;

    type Game = (JoinHandle<Vec<Request>>, Arc<AtomicBool>);

    /// The game, for any number of connections until stopped: every verb answered ok.
    fn game(fx: &Fake) -> Game {
        let stop = Arc::new(AtomicBool::new(false));
        let answer = Box::new(|r: &Request| vec![reply(r.id, true, "done", None)]);
        (fake_game_until(&token(fx), answer, stop.clone()), stop)
    }

    fn verbs(game: Game) -> Vec<Verb> {
        game.1.store(true, Ordering::Relaxed);
        game.0.join().unwrap().into_iter().map(|r| r.verb).collect()
    }

    fn lasts(state: State) -> Verb {
        Verb::Notice {
            text: state.text(),
            duration_ms: Some(MAX_NOTICE_MS),
        }
    }

    /// Lasting: each transition sends its state once; the same state again sends nothing until RESEND has passed since
    /// the last send; measuring only with the agent's controls and outside a plan's steps; the waiting state after IDLE
    /// without a call, and working again at the session thread's next look once a call came or a measuring call ended.
    #[test]
    fn the_lasting_line_follows_quen_s_transitions_and_comes_again_every_10_s() {
        let fx = served("status-states");
        let server = game(&fx);
        fx.shared.set_hands(Some(Hands::Agent));
        let session = fx.session();
        let mut call = session.call(crate::mcp::Conn::nobody(), Value::Null, None);
        let live = session.live.as_ref().unwrap();
        let cx = |hands| Ctx {
            config: session.config().unwrap(),
            live,
            hands,
        };
        let agent = cx(Some(Hands::Agent));
        assert_eq!(set(&agent, None, State::Working).as_deref(), Some("done"));
        assert_eq!(set(&agent, None, State::Working), None, "the same state");
        assert!(tick(&agent).is_none(), "not due yet");
        let ago = |d| Some(Instant::now() - d);
        live.status.sent.set(ago(RESEND - Duration::from_secs(1)));
        assert!(tick(&agent).is_none(), "9 s after the last send");
        live.status.sent.set(ago(RESEND));
        assert!(tick(&agent).is_some(), "10 s after: sent again");
        let user = cx(Some(Hands::User));
        assert_eq!(measuring(&user, &mut call, 60, &mut None), None);
        live.in_plan.set(true);
        assert_eq!(
            measuring(&agent, &mut call, 60, &mut None),
            None,
            "a plan's step"
        );
        live.in_plan.set(false);
        let short = measuring(&agent, &mut call, 5, &mut None);
        assert!(short.is_some(), "lasting: no 30 s floor");
        // The session thread between calls: the measuring call has ended.
        poll(&session);
        assert_eq!(live.status.state.get(), Some(State::Working));
        assert!(set(&agent, None, State::Yours).is_some());
        set(&agent, None, State::Working);
        // IDLE without a call: waiting, whatever is in front (the Fake's game has no window: no idle focus), once.
        let last = Instant::now() - IDLE;
        idle(&agent, Some(last));
        assert_eq!(live.idled.get(), Some(last));
        assert_eq!(live.status.state.get(), Some(State::Waiting));
        idle(&agent, Some(last));
        // A call came since (any tool: last_call moved on): working.
        poll(&session);
        assert_eq!(live.status.state.get(), Some(State::Working));
        let want = [
            lasts(State::Working),
            lasts(State::Working),
            lasts(State::Measuring(5)),
            lasts(State::Working),
            lasts(State::Yours),
            lasts(State::Working),
            lasts(State::Waiting),
            lasts(State::Working),
        ];
        assert_eq!(verbs(server), want);
    }

    /// Before a capture or window the line goes down, the capture waits HIDE_WAIT after the game's reply (status_hide in
    /// timing_s, the budget line in the result), and the line comes back at the next tick; nothing to take down, no
    /// wait. A window's sound cue comes as long before it as the hide. A hide the game refuses captures nothing.
    #[test]
    fn the_line_goes_down_before_a_capture_and_comes_back_after() {
        let fx = served("status-hide");
        let server = game(&fx);
        let session = fx.session();
        let live = session.live.as_ref().unwrap();
        let cx = Ctx {
            config: session.config().unwrap(),
            live,
            hands: Some(Hands::Agent),
        };
        set(&cx, None, State::Measuring(40));
        let mut call = session.call(crate::mcp::Conn::nobody(), Value::Null, None);
        let t = Instant::now();
        assert_eq!(hide(&cx, &mut call, false), Ok(()));
        assert!(t.elapsed() >= HIDE_WAIT, "{:?}", t.elapsed());
        let t = Instant::now();
        assert_eq!(
            hide(&cx, &mut call, false),
            Ok(()),
            "a second capture: down already"
        );
        assert!(
            t.elapsed() < Duration::from_millis(100),
            "{:?}",
            t.elapsed()
        );
        let out = session.envelope("game_screenshot", call, Done::Ok(Fields::new()));
        assert!(
            out["timing_s"]["status_hide"].as_f64().unwrap() >= 0.5,
            "{out}"
        );
        let line = &out["indicator"];
        assert_eq!(
            (&line["shown_before"], &line["hides"], &line["hide_wait_s"]),
            (&json!("measuring"), &json!(1), &json!(0.5)),
            "{out}"
        );
        assert!(
            line["hidden_at"].is_string() && line["evidence"].is_null(),
            "{out}"
        );
        assert!(tick(&cx).is_some(), "back after the capture");
        // A window with the config's sound cue: the cue, then the hide and its wait; the cue again at its end.
        let mut config = session.config().unwrap().clone();
        config.window_sound = Some("gui_global_panel_open".into());
        let cx = Ctx {
            config: &config,
            ..cx
        };
        let mut call = session.call(crate::mcp::Conn::nobody(), Value::Null, None);
        assert_eq!(hide(&cx, &mut call, true), Ok(()));
        cue(&cx, &mut call);
        assert_eq!(call.indicator.as_ref().unwrap()["sound"], "done");
        let sound = Verb::Sound {
            event: "gui_global_panel_open".into(),
        };
        let want = [
            lasts(State::Measuring(40)),
            Verb::NoticeHide {},
            lasts(State::Measuring(40)),
            sound.clone(),
            Verb::NoticeHide {},
            sound,
        ];
        assert_eq!(verbs(server), want);
        // The game refuses the hide: nothing to capture.
        let fx = served("status-hide-refused");
        let refuses =
            Box::new(|r: &Request| vec![reply(r.id, r.verb != Verb::NoticeHide {}, "no", None)]);
        let server = fake_game(&token(&fx), 2, refuses);
        let session = fx.session();
        let live = session.live.as_ref().unwrap();
        let cx = Ctx {
            config: session.config().unwrap(),
            live,
            hands: Some(Hands::Agent),
        };
        set(&cx, None, State::Working);
        let mut call = session.call(crate::mcp::Conn::nobody(), Value::Null, None);
        let why = hide(&cx, &mut call, false).unwrap_err();
        let refused = "quen's line in the game wasn't taken down (the game refused the verb";
        assert!(why.starts_with(refused), "{why}");
        assert_eq!(server.join().unwrap().len(), 2);
    }

    /// Transitions only (`indicator = 'off'`, a play session, a package before 0.1.15): 0.1.14's notices, 8 s each, at
    /// hands_give, a measuring call of MEASURING_MIN s or more and the agent waiting; none for working, none sent again.
    /// From 0.1.15 a capture takes one down; before it, the capture waits until 9 s after it (no hide verb).
    #[test]
    fn transitions_only_shows_timed_notices_and_waits_them_out() {
        let fx = served("status-timed");
        let server = game(&fx);
        let session = fx.session();
        let live = session.live.as_ref().unwrap();
        let mut config = session.config().unwrap().clone();
        config.indicator = Some(false);
        let cx = Ctx {
            config: &config,
            live,
            hands: Some(Hands::Agent),
        };
        let mut call = session.call(crate::mcp::Conn::nobody(), Value::Null, None);
        assert_eq!(set(&cx, None, State::Working), None);
        assert_eq!(measuring(&cx, &mut call, 29, &mut None), None);
        assert!(measuring(&cx, &mut call, 30, &mut None).is_some());
        assert!(set(&cx, None, State::Yours).is_some());
        assert!(tick(&cx).is_none(), "no lasting state to send again");
        assert_eq!(hide(&cx, &mut call, false), Ok(()));
        let once = |state: State| Verb::Notice {
            text: state.text(),
            duration_ms: None,
        };
        let want = [
            once(State::Measuring(30)),
            once(State::Yours),
            Verb::NoticeHide {},
        ];
        assert_eq!(verbs(server), want);
        // quen-w3 0.1.14: no hide verb; a notice shown 8.7 s ago keeps the capture waiting the rest of 9 s.
        let fx = served("status-old");
        let session = fx.session();
        let live = session.live.as_ref().unwrap();
        live.status.package.set(Some("quen-w3 0.1.14")).unwrap();
        let cx = Ctx {
            config: session.config().unwrap(),
            live,
            hands: Some(Hands::Agent),
        };
        assert!(!lasting(&cx));
        let at = Instant::now() - ONCE - FADE + Duration::from_millis(300);
        live.status.up.set(Some((State::Yours, at, ONCE)));
        let mut call = session.call(crate::mcp::Conn::nobody(), Value::Null, None);
        let t = Instant::now();
        assert_eq!(hide(&cx, &mut call, false), Ok(()));
        assert!(
            t.elapsed() >= Duration::from_millis(250),
            "{:?}",
            t.elapsed()
        );
        let t = Instant::now();
        assert_eq!(hide(&cx, &mut call, false), Ok(()));
        assert!(t.elapsed() < Duration::from_millis(50), "{:?}", t.elapsed());
    }

    /// Finding 80: a caller holding the game's only connection (sweep_play's first pose) gets the line on it; a second
    /// connection beside it would find every pipe instance busy.
    #[test]
    fn the_line_goes_on_a_held_connection() {
        let fx = served("status-held");
        let server = fake_game(
            &token(&fx),
            1,
            Box::new(|r| vec![reply(r.id, true, "notice shown", None)]),
        );
        let session = fx.session();
        let mut call = session.call(crate::mcp::Conn::nobody(), Value::Null, None);
        let live = session.live.as_ref().unwrap();
        let cx = Ctx {
            config: session.config().unwrap(),
            live,
            hands: Some(Hands::Agent),
        };
        let mut held = Some(cx.channel().unwrap());
        let shown = measuring(&cx, &mut call, 30, &mut held);
        assert_eq!(shown.as_deref(), Some("notice shown"));
        assert!(held.is_none(), "the held connection carried it");
        assert_eq!(server.join().unwrap().len(), 1);
        assert!(live.status.up.get().is_some());
    }

    /// After quen brought the client forward, only a call that needs the game in front, with the session's token and the
    /// agent's controls, brings it back, once; its failure is the call's result (the Fake's game shows no window).
    #[test]
    fn the_next_game_call_brings_the_game_back_once() {
        let fx = served("status-back");
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
        let back = Some(json!({"menu": "closed"}));
        let Done::RefusedWith(why, fields) = with(Done::Refused("no".into()), "game_back", back)
        else {
            panic!("a refusal after the bring-back says what it did");
        };
        assert_eq!(
            (why.as_str(), &fields["game_back"]["menu"]),
            ("no", &json!("closed"))
        );
    }
}
