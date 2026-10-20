//! Placement for quen's scene tools (`game_frame`, `game_place`): the photo-mode camera pose that frames a subject from
//! one side, and the yaw that faces a point. The game's conventions, as quen's camera and trace tools use them: a
//! camera's or a body's forward is (-sin yaw, cos yaw) in world xy, so +yaw turns left; pitch is the angle above the
//! horizon (sweep.rs's poses).

use crate::sweep::Pose;

/// A side of the subject: degrees from its forward, +90 its left (+yaw turns left).
pub const SIDES: [(&str, f64); 4] = [
    ("front", 0.0),
    ("left", 90.0),
    ("back", 180.0),
    ("right", -90.0),
];

/// A side's degrees by name.
pub fn side(name: &str) -> Option<f64> {
    SIDES.iter().find(|(n, _)| *n == name).map(|(_, d)| *d)
}

/// An angle in degrees, in (-180, 180].
pub fn wrap(deg: f64) -> f64 {
    let a = deg - 360.0 * (deg / 360.0).round();
    if a <= -180.0 { a + 360.0 } else { a }
}

/// The unit forward of `yaw` degrees, in world xy.
pub fn forward(yaw: f64) -> [f64; 2] {
    let r = yaw.to_radians();
    [-r.sin(), r.cos()]
}

/// The yaw whose forward points from `from` to `to` in xy; none when they are within a millimetre.
pub fn facing(from: [f64; 2], to: [f64; 2]) -> Option<f64> {
    let (dx, dy) = (to[0] - from[0], to[1] - from[1]);
    (dx.hypot(dy) >= 1e-3).then(|| wrap((-dx).atan2(dy).to_degrees()))
}

/// How a subject is framed: from `side` degrees off its forward, `distance` metres away from the aim point in xy,
/// `height` metres above it, with `fov` degrees.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Framing {
    pub side: f64,
    pub distance: f64,
    pub height: f64,
    pub fov: f64,
}

/// The camera pose that frames `aim` (a point on a subject whose heading is `heading`) as `f` says: the camera
/// `f.distance` out along the subject's forward turned by `f.side`, `f.height` above the aim point, looking at it, no
/// roll.
pub fn frame_pose(aim: [f64; 3], heading: f64, f: &Framing) -> Pose {
    let out = forward(heading + f.side);
    let pos = [
        aim[0] + f.distance * out[0],
        aim[1] + f.distance * out[1],
        aim[2] + f.height,
    ];
    let pitch = (-f.height).atan2(f.distance).to_degrees();
    Pose {
        pos,
        rot: [pitch, wrap(heading + f.side + 180.0), 0.0],
        fov: f.fov,
    }
}

/// The unit vector a pose looks along, in world xyz.
pub fn look(p: &Pose) -> [f64; 3] {
    let (pitch, [x, y]) = (p.rot[0].to_radians(), forward(p.rot[1]));
    [x * pitch.cos(), y * pitch.cos(), pitch.sin()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    /// The direction from `p`'s position to `aim`, unit.
    fn towards(p: &Pose, aim: [f64; 3]) -> [f64; 3] {
        let d = [aim[0] - p.pos[0], aim[1] - p.pos[1], aim[2] - p.pos[2]];
        let n = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        d.map(|c| c / n)
    }

    #[test]
    fn sides_frame_the_subject_from_where_they_say() {
        let aim = [604.87, 2089.32, 32.6];
        // A subject facing +y (heading 0): its front is +y, its left -x, its back -y, its right +x.
        let want = [
            ("front", [0.0, 3.0], 180.0),
            ("left", [-3.0, 0.0], -90.0),
            ("back", [0.0, -3.0], 0.0),
            ("right", [3.0, 0.0], 90.0),
        ];
        for (name, offset, yaw) in want {
            let f = Framing {
                side: side(name).unwrap(),
                distance: 3.0,
                height: 0.0,
                fov: 40.0,
            };
            let p = frame_pose(aim, 0.0, &f);
            assert!(
                close(p.pos[0] - aim[0], offset[0]) && close(p.pos[1] - aim[1], offset[1]),
                "{name}: {p:?}"
            );
            assert!(close(p.rot[1], yaw), "{name}: {p:?}");
            assert_eq!((p.rot[0], p.rot[2], p.fov), (0.0, 0.0, 40.0));
            let (look, to) = (look(&p), towards(&p, aim));
            assert!(
                (0..3).all(|c| close(look[c], to[c])),
                "{name}: looks {look:?}, aim {to:?}"
            );
        }
    }

    /// Any heading, side and height: the camera stands the asked distance off in xy and the height above, and looks
    /// straight at the aim point.
    #[test]
    fn a_framing_looks_at_its_aim_from_any_heading() {
        let aim = [-12.5, 40.25, 3.0];
        for heading in [-179.0, -127.3, -45.0, 0.0, 33.3, 90.0, 180.0] {
            for side in [0.0, 90.0, 180.0, -90.0, 30.0, -150.0] {
                for (distance, height) in [(3.0, 0.0), (1.5, 0.8), (8.0, -1.0)] {
                    let f = Framing {
                        side,
                        distance,
                        height,
                        fov: 50.0,
                    };
                    let p = frame_pose(aim, heading, &f);
                    let d = [p.pos[0] - aim[0], p.pos[1] - aim[1]];
                    assert!(close(d[0].hypot(d[1]), distance), "{heading} {side}: {p:?}");
                    assert!(close(p.pos[2] - aim[2], height));
                    let (look, to) = (look(&p), towards(&p, aim));
                    assert!(
                        (0..3).all(|c| close(look[c], to[c])),
                        "{heading} {side} {height}: {look:?} {to:?}"
                    );
                    // The camera stands on the asked side: along the subject's forward turned by `side`.
                    let out = forward(heading + side);
                    assert!(close(d[0] * out[0] + d[1] * out[1], distance));
                    assert!(p.rot[1] > -180.0 && p.rot[1] <= 180.0);
                }
            }
        }
        // Above the aim it looks down, below it up.
        let above = frame_pose(
            aim,
            0.0,
            &Framing {
                side: 0.0,
                distance: 2.0,
                height: 2.0,
                fov: 40.0,
            },
        );
        assert!(close(above.rot[0], -45.0), "{above:?}");
    }

    #[test]
    fn facing_points_the_forward_at_the_target() {
        assert_eq!(facing([0.0, 0.0], [0.0, 5.0]), Some(0.0));
        assert!(close(facing([0.0, 0.0], [-5.0, 0.0]).unwrap(), 90.0));
        assert!(close(facing([0.0, 0.0], [5.0, 0.0]).unwrap(), -90.0));
        assert!(close(facing([1.0, 1.0], [1.0, -4.0]).unwrap(), 180.0));
        assert_eq!(facing([1.0, 1.0], [1.0, 1.0005]), None);
        for yaw in [-170.0, -60.0, 0.0, 45.0, 135.0] {
            let [x, y] = forward(yaw);
            assert!(close(
                facing([10.0, 10.0], [10.0 + 3.0 * x, 10.0 + 3.0 * y]).unwrap(),
                yaw
            ));
        }
        assert_eq!(wrap(-180.0), 180.0);
        assert_eq!(wrap(540.0), 180.0);
        assert!(close(wrap(-190.0), 170.0));
        assert_eq!(side("up"), None);
    }
}
