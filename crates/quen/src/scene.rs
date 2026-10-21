//! The scene tools (quen-w3 0.1.13; the runner design's phase 4): `world_frame`, `world_ground`, `world_ray` and
//! `world_nav` read the scene through the game's scripts (the channel's scene.frame, scene.ground, scene.ray and nav.line
//! verbs) and change nothing; `game_frame` sets the photo-mode camera to frame a subject from its sides, each pose
//! checked first with a sphere sweep and then set with no-clip; `game_place` puts the player on the navmesh. The
//! placement maths is quen-core's `scene`.

use std::thread::sleep;
use std::time::Duration;

use quen_core::bridge_log::{VALID, in_photo};
use quen_core::protocol::{
    MAX_ACTORS_RADIUS, MAX_FRAME, MAX_GROUND_RADIUS, MAX_GROUND_RANGE, MAX_GROUND_SEARCH,
    MAX_NAV_WIDTH, MAX_QUERIES, MAX_SWEEP_RADIUS, Reply, Verb,
};
use quen_core::scene::{self, Framing};
use quen_core::sweep::{FOV, Pose};
use serde_json::{Map, Value, json};

use crate::camera;
use crate::channel::REPLY_MAX;
use crate::doctor;
use crate::game::{Ctx, Fields, ctx, schema, shown};
use crate::session::{Call, Done, Session};

/// The runner design's per-tick caps for readings (§4.4), until an overhead boot measures them.
const RAYS_PER_TICK: usize = 64;
/// game_frame: at most this many sides in one call.
const MAX_SIDES: usize = 8;
/// game_frame's wait after each set by default, and at most (ms): the rendered camera follows a set one frame later.
const SETTLE_MS: u64 = 250;
const SETTLE_MAX_MS: u64 = 10_000;
/// game_place: how far above and below the z the navmesh is looked for (metres): vanilla's scene player corrects a walk
/// target's z within 5 m (scenePlayer.ws). Boot 190: within 10 m of a wall's foot it found an upper level 8.9 m up.
const PLACE_RANGE: f32 = 5.0;
/// game_place: how far the player may stand from the spot in xy when read back (metres), and how long the read-back
/// waits (the pose reads back from the game's next tick).
const PLACE_OFF_M: f64 = 0.5;
const PLACE_READ: Duration = Duration::from_millis(200);

pub fn tools() -> Vec<Value> {
    let vec3 = |what: &str| json!({"type": "array", "items": {"type": "number"}, "minItems": 3, "maxItems": 3, "description": what});
    let segments = json!({
        "type": "array", "minItems": 1, "maxItems": MAX_QUERIES,
        "description": "1 to 16 segments",
        "items": {
            "type": "object",
            "properties": {"from": vec3("[x, y, z] in metres"), "to": vec3("[x, y, z] in metres")},
            "required": ["from", "to"], "additionalProperties": false,
        },
    });
    let reads = "Changes nothing, taints no save; runs while the user has the controls. quen-w3 0.1.13 or later. \
        budget: the rays and queries of its one game tick (cap 64 and 16; their frame-time cost unmeasured). Refused, \
        nothing sent: wrong token, an older package (with needs), no loaded game in quen's state, a value past its \
        range. Failed: no reply within 5 s";
    vec![
        json!({
            "name": "world_frame",
            "description": format!("Read what the rendered camera sees: the alive actors within radius of it, the \
                player among them, nearest first, at most max; found counts all; camera is its pose (rot as \
                game_camera gives it). Each: id, name, \
                entity, player, pos, heading, head, torso (null without the bone), in_front, in_view (IsInView(): its \
                meaning unverified), x, y (the box's centre in the frame, -1 to 1; null behind it), share (its box's \
                corners in front, clipped to the frame, over its area; which box: quen_guide), box_whole, \
                distance, visible (head, torso, origin, as points asks: a ray from the camera hits no static geometry; \
                null: not cast), visible_points. {reads}."),
            "inputSchema": schema(json!({
                "radius": {"type": "number", "minimum": 0, "maximum": MAX_ACTORS_RADIUS, "description": "metres from the camera, above 0; default 30"},
                "max": {"type": "integer", "minimum": 1, "maximum": MAX_FRAME, "description": "how many, nearest first; default 16"},
                "points": {"type": "integer", "minimum": 0, "maximum": 3, "description": "rays per entity: head, torso, origin, in that order; default 3"},
            }), &[]),
        }),
        json!({
            "name": "world_ground",
            "description": format!("Read the ground at points: nav_z (NavigationComputeZ within range of the point's \
                z; null off the navmesh), ground (a ray down from z + range to z - range: the hit's pos and normal, or \
                null), water_level and water_depth (GetWaterLevel, GetWaterDepth: callers in the game's scripts treat a \
                depth above 0 as water), free (with radius above 0 and a navmesh z: NavigationCircleTest of that \
                radius there; else null), safe (with search above 0: NavigationFindSafeSpot within search, radius as \
                its personal space; null if none). Boot 190, range 10 at a wall's foot: the ray hit an overhang 8.8 m \
                up and nav_z was that level; a small range with z near the ground keeps both near z. {reads}."),
            "inputSchema": schema(json!({
                "points": {"type": "array", "minItems": 1, "maxItems": MAX_QUERIES, "items": vec3("[x, y, z] in metres: z where to look"), "description": "1 to 16 points"},
                "range": {"type": "number", "minimum": 0, "maximum": MAX_GROUND_RANGE, "description": "metres above and below z, above 0; default 10"},
                "radius": {"type": "number", "minimum": 0, "maximum": MAX_GROUND_RADIUS, "description": "metres; default 0 (no circle test)"},
                "search": {"type": "number", "minimum": 0, "maximum": MAX_GROUND_SEARCH, "description": "metres; default 0 (no search)"},
            }), &["points"]),
        }),
        json!({
            "name": "world_ray",
            "description": format!("Cast a ray from each segment's from to its to (StaticTrace), or with radius above \
                0 sweep a sphere (SweepTest), over the static collision groups (Static, Terrain, Foliage, RigidBody, \
                Dynamic, Destructible, Door, Platforms, Fence, Boat, BoatDocking; no characters, no water): hit, its \
                pos and normal, and distance from from. Line of sight: no hit. {reads}."),
            "inputSchema": schema(json!({
                "segments": segments,
                "radius": {"type": "number", "minimum": 0, "maximum": MAX_SWEEP_RADIUS, "description": "metres; default 0 (a ray)"},
            }), &["segments"]),
        }),
        json!({
            "name": "world_nav",
            "description": format!("Test a navmesh lane from each segment's from to its to, width as the radius \
                (each of NavigationLineTest, NavigationClearLineInDirection, NavigationClosestObstacleToLine): clear \
                (the lane is free), clear_in_direction and reach (the second's answer and its point, the closest on \
                the line it gives), edge_m (the third's distance; vanilla never calls it). A walking route's length is \
                no script's: check a route leg by leg. {reads}."),
            "inputSchema": schema(json!({
                "segments": segments,
                "width": {"type": "number", "minimum": 0, "maximum": MAX_NAV_WIDTH, "description": "metres, the lane's radius; default 0.4"},
            }), &["segments"]),
        }),
        json!({
            "name": "game_frame",
            "description": "Frame a subject with the photo-mode camera from one or more sides, in photo mode: the \
                player, or the actor of entity_id (world_actors' id, within 100 m of the player). Per side the camera \
                stands distance metres from the aim point (head, torso or origin) in xy, height above it, along the \
                subject's forward turned by the side (front 0, left 90, back 180, right -90, or degrees), looking at \
                it, no roll. First, for every side, a sphere sweep (clearance) from the aim point to the camera: refused, \
                nothing set, if one hits (sweep says where). Then no-clip on (photo mode's menu has it back when it \
                closes) and per side: set and read back (off), settle_ms, the world_frame reading of the subject, and \
                with tag a game_screenshot <tag>-<side>. Not journaled. quen-w3 0.1.13 or later. Refused, nothing set: \
                wrong token, the user has the controls, not photo mode, no such subject or aim bone, a bad value, a \
                tag taken, a sweep that hits. Failed: a set the game kept short, a capture or read that failed (sides \
                so far kept).",
            "inputSchema": schema(json!({
                "entity_id": {"type": "integer", "description": "the subject's id (world_actors, world_frame); default the player"},
                "sides": {"type": "array", "minItems": 1, "maxItems": MAX_SIDES, "items": {"type": "string"}, "description": "each front, left, back, right, or whole degrees from the subject's forward, + to its left (\"30\", \"-150\"); default [\"front\"]"},
                "distance": {"type": "number", "minimum": 0.5, "maximum": 50, "description": "metres in xy from the aim point; default 3"},
                "height": {"type": "number", "minimum": -10, "maximum": 10, "description": "metres above the aim point; default 0"},
                "fov": {"type": "number", "minimum": 1, "maximum": 179, "description": "degrees; default 40"},
                "aim": {"type": "string", "enum": ["head", "torso", "origin"], "description": "the point looked at; default torso"},
                "clearance": {"type": "number", "minimum": 0.05, "maximum": MAX_SWEEP_RADIUS, "description": "the sweep's sphere radius, metres; default 0.3"},
                "tag": {"type": "string", "pattern": "^[A-Za-z0-9_-]{1,58}$", "description": "capture each side as <tag>-<side> (a side in degrees: y<deg>)"},
                "settle_ms": {"type": "integer", "minimum": 0, "maximum": SETTLE_MAX_MS, "description": "wait after each set (default 250)"},
            }), &[]),
        }),
        json!({
            "name": "game_place",
            "description": "Place the player on the navmesh at pos: z from NavigationComputeZ within 5 m of pos's z \
                (default the player's), then a free circle of radius there (navmesh: nav_z, free, safe as read); with \
                search, the nearest safe spot within \
                it if the circle isn't free (moved_m). Facing face (a point) or yaw, default the current heading. Then \
                the teleport verb, as game_teleport sends it (journaled; every save the game writes after it counts as \
                tainted; sent: its pos and yaw), then placed: the player's pose read back (world_get; the game sets him \
                on the ground, under the navmesh z). quen-w3 0.1.13 or later. Refused, nothing \
                moved: wrong token, the user has the controls, no loaded game, no navmesh there, the circle not free \
                and no safe spot, a bad value. Failed: no reply, or the player read back more than 0.5 m away in xy.",
            "inputSchema": schema(json!({
                "pos": {"type": "array", "items": {"type": "number"}, "minItems": 2, "maxItems": 3, "description": "[x, y] or [x, y, z] in metres"},
                "face": {"type": "array", "items": {"type": "number"}, "minItems": 2, "maxItems": 2, "description": "[x, y]: face this point"},
                "yaw": {"type": "number", "description": "degrees, instead of face"},
                "radius": {"type": "number", "minimum": 0.1, "maximum": MAX_GROUND_RADIUS, "description": "the free circle, metres; default 0.5"},
                "search": {"type": "number", "minimum": 0, "maximum": MAX_GROUND_SEARCH, "description": "metres to look for a safe spot; default 0 (none)"},
            }), &["pos"]),
        }),
    ]
}

/// One of [`tools`], from game.rs's dispatch.
pub(crate) fn call(
    session: &Session,
    call: &mut Call,
    name: &str,
    args: &Value,
) -> Result<Fields, Done> {
    match name {
        "world_frame" => frame_read(session, call, args),
        "world_ground" => ground(session, call, args),
        "world_ray" | "world_nav" => ray(session, call, name, args),
        "game_frame" => frame(session, call, args),
        "game_place" => place(session, call, args),
        _ => Err(Done::Refused(format!("unknown tool {name:?}"))),
    }
}

/// Refused with needs, nothing sent, without quen-w3 0.1.13; refused unless quen's state shows a loaded game (and photo
/// mode when `photo`).
fn ready(cx: &Ctx, tool: &str, photo: bool) -> Result<(), Done> {
    doctor::installed(&[&doctor::PACKAGE], cx.config, tool)?;
    doctor::package_at_least(cx.config, doctor::BEFORE_SCENE, tool, "0.1.13")?;
    let last = cx.last_flags();
    if !last.is_some_and(|f| f & VALID != 0 && (!photo || in_photo(f))) {
        let need = if photo { "photo mode" } else { "a loaded game" };
        return Err(format!(
            "quen's state shows {}: {tool} needs {need}. Nothing sent",
            shown(last)
        )
        .into());
    }
    Ok(())
}

/// One verb on a connection of its own, its reply; the verb's own checks (quen-core's) before anything is sent.
fn send(cx: &Ctx, verb: Verb, fields: &Fields) -> Result<Reply, Done> {
    let channel = cx
        .channel()
        .map_err(|why| Done::Failed(why, fields.clone()))?;
    channel
        .call(verb, REPLY_MAX)
        .map_err(|e| e.done(fields.clone()))
}

/// A verb's reply data as an array of `n`.
fn items(reply: &Reply, n: usize, fields: &Fields) -> Result<Vec<Value>, Done> {
    match reply.data.as_ref().and_then(Value::as_array) {
        Some(a) if a.len() == n => Ok(a.clone()),
        _ => Err(Done::Failed(
            format!("the game's read-back isn't {n} items: {:?}", reply.data),
            fields.clone(),
        )),
    }
}

/// What a read cast in its tick, against the design's caps.
fn budget(rays: usize, queries: usize) -> Value {
    json!({"ticks": 1, "rays": rays, "queries": queries, "cap": {"rays": RAYS_PER_TICK, "queries": MAX_QUERIES},
           "cost_ms_per_frame": "unmeasured"})
}

/// An optional number argument within `range`, `default` when absent.
fn num(
    args: &Value,
    key: &str,
    default: f64,
    range: std::ops::RangeInclusive<f64>,
) -> Result<f64, String> {
    match args.get(key) {
        None => Ok(default),
        Some(v) => v.as_f64().filter(|x| range.contains(x)).ok_or(format!(
            "{key} must be a number from {} to {}",
            range.start(),
            range.end()
        )),
    }
}

/// An optional whole-number argument within `range`, `default` when absent.
fn whole(
    args: &Value,
    key: &str,
    default: u32,
    range: std::ops::RangeInclusive<u32>,
) -> Result<u32, String> {
    match args.get(key) {
        None => Ok(default),
        Some(v) => v
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| range.contains(n))
            .ok_or(format!(
                "{key} must be a whole number from {} to {}",
                range.start(),
                range.end()
            )),
    }
}

/// `v` as `N` numbers.
fn numbers<const N: usize>(v: &Value) -> Option<[f64; N]> {
    let n: Vec<f64> = v
        .as_array()?
        .iter()
        .map(Value::as_f64)
        .collect::<Option<_>>()?;
    n.try_into().ok()
}

/// `[x, y, z]` of a read-back, if it is one.
fn point(v: &Value) -> Option<[f64; 3]> {
    numbers::<3>(v)
}

/// 1 to [`MAX_QUERIES`] items of `key`, each read by `one`.
fn list<T>(
    args: &Value,
    key: &str,
    one: impl Fn(&Value) -> Option<T>,
    what: &str,
) -> Result<Vec<T>, String> {
    let bad = || format!("{key} must be 1 to {MAX_QUERIES} {what}");
    let a = args.get(key).and_then(Value::as_array).ok_or_else(bad)?;
    if a.is_empty() || a.len() > MAX_QUERIES {
        return Err(bad());
    }
    a.iter().map(|v| one(v).ok_or_else(bad)).collect()
}

/// A segment `{from, to}`.
fn segment(v: &Value) -> Option<([f64; 3], [f64; 3])> {
    let map = v.as_object()?;
    (map.len() == 2).then_some(())?;
    Some((point(&v["from"])?, point(&v["to"])?))
}

fn flat(v: impl IntoIterator<Item = f64>) -> Vec<f32> {
    v.into_iter().map(|x| x as f32).collect()
}

fn frame_read(session: &Session, call: &mut Call, args: &Value) -> Result<Fields, Done> {
    let cx = ctx(session, args, &["radius", "max", "points"])?;
    let radius = num(args, "radius", 30.0, 0.0..=f64::from(MAX_ACTORS_RADIUS))?;
    if radius <= 0.0 {
        return Err("radius must be above 0".to_string().into());
    }
    let verb = Verb::SceneFrame {
        radius: radius as f32,
        max: whole(args, "max", MAX_FRAME, 1..=MAX_FRAME)?,
        points: whole(args, "points", 3, 0..=3)?,
        at: None,
        entity_id: None,
    };
    ready(&cx, "world_frame", false)?;
    call.lap("check");
    let mut fields = Map::new();
    let reply = send(&cx, verb, &fields)?;
    call.lap("reply");
    let Some(Value::Object(data)) = reply.data else {
        return Err(Done::Failed(
            "the game's read-back isn't an object".into(),
            fields,
        ));
    };
    fields.extend(data);
    // Finding 91: pitch, yaw and roll in (-180, 180], as game_camera reads them (the director's pitch read 350 for -10).
    let rot = fields.get_mut("camera").and_then(|c| c.get_mut("rot"));
    for a in rot.and_then(Value::as_array_mut).into_iter().flatten() {
        if let Some(deg) = a.as_f64() {
            *a = json!((scene::wrap(deg) * 1e4).round() / 1e4);
        }
    }
    let entities = fields["entities"].as_array().cloned().unwrap_or_default();
    let rays = entities
        .iter()
        .map(|e| {
            ["head", "torso", "origin"]
                .iter()
                .filter(|k| !e["visible"][**k].is_null())
                .count()
        })
        .sum();
    fields.insert("budget".into(), budget(rays, entities.len()));
    Ok(fields)
}

fn ground(session: &Session, call: &mut Call, args: &Value) -> Result<Fields, Done> {
    let cx = ctx(session, args, &["points", "range", "radius", "search"])?;
    let points = list(args, "points", point, "points of [x, y, z]")?;
    let range = num(args, "range", 10.0, 0.0..=f64::from(MAX_GROUND_RANGE))?;
    let verb = Verb::SceneGround {
        xyz: flat(points.iter().flatten().copied()),
        range: range as f32,
        radius: num(args, "radius", 0.0, 0.0..=f64::from(MAX_GROUND_RADIUS))? as f32,
        search: num(args, "search", 0.0, 0.0..=f64::from(MAX_GROUND_SEARCH))? as f32,
    };
    check(&verb)?;
    ready(&cx, "world_ground", false)?;
    call.lap("check");
    let mut fields = Map::new();
    let reply = send(&cx, verb, &fields)?;
    call.lap("reply");
    let read = items(&reply, points.len(), &fields)?;
    let read: Vec<Value> = points
        .iter()
        .zip(read)
        .map(|(p, mut r)| {
            r["at"] = json!(p);
            r
        })
        .collect();
    fields.insert("points".into(), json!(read));
    fields.insert("budget".into(), budget(points.len(), points.len()));
    Ok(fields)
}

/// world_ray and world_nav: one verb per call over segments.
fn ray(session: &Session, call: &mut Call, name: &str, args: &Value) -> Result<Fields, Done> {
    let nav = name == "world_nav";
    let size = if nav { "width" } else { "radius" };
    let cx = ctx(session, args, &["segments", size])?;
    let segments = list(args, "segments", segment, "segments of {from, to}")?;
    let coords = flat(
        segments
            .iter()
            .flat_map(|(a, b)| a.iter().chain(b).copied()),
    );
    let verb = match nav {
        true => Verb::NavLine {
            segments: coords,
            width: num(args, "width", 0.4, 0.0..=f64::from(MAX_NAV_WIDTH))? as f32,
        },
        false => Verb::SceneRay {
            segments: coords,
            radius: num(args, "radius", 0.0, 0.0..=f64::from(MAX_SWEEP_RADIUS))? as f32,
        },
    };
    check(&verb)?;
    ready(&cx, name, false)?;
    call.lap("check");
    let mut fields = Map::new();
    let reply = send(&cx, verb, &fields)?;
    call.lap("reply");
    let read = items(&reply, segments.len(), &fields)?;
    let read: Vec<Value> = segments
        .iter()
        .zip(read)
        .map(|((a, b), mut r)| {
            r["from"] = json!(a);
            r["to"] = json!(b);
            r
        })
        .collect();
    fields.insert("segments".into(), json!(read));
    let rays = if nav { 0 } else { segments.len() };
    fields.insert("budget".into(), budget(rays, segments.len()));
    Ok(fields)
}

/// The protocol's own checks of a verb, as a refusal before anything is sent.
fn check(verb: &Verb) -> Result<(), String> {
    let request = quen_core::protocol::Request {
        id: 1,
        ttl_ms: 0,
        verb: verb.clone(),
    };
    quen_core::protocol::encode_request(&request)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// game_frame's subject: its world_frame entry, found within `radius` of the player (scene.frame around the player's
/// position): the player's own entry without an id, else the entity with that id.
fn subject(cx: &Ctx, id: Option<i32>, fields: &Fields) -> Result<Value, Done> {
    let world = send(cx, Verb::WorldGet {}, fields)?;
    let at = world.data.as_ref().and_then(|d| point(&d["player"]["pos"]));
    let at = at.ok_or_else(|| {
        Done::Failed(
            "world.get read back no player position".into(),
            fields.clone(),
        )
    })?;
    let verb = Verb::SceneFrame {
        radius: if id.is_some() { MAX_ACTORS_RADIUS } else { 2.0 },
        max: MAX_FRAME,
        points: 0,
        at: Some(at.map(|x| x as f32)),
        entity_id: id,
    };
    let reply = send(cx, verb, fields)?;
    let entities = reply
        .data
        .as_ref()
        .and_then(|d| d["entities"].as_array().cloned())
        .unwrap_or_default();
    let found = entities.into_iter().find(|e| match id {
        Some(id) => e["id"] == id,
        None => e["player"] == true,
    });
    found.ok_or_else(|| {
        let who = id.map_or("the player".to_string(), |id| {
            format!("no entity of id {id} within 100 m of the player")
        });
        Done::Refused(format!("{who}: not found by scene.frame. Nothing set"))
    })
}

/// A side of game_frame's `sides`: its label (also the capture's suffix) and degrees.
fn sides(args: &Value) -> Result<Vec<(String, f64)>, String> {
    let Some(v) = args.get("sides") else {
        return Ok(vec![("front".into(), 0.0)]);
    };
    let bad = || {
        format!(
            "sides: 1 to {MAX_SIDES} of front, left, back, right or whole degrees from -180 to 180"
        )
    };
    let a = v
        .as_array()
        .filter(|a| !a.is_empty() && a.len() <= MAX_SIDES)
        .ok_or_else(bad)?;
    let mut out: Vec<(String, f64)> = Vec::new();
    for s in a {
        let s = s.as_str().ok_or_else(bad)?;
        let side = match (scene::side(s), s.parse::<i32>()) {
            (Some(d), _) => (s.to_string(), d),
            (None, Ok(d)) if (-180..=180).contains(&d) => (format!("y{d}"), f64::from(d)),
            _ => return Err(bad()),
        };
        if out.iter().any(|(label, _)| *label == side.0) {
            return Err(format!("sides: {} twice", side.0));
        }
        out.push(side);
    }
    Ok(out)
}

/// The world_frame fields of a framing's reading.
fn reading(e: &Value) -> Value {
    let keys = [
        "in_front",
        "in_view",
        "x",
        "y",
        "share",
        "box_whole",
        "distance",
        "visible",
        "visible_points",
    ];
    json!(Map::from_iter(
        keys.iter().map(|k| (k.to_string(), e[*k].clone()))
    ))
}

fn frame(session: &Session, call: &mut Call, args: &Value) -> Result<Fields, Done> {
    let known = [
        "entity_id",
        "sides",
        "distance",
        "height",
        "fov",
        "aim",
        "clearance",
        "tag",
        "settle_ms",
    ];
    let cx = ctx(session, args, &known)?;
    let id = match args.get("entity_id") {
        None => None,
        Some(v) => Some(
            v.as_i64()
                .and_then(|n| i32::try_from(n).ok())
                .ok_or("entity_id must be a whole number in int's range".to_string())?,
        ),
    };
    let sides = sides(args)?;
    let framing = |side| -> Result<Framing, String> {
        Ok(Framing {
            side,
            distance: num(args, "distance", 3.0, 0.5..=50.0)?,
            height: num(args, "height", 0.0, -10.0..=10.0)?,
            fov: num(args, "fov", 40.0, FOV)?,
        })
    };
    framing(0.0)?;
    let aim = match args.get("aim").map(Value::as_str) {
        None => "torso",
        Some(Some(a @ ("head" | "torso" | "origin"))) => a,
        Some(_) => return Err("aim is head, torso or origin".to_string().into()),
    };
    let clearance = num(args, "clearance", 0.3, 0.05..=f64::from(MAX_SWEEP_RADIUS))?;
    let settle = Duration::from_millis(u64::from(whole(
        args,
        "settle_ms",
        SETTLE_MS as u32,
        0..=SETTLE_MAX_MS as u32,
    )?));
    let tag = match args.get("tag") {
        None => None,
        Some(v) => {
            let t = v.as_str().filter(|t| {
                (1..=58).contains(&t.len())
                    && t.chars()
                        .all(|c| c.is_ascii_alphanumeric() || "_-".contains(c))
            });
            Some(
                t.ok_or("tag: 1-58 letters, digits, _ or -".to_string())?
                    .to_string(),
            )
        }
    };
    if let Some(tag) = &tag {
        let dir = cx.config.capture_root().join(&cx.live.id);
        if let Some((label, _)) = sides
            .iter()
            .find(|(label, _)| dir.join(format!("{tag}-{label}.png")).exists())
        {
            return Err(format!("{tag}-{label}.png exists: pick a tag new in this session").into());
        }
    }
    ready(&cx, "game_frame", true)?;
    let mut fields = Map::new();
    let body = subject(&cx, id, &fields)?;
    let at = match aim {
        "origin" => point(&body["pos"]),
        bone => point(&body[bone]),
    };
    let at = at.ok_or_else(|| {
        format!("the subject has no {aim} bone: aim at origin, or at another point")
    })?;
    let heading = body["heading"].as_f64().unwrap_or_default();
    let subject_id = body["id"].as_i64().and_then(|n| i32::try_from(n).ok());
    fields.insert(
        "subject".into(),
        json!({"id": body["id"], "name": body["name"], "entity": body["entity"], "player": body["player"],
               "pos": body["pos"], "heading": body["heading"], "aim": aim, "aim_at": at}),
    );
    let poses: Vec<Pose> = sides
        .iter()
        .map(|(_, deg)| framing(*deg).map(|f| scene::frame_pose(at, heading, &f)))
        .collect::<Result<_, _>>()?;
    // Every pose's space checked before any is set: a sphere sweep from the aim point to the camera.
    let segments = flat(poses.iter().flat_map(|p| at.iter().chain(&p.pos).copied()));
    let sweep = send(
        &cx,
        Verb::SceneRay {
            segments,
            radius: clearance as f32,
        },
        &fields,
    )?;
    let swept = items(&sweep, poses.len(), &fields)?;
    let sweep: Vec<Value> = sides
        .iter()
        .zip(&swept)
        .map(|((label, _), r)| json!({"side": label, "clear": r["hit"] == false, "hit": r["pos"], "distance": r["distance"]}))
        .collect();
    fields.insert("sweep".into(), json!(sweep));
    let blocked: Vec<String> = sweep
        .iter()
        .filter(|s| s["clear"] == false)
        .map(|s| {
            format!(
                "{} at {} m",
                s["side"].as_str().unwrap_or_default(),
                s["distance"]
            )
        })
        .collect();
    if !blocked.is_empty() {
        let why = format!(
            "the sphere sweep (radius {clearance} m) from the subject's {aim} towards the camera hits geometry: {}. \
             Nothing set (a shorter distance, another height or side)",
            blocked.join(", ")
        );
        return Err(Done::RefusedWith(why, fields));
    }
    call.lap("check");
    let noclip = send(&cx, Verb::CamNoclip { on: true }, &fields)?;
    fields.insert("noclip".into(), json!(noclip.text));
    let mut done = Vec::new();
    for ((label, _), pose) in sides.iter().zip(&poses) {
        if call.stopped() {
            fields.insert("sides".into(), json!(done));
            return Err(Done::Failed(
                format!("cancelled after {} of {} sides", done.len(), sides.len()),
                fields,
            ));
        }
        let set = Verb::CamSet {
            pos: pose.pos.map(|x| x as f32),
            rot: pose.rot.map(|x| x as f32),
            fov: Some(pose.fov as f32),
        };
        let mut side = Map::from_iter([
            ("side".to_string(), json!(label)),
            ("pose".into(), camera::pose_json(pose)),
        ]);
        let failed =
            |why: String, side: Map<String, Value>, mut done: Vec<Value>, mut fields: Fields| {
                done.push(json!(side));
                fields.insert("sides".into(), json!(done));
                Done::Failed(why, fields)
            };
        let got = send(&cx, set, &fields)
            .and_then(|r| camera::read_back(&r).map_err(|why| Done::Failed(why, fields.clone())));
        let (_, got) = match got {
            Ok(got) => got,
            Err(e) => {
                return Err(failed(
                    format!("side {label}: {}", why(e)),
                    side,
                    done,
                    fields,
                ));
            }
        };
        side.insert("off".into(), camera::off_json(camera::off(&got, pose)));
        if let Some(why) = camera::short(&got, pose) {
            return Err(failed(format!("side {label}: {why}"), side, done, fields));
        }
        sleep(settle);
        let read = Verb::SceneFrame {
            radius: (pose_distance(pose, at) + 5.0).min(f64::from(MAX_ACTORS_RADIUS)) as f32,
            max: 1,
            points: 3,
            at: None,
            entity_id: subject_id,
        };
        let entity =
            send(&cx, read, &fields).map(|r| r.data.and_then(|d| d["entities"].get(0).cloned()));
        match entity {
            Ok(Some(e)) => side.insert("reading".into(), reading(&e)),
            Ok(None) => side.insert(
                "reading".into(),
                json!("the subject isn't within the read's radius of the camera"),
            ),
            Err(e) => {
                return Err(failed(
                    format!("side {label}, reading: {}", why(e)),
                    side,
                    done,
                    fields,
                ));
            }
        };
        if let Some(tag) = &tag {
            match camera::grab(session, &cx.live.token, &format!("{tag}-{label}"), "sdr") {
                // The 0.1.12 live check: a plan's result lacked them. Each side's file and row are game_frame's.
                Ok((id, path)) => {
                    call.row("captures", id);
                    if let Some(path) = path {
                        call.artifact(&path);
                    }
                    side.insert("capture_id".into(), json!(id))
                }
                Err(e) => {
                    return Err(failed(
                        format!("side {label}, capture: {}", why(e)),
                        side,
                        done,
                        fields,
                    ));
                }
            };
        }
        call.lap(&format!("side_{label}"));
        done.push(json!(side));
    }
    fields.insert("sides".into(), json!(done));
    Ok(fields)
}

/// The camera's distance from `at`.
fn pose_distance(p: &Pose, at: [f64; 3]) -> f64 {
    (0..3)
        .map(|c| (p.pos[c] - at[c]).powi(2))
        .sum::<f64>()
        .sqrt()
}

/// A failure's or refusal's reason.
fn why(done: Done) -> String {
    match done {
        Done::Refused(why)
        | Done::RefusedWith(why, _)
        | Done::Needs(why, _)
        | Done::Failed(why, _) => why,
        Done::Ok(_) => String::new(),
    }
}

fn place(session: &Session, call: &mut Call, args: &Value) -> Result<Fields, Done> {
    let cx = ctx(session, args, &["pos", "face", "yaw", "radius", "search"])?;
    let pos: Vec<f64> = args
        .get("pos")
        .and_then(Value::as_array)
        .filter(|a| (2..=3).contains(&a.len()))
        .and_then(|a| a.iter().map(Value::as_f64).collect())
        .ok_or("pos must be [x, y] or [x, y, z]".to_string())?;
    let face = match args.get("face") {
        None => None,
        Some(v) => Some(numbers::<2>(v).ok_or("face must be [x, y]".to_string())?),
    };
    let yaw = match args.get("yaw") {
        None => None,
        Some(v) => Some(
            v.as_f64()
                .filter(|y| y.is_finite())
                .ok_or("yaw must be a number".to_string())?,
        ),
    };
    if face.is_some() && yaw.is_some() {
        return Err("face or yaw, not both".to_string().into());
    }
    let radius = num(args, "radius", 0.5, 0.1..=f64::from(MAX_GROUND_RADIUS))?;
    let search = num(args, "search", 0.0, 0.0..=f64::from(MAX_GROUND_SEARCH))?;
    ready(&cx, "game_place", false)?;
    let mut fields = Map::new();
    let world = send(&cx, Verb::WorldGet {}, &fields)?
        .data
        .unwrap_or_default();
    let player = &world["player"];
    let z = match pos.get(2) {
        Some(z) => *z,
        None => {
            point(&player["pos"]).ok_or("world.get read back no player position".to_string())?[2]
        }
    };
    let verb = Verb::SceneGround {
        xyz: flat([pos[0], pos[1], z]),
        range: PLACE_RANGE,
        radius: radius as f32,
        search: search as f32,
    };
    check(&verb)?;
    let ground = send(&cx, verb, &fields)?;
    let ground = items(&ground, 1, &fields)?.remove(0);
    // What the placement reads: the navmesh. The read's own ground ray, from 5 m up, can hit an overhang (the 0.1.12 live
    // check: 15.236 at a wall's foot), so it isn't reported here.
    let navmesh = json!({"nav_z": ground["nav_z"], "free": ground["free"], "safe": ground["safe"]});
    fields.insert("navmesh".into(), navmesh);
    let Some(nav_z) = ground["nav_z"].as_f64() else {
        let why = format!(
            "no navmesh within {PLACE_RANGE} m of z {z} at x {}, y {}: nothing moved",
            pos[0], pos[1]
        );
        return Err(Done::RefusedWith(why, fields));
    };
    let mut spot = [pos[0], pos[1], nav_z];
    if ground["free"] == false {
        match point(&ground["safe"]) {
            Some(safe) if search > 0.0 => {
                let moved = ((safe[0] - spot[0]).powi(2) + (safe[1] - spot[1]).powi(2)).sqrt();
                fields.insert("moved_m".into(), json!((moved * 1e3).round() / 1e3));
                spot = safe;
            }
            _ => {
                let why = format!(
                    "the navmesh circle of radius {radius} m there isn't free{}: nothing moved",
                    if search > 0.0 {
                        ", and no safe spot was found within search"
                    } else {
                        " (search finds a safe spot near it)"
                    }
                );
                return Err(Done::RefusedWith(why, fields));
            }
        }
    }
    let yaw = match (face, yaw) {
        (Some(f), _) => scene::facing([spot[0], spot[1]], f)
            .ok_or("face is the spot itself: give another point, or yaw".to_string())?,
        (None, Some(y)) => scene::wrap(y),
        (None, None) => player["heading"].as_f64().unwrap_or_default(),
    };
    call.lap("check");
    let teleport = Verb::Teleport {
        pos: spot.map(|x| x as f32),
        rot: [0.0, yaw as f32, 0.0],
    };
    let what = format!(
        "channel {}",
        serde_json::to_string(&teleport).unwrap_or_default()
    );
    let (mut journal, step) = cx.journal(&what, None, true)?;
    let sent = send(&cx, teleport, &fields);
    let ran = !matches!(sent, Err(Done::Refused(_) | Done::RefusedWith(..)));
    if let Some(note) = cx.settle(&mut journal, step, ran) {
        fields.insert("journal_note".into(), json!(note));
    }
    let reply = sent?;
    fields.insert("reply".into(), json!(reply.text));
    fields.insert("sent".into(), json!({"pos": spot, "yaw": yaw}));
    call.lap("teleport");
    sleep(PLACE_READ);
    let back = send(&cx, Verb::WorldGet {}, &fields)?
        .data
        .unwrap_or_default();
    fields.insert("world".into(), back.clone());
    // Finding 91: the game sets the player on the ground below the navmesh z sent (0.4-1.3 m in boot 190).
    let placed = json!({"pos": back["player"]["pos"], "yaw": back["player"]["heading"]});
    fields.insert("placed".into(), placed);
    let off = point(&back["player"]["pos"])
        .map(|p| ((p[0] - spot[0]).powi(2) + (p[1] - spot[1]).powi(2)).sqrt());
    fields.insert("off_m".into(), json!(off.map(|o| (o * 1e3).round() / 1e3)));
    call.lap("read");
    match off {
        Some(off) if off <= PLACE_OFF_M => Ok(fields),
        _ => Err(Done::Failed(
            format!("the player read back more than {PLACE_OFF_M} m from the spot in xy (off_m)"),
            fields,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::tests::{fake_game, reply, served, token};
    use crate::game::tests::Fake;
    use quen_core::protocol::Request;
    use std::fs;

    fn run(session: &mut Session, tool: &str, args: Value) -> Value {
        let mut call = session.call(crate::mcp::Conn::nobody(), Value::Null, None);
        let done = crate::game::call(session, &mut call, tool, &args);
        session.envelope(tool, call, done)
    }

    /// quen's state log in the stand-in game's folder showing `flags`.
    fn flags(fx: &Fake, flags: i64) {
        let log = fx.dir.join("game/bin/x64_dx12/W3QuenState.log");
        fs::write(log, format!("push flags={flags}\n")).unwrap();
    }

    fn verbs(requests: Vec<Request>) -> Vec<Verb> {
        requests.into_iter().map(|r| r.verb).collect()
    }

    #[test]
    fn reads_send_their_verbs_and_give_the_game_s_read_back() {
        let fx = served("scene-reads");
        let server = fake_game(
            &token(&fx),
            4,
            Box::new(|r| {
                let data = match &r.verb {
                    Verb::SceneGround { .. } => {
                        json!([{"nav_z": 3.5, "ground": null, "water_level": 0,
                        "water_depth": 0, "free": true, "safe": null}])
                    }
                    Verb::SceneRay { .. } => json!([
                        {"hit": false, "pos": null, "normal": null, "distance": null},
                        {"hit": true, "pos": [1, 0, 1], "normal": [-1, 0, 0], "distance": 1.0}]),
                    Verb::NavLine { .. } => {
                        json!([{"clear": true, "clear_in_direction": true, "reach": [3, 0, 1], "edge_m": 0.7}])
                    }
                    _ => {
                        json!({"found": 1, "camera": {"pos": [0, 0, 2], "rot": [350.0001, 190, 0], "fov": 40},
                        "entities": [{"id": -7, "player": true,
                            "visible": {"head": true, "torso": false, "origin": null}}]})
                    }
                };
                vec![reply(r.id, true, "read", Some(data))]
            }),
        );
        let mut session = fx.session();
        let args = json!({"token": "tok", "points": [[1, 2, 3]], "radius": 0.5});
        let ground = run(&mut session, "world_ground", args);
        assert_eq!(ground["ok"], true, "{ground}");
        assert_eq!(ground["points"][0]["at"], json!([1.0, 2.0, 3.0]));
        assert_eq!(ground["points"][0]["free"], true);
        assert_eq!(ground["budget"]["rays"], 1);
        let segs =
            json!([{"from": [0, 0, 1], "to": [3, 0, 1]}, {"from": [0, 0, 1], "to": [0, 3, 1]}]);
        let args = json!({"token": "tok", "segments": segs, "radius": 0.3});
        let ray = run(&mut session, "world_ray", args);
        assert_eq!(ray["segments"][1]["hit"], true, "{ray}");
        assert_eq!(ray["segments"][1]["to"], json!([0.0, 3.0, 1.0]));
        assert_eq!(ray["budget"]["rays"], 2);
        assert_eq!(ray["budget"]["cap"]["rays"], 64);
        let one = json!([{"from": [0, 0, 1], "to": [3, 0, 1]}]);
        let nav = run(
            &mut session,
            "world_nav",
            json!({"token": "tok", "segments": one}),
        );
        assert_eq!(nav["segments"][0]["edge_m"], 0.7, "{nav}");
        let frame = run(
            &mut session,
            "world_frame",
            json!({"token": "tok", "radius": 10}),
        );
        assert_eq!(frame["entities"][0]["id"], -7, "{frame}");
        assert_eq!(
            frame["budget"]["rays"], 2,
            "two rays cast, one not: {frame}"
        );
        // Finding 91: the camera's angles as game_camera reads them.
        assert_eq!(
            frame["camera"]["rot"],
            json!([-9.9999, -170.0, 0.0]),
            "{frame}"
        );
        // Refused before anything is sent: the core's caps and shapes.
        for (tool, args) in [
            ("world_ground", json!({"token": "tok", "points": []})),
            ("world_ground", json!({"token": "tok", "points": [[1, 2]]})),
            (
                "world_ground",
                json!({"token": "tok", "points": [[1, 2, 3]], "range": 0}),
            ),
            (
                "world_ground",
                json!({"token": "tok", "points": vec![[0, 0, 0]; 17]}),
            ),
            (
                "world_ray",
                json!({"token": "tok", "segments": [{"from": [0, 0, 0]}]}),
            ),
            (
                "world_ray",
                json!({"token": "tok", "segments": one, "radius": 3}),
            ),
            (
                "world_nav",
                json!({"token": "tok", "segments": one, "width": 6}),
            ),
            ("world_frame", json!({"token": "tok", "radius": 0})),
            ("world_frame", json!({"token": "tok", "max": 17})),
            ("world_frame", json!({"token": "tok", "points": 4})),
        ] {
            let out = run(&mut session, tool, args.clone());
            assert_eq!(out["refused"], true, "{tool} {args}: {out}");
        }
        let sent = verbs(server.join().unwrap());
        let ground = Verb::SceneGround {
            xyz: vec![1.0, 2.0, 3.0],
            range: 10.0,
            radius: 0.5,
            search: 0.0,
        };
        let ray = Verb::SceneRay {
            segments: vec![0.0, 0.0, 1.0, 3.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 3.0, 1.0],
            radius: 0.3,
        };
        let nav = Verb::NavLine {
            segments: vec![0.0, 0.0, 1.0, 3.0, 0.0, 1.0],
            width: 0.4,
        };
        let frame = Verb::SceneFrame {
            radius: 10.0,
            max: 16,
            points: 3,
            at: None,
            entity_id: None,
        };
        assert_eq!(sent, [ground, ray, nav, frame]);
        // No loaded game in quen's state: refused, nothing sent.
        flags(&fx, 0);
        let out = run(&mut session, "world_frame", json!({"token": "tok"}));
        assert!(
            out["reason"]
                .as_str()
                .unwrap()
                .contains("needs a loaded game"),
            "{out}"
        );
    }

    /// A stand-in for the framing verbs: the subject Geralt at (100, 200, 30) facing +y, his torso 1.2 m up, and a wall
    /// at x = 98 (to his left) that sweeps ending past it hit. Each verb answered as modQuen does, its camera what was
    /// set.
    fn framing_game(fx: &Fake, conns: usize) -> std::thread::JoinHandle<Vec<Request>> {
        let camera = std::sync::Mutex::new(Value::Null);
        fake_game(
            &token(fx),
            conns,
            Box::new(move |r| {
                let body = |seen: bool| {
                    json!({"id": -7, "name": "Geralt", "entity": "geralt", "player": true,
                        "pos": [100, 200, 30], "heading": 0, "head": [100, 200, 31.7], "torso": [100, 200, 31.2],
                        "in_front": seen, "x": 0, "y": 0, "share": 0.2, "box_whole": true, "distance": 3,
                        "visible": {"head": seen, "torso": seen, "origin": seen}, "visible_points": 3})
                };
                let (ok, data) = match &r.verb {
                    Verb::WorldGet {} => (
                        true,
                        json!({"player": {"pos": [100, 200, 30], "heading": 0}}),
                    ),
                    Verb::SceneFrame { at: Some(_), .. } => {
                        (true, json!({"found": 1, "entities": [body(false)]}))
                    }
                    Verb::SceneFrame { .. } => {
                        (true, json!({"found": 1, "entities": [body(true)]}))
                    }
                    Verb::SceneRay { segments, .. } => {
                        let hit = |s: &[f32]| match s[3] < 98.0 {
                            true => {
                                json!({"hit": true, "pos": [98, s[4], s[5]], "normal": [1, 0, 0], "distance": 2})
                            }
                            false => {
                                json!({"hit": false, "pos": null, "normal": null, "distance": null})
                            }
                        };
                        (true, json!(segments.chunks(6).map(hit).collect::<Vec<_>>()))
                    }
                    Verb::CamSet { pos, rot, fov } => {
                        let mut cam = camera.lock().unwrap();
                        *cam = json!({"camera": "photomode", "pos": pos, "rot": rot, "fov": fov});
                        (true, cam.clone())
                    }
                    Verb::CamNoclip { .. } => (true, Value::Null),
                    _ => (false, Value::Null),
                };
                vec![reply(r.id, ok, "ok", (!data.is_null()).then_some(data))]
            }),
        )
    }

    #[test]
    fn frame_sets_each_side_after_checking_every_sweep() {
        let fx = served("scene-frame");
        flags(&fx, 129);
        // world.get, scene.frame (the subject), scene.ray (every side), cam.noclip, then cam.set and scene.frame a side.
        let server = framing_game(&fx, 4 + 2 * 3);
        let mut session = fx.session();
        let args = json!({"token": "tok", "sides": ["front", "back", "-90"], "distance": 3, "height": 0.5, "fov": 35,
                          "tag": "f"});
        let mut ids = 40;
        crate::camera::tests::set_grab(Some(Box::new(move |_| {
            ids += 1;
            Ok(ids)
        })));
        let out = run(&mut session, "game_frame", args);
        crate::camera::tests::set_grab(None);
        assert_eq!(out["ok"], true, "{out}");
        // The 0.1.12 live check: each side's capture row in game_frame's own envelope (a plan's merges them).
        assert_eq!(out["ledger"]["captures"], json!([41, 42, 43]), "{out}");
        assert_eq!(out["sides"][2]["capture_id"], 43, "{out}");
        assert_eq!(out["subject"]["aim_at"], json!([100.0, 200.0, 31.2]));
        let sides = out["sides"].as_array().unwrap();
        assert_eq!(sides.len(), 3);
        let pose = |i: usize| -> Vec<f64> {
            let p = &sides[i]["pose"];
            let pos = p["pos"].as_array().unwrap().iter();
            pos.chain(p["rot"].as_array().unwrap())
                .map(|v| v.as_f64().unwrap())
                .collect()
        };
        let close = |a: &[f64], b: &[f64]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-6);
        // Front: 3 m out along his forward (+y), 0.5 m above the torso, looking back at it, down by atan(0.5 / 3).
        let down = -(0.5f64).atan2(3.0).to_degrees();
        assert!(
            close(&pose(0), &[100.0, 203.0, 31.7, down, 180.0, 0.0]),
            "{:?}",
            pose(0)
        );
        assert!(
            close(&pose(1), &[100.0, 197.0, 31.7, down, 0.0, 0.0]),
            "{:?}",
            pose(1)
        );
        assert!(
            close(&pose(2), &[103.0, 200.0, 31.7, down, 90.0, 0.0]),
            "{:?}",
            pose(2)
        );
        assert_eq!(sides[2]["side"], "y-90");
        assert_eq!(sides[0]["reading"]["visible_points"], 3, "{out}");
        assert!(sides[0]["off"]["pos_m"].as_f64().unwrap() < 1e-3, "{out}");
        assert!(
            out["sweep"]
                .as_array()
                .unwrap()
                .iter()
                .all(|s| s["clear"] == true),
            "{out}"
        );
        let sent = verbs(server.join().unwrap());
        assert_eq!(sent[0], Verb::WorldGet {});
        assert!(
            matches!(
                sent[1],
                Verb::SceneFrame {
                    at: Some(_),
                    entity_id: None,
                    ..
                }
            ),
            "{:?}",
            sent[1]
        );
        assert!(
            matches!(&sent[2], Verb::SceneRay { segments, radius } if segments.len() == 18 && *radius == 0.3)
        );
        assert_eq!(sent[3], Verb::CamNoclip { on: true });
        assert!(matches!(sent[4], Verb::CamSet { .. }));
        assert!(matches!(
            sent[5],
            Verb::SceneFrame {
                entity_id: Some(-7),
                ..
            }
        ));
    }

    #[test]
    fn frame_refuses_a_blocked_side_and_sets_nothing() {
        let fx = served("scene-frame-wall");
        flags(&fx, 129);
        // world.get, scene.frame, scene.ray: then the refusal.
        let server = framing_game(&fx, 3);
        let mut session = fx.session();
        let args = json!({"token": "tok", "sides": ["front", "left"], "distance": 3});
        let out = run(&mut session, "game_frame", args);
        assert_eq!(out["refused"], true, "{out}");
        assert!(
            out["reason"].as_str().unwrap().contains("left at 2"),
            "{out}"
        );
        assert_eq!(out["sweep"][0]["clear"], true);
        assert_eq!(out["sweep"][1]["hit"][0], 98);
        let sent = verbs(server.join().unwrap());
        let set = |v: &Verb| matches!(v, Verb::CamSet { .. } | Verb::CamNoclip { .. });
        assert!(!sent.iter().any(set), "{sent:?}");
        // Refused before anything is sent: bad sides and values, then outside photo mode.
        for args in [
            json!({"token": "tok", "sides": ["up"]}),
            json!({"token": "tok", "sides": ["front", "front"]}),
            json!({"token": "tok", "sides": ["200"]}),
            json!({"token": "tok", "sides": []}),
            json!({"token": "tok", "sides": [90]}),
            json!({"token": "tok", "distance": 0.1}),
            json!({"token": "tok", "aim": "feet"}),
            json!({"token": "tok", "tag": "a b"}),
            json!({"token": "tok", "entity_id": 1.5}),
        ] {
            let out = run(&mut session, "game_frame", args.clone());
            assert_eq!(out["refused"], true, "{args}: {out}");
        }
        flags(&fx, 1);
        let out = run(&mut session, "game_frame", json!({"token": "tok"}));
        assert!(
            out["reason"].as_str().unwrap().contains("needs photo mode"),
            "{out}"
        );
    }

    #[test]
    fn place_stands_the_player_on_the_navmesh_facing_a_point() {
        let fx = served("scene-place");
        let read = |nav_z: Value, free: Value, safe: Value| json!([{"nav_z": nav_z, "ground": null, "water_level": 0, "water_depth": 0, "free": free, "safe": safe}]);
        let ground = std::sync::Mutex::new(vec![
            read(json!(30.25), json!(true), Value::Null),
            read(json!(30.25), json!(false), json!([11, 20, 30.5])),
            read(Value::Null, Value::Null, Value::Null),
        ]);
        let at = std::sync::Mutex::new(json!([0, 0, 29]));
        let heading = std::sync::Mutex::new(json!(45));
        let server = fake_game(
            &token(&fx),
            // Placed: world.get, scene.ground, teleport, world.get; the safe spot's four; the refusal's two.
            10,
            Box::new(move |r| {
                let data = match &r.verb {
                    Verb::WorldGet {} => Some(
                        json!({"player": {"pos": *at.lock().unwrap(), "heading": *heading.lock().unwrap()}}),
                    ),
                    Verb::SceneGround { .. } => Some(ground.lock().unwrap().remove(0)),
                    Verb::Teleport { pos, rot } => {
                        // The game sets the player 0.25 m below the navmesh z, on the ground, turned to the yaw.
                        *at.lock().unwrap() = json!([pos[0], pos[1], pos[2] - 0.25]);
                        *heading.lock().unwrap() = json!(rot[1]);
                        None
                    }
                    _ => None,
                };
                vec![reply(r.id, true, "ok", data)]
            }),
        );
        let mut session = fx.session();
        let args = json!({"token": "tok", "pos": [10, 20], "face": [10, 30]});
        let out = run(&mut session, "game_place", args);
        assert_eq!(out["ok"], true, "{out}");
        assert_eq!(out["sent"], json!({"pos": [10.0, 20.0, 30.25], "yaw": 0.0}));
        // Finding 91: placed is where the game put the player, read back.
        assert_eq!(
            out["placed"],
            json!({"pos": [10.0, 20.0, 30.0], "yaw": 0.0})
        );
        let args = json!({"token": "tok", "pos": [10, 20, 29], "search": 3});
        let out = run(&mut session, "game_place", args);
        assert_eq!(out["ok"], true, "{out}");
        assert_eq!(out["moved_m"], 1.0);
        assert_eq!(
            out["sent"]["yaw"], 0.0,
            "the heading kept: the first place turned him to 0"
        );
        let out = run(
            &mut session,
            "game_place",
            json!({"token": "tok", "pos": [50, 50], "yaw": 10}),
        );
        assert_eq!(out["refused"], true, "{out}");
        assert!(
            out["reason"].as_str().unwrap().contains("no navmesh"),
            "{out}"
        );
        for args in [
            json!({"token": "tok", "pos": [1]}),
            json!({"token": "tok", "pos": [1, 2], "face": [1, 2], "yaw": 3}),
            json!({"token": "tok", "pos": [1, 2], "radius": 0}),
        ] {
            assert_eq!(
                run(&mut session, "game_place", args.clone())["refused"],
                true,
                "{args}"
            );
        }
        let sent = verbs(server.join().unwrap());
        let ground = Verb::SceneGround {
            xyz: vec![10.0, 20.0, 29.0],
            range: 5.0,
            radius: 0.5,
            search: 0.0,
        };
        assert_eq!(sent[1], ground);
        let first = Verb::Teleport {
            pos: [10.0, 20.0, 30.25],
            rot: [0.0; 3],
        };
        assert_eq!(sent[2], first);
        let safe = Verb::Teleport {
            pos: [11.0, 20.0, 30.5],
            rot: [0.0; 3],
        };
        assert_eq!(sent[6], safe);
        // Each teleport journaled and tainting, as game_teleport's; the refused place sent none.
        let patches = session.live.as_ref().unwrap().patches.borrow();
        assert_eq!(patches.len(), 2);
        let teleport =
            |p: &Value| p["taints"] == true && p["what"].as_str().unwrap().contains("teleport");
        assert!(patches.iter().all(|(_, p)| teleport(p)));
    }

    #[test]
    fn exec_sends_the_typed_verb_and_never_a_line() {
        let fx = served("scene-exec");
        let server = fake_game(
            &token(&fx),
            2,
            Box::new(|r| match &r.verb {
                Verb::Exec { function, .. } if function == "nope" => {
                    let data = json!({"line": "nope()", "log": ""});
                    vec![reply(r.id, false, "the game called nothing", Some(data))]
                }
                _ => {
                    let data = json!({"line": "x", "log": "[NpcAvoid] preset 2"});
                    vec![reply(r.id, true, "called", Some(data))]
                }
            }),
        );
        let mut session = fx.session();
        let out = run(
            &mut session,
            "game_exec",
            json!({"token": "tok", "line": "npcavoidpreset( 2 )"}),
        );
        assert_eq!(out["ok"], true, "{out}");
        assert_eq!(out["line"], "npcavoidpreset(2)");
        assert_eq!(out["log"], "[NpcAvoid] preset 2");
        let out = run(
            &mut session,
            "game_exec",
            json!({"token": "tok", "line": "nope"}),
        );
        assert_eq!(out["refused"], true, "{out}");
        assert_eq!(out["line"], "nope()");
        assert_eq!(out["game_text"], "the game called nothing");
        // Refused, nothing sent: a line exec can't read, no gameplay or photo mode in quen's state.
        for line in ["two words", "f(\"a)", "say(\u{e9})"] {
            let out = run(
                &mut session,
                "game_exec",
                json!({"token": "tok", "line": line}),
            );
            assert_eq!(out["refused"], true, "{line}: {out}");
        }
        flags(&fx, 0);
        let out = run(
            &mut session,
            "game_exec",
            json!({"token": "tok", "line": "npcavoid()"}),
        );
        assert!(
            out["reason"]
                .as_str()
                .unwrap()
                .contains("not gameplay or photo mode"),
            "{out}"
        );
        let preset = Verb::Exec {
            function: "npcavoidpreset".into(),
            args: vec!["2".into()],
        };
        let nope = Verb::Exec {
            function: "nope".into(),
            args: Vec::new(),
        };
        assert_eq!(verbs(server.join().unwrap()), [preset, nope]);
        // While the user has the controls none of these run; the reads do.
        for tool in ["game_exec", "game_frame", "game_place"] {
            assert!(!crate::hands::user_may(tool, &json!({})), "{tool}");
        }
        for tool in ["world_frame", "world_ground", "world_ray", "world_nav"] {
            assert!(crate::hands::user_may(tool, &json!({})), "{tool}");
        }
    }

    /// measure_ab's console action with `exec` (quen-w3 0.1.13) goes as the exec verb, and its note says so; only a
    /// pinned package with the verb gets it.
    #[test]
    fn a_console_action_goes_as_exec_when_the_package_has_it() {
        let fx = served("scene-exec-action");
        let answer = |r: &Request| vec![reply(r.id, true, "called", Some(json!({"log": ""})))];
        let server = fake_game(&token(&fx), 1, Box::new(answer));
        let session = fx.session();
        let cx = ctx(&session, &json!({"token": "tok"}), &[]).unwrap();
        let mut call = session.call(crate::mcp::Conn::nobody(), Value::Null, None);
        let action = crate::game::Action::Console {
            line: "settime(1,22)".into(),
            expect: None,
            exec: true,
        };
        assert_eq!(action.to_string(), "exec settime(1, 22)");
        assert!(action.run(&cx, &mut call).is_ok());
        let settime = Verb::Exec {
            function: "settime".into(),
            args: vec!["1".into(), "22".into()],
        };
        assert_eq!(verbs(server.join().unwrap()), [settime]);
        assert!(doctor::has_exec(Some("quen-w3 0.1.13")) && doctor::has_exec(Some("test")));
        assert!(!doctor::has_exec(Some("quen-w3 0.1.12")) && !doctor::has_exec(None));
    }
}
