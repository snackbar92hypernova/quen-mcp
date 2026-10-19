//! quen's client for its in-game command channel, and the tools that need nothing else:
//! `game_listsaves`, `game_pause`, `game_settime`, `game_weather` and `game_teleport`. game_load and the launch's named
//! save use it too (game.rs, boot.rs). W3Quen.asi serves `\\.\pipe\quen-w3-<token>` only when quen launched the game
//! with that token and quen's pid (session.rs), and only to that pid; quen in turn talks only to the session's game.
//! JSON lines, quen-core's `protocol`. One verb per connection: when quen leaves, the ASI drops what still waits, so a
//! verb quen gave up on never runs late.

use std::io::{ErrorKind, Write as _};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use quen_core::bridge_log::VALID;
use quen_core::protocol::{self, MAX_REPLY_BYTES, Reply, Request, Verb};
use serde_json::{Map, Value, json};

use crate::doctor;
use crate::game::{self, Ctx, Fields, ctx, schema, text};
use crate::session::{Call, Done, Session};
use crate::win::Pipe;

/// How long a reply may take, and each request's ttl_ms (the ASI drops one that waited longer). The game answers on
/// its next tick, about one a frame and at the main menu too (live boot 3), but none come during a loading screen.
pub(crate) const REPLY_MAX: Duration = Duration::from_secs(5);

/// The ASI's reply when the script took a command and never answered it (W3Quen.cpp `Next`): it may have run part-way.
const NO_REPLY: &str = "modQuen gave no reply";

/// The most of the game's text a result carries (a reply may be 64 KiB): what the game, or any script mod, writes is
/// data, never quen's reason.
const GAME_TEXT_MAX: usize = 2048;

/// `text` cut to [`GAME_TEXT_MAX`] bytes on a character boundary, marked when cut.
pub(crate) fn game_text(text: &str) -> String {
    if text.len() <= GAME_TEXT_MAX {
        return text.to_owned();
    }
    let end = (0..=GAME_TEXT_MAX)
        .rev()
        .find(|&i| text.is_char_boundary(i));
    format!(
        "{}... (cut at {GAME_TEXT_MAX} bytes)",
        &text[..end.unwrap_or(0)]
    )
}

/// Why a verb that went out didn't run as asked: quen's own words, and what the game wrote, if it said anything.
#[derive(Debug)]
pub(crate) enum Error {
    /// The game said no and nothing ran: the script refused, or the ASI did (expired, queue full, a request it
    /// couldn't read).
    Refused(String, Option<String>),
    /// Nobody can say what ran: no reply in time, the script's "no reply", the pipe broke, a reply quen can't read.
    Failed(String, Option<String>),
}

impl Error {
    pub(crate) fn why(&self) -> &str {
        match self {
            Error::Refused(why, _) | Error::Failed(why, _) => why,
        }
    }

    /// What the game wrote, capped.
    pub(crate) fn game_text(&self) -> Option<&str> {
        match self {
            Error::Refused(_, text) | Error::Failed(_, text) => text.as_deref(),
        }
    }

    /// As a tool's end, keeping `fields` where the variant has room for them; the game's words go in `game_text`.
    pub(crate) fn done(self, mut fields: Fields) -> Done {
        match self {
            Error::Refused(why, None) => Done::Refused(why),
            Error::Refused(why, Some(text)) => {
                Done::RefusedWith(why, Map::from_iter([("game_text".into(), json!(text))]))
            }
            Error::Failed(why, text) => {
                fields.extend(text.map(|t| ("game_text".to_string(), json!(t))));
                Done::Failed(why, fields)
            }
        }
    }
}

/// A connection to the game's channel, for one verb.
pub(crate) struct Channel(Pipe);

impl Channel {
    /// The channel of the game launched with `token`, served by process `server`. Err, nothing sent: no such pipe (the
    /// ASI not loaded, or not up yet), every instance busy, or another process serving it.
    pub(crate) fn open(token: &str, server: u32) -> Result<Channel, String> {
        let pipe = Pipe::open(&format!(r"\\.\pipe\quen-w3-{token}"))
            .map_err(|e| format!("the game's command channel isn't up: {e}"))?;
        match pipe.peer_pid(false) {
            Ok(pid) if pid == server => Ok(Channel(pipe)),
            Ok(pid) => Err(format!(
                "the command channel's pipe is served by pid {pid}, not the game ({server}): nothing sent"
            )),
            Err(e) => Err(format!("the command channel's server: {e}")),
        }
    }

    /// Sends `verb` and waits up to `timeout` for its reply. Ok: it ran.
    pub(crate) fn call(self, verb: Verb, timeout: Duration) -> Result<Reply, Error> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let ttl_ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
        let line = protocol::encode_request(&Request { id, ttl_ms, verb })
            .map_err(|e| Error::Refused(e.to_string(), None))?;
        (&self.0)
            .write_all(line.as_bytes())
            .map_err(|e| Error::Failed(format!("sending to the game: {e}"), None))?;
        let deadline = Instant::now() + timeout;
        let (mut got, mut buf) = (Vec::new(), vec![0; 64 * 1024]);
        loop {
            while let Some(nl) = got.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = got.drain(..=nl).collect();
                let mut reply =
                    protocol::decode_reply(&String::from_utf8_lossy(&line)).map_err(|e| {
                        let why = "the game's reply doesn't parse (game_text says why)".into();
                        Error::Failed(why, Some(game_text(&e.to_string())))
                    })?;
                reply.text = game_text(&reply.text);
                match reply.id {
                    n if n == id => return judge(reply),
                    0 => {
                        let why =
                            "the game couldn't read the request (its words: game_text)".into();
                        return Err(Error::Refused(why, Some(reply.text)));
                    }
                    _ => {} // ponytail: another request's reply can't come, one verb per connection
                }
            }
            if got.len() > MAX_REPLY_BYTES {
                let why = format!("a reply line over {MAX_REPLY_BYTES} bytes");
                return Err(Error::Failed(why, None));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            match self.0.read_timeout(&mut buf, left) {
                Ok(0) => {
                    let why = "the game closed the channel before it replied";
                    return Err(Error::Failed(why.into(), None));
                }
                Ok(n) => got.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == ErrorKind::TimedOut => {
                    return Err(Error::Failed(
                        format!(
                            "no reply within {} s: the game didn't tick (a loading screen?). quen left, so the game \
                         drops the request, unless the script had taken it already",
                            timeout.as_secs_f32()
                        ),
                        None,
                    ));
                }
                Err(e) => {
                    let why = format!("reading the game's reply: {e}");
                    return Err(Error::Failed(why, None));
                }
            }
        }
    }
}

/// A reply for this request: Ok when it says the verb ran. The game's words go beside quen's reason, never in it.
fn judge(reply: Reply) -> Result<Reply, Error> {
    match reply {
        r if r.ok => Ok(r),
        r if r.text.starts_with(NO_REPLY) => Err(Error::Failed(
            "modQuen took the verb and gave no reply: it may have run part-way".into(),
            Some(r.text),
        )),
        r => Err(Error::Refused(
            "the game refused the verb (its words: game_text)".into(),
            Some(r.text),
        )),
    }
}

impl Ctx<'_> {
    /// The session's game's channel ([`Channel::open`]), its server checked to be the game in the session's job.
    pub(crate) fn channel(&self) -> Result<Channel, String> {
        #[cfg(test)]
        if let Some(pid) = tests::SERVER.get() {
            return Channel::open(&self.live.channel, pid);
        }
        Channel::open(&self.live.channel, self.game_pid()?)
    }
}

pub fn tools() -> Vec<Value> {
    let needs = "Through quen's command channel into the game: needs quen's in-game package (W3Quen.asi + modQuen) \
        and a game quen launched. Refused, nothing sent: wrong token, the package missing (with needs), the channel \
        not up";
    let game = "no game loaded (quen's state shows the main menu or a load)";
    let taints = "Journaled, and every save the game writes after it counts as tainted";
    let vec3 = |what: &str| json!({"type": "array", "items": {"type": "number"}, "minItems": 3, "maxItems": 3, "description": what});
    vec![
        json!({
            "name": "game_listsaves",
            "description": format!("List the saves the game itself sees (its own list: bare lowercase names, \
                cloud-only entries too), wherever the game ticks: gameplay, photo mode, the pause and main menus, \
                not a loading screen. {needs}. Failed: no reply within 5 s."),
            "inputSchema": schema(json!({}), &[]),
        }),
        json!({
            "name": "game_pause",
            "description": format!("Pause or unpause the game world under quen's own pause reason: menus and \
                other pauses keep theirs, on twice is on, off once is off. The reply says whether quen's pause is on \
                and whether the game is paused. {needs}, or {game}."),
            "inputSchema": schema(json!({"on": {"type": "boolean", "description": "true pauses, false unpauses"}}), &["on"]),
        }),
        json!({
            "name": "game_settime",
            "description": format!("Set the game's clock to day and hour, as the vanilla console's settime does. \
                Photo mode's time slider overrides it while photo mode is open. {taints}. {needs}, or {game}."),
            "inputSchema": schema(json!({
                "day": {"type": "integer", "minimum": 0, "description": "the game's day count (settime's first argument)"},
                "hour": {"type": "integer", "minimum": 0, "maximum": 23, "description": "0 to 23"},
            }), &["day", "hour"]),
        }),
        json!({
            "name": "game_weather",
            "description": format!("Change the weather, blending over 1 s, as the vanilla console's \
                changeweather does: only a name the current area's list holds, e.g. WT_Clear. An unknown name is \
                refused with the area's list. {taints}. {needs}, or {game}."),
            "inputSchema": schema(json!({"name": {"type": "string", "description": "a weather name, e.g. WT_Clear"}}), &["name"]),
        }),
        json!({
            "name": "world_get",
            "description": format!("Read where the player is and what the world is: player.pos ([x, y, z]: \
                thePlayer.GetWorldPosition()) and player.heading (GetHeading()), time.day and time.hour \
                (GameTimeDays and GameTimeHours of theGame.GetGameTime(): game_settime's arguments) and weather \
                (GetWeatherConditionName()). Changes nothing: not journaled, taints no save. Runs while the user has \
                the controls in a play session too. {needs}; with needs: quen-w3 0.1.4 or older, or {game}, or no \
                player. Failed: no reply within 5 s."),
            "inputSchema": schema(json!({}), &[]),
        }),
        json!({
            "name": "game_teleport",
            "description": format!("Place the player at pos facing rot (the units the game's scripts use, as a \
                camera pose reads; z snaps to the ground). {taints}. {needs}, or {game}, or no player."),
            "inputSchema": schema(json!({
                "pos": vec3("[x, y, z] in world units"),
                "rot": vec3("[pitch, yaw, roll] in degrees; default [0, 0, 0]"),
            }), &["pos"]),
        }),
    ]
}

/// One of [`tools`]: the verb from its arguments, checked as the protocol checks it; the package and (but for
/// listsaves) a loaded game; the journal for the verbs that taint saves; then the verb and its reply.
pub(crate) fn call(
    session: &Session,
    call: &mut Call,
    name: &str,
    args: &Value,
) -> Result<Fields, Done> {
    let known: &[&str] = match name {
        "game_pause" => &["on"],
        "game_settime" => &["day", "hour"],
        "game_weather" => &["name"],
        "game_teleport" => &["pos", "rot"],
        _ => &[],
    };
    let cx = ctx(session, args, known)?;
    let verb = match name {
        "game_listsaves" => Verb::ListSaves {},
        "world_get" => Verb::WorldGet {},
        "game_pause" => Verb::Pause {
            on: args["on"]
                .as_bool()
                .ok_or("on must be true or false".to_string())?,
        },
        "game_settime" => Verb::SetTime {
            day: whole(args, "day", i32::MAX as u32)?,
            hour: whole(args, "hour", 23)?,
        },
        "game_weather" => Verb::Weather {
            name: text(args, "name")?.into(),
        },
        "game_teleport" => Verb::Teleport {
            pos: vec3(args, "pos")?.ok_or("pos must be [x, y, z]".to_string())?,
            rot: vec3(args, "rot")?.unwrap_or_default(),
        },
        _ => return Err(Done::Refused(format!("unknown tool {name:?}"))),
    };
    let request = Request {
        id: 1,
        ttl_ms: 0,
        verb: verb.clone(),
    };
    protocol::encode_request(&request).map_err(|e| e.to_string())?;
    doctor::installed(&[&doctor::PACKAGE], cx.config, name)?;
    if name == "world_get" {
        doctor::package_at_least(cx.config, doctor::BEFORE_WORLD, "world_get", "0.1.5")?;
    }
    let last = cx.last_flags();
    if name != "game_listsaves" && last.is_none_or(|f| f & VALID == 0) {
        let why = format!(
            "quen's state shows {}: {name} needs a loaded game. Nothing sent",
            game::shown(last)
        );
        return Err(why.into());
    }
    let channel = cx.channel()?;
    call.lap("check");
    let mut fields = Map::new();
    let taints = matches!(
        verb,
        Verb::SetTime { .. } | Verb::Weather { .. } | Verb::Teleport { .. }
    );
    let journal = match taints {
        true => {
            let what = format!(
                "channel {}",
                serde_json::to_string(&verb).unwrap_or_default()
            );
            Some(cx.journal(&what, None, true)?)
        }
        false => None,
    };
    let reply = channel.call(verb, REPLY_MAX);
    call.lap("reply");
    // A verb the game refused never ran (feature tests 2c).
    if let Some((mut journal, id)) = journal {
        let ran = !matches!(reply, Err(Error::Refused(..)));
        let note = cx.settle(&mut journal, id, ran);
        fields.extend(note.map(|n| ("journal_note".to_string(), json!(n))));
    }
    match reply {
        Ok(reply) => {
            if name == "game_pause" {
                note_pause(cx.live, &reply);
            }
            fields.insert("reply".into(), json!(reply.text));
            // Only listsaves and world.get read back.
            let key = if name == "world_get" {
                "world"
            } else {
                "saves"
            };
            fields.extend(reply.data.map(|data| (key.to_string(), data)));
            Ok(fields)
        }
        Err(e) => Err(e.done(fields)),
    }
}

/// Keeps quen's pause as a `pause` verb's reply says it (modQuen's "quen's pause is on" / "off").
pub(crate) fn note_pause(live: &crate::session::Live, reply: &Reply) {
    live.paused
        .set(reply.text.starts_with("quen's pause is on"));
}

/// A whole-number argument, 0 to `max`.
fn whole(args: &Value, key: &str, max: u32) -> Result<u32, String> {
    args[key]
        .as_u64()
        .filter(|n| *n <= u64::from(max))
        .map(|n| n as u32)
        .ok_or(format!("{key} must be a whole number from 0 to {max}"))
}

/// An optional `[a, b, c]` argument of numbers.
fn vec3(args: &Value, key: &str) -> Result<Option<[f32; 3]>, String> {
    let Some(v) = args.get(key) else {
        return Ok(None);
    };
    let n: Option<Vec<f32>> = v
        .as_array()
        .filter(|a| a.len() == 3)
        .and_then(|a| a.iter().map(|x| x.as_f64().map(|x| x as f32)).collect());
    let n = n.ok_or(format!("{key} must be three numbers"))?;
    Ok(Some([n[0], n[1], n[2]]))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::game::tests::{Fake, PARTS};
    use std::cell::Cell;
    use std::fs;
    use std::io::Read;
    use std::thread::{self, JoinHandle};

    thread_local! {
        /// The pid the channel's server must have in unit tests: the test's own, where its stand-in serves. Unset, it
        /// is the game's.
        pub(crate) static SERVER: Cell<Option<u32>> = const { Cell::new(None) };
    }

    pub(crate) type Answer = Box<dyn Fn(&Request) -> Vec<String> + Send>;

    /// A stand-in for W3Quen.asi's pipe on `token`, in a thread, for `conns` connections: it checks each client is this
    /// process (as the ASI checks quen's pid), then sends `answer`'s lines for each request line (none: silence) until
    /// the client leaves. Joined, it gives the requests it read. Its first instance is up when this returns.
    pub(crate) fn fake_game(token: &str, conns: usize, answer: Answer) -> JoinHandle<Vec<Request>> {
        let name = format!(r"\\.\pipe\quen-w3-{token}");
        let first = Pipe::create(&name, true).unwrap();
        thread::spawn(move || {
            let (mut next, mut seen) = (first, Vec::new());
            for _ in 0..conns {
                let pipe = next;
                let start = Instant::now();
                assert!(pipe.accept(|| start.elapsed().as_secs() > 10).unwrap());
                // The next instance is up before this client leaves, so the next open never finds none.
                next = Pipe::create(&name, false).unwrap();
                assert_eq!(pipe.peer_pid(true).unwrap(), std::process::id());
                let (mut got, mut buf) = (Vec::new(), [0; 4096]);
                loop {
                    let n = (&pipe).read(&mut buf).unwrap();
                    if n == 0 {
                        break;
                    }
                    got.extend_from_slice(&buf[..n]);
                    while let Some(nl) = got.iter().position(|&b| b == b'\n') {
                        let line: Vec<u8> = got.drain(..=nl).collect();
                        let line = std::str::from_utf8(&line).unwrap();
                        let request = protocol::decode_request(line).unwrap();
                        for reply in answer(&request) {
                            (&pipe).write_all(reply.as_bytes()).unwrap();
                        }
                        seen.push(request);
                    }
                }
            }
            seen
        })
    }

    /// A reply line as W3Quen.cpp writes one.
    pub(crate) fn reply(id: u64, ok: bool, text: &str, data: Option<Value>) -> String {
        let reply = Reply {
            id,
            ok,
            text: text.into(),
            data,
        };
        protocol::encode_reply(&reply).unwrap()
    }

    fn open(token: &str) -> Channel {
        Channel::open(token, std::process::id()).unwrap()
    }

    /// A reply may be 64 KiB of any script mod's text; a result carries at most 2 KiB of it, cut on a character
    /// boundary and marked.
    #[test]
    fn game_text_is_capped_on_a_character_boundary() {
        assert_eq!(game_text("short"), "short");
        let long = "é".repeat(5000);
        let cut = game_text(&long);
        assert!(cut.ends_with("... (cut at 2048 bytes)"), "{cut}");
        assert_eq!(cut.len(), 2048 + "... (cut at 2048 bytes)".len());
    }

    /// Every reply form the game sends: the script's ok, read-back and refusal, and the ASI's own: expired, queue full,
    /// no reply, and a refusal with id 0. Another request's line is skipped.
    #[test]
    fn every_reply_form_the_game_sends() {
        // The stand-in answers each load by its save name.
        let server = fake_game(
            "replies",
            7,
            Box::new(|r| {
                let Verb::Load { save } = &r.verb else {
                    unreachable!()
                };
                let (id, ok, text, data) = match save.as_str() {
                    "ok" => (r.id, true, "loading the save matching ok", None),
                    "data" => (r.id, true, "2 saves", Some(json!(["a", "b"]))),
                    "none" => (r.id, false, "not exactly one save matches none", None),
                    "expired" => (r.id, false, "expired: waited 6000 ms, ttl 5000 ms", None),
                    "full" => (r.id, false, "refused: 16 commands already wait", None),
                    "lost" => (r.id, false, "modQuen gave no reply (a script error?)", None),
                    _ => (0, false, "refused: malformed message: no id", None),
                };
                vec![
                    reply(r.id + 1000, true, "another request's", None),
                    reply(id, ok, text, data),
                ]
            }),
        );
        let send = |save: &str| {
            let verb = Verb::Load { save: save.into() };
            open("replies").call(verb, REPLY_MAX)
        };
        assert_eq!(send("ok").unwrap().text, "loading the save matching ok");
        assert_eq!(send("data").unwrap().data, Some(json!(["a", "b"])));
        for (save, ran) in [
            ("none", "no"),
            ("expired", "no"),
            ("full", "no"),
            ("zero", "no"),
            ("lost", "unknown"),
        ] {
            match (send(save), ran) {
                (Err(Error::Refused(..)), "no") | (Err(Error::Failed(..)), "unknown") => {}
                (out, _) => panic!("{save}: {out:?}"),
            }
        }
        assert_eq!(server.join().unwrap().len(), 7);
    }

    /// The client gives up after its timeout, quickly, and says so; a game that closes the channel is a failure too.
    #[test]
    fn a_silent_or_closing_game_fails_in_time() {
        let server = fake_game("silent", 1, Box::new(|_| Vec::new()));
        let start = Instant::now();
        let out = open("silent").call(Verb::Pause { on: true }, Duration::from_millis(300));
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "{:?}",
            start.elapsed()
        );
        assert!(
            matches!(&out, Err(Error::Failed(why, None)) if why.contains("no reply within 0.3 s")),
            "{out:?}"
        );
        let seen = server.join().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].ttl_ms, 300, "the ttl is the timeout");

        let pipe = Pipe::create(r"\\.\pipe\quen-w3-closing", true).unwrap();
        let server = thread::spawn(move || {
            pipe.accept(|| false).unwrap();
            let mut buf = [0; 64];
            let _ = (&pipe).read(&mut buf);
        });
        let out = open("closing").call(Verb::ListSaves {}, REPLY_MAX);
        assert!(
            matches!(&out, Err(Error::Failed(why, None)) if why.contains("closed the channel")),
            "{out:?}"
        );
        server.join().unwrap();
    }

    /// The channel is the game's only: another token finds no pipe, and a pipe another process serves is refused.
    #[test]
    fn opens_only_the_session_s_pipe_from_the_game() {
        let down = Channel::open("no-such-channel", std::process::id())
            .err()
            .unwrap();
        assert!(down.contains("isn't up"), "{down}");
        let server = fake_game("squatter", 1, Box::new(|_| Vec::new()));
        let other = Channel::open("squatter", std::process::id() + 4)
            .err()
            .unwrap();
        assert!(other.contains("not the game"), "{other}");
        assert!(server.join().unwrap().is_empty(), "nothing was sent");
    }

    /// A session whose channel this test serves, its state showing gameplay.
    pub(crate) fn served(test: &str) -> Fake {
        SERVER.set(Some(std::process::id()));
        let fx = Fake::new(test);
        let log = fx.dir.join("game/bin/x64_dx12/W3QuenState.log");
        fs::write(log, "push flags=1\n").unwrap();
        fx
    }

    pub(crate) fn token(fx: &Fake) -> String {
        fx.session().live.unwrap().channel
    }

    /// One session for several calls, so its patch list carries over.
    fn run(session: &mut Session, tool: &str, args: Value) -> Value {
        let mut call = session.call(crate::mcp::Conn::nobody(), Value::Null, None);
        let done = game::call(session, &mut call, tool, &args);
        session.envelope(tool, call, done)
    }

    fn journal(fx: &Fake) -> String {
        let config = fx.shared.config().as_ref().clone().unwrap();
        fs::read_to_string(crate::session::journal_path(&config)).unwrap()
    }

    #[test]
    fn the_tools_send_their_verbs_and_journal_the_ones_that_taint() {
        let fx = served("channel-tools");
        let server = fake_game(
            &token(&fx),
            5,
            Box::new(|r| match &r.verb {
                Verb::ListSaves {} => vec![reply(r.id, true, "2 saves", Some(json!(["a", "b"])))],
                Verb::Weather { name } if name == "WT_Nope" => {
                    let why = "unknown weather WT_Nope; this area has: WT_Clear";
                    vec![reply(r.id, false, why, None)]
                }
                _ => vec![reply(r.id, true, "done", None)],
            }),
        );
        let mut session = fx.session();
        let saves = run(&mut session, "game_listsaves", json!({"token": "tok"}));
        assert_eq!(saves["ok"], true, "{saves}");
        assert_eq!(saves["saves"], json!(["a", "b"]));
        let pause = run(
            &mut session,
            "game_pause",
            json!({"token": "tok", "on": true}),
        );
        assert_eq!(pause["reply"], "done", "{pause}");
        let time = json!({"token": "tok", "day": 3, "hour": 22});
        assert_eq!(run(&mut session, "game_settime", time)["ok"], true);
        let tp = json!({"token": "tok", "pos": [1.5, -2, 30]});
        assert_eq!(run(&mut session, "game_teleport", tp)["ok"], true);
        let nope = json!({"token": "tok", "name": "WT_Nope"});
        let nope = run(&mut session, "game_weather", nope);
        assert_eq!(nope["refused"], true, "{nope}");
        // The game's words are in game_text, never in quen's reason.
        assert!(
            nope["game_text"].as_str().unwrap().contains("WT_Clear")
                && !nope["reason"].as_str().unwrap().contains("WT_Clear"),
            "{nope}"
        );
        let verbs: Vec<Verb> = server.join().unwrap().into_iter().map(|r| r.verb).collect();
        assert_eq!(
            verbs,
            [
                Verb::ListSaves {},
                Verb::Pause { on: true },
                Verb::SetTime { day: 3, hour: 22 },
                Verb::Teleport {
                    pos: [1.5, -2.0, 30.0],
                    rot: [0.0; 3]
                },
                Verb::Weather {
                    name: "WT_Nope".into()
                },
            ]
        );
        // settime, the teleport and the weather were journaled first; the two that ran have their done line, the
        // refused weather's step went back off (feature tests 2c). Only what ran taints the saves.
        let journal = journal(&fx);
        assert_eq!(journal.matches("game_memory").count(), 2, "{journal}");
        assert_eq!(journal.matches(r#""rec":"done""#).count(), 2, "{journal}");
        assert!(
            journal.contains(r#"\"verb\":\"settime\""#) && !journal.contains("WT_Nope"),
            "{journal}"
        );
        let patches = session.live.as_ref().unwrap().patches.borrow();
        let what: Vec<&str> = patches
            .iter()
            .map(|(_, p)| p["what"].as_str().unwrap())
            .collect();
        assert_eq!(what.len(), 2, "{what:?}");
        assert!(
            what[0].contains("settime") && what[1].contains("teleport"),
            "{what:?}"
        );
        assert!(patches.iter().all(|(_, p)| p["taints"] == true));
    }

    /// world_get sends world.get and answers the game's read-back as `world`, journaling nothing: it taints no save. While
    /// the user has the controls in a play session it runs too. Without a loaded game nothing is sent.
    #[test]
    fn world_get_reads_the_world_and_changes_nothing() {
        let fx = served("channel-world");
        let world = json!({"player": {"pos": [-101.5, 200.25, 3.0], "heading": 90.5},
                           "time": {"day": 3, "hour": 22}, "weather": "WT_Clear"});
        let data = world.clone();
        let server = fake_game(
            &token(&fx),
            1,
            Box::new(move |r| {
                assert_eq!(r.verb, Verb::WorldGet {});
                vec![reply(r.id, true, "the world", Some(data.clone()))]
            }),
        );
        fx.shared.set_hands(Some(crate::play::Hands::User));
        let mut session = fx.session();
        let out = run(&mut session, "world_get", json!({"token": "tok"}));
        assert_eq!(out["ok"], true, "{out}");
        assert_eq!(out["world"], world, "{out}");
        assert!(out.get("saves").is_none(), "{out}");
        assert!(journal(&fx).is_empty(), "{}", journal(&fx));
        assert!(session.live.as_ref().unwrap().patches.borrow().is_empty());
        assert_eq!(server.join().unwrap().len(), 1);
        let extra = run(&mut session, "world_get", json!({"token": "tok", "x": 1}));
        assert_eq!(extra["refused"], true, "{extra}");
        fs::write(
            fx.dir.join("game/bin/x64_dx12/W3QuenState.log"),
            "push flags=0\n",
        )
        .unwrap();
        let menu = run(&mut session, "world_get", json!({"token": "tok"}));
        let why = menu["reason"].as_str().unwrap();
        assert!(why.contains("needs a loaded game"), "{menu}");
    }

    /// game_load sends the load verb and confirms the load on quen's state: nothing typed, no console to close. A load
    /// the game didn't take in time is refused, nothing typed either.
    #[test]
    fn game_load_goes_through_the_channel() {
        let fx = served("channel-load");
        fs::write(fx.dir.join("docs/gamesaves/QuickSave.sav"), "").unwrap();
        let state = fx.dir.join("game/bin/x64_dx12/W3QuenState.log");
        let answered = std::sync::atomic::AtomicUsize::new(0);
        let server = fake_game(
            &token(&fx),
            2,
            Box::new(move |r| {
                if answered.fetch_add(1, Ordering::SeqCst) > 0 {
                    return vec![reply(
                        r.id,
                        false,
                        "expired: waited 5100 ms, ttl 5000 ms",
                        None,
                    )];
                }
                // The load: its 0, then gameplay.
                let mut log = fs::OpenOptions::new().append(true).open(&state).unwrap();
                log.write_all(b"push flags=0\npush flags=1\n").unwrap();
                vec![reply(
                    r.id,
                    true,
                    "loading the save matching quicksave",
                    None,
                )]
            }),
        );
        let out = fx.run(
            "game_load",
            json!({"token": "tok", "save": "quicksave.sav"}),
        );
        assert_eq!(out["ok"], true, "{out}");
        assert_eq!(out["via"], "channel", "{out}");
        assert_eq!(out["reply"], "loading the save matching quicksave", "{out}");
        assert_eq!(out["flags_after"], json!(["valid"]), "{out}");
        let late = fx.run("game_load", json!({"token": "tok", "save": "QuickSave"}));
        assert_eq!(late["refused"], true, "{late}");
        assert!(
            late["game_text"].as_str().unwrap().starts_with("expired"),
            "{late}"
        );
        let verbs: Vec<Verb> = server.join().unwrap().into_iter().map(|r| r.verb).collect();
        let load = Verb::Load {
            save: "quicksave".into(),
        };
        assert_eq!(verbs[0], load);
        assert!(fx.calls().is_empty(), "typed: {:?}", fx.calls());
    }

    #[test]
    fn the_tools_refuse_before_sending() {
        let fx = served("channel-refuse");
        let server = fake_game(
            &token(&fx),
            1,
            Box::new(|r| vec![reply(r.id, true, "ok", None)]),
        );
        for (tool, args) in [
            ("game_pause", json!({"token": "nope", "on": true})),
            ("game_pause", json!({"token": "tok"})),
            (
                "game_pause",
                json!({"token": "tok", "on": true, "extra": 1}),
            ),
            (
                "game_settime",
                json!({"token": "tok", "day": 1, "hour": 24}),
            ),
            (
                "game_settime",
                json!({"token": "tok", "day": -1, "hour": 2}),
            ),
            ("game_weather", json!({"token": "tok", "name": ""})),
            ("game_teleport", json!({"token": "tok", "pos": [1, 2]})),
            (
                "game_teleport",
                json!({"token": "tok", "pos": [1e39, 0, 0]}),
            ),
            (
                "game_teleport",
                json!({"token": "tok", "pos": [0, 0, 0], "rot": "north"}),
            ),
        ] {
            let out = fx.run(tool, args.clone());
            assert_eq!(out["refused"], true, "{tool} {args}: {out}");
        }
        // No game loaded: the state-changing verbs wait for one; listsaves goes anyway.
        fs::write(
            fx.dir.join("game/bin/x64_dx12/W3QuenState.log"),
            "push flags=0\n",
        )
        .unwrap();
        let menu = fx.run("game_pause", json!({"token": "tok", "on": true}));
        let why = menu["reason"].as_str().unwrap();
        assert!(why.contains("needs a loaded game"), "{menu}");
        let saves = fx.run("game_listsaves", json!({"token": "tok"}));
        assert_eq!(saves["ok"], true, "{saves}");
        assert_eq!(server.join().unwrap().len(), 1, "only listsaves was sent");
        assert!(journal(&fx).is_empty(), "{}", journal(&fx));
        // Without the package: refused with needs.
        for part in PARTS {
            fs::remove_file(fx.dir.join("game").join(part)).unwrap(); // this test's own synthetic files
        }
        let none = fx.run("game_listsaves", json!({"token": "tok"}));
        assert_eq!(
            none["needs"][0]["dependency"],
            doctor::PACKAGE.name,
            "{none}"
        );
    }
}
