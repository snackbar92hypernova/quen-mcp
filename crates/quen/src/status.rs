//! What the user sees of the agent's state while a session is live (finding 70), so they know when to keep their hands
//! off and when the agent waits for them. The drawing is the game's own notification, modQuen's `notice` verb (the one
//! hands_give showed first): in the probe boot (finding 94) it showed in gameplay, in photo mode and over the pause menu.
//! Every state is set at quen's own transitions ([`State`]), none read from the game.
//!
//! **Lasting** (quen-w3 0.1.15 or later, the config's `indicator` on): one line, up for an hour, sent at each
//! transition and again [`RESEND`] after it was last sent while a state is set: the game's own notifications and its
//! hub menu's key clear it without quen seeing it (the probe boot). modQuen keeps a line that is still up with the same
//! text, so then a re-send changes nothing on screen. From quen-w3 0.1.16 modQuen draws every lasting line on a HUD
//! movie of its own where the game draws its HUD, which no game notification replaces, and on the notification
//! elsewhere (w3/README.md, HUD line); the replies are the same.
//! - **working:** a call of the connection that launched starts, with the agent's controls ([`start`], hands_take);
//! - **measuring:** a measure call, a session's first sweep_play or a run_plan, from its checks to its end
//!   ([`measuring`], [`poll`]);
//! - **yours:** whenever the user has the controls: a play session from its launch, hands_give .. hands_take ([`poll`]);
//! - **waiting:** no call from that connection for [`IDLE`] with the agent's controls, until its next call ([`poll`]).
//!
//! **Transitions only** (`indicator = 'off'`, or a package before 0.1.15): one notice for 8 s at three transitions:
//! hands_give, a measuring call of [`MEASURING_MIN`] s or more, and the agent waiting when quen doesn't bring its client
//! forward; in a play session also at its launch (the session thread's first look, as the lasting line's first send)
//! and at hands_take (the owner, after the 0.1.16 live check).
//!
//! Either way the game draws the notice into its frames: before every capture and window quen takes it down and waits
//! [`HIDE_WAIT`] ([`hide`]); the line comes back at quen's next send, which is never inside one: a transition, the
//! session thread between calls ([`poll`]), a run_plan between steps, measure_ab between windows. With a package before
//! 0.1.15 (no `notice.hide`) a capture waits until 9 s after the notice instead. A measuring call's line stays up
//! [`MEASURING_SHOWN`] before the call's first hide, counted from when the game answered that it is up ([`measuring`]).
//! Lasting, measure_ab's line names the state of the window that comes next, [`NEXT_SHOWN`] at least ([`next`]).
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
/// How long a measuring call's line is up, at least, before the call's first hide, counted from when the game answered
/// that it is up ([`measuring`]). Once per call, not per window. The owner asked for 3 s after the 0.1.15 live check (up
/// 79 ms before a trace, "a brief flash"). In the 0.1.16 live check 3 s by quen's clock looked like 1 to 2 s in
/// gameplay and 3 s or more in photo mode; the swap's 0.3 s fades (OverlayPopupMenu.as) explain only part, so 2 s more.
// ponytail: the owner's eye in one boot; a live check tunes it.
pub(crate) const MEASURING_SHOWN: Duration = Duration::from_secs(5);
/// How long measure_ab's line naming the next window's state is up, at least, before that window's hide, counted from
/// when the game answered that it is up ([`next`]): the owner's 3 s (2026-10-22), so their eye can be credited to a
/// state.
pub(crate) const NEXT_SHOWN: Duration = Duration::from_secs(3);
/// The longest state label the line names, in bytes: measure_ab's are 1 to 40 letters, digits, _ and -.
const LABEL_MAX: usize = 40;
/// How often [`measuring`] sends its line again until the game answers [`ALREADY`]: past the 0.3 s fade that takes a
/// notification on screen down before the new one shows (OverlayPopupMenu.as showNotification), which a send restarts.
pub(crate) const CONFIRM_EVERY: Duration = Duration::from_millis(500);
/// How many times [`measuring`] asks (2 s): unconfirmed by then, the line's time counts from its first send.
const CONFIRM_TRIES: u32 = 4;
/// modQuen's reply to a line whose text is up already (quen-w3 0.1.15: notificationModule visible, alpha above 0, its
/// tfMessage the same text; from 0.1.16 the same read of its HUD movie when the line is there), so [`confirm`]'s count
/// starts from the element that shows it.
pub(crate) const ALREADY: &str = "notice already shown";
/// A transition's notice: the package's own 8 s.
const ONCE: Duration = Duration::from_secs(8);
/// After a notice's time, its 0.3 s fade and a margin: until then it may be on screen (the 9 s quen waited before 0.1.15).
const FADE: Duration = Duration::from_secs(1);

/// What the line says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum State {
    Working,
    /// For about this many seconds; measure_ab's first window, lasting.
    Measuring(u64, Option<Next>),
    /// measure_ab's next window, lasting: sent by [`next`], never the lasting line's state.
    Next(Next),
    Yours,
    Waiting,
}

/// measure_ab's next window, as the line names it: its state's label, which window (from 1) and of how many.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Next {
    label: [u8; LABEL_MAX],
    len: usize,
    window: usize,
    of: usize,
}

impl Next {
    /// The label's printable ASCII only, cut at [`LABEL_MAX`] bytes: a notice takes no control character, and the
    /// longest line stays within its 160 bytes.
    pub(crate) fn new(label: &str, window: usize, of: usize) -> Next {
        let mut next = Next {
            label: [0; LABEL_MAX],
            len: 0,
            window,
            of,
        };
        let bytes = label.bytes().filter(u8::is_ascii_graphic);
        for (slot, b) in next.label.iter_mut().zip(bytes) {
            *slot = b;
            next.len += 1;
        }
        next
    }
}

impl std::fmt::Display for Next {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let label = std::str::from_utf8(&self.label[..self.len]).unwrap_or_default();
        write!(f, "Next: {label} (window {} of {})", self.window, self.of)
    }
}

impl State {
    fn name(self) -> &'static str {
        match self {
            State::Working => "working",
            State::Measuring(..) => "measuring",
            State::Next(_) => "next",
            State::Yours => "yours",
            State::Waiting => "waiting",
        }
    }

    fn text(self) -> String {
        match self {
            State::Working => "quen: agent at work. Please keep off the mouse and keys.".into(),
            State::Measuring(secs, None) => {
                format!("quen: measuring for about {secs} s. Hands off until it ends.")
            }
            State::Measuring(secs, Some(next)) => {
                format!("quen: measuring for about {secs} s. {next}. Hands off.")
            }
            State::Next(next) => format!("quen: measuring. {next}. Hands off."),
            State::Yours => {
                "quen: you have the controls. Tell the agent in the chat (Alt+Tab) to take them."
                    .into()
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
    /// When the running measuring call's line went up and how long it stays, until the call's first hide
    /// ([`MEASURING_SHOWN`]); measure_ab's next-window line, until its window's ([`NEXT_SHOWN`]).
    measuring_at: Cell<Option<(Instant, Duration)>>,
    /// quen's package in the game, read once ([`doctor::package_version`]).
    package: OnceCell<Option<&'static str>>,
    /// A sweep_play's measuring line went out in this session: later ones send none, so wait for none.
    pub(crate) swept: Cell<bool>,
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

/// Whether the line lasts: the config's `indicator` on, and a package with the duration and the hide.
fn lasting(cx: &Ctx) -> bool {
    cx.config.indicator() && !old(cx)
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
/// the transition's own notice, if it has one (yours and waiting; measuring from [`MEASURING_MIN`] s; working in a play
/// session when the user had the controls: hands_take). None: nothing sent; else the game's reply, or why it wasn't
/// shown. With `call`, its time is `status` in `timing_s`.
fn send(
    cx: &Ctx,
    call: Option<&mut Call>,
    state: State,
    held: &mut Option<Channel>,
) -> Option<Result<String, String>> {
    let lasting = lasting(cx);
    let st = &cx.live.status;
    let before = st.state.replace(Some(state));
    let due = match lasting {
        true => before != Some(state) || st.sent.get().is_none_or(|t| t.elapsed() >= RESEND),
        false => match state {
            State::Working => cx.live.play && before == Some(State::Yours),
            State::Measuring(secs, _) => secs >= MEASURING_MIN,
            State::Next(_) => false,
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
    if !lasting(cx) {
        return None;
    }
    send(cx, None, cx.live.status.state.get()?, &mut None)
}

/// A measure call's, a sweep's or a run_plan's transition to measuring for about `secs`, after its checks: only with
/// the agent's controls, and none for a run_plan's step (the plan's covers it). On `held` when the caller holds the
/// connection (taking it), else on a connection of its own. Once sent, the line's [`MEASURING_SHOWN`] counts from when
/// the game answered that it is up ([`confirm`]). Lasting, it names measure_ab's first window (`next`).
pub(crate) fn measuring(
    cx: &Ctx,
    call: &mut Call,
    secs: u64,
    next: Option<Next>,
    held: &mut Option<Channel>,
) -> Option<String> {
    if cx.hands != Some(Hands::Agent) || cx.live.in_plan.get() {
        return None;
    }
    let next = next.filter(|_| lasting(cx));
    let sent = send(cx, Some(call), State::Measuring(secs, next), held);
    let st = &cx.live.status;
    if let Some((state @ State::Measuring(..), at, _)) = st.up.get() {
        let up = match &sent {
            Some(Ok(_)) => confirm(cx, call, state),
            _ => None,
        };
        st.measuring_at
            .set(Some((up.unwrap_or(at), MEASURING_SHOWN)));
    }
    sent.map(|r| r.unwrap_or_else(|why| why))
}

/// measure_ab, before each window, lasting with the agent's controls: the line names the state that comes next
/// ([`State::Next`]), and stays up [`NEXT_SHOWN`] from the game's answer that it is up ([`confirm`]), the window's apply
/// and settle counted, until its hide ([`held`]); a measuring line still short of its time stays up until then first.
/// Not for a first window the call's measuring line names ([`measuring`]). The sends, the answer and the waits are
/// `status_label` in `timing_s`, summed. Otherwise, the line back for every window after the first, as [`tick`]. Err:
/// the call was stopped in a wait.
pub(crate) fn next(cx: &Ctx, call: &mut Call, next: Next) -> Result<(), String> {
    let st = &cx.live.status;
    if !lasting(cx) || cx.hands != Some(Hands::Agent) {
        if next.window > 1 {
            tick(cx);
        }
        return Ok(());
    }
    if matches!(st.state.get(), Some(State::Measuring(_, Some(named))) if named == next) {
        return Ok(());
    }
    held(cx, call)?;
    let state = State::Next(next);
    let shown = show(cx, &mut None, state, true);
    if let Some((_, at, _)) = st.up.get().filter(|(up, ..)| *up == state) {
        let up = match shown {
            Ok(_) => confirm(cx, call, state),
            Err(_) => None,
        };
        st.measuring_at.set(Some((up.unwrap_or(at), NEXT_SHOWN)));
    }
    Ok(())
}

/// After `state`'s line went out: sends it again every [`CONFIRM_EVERY`], at most [`CONFIRM_TRIES`] times, until the
/// game answers that it is up ([`ALREADY`]); when it did (`status_confirm` in `timing_s`; `status_unconfirmed` when it
/// didn't; `status_label`, summed, for [`next`]'s). A replaced notification fades out before the new one fades in (0.3 s
/// each, OverlayPopupMenu.as), and the 1 to 2 s the owner missed in the 0.1.16 live check may be partly the game's:
/// counting from its answer leaves out the fades and any lag its own state shows. None with a package before 0.1.15,
/// which shows a text again rather than answer so.
fn confirm(cx: &Ctx, call: &mut Call, state: State) -> Option<Instant> {
    if old(cx) {
        return None;
    }
    let lasting = lasting(cx);
    let mut up = None;
    for _ in 0..CONFIRM_TRIES {
        if !measure::settle(call, CONFIRM_EVERY, "quen's measuring line in the game") {
            break;
        }
        match show(cx, &mut None, state, lasting) {
            Ok(reply) if reply == ALREADY => {
                up = Some(Instant::now());
                break;
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    match (state, up) {
        (State::Next(_), _) => call.lap_sum("status_label"),
        (_, Some(_)) => call.lap("status_confirm"),
        (_, None) => call.lap("status_unconfirmed"),
    }
    up
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
/// notice); `status_hide` in `timing_s` and the budget line in the result (`indicator`, [`note`]). First, [`held`].
/// Err, nothing to capture or measure: the game didn't confirm the hide, or the call was stopped in a wait.
pub(crate) fn hide(cx: &Ctx, call: &mut Call, window: bool) -> Result<(), String> {
    held(cx, call)?;
    let st = &cx.live.status;
    let up = up(st);
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
        false => Err(stopped(call, wait, "quen's line in the game to be gone")),
    }
}

/// quen's notice that may be on screen.
fn up(st: &Status) -> Option<(State, Instant, Duration)> {
    st.up
        .get()
        .filter(|(_, at, time)| at.elapsed() < *time + FADE)
}

fn stopped(call: &Call, wait: Duration, what: &str) -> String {
    format!(
        "{} during the {:.1} s wait for {what}: nothing captured or measured",
        call.stop_cause(),
        wait.as_secs_f64()
    )
}

/// A measuring call's line, at the call's first hide or measure_ab's first next-window line, stays up until
/// [`MEASURING_SHOWN`] after it went up ([`measuring`]); a next-window line, at its window's hide, until [`NEXT_SHOWN`]
/// ([`next`]). The wait is
/// `status_shown` in `timing_s` (`status_label` for a next-window line, summed), shorter by what the call did since
/// (measure_ab's apply and settle). Err: the call was stopped in it.
fn held(cx: &Ctx, call: &mut Call) -> Result<(), String> {
    let st = &cx.live.status;
    let Some((at, shown)) = st.measuring_at.take() else {
        return Ok(());
    };
    let key = match up(st) {
        Some((State::Measuring(..), ..)) => "status_shown",
        Some((State::Next(_), ..)) => "status_label",
        _ => return Ok(()),
    };
    let rest = shown.saturating_sub(at.elapsed());
    if rest.is_zero() {
        return Ok(());
    }
    let waited = measure::settle(call, rest, "quen's measuring line in the game");
    call.lap_sum(key);
    if waited {
        return Ok(());
    }
    let what = format!(
        "quen's measuring line to have been up {} s",
        shown.as_secs()
    );
    Err(stopped(call, rest, &what))
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

/// The session thread, between messages: the user has the controls (yours, lasting: a play session from its launch, or
/// controls given back when its launching connection closed or a save was found; transitions only: a play session's
/// launch, once quen's state shows gameplay or photo mode); a measuring call has ended, or a call came since the agent
/// was waiting (working); the agent waiting ([`idle`]); the lasting line sent again when due. A failed send goes to the
/// daemon's log, with the time (UTC).
pub(crate) fn poll(session: &Session) {
    let (Some(live), Ok(config)) = (&session.live, session.config()) else {
        return;
    };
    let cx = Ctx {
        config,
        live,
        hands: session.shared.hands(),
    };
    live.status.measuring_at.set(None);
    let last = session.shared.last_call();
    let state = live.status.state.get();
    let measured = matches!(state, Some(State::Measuring(..)));
    if cx.hands == Some(Hands::User) {
        // Transitions only, once gameplay began: a play session launched without a save is at the main menu.
        let due = || lasting(&cx) || (live.play && cx.last_flags().is_some_and(game::in_game));
        if state != Some(State::Yours) && due() {
            log(send(&cx, None, State::Yours, &mut None));
        }
    } else if measured || (state == Some(State::Waiting) && last != live.idled.get()) {
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
    use quen_core::protocol::{MAX_NOTICE_BYTES, Request};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread::JoinHandle;

    type Game = (JoinHandle<Vec<Request>>, Arc<AtomicBool>);

    /// The game, for any number of connections until stopped: every verb answered ok; a notice whose text is up already
    /// (the last one sent, not taken down since) as modQuen answers it.
    fn game(fx: &Fake) -> Game {
        let stop = Arc::new(AtomicBool::new(false));
        let up = std::sync::Mutex::new(None);
        let answer = Box::new(move |r: &Request| {
            let mut up = up.lock().unwrap();
            let text = match &r.verb {
                Verb::Notice { text, .. } if up.as_ref() == Some(text) => ALREADY,
                Verb::Notice { text, .. } => {
                    *up = Some(text.clone());
                    "done"
                }
                Verb::NoticeHide {} => {
                    *up = None;
                    "done"
                }
                _ => "done",
            };
            vec![reply(r.id, true, text, None)]
        });
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
        assert_eq!(measuring(&user, &mut call, 60, None, &mut None), None);
        live.in_plan.set(true);
        assert_eq!(
            measuring(&agent, &mut call, 60, None, &mut None),
            None,
            "a plan's step"
        );
        live.in_plan.set(false);
        let short = measuring(&agent, &mut call, 5, None, &mut None);
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
            lasts(State::Measuring(5, None)),
            lasts(State::Measuring(5, None)),
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
        set(&cx, None, State::Measuring(40, None));
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
            lasts(State::Measuring(40, None)),
            Verb::NoticeHide {},
            lasts(State::Measuring(40, None)),
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

    /// Transitions only (`indicator = 'off'`, a package before 0.1.15): 0.1.14's notices, 8 s each, at hands_give, a
    /// measuring call of MEASURING_MIN s or more (sent again until the game answers that it is up) and the agent waiting;
    /// none for working in a test session, none sent again. From 0.1.15 a capture takes one down; before it, the capture
    /// waits until 9 s after it (no hide verb).
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
        assert_eq!(measuring(&cx, &mut call, 29, None, &mut None), None);
        assert!(measuring(&cx, &mut call, 30, None, &mut None).is_some());
        assert!(set(&cx, None, State::Yours).is_some());
        assert!(tick(&cx).is_none(), "nothing sent again");
        assert_eq!(
            set(&cx, None, State::Working),
            None,
            "a test session's hands_take"
        );
        assert_eq!(hide(&cx, &mut call, false), Ok(()));
        let once = |state: State| Verb::Notice {
            text: state.text(),
            duration_ms: None,
        };
        let want = [
            once(State::Measuring(30, None)),
            once(State::Measuring(30, None)),
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

    /// The 0.1.15 live check: the measuring line was up 79 ms before a trace; the 0.1.16 one: up 3 s by quen's clock,
    /// 1 to 2 s by the owner's eye in gameplay. The line goes again until the game answers that it is up
    /// (`status_confirm`), and the call's first hide leaves it up until MEASURING_SHOWN after that; `status_shown` is
    /// that wait, shorter by what the call did since (measure_ab's first apply and settle), on every tool. The next
    /// window's hide doesn't wait for it again; a line up long enough already adds nothing.
    #[test]
    fn a_measuring_line_stays_up_its_time_after_the_game_confirms_it() {
        let fx = served("status-shown");
        let server = game(&fx);
        let session = fx.session();
        let live = session.live.as_ref().unwrap();
        let cx = Ctx {
            config: session.config().unwrap(),
            live,
            hands: Some(Hands::Agent),
        };
        let mut call = session.call(crate::mcp::Conn::nobody(), Value::Null, None);
        assert_eq!(
            measuring(&cx, &mut call, 40, None, &mut None).as_deref(),
            Some("done"),
            "the first send's reply"
        );
        let shown = live.status.up.get().unwrap().1;
        // The call's own work before its first window, as measure_ab's settle.
        std::thread::sleep(Duration::from_secs(1));
        call.lap("settle");
        assert_eq!(hide(&cx, &mut call, true), Ok(()));
        assert!(
            shown.elapsed() >= MEASURING_SHOWN + HIDE_WAIT,
            "{:?}",
            shown.elapsed()
        );
        let out = session.envelope("measure_ab", call, Done::Ok(Fields::new()));
        let t = |k: &str| out["timing_s"][k].as_f64().unwrap_or(-1.0);
        assert!((0.5..1.0).contains(&t("status_confirm")), "{out}");
        let up = t("settle") + t("status_shown");
        assert!(
            t("status_shown") < 4.5 && (4.9..5.3).contains(&up),
            "the wait, the settle counted: {out}"
        );
        // The next window: the line back between them, hidden without the wait.
        let mut call = session.call(crate::mcp::Conn::nobody(), Value::Null, None);
        live.status.sent.set(None);
        assert!(tick(&cx).is_some());
        let t = Instant::now();
        assert_eq!(hide(&cx, &mut call, true), Ok(()));
        assert!(
            t.elapsed() < HIDE_WAIT + Duration::from_millis(400),
            "{:?}",
            t.elapsed()
        );
        // A new measuring call whose line went up MEASURING_SHOWN ago: only the hide's own wait.
        poll(&session);
        assert!(measuring(&cx, &mut call, 50, None, &mut None).is_some());
        live.status
            .measuring_at
            .set(Some((Instant::now() - MEASURING_SHOWN, MEASURING_SHOWN)));
        let t = Instant::now();
        assert_eq!(hide(&cx, &mut call, false), Ok(()));
        assert!(
            t.elapsed() < HIDE_WAIT + Duration::from_millis(400),
            "{:?}",
            t.elapsed()
        );
        verbs(server);
    }

    /// The owner, 2026-10-22: measure_ab's line names each window's state. The first window's in the measuring line,
    /// each later one's in a line of its own; a label is cut to printable ASCII and LABEL_MAX bytes, and the longest line
    /// fits a notice.
    #[test]
    fn measure_ab_s_line_names_each_window_s_state() {
        let first = State::Measuring(87, Some(Next::new("m0", 1, 4)));
        assert_eq!(
            first.text(),
            "quen: measuring for about 87 s. Next: m0 (window 1 of 4). Hands off."
        );
        assert_eq!(
            State::Next(Next::new("m3", 2, 4)).text(),
            "quen: measuring. Next: m3 (window 2 of 4). Hands off."
        );
        let cut = Next::new(&format!("a\u{7}b é{}", "x".repeat(50)), 3, 4);
        let want = format!("Next: ab{} (window 3 of 4)", "x".repeat(LABEL_MAX - 2));
        assert_eq!(cut.to_string(), want);
        // measure_ab's longest: a 40-byte label, 12 windows.
        let longest = State::Measuring(u64::MAX, Some(Next::new(&"x".repeat(40), 12, 12)));
        assert!(
            longest.text().len() <= MAX_NOTICE_BYTES,
            "{}",
            longest.text()
        );
        assert_eq!(State::Next(cut).name(), "next");
    }

    /// Lasting: no line before the first window, which the measuring line names; before each later one its own line,
    /// sent again until the game says it is up, then up NEXT_SHOWN from that answer before the window's hide
    /// (`status_label` in `timing_s`, summed); a settle as long adds no wait. In a run_plan's step (the plan's line names
    /// no window) window 1 gets its line too, once the plan's measuring line has been up its MEASURING_SHOWN.
    #[test]
    fn each_later_window_s_state_is_up_3_s_from_the_game_s_answer() {
        let fx = served("status-next");
        let server = game(&fx);
        let session = fx.session();
        let live = session.live.as_ref().unwrap();
        let cx = Ctx {
            config: session.config().unwrap(),
            live,
            hands: Some(Hands::Agent),
        };
        let mut call = session.call(crate::mcp::Conn::nobody(), Value::Null, None);
        let (a1, b2, a3) = (
            Next::new("A", 1, 3),
            Next::new("B", 2, 3),
            Next::new("A", 3, 3),
        );
        assert!(measuring(&cx, &mut call, 40, Some(a1), &mut None).is_some());
        assert_eq!(
            next(&cx, &mut call, a1),
            Ok(()),
            "named by the measuring line"
        );
        let past = |d: Duration| Some((Instant::now() - d, d));
        live.status.measuring_at.set(past(MEASURING_SHOWN));
        assert_eq!(hide(&cx, &mut call, true), Ok(()));
        // Window 2: no settle, so the hide waits out the 3 s.
        assert_eq!(next(&cx, &mut call, b2), Ok(()));
        let answered = Instant::now();
        assert_eq!(hide(&cx, &mut call, true), Ok(()));
        let up = answered.elapsed();
        assert!(up >= NEXT_SHOWN + HIDE_WAIT, "{up:?}");
        // Window 3: its settle took the 3 s.
        assert_eq!(next(&cx, &mut call, a3), Ok(()));
        live.status.measuring_at.set(past(NEXT_SHOWN));
        let t = Instant::now();
        assert_eq!(hide(&cx, &mut call, true), Ok(()));
        let took = t.elapsed();
        assert!(took < HIDE_WAIT + Duration::from_millis(400), "{took:?}");
        let out = session.envelope("measure_ab", call, Done::Ok(Fields::new()));
        let label = out["timing_s"]["status_label"].as_f64().unwrap_or(-1.0);
        assert!(
            (2.0 * CONFIRM_EVERY.as_secs_f64() + NEXT_SHOWN.as_secs_f64() - 0.1..5.0)
                .contains(&label),
            "two answers and one wait: {out}"
        );
        // A run_plan's step: the plan's line, 0.5 s short of its time, stays up for it, then window 1's line goes.
        poll(&session);
        let mut call = session.call(crate::mcp::Conn::nobody(), Value::Null, None);
        assert!(measuring(&cx, &mut call, 90, None, &mut None).is_some());
        live.in_plan.set(true);
        let ago = Instant::now() - (MEASURING_SHOWN - Duration::from_millis(500));
        live.status.measuring_at.set(Some((ago, MEASURING_SHOWN)));
        let t = Instant::now();
        assert_eq!(next(&cx, &mut call, a1), Ok(()));
        let took = t.elapsed();
        assert!(
            took >= Duration::from_millis(500) + CONFIRM_EVERY,
            "{took:?}"
        );
        live.status.measuring_at.set(past(NEXT_SHOWN));
        assert_eq!(hide(&cx, &mut call, true), Ok(()));
        live.in_plan.set(false);
        let want = [
            lasts(State::Measuring(40, Some(a1))),
            lasts(State::Measuring(40, Some(a1))),
            Verb::NoticeHide {},
            lasts(State::Next(b2)),
            lasts(State::Next(b2)),
            Verb::NoticeHide {},
            lasts(State::Next(a3)),
            lasts(State::Next(a3)),
            Verb::NoticeHide {},
            lasts(State::Working),
            lasts(State::Measuring(90, None)),
            lasts(State::Measuring(90, None)),
            lasts(State::Next(a1)),
            lasts(State::Next(a1)),
            Verb::NoticeHide {},
        ];
        assert_eq!(verbs(server), want);
    }

    /// `indicator = 'off'` (and a package before 0.1.15): transitions only, as before: the measuring notice names no
    /// window, and no line goes between windows.
    #[test]
    fn with_the_line_off_measure_ab_names_no_window() {
        let fx = served("status-next-off");
        let server = game(&fx);
        let session = fx.session();
        let mut config = session.config().unwrap().clone();
        config.indicator = Some(false);
        let cx = Ctx {
            config: &config,
            live: session.live.as_ref().unwrap(),
            hands: Some(Hands::Agent),
        };
        let mut call = session.call(crate::mcp::Conn::nobody(), Value::Null, None);
        let shown = measuring(&cx, &mut call, 30, Some(Next::new("A", 1, 4)), &mut None);
        assert!(shown.is_some());
        let t = Instant::now();
        assert_eq!(next(&cx, &mut call, Next::new("B", 2, 4)), Ok(()));
        assert!(
            t.elapsed() < Duration::from_millis(100),
            "{:?}",
            t.elapsed()
        );
        let out = session.envelope("measure_ab", call, Done::Ok(Fields::new()));
        assert!(out["timing_s"].get("status_label").is_none(), "{out}");
        let once = Verb::Notice {
            text: State::Measuring(30, None).text(),
            duration_ms: None,
        };
        assert_eq!(verbs(server), [once.clone(), once]);
    }

    /// A game that never answers "notice already shown": the line goes CONFIRM_TRIES times more
    /// (`status_unconfirmed`), and its time counts from its first send, so the cap adds no wait.
    #[test]
    fn an_unconfirmed_measuring_line_counts_from_its_send() {
        let fx = served("status-unconfirmed");
        let server = fake_game(
            &token(&fx),
            2 + CONFIRM_TRIES as usize,
            Box::new(|r| vec![reply(r.id, true, "notice shown", None)]),
        );
        let session = fx.session();
        let cx = Ctx {
            config: session.config().unwrap(),
            live: session.live.as_ref().unwrap(),
            hands: Some(Hands::Agent),
        };
        let mut call = session.call(crate::mcp::Conn::nobody(), Value::Null, None);
        let t = Instant::now();
        assert!(measuring(&cx, &mut call, 40, None, &mut None).is_some());
        assert_eq!(hide(&cx, &mut call, false), Ok(()));
        let took = t.elapsed();
        assert!(
            took >= MEASURING_SHOWN + HIDE_WAIT
                && took < MEASURING_SHOWN + HIDE_WAIT + Duration::from_millis(600),
            "{took:?}"
        );
        let out = session.envelope("measure_trace", call, Done::Ok(Fields::new()));
        assert!(
            out["timing_s"]["status_unconfirmed"].as_f64().unwrap() >= 2.0,
            "{out}"
        );
        let sent: Vec<Verb> = server.join().unwrap().into_iter().map(|r| r.verb).collect();
        let notices = sent
            .iter()
            .filter(|v| matches!(v, Verb::Notice { .. }))
            .count();
        assert_eq!(
            (notices, sent.last()),
            (1 + CONFIRM_TRIES as usize, Some(&Verb::NoticeHide {}))
        );
    }

    /// A play session, lasting: "yours" from its launch (the session thread's first look with the user's controls), sent
    /// again every 10 s; "agent at work" from hands_take; "yours" again whenever the user has the controls, also when
    /// they went back without hands_give (the launching connection closed). `indicator = 'off'`: transitions only.
    #[test]
    fn a_play_session_shows_whose_controls_from_its_launch() {
        let fx = served("status-play");
        let server = game(&fx);
        let mut session = fx.session();
        session.live.as_mut().unwrap().play = true;
        fx.shared.set_hands(Some(Hands::User));
        poll(&session);
        let live = session.live.as_ref().unwrap();
        assert_eq!(live.status.state.get(), Some(State::Yours));
        poll(&session);
        live.status.sent.set(Some(Instant::now() - RESEND));
        poll(&session);
        // hands_take: the agent at work, until the controls go back.
        fx.shared.set_hands(Some(Hands::Agent));
        let agent = Ctx {
            config: session.config().unwrap(),
            live,
            hands: Some(Hands::Agent),
        };
        assert!(set(&agent, None, State::Working).is_some());
        poll(&session);
        fx.shared.set_hands(Some(Hands::User));
        poll(&session);
        assert_eq!(
            verbs(server),
            [
                lasts(State::Yours),
                lasts(State::Yours),
                lasts(State::Working),
                lasts(State::Yours)
            ]
        );
        let mut off = session.config().unwrap().clone();
        off.indicator = Some(false);
        let cx = Ctx {
            config: &off,
            live,
            hands: Some(Hands::User),
        };
        assert!(!lasting(&cx), "indicator off: 8 s notices only");
    }

    /// The 0.1.16 live check: with the line off a play session said nothing at its launch or at hands_take. Now the 8 s
    /// notice comes at its launch once quen's state shows gameplay (not at the main menu, where a play session without a
    /// save starts), at hands_take and at hands_give; never sent again.
    #[test]
    fn with_the_line_off_a_play_session_says_whose_controls_at_launch_and_take() {
        let fx = served("status-play-off");
        let server = game(&fx);
        let mut config = fx.shared.config().as_ref().clone().unwrap();
        config.indicator = Some(false);
        fx.shared.set_config(Ok(config));
        let mut session = fx.session();
        session.live.as_mut().unwrap().play = true;
        let live = session.live.as_ref().unwrap();
        fx.shared.set_hands(Some(Hands::User));
        let flags = |f: i64| {
            let log = fx.dir.join("game/bin/x64_dx12/W3QuenState.log");
            std::fs::write(log, format!("push flags={f}\n")).unwrap();
        };
        flags(0);
        poll(&session);
        assert_eq!(live.status.state.get(), None, "the main menu: nothing yet");
        flags(1);
        poll(&session);
        assert_eq!(live.status.state.get(), Some(State::Yours));
        live.status.sent.set(Some(Instant::now() - RESEND));
        poll(&session);
        // hands_take, a call's start after it, hands_give.
        fx.shared.set_hands(Some(Hands::Agent));
        let agent = Ctx {
            config: session.config().unwrap(),
            live,
            hands: Some(Hands::Agent),
        };
        assert!(set(&agent, None, State::Working).is_some());
        assert_eq!(set(&agent, None, State::Working), None);
        assert!(set(&agent, None, State::Yours).is_some());
        fx.shared.set_hands(Some(Hands::User));
        poll(&session);
        let once = |state: State| Verb::Notice {
            text: state.text(),
            duration_ms: None,
        };
        assert_eq!(
            verbs(server),
            [once(State::Yours), once(State::Working), once(State::Yours)]
        );
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
        let shown = measuring(&cx, &mut call, 30, None, &mut held);
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
