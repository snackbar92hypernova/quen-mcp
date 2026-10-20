//! measure_ab's drift guard (finding 73): in boot 181 the world changed by itself about 1.5 min after photo mode opened
//! (48 to 60 fps), 4 s after a toggle, and an A/B straddling it gave a meaningless verdict. Two checks: window 1's
//! frame time against its state's later windows, and a fingerprint of the world, read before and after each window
//! (never inside one), between the reads of one state.

use serde::Serialize;
use serde_json::Value;

use crate::trace::{Body, vec3};

/// The radius of the fingerprint's actor count, metres (world_actors' default).
pub const RADIUS_M: f32 = 30.0;
/// How far window 1 may sit from its state's later windows, in times the later windows' drift, before
/// [`first_window`] says so.
// ponytail: a guess (boot 181's step was far beyond it); tune once live A/Bs show how far a settled window 1 strays.
pub const FIRST_FACTOR: f64 = 3.0;
/// What counts as a change between two fingerprints of one state: the player or the camera moved this far (metres) or
/// turned this far (degrees); the actors within [`RADIUS_M`] changed by 3 or more and by over half the larger count.
// ponytail: guesses, wide on purpose (boot 176's crowd within 8 m went 12, 14, 7 in one state); tune on live runs.
const MOVED_M: f64 = 1.0;
const TURNED_DEG: f64 = 10.0;

/// The world as cheap reads show it: world.get's player and weather, cam.get's pose and world.actors' count.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Fingerprint {
    pub player_pos: [f64; 3],
    /// Degrees.
    pub player_heading: f64,
    pub camera_pos: [f64; 3],
    /// [pitch, yaw, roll], degrees.
    pub camera_rot: [f64; 3],
    /// The alive actors within [`RADIUS_M`] of the player.
    pub actors: u64,
    pub weather: String,
}

impl Fingerprint {
    /// From the replies' data of world.get, cam.get and world.actors.
    pub fn new(world: &Value, camera: &Value, actors: &Value) -> Result<Fingerprint, String> {
        let player = Body::player(world)?;
        Ok(Fingerprint {
            player_pos: player.pos,
            player_heading: player.heading,
            camera_pos: vec3(&camera["pos"]).ok_or("cam.get without pos")?,
            camera_rot: vec3(&camera["rot"]).ok_or("cam.get without rot")?,
            actors: actors["found"]
                .as_u64()
                .ok_or("world.actors without found")?,
            weather: world["weather"]
                .as_str()
                .ok_or("world.get without weather")?
                .into(),
        })
    }

    /// How this differs from `from`, beyond the tolerances; empty when it doesn't.
    pub fn changes(&self, from: &Fingerprint) -> Vec<String> {
        let moved =
            |a: [f64; 3], b: [f64; 3]| (0..3).map(|i| (a[i] - b[i]).powi(2)).sum::<f64>().sqrt();
        let turned = |a: f64, b: f64| {
            let d = (a - b).rem_euclid(360.0);
            d.min(360.0 - d)
        };
        let camera_turned = (0..3)
            .map(|i| turned(self.camera_rot[i], from.camera_rot[i]))
            .fold(0.0, f64::max);
        let mut out = Vec::new();
        for (what, by, unit, max) in [
            (
                "the player moved",
                moved(self.player_pos, from.player_pos),
                "m",
                MOVED_M,
            ),
            (
                "the player turned",
                turned(self.player_heading, from.player_heading),
                "deg",
                TURNED_DEG,
            ),
            (
                "the camera moved",
                moved(self.camera_pos, from.camera_pos),
                "m",
                MOVED_M,
            ),
            ("the camera turned", camera_turned, "deg", TURNED_DEG),
        ] {
            if by > max {
                out.push(format!("{what} {by:.2} {unit}"));
            }
        }
        let (a, b) = (from.actors, self.actors);
        let d = a.abs_diff(b);
        if d >= 3 && 2 * d > a.max(b) {
            out.push(format!(
                "the actors within {RADIUS_M} m went from {a} to {b}"
            ));
        }
        if self.weather != from.weather {
            out.push(format!(
                "the weather went from {} to {}",
                from.weather, self.weather
            ));
        }
        out
    }
}

/// Per state, the first read whose fingerprint differs from that state's first read, and how. `reads` in order, each
/// with its state's label and its name ("window 3 (A) before").
pub fn world_changes(reads: &[(&str, String, &Fingerprint)]) -> Vec<String> {
    let mut labels: Vec<&str> = Vec::new();
    for (label, ..) in reads {
        if !labels.contains(label) {
            labels.push(label);
        }
    }
    let mut out = Vec::new();
    for label in labels {
        let mut of = reads.iter().filter(|(l, ..)| *l == label);
        let Some((_, first, from)) = of.next() else {
            continue;
        };
        if let Some((name, changes)) = of
            .map(|(_, name, fp)| (name, fp.changes(from)))
            .find(|(_, c)| !c.is_empty())
        {
            out.push(format!(
                "{name} differs from {first}: {}",
                changes.join(", ")
            ));
        }
    }
    out
}

/// Window 1's mean frame time against its state's later windows: `means`, the windows' labels and mean frame times in
/// order. Some when it differs from their mean by more than [`FIRST_FACTOR`] times the largest drift among the windows
/// after it (the spread of each state's later windows).
pub fn first_window(means: &[(&str, f64)]) -> Option<String> {
    let ((label, first), rest) = means.split_first()?;
    let of = |l: &str| -> Vec<f64> {
        rest.iter()
            .filter(|(o, _)| *o == l)
            .map(|(_, m)| *m)
            .collect()
    };
    let same = of(label);
    if same.is_empty() {
        return None;
    }
    let step = (first - same.iter().sum::<f64>() / same.len() as f64).abs();
    let spread = |v: Vec<f64>| {
        v.iter().copied().fold(f64::MIN, f64::max) - v.iter().copied().fold(f64::MAX, f64::min)
    };
    let later = rest.iter().map(|(l, _)| spread(of(l))).fold(0.0, f64::max);
    (step > FIRST_FACTOR * later).then(|| {
        format!(
            "window 1 ({label}) is {step:.2} ms from {label}'s later windows, over {FIRST_FACTOR} times the later \
             windows' drift ({later:.2} ms): the world may have changed after it"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fp(x: f64, yaw: f64, actors: u64, weather: &str) -> Fingerprint {
        let world = json!({"player": {"pos": [x, 0, 0], "heading": 90}, "weather": weather});
        let camera = json!({"camera": "photo", "pos": [x, -2, 1.8], "rot": [0, yaw, 0], "fov": 60});
        Fingerprint::new(&world, &camera, &json!({"found": actors, "actors": []})).unwrap()
    }

    #[test]
    fn a_fingerprint_needs_every_read() {
        let world = json!({"player": {"pos": [1, 2, 3], "heading": 4}, "weather": "WT_Clear"});
        let camera = json!({"pos": [0, 0, 0], "rot": [0, 0, 0]});
        let found = json!({"found": 7});
        assert_eq!(Fingerprint::new(&world, &camera, &found).unwrap().actors, 7);
        for (w, c, a) in [
            (
                json!({"player": {"pos": [1, 2, 3], "heading": 4}}),
                camera.clone(),
                found.clone(),
            ),
            (world.clone(), json!({"pos": [0, 0, 0]}), found.clone()),
            (world.clone(), camera.clone(), json!({"actors": []})),
            (
                json!({"weather": "WT_Clear"}),
                camera.clone(),
                found.clone(),
            ),
        ] {
            assert!(Fingerprint::new(&w, &c, &a).is_err(), "{w} {c} {a}");
        }
    }

    #[test]
    fn changes_beyond_the_tolerances_are_named() {
        let base = fp(0.0, 10.0, 12, "WT_Clear");
        assert!(
            fp(0.5, 15.0, 7, "WT_Clear").changes(&base).is_empty(),
            "within: crowds vary"
        );
        // Turns wrap at 360.
        assert!(fp(0.0, -350.0, 12, "WT_Clear").changes(&base).is_empty());
        let all = fp(2.0, 40.0, 4, "WT_Rain").changes(&base);
        assert_eq!(
            all,
            [
                "the player moved 2.00 m",
                "the camera moved 2.00 m",
                "the camera turned 30.00 deg",
                "the actors within 30 m went from 12 to 4",
                "the weather went from WT_Clear to WT_Rain"
            ]
        );
        assert!(
            fp(0.0, 10.0, 2, "WT_Clear")
                .changes(&fp(0.0, 10.0, 0, "WT_Clear"))
                .is_empty()
        );
    }

    #[test]
    fn the_first_differing_read_of_each_state_is_named() {
        let (a, moved, b) = (
            fp(0.0, 0.0, 10, "WT_Clear"),
            fp(5.0, 0.0, 10, "WT_Clear"),
            fp(0.0, 0.0, 30, "WT_Clear"),
        );
        let reads = [
            ("A", "window 1 (A) before".to_string(), &a),
            ("A", "window 1 (A) after".to_string(), &a),
            ("B", "window 2 (B) before".to_string(), &b),
            ("B", "window 2 (B) after".to_string(), &b),
            ("A", "window 3 (A) before".to_string(), &moved),
            ("A", "window 3 (A) after".to_string(), &a),
            ("B", "window 4 (B) before".to_string(), &b),
        ];
        // Another crowd in B than in A is the toggle's doing: only a change within one state counts.
        assert_eq!(
            world_changes(&reads),
            [
                "window 3 (A) before differs from window 1 (A) before: the player moved 5.00 m, the camera moved 5.00 m"
            ]
        );
        assert!(world_changes(&reads[..4]).is_empty());
    }

    #[test]
    fn window_1_far_from_the_rest_is_named() {
        // Boot 181's shape: the world changed after window 1, the later windows agree.
        let note = first_window(&[("A", 20.8), ("B", 16.7), ("A", 16.6), ("B", 16.75)]).unwrap();
        assert!(
            note.starts_with("window 1 (A) is 4.20 ms from A's later windows"),
            "{note}"
        );
        assert!(note.contains("(0.05 ms)"), "{note}");
        // Within three times the later drift, or no later window of its state: nothing.
        assert_eq!(
            first_window(&[("A", 16.7), ("B", 16.7), ("A", 16.6), ("B", 16.75)]),
            None
        );
        assert_eq!(
            first_window(&[("A", 20.0), ("B", 23.0), ("A", 20.0), ("B", 23.0)]),
            None
        );
        assert_eq!(first_window(&[("A", 20.0), ("B", 23.0), ("B", 23.0)]), None);
        assert_eq!(first_window(&[]), None);
    }
}
