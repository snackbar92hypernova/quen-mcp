//! The scene tools (quen-w3 0.1.13; the runner design's phase 4): `world_frame`, `world_ground`, `world_ray` and
//! `world_nav` read the scene through the game's scripts (the channel's scene.frame, scene.ground, scene.ray and nav.line
//! verbs) and change nothing; `world_map` reads the space around a point into a top-down map with them (a grid, a
//! street's width profile, or walkers' look-aheads), its files and captures rows quen's own; `game_frame` sets the
//! photo-mode camera to frame a subject from its sides, each pose checked first with a sphere sweep and then set with
//! no-clip; `game_place` puts the player on the navmesh. The placement maths is quen-core's `scene`, the map's `map`.

use std::fs;
use std::path::Path;
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime};

use quen_core::bridge_log::{VALID, in_photo};
use quen_core::journal::sha256_hex;
use quen_core::ledger::{Capture, Ledger};
use quen_core::map::{self, Frame, Grid, Reading, Rib};
use quen_core::protocol::{
    MAX_ACTORS, MAX_ACTORS_RADIUS, MAX_FRAME, MAX_GROUND_RADIUS, MAX_GROUND_RANGE,
    MAX_GROUND_SEARCH, MAX_NAV_WIDTH, MAX_QUERIES, MAX_SWEEP_RADIUS, Reply, Verb,
};
use quen_core::scene::{self, Framing};
use quen_core::sweep::{FOV, Pose};
use quen_core::trace::{self, Body};
use serde_json::{Map, Value, json};

use crate::camera;
use crate::channel::REPLY_MAX;
use crate::doctor;
use crate::game::{Ctx, Fields, ctx, schema, shown};
use crate::ledger::INLINE_BYTES;
use crate::measure;
use crate::session::{self, Call, Done, Session};

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
/// world_map: a cell's side (metres), the most profile stations, the most trace captures a side, the walkers' lane
/// radius and their look-ahead ray's height above their position (metres).
const MAP_CELL: std::ops::RangeInclusive<f64> = 0.25..=2.0;
const MAP_STATIONS: usize = 200;
const MAP_TRACES: usize = 8;
const WALKER_LANE_M: f32 = 0.4;
const WALKER_RAY_UP_M: f64 = 0.5;
/// world_map's arguments only one mode takes; the others take at, cell, z_range, radius, occupants, class, tag, png.
const MAP_ONLY: [(&str, &str); 8] = [
    ("size", "grid"),
    ("traces", "grid"),
    ("traces_b", "grid"),
    ("base", "grid"),
    ("axis", "profile"),
    ("step", "profile"),
    ("max_m", "profile"),
    ("ahead_m", "blockers"),
];
/// The inline result's lists, halved (the longest first) while it is over 16 KB; the file holds them whole.
const MAP_LISTS: [&str; 6] = [
    "/profile/steps",
    "/blockers",
    "/occupants/list",
    "/components",
    "/chokepoints",
    "/layers",
];

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
            "name": "world_map",
            "description": "Read the space around a point into a top-down map (quen_guide: world_map). grid: a \
                scene.ground point per cell, 16 a call: class (walkable, tight, covered, water, obstacle, tall, \
                ledge), clearance_m, chokepoints, components, edges; traces: heat layers from measure_trace CSVs. \
                profile: a street's width along axis, two nav.line reaches a step, 8 steps a call. blockers: per \
                walker near at, a nav.line and a scene.ray along its velocity and the bodies near that line: clear, \
                navmesh_edge, static or actor. occupants: one world.actors read, off_level outside at.z ± z_range. \
                Files <tag>-map.json (the cells) and <tag>-map.png, each with a captures row; inline a summary under \
                16 KB. budget, limits. Changes nothing; runs while the user has the controls. quen-w3 0.1.13 or later. \
                Refused, nothing sent: wrong token, an older package, no loaded game, a bad value, a tag taken, a \
                trace or base not found or changed. Failed: no reply within 5 s, or a cancel: the cells read so far.",
            "inputSchema": schema(json!({
                "mode": {"type": "string", "enum": ["grid", "profile", "blockers"], "description": "default grid"},
                "at": vec3("[x, y, z]: the centre and level; default the player's position"),
                "size": {"type": "array", "items": {"type": "number"}, "minItems": 2, "maxItems": 2, "description": "grid: [w, h] metres, each 1 to 200, at most 10,000 cells; default [40, 40]"},
                "cell": {"type": "number", "minimum": 0.25, "maximum": 2, "description": "metres; default 1 (profile, blockers: the picture's)"},
                "z_range": {"type": "number", "minimum": 0.1, "maximum": MAX_GROUND_RANGE, "description": "metres above and below at.z; default 2"},
                "radius": {"type": "number", "minimum": 0.1, "maximum": MAX_ACTORS_RADIUS, "description": "grid, profile: the body's radius, 0.1 to 2, default 0.4; blockers: walkers within it of at, from 1, default 15"},
                "axis": {"type": "array", "minItems": 2, "maxItems": MAX_QUERIES, "items": {"type": "array", "items": {"type": "number"}, "minItems": 2, "maxItems": 3}, "description": "profile: 2 to 16 points [x, y] or [x, y, z] (z default at.z)"},
                "step": {"type": "number", "minimum": 0.25, "maximum": 10, "description": "profile: metres between steps, at most 200 steps; default 1"},
                "max_m": {"type": "number", "minimum": 1, "maximum": 50, "description": "profile: each side's reach tested; default 20"},
                "ahead_m": {"type": "number", "minimum": 0.5, "maximum": 10, "description": "blockers: the look-ahead; default 3"},
                "occupants": {"type": "boolean", "description": "read world.actors onto the map; default true (blockers always reads them)"},
                "class": {"type": "string", "description": "the occupants' world.actors class"},
                "tag": {"type": "string", "pattern": "^[A-Za-z0-9_-]{1,58}$", "description": "the files' <tag>, new in the session; default map-<UTC time>"},
                "traces": {"type": "array", "minItems": 1, "maxItems": MAP_TRACES, "items": {"type": "string"}, "description": "grid: trace CSVs, each a captures id or a measure_trace tag of this session"},
                "traces_b": {"type": "array", "minItems": 1, "maxItems": MAP_TRACES, "items": {"type": "string"}, "description": "grid: as traces; adds occupancy_diff, B less A"},
                "png": {"type": "boolean", "description": "write the picture; default true"},
                "base": {"type": "integer", "description": "grid: a world_map JSON capture of this boot: its cells again, occupants and traces read fresh"},
            }), &[]),
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
        "world_map" => world_map(session, call, args),
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
    let tag = args.get("tag").map(tag).transpose()?;
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
    // Its captures: quen's status line down once for all the sides (status.rs).
    if tag.is_some() {
        crate::status::hide(&cx, call, false).map_err(Done::Refused)?;
    }
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

/// A capture's tag: 1 to 58 letters, digits, _ or -.
fn tag(v: &Value) -> Result<String, String> {
    let t = v.as_str().filter(|t| {
        (1..=58).contains(&t.len())
            && t.chars()
                .all(|c| c.is_ascii_alphanumeric() || "_-".contains(c))
    });
    Ok(t.ok_or("tag: 1-58 letters, digits, _ or -".to_string())?
        .to_string())
}

/// What world_map's reads cast: the verbs, their points or segments, the rays among them, and when the first went.
struct Reads<'a, 's> {
    cx: &'a Ctx<'s>,
    calls: usize,
    queries: usize,
    rays: usize,
    start: Instant,
}

impl Reads<'_, '_> {
    fn send(&mut self, verb: Verb, fields: &Fields) -> Result<Reply, Done> {
        let (queries, rays) = match &verb {
            Verb::SceneGround { xyz, .. } => (xyz.len() / 3, xyz.len() / 3),
            Verb::SceneRay { segments, .. } => (segments.len() / 6, segments.len() / 6),
            Verb::NavLine { segments, .. } => (segments.len() / 6, 0),
            _ => (0, 0),
        };
        let reply = send(self.cx, verb, fields)?;
        self.calls += 1;
        self.queries += queries;
        self.rays += rays;
        Ok(reply)
    }

    /// Each verb one game tick, its frame-time cost unmeasured; never inside a measure window.
    fn budget(&self) -> Value {
        json!({"calls": self.calls, "ticks": self.calls, "queries": self.queries, "rays": self.rays,
               "span_s": (self.start.elapsed().as_secs_f64() * 100.0).round() / 100.0,
               "cost_ms_per_frame": "unmeasured", "in_window": false})
    }
}

/// The occupants: one world.actors read within `reach` of the player (3-D, through walls; the protocol's cap), each on
/// `frame` and its level or not; `found` and when it came.
fn occupants(
    reads: &mut Reads,
    frame: &Frame,
    z_range: f64,
    reach: f64,
    class: &Option<String>,
    fields: &Fields,
) -> Result<(Vec<map::Occupant>, Value), Done> {
    let radius = reach.min(f64::from(MAX_ACTORS_RADIUS));
    let verb = Verb::WorldActors {
        radius: radius as f32,
        max: MAX_ACTORS,
        tag: None,
        class: class.clone(),
    };
    let data = reads.send(verb, fields)?.data.unwrap_or_default();
    let list = map::occupants(&data, frame, z_range)
        .map_err(|why| Done::Failed(format!("world.actors: {why}"), fields.clone()))?;
    let about = json!({"found": data["found"], "at_t": session::utc_ms(SystemTime::now()),
                       "radius_m": (radius * 100.0).round() / 100.0,
                       "list": list.iter().map(map::Occupant::json).collect::<Vec<_>>()});
    Ok((list, about))
}

/// world_map's `key` (traces, traces_b): each a captures id or a measure_trace tag of this session (its
/// `<tag>-trace.csv`, or a measure_ab window's `<tag>_<n><label>`), the file checked against its captures row, the
/// sampler's radius and contact from its measurement rows' note; with when each CSV was written (seconds since 1970).
fn traces(cx: &Ctx, args: &Value, key: &str) -> Result<(Vec<map::Trace>, Vec<i64>), String> {
    let Some(v) = args.get(key) else {
        return Ok((Vec::new(), Vec::new()));
    };
    let bad = || format!("{key} must be 1 to {MAP_TRACES} captures ids or measure_trace tags");
    let items = v
        .as_array()
        .filter(|a| (1..=MAP_TRACES).contains(&a.len()))
        .ok_or_else(bad)?;
    let db = Ledger::open_readonly(cx.config.state_dir().join("quen.db"))
        .map_err(|e| format!("the ledger: {e}"))?;
    let query = |sql: &str, p: Value| {
        db.query(sql, &[p], Duration::from_secs(5))
            .map(|r| r.rows)
            .map_err(|e| format!("the ledger: {e}"))
    };
    let (mut out, mut written) = (Vec::new(), Vec::new());
    for item in items {
        let row =
            "SELECT id, path, sha256, method, CAST(strftime('%s', t) AS INTEGER) FROM captures";
        let id = item
            .as_i64()
            .or_else(|| item.as_str().and_then(|s| s.parse().ok()));
        let (rows, what) = match id {
            Some(id) => (
                query(&format!("{row} WHERE id = ?1"), json!(id))?,
                format!("capture {id}"),
            ),
            None => {
                let t = tag(item).map_err(|_| bad())?;
                let path = format!("{}/{t}-trace.csv", cx.live.id);
                (
                    query(&format!("{row} WHERE path = ?1"), json!(path))?,
                    format!("tag {t}"),
                )
            }
        };
        let Some([id, Value::String(path), Value::String(sha), method, t]) =
            rows.first().map(Vec::as_slice)
        else {
            return Err(format!("{key}: no capture for {what}"));
        };
        if *method != "actor_trace" {
            return Err(format!("{key}: {what} is no actor trace (method {method})"));
        }
        let file = cx.config.capture_root().join(path);
        let at = |e: &dyn std::fmt::Display| format!("{key}: {}: {e}", file.display());
        let bytes = fs::read(&file).map_err(|e| at(&e))?;
        if sha256_hex(&bytes) != *sha {
            return Err(at(&"changed since quen wrote it"));
        }
        let samples = trace::parse_csv(&String::from_utf8_lossy(&bytes)).map_err(|e| at(&e))?;
        let id = id.as_i64().unwrap_or_default();
        let note =
            "SELECT note FROM measurements WHERE capture_id = ?1 AND note IS NOT NULL LIMIT 1";
        let notes = query(note, json!(id))?;
        let note = notes
            .first()
            .and_then(|r| r[0].as_str())
            .unwrap_or_default();
        let (radius, contact) = map::sampler_of(note);
        out.push(map::Trace {
            id,
            samples,
            contact,
            radius,
        });
        written.push(t.as_i64().unwrap_or_default());
    }
    Ok((out, written))
}

/// world_map's `base`: an earlier grid of this boot from its JSON capture, checked against its row, and how old.
fn base(cx: &Ctx, id: i64) -> Result<(Grid, Value), String> {
    let db = Ledger::open_readonly(cx.config.state_dir().join("quen.db"))
        .map_err(|e| format!("the ledger: {e}"))?;
    let query = |sql: &str, p: Value| {
        db.query(sql, &[p], Duration::from_secs(5))
            .map(|r| r.rows)
            .map_err(|e| format!("the ledger: {e}"))
    };
    let sql = "SELECT path, sha256, method, kind, boot_id FROM captures WHERE id = ?1";
    let row = query(sql, json!(id))?.pop();
    let Some([Value::String(path), Value::String(sha), method, kind, boot]) = row.as_deref() else {
        return Err(format!("base: no capture {id}"));
    };
    if *method != "world_map" || *kind != "json" {
        return Err(format!(
            "base: capture {id} is no world_map JSON (method {method}, kind {kind})"
        ));
    }
    if boot.as_i64() != Some(cx.live.boot) {
        return Err(format!(
            "base: capture {id} is of boot {boot}, not this one ({}): read the grid again",
            cx.live.boot
        ));
    }
    let file = cx.config.capture_root().join(path);
    let at = |e: &dyn std::fmt::Display| format!("base: {}: {e}", file.display());
    let bytes = fs::read(&file).map_err(|e| at(&e))?;
    if sha256_hex(&bytes) != *sha {
        return Err(at(&"changed since quen wrote it"));
    }
    let data: Value = serde_json::from_slice(&bytes).map_err(|e| at(&e))?;
    if data["mode"] != "grid" {
        return Err(format!(
            "base: capture {id} is a {} map, not a grid",
            data["mode"]
        ));
    }
    let grid = Grid::from_file(&data).map_err(|e| at(&e))?;
    // Its cells' age: from their own read_at, which a map made from a base carries on.
    let read = query(
        "SELECT CAST(strftime('%s', ?1) AS INTEGER)",
        data["read_at"].clone(),
    )?;
    let read = read.first().and_then(|r| r[0].as_i64());
    let age = read.map(|t| session::now() as i64 - t);
    let about = json!({"capture_id": id, "read_at": data["read_at"], "age_s": age});
    Ok((grid, about))
}

/// The grid's cells, 16 points a scene.ground call. A cancel, or a call that failed, stops it (`stopped` says which);
/// the cells read so far are kept.
fn ground_pages(
    reads: &mut Reads,
    call: &Call,
    frame: &Frame,
    z_range: f64,
    radius: f64,
    fields: &Fields,
    stopped: &mut Option<String>,
) -> Vec<Option<Reading>> {
    let points = frame.points();
    let pages = points.len().div_ceil(MAX_QUERIES);
    let mut cells = vec![None; points.len()];
    for (k, page) in points.chunks(MAX_QUERIES).enumerate() {
        if call.stopped() {
            let cause = call.stop_cause();
            *stopped = Some(format!(
                "cancelled after {k} of {pages} scene.ground calls ({cause})"
            ));
            break;
        }
        let verb = Verb::SceneGround {
            xyz: flat(page.iter().flatten().copied()),
            range: z_range as f32,
            radius: radius as f32,
            search: 0.0,
        };
        match reads
            .send(verb, fields)
            .and_then(|r| items(&r, page.len(), fields))
        {
            Ok(read) => {
                for (j, v) in read.iter().enumerate() {
                    cells[k * MAX_QUERIES + j] = Some(Reading::from_ground(v));
                }
            }
            Err(e) => {
                *stopped = Some(format!(
                    "scene.ground call {} of {pages}: {}",
                    k + 1,
                    why(e)
                ));
                break;
            }
        }
    }
    cells
}

/// A profile's ribs: per station a nav.line to its left and one to its right, `max_m` long, 8 stations a call; a
/// cancel or a failed call stops it, as [`ground_pages`].
fn profile_pages(
    reads: &mut Reads,
    call: &Call,
    stations: &[map::Station],
    max_m: f64,
    radius: f64,
    fields: &Fields,
    stopped: &mut Option<String>,
) -> Vec<Rib> {
    let per = MAX_QUERIES / 2;
    let pages = stations.len().div_ceil(per);
    let side = |s: &map::Station, sign: f64| {
        [
            s.at[0] + sign * s.left[0] * max_m,
            s.at[1] + sign * s.left[1] * max_m,
            s.at[2],
        ]
    };
    let mut ribs = Vec::new();
    for (k, page) in stations.chunks(per).enumerate() {
        if call.stopped() {
            let cause = call.stop_cause();
            *stopped = Some(format!(
                "cancelled after {k} of {pages} nav.line calls ({cause})"
            ));
            break;
        }
        let segments = flat(page.iter().flat_map(|s| {
            s.at.into_iter()
                .chain(side(s, 1.0))
                .chain(s.at)
                .chain(side(s, -1.0))
        }));
        let verb = Verb::NavLine {
            segments,
            width: radius as f32,
        };
        match reads
            .send(verb, fields)
            .and_then(|r| items(&r, 2 * page.len(), fields))
        {
            Ok(read) => {
                for (s, pair) in page.iter().zip(read.chunks(2)) {
                    let (left_m, left_open) = map::reach(s.at, &pair[0], max_m);
                    let (right_m, right_open) = map::reach(s.at, &pair[1], max_m);
                    ribs.push(Rib {
                        at: s.at,
                        left_m,
                        right_m,
                        left_open,
                        right_open,
                    });
                }
            }
            Err(e) => {
                *stopped = Some(format!("nav.line call {} of {pages}: {}", k + 1, why(e)));
                break;
            }
        }
    }
    ribs
}

/// The walkers' verdicts: per 16 walkers (a body and the end of its look-ahead) a nav.line along the look-ahead and a
/// scene.ray 0.5 m above it, and quen-side the nearest of `others` in its way; a cancel or a failed call stops it, as
/// [`ground_pages`].
fn blocker_pages(
    reads: &mut Reads,
    call: &Call,
    walkers: &[(&Body, [f64; 3])],
    others: &[(Value, [f64; 3])],
    fields: &Fields,
    stopped: &mut Option<String>,
) -> Vec<Value> {
    let pages = walkers.len().div_ceil(MAX_QUERIES);
    let up = |p: [f64; 3]| [p[0], p[1], p[2] + WALKER_RAY_UP_M];
    let mut out = Vec::new();
    for (k, page) in walkers.chunks(MAX_QUERIES).enumerate() {
        if call.stopped() {
            let cause = call.stop_cause();
            *stopped = Some(format!(
                "cancelled after {k} of {pages} walker pages ({cause})"
            ));
            break;
        }
        let nav = Verb::NavLine {
            segments: flat(page.iter().flat_map(|(b, to)| b.pos.into_iter().chain(*to))),
            width: WALKER_LANE_M,
        };
        let ray = Verb::SceneRay {
            segments: flat(
                page.iter()
                    .flat_map(|(b, to)| up(b.pos).into_iter().chain(up(*to))),
            ),
            radius: 0.0,
        };
        let read = reads
            .send(nav, fields)
            .and_then(|r| items(&r, page.len(), fields))
            .and_then(|navs| {
                let rays = reads
                    .send(ray, fields)
                    .and_then(|r| items(&r, page.len(), fields))?;
                Ok((navs, rays))
            });
        match read {
            Ok((navs, rays)) => {
                for (((b, to), nav), ray) in page.iter().zip(&navs).zip(&rays) {
                    let by = map::in_the_way(b.pos, *to, others);
                    out.push(map::verdict(b, *to, nav, ray, by));
                }
            }
            Err(e) => {
                *stopped = Some(format!("walker page {} of {pages}: {}", k + 1, why(e)));
                break;
            }
        }
    }
    out
}

/// Writes a world_map file with its captures row (method world_map, `frame_state`; the PNG gets no encoding: it is a
/// drawing, not a capture of the game's pixels).
fn keep(
    cx: &Ctx,
    call: &mut Call,
    path: &Path,
    bytes: &[u8],
    kind: &str,
    frame_state: &Fields,
) -> Result<(), String> {
    fs::create_dir_all(measure::captures(cx))
        .and_then(|()| fs::write(path, bytes))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    call.artifact(path);
    let row = Capture {
        boot_id: Some(cx.live.boot),
        path: measure::relative(cx, path),
        sha256: sha256_hex(bytes),
        bytes: bytes.len() as i64,
        kind: kind.into(),
        method: Some("world_map".into()),
        frame_state: Some(frame_state.clone()),
        t: session::utc(SystemTime::now()),
        ..Capture::default()
    };
    let id = session::open_ledger(cx.config)
        .and_then(|mut l| l.add_capture(&row).map_err(session::ledger_error))
        .map_err(|why| format!("{}: written, but no captures row: {why}", path.display()))?;
    call.row("captures", id);
    Ok(())
}

/// What each mode's reading can't tell: fixed lines, and when the characters were read.
fn limits(mode: &str, z_range: f64, actors_at: Option<&Value>, traces: bool) -> Vec<String> {
    let mut out: Vec<String> = match mode {
        "grid" => vec![
            format!("one level per cell within ±{z_range} m of at.z: map another level with another at.z"),
            "obstacle kinds unknown: heights only, over ground_ref (the walkable cells' median ground hit; z_ref, \
             their median navmesh z, ran 0.4-1.3 m above the ground in boot 190)"
                .into(),
            "tall: no navmesh and no ray hit within the band: inside geometry, or a drop below it".into(),
            "clearance and chokepoints from cell centres: widths within one cell; what lies past the map's edge \
             is unseen"
                .into(),
        ],
        "profile" => vec![
            "unverified: clear_in_direction: a side that stops short reaches NavigationClearLineInDirection's \
             point, its meaning not live-confirmed"
                .into(),
            "a side is open when NavigationLineTest finds its whole max_m clear; width_m is the two reaches' sum"
                .into(),
            "the navmesh only: characters and props off it aren't seen".into(),
        ],
        _ => vec![
            "walkers: GetVelocity() above 0.3 m/s; a body blocks within 0.6 m of the look-ahead line".into(),
            "static: a scene.ray 0.5 m above the walker's position (no characters, no water)".into(),
            "unverified: clear_in_direction: navmesh_edge's distance is NavigationClearLineInDirection's point"
                .into(),
        ],
    };
    out.push(match actors_at {
        Some(t) => format!(
            "characters from world.actors at {t}, not from rays: positions only; it reads by 3-D distance from \
             the player, through walls, the 50 nearest the player"
        ),
        None => "no characters read (occupants false, or the reading stopped first)".into(),
    });
    if traces {
        out.push(
            "trace layers only where a trace's radius around its reference reached (coverage); a layer's total \
             is its trace metric while every subject stayed in the map and on its level"
                .into(),
        );
    }
    out
}

/// world_map's verbs, for run_plan's estimate: world.get, the mode's pages (blockers: as for 50 walkers) and
/// world.actors; with base, the two reads alone.
pub(crate) fn map_calls(args: &Value) -> usize {
    if args.get("base").is_some() {
        return 2;
    }
    let pages = match args["mode"].as_str() {
        Some("profile") => {
            let axis: Vec<[f64; 2]> = args["axis"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|p| Some([p[0].as_f64()?, p[1].as_f64()?]))
                .collect();
            let length: f64 = axis
                .windows(2)
                .map(|w| (w[1][0] - w[0][0]).hypot(w[1][1] - w[0][1]))
                .sum();
            let step = args["step"].as_f64().unwrap_or(1.0);
            let stations = (length / step).floor() as usize + 1;
            stations.min(MAP_STATIONS).div_ceil(MAX_QUERIES / 2)
        }
        Some("blockers") => 2 * (MAX_ACTORS as usize).div_ceil(MAX_QUERIES),
        _ => {
            let size = numbers::<2>(&args["size"]).unwrap_or([40.0, 40.0]);
            let cell = args["cell"].as_f64().unwrap_or(1.0);
            let cells = Frame::new([0.0; 3], size, cell).map_or(0, |f| f.cells());
            cells.div_ceil(MAX_QUERIES)
        }
    };
    pages + 2
}

fn world_map(session: &Session, call: &mut Call, args: &Value) -> Result<Fields, Done> {
    let known = [
        "mode",
        "at",
        "size",
        "cell",
        "z_range",
        "radius",
        "axis",
        "step",
        "max_m",
        "ahead_m",
        "occupants",
        "class",
        "tag",
        "traces",
        "traces_b",
        "png",
        "base",
    ];
    let cx = ctx(session, args, &known)?;
    let mode = match args.get("mode").map(Value::as_str) {
        None => "grid",
        Some(Some(m @ ("grid" | "profile" | "blockers"))) => m,
        Some(_) => return Err("mode is grid, profile or blockers".to_string().into()),
    };
    if let Some((key, of)) = MAP_ONLY
        .iter()
        .find(|(k, of)| *of != mode && args.get(*k).is_some())
    {
        return Err(format!("{key} is mode {of}'s, not {mode}'s").into());
    }
    let base_id = match args.get("base") {
        None => None,
        Some(v) => Some(v.as_i64().ok_or("base must be a captures id".to_string())?),
    };
    let theirs = ["at", "size", "cell", "z_range", "radius"];
    if base_id.is_some()
        && let Some(k) = theirs.iter().find(|k| args.get(**k).is_some())
    {
        return Err(format!("{k} is the base's: give base or {k}, not both").into());
    }
    if args.get("traces_b").is_some() && args.get("traces").is_none() {
        return Err("traces_b needs traces: occupancy_diff is B less A"
            .to_string()
            .into());
    }
    let coordinate = |x: &f64| x.abs() < quen_core::sweep::MAX_COORD;
    let at = match args.get("at") {
        None => None,
        Some(v) => Some(
            point(v)
                .filter(|p| p.iter().all(coordinate))
                .ok_or("at must be [x, y, z], each below 100000 in size".to_string())?,
        ),
    };
    let cell = num(args, "cell", 1.0, MAP_CELL)?;
    let z_range = num(args, "z_range", 2.0, 0.1..=f64::from(MAX_GROUND_RANGE))?;
    let radius = match mode {
        "blockers" => num(args, "radius", 15.0, 1.0..=f64::from(MAX_ACTORS_RADIUS))?,
        _ => num(args, "radius", 0.4, 0.1..=2.0)?,
    };
    let size = match args.get("size") {
        None => [40.0, 40.0],
        Some(v) => numbers::<2>(v)
            .filter(|s| s.iter().all(|x| (1.0..=map::MAX_SIDE_M).contains(x)))
            .ok_or(format!(
                "size must be [w, h], each 1 to {} m",
                map::MAX_SIDE_M
            ))?,
    };
    if mode == "grid" {
        Frame::new([0.0; 3], size, cell)?;
    }
    let on = |key: &str| match args.get(key) {
        None => Ok(true),
        Some(v) => v.as_bool().ok_or(format!("{key} must be true or false")),
    };
    let (show, png) = (on("occupants")?, on("png")?);
    let class = match args.get("class") {
        None => None,
        Some(v) => Some(
            v.as_str()
                .filter(|c| trace::plain_name(c))
                .ok_or("class: 1 to 64 letters, digits and _".to_string())?
                .to_string(),
        ),
    };
    let tag = match args.get("tag") {
        None => format!(
            "map-{}",
            session::utc(SystemTime::now()).replace(['-', ':'], "")
        ),
        Some(v) => tag(v)?,
    };
    let axis: Vec<Vec<f64>> = match args.get("axis") {
        None if mode == "profile" => {
            return Err("profile needs axis: 2 to 16 points".to_string().into());
        }
        None => Vec::new(),
        Some(v) => v
            .as_array()
            .filter(|a| (2..=MAX_QUERIES).contains(&a.len()))
            .and_then(|a| {
                a.iter()
                    .map(|p| {
                        let n: Vec<f64> = p
                            .as_array()?
                            .iter()
                            .map(Value::as_f64)
                            .collect::<Option<_>>()?;
                        ((2..=3).contains(&n.len()) && n.iter().all(coordinate)).then_some(n)
                    })
                    .collect::<Option<Vec<_>>>()
            })
            .ok_or("axis must be 2 to 16 points of [x, y] or [x, y, z]".to_string())?,
    };
    let axis_at = |z: f64| -> Vec<[f64; 3]> {
        axis.iter()
            .map(|p| [p[0], p[1], p.get(2).copied().unwrap_or(z)])
            .collect()
    };
    let step = num(args, "step", 1.0, 0.25..=10.0)?;
    let max_m = num(args, "max_m", 20.0, 1.0..=50.0)?;
    let ahead_m = num(args, "ahead_m", 3.0, 0.5..=10.0)?;
    if mode == "profile" {
        map::stations(&axis_at(0.0), step, MAP_STATIONS)?;
    }
    ready(&cx, "world_map", false)?;
    let dir = measure::captures(&cx);
    let files = [
        dir.join(format!("{tag}-map.json")),
        dir.join(format!("{tag}-map.png")),
    ];
    if let Some(f) = files.iter().find(|f| f.exists()) {
        return Err(format!("{} exists: pick a tag new in this session", f.display()).into());
    }
    let (traces_a, written_a) = traces(&cx, args, "traces")?;
    let (traces_b, written_b) = traces(&cx, args, "traces_b")?;
    let base = base_id.map(|id| base(&cx, id)).transpose()?;
    // A base's grid keeps its own band and body.
    let (z_range, radius) = base
        .as_ref()
        .map_or((z_range, radius), |(g, _)| (g.z_range, g.radius));
    call.lap("check");

    let fields = Map::new();
    let mut reads = Reads {
        cx: &cx,
        calls: 0,
        queries: 0,
        rays: 0,
        start: Instant::now(),
    };
    let world = reads.send(Verb::WorldGet {}, &fields)?;
    let player = Body::player(world.data.as_ref().unwrap_or(&Value::Null))
        .map_err(|why| Done::Failed(format!("world.get: {why}"), fields.clone()))?;
    let at = base
        .as_ref()
        .map(|(g, _)| g.frame.at)
        .or(at)
        .unwrap_or(player.pos);
    let (read_at, read_from) = (session::utc_ms(SystemTime::now()), Instant::now());
    let flat_m = |p: [f64; 3]| (p[0] - at[0]).hypot(p[1] - at[1]);
    let reach = |p: [f64; 3]| flat_m(p).hypot(p[2] - at[2]);
    let mut stopped: Option<String> = None;
    let mut out = Map::new();
    let (mut grid, mut lines, mut people) = (None, Vec::new(), None);
    let frame = match mode {
        "grid" => {
            let g = match base {
                Some((g, about)) => {
                    out.insert("base".into(), about);
                    g
                }
                None => {
                    let frame = Frame::new(at, size, cell)?;
                    let cells = ground_pages(
                        &mut reads,
                        call,
                        &frame,
                        z_range,
                        radius,
                        &fields,
                        &mut stopped,
                    );
                    Grid::new(frame, z_range, radius, cells)
                }
            };
            out.extend(g.summary());
            let frame = g.frame;
            grid = Some(g);
            frame
        }
        "profile" => {
            let stations = map::stations(&axis_at(at[2]), step, MAP_STATIONS)?;
            let ribs = profile_pages(
                &mut reads,
                call,
                &stations,
                max_m,
                radius,
                &fields,
                &mut stopped,
            );
            for (s, b) in stations.iter().zip(&ribs) {
                let (left, p) = (s.left, [b.at[0], b.at[1]]);
                let colour = |open: bool| if open { map::OPEN } else { map::SHORT };
                let to = |m: f64| [p[0] + left[0] * m, p[1] + left[1] * m];
                lines.push((p, to(b.left_m), colour(b.left_open)));
                lines.push((p, to(-b.right_m), colour(b.right_open)));
            }
            out.insert("profile".into(), map::profile(&ribs));
            out.insert("unverified".into(), json!(["clear_in_direction"]));
            let ends: Vec<[f64; 2]> = stations
                .iter()
                .flat_map(|s| {
                    [-1.0, 1.0].map(|k| {
                        [
                            s.at[0] + k * s.left[0] * max_m,
                            s.at[1] + k * s.left[1] * max_m,
                        ]
                    })
                })
                .collect();
            Frame::around(&ends, at[2], cell)
        }
        _ => {
            let corners = [
                [at[0] - radius, at[1] - radius],
                [at[0] + radius, at[1] + radius],
            ];
            let frame = Frame::around(&corners, at[2], cell);
            let (list, about) = occupants(
                &mut reads,
                &frame,
                z_range,
                reach(player.pos) + radius,
                &class,
                &fields,
            )?;
            let walkers: Vec<(&Body, [f64; 3])> = list
                .iter()
                .filter(|o| !o.off_level && flat_m(o.body.pos) <= radius)
                .filter_map(|o| map::ahead(&o.body, ahead_m).map(|to| (&o.body, to)))
                .collect();
            let others: Vec<(Value, [f64; 3])> = list
                .iter()
                .filter(|o| !o.off_level)
                .map(|o| (json!(o.body.id), o.body.pos))
                .chain([(json!("player"), player.pos)])
                .collect();
            let verdicts =
                blocker_pages(&mut reads, call, &walkers, &others, &fields, &mut stopped);
            for ((b, to), v) in walkers.iter().zip(&verdicts) {
                let colour = if v["verdict"] == "clear" {
                    map::CLEAR
                } else {
                    map::STOPPED
                };
                lines.push(([b.pos[0], b.pos[1]], [to[0], to[1]], colour));
            }
            out.insert("walkers".into(), json!(walkers.len()));
            out.insert("blockers".into(), json!(verdicts));
            out.insert("unverified".into(), json!(["clear_in_direction"]));
            people = Some((list, about));
            frame
        }
    };
    if people.is_none() && show && stopped.is_none() {
        let b = frame.bbox();
        let half = ((b[2] - b[0]).hypot(b[3] - b[1]) / 2.0).hypot(z_range);
        match occupants(
            &mut reads,
            &frame,
            z_range,
            reach(player.pos) + half,
            &class,
            &fields,
        ) {
            Ok(read) => people = Some(read),
            Err(e) => stopped = Some(format!("world.actors: {}", why(e))),
        }
    }
    let budget = reads.budget();
    call.lap("read");

    let heat_a = (!traces_a.is_empty()).then(|| map::heat(&frame, z_range, &traces_a));
    let heat_b = (!traces_b.is_empty()).then(|| map::heat(&frame, z_range, &traces_b));
    let difference = heat_a
        .as_ref()
        .zip(heat_b.as_ref())
        .map(|(a, b)| map::difference(a, b));
    let mut layers: Vec<Value> = heat_a.as_ref().map_or(Vec::new(), |h| h.layers(&frame));
    if let (Some(a), Some(b), Some(d)) = (&heat_a, &heat_b, &difference) {
        let area = frame.cell * frame.cell;
        let max = d.iter().map(|x| x.abs()).fold(0.0, f64::max);
        layers.push(json!({"name": "occupancy_diff", "unit": "subject-s/m²/min",
            "sources": {"a": a.sources, "b": b.sources}, "bins": d.iter().filter(|x| **x != 0.0).count(),
            "max": (max * 1e4).round() / 1e4, "total": (d.iter().sum::<f64>() * area * 1e4).round() / 1e4,
            "coverage": {"a": a.coverage(), "b": b.coverage()}}));
    }
    let shown_in = |p: [f64; 3]| (p[2] - at[2]).abs() <= z_range;
    let mut bodies: Vec<([f64; 2], f64, bool)> = Vec::new();
    if shown_in(player.pos) {
        bodies.push(([player.pos[0], player.pos[1]], player.heading, true));
    }
    if let Some((list, _)) = &people {
        let shown = list.iter().filter(|o| o.shown());
        bodies.extend(shown.map(|o| ([o.body.pos[0], o.body.pos[1]], o.body.heading, false)));
    }

    out.insert("mode".into(), json!(mode));
    out.insert("at".into(), json!(at));
    let read_at = out
        .get("base")
        .map_or(json!(read_at), |b| b["read_at"].clone());
    out.insert("read_at".into(), read_at.clone());
    if grid.is_none() {
        let f = &frame;
        out.insert(
            "size".into(),
            json!([f.nx as f64 * f.cell, f.ny as f64 * f.cell]),
        );
        out.insert("cell".into(), json!(f.cell));
        out.insert("nx".into(), json!(f.nx));
        out.insert("ny".into(), json!(f.ny));
        out.insert("bbox".into(), json!(f.bbox()));
        out.insert("z_ref".into(), Value::Null);
    }
    let at_t = people.as_ref().map(|(_, about)| about["at_t"].clone());
    out.insert(
        "occupants".into(),
        people
            .as_ref()
            .map_or(Value::Null, |(_, about)| about.clone()),
    );
    if !layers.is_empty() {
        out.insert("layers".into(), json!(layers));
    }
    let age = |since: Instant| (since.elapsed().as_secs_f64() * 10.0).round() / 10.0;
    let mut sources = vec![match out.get("base") {
        Some(b) => {
            json!({"layer": "cells", "family": "game_state", "at": b["read_at"], "age_s": b["age_s"]})
        }
        None => {
            let layer = if mode == "grid" { "cells" } else { mode };
            json!({"layer": layer, "family": "game_state", "at": read_at, "age_s": age(read_from)})
        }
    }];
    if let Some(t) = &at_t {
        sources.push(json!({"layer": "occupants", "family": "game_state", "at": t, "age_s": 0.0}));
    }
    for (layer, ts, written) in [
        ("traces", &traces_a, &written_a),
        ("traces_b", &traces_b, &written_b),
    ] {
        if let Some(newest) = written.iter().max() {
            let at = session::utc(SystemTime::UNIX_EPOCH + Duration::from_secs(*newest as u64));
            sources.push(json!({"layer": layer, "family": "game_state", "captures": ts.iter().map(|t| t.id).collect::<Vec<_>>(),
                                "at": at, "age_s": session::now() as i64 - newest}));
        }
    }
    out.insert("sources".into(), json!(sources));
    out.insert("budget".into(), budget);
    out.insert(
        "limits".into(),
        json!(limits(mode, z_range, at_t.as_ref(), heat_a.is_some())),
    );
    out.insert(
        "files".into(),
        json!({"json": files[0], "png": png.then(|| &files[1])}),
    );
    if stopped.is_some() {
        out.insert("partial".into(), json!(true));
    }

    let mut file = out.clone();
    if let Some(g) = &grid {
        file.extend(g.arrays());
    }
    let mut bins = Map::new();
    for (key, h) in [("a", &heat_a), ("b", &heat_b)] {
        if let Some(h) = h {
            bins.insert(key.into(), h.bins());
        }
    }
    if let Some(d) = &difference {
        let d: Vec<f64> = d.iter().map(|x| (x * 1e5).round() / 1e5).collect();
        bins.insert("occupancy_diff".into(), json!(d));
    }
    if !bins.is_empty() {
        file.insert("layer_bins".into(), Value::Object(bins));
    }
    file.insert("legend".into(), map::legend());
    let mut given = args.clone();
    if let Some(g) = given.as_object_mut() {
        g.remove("token");
    }
    file.insert(
        "header".into(),
        json!({"tool": "world_map", "args": given, "boot": cx.live.boot}),
    );
    let ids: Vec<i64> = traces_a
        .iter()
        .chain(&traces_b)
        .map(|t| t.id)
        .chain(base_id)
        .collect();
    let frame_state = Fields::from_iter([
        ("mode".to_string(), json!(mode)),
        ("at".into(), json!(at)),
        ("size".into(), out["size"].clone()),
        ("cell".into(), json!(frame.cell)),
        ("sources".into(), json!(ids)),
        ("world".into(), json!(measure::world(&cx))),
    ]);
    let text = Value::Object(file).to_string();
    let drawing = map::Drawing {
        classes: grid.as_ref().map_or(&[][..], |g| &g.classified.class),
        heights: grid.as_ref().map_or(&[][..], |g| &g.classified.height),
        heat: heat_a.as_ref(),
        difference: difference.as_deref(),
        chokepoints: grid.as_ref().map_or(&[][..], |g| &g.chokepoints),
        lines: &lines,
        bodies: &bodies,
    };
    let written = keep(&cx, call, &files[0], text.as_bytes(), "json", &frame_state).and_then(
        |()| match png {
            true => keep(
                &cx,
                call,
                &files[1],
                &map::render(&frame, &drawing)?,
                "png",
                &frame_state,
            ),
            false => Ok(()),
        },
    );
    call.lap("files");
    // The envelope (state, timing, artifacts, ledger) comes on top: the summary keeps 2 KiB of room for it.
    let mut inline = Value::Object(out);
    if map::fit(&mut inline, &MAP_LISTS, INLINE_BYTES - 2048) {
        inline["truncated"] = json!(true);
        inline["file"] = json!(files[0]);
    }
    let Value::Object(fields) = inline else {
        unreachable!("an object stays one");
    };
    if let Err(why) = written {
        return Err(Done::Failed(why, fields));
    }
    match stopped {
        None => Ok(fields),
        Some(why) => Err(Done::Failed(
            format!("{why}: what was read so far is in the result and its files (partial)"),
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
        for tool in [
            "world_frame",
            "world_ground",
            "world_ray",
            "world_nav",
            "world_map",
        ] {
            assert!(crate::hands::user_may(tool, &json!({})), "{tool}");
        }
    }

    /// A stand-in for world_map's reads: the player at (0, 0, 10) facing +y; a street along y where |x| < 2 (navmesh
    /// at 10.5 and free, the ground at 10), beyond it no navmesh and a 2 m wall's top (a hit at 12); three actors: 3
    /// walking up the street at 1 m/s, 4 on a floor 5 m up, 5 standing in 3's way; each nav.line reaching 3 m and not
    /// clear, no ray hitting. With `cancel`, the connection goes away during that scene.ground call.
    fn map_game(
        fx: &Fake,
        conns: usize,
        cancel: Option<(std::sync::Arc<crate::mcp::Conn>, usize)>,
    ) -> std::thread::JoinHandle<Vec<Request>> {
        let grounds = std::sync::atomic::AtomicUsize::new(0);
        fake_game(
            &token(fx),
            conns,
            Box::new(move |r| {
                let data = match &r.verb {
                    Verb::WorldGet {} => json!({"player": {"pos": [0, 0, 10], "heading": 0}}),
                    Verb::WorldActors { .. } => json!({"found": 3, "actors": [
                        {"id": 3, "pos": [0.5, 2, 10], "heading": 0, "velocity": [0, 1, 0], "actor": true},
                        {"id": 4, "pos": [0, 1, 15], "heading": 90, "velocity": null, "actor": true},
                        {"id": 5, "pos": [0.6, 3.5, 10], "heading": 180, "velocity": [0, 0, 0], "actor": true}]}),
                    Verb::SceneGround { xyz, .. } => {
                        let n = grounds.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                        if let Some((conn, at)) = &cancel
                            && *at == n
                        {
                            conn.closed.store(true, std::sync::atomic::Ordering::SeqCst);
                        }
                        let cell = |p: &[f32]| match p[0].abs() < 2.0 {
                            true => {
                                json!({"nav_z": 10.5, "ground": {"pos": [p[0], p[1], 10], "normal": [0, 0, 1]},
                                           "water_level": 0, "water_depth": 0, "free": true, "safe": null})
                            }
                            false => {
                                json!({"nav_z": null, "ground": {"pos": [p[0], p[1], 12], "normal": [0, 0, 1]},
                                            "water_level": 0, "water_depth": 0, "free": null, "safe": null})
                            }
                        };
                        json!(xyz.chunks(3).map(cell).collect::<Vec<_>>())
                    }
                    Verb::NavLine { segments, .. } => {
                        let reach = |s: &[f32]| {
                            let (dx, dy) = (s[3] - s[0], s[4] - s[1]);
                            let l = dx.hypot(dy);
                            json!({"clear": false, "clear_in_direction": false,
                                   "reach": [s[0] + 3.0 * dx / l, s[1] + 3.0 * dy / l, s[2]], "edge_m": 0.5})
                        };
                        json!(segments.chunks(6).map(reach).collect::<Vec<_>>())
                    }
                    Verb::SceneRay { segments, .. } => {
                        let miss =
                            json!({"hit": false, "pos": null, "normal": null, "distance": null});
                        json!(vec![miss; segments.len() / 6])
                    }
                    _ => Value::Null,
                };
                vec![reply(r.id, true, "read", Some(data))]
            }),
        )
    }

    fn file(out: &Value, kind: &str) -> Value {
        let path = out["files"][kind].as_str().unwrap();
        serde_json::from_slice(&fs::read(path).unwrap()).unwrap_or(Value::Null)
    }

    /// The grid: ⌈cells / 16⌉ scene.ground verbs with every cell's centre, its files and captures rows; again from
    /// its JSON as base with only the occupants read; and the refusals that send nothing.
    #[test]
    fn map_reads_a_grid_in_pages_and_again_from_its_base() {
        let fx = served("scene-map");
        // world.get, 3 scene.ground (40 cells), world.actors; then the base's world.get and world.actors.
        let server = map_game(&fx, 5 + 2, None);
        let mut session = fx.session();
        let args = json!({"token": "tok", "size": [10, 4], "tag": "street"});
        let out = run(&mut session, "world_map", args);
        assert_eq!(out["ok"], true, "{out}");
        assert_eq!(
            (out["nx"].as_u64(), out["ny"].as_u64()),
            (Some(10), Some(4))
        );
        assert_eq!(out["classes"]["walkable"], 16, "{out}");
        assert_eq!(out["classes"]["obstacle"], 24, "a 2 m wall's top: {out}");
        assert_eq!(
            (out["z_ref"].as_f64(), out["ground_ref"].as_f64()),
            (Some(10.5), Some(10.0))
        );
        assert_eq!(out["chokepoints"], json!([]), "a street off both edges");
        assert_eq!(out["budget"]["calls"], 5, "{out}");
        assert_eq!(out["budget"]["rays"], 40);
        assert_eq!(out["budget"]["in_window"], false);
        let occupants = out["occupants"]["list"].as_array().unwrap();
        let off: Vec<bool> = occupants.iter().map(|o| o["off_level"] == true).collect();
        assert_eq!(off, [false, true, false], "id 4 is 5 m up: {out}");
        assert_eq!(
            out["ledger"]["captures"].as_array().unwrap().len(),
            2,
            "{out}"
        );
        assert!(
            fs::read(out["files"]["png"].as_str().unwrap())
                .unwrap()
                .starts_with(b"\x89PNG")
        );
        let json_file = file(&out, "json");
        assert_eq!(json_file["clearance_m"].as_array().unwrap().len(), 40);
        assert_eq!(json_file["legend"]["px_per_cell"], 8);
        assert!(
            out["limits"][0]
                .as_str()
                .unwrap()
                .starts_with("one level per cell")
        );
        assert!(out.to_string().len() < 16 * 1024);
        // Its JSON as base: the same cells, only the occupants read again.
        let id = out["ledger"]["captures"][0].clone();
        let again = run(
            &mut session,
            "world_map",
            json!({"token": "tok", "base": id, "tag": "again"}),
        );
        assert_eq!(again["ok"], true, "{again}");
        assert_eq!(again["base"]["capture_id"], id);
        assert_eq!(again["classes"], out["classes"]);
        assert_eq!(again["budget"]["calls"], 2);
        // Refused, nothing sent.
        for args in [
            json!({"token": "tok", "size": [200, 200]}),
            json!({"token": "tok", "mode": "line"}),
            json!({"token": "tok", "axis": [[0, 0], [1, 1]]}),
            json!({"token": "tok", "base": id, "size": [10, 10]}),
            json!({"token": "tok", "traces_b": ["walk"]}),
            json!({"token": "tok", "tag": "street"}),
            json!({"token": "tok", "traces": ["nope"]}),
            json!({"token": "tok", "traces": [id.to_string()]}),
            json!({"token": "tok", "base": 999}),
            json!({"token": "tok", "mode": "profile"}),
            json!({"token": "tok", "cell": 0.1}),
        ] {
            let out = run(&mut session, "world_map", args.clone());
            assert_eq!(out["refused"], true, "{args}: {out}");
        }
        let sent = verbs(server.join().unwrap());
        assert_eq!(sent.len(), 7, "{sent:?}");
        let points = Frame::new([0.0, 0.0, 10.0], [10.0, 4.0], 1.0)
            .unwrap()
            .points();
        for (k, page) in points.chunks(16).enumerate() {
            let ground = Verb::SceneGround {
                xyz: flat(page.iter().flatten().copied()),
                range: 2.0,
                radius: 0.4,
                search: 0.0,
            };
            assert_eq!(sent[1 + k], ground, "page {k}");
        }
        assert!(matches!(sent[4], Verb::WorldActors { max: 50, .. }));
        assert_eq!(&sent[5..], [Verb::WorldGet {}, sent[4].clone()]);
    }

    /// A cancel between scene.ground calls: the cells read so far, in the result and its files.
    #[test]
    fn a_cancelled_map_keeps_the_cells_read_so_far() {
        let fx = served("scene-map-cancel");
        let conn = crate::mcp::Conn::nobody();
        // world.get and two of the three scene.ground calls: the second closes the connection.
        let server = map_game(&fx, 3, Some((conn.clone(), 2)));
        let mut session = fx.session();
        let mut call = session.call(conn, Value::Null, None);
        let args = json!({"token": "tok", "size": [10, 4], "tag": "cut"});
        let done = crate::game::call(&mut session, &mut call, "world_map", &args);
        let out = session.envelope("world_map", call, done);
        assert_eq!(
            (out["ok"].as_bool(), out["refused"].as_bool()),
            (Some(false), Some(false)),
            "{out}"
        );
        assert!(
            out["reason"]
                .as_str()
                .unwrap()
                .starts_with("cancelled after 2 of 3 scene.ground calls"),
            "{out}"
        );
        assert_eq!(out["read"], 32, "{out}");
        assert_eq!(out["classes"]["unread"], 8);
        assert_eq!(out["partial"], true);
        assert_eq!(out["occupants"], Value::Null, "no read after the cancel");
        assert_eq!(file(&out, "json")["read"], 32);
        assert_eq!(verbs(server.join().unwrap()).len(), 3);
    }

    /// A profile of 200 steps: 25 nav.line calls of 8 steps each way; its summary over 16 KB stays inline, cut,
    /// with the file whole.
    #[test]
    fn a_long_profile_keeps_its_summary_inline_and_the_rest_in_its_file() {
        let fx = served("scene-map-profile");
        let server = map_game(&fx, 1 + 25 + 1, None);
        let mut session = fx.session();
        let axis = json!([[0, -99.5, 10], [0, 99.5, 10]]);
        let args = json!({"token": "tok", "mode": "profile", "axis": axis, "tag": "long"});
        assert_eq!(map_calls(&args), 27);
        let out = run(&mut session, "world_map", args);
        assert_eq!(out["ok"], true, "{out}");
        assert_eq!(out["truncated"], true, "{out}");
        assert_eq!(out["file"], out["files"]["json"]);
        assert!(
            out.to_string().len() < 16 * 1024,
            "{}",
            out.to_string().len()
        );
        let whole = file(&out, "json");
        assert_eq!(whole["profile"]["steps"].as_array().unwrap().len(), 200);
        assert!(out["profile"]["steps"].as_array().unwrap().len() < 200);
        assert_eq!(whole["profile"]["min_width_m"], 6.0, "3 m each way");
        assert_eq!(out["unverified"], json!(["clear_in_direction"]));
        let sent = verbs(server.join().unwrap());
        // The first station, its left (-x along +y) and its right.
        let Verb::NavLine { segments, width } = &sent[1] else {
            panic!("{:?}", sent[1]);
        };
        let first = [
            0.0, -99.5, 10.0, -20.0, -99.5, 10.0, 0.0, -99.5, 10.0, 20.0, -99.5, 10.0,
        ];
        assert_eq!(
            (&segments[..12], segments.len(), *width),
            (&first[..], 96, 0.4)
        );
        assert_eq!(
            sent.iter()
                .filter(|v| matches!(v, Verb::NavLine { .. }))
                .count(),
            25
        );
    }

    /// blockers: the walker's nav.line and scene.ray along its velocity, and the body standing in its way.
    #[test]
    fn a_walker_is_blocked_by_the_body_in_its_way() {
        let fx = served("scene-map-blockers");
        let server = map_game(&fx, 4, None);
        let mut session = fx.session();
        let out = run(
            &mut session,
            "world_map",
            json!({"token": "tok", "mode": "blockers", "tag": "walkers"}),
        );
        assert_eq!(out["ok"], true, "{out}");
        assert_eq!(out["walkers"], 1, "{out}");
        let b = &out["blockers"][0];
        assert_eq!(
            (
                b["verdict"].as_str(),
                b["by"].as_i64(),
                b["dist_m"].as_f64()
            ),
            (Some("actor"), Some(5), Some(1.5)),
            "{out}"
        );
        assert_eq!(b["edge_m"], 3.0);
        let sent = verbs(server.join().unwrap());
        assert_eq!(
            sent[2],
            Verb::NavLine {
                segments: vec![0.5, 2.0, 10.0, 0.5, 5.0, 10.0],
                width: 0.4
            }
        );
        assert_eq!(
            sent[3],
            Verb::SceneRay {
                segments: vec![0.5, 2.0, 10.5, 0.5, 5.0, 10.5],
                radius: 0.0
            }
        );
    }

    /// Heat layers from a trace CSV found by tag and by id: each layer's total is the trace's metric.
    #[test]
    fn map_layers_from_traces_sum_to_their_metrics() {
        let fx = served("scene-map-traces");
        let server = map_game(&fx, 5, None);
        let body = |id: i64, x: f64, y: f64, heading: f64, v: f64| trace::Body {
            id: Some(id),
            pos: [x, y, 10.0],
            heading,
            velocity: Some([v, 0.0, 0.0]),
            flags: 64,
            entity: "npc".into(),
        };
        let samples: Vec<trace::Sample> = (0..=50)
            .map(|i| {
                let t = f64::from(i) / 10.0;
                trace::Sample {
                    t,
                    reference: Some(body(0, 0.0, 0.0, 0.0, 0.0)),
                    subjects: vec![body(1, -2.5 + t, 0.5, -90.0, 1.0)],
                    found: 1,
                }
            })
            .collect();
        let m = trace::metrics(&samples, trace::CONTACT_M).unwrap();
        let text = trace::csv(&samples);
        let csv = fx.dir.join("data/captures/s1/walk-trace.csv");
        fs::create_dir_all(csv.parent().unwrap()).unwrap();
        fs::write(&csv, &text).unwrap();
        let config = fx.shared.config().as_ref().clone().unwrap();
        let row = Capture {
            boot_id: Some(1),
            path: "s1/walk-trace.csv".into(),
            sha256: sha256_hex(text.as_bytes()),
            bytes: text.len() as i64,
            kind: "csv".into(),
            method: Some("actor_trace".into()),
            t: "2026-10-21T10:00:00Z".into(),
            ..Capture::default()
        };
        let id = session::open_ledger(&config)
            .unwrap()
            .add_capture(&row)
            .unwrap();
        let mut session = fx.session();
        let args = json!({"token": "tok", "size": [10, 4], "tag": "heat", "traces": ["walk"],
                          "traces_b": [id.to_string()]});
        let out = run(&mut session, "world_map", args);
        assert_eq!(out["ok"], true, "{out}");
        let layer = |name: &str| {
            out["layers"]
                .as_array()
                .unwrap()
                .iter()
                .find(|l| l["name"] == name)
                .cloned()
                .unwrap()
        };
        let close = |v: &Value, want: f64| (v.as_f64().unwrap() - want).abs() < 1e-3;
        assert!(
            close(&layer("occupancy")["total"], m.subject_minutes),
            "{out}"
        );
        assert!(
            close(&layer("contacts")["total"], m.contacts_per_min),
            "{out}"
        );
        assert_eq!(layer("occupancy")["sources"], json!([id]));
        assert_eq!(layer("occupancy_diff")["bins"], 0, "the same trace twice");
        assert_eq!(out["sources"][2]["layer"], "traces", "{out}");
        assert!(file(&out, "json")["layer_bins"]["a"]["occupancy"].is_array());
        // A capture that isn't a trace: refused, nothing sent.
        let map_json = out["ledger"]["captures"][0].clone();
        let not = run(
            &mut session,
            "world_map",
            json!({"token": "tok", "traces": [map_json.to_string()]}),
        );
        assert!(
            not["reason"]
                .as_str()
                .unwrap()
                .contains("is no actor trace"),
            "{not}"
        );
        assert_eq!(verbs(server.join().unwrap()).len(), 5);
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
