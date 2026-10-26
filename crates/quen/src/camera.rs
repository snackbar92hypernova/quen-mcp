//! The camera tools: `game_camera` reads or sets the photo-mode camera by numbers, `sweep_save`
//! stores a camera sweep (quen-core `sweep`) in the ledger with a pose row per keyframe, and `sweep_play` plays a stored
//! one in the game: in real time, or frame step with a capture of every frame. All three game verbs go through quen's
//! command channel (channel.rs). quen sends a path once; the game interpolates it, one camera set per game tick
//! (W3Quen.asi ports quen-core's `sweep::Path`), and in frame step sets each frame when quen asks for it.

use std::path::PathBuf;
use std::thread::sleep;
use std::time::{Duration, Instant};

use quen_core::bridge_log::{VALID, in_photo};
use quen_core::journal::sha256_hex;
use quen_core::ledger::{
    Error as LedgerError, Ledger, PlayerPose, Pose as PoseRow, Sweep as SweepRow, SweepFrame,
    SweepRun,
};
use quen_core::protocol::{Reply, Verb};
use quen_core::sweep::{self, Ease, FOV, Mode, Pose, Sweep};
use serde_json::{Map, Value, json};

use crate::capture;
use crate::channel::{self, REPLY_MAX};
use crate::doctor;
use crate::game::{self, Ctx, Fields, schema, text};
use crate::session::{self, Call, Done, Session};
use crate::status;

/// How long real-time playback is given past the sweep's end before quen reads the camera back.
const END_MARGIN: Duration = Duration::from_millis(500);
/// The longest wait sweep_play's settle_ms takes, per frame.
const SETTLE_MAX_MS: u64 = 10_000;
/// How far a set's read-back may be off what was asked (metres, degrees): beyond, the game kept the camera elsewhere.
/// The f32 round trip costs under a millimetre (feature tests 2: sets read back off 0.0).
pub(crate) const OFF_MAX: (f64, f64) = (0.01, 0.1);

pub fn tools() -> Vec<Value> {
    let vec3 = |what: &str| json!({"type": "array", "items": {"type": "number"}, "minItems": 3, "maxItems": 3, "description": what});
    let ease = |what: &str| json!({"type": "string", "enum": ["linear", "ease_in_out"], "description": what});
    let object = |props: Value, required: &[&str]| json!({"type": "object", "properties": props, "required": required, "additionalProperties": false});
    let mut keyframe = object(
        json!({
            "t": {"type": "number", "description": "seconds from the start; the first keyframe is at 0"},
            "pos": vec3("[x, y, z] in metres"),
            "rot": vec3("[pitch, yaw, roll] in degrees"),
            "fov": {"type": "number", "description": "degrees, 1 to 179"},
            "ease": ease("the segment leaving it, under speed keys; default linear"),
            "capture_id": {"type": "integer", "description": "the game_screenshot capture it was checked on"},
        }),
        &["t", "pos", "rot", "fov"],
    );
    keyframe["description"] = json!("a pose as game_camera reads it, at time t");
    let mut room_pan = object(
        json!({
            "pos": vec3("[x, y, z] in metres"),
            "rot": vec3("[pitch, yaw, roll] in degrees at the start"),
            "fov": {"type": "number", "description": "degrees"},
            "turn": {"type": "number", "description": "degrees of yaw, toward increasing yaw if positive; 360 is a full turn"},
            "keyframes": {"type": "integer", "minimum": 2, "maximum": 24, "description": "evenly spaced, both ends included; a full turn needs 4 or more"},
            "duration": {"type": "number", "description": "seconds"},
            "ease": ease("the whole pan; default linear"),
        }),
        &["pos", "rot", "fov", "turn", "keyframes", "duration"],
    );
    room_pan["description"] = json!("instead of keyframes: a turn in place at constant speed");
    let mut world = object(
        json!({
            "save": {"type": "string", "description": "the save it was made in"},
            "time": object(json!({
                "day": {"type": "integer", "minimum": 0, "description": "game_settime's day"},
                "hour": {"type": "integer", "minimum": 0, "maximum": 23, "description": "0 to 23"},
            }), &["day", "hour"]),
            "weather": {"type": "string", "description": "a weather name, e.g. WT_Clear"},
            "player": object(json!({
                "pos": vec3("[x, y, z] in metres"),
                "yaw": {"type": "number", "description": "degrees"},
            }), &["pos", "yaw"]),
            "paused": {"type": "boolean", "description": "pause the world during real-time playback too"},
        }),
        &[],
    );
    world["description"] = json!("what the sweep was made in; recorded, and paused is applied");
    world["properties"]["time"]["description"] = json!("the game time");
    world["properties"]["player"]["description"] = json!("where the player stood");
    vec![
        json!({
            "name": "game_camera",
            "description": "Read or set the photo-mode camera by numbers. No pos: read the view (camera photomode: \
                the photo-mode camera's own pose; game: the game camera's, outside photo mode). pos and rot (fov \
                optional): set the photo-mode camera, photo mode only, and read it back; off is the largest \
                difference from what was asked (pos_m, deg). rot is [pitch, yaw, roll] in degrees; the camera's \
                forward is (-sin yaw, cos yaw, 0) in world xy, so +yaw turns left (observed once, 2026-10-19). A pose \
                is what sweep_save takes as a keyframe. Photo mode's camera collides with geometry. Not journaled: no save holds the photo-mode camera. Through quen's \
                command channel: needs quen's in-game package. Refused, nothing sent: wrong token, pos without rot, \
                no loaded game, a set outside photo mode (quen's state). Failed: no reply within 5 s, or the read-back \
                off by more than 0.01 m or 0.1 degrees (the game kept the camera short of the pose).",
            "inputSchema": schema(json!({
                "pos": vec3("[x, y, z] in metres: set the camera here"),
                "rot": vec3("[pitch, yaw, roll] in degrees"),
                "fov": {"type": "number", "description": "degrees, 1 to 179; left out: unchanged"},
            }), &[]),
        }),
        json!({
            "name": "sweep_save",
            "description": "Store a camera sweep for cinematics in the ledger: keyframes, or a \
                room_pan. The path curves smoothly through every keyframe, the short way round, the horizon level. \
                speed keys (default): each keyframe at its t, its segment shaped by its ease. constant: the whole \
                path at one speed from 0 to the last t, shaped by the sweep's ease. Stored canonical (angles in \
                (-180, 180], numbers on a 0.0001 grid) with a pose row per keyframe, '<name> k<i>', its \
                ref_capture_id the keyframe's screenshot. A sweep never changes: the same name and sweep again \
                returns its id (existed), another sweep under a taken name is refused. Needs no session. Refused, \
                nothing written: 2 to 24 keyframes, times increasing from 0 to at most 3600 s, pitch within +-89, \
                fov 1 to 179, no segment turning 179 degrees or more (add a keyframe between), no ease where it \
                isn't used, a known capture_id.",
            "inputSchema": object(json!({
                "name": {"type": "string", "description": "unique among sweeps"},
                "keyframes": {"type": "array", "items": keyframe, "minItems": 2, "maxItems": 24, "description": "2 to 24, in time order"},
                "room_pan": room_pan,
                "speed": {"type": "string", "enum": ["keys", "constant"], "description": "default keys"},
                "ease": ease("constant speed's, over the whole path; default linear"),
                "world": world,
                "note": {"type": "string", "description": "free text"},
            }), &["name"]),
        }),
        json!({
            "name": "sweep_play",
            "description": "Play a stored sweep on the photo-mode camera, in photo mode. quen sets the first pose and \
                reads it back (refused, nothing played, if off by over 0.01 m or 0.1 deg: photo mode's collision kept \
                it short), then sends the path once and the game sets the camera every tick. realtime: the call waits \
                it out, then reads the end back (off_end). step: the world paused (quen's pause); frame n set at n / \
                fps, read back (off: the worst miss of quen's own sample), captured as game_screenshot <tag>-<n:05>; \
                a sweep_runs row, a sweep_frames row per frame. The same sweep and fps give the same poses at any \
                frame rate; frames_digest is equal for identical runs. About 0.5 s a frame: tell the user the count \
                first. Only a cancel stops a sweep (job_cancel; call with wait: false); at the end the path stops, \
                the pause lifts (pause: its final state). Needs quen-w3 0.1.2+. Refused, nothing sent: no such \
                sweep, a bad fps, not in photo mode, the tag taken. Failed: the game refused or didn't answer, a \
                capture failed (frames so far kept).",
            "inputSchema": schema(json!({
                "sweep": {"type": "string", "description": "the sweep's name (sweep_save)"},
                "mode": {"type": "string", "enum": ["realtime", "step"], "description": "realtime, or step: frame by frame, each captured"},
                "fps": {"type": "integer", "minimum": 1, "maximum": 240, "description": "step only: frames per second of sweep time. The result's frames_gb_est (also the job's progress before frame 0): about the GB the frames write, the game window's pixels x 1.86 bytes sdr, 4.26 hdr"},
                "method": {"type": "string", "enum": ["sdr", "hdr"], "description": "step: game_screenshot's method (default sdr)"},
                "tag": {"type": "string", "pattern": "^[A-Za-z0-9_-]{1,58}$", "description": "step: names the frames, unique in the session (default sweep<id>-<time>)"},
                "settle_ms": {"type": "integer", "minimum": 0, "maximum": SETTLE_MAX_MS, "description": "step: a wait after each frame is set, for denoisers and TAA to settle (default 0; the capture's pin adds 250 ms)"},
            }), &["sweep", "mode"]),
        }),
    ]
}

/// `sweep_save` and `sweep_play`; game_camera comes through game.rs.
pub(crate) fn call(session: &Session, call: &mut Call, name: &str, args: &Value) -> Done {
    let out = match name {
        "sweep_save" => save(session, call, args),
        "sweep_play" => play(session, call, args),
        _ => Err(Done::Refused(format!("unknown tool {name:?}"))),
    };
    out.map_or_else(|done| done, Done::Ok)
}

pub(crate) fn camera(session: &Session, call: &mut Call, args: &Value) -> Result<Fields, Done> {
    let cx = game::ctx(session, args, &["pos", "rot", "fov"])?;
    let fov = match args.get("fov") {
        None => None,
        Some(v) => Some(
            v.as_f64()
                .filter(|f| FOV.contains(f))
                .ok_or("fov must be a number from 1 to 179".to_string())?,
        ),
    };
    let asked = match (numbers(args, "pos")?, numbers(args, "rot")?) {
        (None, None) if fov.is_none() => None,
        (Some(pos), Some(rot)) => Some(Pose {
            pos,
            rot,
            fov: fov.unwrap_or(f64::NAN),
        }),
        _ => {
            return Err(
                "pos and rot go together: both set the camera, neither reads it"
                    .to_string()
                    .into(),
            );
        }
    };
    let verb = match &asked {
        None => Verb::CamGet {},
        Some(p) => Verb::CamSet {
            pos: p.pos.map(|x| x as f32),
            rot: p.rot.map(|x| x as f32),
            fov: fov.map(|f| f as f32),
        },
    };
    let channel = ready(&cx, "game_camera", asked.is_some())?;
    call.lap("check");
    let reply = send_on(channel, verb, &Map::new())?;
    call.lap("reply");
    let mut fields = Map::new();
    fields.insert("reply".into(), json!(reply.text));
    let (camera, got) = read_back(&reply).map_err(|why| Done::Failed(why, fields.clone()))?;
    fields.insert("camera".into(), json!(camera));
    fields.insert("pose".into(), pose_json(&got));
    if let Some(mut asked) = asked {
        if asked.fov.is_nan() {
            asked.fov = got.fov;
        }
        fields.insert("off".into(), off_json(off(&got, &asked)));
        if let Some(why) = short(&got, &asked) {
            return Err(Done::Failed(why, fields));
        }
    }
    Ok(fields)
}

/// Why the camera isn't at `want`, when its read-back `got` is off by more than [`OFF_MAX`]: photo mode's camera
/// collides with geometry (feature tests 2: a set across a railing stayed 4 m short, its rotation and fov set).
pub(crate) fn short(got: &Pose, want: &Pose) -> Option<String> {
    let (pos, deg) = off(got, want);
    (pos > OFF_MAX.0 || deg > OFF_MAX.1).then(|| {
        format!(
            "the game kept the camera short of the pose (off {pos:.3} m, {deg:.2} deg): photo mode's collision \
             (Noclip Off in its Camera tab) stops it at geometry. Set it along a free path, a step at a time"
        )
    })
}

/// Refused, nothing sent, unless quen's package is installed, the game is loaded (in photo mode when `photo`), and its
/// channel is up: a connection for the first verb.
fn ready(cx: &Ctx, tool: &str, photo: bool) -> Result<channel::Channel, Done> {
    doctor::installed(&[&doctor::PACKAGE], cx.config, tool)?;
    let last = cx.last_flags();
    let fine = last.is_some_and(|f| f & VALID != 0 && (!photo || in_photo(f)));
    if !fine {
        let need = if photo { "photo mode" } else { "a loaded game" };
        let why = format!(
            "quen's state shows {}: {tool} needs {need}. Nothing sent",
            game::shown(last)
        );
        return Err(why.into());
    }
    Ok(cx.channel()?)
}

/// One verb on a connection of its own, and its reply. A game whose package predates the camera paths refuses them as
/// an unknown verb.
fn send(cx: &Ctx, verb: Verb, fields: &Fields) -> Result<Reply, Done> {
    let channel = cx
        .channel()
        .map_err(|why| Done::Failed(why, fields.clone()))?;
    send_on(channel, verb, fields)
}

fn send_on(channel: channel::Channel, verb: Verb, fields: &Fields) -> Result<Reply, Done> {
    channel.call(verb, REPLY_MAX).map_err(|e| match e {
        channel::Error::Refused(_, Some(text)) if text.contains("unknown verb") => {
            Done::RefusedWith(
                "the game's quen package predates the camera paths: install quen-w3 0.1.2 or later"
                    .into(),
                Map::from_iter([("game_text".into(), json!(text))]),
            )
        }
        e => e.done(fields.clone()),
    })
}

/// The camera and pose in a reply's read-back, `{"camera", "pos", "rot", "fov"}`.
pub(crate) fn read_back(reply: &Reply) -> Result<(String, Pose), String> {
    let data = reply
        .data
        .as_ref()
        .ok_or("the game's reply has no read-back")?;
    let three = |key: &str| -> Option<[f64; 3]> {
        let v: Vec<f64> = data[key]
            .as_array()?
            .iter()
            .map(Value::as_f64)
            .collect::<Option<_>>()?;
        v.try_into().ok()
    };
    match (three("pos"), three("rot"), data["fov"].as_f64()) {
        (Some(pos), Some(rot), Some(fov)) => Ok((
            data["camera"].as_str().unwrap_or_default().into(),
            Pose { pos, rot, fov },
        )),
        _ => Err(format!("the game's read-back isn't a pose: {data}")),
    }
}

/// The largest difference of `got` from `want`: metres, and degrees over the angles (modulo 360) and fov.
pub(crate) fn off(got: &Pose, want: &Pose) -> (f64, f64) {
    let pos = (0..3)
        .map(|c| (got.pos[c] - want.pos[c]).abs())
        .fold(0.0, f64::max);
    let angle = |d: f64| (d - 360.0 * (d / 360.0).round()).abs();
    let deg = (0..3)
        .map(|c| angle(got.rot[c] - want.rot[c]))
        .chain([(got.fov - want.fov).abs()])
        .fold(0.0, f64::max);
    (pos, deg)
}

pub(crate) fn off_json((pos, deg): (f64, f64)) -> Value {
    let r = |x: f64| (x * 1e4).round() / 1e4;
    json!({"pos_m": r(pos), "deg": r(deg)})
}

pub(crate) fn pose_json(p: &Pose) -> Value {
    json!({"pos": p.pos, "rot": p.rot, "fov": p.fov})
}

/// An optional `[a, b, c]` argument of numbers.
fn numbers(args: &Value, key: &str) -> Result<Option<[f64; 3]>, String> {
    let Some(v) = args.get(key) else {
        return Ok(None);
    };
    let n: Option<Vec<f64>> = v
        .as_array()
        .and_then(|a| a.iter().map(Value::as_f64).collect());
    let n = n.and_then(|n| <[f64; 3]>::try_from(n).ok());
    n.map(Some).ok_or(format!("{key} must be three numbers"))
}

/// sweep_save's room_pan: a sweep from `Sweep::room_pan`.
fn room_pan(pan: &Value) -> Result<Sweep, String> {
    let known = ["pos", "rot", "fov", "turn", "keyframes", "duration", "ease"];
    let map = pan.as_object().ok_or("room_pan must be an object")?;
    if let Some(key) = map.keys().find(|k| !known.contains(&k.as_str())) {
        return Err(format!("room_pan: unknown field {key:?}"));
    }
    let centre = json!({"pos": pan["pos"], "rot": pan["rot"], "fov": pan["fov"]});
    let centre: Pose = serde_json::from_value(centre).map_err(|e| format!("room_pan: {e}"))?;
    let ease = pan.get("ease").cloned().unwrap_or(json!("linear"));
    let ease: Ease = serde_json::from_value(ease).map_err(|e| format!("room_pan ease: {e}"))?;
    let num = |key: &str| {
        pan[key]
            .as_f64()
            .ok_or(format!("room_pan: {key} must be a number"))
    };
    let keyframes = pan["keyframes"]
        .as_u64()
        .ok_or("room_pan: keyframes must be a whole number")?;
    let sweep = Sweep::room_pan(
        &centre,
        num("turn")?,
        keyframes as usize,
        num("duration")?,
        ease,
    );
    sweep.map_err(|e| format!("room_pan: {e}"))
}

fn save(session: &Session, call: &mut Call, args: &Value) -> Result<Fields, Done> {
    let map = args
        .as_object()
        .ok_or("arguments must be an object".to_string())?;
    let known = [
        "name",
        "keyframes",
        "room_pan",
        "speed",
        "ease",
        "world",
        "note",
    ];
    if let Some(key) = map.keys().find(|k| !known.contains(&k.as_str())) {
        return Err(format!("unknown argument {key:?}").into());
    }
    let name = text(args, "name")?.trim();
    if name.is_empty() {
        return Err("name must not be empty".to_string().into());
    }
    let note = match args.get("note") {
        None => None,
        Some(_) => Some(text(args, "note")?.to_string()),
    };
    let config = session.config()?;
    // The core reads the sweep; each keyframe's capture_id comes off first.
    let mut sweep = json!({});
    let mut captures = Vec::new();
    match (args.get("keyframes"), args.get("room_pan")) {
        (Some(keys), None) => {
            let keys = keys
                .as_array()
                .ok_or("keyframes must be an array".to_string())?;
            let mut keys = keys.clone();
            for k in &mut keys {
                let id = k.as_object_mut().and_then(|k| k.remove("capture_id"));
                captures.push(match id {
                    None => None,
                    Some(id) => Some(
                        id.as_i64()
                            .ok_or("capture_id must be a whole number".to_string())?,
                    ),
                });
            }
            sweep["keyframes"] = json!(keys);
            for key in ["speed", "ease"] {
                if let Some(v) = args.get(key) {
                    sweep[key] = v.clone();
                }
            }
        }
        (None, Some(pan)) => {
            if args.get("speed").is_some() || args.get("ease").is_some() {
                let why = "room_pan is constant speed, its ease inside room_pan";
                return Err(why.to_string().into());
            }
            let made = room_pan(pan)?;
            captures = vec![None; made.keyframes.len()];
            sweep = serde_json::to_value(made).map_err(|e| e.to_string())?;
        }
        _ => return Err("give keyframes or room_pan, one of them".to_string().into()),
    }
    if let Some(world) = args.get("world") {
        sweep["world"] = world.clone();
    }
    let sweep = Sweep::from_json(&sweep.to_string()).map_err(|e| e.to_string())?;
    let path = sweep.path().map_err(|e| e.to_string())?;
    call.lap("check");

    let mut ledger = open_ledger(config)?;
    let player = sweep.world.player.map(|p| PlayerPose {
        position: p.pos,
        yaw: Some(p.yaw),
    });
    let (id, poses) = ledger
        .atomic(|l| {
            let row = |poses| SweepRow {
                name: name.into(),
                sweep: sweep.clone(),
                save_id: None,
                poses,
                note: note.clone(),
            };
            let known = l.query(
                "SELECT id FROM sweeps WHERE name = ?1",
                &[json!(name)],
                Duration::from_secs(5),
            )?;
            if !known.rows.is_empty() {
                // add_sweep returns its id when it is the same sweep, and refuses another.
                return Ok((l.add_sweep(&row(Vec::new()))?, None));
            }
            let mut poses = Vec::new();
            for (i, (k, capture)) in sweep.keyframes.iter().zip(&captures).enumerate() {
                poses.push(Some(l.add_pose(&PoseRow {
                    name: format!("{name} k{i}"),
                    camera: k.pos,
                    rotation: k.rot,
                    fov: k.fov,
                    player: player.clone(),
                    save_id: None,
                    ref_capture_id: *capture,
                })?));
            }
            Ok((l.add_sweep(&row(poses.clone()))?, Some(poses)))
        })
        .map_err(|e| match e {
            LedgerError::Refused(why) => format!("{why}; nothing written"),
            e => format!("{e}; nothing written"),
        })?;
    call.lap("ledger");
    call.row("sweeps", id);
    for pose in poses.iter().flatten().flatten() {
        call.row("poses", *pose);
    }
    let mut fields = Map::new();
    fields.insert("sweep_id".into(), json!(id));
    fields.insert("existed".into(), json!(poses.is_none()));
    fields.insert(
        "sha256".into(),
        json!(sha256_hex(sweep.to_json().as_bytes())),
    );
    fields.insert("duration_s".into(), json!(path.duration()));
    let frames: Map<String, Value> = [24, 30, 60]
        .into_iter()
        .filter_map(|fps| Some((fps.to_string(), json!(path.frame_count(fps).ok()?))))
        .collect();
    fields.insert("frames_at_fps".into(), json!(frames));
    fields.insert("sweep".into(), json!(sweep));
    Ok(fields)
}

fn open_ledger(config: &crate::config::Config) -> Result<Ledger, String> {
    Ledger::open(config.state_dir().join("quen.db")).map_err(|e| e.to_string())
}

/// A checked sweep_play request.
struct Play {
    sweep_id: i64,
    sweep: Sweep,
    mode: Mode,
    fps: Option<u32>,
    method: &'static str,
    tag: String,
    settle: Duration,
}

impl Play {
    fn parse(cx: &Ctx, args: &Value, ledger: &Ledger) -> Result<Play, String> {
        let name = text(args, "sweep")?;
        let mode = match text(args, "mode")? {
            "realtime" => Mode::Realtime,
            "step" => Mode::Step,
            m => return Err(format!("mode {m:?}: realtime or step")),
        };
        let fps = match args.get("fps") {
            None => None,
            Some(v) => Some(
                v.as_u64()
                    .and_then(|f| u32::try_from(f).ok())
                    .ok_or("fps must be a whole number")?,
            ),
        };
        sweep::check_playback(mode, fps).map_err(|e| e.to_string())?;
        if mode == Mode::Realtime
            && ["method", "tag", "settle_ms"]
                .iter()
                .any(|k| args.get(k).is_some())
        {
            return Err("method, tag and settle_ms are frame step's".into());
        }
        let method = match args.get("method").map(Value::as_str) {
            None | Some(Some("sdr")) => "sdr",
            Some(Some("hdr")) => "hdr",
            Some(m) => return Err(format!("method {m:?}: sdr or hdr")),
        };
        let settle = match args.get("settle_ms") {
            None => 0,
            Some(v) => v.as_u64().filter(|ms| *ms <= SETTLE_MAX_MS).ok_or(format!(
                "settle_ms must be a whole number from 0 to {SETTLE_MAX_MS}"
            ))?,
        };
        let found = ledger
            .query(
                "SELECT id, sweep FROM sweeps WHERE name = ?1",
                &[json!(name)],
                Duration::from_secs(5),
            )
            .map_err(|e| e.to_string())?;
        let Some([id, json]) = found
            .rows
            .first()
            .and_then(|r| <&[Value; 2]>::try_from(&r[..]).ok())
        else {
            return Err(format!(
                "no sweep named {name:?}: sweep_save stores one (ledger_query: SELECT name FROM sweeps)"
            ));
        };
        let sweep = Sweep::from_json(json.as_str().unwrap_or_default())
            .map_err(|e| format!("sweep {name:?} in the ledger: {e}"))?;
        let sweep_id = id.as_i64().unwrap_or_default();
        let tag = match args.get("tag") {
            None => format!("sweep{sweep_id}-{}", session::now()),
            Some(_) => text(args, "tag")?.to_string(),
        };
        let fine = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-';
        if tag.is_empty() || tag.len() > 58 || !tag.chars().all(fine) {
            return Err("tag: 1-58 letters, digits, _ or - (it names the frames)".into());
        }
        let folder = cx.config.capture_root().join(&cx.live.id);
        if mode == Mode::Step
            && let Some(first) = ["png", "npy"]
                .iter()
                .map(|ext| folder.join(format!("{tag}-00000.{ext}")))
                .find(|p| p.exists())
        {
            return Err(format!(
                "{} exists: tag {tag:?} is taken in this session",
                first.display()
            ));
        }
        Ok(Play {
            sweep_id,
            sweep,
            mode,
            fps,
            method,
            tag,
            settle: Duration::from_millis(settle),
        })
    }
}

fn play(session: &Session, call: &mut Call, args: &Value) -> Result<Fields, Done> {
    let known = ["sweep", "mode", "fps", "method", "tag", "settle_ms"];
    let cx = game::ctx(session, args, &known)?;
    let mut ledger = open_ledger(cx.config)?;
    let p = Play::parse(&cx, args, &ledger)?;
    let path = p.sweep.path().map_err(|e| e.to_string())?;
    let mut first = Some(ready(&cx, "sweep_play", true)?);
    let back = crate::measure::game_back(&cx, call)?;
    call.lap("check");

    // Acting from here: what is done goes into the fields, and the path and the pause are undone at the end.
    let mut fields = Map::new();
    fields.insert("sweep_id".into(), json!(p.sweep_id));
    fields.insert("mode".into(), json!(p.mode.as_str()));
    if let Some(back) = back {
        fields.insert("game_back".into(), json!(back));
    }
    let frames = match p.mode {
        Mode::Realtime => 0,
        Mode::Step => path.frame_count(p.fps.unwrap_or_default()).unwrap_or(0),
    };
    // Step mode took about 0.5 s a frame (feature tests 2).
    let secs = match p.mode {
        Mode::Realtime => path.duration(),
        Mode::Step => f64::from(frames) / 2.0,
    };
    if p.mode == Mode::Step {
        let _dpi = capture::Dpi::per_monitor();
        if let Ok((_, r)) = capture::game_window(&cx.live.job.pids().unwrap_or_default()) {
            let (w, h) = ((r.right - r.left) as u32, (r.bottom - r.top) as u32);
            let gb = quen_core::disk::gb(frames_bytes(frames, w, h, p.method));
            fields.insert("frames_gb_est".into(), json!(gb));
            call.progress(&format!("{frames} frames, about {gb} GB"));
        }
    }
    // The owner, 0.1.28: the line and its wait before a session's first sweep_play only; later ones start at once. On
    // the connection the checks opened: a second one beside it finds the ASI's pipe busy (finding 80).
    let st = &cx.live.status;
    if !st.swept.get()
        && let Some(shown) = status::measuring(&cx, call, secs.ceil() as u64, None, &mut first)
    {
        st.swept.set(true);
        fields.insert("status".into(), json!(shown));
    }
    let pause = p.mode == Mode::Step || p.sweep.world.paused;
    let (mut paused, mut loaded) = (false, false);
    let played = (|| {
        let mut send = |verb, fields: &Fields| match first.take() {
            Some(channel) => send_on(channel, verb, fields),
            None => send(&cx, verb, fields),
        };
        // The first pose, set and read back before anything plays: a camera far from it can stop at geometry on the
        // way (feature tests 2), and a real-time play reads back only its end.
        let start = path.sample(0.0);
        let verb = Verb::CamSet {
            pos: start.pos.map(|x| x as f32),
            rot: start.rot.map(|x| x as f32),
            fov: Some(start.fov as f32),
        };
        let reply = send(verb, &fields)?;
        let (_, got) = read_back(&reply).map_err(|why| Done::Failed(why, fields.clone()))?;
        if let Some(why) = short(&got, &start) {
            let why = format!("the sweep's first pose: {why}; nothing played");
            return Err(Done::Refused(why));
        }
        if pause {
            let reply = send(Verb::Pause { on: true }, &fields)?;
            channel::note_pause(cx.live, &reply);
            paused = true;
            fields.insert("pause".into(), json!(reply.text));
        }
        // A real-time path plays from its load, and step mode's frames are captured after it: the status line is down
        // first, its wait a lap of its own (finding 86: a notice hid inside frames), and stays down to the sweep's end.
        status::hide(&cx, call, false)?;
        let verb = Verb::CamPath {
            mode: p.mode,
            fps: p.fps,
            speed: p.sweep.speed,
            ease: p.sweep.ease,
            keys: path.keys().to_vec(),
        };
        let reply = send(verb, &fields)?;
        loaded = true;
        fields.insert("reply".into(), json!(reply.text));
        if let Ok((_, first)) = read_back(&reply) {
            fields.insert("first".into(), pose_json(&first));
        }
        call.lap("load");
        let t = ledger.utc(session::now() as i64).map_err(|e| e.to_string());
        let run = t.and_then(|t| {
            let run = SweepRun {
                sweep_id: p.sweep_id,
                boot_id: Some(cx.live.boot),
                mode: p.mode,
                fps: p.fps,
                t,
            };
            ledger.add_sweep_run(&run).map_err(|e| e.to_string())
        });
        let run =
            run.map_err(|why| Done::Failed(format!("{why}; the path is loaded"), fields.clone()))?;
        call.row("sweep_runs", run);
        fields.insert("run_id".into(), json!(run));
        match p.mode {
            Mode::Realtime => realtime(&cx, call, &path, &mut fields),
            Mode::Step => step(session, &cx, call, &p, &path, run, &mut ledger, &mut fields),
        }
    })();
    // However it went, the path stops and quen's pause is lifted. A verb that failed (rather than was refused) may have
    // run, so it is undone too.
    let failed = matches!(played, Err(Done::Failed(..)));
    let mut left = Vec::new();
    let undo = [
        (loaded || failed, Verb::CamStop {}),
        (paused || (pause && failed), Verb::Pause { on: false }),
    ];
    for (_, verb) in undo.into_iter().filter(|(due, _)| *due) {
        let what = serde_json::to_string(&verb).unwrap_or_default();
        let lifts = matches!(verb, Verb::Pause { .. });
        let sent = send(&cx, verb, &Map::new());
        // A path that may still be loaded: hands_give stops it.
        if !lifts {
            cx.live.path.set(sent.is_err());
        }
        match sent {
            // The pause as it is now, not as the start set it (feature tests 2).
            Ok(reply) if lifts => {
                channel::note_pause(cx.live, &reply);
                fields.insert("pause".into(), json!(reply.text));
            }
            Ok(_) => {}
            Err(done) => left.push(format!("{what}: {}", why(&done))),
        }
    }
    call.lap("stop");
    // A refusal before the pause and the path left nothing behind; after either, the pause was toggled (modQuen's pause
    // is one flag: an earlier game_pause on is lifted), or frames and the run row exist.
    let (why, mut done) = match played {
        Ok(done) if left.is_empty() => return Ok(done.into_iter().chain(fields).collect()),
        Err(refused @ (Done::Refused(_) | Done::RefusedWith(..) | Done::Needs(..)))
            if left.is_empty() && !loaded && !paused =>
        {
            return Err(refused);
        }
        Ok(done) => ("the sweep played, but undoing it failed".to_string(), done),
        Err(Done::Failed(why, done)) => (why, done),
        Err(other) => (why(&other).to_string(), Map::new()),
    };
    done.extend(fields);
    if !left.is_empty() {
        done.insert("undo_left".into(), json!(left));
    }
    Err(Done::Failed(why, done))
}

/// About how many bytes `frames` captures of `method` write at `w` x `h`: the PNG's share of its raw bytes on real 4K
/// frames (2026-10-26: sdr 62 % of 3 a pixel, hdr 71 % of 6; the scene changes it).
fn frames_bytes(frames: u32, w: u32, h: u32, method: &str) -> u64 {
    let per_px = if method == "hdr" {
        6.0 * 0.71
    } else {
        3.0 * 0.62
    };
    (f64::from(frames) * f64::from(w) * f64::from(h) * per_px).round() as u64
}

fn why(done: &Done) -> &str {
    match done {
        Done::Refused(why)
        | Done::RefusedWith(why, _)
        | Done::Needs(why, _)
        | Done::Failed(why, _) => why,
        Done::Ok(_) => "",
    }
}

/// Real time: waits out the sweep, then reads the camera back against the last keyframe.
fn realtime(
    cx: &Ctx,
    call: &mut Call,
    path: &sweep::Path,
    fields: &mut Fields,
) -> Result<Fields, Done> {
    let end = Duration::from_secs_f64(path.duration()) + END_MARGIN;
    let start = Instant::now();
    while start.elapsed() < end {
        if call.stopped() {
            let why = format!(
                "cancelled {:.1} s into the sweep",
                start.elapsed().as_secs_f64()
            );
            return Err(Done::Failed(why, fields.clone()));
        }
        call.progress(&format!(
            "{:.0} of {:.0} s",
            start.elapsed().as_secs_f64(),
            path.duration()
        ));
        sleep(Duration::from_millis(100).min(end.saturating_sub(start.elapsed())));
    }
    call.lap("play");
    let reply = send(cx, Verb::CamGet {}, fields)?;
    let (_, got) = read_back(&reply).map_err(|why| Done::Failed(why, fields.clone()))?;
    let mut done = Map::new();
    done.insert("duration_s".into(), json!(path.duration()));
    done.insert("end".into(), pose_json(&got));
    done.insert(
        "off_end".into(),
        off_json(off(&got, &path.sample(path.duration()))),
    );
    Ok(done)
}

/// Frame step: every frame set, read back and captured, each capture recorded as a frame of `run`.
#[allow(clippy::too_many_arguments)]
fn step(
    session: &Session,
    cx: &Ctx,
    call: &mut Call,
    p: &Play,
    path: &sweep::Path,
    run: i64,
    ledger: &mut Ledger,
    fields: &mut Fields,
) -> Result<Fields, Done> {
    let fps = p.fps.unwrap_or_default();
    let times = path.frame_times(fps).map_err(|e| e.to_string())?;
    let frames = times.len();
    let folder = cx.config.capture_root().join(&cx.live.id);
    fields.insert("frames_total".into(), json!(frames));
    fields.insert("folder".into(), json!(folder));
    let (mut shas, mut worst) = (String::new(), (0.0, 0.0));
    let start = Instant::now();
    for (n, t) in times.iter().enumerate() {
        let n = n as u32;
        if call.stopped() {
            fields.insert("frames".into(), json!(n));
            return Err(Done::Failed(
                format!("cancelled after {n} of {frames} frames"),
                fields.clone(),
            ));
        }
        let reply = send(cx, Verb::CamFrame { n }, fields)?;
        let (_, got) = read_back(&reply).map_err(|why| Done::Failed(why, fields.clone()))?;
        let (pos, deg) = off(&got, &path.sample(*t));
        worst = (f64::max(worst.0, pos), f64::max(worst.1, deg));
        fields.insert("off".into(), off_json(worst));
        sleep(p.settle);
        let tag = format!("{}-{n:05}", p.tag);
        let (capture, _) = grab(session, &cx.live.token, &tag, p.method)?;
        let sha = ledger
            .add_sweep_frame(&SweepFrame {
                run_id: run,
                frame: n,
                capture_id: capture,
            })
            .and_then(|()| {
                ledger.query(
                    "SELECT sha256 FROM captures WHERE id = ?1",
                    &[json!(capture)],
                    Duration::from_secs(5),
                )
            });
        let sha =
            sha.map_err(|e| Done::Failed(format!("ledger, frame {n}: {e}"), fields.clone()))?;
        shas.push_str(sha.rows[0][0].as_str().unwrap_or_default());
        fields.insert("frames".into(), json!(n + 1));
        call.progress(&format!("frame {} of {frames}", n + 1));
    }
    call.lap("frames");
    let mut done = Map::new();
    done.insert("fps".into(), json!(fps));
    done.insert("frames_digest".into(), json!(sha256_hex(shas.as_bytes())));
    let each = start.elapsed().as_secs_f64() / frames as f64;
    done.insert("per_frame_s".into(), json!((each * 100.0).round() / 100.0));
    Ok(done)
}

/// One frame: game_screenshot tagged `tag` (its PNG alone: no npy), and its capture row and file. The screenshot's own
/// timing and artifacts stay out of sweep_play's envelope: its run and frames name them.
pub(crate) fn grab(
    session: &Session,
    token: &str,
    tag: &str,
    method: &str,
) -> Result<(i64, Option<PathBuf>), Done> {
    #[cfg(test)]
    if let Some(got) = tests::grab(tag) {
        return got.map(|id| (id, None));
    }
    let mut shot = session.call(crate::mcp::Conn::nobody(), Value::Null, None);
    let args = json!({"token": token, "tag": tag, "method": method});
    let fields = capture::screenshot(session, &mut shot, &args)?;
    let path = fields["captures"][0]["path"].as_str().map(PathBuf::from);
    match fields["captures"][0]["id"].as_i64() {
        Some(id) => Ok((id, path)),
        None => Err(Done::Failed(format!("frame {tag}: no capture row"), fields)),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::channel::tests::{fake_game, reply, served, token};
    use crate::game::tests::Fake;
    use quen_core::ledger::Capture;
    use quen_core::protocol::Request;
    use quen_core::sweep::Speed;
    use std::cell::RefCell;
    use std::fs;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    pub(crate) type Grab = Box<dyn FnMut(&str) -> Result<i64, Done>>;

    thread_local! {
        /// Unit tests' frame grab: the tag in, a capture row's id out. Unset, game_screenshot runs.
        static GRAB: RefCell<Option<Grab>> = const { RefCell::new(None) };
    }

    pub(super) fn grab(tag: &str) -> Option<Result<i64, Done>> {
        GRAB.with_borrow_mut(|g| g.as_mut().map(|g| g(tag)))
    }

    /// Sets the unit tests' frame grab (scene.rs's game_frame tests).
    pub(crate) fn set_grab(grab: Option<Grab>) {
        GRAB.set(grab);
    }

    fn run(session: &Session, tool: &str, args: Value) -> Value {
        let mut call = session.call(crate::mcp::Conn::nobody(), Value::Null, None);
        let done = match tool {
            "game_camera" => camera(session, &mut call, &args).map_or_else(|d| d, Done::Ok),
            _ => super::call(session, &mut call, tool, &args),
        };
        session.envelope(tool, call, done)
    }

    fn ledger(fx: &Fake) -> Ledger {
        open_ledger(&fx.shared.config().as_ref().clone().unwrap()).unwrap()
    }

    fn rows(fx: &Fake, sql: &str) -> Vec<Vec<Value>> {
        ledger(fx)
            .query(sql, &[], Duration::from_secs(5))
            .unwrap()
            .rows
    }

    /// A capture row of the test's own, a synthetic file's.
    fn capture(fx: &Fake, path: &str) -> i64 {
        let row = Capture {
            boot_id: Some(1),
            path: path.into(),
            sha256: sha256_hex(path.as_bytes()),
            bytes: 1,
            kind: "screenshot".into(),
            t: "2026-10-18T00:00:00Z".into(),
            ..Capture::default()
        };
        ledger(fx).add_capture(&row).unwrap()
    }

    /// The reference sweep of sweep.rs's tests: a synthetic inn interior.
    fn inn() -> Value {
        json!([
            {"t": 0, "pos": [100, 200, 10], "rot": [-5, 170, 0], "fov": 60},
            {"t": 2, "pos": [103, 201, 10.5], "rot": [0, -170, 0], "fov": 60, "ease": "ease_in_out"},
            {"t": 4, "pos": [103, 201, 10.5], "rot": [10, -120, 5], "fov": 45},
            {"t": 7, "pos": [98, 205, 11], "rot": [0, -60, 0], "fov": 50},
        ])
    }

    #[test]
    fn sweeps_are_stored_canonical_with_a_pose_per_keyframe() {
        let fx = Fake::new("sweep-save");
        let session = fx.session();
        let shot = capture(&fx, "s1/k1.png");
        let mut keys = inn();
        keys[1]["capture_id"] = json!(shot);
        keys[0]["rot"][1] = json!(530.00004); // canonical: 170
        let world = json!({"save": "synthetic", "player": {"pos": [100, 199, 9], "yaw": 10}});
        let args = json!({"name": "inn", "keyframes": keys, "world": world, "note": "test"});
        let out = run(&session, "sweep_save", args.clone());
        assert_eq!(out["ok"], true, "{out}");
        assert_eq!(out["existed"], false);
        assert_eq!(out["duration_s"], 7.0);
        assert_eq!(out["frames_at_fps"]["30"], 211);
        assert_eq!(
            out["sweep"]["keyframes"][0]["rot"],
            json!([-5.0, 170.0, 0.0])
        );
        let id = out["sweep_id"].as_i64().unwrap();
        assert_eq!(out["ledger"]["poses"].as_array().unwrap().len(), 4);

        let stored = rows(&fx, "SELECT sweep, sha256, note FROM sweeps");
        let json = stored[0][0].as_str().unwrap();
        assert_eq!(
            Sweep::from_json(json).unwrap().to_json(),
            json,
            "stored canonical"
        );
        assert_eq!(stored[0][1], out["sha256"]);
        let poses = rows(
            &fx,
            "SELECT p.name, p.cam_yaw, p.fov, p.ref_capture_id, p.player_x FROM sweep_keyframes k
             JOIN poses p ON p.id = k.pose_id ORDER BY k.keyframe",
        );
        assert_eq!(poses.len(), 4);
        assert_eq!(poses[0][..2], [json!("inn k0"), json!(170.0)]);
        assert_eq!(poses[1][3], json!(shot));
        assert_eq!(poses[3][2], json!(50.0));
        assert!(poses.iter().all(|p| p[4] == json!(100.0)), "{poses:?}");

        // The same again: its id, nothing new. Another sweep under the name: refused, nothing written.
        let again = run(&session, "sweep_save", args);
        assert_eq!(
            (again["sweep_id"].as_i64(), again["existed"].as_bool()),
            (Some(id), Some(true)),
            "{again}"
        );
        let mut other = inn();
        other[3]["fov"] = json!(55);
        let taken = run(
            &session,
            "sweep_save",
            json!({"name": "inn", "keyframes": other}),
        );
        assert_eq!(taken["refused"], true, "{taken}");
        assert!(
            taken["reason"]
                .as_str()
                .unwrap()
                .contains("name this one anew"),
            "{taken}"
        );
        let count = || {
            rows(
                &fx,
                "SELECT (SELECT count(*) FROM sweeps), (SELECT count(*) FROM poses)",
            )
        };
        assert_eq!(count()[0], [json!(1), json!(4)]);

        // Refused, nothing written: the core's checks, an unknown capture, bad shapes.
        let mut turn = inn();
        turn[3]["rot"][1] = json!(60);
        let mut ghost = inn();
        ghost[2]["capture_id"] = json!(9999);
        for (args, why) in [
            (json!({"name": "x", "keyframes": turn}), "turn 179"),
            (
                json!({"name": "x", "keyframes": ghost}),
                "no capture 9999; nothing written",
            ),
            (
                json!({"name": "x", "keyframes": inn(), "speed": "constant"}),
                "under constant speed",
            ),
            (json!({"name": "x", "keyframes": [inn()[0]]}), "1 keyframes"),
            (json!({"name": " ", "keyframes": inn()}), "name"),
            (json!({"name": "x"}), "keyframes or room_pan"),
            (
                json!({"name": "x", "keyframes": inn(), "extra": 1}),
                "unknown argument",
            ),
        ] {
            let out = run(&session, "sweep_save", args.clone());
            assert_eq!(out["refused"], true, "{args}: {out}");
            assert!(
                out["reason"].as_str().unwrap().contains(why),
                "{why}: {out}"
            );
        }
        assert_eq!(count()[0], [json!(1), json!(4)]);

        // A room pan: a full turn in place at constant speed.
        let pan = json!({"pos": [1, 2, 3], "rot": [-10, 170, 0], "fov": 70, "turn": 360, "keyframes": 5, "duration": 20, "ease": "ease_in_out"});
        let out = run(
            &session,
            "sweep_save",
            json!({"name": "pan", "room_pan": pan}),
        );
        assert_eq!(out["ok"], true, "{out}");
        let yaws: Vec<f64> = out["sweep"]["keyframes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|k| k["rot"][1].as_f64().unwrap())
            .collect();
        assert_eq!(yaws, [170.0, -100.0, -10.0, 80.0, 170.0]);
        assert_eq!(out["sweep"]["speed"], "constant");
        assert_eq!(out["frames_at_fps"]["24"], 481);
    }

    /// The stand-in game's wall: a set beyond x = 1000 keeps the camera's position, as photo mode's collision does.
    const WALL: f32 = 1000.0;
    /// A first key at this x makes the stand-in refuse cam.path as an unknown verb, as quen-w3 0.1.1 does.
    const OLD_PACKAGE: f64 = -77.0;

    /// A stand-in game for the camera verbs: it answers each as modQuen does, its read-back the camera it holds; a path
    /// sets that camera to quen-core's own sample (frame step: frame n's, n kept in `frame`), with `nudge` metres added
    /// to frame 1.
    fn camera_game(
        fx: &Fake,
        conns: usize,
        nudge: f64,
    ) -> (std::thread::JoinHandle<Vec<Request>>, Arc<AtomicU32>) {
        let held = std::sync::Mutex::new((
            None::<sweep::Path>,
            0u32,
            Pose {
                pos: [0.0; 3],
                rot: [0.0; 3],
                fov: 60.0,
            },
        ));
        let frame = Arc::new(AtomicU32::new(u32::MAX));
        let set = frame.clone();
        let server = fake_game(
            &token(fx),
            conns,
            Box::new(move |r| {
                let mut held = held.lock().unwrap();
                let data = |p: &Pose| {
                    Some(json!({"camera": "photomode", "pos": p.pos, "rot": p.rot, "fov": p.fov}))
                };
                let (ok, text, data) = match &r.verb {
                    Verb::CamGet {} => (true, String::new(), data(&held.2)),
                    Verb::CamSet { pos, rot, fov } => {
                        let fov = fov.map_or(held.2.fov, f64::from);
                        held.2 = Pose {
                            pos: if pos[0] > WALL {
                                held.2.pos
                            } else {
                                pos.map(f64::from)
                            },
                            rot: rot.map(f64::from),
                            fov,
                        };
                        (true, "set".into(), data(&held.2))
                    }
                    // A package that predates the camera paths (quen-w3 0.1.1), for a first key at x = OLD_PACKAGE.
                    Verb::CamPath { keys, .. } if keys[0].pos[0] == OLD_PACKAGE => {
                        (false, "unknown verb \"cam.path\"".into(), None)
                    }
                    Verb::CamPath {
                        mode,
                        fps,
                        speed,
                        ease,
                        keys,
                    } => {
                        let path = sweep::Path::new(keys, *speed, *ease).unwrap();
                        held.2 = path.sample(0.0);
                        held.1 = fps.unwrap_or_default();
                        held.0 = Some(path);
                        (true, format!("{} path", mode.as_str()), data(&held.2))
                    }
                    Verb::CamFrame { n } => {
                        let path = held.0.clone().unwrap();
                        let mut pose = path.sample(f64::from(*n) / f64::from(held.1));
                        if *n == 1 {
                            pose.pos[0] += nudge;
                        }
                        held.2 = pose;
                        set.store(*n, Ordering::SeqCst);
                        (true, format!("frame {n} set"), data(&held.2))
                    }
                    Verb::CamStop {} => (true, "stopped".into(), None),
                    Verb::Pause { on } => (
                        true,
                        format!("quen's pause is {}", if *on { "on" } else { "off" }),
                        None,
                    ),
                    _ => (false, "not a camera verb".into(), None),
                };
                vec![reply(r.id, ok, &text, data)]
            }),
        );
        (server, frame)
    }

    fn photo_mode(fx: &Fake, on: bool) {
        let flags = if on { 129 } else { 1 };
        let log = fx.dir.join("game/bin/x64_dx12/W3QuenState.log");
        fs::write(log, format!("push flags={flags}\n")).unwrap();
    }

    #[test]
    fn game_camera_reads_and_sets_in_photo_mode_only() {
        let fx = served("camera");
        let (server, _) = camera_game(&fx, 4, 0.0);
        let session = fx.session();
        let get = run(&session, "game_camera", json!({"token": "tok"}));
        assert_eq!(get["camera"], "photomode", "{get}");
        assert_eq!(get["pose"]["fov"], 60.0);
        let set =
            json!({"token": "tok", "pos": [671.3, 1874.8, 11.9], "rot": [-5, 170, 0], "fov": 50});
        let refused = run(&session, "game_camera", set.clone());
        assert!(
            refused["reason"]
                .as_str()
                .unwrap()
                .contains("needs photo mode"),
            "{refused}"
        );
        photo_mode(&fx, true);
        let out = run(&session, "game_camera", set);
        assert_eq!(out["ok"], true, "{out}");
        assert_eq!(out["pose"]["rot"], json!([-5.0, 170.0, 0.0]));
        // Sent as f32: 671.3 comes back as 671.29998779296875, within a millimetre.
        assert!(out["off"]["pos_m"].as_f64().unwrap() < 1e-3, "{out}");
        let only_pos = run(
            &session,
            "game_camera",
            json!({"token": "tok", "pos": [1, 2, 3]}),
        );
        assert!(
            only_pos["reason"].as_str().unwrap().contains("go together"),
            "{only_pos}"
        );
        let get = run(&session, "game_camera", json!({"token": "tok"}));
        assert_eq!(get["pose"]["fov"], 50.0, "{get}");
        // Feature tests 2: across a railing, photo mode kept the position and set the rest; that was ok: true.
        let blocked = run(
            &session,
            "game_camera",
            json!({"token": "tok", "pos": [1003.3, 1874.8, 11.9], "rot": [-5, 170, 0]}),
        );
        assert_eq!(
            (blocked["ok"].as_bool(), blocked["refused"].as_bool()),
            (Some(false), Some(false)),
            "{blocked}"
        );
        assert!(
            blocked["reason"].as_str().unwrap().contains("Noclip Off"),
            "{blocked}"
        );
        let short = blocked["off"]["pos_m"].as_f64().unwrap();
        assert!((short - 332.0).abs() < 1e-3, "{blocked}");
        assert_eq!(blocked["pose"]["pos"][0], json!(671.2999877929688));
        let verbs: Vec<Verb> = server.join().unwrap().into_iter().map(|r| r.verb).collect();
        assert_eq!(verbs.len(), 4, "the refusals sent nothing: {verbs:?}");
        assert_eq!(
            verbs[1],
            Verb::CamSet {
                pos: [671.3, 1874.8, 11.9],
                rot: [-5.0, 170.0, 0.0],
                fov: Some(50.0)
            }
        );
    }

    /// Frame step end to end against the stand-in game: the pause, the path, every frame set, read back and captured
    /// (by the test's grab), the run and its frames in the ledger, then the path stopped and the pause lifted.
    #[test]
    fn sweep_play_steps_and_captures_every_frame() {
        let fx = served("sweep-step");
        photo_mode(&fx, true);
        let session = fx.session();
        let saved = run(
            &session,
            "sweep_save",
            json!({"name": "inn", "keyframes": inn()}),
        );
        assert_eq!(saved["ok"], true, "{saved}");
        let frames = 29; // 7 s at 4 fps, both ends
        let (server, _) = camera_game(&fx, 3 + frames + 2, 0.01);
        let db = fx
            .shared
            .config()
            .as_ref()
            .as_ref()
            .unwrap()
            .state_dir()
            .join("quen.db");
        let shas = std::rc::Rc::new(RefCell::new(String::new()));
        let seen = shas.clone();
        GRAB.set(Some(Box::new(move |tag| {
            let fx_ledger = Ledger::open(&db).unwrap();
            let row = Capture {
                boot_id: Some(1),
                path: format!("s1/{tag}.png"),
                sha256: sha256_hex(tag.as_bytes()),
                bytes: 1,
                kind: "screenshot".into(),
                t: "2026-10-18T00:00:00Z".into(),
                ..Capture::default()
            };
            let mut l = fx_ledger;
            seen.borrow_mut().push_str(&row.sha256);
            Ok(l.add_capture(&row).unwrap())
        })));
        let args =
            json!({"token": "tok", "sweep": "inn", "mode": "step", "fps": 4, "tag": "inn-a"});
        let out = run(&session, "sweep_play", args);
        GRAB.set(None);
        assert_eq!(out["ok"], true, "{out}");
        assert_eq!(
            (out["frames"].as_u64(), out["frames_total"].as_u64()),
            (Some(29), Some(29)),
            "{out}"
        );
        assert_eq!(out["frames_digest"], sha256_hex(shas.borrow().as_bytes()));
        let worst = out["off"]["pos_m"].as_f64().unwrap();
        assert!((worst - 0.01).abs() < 1e-6, "frame 1's nudge: {out}");
        assert_eq!(out["off"]["deg"], 0.0, "{out}");

        assert_eq!(out["pause"], "quen's pause is off", "the final state");

        let requests = server.join().unwrap();
        let verbs: Vec<&Verb> = requests.iter().map(|r| &r.verb).collect();
        // The first keyframe's pose, set and read back before anything plays.
        let Verb::CamSet { pos, fov, .. } = verbs[0] else {
            panic!("{verbs:?}")
        };
        assert_eq!((*pos, *fov), ([100.0, 200.0, 10.0], Some(60.0)));
        assert_eq!(*verbs[1], Verb::Pause { on: true });
        let Verb::CamPath {
            mode,
            fps,
            speed,
            keys,
            ..
        } = verbs[2]
        else {
            panic!("{verbs:?}")
        };
        assert_eq!(
            (*mode, *fps, *speed, keys.len()),
            (Mode::Step, Some(4), Speed::Keys, 4)
        );
        let frames_sent: Vec<u32> = verbs[3..3 + frames]
            .iter()
            .map(|v| match v {
                Verb::CamFrame { n } => *n,
                v => panic!("{v:?}"),
            })
            .collect();
        assert_eq!(frames_sent, (0..29).collect::<Vec<_>>());
        assert_eq!(
            verbs[3 + frames..],
            [&Verb::CamStop {}, &Verb::Pause { on: false }]
        );

        let run_rows = rows(
            &fx,
            "SELECT id, sweep_id, boot_id, mode, fps FROM sweep_runs",
        );
        assert_eq!(
            run_rows[0][1..],
            [json!(1), json!(1), json!("step"), json!(4)]
        );
        let frame_rows = rows(
            &fx,
            "SELECT f.frame, c.path FROM sweep_frames f JOIN captures c ON c.id = f.capture_id ORDER BY f.frame",
        );
        assert_eq!(frame_rows.len(), 29);
        assert_eq!(frame_rows[28], [json!(28), json!("s1/inn-a-00028.png")]);
    }

    #[test]
    fn sweep_play_in_real_time_waits_reads_back_and_stops() {
        let fx = served("sweep-realtime");
        photo_mode(&fx, true);
        let session = fx.session();
        let keys = json!([
            {"t": 0, "pos": [0, 0, 0], "rot": [0, 0, 0], "fov": 60},
            {"t": 0.3, "pos": [1, 0, 0], "rot": [0, 90, 0], "fov": 60},
        ]);
        let saved = run(
            &session,
            "sweep_save",
            json!({"name": "dolly", "keyframes": keys, "world": {"paused": true}}),
        );
        assert_eq!(saved["ok"], true, "{saved}");
        // The stand-in leaves the camera at the path's first pose: off_end shows the whole way to the last.
        let (server, _) = camera_game(&fx, 6, 0.0);
        let start = Instant::now();
        let out = run(
            &session,
            "sweep_play",
            json!({"token": "tok", "sweep": "dolly", "mode": "realtime"}),
        );
        assert!(
            start.elapsed() >= Duration::from_millis(800),
            "{:?}",
            start.elapsed()
        );
        assert_eq!(out["ok"], true, "{out}");
        assert_eq!(out["off_end"], json!({"pos_m": 1.0, "deg": 90.0}), "{out}");
        assert_eq!(out["first"]["pos"], json!([0.0, 0.0, 0.0]));
        let verbs: Vec<Verb> = server.join().unwrap().into_iter().map(|r| r.verb).collect();
        assert!(matches!(verbs[0], Verb::CamSet { .. }), "{verbs:?}");
        assert!(
            matches!(
                verbs[2],
                Verb::CamPath {
                    mode: Mode::Realtime,
                    fps: None,
                    ..
                }
            ),
            "{verbs:?}"
        );
        assert_eq!(verbs[1], Verb::Pause { on: true }, "world.paused");
        assert_eq!(
            verbs[3..],
            [Verb::CamGet {}, Verb::CamStop {}, Verb::Pause { on: false }]
        );
        assert_eq!(
            rows(&fx, "SELECT mode, fps FROM sweep_runs")[0],
            [json!("realtime"), Value::Null]
        );
    }

    #[test]
    fn sweep_play_refuses_before_sending() {
        let fx = served("sweep-refuse");
        let session = fx.session();
        let saved = run(
            &session,
            "sweep_save",
            json!({"name": "inn", "keyframes": inn()}),
        );
        assert_eq!(saved["ok"], true, "{saved}");
        let (server, _) = camera_game(&fx, 0, 0.0);
        for (args, why) in [
            (
                json!({"token": "tok", "sweep": "nope", "mode": "realtime"}),
                "no sweep named",
            ),
            (
                json!({"token": "tok", "sweep": "inn", "mode": "step"}),
                "frame step takes 1 to 240",
            ),
            (
                json!({"token": "tok", "sweep": "inn", "mode": "realtime", "fps": 30}),
                "frame step takes",
            ),
            (
                json!({"token": "tok", "sweep": "inn", "mode": "realtime", "tag": "a"}),
                "frame step's",
            ),
            (
                json!({"token": "tok", "sweep": "inn", "mode": "step", "fps": 4, "tag": "a/b"}),
                "tag",
            ),
            (
                json!({"token": "tok", "sweep": "inn", "mode": "step", "fps": 4, "method": "jxr"}),
                "method",
            ),
            (
                json!({"token": "tok", "sweep": "inn", "mode": "step", "fps": 4, "settle_ms": 10001}),
                "settle_ms",
            ),
            (
                json!({"token": "tok", "sweep": "inn", "mode": "step", "fps": 4}),
                "needs photo mode",
            ),
            (
                json!({"token": "nope", "sweep": "inn", "mode": "realtime"}),
                "token",
            ),
        ] {
            let out = run(&session, "sweep_play", args.clone());
            assert_eq!(out["refused"], true, "{args}: {out}");
            assert!(
                out["reason"].as_str().unwrap().contains(why),
                "{why}: {out}"
            );
        }
        assert!(server.join().unwrap().is_empty());
        assert!(rows(&fx, "SELECT id FROM sweep_runs").is_empty());
    }

    /// Feature tests 2: photo mode's collision can stop the camera short of a sweep's first pose, and a real-time play
    /// reads back only its end. The first pose is set and read back first: short, it is refused, nothing played.
    #[test]
    fn sweep_play_refuses_a_first_pose_the_camera_stopped_short_of() {
        let fx = served("sweep-wall");
        photo_mode(&fx, true);
        let session = fx.session();
        let keys = json!([
            {"t": 0, "pos": [1001, 0, 0], "rot": [0, 0, 0], "fov": 60},
            {"t": 1, "pos": [999, 0, 0], "rot": [0, 10, 0], "fov": 60},
        ]);
        let saved = run(
            &session,
            "sweep_save",
            json!({"name": "wall", "keyframes": keys, "world": {"paused": true}}),
        );
        assert_eq!(saved["ok"], true, "{saved}");
        let (server, _) = camera_game(&fx, 1, 0.0);
        let out = run(
            &session,
            "sweep_play",
            json!({"token": "tok", "sweep": "wall", "mode": "realtime"}),
        );
        assert_eq!(out["refused"], true, "{out}");
        let why = out["reason"].as_str().unwrap();
        assert!(
            why.contains("first pose")
                && why.contains("Noclip Off")
                && why.contains("nothing played"),
            "{out}"
        );
        let verbs: Vec<Verb> = server.join().unwrap().into_iter().map(|r| r.verb).collect();
        assert!(
            matches!(verbs[..], [Verb::CamSet { .. }]),
            "no pause, no path: {verbs:?}"
        );
        assert!(rows(&fx, "SELECT id FROM sweep_runs").is_empty());
    }

    /// The owner, 0.1.28: the measuring line, and its wait, before a session's first sweep_play only; later ones start
    /// the path at once. The stand-in refuses notices, so the first waits for none either.
    #[test]
    fn only_a_session_s_first_sweep_play_sends_the_measuring_line() {
        let fx = served("sweep-line");
        photo_mode(&fx, true);
        fx.shared.set_hands(Some(crate::hands::Hands::Agent));
        let session = fx.session();
        let keys = json!([
            {"t": 0, "pos": [0, 0, 0], "rot": [0, 0, 0], "fov": 60},
            {"t": 0.1, "pos": [1, 0, 0], "rot": [0, 10, 0], "fov": 60},
        ]);
        let saved = run(
            &session,
            "sweep_save",
            json!({"name": "short", "keyframes": keys}),
        );
        assert_eq!(saved["ok"], true, "{saved}");
        // Each play: cam.set, cam.path, cam.get, cam.stop; the first one's notice before them.
        let (server, _) = camera_game(&fx, 1 + 4 + 4, 0.0);
        let args = json!({"token": "tok", "sweep": "short", "mode": "realtime"});
        let first = run(&session, "sweep_play", args.clone());
        let second = run(&session, "sweep_play", args);
        assert!(
            first["ok"] == true && first["status"].is_string(),
            "{first}"
        );
        assert!(
            second["ok"] == true && second.get("status").is_none(),
            "{second}"
        );
        let verbs: Vec<Verb> = server.join().unwrap().into_iter().map(|r| r.verb).collect();
        let notices = verbs
            .iter()
            .filter(|v| matches!(v, Verb::Notice { .. }))
            .count();
        assert!(
            notices == 1 && matches!(verbs[0], Verb::Notice { .. }),
            "{verbs:?}"
        );
        // And step mode's size estimate: 121 4K frames.
        assert_eq!(frames_bytes(121, 3840, 2160, "hdr"), 4_275_431_424);
        assert_eq!(frames_bytes(121, 3840, 2160, "sdr"), 1_866_737_664);
    }

    /// A package that refuses cam.path, after sweep_play paused the game, once answered `refused` though the
    /// pause had been toggled (modQuen's pause is one flag: an earlier game_pause on was lifted). Now it is Failed, with
    /// the pause as it is now.
    #[test]
    fn a_refused_path_after_the_pause_is_a_failure_that_names_the_pause() {
        let fx = served("sweep-old");
        photo_mode(&fx, true);
        let session = fx.session();
        let keys = json!([
            {"t": 0, "pos": [OLD_PACKAGE, 0, 0], "rot": [0, 0, 0], "fov": 60},
            {"t": 1, "pos": [0, 0, 0], "rot": [0, 10, 0], "fov": 60},
        ]);
        let saved = run(
            &session,
            "sweep_save",
            json!({"name": "old", "keyframes": keys, "world": {"paused": true}}),
        );
        assert_eq!(saved["ok"], true, "{saved}");
        let (server, _) = camera_game(&fx, 4, 0.0);
        let out = run(
            &session,
            "sweep_play",
            json!({"token": "tok", "sweep": "old", "mode": "realtime"}),
        );
        assert_eq!(
            (out["ok"].clone(), out["refused"].clone()),
            (json!(false), json!(false)),
            "{out}"
        );
        assert!(out["reason"].as_str().unwrap().contains("0.1.2"), "{out}");
        assert_eq!(out["pause"], "quen's pause is off", "{out}");
        let verbs: Vec<Verb> = server.join().unwrap().into_iter().map(|r| r.verb).collect();
        assert_eq!(verbs[1], Verb::Pause { on: true }, "{verbs:?}");
        assert_eq!(verbs.last(), Some(&Verb::Pause { on: false }), "{verbs:?}");
    }

    /// A cancel between frames stops the run there: the path stopped, the pause lifted, the frames so far recorded.
    #[test]
    fn a_cancelled_frame_step_stops_and_undoes() {
        let fx = served("sweep-cancel");
        photo_mode(&fx, true);
        let session = fx.session();
        let two = json!([
            {"t": 0, "pos": [0, 0, 0], "rot": [0, 0, 0], "fov": 60},
            {"t": 1, "pos": [1, 0, 0], "rot": [0, 10, 0], "fov": 60},
        ]);
        assert_eq!(
            run(
                &session,
                "sweep_save",
                json!({"name": "two", "keyframes": two})
            )["ok"],
            true
        );
        let (server, _) = camera_game(&fx, 3 + 2 + 2, 0.0);
        let ids: Vec<i64> = (0..2)
            .map(|k| capture(&fx, &format!("s1/c{k}.png")))
            .collect();
        // The client goes away during the second frame's capture, as a cancel would land.
        let conn = crate::mcp::Conn::nobody();
        let (gone, mut taken) = (conn.clone(), 0);
        GRAB.set(Some(Box::new(move |_| {
            taken += 1;
            if taken == 2 {
                gone.closed.store(true, Ordering::SeqCst);
            }
            Ok(ids[taken - 1])
        })));
        let mut call = session.call(conn, Value::Null, None);
        let args = json!({"token": "tok", "sweep": "two", "mode": "step", "fps": 10});
        let done = super::call(&session, &mut call, "sweep_play", &args);
        let out = session.envelope("sweep_play", call, done);
        GRAB.set(None);
        assert_eq!(
            (out["ok"].as_bool(), out["refused"].as_bool()),
            (Some(false), Some(false)),
            "{out}"
        );
        assert!(
            out["reason"]
                .as_str()
                .unwrap()
                .contains("cancelled after 2 of 11 frames"),
            "{out}"
        );
        let verbs: Vec<Verb> = server.join().unwrap().into_iter().map(|r| r.verb).collect();
        assert_eq!(
            verbs[verbs.len() - 2..],
            [Verb::CamStop {}, Verb::Pause { on: false }]
        );
        // Feature tests 2: it said the start's "pause is on" after the undo had lifted it.
        assert_eq!(out["pause"], "quen's pause is off", "{out}");
        assert_eq!(rows(&fx, "SELECT count(*) FROM sweep_frames")[0][0], 2);
    }

    /// Frame step through a real grab of a window this test shows itself (never the game or the desktop): the stand-in
    /// game keeps the frame it set; the grab paints that frame's colour into the window and takes it with the capture
    /// code's own PrintWindow behind the pin. Every capture must be of the frame set just before it, in its colour. It
    /// shows a small window for about five seconds, so it runs only when asked:
    /// `cargo test -p quen -- --ignored frame_step_captures`.
    #[test]
    #[ignore = "shows a window on the desktop"]
    fn frame_step_captures_a_window_of_its_own() {
        use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
        use windows::Win32::Graphics::Gdi::{
            CreateSolidBrush, DeleteObject, HGDIOBJ, RDW_ERASE, RDW_INVALIDATE, RDW_UPDATENOW,
            RedrawWindow,
        };
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DestroyWindow, GCLP_HBRBACKGROUND, RegisterClassW,
            SetClassLongPtrW, UnregisterClassW, WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
            WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
        };
        use windows::core::w;

        unsafe extern "system" fn proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
            // SAFETY: the arguments Windows passed.
            unsafe { DefWindowProcW(hwnd, msg, w, l) }
        }
        let colour = |n: u32| [(n * 37 % 256) as u8, (n * 91 % 256) as u8, 200];
        let fx = served("sweep-window");
        photo_mode(&fx, true);
        let session = fx.session();
        let keys = json!([
            {"t": 0, "pos": [0, 0, 0], "rot": [0, 0, 0], "fov": 60},
            {"t": 1, "pos": [1, 0, 0], "rot": [0, 30, 0], "fov": 60},
        ]);
        assert_eq!(
            run(
                &session,
                "sweep_save",
                json!({"name": "win", "keyframes": keys})
            )["ok"],
            true
        );
        let (server, frame) = camera_game(&fx, 3 + 11 + 2, 0.0);
        // The window per-monitor DPI aware, as the grab measures it: else, at 125 % display scaling, its 320x200 are
        // 400x250 physical pixels (feature tests 2).
        let _dpi = capture::Dpi::per_monitor();
        // SAFETY: a class and window of this test's own, destroyed at its end on this thread.
        let hwnd = unsafe {
            let class = WNDCLASSW {
                lpfnWndProc: Some(proc),
                hbrBackground: CreateSolidBrush(COLORREF(0)),
                lpszClassName: w!("quen-sweep-test"),
                ..Default::default()
            };
            assert_ne!(RegisterClassW(&class), 0);
            CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                w!("quen-sweep-test"),
                w!("quen sweep test"),
                WS_POPUP | WS_VISIBLE,
                64,
                64,
                320,
                200,
                None,
                None,
                None,
                None,
            )
            .unwrap()
        };
        let db = fx
            .shared
            .config()
            .as_ref()
            .as_ref()
            .unwrap()
            .state_dir()
            .join("quen.db");
        GRAB.set(Some(Box::new(move |tag| {
            let n = frame.load(Ordering::SeqCst);
            assert!(
                tag.ends_with(&format!("-{n:05}")),
                "{tag} after the game set frame {n}"
            );
            let [r, g, b] = colour(n);
            // SAFETY: this test's own window; the class's previous brush is released once replaced.
            unsafe {
                let brush = CreateSolidBrush(COLORREF(u32::from_le_bytes([r, g, b, 0])));
                let old = SetClassLongPtrW(hwnd, GCLP_HBRBACKGROUND, brush.0 as isize);
                let _ = DeleteObject(HGDIOBJ(old as *mut _));
                let _ = RedrawWindow(
                    Some(hwnd),
                    None,
                    None,
                    RDW_INVALIDATE | RDW_ERASE | RDW_UPDATENOW,
                );
            }
            let (width, height, px) =
                capture::window_frame(&[std::process::id()]).map_err(Done::Refused)?;
            assert_eq!((width, height), (320, 200));
            let centre = (height / 2 * width + width / 2) * 4;
            assert_eq!(px[centre..centre + 3], [b, g, r], "frame {n}: BGRA");
            let row = Capture {
                boot_id: Some(1),
                path: format!("s1/{tag}.bgra"),
                sha256: sha256_hex(&px),
                bytes: px.len() as i64,
                kind: "bgra".into(),
                t: "2026-10-18T00:00:00Z".into(),
                ..Capture::default()
            };
            Ok(Ledger::open(&db).unwrap().add_capture(&row).unwrap())
        })));
        let args =
            json!({"token": "tok", "sweep": "win", "mode": "step", "fps": 10, "tag": "win-a"});
        let out = run(&session, "sweep_play", args);
        GRAB.set(None);
        // SAFETY: as above.
        unsafe {
            let _ = DestroyWindow(hwnd);
            let _ = UnregisterClassW(w!("quen-sweep-test"), Some(HINSTANCE::default()));
        }
        assert_eq!(out["ok"], true, "{out}");
        assert_eq!(out["frames"], 11, "{out}");
        assert_eq!(server.join().unwrap().len(), 3 + 11 + 2);
        let shas = rows(
            &fx,
            "SELECT count(DISTINCT c.sha256) FROM sweep_frames f JOIN captures c ON c.id = f.capture_id",
        );
        assert_eq!(shas[0][0], 11, "every frame its own colour");
    }
}
