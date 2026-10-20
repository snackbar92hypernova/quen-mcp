//! The command channel between quen and an in-game end, as JSON lines (docs/intent.md, "The in-game package").
//!
//! Game-neutral: the Witcher 3 end is `W3Quen.asi` + `modQuen` (quen's package, `w3/`), and CET, BG3SE or UE4SS ends
//! can reuse this side.
//! One message per line, newline-terminated. A request is one flat object, the verb's fields beside `id` and `ttl_ms`:
//! `{"id":7,"ttl_ms":2000,"verb":"load","save":"..."}`. Verbs are typed: `exec` names an exec function and its
//! arguments, never a raw console line. Unknown verbs and fields, oversize lines and non-finite floats are refused
//! both ways.

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::fmt;

use crate::sweep::{self, Ease, Keyframe, Mode, Path, Speed, is_default};

/// The longest request line, without its newline. The in-game end reads requests into a buffer of this size.
pub const MAX_REQUEST_BYTES: usize = 4096;
/// The longest reply line, without its newline. Larger than a request: `listsaves` returns every save name.
pub const MAX_REPLY_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    /// The in-game end drops a request older than this instead of running it late.
    pub ttl_ms: u32,
    #[serde(flatten)]
    pub verb: Verb,
}

/// `pos` and `rot` pass through unchanged, in whatever units the in-game end's scripts take. Verbs without fields are
/// empty structs, not unit variants: serde lets a unit variant of a tagged enum ignore stray fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "verb", deny_unknown_fields)]
pub enum Verb {
    /// Load the save whose name matches, case-insensitively, with or without `.sav`.
    #[serde(rename = "load")]
    Load { save: String },
    #[serde(rename = "loadlast")]
    LoadLast {},
    #[serde(rename = "listsaves")]
    ListSaves {},
    #[serde(rename = "cam.get")]
    CamGet {},
    #[serde(rename = "cam.set")]
    CamSet {
        pos: [f32; 3],
        rot: [f32; 3],
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fov: Option<f32>,
    },
    /// Place the player (it taints later saves).
    #[serde(rename = "teleport")]
    Teleport { pos: [f32; 3], rot: [f32; 3] },
    #[serde(rename = "settime")]
    SetTime { day: u32, hour: u32 },
    #[serde(rename = "weather")]
    Weather { name: String },
    #[serde(rename = "pause")]
    Pause { on: bool },
    /// Load a camera path on the photo-mode camera, replacing a loaded one: `realtime` plays it from the
    /// next tick by local time, `step` (with `fps`) sets frame 0 and holds. The game interpolates the keyframes as
    /// `sweep::Path` does, so they must be canonical (`Path::keys`). At most `sweep::MAX_KEYFRAMES`.
    #[serde(rename = "cam.path")]
    CamPath {
        mode: Mode,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fps: Option<u32>,
        #[serde(default, skip_serializing_if = "is_default")]
        speed: Speed,
        #[serde(default, skip_serializing_if = "is_default")]
        ease: Ease,
        keys: Vec<Keyframe>,
    },
    /// Set frame `n` of a loaded step path, its time `n / fps`; the reply's data is the read-back.
    #[serde(rename = "cam.frame")]
    CamFrame { n: u32 },
    /// Unload the path; the camera stays where it is.
    #[serde(rename = "cam.stop")]
    CamStop {},
    /// Write a new manual save, as the menu's "Empty save slot" does. No fields: a request can't name a slot, a
    /// save type or an existing save, so it can't ask for an overwrite or a quicksave.
    #[serde(rename = "save")]
    Save {},
    /// Read the player's position and heading, the game time (day and hour) and the weather's name; changes nothing.
    #[serde(rename = "world.get")]
    WorldGet {},
    /// Show `text` on screen for a few seconds (the game's notification); changes nothing in the game. 1 to
    /// [`MAX_NOTICE_BYTES`] bytes, no control character.
    #[serde(rename = "notice")]
    Notice { text: String },
    /// Open (`on`) or close photo mode by the game's own functions, never by keys; closing skips its exit dialog.
    #[serde(rename = "photo.mode")]
    PhotoMode { on: bool },
    /// Read photo mode's own state (the reply's data, as after every photo set below); changes nothing.
    #[serde(rename = "photo.get")]
    PhotoGet {},
    /// Photo mode's own weather: a name of the area's list.
    #[serde(rename = "photo.weather")]
    PhotoWeather { name: String },
    /// Photo mode's environment definition, by a path the game lists (`photo.list` envs).
    #[serde(rename = "photo.env")]
    PhotoEnv { path: String },
    /// Photo mode's time of day, `minute` as its menu's slider passes it: minutes of the day, 0 to below 1440.
    #[serde(rename = "photo.time")]
    PhotoTime { minute: f32 },
    /// Photo mode's grading sliders: the ones given, at least one.
    #[serde(rename = "photo.grade")]
    PhotoGrade {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exposure: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        contrast: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        highlights: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        saturation: Option<f32>,
        /// Kelvin.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        temperature: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        vignette: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        grain: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chromatic_aberration: Option<f32>,
    },
    /// Photo mode's weather (the world's back), env (none) or grading (the sliders as photo mode opened).
    #[serde(rename = "photo.reset")]
    PhotoReset { part: PhotoPart },
    /// Photo mode's one-off world tick and cloth simulation, in seconds, 0 to [`MAX_TICK_S`]: at least one.
    #[serde(rename = "photo.tick")]
    PhotoTick {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tick_s: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cloth_s: Option<f32>,
    },
    /// The area's weather names or every environment definition's path, those holding `filter` (any case) if given.
    #[serde(rename = "photo.list")]
    PhotoList {
        what: PhotoListed,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filter: Option<String>,
    },
    /// Add `count` of the item `name` (its own name, spaces allowed) to the player's inventory, as the console's
    /// `additem` does, and with `equip` equip the first; the reply's data reads the item's count back (it taints later
    /// saves). 1 to [`MAX_ITEM_BYTES`] bytes, no control character; `count` 1 to [`MAX_ITEM_COUNT`].
    #[serde(rename = "items.add")]
    ItemsAdd {
        name: String,
        count: u32,
        equip: bool,
    },
    /// Read the alive actors within `radius` of the player, nearest first, at most `max`: each one's position, heading,
    /// velocity, names and flags (the reply's data); changes nothing. `radius` above 0 to [`MAX_ACTORS_RADIUS`], `max`
    /// 1 to [`MAX_ACTORS`]. From quen-w3 0.1.12: only those with `tag`, and with `class` the gameplay entities of that
    /// class instead, actors or not; each with its id. Both names as [`crate::trace::plain_name`] checks them.
    #[serde(rename = "world.actors")]
    WorldActors {
        radius: f32,
        max: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tag: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        class: Option<String>,
    },
    /// Read what the rendered camera sees (quen-w3 0.1.13): the alive actors within `radius` of the camera, or of `at`,
    /// the player among them, nearest first, at most `max`; with `entity_id` only the entity of that id (`world.actors`'
    /// `id`; the request's own `id` is taken).
    /// Each one projected for the rendered camera, its box's share of the frame, and its first `points` of head, torso
    /// and origin each tested by a ray from the camera. `radius` above 0 to [`MAX_ACTORS_RADIUS`], `max` 1 to
    /// [`MAX_FRAME`], `points` 0 to 3. Changes nothing.
    #[serde(rename = "scene.frame")]
    SceneFrame {
        radius: f32,
        max: u32,
        points: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at: Option<[f32; 3]>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        entity_id: Option<i32>,
    },
    /// Read the ground at points, `xyz` flat (x, y, z per point, 1 to [`MAX_QUERIES`] points): the navmesh z within
    /// `range` of each z, a ray down through that range, the water; with `radius` above 0 whether a navmesh circle of
    /// that radius is free there, with `search` above 0 the nearest safe spot within it. Changes nothing.
    #[serde(rename = "scene.ground")]
    SceneGround {
        xyz: Vec<f32>,
        range: f32,
        radius: f32,
        search: f32,
    },
    /// Cast a ray from a to b per segment (`segments` flat: ax, ay, az, bx, by, bz; 1 to [`MAX_QUERIES`]), or with
    /// `radius` above 0 sweep a sphere of that radius: the first hit. Changes nothing.
    #[serde(rename = "scene.ray")]
    SceneRay { segments: Vec<f32>, radius: f32 },
    /// Test a navmesh lane `width` wide from a to b per segment (as scene.ray's): clear or not, the furthest point
    /// reached towards b, the distance to the nearest obstacle. Changes nothing.
    #[serde(rename = "nav.line")]
    NavLine { segments: Vec<f32>, width: f32 },
    /// Turn the photo-mode camera's no-clip on, or back to what photo mode's menu has.
    #[serde(rename = "cam.noclip")]
    CamNoclip { on: bool },
    /// Run the global `exec function` named `function` with `args`, as the console runs `function(args)` (quen-w3
    /// 0.1.13): W3Quen.asi calls the game's console executor, no console is typed. [`Verb::exec_line`] is the line.
    #[serde(rename = "exec")]
    Exec {
        function: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        args: Vec<String>,
    },
}

/// What `photo.reset` puts back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhotoPart {
    Weather,
    Env,
    Grading,
}

/// What `photo.list` lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhotoListed {
    Weathers,
    Envs,
}

/// The longest notice text, in bytes.
pub const MAX_NOTICE_BYTES: usize = 160;
/// The longest `photo.list` filter, in bytes.
pub const MAX_FILTER_BYTES: usize = 200;
/// The longest `photo.tick` time, in seconds (photo mode's menu itself passes 0.00001 s and 1 s).
pub const MAX_TICK_S: f32 = 10.0;
/// The longest `items.add` item name, in bytes.
pub const MAX_ITEM_BYTES: usize = 200;
/// The most of one item `items.add` adds.
pub const MAX_ITEM_COUNT: u32 = 1000;
/// The widest `world.actors` radius, in the scripts' units (metres).
pub const MAX_ACTORS_RADIUS: f32 = 100.0;
/// The most actors one `world.actors` reply holds: 50 of them, the display name cut at 80 characters and the entity at
/// 255 (quen-w3 0.1.11), take about 53 KB at worst (every entity character a `\`, escaped; every name character 3
/// UTF-8 bytes), under [`MAX_REPLY_BYTES`]. Names full of control characters (6 bytes each, escaped) would exceed it:
/// the ASI then answers that the reply is too long.
pub const MAX_ACTORS: u32 = 50;
/// The most entities one `scene.frame` reads: with 3 points, 48 rays in the tick, under the runner design's cap of 64
/// rays a tick (until measured).
pub const MAX_FRAME: u32 = 16;
/// The most points, segments or lanes one `scene.ground`, `scene.ray` or `nav.line` takes: the design's 16 queries a
/// tick.
pub const MAX_QUERIES: usize = 16;
/// `scene.ground`'s widest `range` and `search`, `radius` (metres), and `scene.ray`'s and `nav.line`'s widest.
pub const MAX_GROUND_RANGE: f32 = 100.0;
pub const MAX_GROUND_SEARCH: f32 = 50.0;
pub const MAX_GROUND_RADIUS: f32 = 10.0;
pub const MAX_SWEEP_RADIUS: f32 = 2.0;
pub const MAX_NAV_WIDTH: f32 = 5.0;
/// `exec`: at most this many arguments, each at most `MAX_EXEC_ARG_BYTES`, the whole line at most `MAX_EXEC_LINE_BYTES`.
pub const MAX_EXEC_ARGS: usize = 8;
pub const MAX_EXEC_ARG_BYTES: usize = 100;
pub const MAX_EXEC_LINE_BYTES: usize = 512;

/// An `exec` argument the console's tokenizer reads: printable ASCII without `"` or `'` (it ends a quoted token at
/// either, with no escapes: witcher3-mods re notes/console_exec.md).
fn exec_arg(a: &str) -> bool {
    a.len() <= MAX_EXEC_ARG_BYTES
        && a.bytes()
            .all(|b| (0x20..0x7f).contains(&b) && b != b'"' && b != b'\'')
}

/// An argument passed bare: one the tokenizer reads whole (no space, comma or parenthesis); the rest go in `"`.
fn bare(a: &str) -> bool {
    !a.is_empty()
        && a.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.+-".contains(&b))
}

/// The coordinates a scene verb passes: finite, inside sweep's world.
fn coords(v: &[f32]) -> bool {
    v.iter()
        .all(|x| x.is_finite() && f64::from(x.abs()) < sweep::MAX_COORD)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reply {
    /// The id of the request this answers.
    pub id: u64,
    pub ok: bool,
    /// What happened, or why not.
    pub text: String,
    /// Read-back, such as a camera pose or the save list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The line is longer than the cap for its direction.
    TooLong { len: usize, max: usize },
    /// Not one JSON object of the expected shape: bad JSON, a line break inside, an unknown verb or field, a wrong type.
    Malformed(String),
    /// Well-formed, but a value the game must not get: a non-finite float, an hour past 23, an empty name.
    Invalid(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::TooLong { len, max } => write!(f, "message of {len} bytes, the limit is {max}"),
            Error::Malformed(msg) => write!(f, "malformed message: {msg}"),
            Error::Invalid(msg) => write!(f, "invalid message: {msg}"),
        }
    }
}

impl std::error::Error for Error {}

/// photo.grade's sliders, in the order the game reads them back (the photo read-back's `grading`).
pub const GRADING: [&str; 8] = [
    "exposure",
    "contrast",
    "highlights",
    "saturation",
    "temperature",
    "vignette",
    "grain",
    "chromatic_aberration",
];

impl Verb {
    /// photo.grade's values in [`GRADING`]'s order, or photo.tick's tick_s and cloth_s; none for another verb.
    pub fn photo_values(&self) -> Vec<Option<f32>> {
        match *self {
            Verb::PhotoGrade {
                exposure,
                contrast,
                highlights,
                saturation,
                temperature,
                vignette,
                grain,
                chromatic_aberration,
            } => vec![
                exposure,
                contrast,
                highlights,
                saturation,
                temperature,
                vignette,
                grain,
                chromatic_aberration,
            ],
            Verb::PhotoTick { tick_s, cloth_s } => vec![tick_s, cloth_s],
            _ => Vec::new(),
        }
    }

    fn check(&self) -> Result<(), Error> {
        let finite = |v: &[f32]| v.iter().all(|x| x.is_finite());
        let name = |s: &str| !s.is_empty() && !s.bytes().any(|b| b < 0x20);
        match self {
            Verb::PhotoWeather { name: s } | Verb::PhotoEnv { path: s } if !name(s) => Err(
                Error::Invalid("an empty name, or one with a control character".into()),
            ),
            Verb::PhotoList {
                filter: Some(filter),
                ..
            } if !name(filter) || filter.len() > MAX_FILTER_BYTES => Err(Error::Invalid(format!(
                "a filter is 1 to {MAX_FILTER_BYTES} bytes, no control character"
            ))),
            Verb::ItemsAdd { name: s, count, .. }
                if !name(s)
                    || s.len() > MAX_ITEM_BYTES
                    || !(1..=MAX_ITEM_COUNT).contains(count) =>
            {
                Err(Error::Invalid(format!(
                    "items.add: a name of 1 to {MAX_ITEM_BYTES} bytes, no control character, and a count of 1 \
                     to {MAX_ITEM_COUNT}"
                )))
            }
            Verb::WorldActors {
                radius,
                max,
                tag,
                class,
            } if !(*radius > 0.0
                && *radius <= MAX_ACTORS_RADIUS
                && (1..=MAX_ACTORS).contains(max)
                && [tag, class]
                    .iter()
                    .all(|n| n.as_deref().is_none_or(crate::trace::plain_name))) =>
            {
                Err(Error::Invalid(format!(
                    "world.actors: a radius above 0 to {MAX_ACTORS_RADIUS}, a max of 1 to {MAX_ACTORS}, and a tag or \
                     class of 1 to 64 letters, digits and _"
                )))
            }
            Verb::PhotoTime { minute } if !(0.0..1440.0).contains(minute) => Err(Error::Invalid(
                format!("photo.time minute {minute}: 0 to below 1440"),
            )),
            Verb::PhotoGrade { .. } | Verb::PhotoTick { .. } => {
                let given: Vec<f32> = self.photo_values().into_iter().flatten().collect();
                let tick = matches!(self, Verb::PhotoTick { .. });
                if given.is_empty() || !finite(&given) {
                    Err(Error::Invalid("no value given, or one not finite".into()))
                } else if tick && !given.iter().all(|s| (0.0..=MAX_TICK_S).contains(s)) {
                    Err(Error::Invalid(format!(
                        "photo.tick seconds 0 to {MAX_TICK_S}"
                    )))
                } else {
                    Ok(())
                }
            }
            Verb::CamSet { pos, rot, fov }
                if !finite(pos) || !finite(rot) || fov.is_some_and(|f| !f.is_finite()) =>
            {
                Err(Error::Invalid("cam.set with a non-finite number".into()))
            }
            Verb::Teleport { pos, rot } if !finite(pos) || !finite(rot) => {
                Err(Error::Invalid("teleport with a non-finite number".into()))
            }
            Verb::SetTime { hour, .. } if *hour > 23 => {
                Err(Error::Invalid(format!("settime hour {hour}, past 23")))
            }
            Verb::Load { save: name } | Verb::Weather { name } if name.is_empty() => {
                Err(Error::Invalid("an empty name".into()))
            }
            Verb::Notice { text }
                if text.is_empty()
                    || text.len() > MAX_NOTICE_BYTES
                    || text.bytes().any(|b| b < 0x20) =>
            {
                Err(Error::Invalid(format!(
                    "a notice is 1 to {MAX_NOTICE_BYTES} bytes, no control character"
                )))
            }
            Verb::CamPath {
                mode,
                fps,
                speed,
                ease,
                keys,
            } => sweep::check_playback(*mode, *fps)
                .and_then(|()| Path::new(keys, *speed, *ease))
                .and_then(|path| {
                    if path.keys() == keys.as_slice() {
                        Ok(())
                    } else {
                        Err(sweep::SweepError(
                            "keyframes not canonical: send Path::keys".into(),
                        ))
                    }
                })
                .map_err(|e| Error::Invalid(format!("cam.path: {e}"))),
            Verb::SceneFrame {
                radius,
                max,
                points,
                at,
                ..
            } if !(*radius > 0.0
                && *radius <= MAX_ACTORS_RADIUS
                && (1..=MAX_FRAME).contains(max)
                && *points <= 3
                && at.is_none_or(|a| coords(&a))) =>
            {
                Err(Error::Invalid(format!(
                    "scene.frame: a radius above 0 to {MAX_ACTORS_RADIUS}, a max of 1 to {MAX_FRAME}, points 0 to 3, \
                     an at of finite coordinates"
                )))
            }
            Verb::SceneGround {
                xyz,
                range,
                radius,
                search,
            } if !(per(xyz, 3)
                && *range > 0.0
                && *range <= MAX_GROUND_RANGE
                && (0.0..=MAX_GROUND_RADIUS).contains(radius)
                && (0.0..=MAX_GROUND_SEARCH).contains(search)) =>
            {
                Err(Error::Invalid(format!(
                    "scene.ground: 1 to {MAX_QUERIES} points of finite x, y, z, a range above 0 to \
                     {MAX_GROUND_RANGE}, a radius 0 to {MAX_GROUND_RADIUS}, a search 0 to {MAX_GROUND_SEARCH}"
                )))
            }
            Verb::SceneRay { segments, radius }
                if !(per(segments, 6) && (0.0..=MAX_SWEEP_RADIUS).contains(radius)) =>
            {
                Err(Error::Invalid(format!(
                    "scene.ray: 1 to {MAX_QUERIES} segments of finite a and b, a radius 0 to {MAX_SWEEP_RADIUS}"
                )))
            }
            Verb::NavLine { segments, width }
                if !(per(segments, 6) && (0.0..=MAX_NAV_WIDTH).contains(width)) =>
            {
                Err(Error::Invalid(format!(
                    "nav.line: 1 to {MAX_QUERIES} segments of finite a and b, a width 0 to {MAX_NAV_WIDTH}"
                )))
            }
            Verb::Exec { function, args } => {
                let line = self.exec_line().unwrap_or_default();
                if !crate::trace::plain_name(function)
                    || args.len() > MAX_EXEC_ARGS
                    || !args.iter().all(|a| exec_arg(a))
                    || line.len() > MAX_EXEC_LINE_BYTES
                {
                    return Err(Error::Invalid(format!(
                        "exec: a function name of 1 to 64 letters, digits and _, at most {MAX_EXEC_ARGS} arguments \
                         of at most {MAX_EXEC_ARG_BYTES} bytes of printable ASCII without quotes, a line of at most \
                         {MAX_EXEC_LINE_BYTES} bytes"
                    )));
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// An `exec` verb's console line, as W3Quen.asi builds it from the verb: `function(a, b)`, an argument bare when the
    /// console's tokenizer reads it whole (letters, digits and `_ . + -`), else in `"`. None for another verb.
    pub fn exec_line(&self) -> Option<String> {
        let Verb::Exec { function, args } = self else {
            return None;
        };
        let args: Vec<String> = args
            .iter()
            .map(|a| match bare(a) {
                true => a.clone(),
                false => format!("\"{a}\""),
            })
            .collect();
        Some(format!("{function}({})", args.join(", ")))
    }

    /// The `exec` verb for a console line `function` or `function(a, b, ...)`: each argument bare or in `"` or `'`,
    /// spaces around it dropped. The verb's own checks follow.
    pub fn exec(line: &str) -> Result<Verb, Error> {
        let bad = |why: &str| Error::Invalid(format!("exec line {line:?}: {why}"));
        let line_t = line.trim();
        let name_end = line_t
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(line_t.len());
        let (function, rest) = line_t.split_at(name_end);
        let rest = rest.trim();
        let mut args = Vec::new();
        if !rest.is_empty() {
            let inner = rest
                .strip_prefix('(')
                .and_then(|r| r.strip_suffix(')'))
                .ok_or_else(|| bad("after the name, only (arguments)"))?;
            if !inner.trim().is_empty() {
                for piece in split_args(inner).ok_or_else(|| bad("a quote that doesn't close"))? {
                    let p = piece.trim();
                    let quoted = p.len() >= 2
                        && matches!(p.as_bytes()[0], b'"' | b'\'')
                        && matches!(p.as_bytes()[p.len() - 1], b'"' | b'\'');
                    let arg = if quoted { &p[1..p.len() - 1] } else { p };
                    if !quoted && (arg.is_empty() || arg.contains(['(', ')', '"', '\''])) {
                        return Err(bad(
                            "an empty argument, or one with a parenthesis or quote outside quotes",
                        ));
                    }
                    args.push(arg.to_string());
                }
            }
        }
        let verb = Verb::Exec {
            function: function.into(),
            args,
        };
        verb.check()?;
        Ok(verb)
    }
}

/// `v` holds whole groups of `n` coordinates, 1 to [`MAX_QUERIES`] of them.
fn per(v: &[f32], n: usize) -> bool {
    !v.is_empty() && v.len().is_multiple_of(n) && v.len() / n <= MAX_QUERIES && coords(v)
}

/// The pieces of an argument list between commas outside quotes (a quote runs to the next `"` or `'`, as the
/// console's tokenizer reads it); None if a quote doesn't close.
fn split_args(s: &str) -> Option<Vec<&str>> {
    let (mut out, mut start, mut quote) = (Vec::new(), 0, false);
    for (i, b) in s.bytes().enumerate() {
        match b {
            b'"' | b'\'' => quote = !quote,
            b',' if !quote => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    (!quote).then(|| {
        out.push(&s[start..]);
        out
    })
}

/// One request line, newline included.
pub fn encode_request(request: &Request) -> Result<String, Error> {
    request.verb.check()?;
    encode(request, MAX_REQUEST_BYTES)
}

/// A request from one line, with or without its newline.
pub fn decode_request(line: &str) -> Result<Request, Error> {
    let request: Request = decode(line, MAX_REQUEST_BYTES)?;
    request.verb.check()?;
    Ok(request)
}

/// One reply line, newline included.
pub fn encode_reply(reply: &Reply) -> Result<String, Error> {
    encode(reply, MAX_REPLY_BYTES)
}

/// A reply from one line, with or without its newline.
pub fn decode_reply(line: &str) -> Result<Reply, Error> {
    decode(line, MAX_REPLY_BYTES)
}

fn encode<T: Serialize>(message: &T, max: usize) -> Result<String, Error> {
    let mut line = serde_json::to_string(message).map_err(|e| Error::Malformed(e.to_string()))?;
    if line.len() > max {
        return Err(Error::TooLong {
            len: line.len(),
            max,
        });
    }
    line.push('\n');
    Ok(line)
}

fn decode<T: DeserializeOwned>(line: &str, max: usize) -> Result<T, Error> {
    let line = line.strip_suffix('\n').unwrap_or(line);
    let line = line.strip_suffix('\r').unwrap_or(line);
    if line.len() > max {
        return Err(Error::TooLong {
            len: line.len(),
            max,
        });
    }
    if line.contains('\n') {
        return Err(Error::Malformed("a line break inside the message".into()));
    }
    serde_json::from_str(line).map_err(|e| Error::Malformed(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request(verb: Verb) -> Request {
        Request {
            id: 7,
            ttl_ms: 2000,
            verb,
        }
    }

    fn every_verb() -> Vec<Verb> {
        vec![
            Verb::Load {
                save: "test save 1".into(),
            },
            Verb::LoadLast {},
            Verb::ListSaves {},
            Verb::CamGet {},
            Verb::CamSet {
                pos: [1.5, -2.25, 30.0],
                rot: [0.0, -10.5, 359.0],
                fov: Some(60.0),
            },
            Verb::CamSet {
                pos: [0.0; 3],
                rot: [0.0; 3],
                fov: None,
            },
            Verb::Teleport {
                pos: [-100.0, 200.5, 3.0],
                rot: [0.0, 0.0, 90.0],
            },
            Verb::SetTime { day: 3, hour: 23 },
            Verb::Weather {
                name: "WT_Clear".into(),
            },
            Verb::Pause { on: true },
            Verb::CamPath {
                mode: Mode::Realtime,
                fps: None,
                speed: Speed::Keys,
                ease: Ease::Linear,
                keys: path_keys(),
            },
            Verb::CamPath {
                mode: Mode::Step,
                fps: Some(30),
                speed: Speed::Constant,
                ease: Ease::EaseInOut,
                keys: path_keys(),
            },
            Verb::CamFrame { n: 4_000_000 },
            Verb::CamStop {},
            Verb::Save {},
            Verb::WorldGet {},
            Verb::Notice {
                text: "quen: you have the controls".into(),
            },
            Verb::PhotoMode { on: true },
            Verb::PhotoGet {},
            Verb::PhotoWeather {
                name: "WT_Clear".into(),
            },
            Verb::PhotoEnv {
                path: r"environment\definitions\synthetic test.env".into(),
            },
            Verb::PhotoTime { minute: 1439.5 },
            photo_grade(Some(0.5), Some(6550.0)),
            Verb::PhotoReset {
                part: PhotoPart::Grading,
            },
            Verb::PhotoTick {
                tick_s: Some(0.00001),
                cloth_s: None,
            },
            Verb::PhotoList {
                what: PhotoListed::Envs,
                filter: Some("novigrad".into()),
            },
            Verb::PhotoList {
                what: PhotoListed::Weathers,
                filter: None,
            },
            Verb::ItemsAdd {
                name: "Red Wolf Armor 2".into(),
                count: 1,
                equip: true,
            },
            Verb::WorldActors {
                radius: 30.0,
                max: 20,
                tag: None,
                class: None,
            },
            Verb::WorldActors {
                radius: 8.0,
                max: 50,
                tag: Some("q104_ciri".into()),
                class: Some("CNewNPC".into()),
            },
            Verb::SceneFrame {
                radius: 30.0,
                max: 16,
                points: 3,
                at: None,
                entity_id: None,
            },
            Verb::SceneFrame {
                radius: 2.0,
                max: 1,
                points: 0,
                at: Some([604.87, 2089.32, 30.99]),
                entity_id: Some(-123_456_789),
            },
            Verb::SceneGround {
                xyz: vec![1.0, 2.0, 3.0, -4.5, 5.5, 6.0],
                range: 10.0,
                radius: 0.5,
                search: 0.0,
            },
            Verb::SceneRay {
                segments: vec![0.0, 0.0, 1.0, 3.0, 0.0, 1.5],
                radius: 0.0,
            },
            Verb::NavLine {
                segments: vec![0.0, 0.0, 1.0, 3.0, 0.0, 1.5],
                width: 0.4,
            },
            Verb::CamNoclip { on: true },
            Verb::Exec {
                function: "npcavoid".into(),
                args: Vec::new(),
            },
            Verb::Exec {
                function: "addset".into(),
                args: vec![
                    "EIST_Wolf".into(),
                    "true".into(),
                    "a b".into(),
                    String::new(),
                ],
            },
        ]
    }

    /// The exec verb's line, as W3Quen.asi builds it (test.cpp checks the same lines), and the verb read from a console
    /// line: arguments bare or quoted, spaces dropped.
    #[test]
    fn exec_lines_round_trip_between_console_text_and_the_verb() {
        let exec = |f: &str, args: &[&str]| Verb::Exec {
            function: f.into(),
            args: args.iter().map(|a| a.to_string()).collect(),
        };
        assert_eq!(exec("npcavoid", &[]).exec_line().unwrap(), "npcavoid()");
        assert_eq!(
            exec("addset", &["EIST_Wolf", "true", "a b", "", "1.5e-3", "x,y"])
                .exec_line()
                .unwrap(),
            r#"addset(EIST_Wolf, true, "a b", "", 1.5e-3, "x,y")"#
        );
        assert_eq!(Verb::CamGet {}.exec_line(), None);
        for (line, want) in [
            ("npcavoid()", exec("npcavoid", &[])),
            ("  npcavoid  ", exec("npcavoid", &[])),
            ("settime(1,22)", exec("settime", &["1", "22"])),
            (
                "addset( EIST_Wolf , true )",
                exec("addset", &["EIST_Wolf", "true"]),
            ),
            (
                r#"say("hello, world", 'x y', "")"#,
                exec("say", &["hello, world", "x y", ""]),
            ),
            ("f(  )", exec("f", &[])),
        ] {
            assert_eq!(Verb::exec(line).unwrap(), want, "{line}");
        }
        for line in [
            "",
            "(1)",
            "f(1",
            "f 1",
            "f(1) x",
            "f(,1)",
            "f(1,)",
            r#"f("a)"#,
            r#"f("a'b")"#,
            r#"f("a"b)"#,
            "f(a(b))",
            "f(\u{e9})",
            "f(a\tb)",
            "two words",
            &"n".repeat(65),
            &format!("f({})", ["1"; MAX_EXEC_ARGS + 1].join(",")),
            &format!("f({})", "x".repeat(MAX_EXEC_ARG_BYTES + 1)),
        ] {
            assert!(
                matches!(Verb::exec(line), Err(Error::Invalid(_))),
                "{line:?}: {:?}",
                Verb::exec(line)
            );
        }
        // Every argument within its cap, the line over its own: 8 quoted arguments of 99 bytes make 825.
        let arg = "a b".repeat(33);
        let long = exec("f", &[arg.as_str(); MAX_EXEC_ARGS]);
        assert!(matches!(
            encode_request(&request(long)),
            Err(Error::Invalid(_))
        ));
    }

    /// The scene verbs' caps: the runner design's 16 queries and 64 rays a tick.
    #[test]
    fn scene_verbs_refuse_past_their_caps() {
        let invalid = |verb: Verb| {
            let req = request(verb);
            assert!(
                matches!(encode_request(&req), Err(Error::Invalid(_))),
                "{req:?}"
            );
        };
        let frame = |radius: f32, max: u32, points: u32, at: Option<[f32; 3]>| Verb::SceneFrame {
            radius,
            max,
            points,
            at,
            entity_id: None,
        };
        for verb in [
            frame(0.0, 1, 0, None),
            frame(100.5, 1, 0, None),
            frame(f32::NAN, 1, 0, None),
            frame(10.0, 0, 0, None),
            frame(10.0, MAX_FRAME + 1, 0, None),
            frame(10.0, 1, 4, None),
            frame(10.0, 1, 0, Some([f32::INFINITY, 0.0, 0.0])),
            frame(10.0, 1, 0, Some([1e6, 0.0, 0.0])),
        ] {
            invalid(verb);
        }
        const { assert!(MAX_FRAME * 3 <= 64, "rays a tick") };
        let ground = |xyz: Vec<f32>, range: f32, radius: f32, search: f32| Verb::SceneGround {
            xyz,
            range,
            radius,
            search,
        };
        for verb in [
            ground(vec![], 10.0, 0.0, 0.0),
            ground(vec![1.0, 2.0], 10.0, 0.0, 0.0),
            ground(vec![0.0; 3 * (MAX_QUERIES + 1)], 10.0, 0.0, 0.0),
            ground(vec![0.0, 0.0, f32::NAN], 10.0, 0.0, 0.0),
            ground(vec![0.0; 3], 0.0, 0.0, 0.0),
            ground(vec![0.0; 3], 100.5, 0.0, 0.0),
            ground(vec![0.0; 3], 10.0, -0.1, 0.0),
            ground(vec![0.0; 3], 10.0, 10.5, 0.0),
            ground(vec![0.0; 3], 10.0, 0.5, 50.5),
        ] {
            invalid(verb);
        }
        for segments in [vec![], vec![0.0; 5], vec![0.0; 6 * (MAX_QUERIES + 1)]] {
            invalid(Verb::SceneRay {
                segments: segments.clone(),
                radius: 0.0,
            });
            invalid(Verb::NavLine {
                segments,
                width: 0.5,
            });
        }
        invalid(Verb::SceneRay {
            segments: vec![0.0; 6],
            radius: 2.5,
        });
        invalid(Verb::NavLine {
            segments: vec![0.0; 6],
            width: -1.0,
        });
        let full = Verb::SceneGround {
            xyz: vec![-99_999.99; 3 * MAX_QUERIES],
            range: 10.0,
            radius: 0.5,
            search: 50.0,
        };
        assert!(encode_request(&request(full)).is_ok());
    }

    /// A photo.grade of exposure and temperature only.
    fn photo_grade(exposure: Option<f32>, temperature: Option<f32>) -> Verb {
        Verb::PhotoGrade {
            exposure,
            contrast: None,
            highlights: None,
            saturation: None,
            temperature,
            vignette: None,
            grain: None,
            chromatic_aberration: None,
        }
    }

    /// The photo verbs' wire shapes, as W3Quen.asi's test sends them; the values photo.grade and photo.tick give.
    #[test]
    fn photo_verbs_are_flat_and_leave_out_what_is_not_given() {
        let line = |verb| encode_request(&request(verb)).unwrap();
        assert_eq!(
            line(photo_grade(Some(0.5), Some(6550.0))),
            "{\"id\":7,\"ttl_ms\":2000,\"verb\":\"photo.grade\",\"exposure\":0.5,\"temperature\":6550.0}\n"
        );
        assert_eq!(
            line(Verb::PhotoReset {
                part: PhotoPart::Weather
            }),
            "{\"id\":7,\"ttl_ms\":2000,\"verb\":\"photo.reset\",\"part\":\"weather\"}\n"
        );
        assert_eq!(
            line(Verb::PhotoList {
                what: PhotoListed::Weathers,
                filter: None
            }),
            "{\"id\":7,\"ttl_ms\":2000,\"verb\":\"photo.list\",\"what\":\"weathers\"}\n"
        );
        let mut want = vec![None; 8];
        want[0] = Some(0.5);
        want[4] = Some(6550.0);
        assert_eq!(photo_grade(Some(0.5), Some(6550.0)).photo_values(), want);
        assert_eq!(GRADING[4], "temperature");
        assert!(Verb::PhotoGet {}.photo_values().is_empty());
    }

    #[test]
    fn photo_verbs_refuse_what_the_game_must_not_get() {
        let invalid = |verb: Verb| {
            let req = request(verb);
            assert!(
                matches!(encode_request(&req), Err(Error::Invalid(_))),
                "{req:?}"
            );
            // serde_json writes a non-finite number as null, which doesn't decode at all.
            let line = serde_json::to_string(&req).unwrap();
            assert!(decode_request(&line).is_err(), "{line}");
        };
        invalid(Verb::PhotoWeather {
            name: String::new(),
        });
        invalid(Verb::PhotoEnv {
            path: "a\tb".into(),
        });
        for minute in [-0.5, 1440.0, f32::NAN, f32::INFINITY] {
            invalid(Verb::PhotoTime { minute });
        }
        invalid(photo_grade(None, None));
        invalid(photo_grade(Some(f32::NAN), None));
        for (tick_s, cloth_s) in [(None, None), (Some(-1.0), None), (None, Some(10.5))] {
            invalid(Verb::PhotoTick { tick_s, cloth_s });
        }
        for filter in [
            String::new(),
            "a\nb".into(),
            "x".repeat(MAX_FILTER_BYTES + 1),
        ] {
            invalid(Verb::PhotoList {
                what: PhotoListed::Envs,
                filter: Some(filter),
            });
        }
        // Finding 42: items.add's name and count.
        for (name, count) in [
            (String::new(), 1),
            ("a\tb".into(), 1),
            ("x".repeat(MAX_ITEM_BYTES + 1), 1),
            ("Wolf Armor 4".into(), 0),
            ("Wolf Armor 4".into(), MAX_ITEM_COUNT + 1),
        ] {
            invalid(Verb::ItemsAdd {
                name,
                count,
                equip: false,
            });
        }
        // Finding 51: world.actors' radius and max.
        for (radius, max) in [
            (0.0, 1),
            (-1.0, 1),
            (MAX_ACTORS_RADIUS + 0.5, 1),
            (f32::NAN, 1),
            (30.0, 0),
            (30.0, MAX_ACTORS + 1),
        ] {
            invalid(Verb::WorldActors {
                radius,
                max,
                tag: None,
                class: None,
            });
        }
        // Finding 83: the reason reads as one line, no run of spaces from a string continuation.
        let verb = Verb::WorldActors {
            radius: 0.0,
            max: 1,
            tag: None,
            class: None,
        };
        let why = encode_request(&Request {
            id: 1,
            ttl_ms: 1,
            verb,
        })
        .unwrap_err()
        .to_string();
        assert!(
            !why.contains("  ") && why.contains("a tag or class"),
            "{why}"
        );
        for name in ["", "a b", "x'y", &"n".repeat(65)] {
            let name = Some(name.to_string());
            for (tag, class) in [(name.clone(), None), (None, name)] {
                invalid(Verb::WorldActors {
                    radius: 8.0,
                    max: 1,
                    tag,
                    class,
                });
            }
        }
        for line in [
            r#"{"id":1,"ttl_ms":1,"verb":"photo.mode"}"#,
            r#"{"id":1,"ttl_ms":1,"verb":"photo.mode","on":1}"#,
            r#"{"id":1,"ttl_ms":1,"verb":"photo.get","x":0}"#,
            r#"{"id":1,"ttl_ms":1,"verb":"photo.grade","dof":1}"#,
            r#"{"id":1,"ttl_ms":1,"verb":"photo.reset","part":"camera"}"#,
            r#"{"id":1,"ttl_ms":1,"verb":"photo.reset"}"#,
            r#"{"id":1,"ttl_ms":1,"verb":"photo.list","what":"props"}"#,
            r#"{"id":1,"ttl_ms":1,"verb":"photo.time","minute":"noon"}"#,
        ] {
            assert!(
                matches!(decode_request(line), Err(Error::Malformed(_))),
                "{line}"
            );
        }
    }

    /// Two canonical keyframes.
    fn path_keys() -> Vec<Keyframe> {
        let key = |t: f64, x: f64, yaw: f64| Keyframe {
            t,
            pos: [x, 2.5, 3.0],
            rot: [-10.5, yaw, 0.0],
            fov: 60.0,
            ease: Ease::Linear,
        };
        vec![key(0.0, 1.0, 170.0), key(2.5, 4.25, -170.0)]
    }

    fn cam_path(mode: Mode, fps: Option<u32>, keys: Vec<Keyframe>) -> Request {
        request(Verb::CamPath {
            mode,
            fps,
            speed: Speed::Keys,
            ease: Ease::Linear,
            keys,
        })
    }

    #[test]
    fn every_verb_round_trips_as_one_line() {
        for verb in every_verb() {
            let req = request(verb);
            let line = encode_request(&req).unwrap();
            assert!(
                line.ends_with('\n') && line.matches('\n').count() == 1,
                "{line:?}"
            );
            assert_eq!(decode_request(&line).unwrap(), req);
        }
    }

    #[test]
    fn wire_shape_is_flat() {
        let line = encode_request(&request(Verb::Load { save: "s".into() })).unwrap();
        assert_eq!(
            line,
            "{\"id\":7,\"ttl_ms\":2000,\"verb\":\"load\",\"save\":\"s\"}\n"
        );
        let line = encode_request(&request(Verb::CamGet {})).unwrap();
        assert_eq!(line, "{\"id\":7,\"ttl_ms\":2000,\"verb\":\"cam.get\"}\n");
        let line = encode_request(&request(Verb::CamSet {
            pos: [1.0, 2.0, 3.0],
            rot: [0.0; 3],
            fov: None,
        }))
        .unwrap();
        assert_eq!(
            line,
            "{\"id\":7,\"ttl_ms\":2000,\"verb\":\"cam.set\",\"pos\":[1.0,2.0,3.0],\"rot\":[0.0,0.0,0.0]}\n"
        );
        // Defaults (keys speed, linear eases) are left out.
        let line = encode_request(&cam_path(Mode::Realtime, None, path_keys())).unwrap();
        assert_eq!(
            line,
            concat!(
                r#"{"id":7,"ttl_ms":2000,"verb":"cam.path","mode":"realtime","keys":["#,
                r#"{"t":0.0,"pos":[1.0,2.5,3.0],"rot":[-10.5,170.0,0.0],"fov":60.0},"#,
                r#"{"t":2.5,"pos":[4.25,2.5,3.0],"rot":[-10.5,-170.0,0.0],"fov":60.0}]}"#,
                "\n"
            )
        );
        let line = encode_request(&request(Verb::CamFrame { n: 12 })).unwrap();
        assert_eq!(
            line,
            "{\"id\":7,\"ttl_ms\":2000,\"verb\":\"cam.frame\",\"n\":12}\n"
        );
    }

    #[test]
    fn cam_path_refuses_what_the_game_must_not_play() {
        let invalid = |req: Request, needle: &str| {
            for result in [
                encode_request(&req).map(|_| ()),
                decode_request(&serde_json::to_string(&req).unwrap()).map(|_| ()),
            ] {
                match result {
                    Err(Error::Invalid(why)) => {
                        assert!(why.contains(needle), "{why:?} lacks {needle:?}")
                    }
                    other => panic!("expected an invalid cam.path about {needle:?}, got {other:?}"),
                }
            }
        };
        invalid(
            cam_path(Mode::Step, None, path_keys()),
            "step with fps None",
        );
        invalid(
            cam_path(Mode::Step, Some(0), path_keys()),
            "step with fps Some(0)",
        );
        invalid(cam_path(Mode::Step, Some(241), path_keys()), "1 to 240");
        invalid(
            cam_path(Mode::Realtime, Some(30), path_keys()),
            "realtime with fps",
        );
        invalid(
            cam_path(Mode::Realtime, None, path_keys()[..1].to_vec()),
            "1 keyframes",
        );
        let mut keys = path_keys();
        keys[1].rot[1] = 190.0;
        invalid(cam_path(Mode::Realtime, None, keys), "not canonical");
        let mut keys = path_keys();
        keys[0].pos[0] = 1.00001;
        invalid(cam_path(Mode::Realtime, None, keys), "not canonical");
        let mut keys = path_keys();
        keys[1].rot[0] = 90.0;
        invalid(cam_path(Mode::Realtime, None, keys), "pitch 90");

        let ok = encode_request(&cam_path(Mode::Realtime, None, path_keys())).unwrap();
        for bad in [
            ok.replace("\"realtime\"", "\"slowmo\""),
            ok.replace("\"fov\":60.0}", "\"fov\":60.0,\"roll\":0}"),
            ok.replace("\"keys\"", "\"frames\""),
            ok.replace("\"mode\":\"realtime\",", ""),
        ] {
            assert!(
                matches!(decode_request(&bad), Err(Error::Malformed(_))),
                "{bad}"
            );
        }
    }

    #[test]
    fn the_longest_cam_path_fits_a_request() {
        // Every number at its longest canonical spelling; eases on all keyframes but the last, as keys speed allows.
        let keys: Vec<Keyframe> = (0..sweep::MAX_KEYFRAMES)
            .map(|i| Keyframe {
                t: if i == 0 {
                    0.0
                } else {
                    3599.9976 + 0.0001 * i as f64
                },
                pos: [-99999.9999; 3],
                rot: [-88.9999, -179.9999, -179.9999],
                fov: 178.9999,
                ease: if i + 1 < sweep::MAX_KEYFRAMES {
                    Ease::EaseInOut
                } else {
                    Ease::Linear
                },
            })
            .collect();
        let keys = Path::new(&keys, Speed::Keys, Ease::Linear)
            .unwrap()
            .keys()
            .to_vec();
        let longest = Request {
            id: u64::MAX,
            ttl_ms: u32::MAX,
            verb: Verb::CamPath {
                mode: Mode::Step,
                fps: Some(sweep::MAX_FPS),
                speed: Speed::Keys,
                ease: Ease::Linear,
                keys,
            },
        };
        let line = encode_request(&longest).unwrap();
        assert_eq!(decode_request(&line).unwrap(), longest);
        assert_eq!(
            line.len() - 1,
            3261,
            "the sweep::MAX_KEYFRAMES doc quotes this"
        );
    }

    #[test]
    fn decodes_what_another_end_writes() {
        let req = decode_request(r#"{"verb":"cam.set","rot":[0,0,90],"pos":[1,2,3],"ttl_ms":500,"id":18446744073709551615}"#)
            .unwrap();
        assert_eq!(req.id, u64::MAX);
        assert_eq!(
            req.verb,
            Verb::CamSet {
                pos: [1.0, 2.0, 3.0],
                rot: [0.0, 0.0, 90.0],
                fov: None
            }
        );
        let req =
            decode_request("{\"id\":1,\"ttl_ms\":0,\"verb\":\"pause\",\"on\":false}\r\n").unwrap();
        assert_eq!(req.verb, Verb::Pause { on: false });
    }

    #[test]
    fn refuses_unknown_verbs_fields_and_shapes() {
        for line in [
            r#"{"id":1,"ttl_ms":1,"verb":"console","line":"quit"}"#,
            r#"{"id":1,"ttl_ms":1,"verb":"exec","line":"quit"}"#,
            r#"{"id":1,"ttl_ms":1,"verb":"LOAD","save":"s"}"#,
            r#"{"id":1,"ttl_ms":1,"verb":"load","save":"s","extra":1}"#,
            r#"{"id":1,"ttl_ms":1,"verb":"cam.set","pos":[0,0,0],"rot":[0,0,0],"FOV":40}"#,
            r#"{"id":1,"ttl_ms":1,"verb":"loadlast","save":"s"}"#,
            r#"{"id":1,"ttl_ms":1,"verb":"save","slot":0}"#,
            r#"{"id":1,"ttl_ms":1,"verb":"save","save":"ManualSave_1"}"#,
            r#"{"id":1,"ttl_ms":1,"verb":"world.get","x":0}"#,
            r#"{"id":1,"ttl_ms":1,"verb":"cam.set","pos":[0,0],"rot":[0,0,0]}"#,
            r#"{"id":1,"ttl_ms":1,"verb":"load"}"#,
            r#"{"id":1,"verb":"loadlast"}"#,
            r#"{"id":-1,"ttl_ms":1,"verb":"loadlast"}"#,
            r#"{"id":1,"ttl_ms":4294967296,"verb":"loadlast"}"#,
            r#"{"id":1,"ttl_ms":1}"#,
            r#"[1,2]"#,
            "{\"id\":1,\n\"ttl_ms\":1,\"verb\":\"loadlast\"}",
            "",
        ] {
            assert!(
                matches!(decode_request(line), Err(Error::Malformed(_))),
                "{line:?}"
            );
        }
        let err = decode_request(r#"{"id":1,"ttl_ms":1,"verb":"console"}"#)
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown variant `console`"), "{err}");
    }

    #[test]
    fn refuses_non_finite_and_out_of_range_values() {
        let nan = request(Verb::CamSet {
            pos: [f32::NAN, 0.0, 0.0],
            rot: [0.0; 3],
            fov: None,
        });
        assert!(matches!(encode_request(&nan), Err(Error::Invalid(_))));
        let inf_fov = request(Verb::CamSet {
            pos: [0.0; 3],
            rot: [0.0; 3],
            fov: Some(f32::INFINITY),
        });
        assert!(matches!(encode_request(&inf_fov), Err(Error::Invalid(_))));
        let inf = request(Verb::Teleport {
            pos: [0.0; 3],
            rot: [0.0, f32::NEG_INFINITY, 0.0],
        });
        assert!(matches!(encode_request(&inf), Err(Error::Invalid(_))));
        // 1e39 is a finite f64 but overflows f32 to infinity.
        let line = r#"{"id":1,"ttl_ms":1,"verb":"teleport","pos":[1e39,0,0],"rot":[0,0,0]}"#;
        assert!(matches!(decode_request(line), Err(Error::Invalid(_))));
        let line = r#"{"id":1,"ttl_ms":1,"verb":"settime","day":1,"hour":24}"#;
        assert!(matches!(decode_request(line), Err(Error::Invalid(_))));
        assert!(matches!(
            encode_request(&request(Verb::Weather {
                name: String::new()
            })),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            encode_request(&request(Verb::Load {
                save: String::new()
            })),
            Err(Error::Invalid(_))
        ));
        let notice = |text: String| encode_request(&request(Verb::Notice { text }));
        assert!(notice("é".repeat(MAX_NOTICE_BYTES / 2)).is_ok());
        for text in [
            String::new(),
            "x".repeat(MAX_NOTICE_BYTES + 1),
            "a\nb".into(),
            "\t".into(),
        ] {
            assert!(
                matches!(notice(text.clone()), Err(Error::Invalid(_))),
                "{text:?}"
            );
        }
    }

    #[test]
    fn caps_line_length_per_direction() {
        let at_cap = |extra: usize| {
            let empty = encode_request(&request(Verb::Load { save: "x".into() }))
                .unwrap()
                .len()
                - 1;
            request(Verb::Load {
                save: "x".repeat(MAX_REQUEST_BYTES - empty + 1 + extra),
            })
        };
        let line = encode_request(&at_cap(0)).unwrap();
        assert_eq!(line.len(), MAX_REQUEST_BYTES + 1);
        assert_eq!(decode_request(&line).unwrap(), at_cap(0));
        assert_eq!(
            encode_request(&at_cap(1)),
            Err(Error::TooLong {
                len: MAX_REQUEST_BYTES + 1,
                max: MAX_REQUEST_BYTES
            })
        );
        let long = format!(
            "{{\"id\":1,\"ttl_ms\":1,\"verb\":\"load\",\"save\":\"{}\"}}",
            "x".repeat(MAX_REQUEST_BYTES)
        );
        assert!(matches!(decode_request(&long), Err(Error::TooLong { .. })));

        let reply = Reply {
            id: 1,
            ok: true,
            text: "x".repeat(MAX_REQUEST_BYTES * 2),
            data: None,
        };
        assert_eq!(decode_reply(&encode_reply(&reply).unwrap()).unwrap(), reply);
        let reply = Reply {
            id: 1,
            ok: true,
            text: "x".repeat(MAX_REPLY_BYTES),
            data: None,
        };
        assert!(matches!(encode_reply(&reply), Err(Error::TooLong { .. })));
    }

    #[test]
    fn replies_round_trip() {
        let saves = Reply {
            id: 9,
            ok: true,
            text: "2 saves".into(),
            data: Some(json!(["save one", "save two"])),
        };
        let pose = Reply {
            id: 10,
            ok: true,
            text: String::new(),
            data: Some(json!({"pos": [1.5, 2.0, 3.0], "rot": [0.0, 0.0, 90.0], "fov": 60.0})),
        };
        let refused = Reply {
            id: 11,
            ok: false,
            text: "no save matches".into(),
            data: None,
        };
        for reply in [saves, pose, refused] {
            let line = encode_reply(&reply).unwrap();
            assert!(
                line.ends_with('\n') && line.matches('\n').count() == 1,
                "{line:?}"
            );
            assert_eq!(decode_reply(&line).unwrap(), reply);
        }
        assert_eq!(
            encode_reply(&Reply {
                id: 11,
                ok: false,
                text: "no".into(),
                data: None
            })
            .unwrap(),
            "{\"id\":11,\"ok\":false,\"text\":\"no\"}\n"
        );
        assert!(matches!(
            decode_reply(r#"{"id":1,"ok":true,"text":"","extra":0}"#),
            Err(Error::Malformed(_))
        ));
        assert!(matches!(
            decode_reply(r#"{"id":1,"ok":true}"#),
            Err(Error::Malformed(_))
        ));
    }
}
