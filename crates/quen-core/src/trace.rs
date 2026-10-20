//! Actor traces (the runner design's §3.5, finding 57): samples of `world.actors`, and of the player from `world.get`,
//! taken by polling the game's channel, then behaviour metrics per window against a reference body (the player, or a
//! subject by id) and a verdict per metric over an A/B run. Every distance is horizontal (x, y), centre to centre, in the
//! scripts' units (metres); every speed is the moving agent component's `GetVelocity()`, never position differences.

use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt::{self, Write as _};

/// The fastest sampling rate: each sample is one or two channel verbs, each answered on a game tick.
pub const MAX_HZ: f64 = 10.0;
/// Contact: entering within the sampler's `contact_m` of the reference, over again only after leaving past it plus
/// `CONTACT_LEAVE_M`. Default `CONTACT_M`: in boot 187 (collisions on), NPCs that pushed the player came no closer than
/// 0.699 m centre to centre, and 9 tracks stopped between 0.70 and 0.83 m (finding 84; 0.65 m counted none of them).
pub const CONTACT_M: f64 = 0.75;
pub const CONTACT_LEAVE_M: f64 = 0.15;
/// A pass: a run of a subject's samples within `PASS_M` of the reference; also what exposes the reference to a push.
pub const PASS_M: f64 = 3.0;
/// Moving above `MOVING_MPS`; gliding when the speed across the heading is above `GLIDE_MPS` too.
pub const MOVING_MPS: f64 = 0.3;
pub const GLIDE_MPS: f64 = 0.3;
/// Blocked: below `STILL_MPS` for at least `STILL_S`, within `BLOCKED_M` of the reference, after having moved.
pub const STILL_MPS: f64 = 0.1;
pub const STILL_S: f64 = 1.0;
pub const BLOCKED_M: f64 = 1.5;
/// A push event: the reference moved over `PUSH_M` within `PUSH_S`; events starting within `CHAIN_S` of the last chain.
pub const PUSH_M: f64 = 0.05;
pub const PUSH_S: f64 = 0.5;
pub const CHAIN_S: f64 = 2.0;
/// Fewer passes than this in any window: the event metrics get no verdict.
pub const MIN_PASSES: u32 = 10;
/// Track association: a subject joins the nearest track seen within `TRACK_GAP_S` and within
/// `TRACK_GATE_M + TRACK_MAX_MPS × dt` of where it was last.
const TRACK_GAP_S: f64 = 1.0;
const TRACK_GATE_M: f64 = 0.5;
const TRACK_MAX_MPS: f64 = 8.0;

/// What the metrics are measured against.
#[derive(Debug, Clone, PartialEq)]
pub enum Reference {
    /// The player, read by `world.get` in every sample.
    Player,
    /// The subject with this id (`GetGuidHash`, quen-w3 0.1.12), taken out of the subjects.
    Id(i64),
}

/// A trace's settings: identical in every window of a comparison, and named on every row.
#[derive(Debug, Clone, PartialEq)]
pub struct Sampler {
    pub hz: f64,
    pub radius: f64,
    pub max: u32,
    pub tag: Option<String>,
    pub class: Option<String>,
    pub reference: Reference,
    /// Metres: a contact's distance (`CONTACT_M` unless given).
    pub contact: f64,
}

/// A tag or class name: 1 to 64 letters, digits and `_`.
pub fn plain_name(s: &str) -> bool {
    (1..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

impl Sampler {
    /// `{hz, radius, max, tag, class, reference_id, contact_m}`, each optional: 10 Hz, 8 m, 20, no tag, alive actors,
    /// the player, [`CONTACT_M`]. Radius and max are `world.actors`' caps.
    pub fn parse(v: &Value, max_radius: f64, max_count: u32) -> Result<Sampler, String> {
        let map = v.as_object().ok_or("trace is an object")?;
        if let Some(k) = map.keys().find(|k| {
            ![
                "hz",
                "radius",
                "max",
                "tag",
                "class",
                "reference_id",
                "contact_m",
            ]
            .contains(&k.as_str())
        }) {
            return Err(format!("trace: unknown key {k:?}"));
        }
        let number = |k: &str, default: f64, ok: &dyn Fn(f64) -> bool, what: &str| match v.get(k) {
            None => Ok(default),
            Some(x) => x
                .as_f64()
                .filter(|x| ok(*x))
                .ok_or(format!("trace: {k} must be {what}")),
        };
        let hz = number("hz", MAX_HZ, &|x| x > 0.0 && x <= MAX_HZ, "above 0 to 10")?;
        let radius = number(
            "radius",
            8.0,
            &|x| x > 0.0 && x <= max_radius,
            "metres above 0, at most the radius cap",
        )?;
        let max = number(
            "max",
            20.0,
            &|x| x.fract() == 0.0 && (1.0..=f64::from(max_count)).contains(&x),
            "a whole number from 1 to the cap",
        )? as u32;
        let name = |k: &str| match v.get(k) {
            None => Ok(None),
            Some(x) => x
                .as_str()
                .filter(|s| plain_name(s))
                .map(|s| Some(s.to_owned()))
                .ok_or(format!("trace: {k} must be 1 to 64 letters, digits and _")),
        };
        let reference = match v.get("reference_id") {
            None => Reference::Player,
            Some(id) => Reference::Id(
                id.as_i64()
                    .ok_or("trace: reference_id is a whole number, an id world_actors read")?,
            ),
        };
        let contact = number(
            "contact_m",
            CONTACT_M,
            &|x| (0.1..=PASS_M).contains(&x),
            "metres from 0.1 to 3",
        )?;
        Ok(Sampler {
            hz,
            radius,
            max,
            tag: name("tag")?,
            class: name("class")?,
            reference,
            contact,
        })
    }

    /// Whether it needs quen-w3 0.1.12's world.actors: a tag, a class or an id reference.
    pub fn needs_ids(&self) -> bool {
        self.tag.is_some() || self.class.is_some() || self.reference != Reference::Player
    }
}

/// `actor_trace 10 Hz r8 max20 ref player contact 0.75`, with ` tag X` and ` class Y` when set: how rows and results
/// name it.
impl fmt::Display for Sampler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "actor_trace {} Hz r{} max{}",
            self.hz, self.radius, self.max
        )?;
        if let Some(t) = &self.tag {
            write!(f, " tag {t}")?;
        }
        if let Some(c) = &self.class {
            write!(f, " class {c}")?;
        }
        match self.reference {
            Reference::Player => write!(f, " ref player"),
            Reference::Id(id) => write!(f, " ref id {id}"),
        }?;
        write!(f, " contact {}", self.contact)
    }
}

/// One body in one sample, as the game read it.
#[derive(Debug, Clone, PartialEq)]
pub struct Body {
    pub id: Option<i64>,
    pub pos: [f64; 3],
    /// Degrees.
    pub heading: f64,
    pub velocity: Option<[f64; 3]>,
    /// modQuen's bits: 1 combat, 2 hostile, 4 guard, 8 at work, 16 action point, 32 scene, 64 velocity known, 128
    /// entity cut, 256 not an actor.
    pub flags: u32,
    pub entity: String,
}

fn vec3(v: &Value) -> Option<[f64; 3]> {
    let a = v.as_array().filter(|a| a.len() == 3)?;
    Some([a[0].as_f64()?, a[1].as_f64()?, a[2].as_f64()?])
}

impl Body {
    /// One of `world_actors`' actors.
    pub fn actor(v: &Value) -> Result<Body, String> {
        let bits = [
            ("combat", 1),
            ("hostile", 2),
            ("guard", 4),
            ("at_work", 8),
            ("action_point", 16),
            ("scene", 32),
            ("entity_cut", 128),
        ];
        let mut flags = bits
            .iter()
            .filter(|(k, _)| v[k] == true)
            .map(|(_, b)| b)
            .sum::<u32>();
        let velocity = vec3(&v["velocity"]);
        flags |=
            if velocity.is_some() { 64 } else { 0 } | if v["actor"] == false { 256 } else { 0 };
        Ok(Body {
            id: v["id"].as_i64(),
            pos: vec3(&v["pos"]).ok_or("an actor without pos")?,
            heading: v["heading"].as_f64().ok_or("an actor without heading")?,
            velocity,
            flags,
            entity: v["entity"].as_str().unwrap_or_default().to_owned(),
        })
    }

    /// The player of a `world_get` read.
    pub fn player(world: &Value) -> Result<Body, String> {
        let p = &world["player"];
        Ok(Body {
            id: None,
            pos: vec3(&p["pos"]).ok_or("world.get without player.pos")?,
            heading: p["heading"]
                .as_f64()
                .ok_or("world.get without player.heading")?,
            velocity: None,
            flags: 0,
            entity: "player".into(),
        })
    }

    fn speed(&self) -> Option<f64> {
        self.velocity.map(|v| v[0].hypot(v[1]))
    }

    /// The speed across the heading, forward being (−sin h, cos h) as game_camera's yaw showed it.
    fn across(&self) -> Option<f64> {
        let h = self.heading.to_radians();
        self.velocity
            .map(|v| (v[0] * h.cos() + v[1] * h.sin()).abs())
    }
}

fn flat(a: [f64; 3], b: [f64; 3]) -> f64 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

/// One sample: when its subjects' reply came (seconds from the window's start), the reference (none when it wasn't
/// found), the subjects and `world.actors`' found.
#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    pub t: f64,
    pub reference: Option<Body>,
    pub subjects: Vec<Body>,
    pub found: u32,
}

impl Sample {
    /// From a `world_actors` read (`found`, `actors`) and, with the player as reference, a `world_get` read. An id
    /// reference is taken out of the subjects; none or several with it leave the sample without one.
    pub fn new(
        t: f64,
        actors: &Value,
        world: Option<&Value>,
        reference: &Reference,
    ) -> Result<Sample, String> {
        let mut subjects = actors["actors"]
            .as_array()
            .ok_or("world.actors without actors")?
            .iter()
            .map(Body::actor)
            .collect::<Result<Vec<_>, _>>()?;
        let reference = match (reference, world) {
            (Reference::Player, Some(world)) => Some(Body::player(world)?),
            (Reference::Player, None) => return Err("a player reference needs world.get".into()),
            (Reference::Id(id), _) => {
                let at: Vec<usize> = (0..subjects.len())
                    .filter(|&i| subjects[i].id == Some(*id))
                    .collect();
                match at[..] {
                    [i] => Some(subjects.remove(i)),
                    _ => None,
                }
            }
        };
        let found = actors["found"].as_u64().unwrap_or(0) as u32;
        Ok(Sample {
            t,
            reference,
            subjects,
            found,
        })
    }
}

/// Per sample, per subject: its track. A subject joins the open track with its id (unique and set in the sample),
/// else the nearest open track within the gate whose id doesn't differ; else it opens a new one.
pub fn tracks(samples: &[Sample]) -> Vec<Vec<usize>> {
    // Per track: last position, last time, id.
    let mut open: Vec<([f64; 3], f64, Option<i64>)> = Vec::new();
    let mut out = Vec::new();
    for s in samples {
        let mut of = vec![usize::MAX; s.subjects.len()];
        let mut taken = vec![false; open.len()];
        let unique = |id: Option<i64>| {
            id.filter(|id| s.subjects.iter().filter(|b| b.id == Some(*id)).count() == 1)
        };
        let live = |k: usize| s.t - open[k].1 <= TRACK_GAP_S;
        for (j, b) in s.subjects.iter().enumerate() {
            if let Some(id) = unique(b.id)
                && let Some(k) =
                    (0..open.len()).find(|&k| !taken[k] && open[k].2 == Some(id) && live(k))
            {
                (of[j], taken[k]) = (k, true);
            }
        }
        let mut pairs: Vec<(f64, usize, usize)> = Vec::new();
        for (j, b) in s
            .subjects
            .iter()
            .enumerate()
            .filter(|(j, _)| of[*j] == usize::MAX)
        {
            for k in (0..open.len()).filter(|&k| !taken[k] && live(k)) {
                let d = flat(b.pos, open[k].0);
                let ids_differ = b.id.is_some() && open[k].2.is_some() && b.id != open[k].2;
                if !ids_differ && d <= TRACK_GATE_M + TRACK_MAX_MPS * (s.t - open[k].1) {
                    pairs.push((d, j, k));
                }
            }
        }
        pairs.sort_by(|a, b| a.0.total_cmp(&b.0));
        for (_, j, k) in pairs {
            if of[j] == usize::MAX && !taken[k] {
                (of[j], taken[k]) = (k, true);
            }
        }
        for (j, b) in s.subjects.iter().enumerate() {
            if of[j] == usize::MAX {
                of[j] = open.len();
                open.push((b.pos, s.t, b.id));
            } else {
                open[of[j]] = (b.pos, s.t, b.id.or(open[of[j]].2));
            }
        }
        out.push(of);
    }
    out
}

/// One window's behaviour metrics. `None`: nothing to measure it on (no pass, no moving sample, no stretch without a
/// subject near the reference).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Metrics {
    pub samples: usize,
    /// From the first sample to the last.
    pub seconds: f64,
    /// Samples without the reference (an id reference not found once).
    pub reference_missing: usize,
    pub contacts_per_min: f64,
    pub closest_m_p10: Option<f64>,
    pub closest_m_min: Option<f64>,
    pub glide_share: Option<f64>,
    pub blocked_s_per_min: f64,
    pub push_m: Option<f64>,
    /// What `push_m`'s jitter floor came from: `calm steps`, or `the median step` when every step was exposed.
    pub push_floor: Option<&'static str>,
    pub push_chains: u32,
    pub passes: u32,
    pub subject_minutes: f64,
    pub crowd_mean: f64,
}

/// Each metric's name, unit, and whether it is an event metric (no verdict below [`MIN_PASSES`] passes).
pub const METRICS: [(&str, &str, bool); 10] = [
    ("contacts_per_min", "1/min", true),
    ("closest_m_p10", "m", true),
    ("closest_m_min", "m", true),
    ("glide_share", "share", true),
    ("blocked_s_per_min", "s/min", true),
    ("push_m", "m", true),
    ("push_chains", "count", true),
    ("passes", "count", false),
    ("subject_minutes", "min", false),
    ("crowd_mean", "count", false),
];

impl Metrics {
    /// The metric named as in [`METRICS`].
    pub fn get(&self, metric: &str) -> Option<f64> {
        match metric {
            "contacts_per_min" => Some(self.contacts_per_min),
            "closest_m_p10" => self.closest_m_p10,
            "closest_m_min" => self.closest_m_min,
            "glide_share" => self.glide_share,
            "blocked_s_per_min" => Some(self.blocked_s_per_min),
            "push_m" => self.push_m,
            "push_chains" => Some(f64::from(self.push_chains)),
            "passes" => Some(f64::from(self.passes)),
            "subject_minutes" => Some(self.subject_minutes),
            "crowd_mean" => Some(self.crowd_mean),
            _ => None,
        }
    }
}

/// The `q` quantile, linear between ranks (numpy's default, as stats.rs).
fn quantile(mut v: Vec<f64>, q: f64) -> Option<f64> {
    v.sort_by(f64::total_cmp);
    let rank = q * (v.len().checked_sub(1)?) as f64;
    let (lo, hi) = (v[rank.floor() as usize], v[rank.ceil() as usize]);
    Some(lo + (hi - lo) * rank.fract())
}

fn p10(v: Vec<f64>) -> Option<f64> {
    quantile(v, 0.1)
}

fn median(v: Vec<f64>) -> Option<f64> {
    quantile(v, 0.5)
}

/// The window's metrics, contacts within `contact` m. Errs with fewer than two samples.
pub fn metrics(samples: &[Sample], contact: f64) -> Result<Metrics, String> {
    let (Some(first), Some(last)) = (samples.first(), samples.last()) else {
        return Err("no sample".into());
    };
    let seconds = last.t - first.t;
    if samples.len() < 2 || seconds <= 0.0 {
        return Err(format!(
            "{} sample(s) over {seconds:.2} s: too few to measure",
            samples.len()
        ));
    }
    let per_min = |x: f64| x * 60.0 / seconds;
    let of = tracks(samples);
    let n_tracks = of.iter().flatten().max().map_or(0, |m| m + 1);
    // Per track: in contact, in a pass (its minimum so far), has moved, a still stretch's start and last time.
    let mut touching = vec![false; n_tracks];
    let mut pass: Vec<Option<f64>> = vec![None; n_tracks];
    let mut moved = vec![false; n_tracks];
    let mut still: Vec<Option<(f64, f64)>> = vec![None; n_tracks];
    let (mut contacts, mut minima, mut blocked) = (0u32, Vec::new(), 0.0);
    let (mut moving, mut gliding) = (0u32, 0u32);
    let still_end = |still: &mut Option<(f64, f64)>, blocked: &mut f64| {
        if let Some((a, b)) = still.take()
            && b - a >= STILL_S
        {
            *blocked += b - a;
        }
    };
    for (s, tracks) in samples.iter().zip(&of) {
        for b in &s.subjects {
            if let Some(v) = b.speed().filter(|v| *v > MOVING_MPS) {
                moving += 1;
                gliding += u32::from(b.across().is_some_and(|a| a > GLIDE_MPS) && v > MOVING_MPS);
            }
        }
        let Some(r) = &s.reference else { continue };
        let seen: Vec<usize> = tracks.clone();
        for (b, &k) in s.subjects.iter().zip(tracks) {
            let d = flat(b.pos, r.pos);
            if !touching[k] && d < contact {
                (touching[k], contacts) = (true, contacts + 1);
            } else if touching[k] && d > contact + CONTACT_LEAVE_M {
                touching[k] = false;
            }
            match (pass[k], d <= PASS_M) {
                (Some(m), true) => pass[k] = Some(m.min(d)),
                (None, true) => pass[k] = Some(d),
                (Some(m), false) => (minima.push(m), pass[k] = None).1,
                (None, false) => {}
            }
            let speed = b.speed();
            if speed.is_some_and(|v| v < STILL_MPS)
                && d <= BLOCKED_M
                && (moved[k] || still[k].is_some())
            {
                still[k] = Some((still[k].map_or(s.t, |(a, _)| a), s.t));
            } else {
                still_end(&mut still[k], &mut blocked);
            }
            moved[k] |= speed.is_some_and(|v| v > MOVING_MPS);
        }
        // A track not in this sample ends its pass and its still stretch.
        for k in (0..n_tracks).filter(|k| !seen.contains(k)) {
            if let Some(m) = pass[k].take() {
                minima.push(m);
            }
            still_end(&mut still[k], &mut blocked);
        }
    }
    minima.extend(pass.into_iter().flatten());
    for s in &mut still {
        still_end(s, &mut blocked);
    }
    // The reference's steps, exposed (a subject within PASS_M at either end) or not.
    let near = |s: &Sample| {
        s.reference
            .as_ref()
            .is_some_and(|r| s.subjects.iter().any(|b| flat(b.pos, r.pos) <= PASS_M))
    };
    let (mut exposed, mut calm, mut steps) = ((0.0, 0u32), (0.0, 0u32), Vec::new());
    for w in samples.windows(2) {
        let (Some(a), Some(b)) = (&w[0].reference, &w[1].reference) else {
            continue;
        };
        let step = flat(a.pos, b.pos);
        steps.push(step);
        let bin = if near(&w[0]) || near(&w[1]) {
            &mut exposed
        } else {
            &mut calm
        };
        (bin.0, bin.1) = (bin.0 + step, bin.1 + 1);
    }
    // The jitter floor: the calm steps' mean; with none (a crowd around the reference throughout, finding 81), the
    // median step, which a push moving the reference in under half the steps leaves at its standing jitter.
    let floor = match calm.1 {
        0 => median(steps).map(|m| (m, "the median step")),
        n => Some((calm.0 / f64::from(n), "calm steps")),
    };
    let push_m = floor.map(|(f, _)| exposed.0 - f64::from(exposed.1) * f);
    // Push events: a sample from which the reference is over PUSH_M away PUSH_S later; a run of them is one event.
    let refs: Vec<(f64, [f64; 3])> = samples
        .iter()
        .filter_map(|s| s.reference.as_ref().map(|r| (s.t, r.pos)))
        .collect();
    let mut starts = Vec::new();
    let mut was = false;
    for (i, &(t, p)) in refs.iter().enumerate() {
        let is = refs[i..]
            .iter()
            .find(|(u, _)| *u >= t + PUSH_S)
            .is_some_and(|&(_, q)| flat(p, q) > PUSH_M);
        if is && !was {
            starts.push(t);
        }
        was = is;
    }
    let mut chains = 0;
    let mut run = 1;
    for w in starts.windows(2) {
        if w[1] - w[0] <= CHAIN_S {
            run += 1;
            chains += u32::from(run == 2);
        } else {
            run = 1;
        }
    }
    let subject_minutes = samples
        .windows(2)
        .map(|w| w[0].subjects.len() as f64 * (w[1].t - w[0].t))
        .sum::<f64>()
        / 60.0;
    Ok(Metrics {
        samples: samples.len(),
        seconds,
        reference_missing: samples.iter().filter(|s| s.reference.is_none()).count(),
        contacts_per_min: per_min(f64::from(contacts)),
        closest_m_p10: p10(minima.clone()),
        closest_m_min: minima.iter().copied().reduce(f64::min),
        glide_share: (moving > 0).then(|| f64::from(gliding) / f64::from(moving)),
        blocked_s_per_min: per_min(blocked),
        push_m,
        push_floor: floor.map(|(_, from)| from),
        push_chains: chains,
        passes: minima.len() as u32,
        subject_minutes,
        crowd_mean: samples.iter().map(|s| f64::from(s.found)).sum::<f64>() / samples.len() as f64,
    })
}

/// One metric's A/B verdict, by measure_ab's rule for frame times: `outside drift` when |diff| exceeds every state's
/// drift (the spread of its windows' values); `too few events` when an event metric's window has under [`MIN_PASSES`]
/// passes; `no value` when a window has none.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MetricVerdict {
    pub metric: &'static str,
    pub unit: &'static str,
    pub means: BTreeMap<String, f64>,
    pub drift: BTreeMap<String, f64>,
    /// The second state's mean minus the first's.
    pub diff: Option<f64>,
    pub verdict: &'static str,
}

/// Per metric of [`METRICS`], over windows labelled with two labels, the first label being the first window's.
pub fn verdicts(windows: &[(&str, &Metrics)]) -> Vec<MetricVerdict> {
    let mut labels: Vec<&str> = Vec::new();
    for (l, _) in windows {
        if !labels.contains(l) {
            labels.push(l);
        }
    }
    let few = windows.iter().any(|(_, m)| m.passes < MIN_PASSES);
    METRICS
        .iter()
        .map(|&(metric, unit, event)| {
            let values: Vec<(&str, Option<f64>)> =
                windows.iter().map(|(l, m)| (*l, m.get(metric))).collect();
            let of = |label: &str| -> Vec<f64> {
                values
                    .iter()
                    .filter(|(l, _)| *l == label)
                    .filter_map(|(_, v)| *v)
                    .collect()
            };
            let mut means = BTreeMap::new();
            let mut drift = BTreeMap::new();
            for l in &labels {
                let v = of(l);
                if !v.is_empty() {
                    means.insert(l.to_string(), v.iter().sum::<f64>() / v.len() as f64);
                    let spread = v.iter().copied().fold(f64::MIN, f64::max)
                        - v.iter().copied().fold(f64::MAX, f64::min);
                    drift.insert(l.to_string(), spread);
                }
            }
            let diff = match labels[..] {
                [a, b] => means.get(b).zip(means.get(a)).map(|(b, a)| b - a),
                _ => None,
            };
            let verdict = if values.iter().any(|(_, v)| v.is_none()) || labels.len() != 2 {
                "no value"
            } else if event && few {
                "too few events"
            } else if diff.is_some_and(|d| drift.values().all(|s| d.abs() > *s)) {
                "outside drift"
            } else {
                "inside drift"
            };
            MetricVerdict {
                metric,
                unit,
                means,
                drift,
                diff,
                verdict,
            }
        })
        .collect()
}

/// A window's trace as CSV: one row per body per sample, the reference first (`who` reference, no track), then the
/// subjects with their track; `distance_m` from the reference in that sample, empty without one.
pub fn csv(samples: &[Sample]) -> String {
    let mut out =
        String::from("t_s,sample,who,track,id,x,y,z,heading,vx,vy,vz,distance_m,flags,entity\n");
    let num = |x: Option<f64>| x.map_or(String::new(), |x| format!("{x}"));
    for (i, (s, tracks)) in samples.iter().zip(tracks(samples)).enumerate() {
        let rows = s.reference.iter().map(|b| ("reference", None, b)).chain(
            s.subjects
                .iter()
                .zip(tracks)
                .map(|(b, k)| ("subject", Some(k), b)),
        );
        for (who, track, b) in rows {
            let v = b.velocity;
            let distance = s
                .reference
                .as_ref()
                .filter(|_| who == "subject")
                .map(|r| flat(b.pos, r.pos));
            let _ = writeln!(
                out,
                "{},{i},{who},{},{},{},{},{},{},{},{},{},{},{},\"{}\"",
                s.t,
                track.map_or(String::new(), |k| k.to_string()),
                b.id.map_or(String::new(), |id| id.to_string()),
                b.pos[0],
                b.pos[1],
                b.pos[2],
                b.heading,
                num(v.map(|v| v[0])),
                num(v.map(|v| v[1])),
                num(v.map(|v| v[2])),
                num(distance),
                b.flags,
                b.entity.replace('"', "\"\""),
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn metrics_(samples: &[Sample]) -> Result<Metrics, String> {
        metrics(samples, CONTACT_M)
    }

    fn body(id: Option<i64>, x: f64, y: f64, heading: f64, v: Option<[f64; 2]>) -> Body {
        Body {
            id,
            pos: [x, y, 0.0],
            heading,
            velocity: v.map(|v| [v[0], v[1], 0.0]),
            flags: 64,
            entity: "npc".into(),
        }
    }

    /// At 10 Hz for `secs`: the reference standing at the origin and the subjects `at(t)` gives.
    fn trace(secs: f64, at: impl Fn(f64) -> Vec<Body>) -> Vec<Sample> {
        (0..=(secs * 10.0) as usize)
            .map(|i| {
                let t = i as f64 / 10.0;
                let subjects = at(t);
                Sample {
                    t,
                    reference: Some(body(None, 0.0, 0.0, 0.0, None)),
                    found: subjects.len() as u32,
                    subjects,
                }
            })
            .collect()
    }

    /// One walker passing along y = `offset` at 1 m/s, heading along +x (forward (−sin h, cos h): h = −90).
    fn walker(id: Option<i64>, offset: f64) -> impl Fn(f64) -> Body {
        move |t| body(id, -5.0 + t, offset, -90.0, Some([1.0, 0.0]))
    }

    #[test]
    fn a_scripted_pass_gives_its_closest_approach_and_one_contact() {
        let near = walker(None, 0.5);
        let m = metrics_(&trace(10.0, |t| vec![near(t)])).unwrap();
        assert_eq!(m.passes, 1);
        assert!((m.closest_m_min.unwrap() - 0.5).abs() < 1e-9, "{m:?}");
        assert_eq!(m.contacts_per_min, 6.0, "one contact in 10 s");
        assert_eq!(m.glide_share, Some(0.0), "walking along its heading");
        assert_eq!(m.push_m, Some(0.0));
        // A pass at 1 m: no contact, the closest 1 m.
        let far = walker(None, 1.0);
        let m = metrics_(&trace(10.0, |t| vec![far(t)])).unwrap();
        assert_eq!((m.contacts_per_min, m.closest_m_min), (0.0, Some(1.0)));
        // Finding 84: a pass at 0.70 m, where bodies touched in boot 187, is a contact by default, not at 0.65 m.
        let touch = walker(None, 0.7);
        let samples = trace(10.0, |t| vec![touch(t)]);
        assert_eq!(metrics(&samples, CONTACT_M).unwrap().contacts_per_min, 6.0);
        assert_eq!(metrics(&samples, 0.65).unwrap().contacts_per_min, 0.0);
    }

    #[test]
    fn contacts_need_leaving_past_the_outer_bound() {
        // In at 0.6, out to 0.7 (not past 0.8), in again, then out to 2 and in: two contacts.
        let path = [0.6, 0.7, 0.6, 0.7, 2.0, 0.6, 2.0];
        let samples: Vec<Sample> = path
            .iter()
            .enumerate()
            .map(|(i, &x)| Sample {
                t: i as f64,
                reference: Some(body(None, 0.0, 0.0, 0.0, None)),
                subjects: vec![body(Some(7), x, 0.0, 0.0, Some([0.0, 0.0]))],
                found: 1,
            })
            .collect();
        let m = metrics_(&samples).unwrap();
        assert_eq!(m.contacts_per_min, 2.0 * 60.0 / 6.0);
    }

    #[test]
    fn sideways_motion_is_glide_and_a_blocked_walker_counts() {
        // Pushed sideways: velocity along +x, heading along +y (h = 0).
        let side = |t: f64| vec![body(None, -2.0 + t * 0.5, 1.0, 0.0, Some([0.5, 0.0]))];
        let m = metrics_(&trace(5.0, side)).unwrap();
        assert_eq!(m.glide_share, Some(1.0));
        // Walks in for 2 s, then stands at 1 m from t = 2 to 5: blocked 3 s in 5 s.
        let stop = |t: f64| {
            let x = if t < 2.0 { -1.0 + t } else { 1.0 };
            let v = if t < 2.0 { 1.0 } else { 0.0 };
            vec![body(None, x, 0.0, -90.0, Some([v, 0.0]))]
        };
        let m = metrics_(&trace(5.0, stop)).unwrap();
        assert!((m.blocked_s_per_min - 3.0 * 12.0).abs() < 1e-6, "{m:?}");
        // One that never moved isn't blocked.
        let idle = |_: f64| vec![body(None, 1.0, 0.0, 0.0, Some([0.0, 0.0]))];
        assert_eq!(metrics_(&trace(5.0, idle)).unwrap().blocked_s_per_min, 0.0);
    }

    #[test]
    fn a_push_is_the_reference_moving_while_exposed_beyond_its_jitter() {
        // The reference jitters 1 cm a sample throughout; from t = 5 a subject stands beside it and it is pushed 10 cm a
        // sample for 1 s (twice, 1.5 s apart: a chain).
        let samples: Vec<Sample> = (0..=100)
            .map(|i| {
                let t = i as f64 / 10.0;
                let jitter = if i % 2 == 0 { 0.0 } else { 0.01 };
                let pushed = |from: f64| ((t - from).clamp(0.0, 1.0) * 10.0).round() * 0.1;
                let x = jitter + pushed(5.0) + pushed(6.5);
                let near = t >= 5.0;
                Sample {
                    t,
                    reference: Some(body(None, x, 0.0, 0.0, None)),
                    subjects: if near {
                        vec![body(None, x + 0.5, 0.0, 0.0, Some([0.0, 0.0]))]
                    } else {
                        vec![]
                    },
                    found: u32::from(near),
                }
            })
            .collect();
        let m = metrics_(&samples).unwrap();
        // 51 exposed steps from t = 4.9 (2 m pushed, 0.31 m of jitter) less 51 times the calm steps' 1 cm floor: the
        // floor is taken from the 20 pushing steps too, whose jitter cancels out.
        assert!((m.push_m.unwrap() - 1.8).abs() < 1e-9, "{m:?}");
        assert_eq!(m.push_chains, 1, "{m:?}");
        assert_eq!(m.push_floor, Some("calm steps"));
        // Finding 81: crowded throughout, no calm step: the floor is the median step, the 1 cm jitter (30 of the 50
        // steps from t = 5; the 20 pushing ones are 9 and 11 cm): 2.3 m of path less 50 x 1 cm.
        let crowded: Vec<Sample> = samples.into_iter().filter(|s| s.t >= 5.0).collect();
        let m = metrics_(&crowded).unwrap();
        assert_eq!(m.push_floor, Some("the median step"));
        assert!((m.push_m.unwrap() - 1.8).abs() < 1e-6, "{m:?}");
        // A reference that never moved in a crowd: no push.
        let still: Vec<Sample> = crowded
            .iter()
            .map(|s| Sample {
                reference: Some(body(None, 0.0, 0.0, 0.0, None)),
                ..s.clone()
            })
            .collect();
        assert_eq!(metrics_(&still).unwrap().push_m, Some(0.0));
    }

    #[test]
    fn tracks_follow_ids_then_the_nearest_within_the_gate() {
        // Two walkers crossing paths keep their ids; without ids, the nearest within the gate.
        let a = walker(Some(1), 0.3);
        let b = |t: f64| body(Some(2), 5.0 - t, 0.3, 90.0, Some([-1.0, 0.0]));
        let samples = trace(10.0, |t| vec![a(t), b(t)]);
        let of = tracks(&samples);
        assert!(
            of.iter().all(|k| k == &vec![0, 1]),
            "ids keep the tracks through the crossing"
        );
        let m = metrics_(&samples).unwrap();
        assert_eq!(m.passes, 2);
        // A walker that vanishes for 2 s comes back as a new track: two passes.
        let gap = |t: f64| {
            if (4.0..6.0).contains(&t) {
                vec![]
            } else {
                vec![walker(None, 0.5)(t)]
            }
        };
        assert_eq!(metrics_(&trace(10.0, gap)).unwrap().passes, 2);
    }

    #[test]
    fn verdicts_follow_measure_ab_s_rule_per_metric() {
        let base = metrics_(&trace(10.0, |t| {
            (0..12)
                .map(|k| walker(None, 0.5 + k as f64 * 3.0)(t))
                .collect()
        }))
        .unwrap();
        let m = |contacts: f64, passes: u32| Metrics {
            contacts_per_min: contacts,
            passes,
            ..base.clone()
        };
        let (a1, a2, b1, b2) = (m(6.0, 12), m(6.5, 12), m(1.0, 12), m(1.4, 12));
        let v = verdicts(&[("on", &a1), ("off", &b1), ("on", &a2), ("off", &b2)]);
        let contacts = v.iter().find(|v| v.metric == "contacts_per_min").unwrap();
        assert_eq!(contacts.verdict, "outside drift", "{contacts:?}");
        assert!((contacts.diff.unwrap() - (1.2 - 6.25)).abs() < 1e-9);
        assert_eq!(contacts.drift["on"], 0.5);
        let crowd = v.iter().find(|v| v.metric == "crowd_mean").unwrap();
        assert_eq!(crowd.verdict, "inside drift", "same crowd: no difference");
        // A window with 9 passes: no verdict for the event metrics, still one for the exposure.
        let few = m(1.0, 9);
        let v = verdicts(&[("on", &a1), ("off", &few), ("on", &a2), ("off", &b2)]);
        assert_eq!(v[0].verdict, "too few events");
        assert_ne!(
            v.iter().find(|v| v.metric == "passes").unwrap().verdict,
            "too few events"
        );
    }

    #[test]
    fn samples_parse_world_actors_and_world_get_and_take_out_an_id_reference() {
        let actors = json!({"found": 3, "actors": [
            {"id": 5, "pos": [1, 2, 3], "heading": 10, "velocity": [0.5, 0, 0], "combat": false, "hostile": true,
             "guard": false, "at_work": false, "action_point": false, "scene": false, "entity_cut": false, "actor": true,
             "entity": "a \"b\""},
            {"pos": [4, 5, 6], "heading": 0, "velocity": null, "entity": "cart", "actor": false},
        ]});
        let world = json!({"player": {"pos": [0, 0, 0], "heading": 90}});
        let s = Sample::new(0.5, &actors, Some(&world), &Reference::Player).unwrap();
        assert_eq!(
            (
                s.found,
                s.subjects.len(),
                s.reference.as_ref().unwrap().heading
            ),
            (3, 2, 90.0)
        );
        assert_eq!((s.subjects[0].flags, s.subjects[1].flags), (2 | 64, 256));
        let by_id = Sample::new(0.5, &actors, None, &Reference::Id(5)).unwrap();
        assert_eq!(
            (by_id.reference.unwrap().pos, by_id.subjects.len()),
            ([1.0, 2.0, 3.0], 1)
        );
        assert_eq!(
            Sample::new(0.5, &actors, None, &Reference::Id(9))
                .unwrap()
                .reference,
            None
        );
        let text = csv(&[s]);
        assert!(
            text.lines()
                .nth(1)
                .unwrap()
                .starts_with("0.5,0,reference,,,0,0,0,90,"),
            "{text}"
        );
        assert!(
            text.contains(",0.5,0,0,2.23606797749979,66,\"a \"\"b\"\"\""),
            "{text}"
        );
    }

    #[test]
    fn sampler_parses_its_defaults_caps_and_names() {
        let s = Sampler::parse(&json!({}), 100.0, 50).unwrap();
        assert_eq!(
            s.to_string(),
            "actor_trace 10 Hz r8 max20 ref player contact 0.75"
        );
        assert!(!s.needs_ids());
        let s = Sampler::parse(&json!({"hz": 5, "radius": 12.5, "max": 50, "tag": "q1", "class": "CNewNPC", "reference_id": -3, "contact_m": 0.9}), 100.0, 50).unwrap();
        assert_eq!(
            s.to_string(),
            "actor_trace 5 Hz r12.5 max50 tag q1 class CNewNPC ref id -3 contact 0.9"
        );
        assert!(s.needs_ids());
        for bad in [
            json!({"hz": 11}),
            json!({"hz": 0}),
            json!({"radius": 101}),
            json!({"max": 51}),
            json!({"max": 2.5}),
            json!({"tag": "a b"}),
            json!({"class": ""}),
            json!({"reference_id": "player"}),
            json!({"reference_id": 1.5}),
            json!({"contact_m": 0.05}),
            json!({"contact_m": 3.5}),
            json!({"every": 1}),
        ] {
            assert!(Sampler::parse(&bad, 100.0, 50).is_err(), "{bad}");
        }
    }
}
