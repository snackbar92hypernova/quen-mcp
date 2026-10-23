//! game_freecam: the game's own developer free camera (CR4Game's EnableFreeCamera, MoveFreeCamera,
//! GetFreeCameraPosition, IsFreeCameraEnabled), quen-w3 0.1.17's `freecam`, `freecam.set` and `freecam.get` verbs
//! (w3/README.md). Unlike photo mode it leaves the world running (the free-camera probe, boot 237: not paused, NPCs
//! walking, the player still), and the rendered camera equals it, so world_frame and game_camera's read follow it.
//! Every action reads the state back on the next tick. A camera only: no journal entry, no taint. quen keeps whether it last
//! read it on (`Live::freecam`): session_end, hands_give, and a run_plan that turned it on, turn it off; game_photo
//! enter is refused while it is on. quen knows only its own calls: one turned on through game_exec isn't seen.

use quen_core::bridge_log::{VALID, in_photo};
use quen_core::protocol::{MAX_ACTORS_RADIUS, Reply, Verb};
use quen_core::scene;
use quen_core::sweep::Pose;
use serde_json::{Map, Value, json};

use crate::camera::{OFF_MAX, off, off_json};
use crate::channel::REPLY_MAX;
use crate::doctor;
use crate::game::{self, Ctx, Fields, ctx, schema, text};
use crate::session::{Call, Done, Session};

/// The arguments the actions take, besides `token`.
const ARGS: [&str; 6] = ["action", "pos", "rot", "relative", "target", "radius"];
/// The radius the state counts actors within when status gives none (world_actors' default), and after the others.
const RADIUS: f64 = 30.0;
/// Past this distance from the player a result warns (boot 237).
const REACH_M: f64 = 100.0;
const REACH: &str = "the camera is over 100 m from the player. The world (NPCs, level of detail) lives around the \
    player: boot 237 counted 23 alive actors within 30 m of the free camera 50 m from him, 0 at 150 m (far-off \
    low-detail figures only), and saw a nearly black view at 400 m";
/// relative's defaults (metres), the height of its aim above the player's position, and how far above and below his z
/// it looks for the ground: the free-camera probe's qfcrel, whose aim put him at the frame's centre (boot 237).
const FORWARD: f64 = 10.0;
const UP: f64 = 2.0;
const AIM_UP: f64 = 1.7;
const GROUND_RANGE: f32 = 30.0;

pub fn tool() -> Value {
    let vec3 = |what: &str| json!({"type": "array", "items": {"type": "number"}, "minItems": 3, "maxItems": 3, "description": what});
    let metres = |what: &str| json!({"type": "number", "description": what});
    json!({
        "name": "game_freecam",
        "description": "The game's own free camera, in gameplay, quen-w3 0.1.17 or later: the world keeps running \
            (photo mode pauses it and has its own camera, game_camera's). on / off: EnableFreeCamera. set: pos and \
            rot, or relative to the player, aimed at 1.7 m above him. aim: look at target from pos (default: where \
            it is). status: radius. Each reads the state back on the next tick: on (IsFreeCameraEnabled), pos \
            (GetFreeCameraPosition: kept while off), camera (the rendered view, the free camera's while on), player, \
            paused, near_camera and near_player (alive actors within radius), distance_to_player_m (from camera); \
            past 100 m a warning. world_frame and game_camera's read follow it. A camera only: not journaled, taints \
            no save. session_end and hands_give turn it off. Refused, nothing changed: the user's controls (but status), no loaded game, on, \
            set or aim in photo mode, set or aim while off; with needs: an older package. Failed: no reply within \
            5 s, the read-back not on as asked, or off by over 0.01 m or 0.1 degrees.",
        "inputSchema": schema(json!({
            "action": {"type": "string", "enum": ["on", "off", "set", "aim", "status"], "description": "what to do"},
            "pos": vec3("set (with rot): [x, y, z] in metres; aim: where it looks from"),
            "rot": vec3("set: [pitch, yaw, roll] in degrees, as game_camera's"),
            "relative": {"type": "object", "additionalProperties": false, "properties": {
                "forward": metres("along the player's heading (default 10 when right is 0 too)"),
                "right": metres("to his right (default 0)"),
                "up": metres("above the navmesh there, else the ground a ray finds, else his z (default 2)"),
            }, "description": "set, instead of pos and rot: metres from the player"},
            "target": vec3("aim: the point [x, y, z] it looks at"),
            "radius": {"type": "number", "minimum": 0, "maximum": MAX_ACTORS_RADIUS, "description": "status: metres, above 0 (default 30)"},
        }), &["action"]),
    })
}

/// game_freecam's work: the arguments, quen's package at 0.1.17 or later, a loaded game in quen's state and, to place
/// the camera, no photo mode; then the action's verbs and the state read back.
pub(crate) fn call(session: &Session, call: &mut Call, args: &Value) -> Result<Fields, Done> {
    let cx = ctx(session, args, &ARGS)?;
    let action = text(args, "action")?;
    let takes: &[&str] = match action {
        "on" | "off" => &[],
        "set" => &["pos", "rot", "relative"],
        "aim" => &["pos", "target"],
        "status" => &["radius"],
        _ => return Err(format!("action is on, off, set, aim or status, not {action:?}").into()),
    };
    if let Some(key) = ARGS[1..]
        .iter()
        .find(|k| args.get(**k).is_some() && !takes.contains(k))
    {
        return Err(format!("game_freecam {action} takes no {key}").into());
    }
    let (pos, rot, target) = (
        vec3(args, "pos")?,
        vec3(args, "rot")?,
        vec3(args, "target")?,
    );
    let relative = relative(args.get("relative"))?;
    let radius = match args.get("radius") {
        None => RADIUS,
        Some(v) => v
            .as_f64()
            .filter(|r| *r > 0.0 && *r <= f64::from(MAX_ACTORS_RADIUS))
            .ok_or(format!(
                "radius must be metres above 0, at most {MAX_ACTORS_RADIUS}"
            ))?,
    };
    match (action, pos.is_some(), rot.is_some(), relative.is_some()) {
        ("set", true, true, false) | ("set", false, false, true) => {}
        ("set", ..) => {
            return Err("game_freecam set takes pos and rot, or relative"
                .to_string()
                .into());
        }
        ("aim", ..) if target.is_none() => {
            return Err("game_freecam aim needs target".to_string().into());
        }
        _ => {}
    }
    doctor::installed(&[&doctor::PACKAGE], cx.config, "game_freecam")?;
    doctor::package_at_least(cx.config, doctor::BEFORE_FREECAM, "game_freecam", "0.1.17")?;
    let last = cx.last_flags();
    if last.is_none_or(|f| f & VALID == 0) {
        let why = format!(
            "quen's state shows {}: game_freecam needs a loaded game. Nothing sent",
            game::shown(last)
        );
        return Err(why.into());
    }
    if matches!(action, "on" | "set" | "aim") && last.is_some_and(in_photo) {
        let why = format!(
            "quen's state shows photo mode, which has its own camera (game_camera sets it): game_freecam {action} \
             works in gameplay. Nothing sent"
        );
        return Err(why.into());
    }
    call.lap("check");
    let mut fields = Map::new();
    // Where set and aim put it: the state first, where they start from the player or the camera's own place.
    let asked = match (action, relative, pos.zip(rot)) {
        ("set", None, Some(pose)) => Some(pose),
        ("set", Some(rel), _) => Some(beside(&cx, &placed(&cx, &fields)?, rel, &mut fields)?),
        ("aim", ..) => {
            let from = match pos {
                Some(p) => p,
                None => three(&placed(&cx, &fields)?["pos"]).unwrap_or_default(),
            };
            let target = target.unwrap_or_default();
            let rot = scene::aim(from, target).ok_or(
                "target is within a millimetre of pos in xy: nothing to aim at".to_string(),
            )?;
            Some((from, rot))
        }
        _ => None,
    };
    let verb = match (action, asked) {
        ("on" | "off", _) => Some(Verb::FreeCam { on: action == "on" }),
        (_, Some((pos, rot))) => Some(Verb::FreeCamSet {
            pos: pos.map(|x| x as f32),
            rot: rot.map(|x| x as f32),
        }),
        _ => None,
    };
    if let Some(verb) = verb {
        let reply = send(&cx, verb, &fields);
        call.lap("reply");
        // An on that may have run is noted at once, so the end turns it off even if the read-back fails.
        let ran = !matches!(reply, Err(Done::Refused(_) | Done::RefusedWith(..)));
        if action == "on" && ran {
            cx.live.freecam.set(true);
        }
        fields.insert("reply".into(), json!(reply?.text));
    }
    let state = get(&cx, radius, &fields)?;
    call.lap("read");
    let on = state["on"] == true;
    fill(&mut fields, state);
    if matches!(action, "on" | "off") && on != (action == "on") {
        let why = format!(
            "the game answered, but IsFreeCameraEnabled reads {on} on the next tick: the free camera isn't {action}"
        );
        return Err(Done::Failed(why, fields));
    }
    if let Some((pos, rot)) = asked {
        fields.insert("asked".into(), json!({"pos": pos, "rot": rot}));
        let pose = |pos, rot| Pose { pos, rot, fov: 0.0 };
        let got = pose(
            three(&fields["pos"]).unwrap_or_default(),
            three(&fields["camera"]["rot"]).unwrap_or_default(),
        );
        let (m, deg) = off(&got, &pose(pos, rot));
        fields.insert("off".into(), off_json((m, deg)));
        if !on || m > OFF_MAX.0 || deg > OFF_MAX.1 {
            let why = format!(
                "the read-back on the next tick isn't the pose asked (on {on}, off {m:.3} m, {deg:.2} deg): pos is \
                 GetFreeCameraPosition, camera.rot the rendered view's"
            );
            return Err(Done::Failed(why, fields));
        }
    }
    Ok(fields)
}

/// The state, which must show the free camera on: set relative and aim without pos start from it. Refused, nothing
/// changed, while it is off.
fn placed(cx: &Ctx, fields: &Fields) -> Result<Value, Done> {
    let state = get(cx, RADIUS, fields)?;
    if state["on"] != true {
        let why = "the free camera is off: game_freecam on first. Nothing changed";
        return Err(Done::RefusedWith(
            why.into(),
            state.as_object().cloned().unwrap_or_default(),
        ));
    }
    Ok(state)
}

/// relative's pose beside the player in `state`: `forward` m along his heading and `right` m to his right, `up` m above
/// the ground there (the navmesh within 30 m of his z, else what a ray down finds, else his z: `ground` says which),
/// aimed at 1.7 m above his position. The forward of a heading h is (-sin h, cos h), its right (cos h, sin h).
fn beside(
    cx: &Ctx,
    state: &Value,
    [forward, right, up]: [f64; 3],
    fields: &mut Fields,
) -> Result<([f64; 3], [f64; 3]), Done> {
    let p = three(&state["player"]["pos"]).unwrap_or_default();
    let f = scene::forward(state["player"]["heading"].as_f64().unwrap_or_default());
    let at = [
        p[0] + forward * f[0] + right * f[1],
        p[1] + forward * f[1] - right * f[0],
    ];
    let verb = Verb::SceneGround {
        xyz: vec![at[0] as f32, at[1] as f32, p[2] as f32],
        range: GROUND_RANGE,
        radius: 0.0,
        search: 0.0,
    };
    let ground = send(cx, verb, fields)?.data.unwrap_or_default();
    let (z, from) = match (
        ground[0]["nav_z"].as_f64(),
        ground[0]["ground"]["pos"][2].as_f64(),
    ) {
        (Some(z), _) => (z, "navmesh"),
        (None, Some(z)) => (z, "ray"),
        _ => (p[2], "player"),
    };
    fields.insert("ground".into(), json!({"z": z, "from": from}));
    let pos = [at[0], at[1], z + up];
    let rot = scene::aim(pos, [p[0], p[1], p[2] + AIM_UP])
        .ok_or("relative puts the camera within a millimetre of the player in xy".to_string())?;
    Ok((pos, rot))
}

/// relative's forward, right and up, its defaults filled in.
fn relative(v: Option<&Value>) -> Result<Option<[f64; 3]>, String> {
    let Some(v) = v else { return Ok(None) };
    let map = v.as_object().ok_or("relative must be an object")?;
    if let Some(key) = map
        .keys()
        .find(|k| !["forward", "right", "up"].contains(&k.as_str()))
    {
        return Err(format!("relative: unknown field {key:?}"));
    }
    let num = |key: &str, default: f64| match map.get(key) {
        None => Ok(default),
        Some(v) => v.as_f64().ok_or(format!("relative.{key} must be metres")),
    };
    let right = num("right", 0.0)?;
    let forward = num("forward", 0.0)?;
    let forward = if forward == 0.0 && right == 0.0 {
        FORWARD
    } else {
        forward
    };
    Ok(Some([forward, right, num("up", UP)?]))
}

/// One verb on a connection of its own, and its reply.
fn send(cx: &Ctx, verb: Verb, fields: &Fields) -> Result<Reply, Done> {
    let channel = cx
        .channel()
        .map_err(|why| Done::Failed(why, fields.clone()))?;
    channel
        .call(verb, REPLY_MAX)
        .map_err(|e| e.done(fields.clone()))
}

/// The free camera's state (freecam.get), which quen keeps whether it reads on.
fn get(cx: &Ctx, radius: f64, fields: &Fields) -> Result<Value, Done> {
    let reply = send(
        cx,
        Verb::FreeCamGet {
            radius: radius as f32,
        },
        fields,
    )?;
    let state = reply.data.unwrap_or_default();
    let whole = state["on"].is_boolean()
        && [
            &state["pos"],
            &state["camera"]["pos"],
            &state["camera"]["rot"],
            &state["player"]["pos"],
        ]
        .iter()
        .all(|v| three(v).is_some());
    if !whole {
        let why = format!("the game's read-back isn't the free camera's state: {state}");
        return Err(Done::Failed(why, fields.clone()));
    }
    cx.live.freecam.set(state["on"] == true);
    Ok(state)
}

/// The state's fields into the result: the rendered camera's angles in (-180, 180] (as world_frame gives them), its
/// distance to the player, and past [`REACH_M`] with the free camera on, the warning.
fn fill(fields: &mut Fields, mut state: Value) {
    for a in state["camera"]["rot"].as_array_mut().into_iter().flatten() {
        if let Some(deg) = a.as_f64() {
            *a = json!((scene::wrap(deg) * 1e4).round() / 1e4);
        }
    }
    let (camera, player) = (
        three(&state["camera"]["pos"]).unwrap_or_default(),
        three(&state["player"]["pos"]).unwrap_or_default(),
    );
    let d = (0..3)
        .map(|c| (camera[c] - player[c]).powi(2))
        .sum::<f64>()
        .sqrt();
    let on = state["on"] == true;
    fields.extend(state.as_object().cloned().unwrap_or_default());
    fields.insert(
        "distance_to_player_m".into(),
        json!((d * 100.0).round() / 100.0),
    );
    if on && d > REACH_M {
        fields.insert("warning".into(), json!(REACH));
    }
}

/// Turns the free camera off if quen last read it on: before session_end closes the game, before hands_give hands over,
/// and after a run_plan that turned it on. What happened, or why not; None when quen last read it off.
pub(crate) fn off_if_on(cx: &Ctx) -> Option<Result<String, String>> {
    if !cx.live.freecam.get() {
        return None;
    }
    let sent = cx.channel().and_then(|c| {
        c.call(Verb::FreeCam { on: false }, REPLY_MAX)
            .map_err(|e| e.why().to_string())
    });
    Some(sent.map(|reply| {
        cx.live.freecam.set(false);
        format!(
            "the free camera was on: quen turned it off (the game: {})",
            reply.text
        )
    }))
}

/// An optional `[a, b, c]` argument of numbers.
fn vec3(args: &Value, key: &str) -> Result<Option<[f64; 3]>, String> {
    match args.get(key) {
        None => Ok(None),
        Some(v) => three(v)
            .map(Some)
            .ok_or(format!("{key} must be three numbers")),
    }
}

fn three(v: &Value) -> Option<[f64; 3]> {
    let n: Vec<f64> = v
        .as_array()?
        .iter()
        .map(Value::as_f64)
        .collect::<Option<_>>()?;
    n.try_into().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::tests::{Answer, fake_game, fake_game_until, reply, served, token};
    use crate::game::tests::Fake;
    use quen_core::protocol::Request;
    use std::fs;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    /// Boot 237's player: Hierarch Square, heading 52.3.
    const PLAYER: [f64; 3] = [668.99, 1875.87, 9.94];
    const HEADING: f64 = 52.3;

    /// A stand-in game's free camera, as the probe saw it in boot 237: on places it 2 m above the player, a set while
    /// off is refused, the rendered camera follows it while on (else 2.5 m behind the player, yaw 350 as the director
    /// reads its angles), 23 actors near a camera within 100 m of the player and none past it, the navmesh 0.21 m below
    /// the player's z everywhere (boot 237: the camera at 11.73). Joined, the requests.
    fn game(fx: &Fake, conns: usize) -> std::thread::JoinHandle<Vec<Request>> {
        fake_game(&token(fx), conns, answer())
    }

    /// [`game`]'s answers; the status line's notices answered too (run_plan's).
    fn answer() -> Answer {
        let cam = Arc::new(Mutex::new((false, [0.0f64; 3], [0.0f64; 3])));
        Box::new(move |r| {
            let mut cam = cam.lock().unwrap();
            let (ok, text) = match &r.verb {
                Verb::FreeCam { on } => {
                    cam.0 = *on;
                    if *on {
                        cam.1 = [PLAYER[0], PLAYER[1], PLAYER[2] + 2.0];
                    }
                    (true, "EnableFreeCamera called")
                }
                Verb::FreeCamSet { .. } if !cam.0 => {
                    (false, "the free camera is off: freecam on first")
                }
                Verb::FreeCamSet { pos, rot } => {
                    cam.1 = pos.map(f64::from);
                    cam.2 = rot.map(f64::from);
                    (true, "MoveFreeCamera called")
                }
                Verb::SceneGround { xyz, .. } => {
                    let data = json!([{"nav_z": f64::from(xyz[2]) - 0.21, "ground": null}]);
                    return vec![reply(r.id, true, "read", Some(data))];
                }
                Verb::FreeCamGet { .. } => (true, "the free camera's state"),
                Verb::Notice { .. } | Verb::NoticeHide {} => {
                    return vec![reply(r.id, true, "notice shown", None)];
                }
                v => panic!("{v:?}"),
            };
            let (pos, rot) = match cam.0 {
                true => (cam.1, cam.2),
                false => (
                    [PLAYER[0], PLAYER[1] - 2.5, PLAYER[2] + 1.9],
                    [350.0, 0.0, 0.0],
                ),
            };
            let far = (0..3)
                .map(|c| (pos[c] - PLAYER[c]).powi(2))
                .sum::<f64>()
                .sqrt()
                > 100.0;
            let state = json!({"on": cam.0, "paused": false, "pos": cam.1, "camera": {"pos": pos, "rot": rot},
                    "player": {"pos": PLAYER, "heading": HEADING}, "radius": 30,
                    "near_camera": if far { 0 } else { 23 }, "near_player": 47});
            vec![reply(r.id, ok, text, ok.then_some(state))]
        })
    }

    fn verbs(server: std::thread::JoinHandle<Vec<Request>>) -> Vec<Verb> {
        server.join().unwrap().into_iter().map(|r| r.verb).collect()
    }

    /// on and off read the state back on the next tick; status reads it; off while quen read it on keeps quen's note
    /// right, which session_end and game_photo enter go by.
    #[test]
    fn on_and_off_read_the_state_back() {
        let fx = served("freecam-onoff");
        let server = game(&fx, 5);
        let mut session = fx.session();
        let run = |session: &mut Session, args: Value| {
            let mut call = session.call(crate::mcp::Conn::nobody(), Value::Null, None);
            let done = game::call(session, &mut call, "game_freecam", &args);
            session.envelope("game_freecam", call, done)
        };
        let on = run(&mut session, json!({"token": "tok", "action": "on"}));
        assert_eq!(on["ok"], true, "{on}");
        assert_eq!(
            (&on["on"], &on["paused"]),
            (&json!(true), &json!(false)),
            "{on}"
        );
        assert_eq!(on["distance_to_player_m"], 2.0, "{on}");
        assert!(on.get("warning").is_none(), "{on}");
        assert!(session.live.as_ref().unwrap().freecam.get());
        let status = run(
            &mut session,
            json!({"token": "tok", "action": "status", "radius": 50}),
        );
        assert_eq!(status["near_camera"], 23, "{status}");
        let off = run(&mut session, json!({"token": "tok", "action": "off"}));
        assert_eq!(off["on"], false, "{off}");
        // The view behind the player again, its yaw of 350 read as -10.
        assert_eq!(off["camera"]["rot"], json!([-10.0, 0.0, 0.0]), "{off}");
        assert!(!session.live.as_ref().unwrap().freecam.get());
        assert_eq!(
            verbs(server),
            [
                Verb::FreeCam { on: true },
                Verb::FreeCamGet { radius: 30.0 },
                Verb::FreeCamGet { radius: 50.0 },
                Verb::FreeCam { on: false },
                Verb::FreeCamGet { radius: 30.0 },
            ]
        );
    }

    /// set by numbers and relative to the player (boot 237's qfcrel(10, 0, 2): (661.08, 1881.99, 11.73) and rot
    /// (-0.53, -127.70), the navmesh's z plus 2), aim from where it is, each read back; past 100 m the warning; a set while off is
    /// refused by the game, relative while off by quen with the state.
    #[test]
    fn sets_read_back_and_warn_past_the_reach() {
        let fx = served("freecam-set");
        let server = game(&fx, 13);
        let off = fx.run(
            "game_freecam",
            json!({"token": "tok", "action": "set", "pos": [1, 2, 3], "rot": [0, 0, 0]}),
        );
        assert_eq!(off["refused"], true, "{off}");
        let rel = fx.run(
            "game_freecam",
            json!({"token": "tok", "action": "set", "relative": {}}),
        );
        assert!(
            rel["reason"]
                .as_str()
                .unwrap()
                .contains("game_freecam on first"),
            "{rel}"
        );
        assert_eq!(rel["on"], false, "{rel}");
        assert_eq!(
            fx.run("game_freecam", json!({"token": "tok", "action": "on"}))["ok"],
            true
        );
        let near = fx.run(
            "game_freecam",
            json!({"token": "tok", "action": "set", "relative": {}}),
        );
        assert_eq!(near["ok"], true, "{near}");
        let (pos, rot) = (
            three(&near["pos"]).unwrap(),
            three(&near["camera"]["rot"]).unwrap(),
        );
        let close = |a: f64, b: f64| (a - b).abs() < 0.01;
        assert!(
            close(pos[0], 661.08) && close(pos[1], 1881.99) && close(pos[2], 11.73),
            "{near}"
        );
        assert!(close(rot[0], -0.52) && close(rot[1], -127.7), "{near}");
        assert_eq!(near["ground"]["from"], "navmesh", "{near}");
        assert!(near.get("warning").is_none(), "{near}");
        let far = fx.run("game_freecam", json!({"token": "tok", "action": "set", "pos": [668.99, 2025.87, 30], "rot": [-5, 180, 0]}));
        assert_eq!(far["ok"], true, "{far}");
        assert!(
            far["warning"].as_str().unwrap().contains("0 at 150 m"),
            "{far}"
        );
        assert_eq!(far["near_camera"], 0, "{far}");
        assert_eq!(far["off"], json!({"pos_m": 0.0, "deg": 0.0}), "{far}");
        // aim from where it is (150 m north of the player) at him: facing south, looking down.
        let aim = fx.run(
            "game_freecam",
            json!({"token": "tok", "action": "aim", "target": PLAYER}),
        );
        assert_eq!(aim["ok"], true, "{aim}");
        let rot = three(&aim["asked"]["rot"]).unwrap();
        assert!(rot[1].abs() > 179.99 && rot[0] < 0.0, "{aim}");
        let sent = verbs(server);
        assert_eq!(sent.len(), 13, "{sent:?}");
        assert!(
            matches!(sent[5], Verb::SceneGround { range: 30.0, .. }),
            "{sent:?}"
        );
    }

    /// Refused, nothing sent: bad arguments, no loaded game, on, set or aim in photo mode (off and status run there),
    /// and the user's controls (the session thread refuses by hands::user_may: status is a read).
    #[test]
    fn game_freecam_refuses_before_sending() {
        let fx = served("freecam-refuse");
        let server = fake_game(&token(&fx), 0, Box::new(|_| Vec::new()));
        let log = fx.dir.join("game/bin/x64_dx12/W3QuenState.log");
        for args in [
            json!({"token": "tok"}),
            json!({"token": "tok", "action": "fly"}),
            json!({"token": "tok", "action": "on", "pos": [0, 0, 0]}),
            json!({"token": "tok", "action": "set", "pos": [0, 0, 0]}),
            json!({"token": "tok", "action": "set", "pos": [0, 0, 0], "rot": [0, 0, 0], "relative": {}}),
            json!({"token": "tok", "action": "set", "relative": {"back": 1}}),
            json!({"token": "tok", "action": "aim"}),
            json!({"token": "tok", "action": "status", "radius": 0}),
            json!({"token": "tok", "action": "status", "radius": 101}),
            json!({"token": "tok", "action": "status", "extra": 1}),
        ] {
            let out = fx.run("game_freecam", args.clone());
            assert_eq!(out["refused"], true, "{args}: {out}");
        }
        fs::write(&log, "push flags=129\n").unwrap();
        for args in [
            json!({"token": "tok", "action": "on"}),
            json!({"token": "tok", "action": "aim", "target": [0, 0, 0]}),
        ] {
            let out = fx.run("game_freecam", args);
            assert!(
                out["reason"]
                    .as_str()
                    .unwrap()
                    .contains("photo mode, which has its own camera"),
                "{out}"
            );
        }
        fs::write(&log, "push flags=0\n").unwrap();
        let menu = fx.run("game_freecam", json!({"token": "tok", "action": "status"}));
        assert!(
            menu["reason"]
                .as_str()
                .unwrap()
                .contains("needs a loaded game"),
            "{menu}"
        );
        assert!(server.join().unwrap().is_empty(), "nothing was sent");
        assert!(crate::hands::user_may(
            "game_freecam",
            &json!({"action": "status"})
        ));
        for action in ["on", "off", "set", "aim"] {
            assert!(
                !crate::hands::user_may("game_freecam", &json!({"action": action})),
                "{action}"
            );
        }
    }

    /// With the free camera on as quen last read it, game_photo enter is refused, nothing sent; session_end turns it
    /// off before it closes the game, and says so.
    #[test]
    fn photo_mode_waits_and_the_end_turns_it_off() {
        let fx = served("freecam-end");
        let server = game(&fx, 1);
        let mut session = fx.session();
        session.live.as_ref().unwrap().freecam.set(true);
        let mut call = session.call(crate::mcp::Conn::nobody(), Value::Null, None);
        let enter = game::call(
            &mut session,
            &mut call,
            "game_photo",
            &json!({"token": "tok", "action": "enter"}),
        );
        let enter = session.envelope("game_photo", call, enter);
        assert!(
            enter["reason"]
                .as_str()
                .unwrap()
                .contains("game_freecam off first"),
            "{enter}"
        );
        let call = session.call(crate::mcp::Conn::nobody(), Value::Null, None);
        let (done, each) =
            session.nested(&call, "session_end", &json!({"token": "tok"}), false, true);
        let out = session.envelope("session_end", each, done);
        assert!(
            out["freecam"]
                .as_str()
                .unwrap()
                .contains("quen turned it off"),
            "{out}"
        );
        assert_eq!(verbs(server), [Verb::FreeCam { on: false }]);
    }

    /// A run_plan whose step turned the free camera on turns it off after its teardown, and says so; one that found it
    /// on leaves it on.
    #[test]
    fn a_plan_that_turned_it_on_turns_it_off() {
        let fx = served("freecam-plan");
        let stop = Arc::new(AtomicBool::new(false));
        let server = fake_game_until(&token(&fx), answer(), stop.clone());
        let mut session = fx.session();
        fn plan(session: &mut Session, tag: &str, step: Value) -> Value {
            let plan = json!({"tag": tag, "steps": [{"game_freecam": step}]});
            let args = json!({"token": "tok", "plan": plan.to_string()});
            let mut call = session.call(crate::mcp::Conn::nobody(), Value::Null, None);
            let done = crate::plan::call(session, &mut call, &args);
            session.envelope("run_plan", call, done)
        }
        fn on(session: &Session) -> bool {
            session.live.as_ref().unwrap().freecam.get()
        }
        let out = plan(&mut session, "turned", json!({"action": "on"}));
        assert_eq!(out["ok"], true, "{out}");
        assert!(
            out["freecam"]
                .as_str()
                .unwrap()
                .contains("quen turned it off"),
            "{out}"
        );
        assert!(!on(&session));
        let mut call = session.call(crate::mcp::Conn::nobody(), Value::Null, None);
        let args = json!({"token": "tok", "action": "on"});
        assert!(matches!(
            game::call(&mut session, &mut call, "game_freecam", &args),
            Done::Ok(_)
        ));
        let out = plan(&mut session, "found", json!({"action": "status"}));
        assert_eq!(out["ok"], true, "{out}");
        assert!(
            out.get("freecam").is_none() && on(&session),
            "found on, left on: {out}"
        );
        stop.store(true, Ordering::Relaxed);
        let ons: Vec<bool> = verbs(server)
            .into_iter()
            .filter_map(|v| match v {
                Verb::FreeCam { on } => Some(on),
                _ => None,
            })
            .collect();
        assert_eq!(ons, [true, false, true]);
    }
}
