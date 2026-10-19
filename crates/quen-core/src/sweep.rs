//! Camera sweeps for cinematics (docs/intent.md, "Camera sweeps"): the data model, the reference interpolation that the
//! in-game `cam.path` verb ports, and the playback schedules. Game-neutral: a pose is a position in metres, a rotation
//! `[pitch, yaw, roll]` in degrees (the Witcher 3 scripts' `EulerAngles` order, as `cam.get` reads it back; yaw turns
//! about world up, pitch is the elevation above the horizon) and a field of view in degrees.
//!
//! The maths, chosen so a game script can port it in single precision and get the same poses:
//! - **position:** centripetal Catmull-Rom (no cusps or loops within a segment) as one cubic Hermite per segment;
//! - **direction:** the zero-roll orientation (yaw, then pitch) as a quaternion, through a squad spline that takes the
//!   short way round; yaw and pitch are read back from its forward vector, so the horizon stays level. A plain slerp of
//!   the whole rotation tilts it 4 to 15° on ordinary diagonal moves;
//! - **roll and fov:** channels of their own, cubic Hermite;
//! - **tangents:** at a keyframe where a channel holds still or turns back, its tangent is zero, so a hold stays still
//!   and roll and fov never overshoot a keyframe;
//! - **timing:** each segment eased at the keyframes' times, or the whole path at constant speed by arc length.

use std::array;
use std::fmt;
use std::iter;
use std::ops::RangeInclusive;

use serde::{Deserialize, Serialize};

/// The most keyframes in a sweep, so one `cam.path` request carries any sweep: the longest possible one (every number
/// at its longest canonical spelling, about 132 bytes a keyframe) takes 3,261 of the channel's 4,096 bytes
/// (`protocol::MAX_REQUEST_BYTES`; tested there). 30 would fit; the rest is headroom.
pub const MAX_KEYFRAMES: usize = 24;
/// The longest sweep, in seconds.
pub const MAX_DURATION_S: f64 = 3600.0;
/// Every position coordinate stays below this in magnitude, in metres.
pub const MAX_COORD: f64 = 100_000.0;
/// Keyframe pitch stays within ±this, in degrees: straight up or down leaves the heading undefined.
pub const MAX_PITCH: f64 = 89.0;
/// The fields of view a keyframe may have, in degrees.
pub const FOV: RangeInclusive<f64> = 1.0..=179.0;
/// The most frames per second of frame-step playback.
pub const MAX_FPS: u32 = 240;
/// Constant speed counts a radian of turn, roll or fov change as this many metres of travel.
pub const METRES_PER_RADIAN: f64 = 1.0;
/// Samples per segment of the constant-speed arc-length table.
pub const ARC_SAMPLES: usize = 32;
/// Keyframes closer than this, in metres, are at the same position.
const SAME_POS_M: f64 = 1e-3;
/// The most a segment may turn, in degrees: near 180° the short way round is ambiguous.
const MAX_TURN_DEG: f64 = 179.0;
/// Above this dot product, slerp is a normalised lerp.
const NLERP_DOT: f64 = 0.9995;

/// A camera pose, as `cam.get` reads it back.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pose {
    pub pos: [f64; 3],
    /// Pitch, yaw, roll.
    pub rot: [f64; 3],
    pub fov: f64,
}

/// How progress through a stretch of the path follows time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ease {
    #[default]
    Linear,
    /// Smoothstep, 3s² − 2s³: starts and ends at rest.
    EaseInOut,
}

/// A pose, the time it is reached, and how the segment leaving it is eased.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Keyframe {
    /// Seconds from the start; the first keyframe is at 0.
    pub t: f64,
    pub pos: [f64; 3],
    /// Pitch, yaw, roll.
    pub rot: [f64; 3],
    pub fov: f64,
    /// Shapes the segment from this keyframe to the next, under [`Speed::Keys`]; linear when left out.
    #[serde(default, skip_serializing_if = "is_default")]
    pub ease: Ease,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Speed {
    /// Each keyframe is reached at its time.
    #[default]
    Keys,
    /// The whole path at one speed by arc length, from 0 to the last keyframe's time, eased by the sweep's `ease`.
    /// Inner keyframes' times are not used, and a hold takes no time.
    Constant,
}

/// How the game plays a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// The game samples the path at its local time on every tick.
    Realtime,
    /// The world is paused and the game sets frame n (time n / fps) when `cam.frame` asks, so quen captures each.
    Step,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Realtime => "realtime",
            Mode::Step => "step",
        }
    }
}

/// The world a sweep plays in, set before the path (`load`, `settime`, `weather`, `teleport`, `pause`). What is left
/// out stays as it is.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct World {
    /// The save to load first, matched as the `load` verb matches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub save: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<GameTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weather: Option<String>,
    /// Where the player stands: photo mode keeps its camera near the player. A teleport taints later saves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub player: Option<Player>,
    /// Hold the world still during real-time playback. Frame step always does.
    #[serde(default, skip_serializing_if = "is_default")]
    pub paused: bool,
}

/// A game time, as the `settime` verb takes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GameTime {
    pub day: u32,
    pub hour: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Player {
    pub pos: [f64; 3],
    pub yaw: f64,
}

/// A camera sweep: keyframes, timing, and the world to play it in. From [`Sweep::from_json`] or [`Sweep::canonical`]
/// it is canonical and checked: every angle in (−180, 180], every number on a 0.0001 grid (4 decimals, as `cam.get`
/// reads poses back), so the JSON, the channel message and the game's numbers are the same.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sweep {
    pub keyframes: Vec<Keyframe>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub speed: Speed,
    /// Eases the whole path under [`Speed::Constant`].
    #[serde(default, skip_serializing_if = "is_default")]
    pub ease: Ease,
    #[serde(default, skip_serializing_if = "is_default")]
    pub world: World,
}

/// Why a sweep, path or playback is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SweepError(pub String);

impl fmt::Display for SweepError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SweepError {}

fn refuse<T>(msg: String) -> Result<T, SweepError> {
    Err(SweepError(msg))
}

pub(crate) fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    *value == T::default()
}

impl Sweep {
    /// A sweep from its JSON, canonical and checked. Unknown fields are refused.
    pub fn from_json(text: &str) -> Result<Sweep, SweepError> {
        let sweep: Sweep = serde_json::from_str(text).map_err(|e| SweepError(e.to_string()))?;
        sweep.canonical()
    }

    /// The JSON the ledger stores and hashes.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// This sweep made canonical, after the checks of [`Path::new`] and the world's.
    pub fn canonical(self) -> Result<Sweep, SweepError> {
        let path = Path::new(&self.keyframes, self.speed, self.ease)?;
        Ok(Sweep {
            keyframes: path.keys,
            world: self.world.canonical()?,
            ..self
        })
    }

    pub fn path(&self) -> Result<Path, SweepError> {
        Path::new(&self.keyframes, self.speed, self.ease)
    }

    /// A room pan from `centre`: `turn` degrees of yaw (±360 for a full turn; positive toward increasing yaw) through
    /// `keyframes` evenly spaced keyframes, the first and last included, over `duration` seconds at constant speed.
    /// Each step turns `turn / (keyframes − 1)`, which must stay below 179°: a full turn needs at least 4 keyframes.
    pub fn room_pan(
        centre: &Pose,
        turn: f64,
        keyframes: usize,
        duration: f64,
        ease: Ease,
    ) -> Result<Sweep, SweepError> {
        let steps = keyframes.saturating_sub(1).max(1) as f64;
        let [pitch, yaw, roll] = centre.rot;
        let keyframes = (0..keyframes)
            .map(|k| {
                let f = k as f64 / steps;
                Keyframe {
                    t: duration * f,
                    pos: centre.pos,
                    rot: [pitch, yaw + turn * f, roll],
                    fov: centre.fov,
                    ease: Ease::Linear,
                }
            })
            .collect();
        Sweep {
            keyframes,
            speed: Speed::Constant,
            ease,
            world: World::default(),
        }
        .canonical()
    }
}

impl World {
    fn canonical(self) -> Result<World, SweepError> {
        for (what, name) in [("save", &self.save), ("weather", &self.weather)] {
            if let Some(name) = name
                && (name.is_empty() || name.chars().any(char::is_control))
            {
                return refuse(format!(
                    "world {what} {name:?}: empty, or with a control character"
                ));
            }
        }
        if let Some(GameTime { day, hour }) = self.time
            && (hour > 23 || day > i32::MAX as u32)
        {
            return refuse(format!(
                "world time day {day} hour {hour}: the hour runs 0 to 23, the day to 2^31 - 1"
            ));
        }
        let player = self.player.map(|p| Player {
            pos: p.pos.map(grid),
            yaw: angle(p.yaw),
        });
        if let Some(p) = &player
            && !(p.yaw.is_finite() && p.pos.iter().all(|x| x.abs() < MAX_COORD))
        {
            return refuse(format!(
                "world player {:?} yaw {}: positions stay below {MAX_COORD} m",
                p.pos, p.yaw
            ));
        }
        Ok(World { player, ..self })
    }
}

/// `x` rounded to the 0.0001 grid. Dividing the integer by 10,000 gives the double nearest the 4-decimal number, so it
/// prints as one; adding 0 turns −0 into 0.
fn grid(x: f64) -> f64 {
    (x * 10_000.0).round() / 10_000.0 + 0.0
}

/// `x` in [−180, 180].
fn wrap180(x: f64) -> f64 {
    x - 360.0 * (x / 360.0).round()
}

/// An angle in (−180, 180], on the grid.
fn angle(x: f64) -> f64 {
    let a = grid(wrap180(x));
    if a == -180.0 { 180.0 } else { a }
}

impl Keyframe {
    fn canonical(&self) -> Keyframe {
        Keyframe {
            t: grid(self.t),
            pos: self.pos.map(grid),
            rot: self.rot.map(angle),
            fov: grid(self.fov),
            ease: self.ease,
        }
    }
}

impl Ease {
    fn apply(self, s: f64) -> f64 {
        match self {
            Ease::Linear => s,
            Ease::EaseInOut => s * s * (3.0 - 2.0 * s),
        }
    }
}

/// Refuses a playback the game can't run: frame step takes 1 to [`MAX_FPS`] frames per second, real time none.
pub fn check_playback(mode: Mode, fps: Option<u32>) -> Result<(), SweepError> {
    match (mode, fps) {
        (Mode::Realtime, None) => Ok(()),
        (Mode::Step, Some(fps)) if (1..=MAX_FPS).contains(&fps) => Ok(()),
        _ => refuse(format!(
            "{} with fps {fps:?}: frame step takes 1 to {MAX_FPS} frames per second, real time none",
            mode.as_str()
        )),
    }
}

/// A checked path, ready to sample: canonical keyframes and each channel's tangents. Every per-keyframe vector has one
/// entry per keyframe; segment `i` runs from keyframe `i` to `i + 1`, its parameter `u` from 0 to 1.
#[derive(Debug, Clone)]
pub struct Path {
    keys: Vec<Keyframe>,
    speed: Speed,
    ease: Ease,
    /// Per segment: the position tangents at its start and end.
    pos_tan: Vec<[[f64; 3]; 2]>,
    /// Per keyframe: the zero-roll orientation, signs aligned so each segment takes the short way, and squad's control
    /// points into and out of it.
    quat: Vec<Quat>,
    s_in: Vec<Quat>,
    s_out: Vec<Quat>,
    /// Per keyframe: yaw and roll unwrapped along the path (each step the short way), and the roll and fov tangents.
    yaw: Vec<f64>,
    roll: Vec<f64>,
    roll_tan: Vec<f64>,
    fov_tan: Vec<f64>,
    /// Constant speed: the arc length at every 1 / [`ARC_SAMPLES`] of the path parameter, from 0.
    arc: Vec<f64>,
}

impl Path {
    /// Checks the keyframes and precomputes the path. The keyframes are made canonical first ([`Path::keys`]).
    pub fn new(keys: &[Keyframe], speed: Speed, ease: Ease) -> Result<Path, SweepError> {
        let n = keys.len();
        if !(2..=MAX_KEYFRAMES).contains(&n) {
            return refuse(format!("{n} keyframes: a sweep has 2 to {MAX_KEYFRAMES}"));
        }
        let keys: Vec<Keyframe> = keys.iter().map(Keyframe::canonical).collect();
        for (i, k) in keys.iter().enumerate() {
            let bad = |what: String| refuse(format!("keyframe {i}: {what}"));
            let mut numbers = [k.t, k.fov].into_iter().chain(k.pos).chain(k.rot);
            if !numbers.all(f64::is_finite) {
                return bad("a number that isn't finite".into());
            }
            if i == 0 && k.t != 0.0 {
                return bad(format!("t {}: the first keyframe is at 0", k.t));
            }
            if i > 0 && k.t <= keys[i - 1].t {
                return bad(format!(
                    "t {} is not after the previous keyframe's {}",
                    k.t,
                    keys[i - 1].t
                ));
            }
            if k.t > MAX_DURATION_S {
                return bad(format!(
                    "t {}: a sweep lasts at most {MAX_DURATION_S} s",
                    k.t
                ));
            }
            if k.pos.iter().any(|x| x.abs() >= MAX_COORD) {
                return bad(format!(
                    "position {:?}: coordinates stay below {MAX_COORD} m",
                    k.pos
                ));
            }
            if k.rot[0].abs() > MAX_PITCH {
                return bad(format!(
                    "pitch {}: beyond ±{MAX_PITCH}°, straight up or down leaves the heading undefined",
                    k.rot[0]
                ));
            }
            if !FOV.contains(&k.fov) {
                return bad(format!(
                    "fov {}: outside {} to {}°",
                    k.fov,
                    FOV.start(),
                    FOV.end()
                ));
            }
            if k.ease != Ease::Linear && i == n - 1 {
                return bad("an ease on the last keyframe: a keyframe's ease shapes the segment that leaves it".into());
            }
            if k.ease != Ease::Linear && speed == Speed::Constant {
                return bad("an ease under constant speed: set the sweep's ease instead".into());
            }
        }
        if ease != Ease::Linear && speed == Speed::Keys {
            return refuse("the sweep's ease applies to constant speed only: under keys, each keyframe's ease shapes its segment".into());
        }

        let mut quat: Vec<Quat> = keys
            .iter()
            .map(|k| Quat::look(k.rot[1], k.rot[0]))
            .collect();
        for i in 1..n {
            if quat[i - 1].dot(quat[i]) < 0.0 {
                quat[i] = quat[i].neg();
            }
            if quat[i - 1].dot(quat[i]) < (MAX_TURN_DEG / 2.0).to_radians().cos() {
                return refuse(format!(
                    "keyframes {} to {} turn {MAX_TURN_DEG}° or more, where the short way round is ambiguous: add a keyframe between",
                    i - 1,
                    i
                ));
            }
        }

        // Each channel's change into keyframe i (a) and out of it (b). A missing neighbour at either end mirrors the
        // one there, so a path starts and ends heading straight for its neighbour keyframe.
        let into = |i: usize| i.max(1) - 1;
        let out = |i: usize| i.min(n - 2);
        let chord: Vec<[f64; 3]> = (0..n - 1)
            .map(|i| sub(keys[i + 1].pos, keys[i].pos))
            .collect();
        let m: Vec<[f64; 3]> = (0..n)
            .map(|i| centripetal(chord[into(i)], chord[out(i)]))
            .collect();
        let pos_tan = (0..n - 1)
            .map(|i| {
                let d = norm(&chord[i]).sqrt();
                [m[i].map(|x| x * d), m[i + 1].map(|x| x * d)]
            })
            .collect();

        // In keyframe i's frame: the half-angle rotation to the next keyframe, and from the previous one.
        let ahead = |i: usize| quat[i].conj().mul(quat[i + 1]).log();
        let behind = |i: usize| quat[i].conj().mul(quat[i - 1]).log().map(|x| -x);
        let (s_in, s_out) = quat
            .iter()
            .enumerate()
            .map(|(i, q)| {
                let b = if i < n - 1 { ahead(i) } else { behind(i) };
                let a = if i > 0 { behind(i) } else { b };
                // Squad's control points that give the tangent `tan` on both sides of the keyframe.
                let tan = harmonic(a, b);
                (
                    q.mul(Quat::exp(array::from_fn(|c| (a[c] - tan[c]) / 2.0))),
                    q.mul(Quat::exp(array::from_fn(|c| (tan[c] - b[c]) / 2.0))),
                )
            })
            .unzip();

        let unwrap = |c: usize| {
            let mut v = vec![keys[0].rot[c]];
            for k in &keys[1..] {
                let last = v[v.len() - 1];
                v.push(last + wrap180(k.rot[c] - last));
            }
            v
        };
        let tangents = |v: &[f64]| -> Vec<f64> {
            (0..n)
                .map(|i| {
                    let a = v[into(i) + 1] - v[into(i)];
                    let b = v[out(i) + 1] - v[out(i)];
                    harmonic([a], [b])[0]
                })
                .collect()
        };
        let roll = unwrap(2);
        let fovs: Vec<f64> = keys.iter().map(|k| k.fov).collect();
        let mut path = Path {
            roll_tan: tangents(&roll),
            fov_tan: tangents(&fovs),
            yaw: unwrap(1),
            roll,
            keys,
            speed,
            ease,
            pos_tan,
            quat,
            s_in,
            s_out,
            arc: Vec::new(),
        };
        if speed == Speed::Constant {
            let samples: Vec<(Pose, Quat)> = (0..=(n - 1) * ARC_SAMPLES)
                .map(|j| path.at(j as f64 / ARC_SAMPLES as f64))
                .collect();
            let mut s = 0.0;
            path.arc = iter::once(0.0)
                .chain(samples.windows(2).map(|w| {
                    s += step(&w[0], &w[1]);
                    s
                }))
                .collect();
        }
        Ok(path)
    }

    /// The canonical keyframes: what the `cam.path` message carries.
    pub fn keys(&self) -> &[Keyframe] {
        &self.keys
    }

    /// The last keyframe's time, in seconds.
    pub fn duration(&self) -> f64 {
        self.keys[self.keys.len() - 1].t
    }

    /// The pose at `t` seconds from the start. Real-time playback samples this on every tick, with `t` from the local
    /// time since its first tick. Before 0 (or NaN) it is the first keyframe, after the end the last.
    pub fn sample(&self, t: f64) -> Pose {
        self.at(self.sigma(t)).0
    }

    /// Frame step at `fps`: frame n shows `n / fps` seconds, from 0 to the last frame at or before the end (both ends
    /// when the duration is a whole number of frames).
    pub fn frame_count(&self, fps: u32) -> Result<u32, SweepError> {
        check_playback(Mode::Step, Some(fps))?;
        // The duration is on the 0.0001 grid, so a fraction of a frame left over is at least 1e-4 of one.
        Ok((self.duration() * f64::from(fps) + 1e-6).floor() as u32 + 1)
    }

    /// Every frame's time at `fps`, computed as n / fps (never summed), so the same on every run.
    pub fn frame_times(&self, fps: u32) -> Result<Vec<f64>, SweepError> {
        let frames = self.frame_count(fps)?;
        Ok((0..frames)
            .map(|n| (f64::from(n) / f64::from(fps)).min(self.duration()))
            .collect())
    }

    /// The path parameter at time `t`: segment index plus the segment's `u`.
    fn sigma(&self, t: f64) -> f64 {
        let end = self.duration();
        let t = t.max(0.0).min(end);
        match self.speed {
            Speed::Keys => {
                let n = self.keys.len();
                let i = self.keys[1..n - 1].iter().take_while(|k| k.t <= t).count();
                let (k0, k1) = (&self.keys[i], &self.keys[i + 1]);
                i as f64 + k0.ease.apply((t - k0.t) / (k1.t - k0.t))
            }
            Speed::Constant => {
                // The first table step that reaches the target length, then linear within it.
                let arc = &self.arc;
                let target = self.ease.apply(t / end) * arc[arc.len() - 1];
                let j = arc[1..].partition_point(|&s| s < target).min(arc.len() - 2) + 1;
                let span = arc[j] - arc[j - 1];
                let frac = if span > 0.0 {
                    ((target - arc[j - 1]) / span).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                ((j - 1) as f64 + frac) / ARC_SAMPLES as f64
            }
        }
    }

    /// The pose at path parameter `sigma`, and squad's quaternion there.
    fn at(&self, sigma: f64) -> (Pose, Quat) {
        let i = (sigma.floor() as usize).min(self.keys.len() - 2);
        let u = sigma - i as f64;
        let (k0, k1) = (&self.keys[i], &self.keys[i + 1]);
        let [m0, m1] = self.pos_tan[i];
        let pos = array::from_fn(|c| hermite(k0.pos[c], k1.pos[c], m0[c], m1[c], u));
        let q = self.quat[i].slerp(self.quat[i + 1], u).slerp(
            self.s_out[i].slerp(self.s_in[i + 1], u),
            2.0 * u * (1.0 - u),
        );
        let f = q.forward();
        // Yaw unwrapped against a reference that runs straight between the keyframes' unwrapped yaws.
        let reference = self.yaw[i] + u * (self.yaw[i + 1] - self.yaw[i]);
        let yaw = reference + wrap180((-f[0]).atan2(f[1]).to_degrees() - reference);
        let pitch = f[2].atan2(f[0].hypot(f[1])).to_degrees();
        let roll = hermite(
            self.roll[i],
            self.roll[i + 1],
            self.roll_tan[i],
            self.roll_tan[i + 1],
            u,
        );
        let fov = hermite(k0.fov, k1.fov, self.fov_tan[i], self.fov_tan[i + 1], u);
        let pose = Pose {
            pos,
            rot: [pitch, yaw, roll],
            fov,
        };
        (pose, q)
    }
}

/// How far the camera moves between two samples: travel, and turn, roll and fov change at [`METRES_PER_RADIAN`]. The
/// turn is twice the quaternions' chord, the rotation angle for short steps.
fn step((a, qa): &(Pose, Quat), (b, qb): &(Pose, Quat)) -> f64 {
    let travel = norm(&sub(b.pos, a.pos));
    let turn = 2.0 * norm(&array::from_fn::<f64, 4, _>(|c| qb.0[c] - qa.0[c]));
    let roll = (b.rot[2] - a.rot[2]).to_radians();
    let fov = (b.fov - a.fov).to_radians();
    (travel * travel + METRES_PER_RADIAN.powi(2) * (turn * turn + roll * roll + fov * fov)).sqrt()
}

/// The cubic Hermite from `p0` with tangent `m0` at u = 0 to `p1` with tangent `m1` at u = 1. Exact at both ends.
fn hermite(p0: f64, p1: f64, m0: f64, m1: f64, u: f64) -> f64 {
    let (u2, u3) = (u * u, u * u * u);
    (2.0 * u3 - 3.0 * u2 + 1.0) * p0
        + (u3 - 2.0 * u2 + u) * m0
        + (3.0 * u2 - 2.0 * u3) * p1
        + (u3 - u2) * m1
}

/// The tangent at a keyframe from the change into it (`a`) and out of it (`b`): (|b|·a + |a|·b) / (|a| + |b|), their
/// harmonic mean along the bisector. Zero when either is zero (a hold) or they point opposite ways (a turn back), and
/// `a` when both are equal.
fn harmonic<const N: usize>(a: [f64; N], b: [f64; N]) -> [f64; N] {
    let (la, lb) = (norm(&a), norm(&b));
    if la + lb == 0.0 {
        return [0.0; N];
    }
    array::from_fn(|c| (lb * a[c] + la * b[c]) / (la + lb))
}

/// The centripetal Catmull-Rom tangent at a keyframe, per unit of the knot parameter (chord length ^ 0.5), from the
/// chords into it (`a`) and out of it (`b`). Zero next to a hold.
fn centripetal(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    let (la, lb) = (norm(&a), norm(&b));
    if la < SAME_POS_M || lb < SAME_POS_M {
        return [0.0; 3];
    }
    let (d0, d1) = (la.sqrt(), lb.sqrt());
    let (wa, wb) = (d1 / (d0 * (d0 + d1)), d0 / (d1 * (d0 + d1)));
    array::from_fn(|c| a[c] * wa + b[c] * wb)
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    array::from_fn(|c| a[c] - b[c])
}

fn norm(v: &[f64]) -> f64 {
    v.iter().map(|x| x * x).sum::<f64>().sqrt()
}

/// A quaternion (w, x, y, z), unit length wherever it is used. The camera looks along +y with +z up.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Quat([f64; 4]);

impl Quat {
    /// The zero-roll orientation: yaw about world up, then pitch about the camera's right axis.
    fn look(yaw: f64, pitch: f64) -> Quat {
        let (sy, cy) = (yaw.to_radians() / 2.0).sin_cos();
        let (sp, cp) = (pitch.to_radians() / 2.0).sin_cos();
        Quat([cy * cp, cy * sp, sy * sp, sy * cp])
    }

    fn mul(self, o: Quat) -> Quat {
        let [a, b, c, d] = self.0;
        let [e, f, g, h] = o.0;
        Quat([
            a * e - b * f - c * g - d * h,
            a * f + b * e + c * h - d * g,
            a * g - b * h + c * e + d * f,
            a * h + b * g - c * f + d * e,
        ])
    }

    fn conj(self) -> Quat {
        let [w, x, y, z] = self.0;
        Quat([w, -x, -y, -z])
    }

    fn neg(self) -> Quat {
        Quat(self.0.map(|x| -x))
    }

    fn dot(self, o: Quat) -> f64 {
        (0..4).map(|c| self.0[c] * o.0[c]).sum()
    }

    /// The half-angle rotation vector (angle / 2 · axis).
    fn log(self) -> [f64; 3] {
        let [w, x, y, z] = self.0;
        let s = norm(&[x, y, z]);
        if s < 1e-12 {
            return [x, y, z];
        }
        let k = s.atan2(w) / s;
        [x * k, y * k, z * k]
    }

    fn exp(v: [f64; 3]) -> Quat {
        let a = norm(&v);
        if a < 1e-12 {
            return Quat([1.0, v[0], v[1], v[2]]);
        }
        let k = a.sin() / a;
        Quat([a.cos(), v[0] * k, v[1] * k, v[2] * k])
    }

    /// Along the great arc from `self` (u = 0) to `o` (u = 1), without flipping either's sign.
    fn slerp(self, o: Quat, u: f64) -> Quat {
        let d = self.dot(o);
        let (wa, wb) = if d > NLERP_DOT {
            (1.0 - u, u)
        } else {
            let th = d.clamp(-1.0, 1.0).acos();
            let s = th.sin();
            (((1.0 - u) * th).sin() / s, (u * th).sin() / s)
        };
        let q: [f64; 4] = array::from_fn(|c| wa * self.0[c] + wb * o.0[c]);
        let len = norm(&q);
        Quat(q.map(|x| x / len))
    }

    /// Where the camera looks: the rotated +y.
    fn forward(self) -> [f64; 3] {
        let [w, x, y, z] = self.0;
        [
            2.0 * (x * y - w * z),
            1.0 - 2.0 * (x * x + z * z),
            2.0 * (y * z + w * x),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(t: f64, pos: [f64; 3], rot: [f64; 3], fov: f64) -> Keyframe {
        Keyframe {
            t,
            pos,
            rot,
            fov,
            ease: Ease::Linear,
        }
    }

    /// The reference sweep, a synthetic inn interior: keyframes 0 to 1 cross ±180° of yaw, and 1 to 2 hold the
    /// position while the camera turns, rolls and zooms, eased in and out.
    fn inn() -> Vec<Keyframe> {
        vec![
            key(0.0, [100.0, 200.0, 10.0], [-5.0, 170.0, 0.0], 60.0),
            Keyframe {
                ease: Ease::EaseInOut,
                ..key(2.0, [103.0, 201.0, 10.5], [0.0, -170.0, 0.0], 60.0)
            },
            key(4.0, [103.0, 201.0, 10.5], [10.0, -120.0, 5.0], 45.0),
            key(7.0, [98.0, 205.0, 11.0], [0.0, -60.0, 0.0], 50.0),
        ]
    }

    /// Two keyframes: a straight dolly that turns, tilts and zooms.
    fn two() -> Vec<Keyframe> {
        vec![
            key(0.0, [0.0, 0.0, 0.0], [0.0, 0.0, 0.0], 60.0),
            key(4.0, [4.0, -2.0, 1.0], [10.0, 90.0, 0.0], 40.0),
        ]
    }

    fn linear(keys: &[Keyframe]) -> Vec<Keyframe> {
        keys.iter()
            .map(|k| Keyframe {
                ease: Ease::Linear,
                ..*k
            })
            .collect()
    }

    #[track_caller]
    fn assert_pose(got: Pose, want: Pose, tol: f64) {
        let pairs = got
            .pos
            .iter()
            .chain(&got.rot)
            .chain([&got.fov])
            .zip(want.pos.iter().chain(&want.rot).chain([&want.fov]));
        for (g, w) in pairs {
            assert!((g - w).abs() <= tol, "got {got:?}\nwant {want:?}");
        }
    }

    fn pose(pos: [f64; 3], rot: [f64; 3], fov: f64) -> Pose {
        Pose { pos, rot, fov }
    }

    #[test]
    fn json_is_canonical_and_strict() {
        let text = r#"{"keyframes":[
            {"t":0,"pos":[1.23456,-0.00004,3],"rot":[-0.00004,350,-180],"fov":60},
            {"t":2.00004,"pos":[1.2,2,3],"rot":[0,370.00001,540],"fov":59.99999,"ease":"linear"}],
            "world":{"save":"synthetic save","time":{"day":3,"hour":21},"player":{"pos":[0.00001,2,3],"yaw":-190},"paused":false}}"#;
        let sweep = Sweep::from_json(text).unwrap();
        let canonical = r#"{"keyframes":[{"t":0.0,"pos":[1.2346,0.0,3.0],"rot":[0.0,-10.0,180.0],"fov":60.0},{"t":2.0,"pos":[1.2,2.0,3.0],"rot":[0.0,10.0,180.0],"fov":60.0}],"world":{"save":"synthetic save","time":{"day":3,"hour":21},"player":{"pos":[0.0,2.0,3.0],"yaw":170.0}}}"#;
        assert_eq!(sweep.to_json(), canonical);
        assert_eq!(Sweep::from_json(canonical).unwrap(), sweep, "idempotent");
        assert_eq!((sweep.speed, sweep.ease), (Speed::Keys, Ease::Linear));

        let constant = r#"{"keyframes":[{"t":0,"pos":[0,0,0],"rot":[0,0,0],"fov":60},{"t":1,"pos":[1,0,0],"rot":[0,0,0],"fov":60}],"speed":"constant","ease":"ease_in_out"}"#;
        let sweep = Sweep::from_json(constant).unwrap();
        assert_eq!(
            (sweep.speed, sweep.ease),
            (Speed::Constant, Ease::EaseInOut)
        );
        assert!(
            sweep
                .to_json()
                .ends_with(r#""speed":"constant","ease":"ease_in_out"}"#)
        );

        for typo in [
            constant.replace("\"fov\":60}", "\"fov\":60,\"fov2\":1}"),
            constant.replace("\"speed\"", "\"speeed\""),
            constant.replace("\"speed\"", "\"world\":{\"wether\":\"x\"},\"speed\""),
            constant.replace("\"ease_in_out\"", "\"ease_in\""),
            constant.replace("[0,0,0],\"fov\"", "[0,0],\"fov\""),
        ] {
            assert!(Sweep::from_json(&typo).is_err(), "{typo}");
        }
    }

    #[test]
    fn refuses_what_the_game_cannot_play() {
        let refused = |sweep: Sweep, needle: &str| match sweep.canonical() {
            Err(SweepError(why)) => assert!(why.contains(needle), "{why:?} lacks {needle:?}"),
            Ok(s) => panic!("expected a refusal about {needle:?}, got {s:?}"),
        };
        let base = || Sweep {
            keyframes: inn(),
            speed: Speed::Keys,
            ease: Ease::Linear,
            world: World::default(),
        };
        let with = |f: &dyn Fn(&mut Sweep)| {
            let mut s = base();
            f(&mut s);
            s
        };
        assert!(base().canonical().is_ok());
        refused(with(&|s| s.keyframes.truncate(1)), "1 keyframes");
        refused(
            with(&|s| {
                s.keyframes = (0..=MAX_KEYFRAMES)
                    .map(|i| key(i as f64, [0.0; 3], [0.0; 3], 60.0))
                    .collect()
            }),
            "25 keyframes",
        );
        refused(
            with(&|s| s.keyframes[0].t = 0.5),
            "the first keyframe is at 0",
        );
        refused(with(&|s| s.keyframes[2].t = 2.00001), "not after");
        refused(with(&|s| s.keyframes[3].t = 3600.1), "at most 3600 s");
        refused(with(&|s| s.keyframes[3].pos[1] = f64::NAN), "isn't finite");
        refused(
            with(&|s| s.keyframes[2].rot[2] = f64::INFINITY),
            "isn't finite",
        );
        refused(
            with(&|s| s.keyframes[3].pos[0] = -100_000.0),
            "below 100000 m",
        );
        refused(with(&|s| s.keyframes[3].rot[0] = 89.5), "pitch 89.5");
        refused(with(&|s| s.keyframes[3].rot[0] = 260.0), "pitch -100");
        refused(with(&|s| s.keyframes[0].fov = 0.9), "fov 0.9");
        refused(with(&|s| s.keyframes[0].fov = 179.5), "fov 179.5");
        refused(
            with(&|s| s.keyframes[3].rot[1] = 60.0),
            "keyframes 2 to 3 turn 179",
        );
        refused(
            with(&|s| s.keyframes[3].ease = Ease::EaseInOut),
            "an ease on the last keyframe",
        );
        refused(with(&|s| s.speed = Speed::Constant), "under constant speed");
        refused(
            with(&|s| s.ease = Ease::EaseInOut),
            "applies to constant speed only",
        );
        refused(
            with(&|s| s.world.time = Some(GameTime { day: 1, hour: 24 })),
            "hour 24",
        );
        refused(
            with(&|s| s.world.save = Some(String::new())),
            "world save \"\"",
        );
        refused(
            with(&|s| s.world.weather = Some("WT_\nClear".into())),
            "world weather",
        );
        refused(
            with(&|s| {
                s.world.player = Some(Player {
                    pos: [0.0, 1e6, 0.0],
                    yaw: 0.0,
                })
            }),
            "world player",
        );
        // The turn check is on the rotation, so a turn just under 179° passes, pitch and all.
        assert!(
            with(&|s| {
                s.keyframes[3].rot = [-30.0, 58.9, 0.0];
            })
            .canonical()
            .is_ok()
        );
    }

    #[test]
    fn keyframes_are_hit_exactly_at_their_times() {
        let path = Path::new(&inn(), Speed::Keys, Ease::Linear).unwrap();
        let unwrapped = [170.0, 190.0, 240.0, 300.0];
        for (k, yaw) in path.keys().iter().zip(unwrapped) {
            let want = pose(k.pos, [k.rot[0], yaw, k.rot[2]], k.fov);
            assert_pose(path.sample(k.t), want, 1e-9);
        }
        assert_pose(path.sample(-1.0), path.sample(0.0), 0.0);
        assert_pose(path.sample(f64::NAN), path.sample(0.0), 0.0);
        assert_pose(path.sample(99.0), path.sample(7.0), 0.0);
    }

    #[test]
    fn yaw_takes_the_short_way_across_180() {
        for (from, to, mid) in [
            (170.0, -170.0, 180.0),
            (-170.0, 170.0, -180.0),
            (350.0, 10.0, 0.0),
            (10.0, -10.0, 0.0),
        ] {
            let keys = [
                key(0.0, [0.0; 3], [-20.0, from, 0.0], 60.0),
                key(2.0, [0.0; 3], [-20.0, to, 0.0], 60.0),
            ];
            let path = Path::new(&keys, Speed::Keys, Ease::Linear).unwrap();
            let start = path.sample(0.0).rot[1];
            for i in 0..=20 {
                let t = 0.1 * i as f64;
                let got = path.sample(t).rot;
                // A pure turn at constant pitch is a turn about world up: linear in time, pitch untouched.
                assert!(
                    (got[1] - (start + (mid - start) * t)).abs() < 1e-9,
                    "{from} to {to}: {got:?} at {t}"
                );
                assert!((got[0] + 20.0).abs() < 1e-9 && got[2] == 0.0, "{got:?}");
            }
        }
    }

    #[test]
    fn two_keyframes_are_a_straight_line_and_a_slerp() {
        let path = Path::new(&two(), Speed::Keys, Ease::Linear).unwrap();
        for i in 0..=8 {
            let f = i as f64 / 8.0;
            let got = path.sample(4.0 * f);
            for c in 0..3 {
                assert!((got.pos[c] - two()[1].pos[c] * f).abs() < 1e-12, "{got:?}");
            }
            assert!((got.fov - (60.0 - 20.0 * f)).abs() < 1e-12, "{got:?}");
            assert_eq!(got.rot[2], 0.0, "the horizon stays level: {got:?}");
        }
        // Pure pitch is a turn about the camera's right axis: linear, yaw untouched.
        let tilt = [
            key(0.0, [0.0; 3], [-30.0, 45.0, 0.0], 60.0),
            key(1.0, [0.0; 3], [40.0, 45.0, 0.0], 60.0),
        ];
        let path = Path::new(&tilt, Speed::Keys, Ease::Linear).unwrap();
        for i in 0..=10 {
            let got = path.sample(0.1 * i as f64).rot;
            assert!(
                (got[0] - (-30.0 + 7.0 * i as f64)).abs() < 1e-9 && (got[1] - 45.0).abs() < 1e-9,
                "{got:?}"
            );
        }
    }

    #[test]
    fn holds_stay_still_and_are_reached_at_rest() {
        let still = vec![key(0.0, [5.0, 6.0, 7.0], [3.0, -40.0, 2.0], 50.0); 3]
            .into_iter()
            .enumerate()
            .map(|(i, k)| Keyframe { t: i as f64, ..k })
            .collect::<Vec<_>>();
        for speed in [Speed::Keys, Speed::Constant] {
            let path = Path::new(&still, speed, Ease::Linear).unwrap();
            for i in 0..=20 {
                let k = &still[0];
                assert_pose(
                    path.sample(0.1 * i as f64),
                    pose(k.pos, k.rot, k.fov),
                    1e-12,
                );
            }
        }

        // A hold in the middle of a moving path: every channel still, and arrived at without a jolt.
        let mut keys = linear(&inn());
        keys[2] = Keyframe { t: 4.0, ..keys[1] };
        keys[3].rot[1] = 0.0;
        let path = Path::new(&keys, Speed::Keys, Ease::Linear).unwrap();
        let held = path.sample(2.0);
        for i in 0..=20 {
            assert_pose(path.sample(2.0 + 0.1 * i as f64), held, 1e-12);
        }
        let dist = |t: f64| {
            let p = path.sample(t);
            norm(&sub(p.pos, held.pos)) + (p.rot[1] - held.rot[1]).abs() + (p.fov - held.fov).abs()
        };
        // At rest: a step of h before the hold moves O(h²), so a tenth of the step moves about a hundredth.
        assert!(
            dist(1.999) < dist(1.99) / 50.0,
            "{} {}",
            dist(1.999),
            dist(1.99)
        );
        assert!(
            dist(4.001) < dist(4.01) / 50.0,
            "{} {}",
            dist(4.001),
            dist(4.01)
        );
    }

    #[test]
    fn roll_and_fov_never_overshoot_a_keyframe() {
        let keys = [
            key(0.0, [0.0; 3], [0.0, 0.0, 0.0], 60.0),
            key(1.0, [1.0, 0.0, 0.0], [0.0, 10.0, 12.0], 30.0),
            key(2.0, [2.0, 0.0, 0.0], [0.0, 20.0, 12.0], 45.0),
            key(3.0, [3.0, 0.0, 0.0], [0.0, 30.0, -3.0], 44.0),
            key(4.0, [4.0, 0.0, 0.0], [0.0, 40.0, -350.0], 90.0),
        ];
        let path = Path::new(&keys, Speed::Keys, Ease::Linear).unwrap();
        // Roll unwrapped: -350 is 10, the short way from -3.
        let rolls = [0.0, 12.0, 12.0, -3.0, 10.0];
        for i in 0..4 {
            let (r, f) = ((rolls[i], rolls[i + 1]), (keys[i].fov, keys[i + 1].fov));
            for j in 0..=50 {
                let got = path.sample(i as f64 + j as f64 / 50.0);
                let inside =
                    |v: f64, (a, b): (f64, f64)| v >= a.min(b) - 1e-9 && v <= a.max(b) + 1e-9;
                assert!(
                    inside(got.rot[2], r) && inside(got.fov, f),
                    "segment {i}: {got:?}"
                );
            }
        }
    }

    #[test]
    fn ease_in_out_starts_and_ends_at_rest() {
        let mut keys = two();
        keys[0].ease = Ease::EaseInOut;
        let eased = Path::new(&keys, Speed::Keys, Ease::Linear).unwrap();
        let plain = Path::new(&two(), Speed::Keys, Ease::Linear).unwrap();
        let moved = |p: &Path, a: f64, b: f64| norm(&sub(p.sample(b).pos, p.sample(a).pos));
        assert!(moved(&eased, 0.0, 0.01) < moved(&plain, 0.0, 0.01) / 50.0);
        assert!(moved(&eased, 3.99, 4.0) < moved(&plain, 3.99, 4.0) / 50.0);
        assert_pose(eased.sample(2.0), plain.sample(2.0), 1e-12);
    }

    #[test]
    fn constant_speed_spreads_travel_evenly() {
        let path = Path::new(&linear(&inn()), Speed::Constant, Ease::Linear).unwrap();
        // The camera's progress (travel plus turn, roll and fov, summed every millisecond) against time: proportional,
        // within 2 mm on this 10.7 m path.
        let at = |ms: u32| path.at(path.sigma(f64::from(ms) / 1000.0));
        let mut done = vec![0.0];
        for ms in 1..=7000 {
            done.push(done[done.len() - 1] + step(&at(ms - 1), &at(ms)));
        }
        let total = done[7000];
        assert!((total - 10.72).abs() < 0.01, "{total}");
        for (ms, done) in done.iter().enumerate() {
            let want = total * ms as f64 / 7000.0;
            assert!((done - want).abs() < 0.002, "at {ms} ms: {done} of {want}");
        }
    }

    #[test]
    fn room_pan_turns_evenly_both_ways() {
        let centre = pose([1.0, 2.0, 3.0], [-10.0, 170.0, 0.0], 70.0);
        let pan = Sweep::room_pan(&centre, 360.0, 5, 8.0, Ease::Linear).unwrap();
        let yaws: Vec<f64> = pan.keyframes.iter().map(|k| k.rot[1]).collect();
        assert_eq!(yaws, [170.0, -100.0, -10.0, 80.0, 170.0]);
        assert_eq!(pan.speed, Speed::Constant);
        let path = pan.path().unwrap();
        for i in 0..=32 {
            let got = path.sample(0.25 * i as f64);
            assert_pose(
                got,
                pose(centre.pos, [-10.0, 170.0 + 11.25 * i as f64, 0.0], 70.0),
                1e-9,
            );
        }
        let back = Sweep::room_pan(&centre, -360.0, 4, 6.0, Ease::Linear).unwrap();
        assert_pose(
            back.path().unwrap().sample(3.0),
            pose(centre.pos, [-10.0, -10.0, 0.0], 70.0),
            1e-9,
        );
        let eased = Sweep::room_pan(&centre, 360.0, 4, 6.0, Ease::EaseInOut)
            .unwrap()
            .path()
            .unwrap();
        for t in [0.1, 1.5, 3.0, 5.9] {
            let s = t / 6.0;
            assert!(
                (eased.sample(t).rot[1] - (170.0 + 360.0 * s * s * (3.0 - 2.0 * s))).abs() < 1e-9
            );
        }
        assert!(
            Sweep::room_pan(&centre, 360.0, 3, 6.0, Ease::Linear).is_err(),
            "180° steps"
        );
        assert!(Sweep::room_pan(&centre, 360.0, 0, 6.0, Ease::Linear).is_err());
    }

    #[test]
    fn frame_times_are_exact_and_cover_both_ends() {
        let path = Path::new(&inn(), Speed::Keys, Ease::Linear).unwrap();
        let times = path.frame_times(30).unwrap();
        assert_eq!(times.len(), 211);
        assert!(times.iter().enumerate().all(|(n, &t)| t == n as f64 / 30.0));
        assert_eq!((times[0], times[210]), (0.0, 7.0));
        assert_eq!(path.frame_count(24).unwrap(), 169);

        let short = [
            key(0.0, [0.0; 3], [0.0; 3], 60.0),
            key(0.3, [1.0, 0.0, 0.0], [0.0; 3], 60.0),
        ];
        let path = Path::new(&short, Speed::Keys, Ease::Linear).unwrap();
        assert_eq!(path.frame_times(10).unwrap(), [0.0, 0.1, 0.2, 0.3]);
        assert_eq!(
            path.frame_count(7).unwrap(),
            3,
            "0.3 s at 7 fps: frames at 0, 1/7 and 2/7 s"
        );

        for fps in [0, MAX_FPS + 1] {
            assert!(path.frame_count(fps).is_err());
        }
        assert!(check_playback(Mode::Realtime, None).is_ok());
        assert!(check_playback(Mode::Step, Some(MAX_FPS)).is_ok());
        for (mode, fps) in [
            (Mode::Realtime, Some(30)),
            (Mode::Step, None),
            (Mode::Step, Some(0)),
        ] {
            assert!(check_playback(mode, fps).is_err(), "{mode:?} {fps:?}");
        }
    }

    /// The reference vectors the in-game `cam.path` port is checked against: (t, pose) for the sweeps
    /// above, to 6 decimals. Yaw is unwrapped along the path; compare angles modulo 360. The game computes in single
    /// precision, so it matches to about 1e-3 m and 1e-2°.
    #[test]
    fn reference_vectors() {
        let keys = Path::new(&inn(), Speed::Keys, Ease::Linear).unwrap();
        let constant = Path::new(&linear(&inn()), Speed::Constant, Ease::EaseInOut).unwrap();
        let two = Path::new(&two(), Speed::Keys, Ease::Linear).unwrap();
        #[rustfmt::skip]
        let vectors = [
            (&keys, 0.0, pose([100.000000, 200.000000, 10.000000], [-5.000000, 170.000000, 0.000000], 60.000000)),
            (&keys, 0.5, pose([100.890625, 200.296875, 10.148438], [-3.803810, 174.602628, 0.000000], 60.000000)),
            (&keys, 1.25, pose([102.314453, 200.771484, 10.385742], [-2.041704, 181.241670, 0.000000], 60.000000)),
            (&keys, 2.0, pose([103.000000, 201.000000, 10.500000], [0.000000, 190.000000, 0.000000], 60.000000)),
            (&keys, 2.75, pose([103.000000, 201.000000, 10.500000], [3.131433, 202.283564, 1.184930], 56.445209)),
            (&keys, 3.5, pose([103.000000, 201.000000, 10.500000], [9.351144, 231.140883, 4.671936], 45.984192)),
            (&keys, 4.0, pose([103.000000, 201.000000, 10.500000], [10.000000, 240.000000, 5.000000], 45.000000)),
            (&keys, 5.5, pose([101.125000, 202.500000, 10.687500], [6.271576, 269.503105, 3.125000], 46.875000)),
            (&keys, 7.0, pose([98.000000, 205.000000, 11.000000], [0.000000, 300.000000, 0.000000], 50.000000)),
            (&constant, 0.0, pose([100.000000, 200.000000, 10.000000], [-5.000000, 170.000000, 0.000000], 60.000000)),
            (&constant, 1.0, pose([100.553810, 200.184603, 10.092302], [-4.214690, 173.065144, 0.000000], 60.000000)),
            (&constant, 2.5, pose([102.913306, 200.971102, 10.485551], [-0.733686, 186.699050, 0.000000], 60.000000)),
            (&constant, 3.5, pose([102.133308, 201.693354, 10.586669], [8.377115, 258.659924, 4.133308], 45.866692)),
            (&constant, 4.5, pose([100.410355, 203.071716, 10.758964], [4.775688, 276.274653, 2.410355], 47.589645)),
            (&constant, 6.0, pose([98.456051, 204.635159, 10.954395], [0.848109, 294.904290, 0.456051], 49.543949)),
            (&constant, 7.0, pose([98.000000, 205.000000, 11.000000], [0.000000, 300.000000, 0.000000], 50.000000)),
            // Checked against an independent slerp of the whole rotation (no roll in either keyframe, so none to keep).
            (&two, 0.0, pose([0.000000, 0.000000, 0.000000], [0.000000, 0.000000, 0.000000], 60.000000)),
            (&two, 1.0, pose([1.000000, -0.500000, 0.250000], [2.292523, 22.411368, 0.000000], 55.000000)),
            (&two, 2.0, pose([2.000000, -1.000000, 0.500000], [4.996722, 44.818750, 0.000000], 50.000000)),
            (&two, 3.0, pose([3.000000, -1.500000, 0.750000], [7.703879, 67.319774, 0.000000], 45.000000)),
            (&two, 4.0, pose([4.000000, -2.000000, 1.000000], [10.000000, 90.000000, 0.000000], 40.000000)),
        ];
        for (path, t, want) in vectors {
            assert_pose(path.sample(t), want, 1e-6);
        }
        // The constant-speed table's total, for checking a port's table before its samples.
        assert!((constant.arc[constant.arc.len() - 1] - 10.720605).abs() < 1e-6);
    }
}
