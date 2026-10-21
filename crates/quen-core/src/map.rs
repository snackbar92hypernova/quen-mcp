//! world_map's maths (camera-independent scene reading, phase 1): a top-down grid of cells, each read by one
//! scene.ground point and classed by its navmesh, ground hit and water; the walkable mask's clearance (a Euclidean
//! distance transform), its chokepoints, connected regions and edges; heat layers binned from actor traces by trace.rs's
//! own rules; a street's width profile; walkers' look-ahead verdicts; and the picture, without text. World xy in metres,
//! +y up and +x right; yaw 0 faces +y, forward (−sin yaw, cos yaw) as scene.rs has it. Cells are row-major, row 0 at the
//! top (+y), column 0 at the left (−x).

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value, json};

use crate::scene::{facing, forward};
use crate::trace::{self, Body, Sample, median};

/// The most cells a map reads: 625 scene.ground calls of 16 points.
pub const MAX_CELLS: usize = 10_000;
/// A map's side, metres, at most.
pub const MAX_SIDE_M: f64 = 200.0;
/// The picture's pixels per cell.
pub const PX: usize = 8;
/// Covered: the ground ray's or the roof ray's hit this far above the navmesh z (a roof, an arch, an overhang).
pub const COVER_M: f64 = 2.2;
/// The roof ray: up from a walkable cell's navmesh z this far, where the ground ray (from at.z + z_range) didn't reach.
/// It starts on the navmesh, in the open, so a low cave's ceiling is hit from below, not from inside the rock. A cell
/// that would class water gets one up from its water surface ([`roofs`]).
pub const ROOF_UP_M: f64 = 6.0;
/// GetWaterDepth above this is the game's no-water value (10000): its scripts read it as 0, six of them with this test
/// (`btCondWaterDepth.ws`, `btTaskDiveCheck.ws`), three with == 10000 (`btTaskTeleport.ws`, the horse's
/// `exploration.ws`).
pub const NO_WATER_ABOVE: f64 = 1000.0;
/// Ledge: no navmesh, the ground hit within this of the reference.
pub const LEDGE_M: f64 = 0.3;
/// The narrowest chokepoints a map lists.
pub const CHOKEPOINTS: usize = 10;
/// The largest connected regions a result lists inline (the file has them all).
pub const COMPONENTS: usize = 10;
/// blockers: walkers move above this (trace.rs's moving), and another body blocks within this of the look-ahead line.
pub const WALKING_MPS: f64 = trace::MOVING_MPS;
pub const IN_THE_WAY_M: f64 = 0.6;

fn r(x: f64, digits: i32) -> f64 {
    let m = 10f64.powi(digits);
    (x * m).round() / m
}

fn dist(a: [f64; 2], b: [f64; 2]) -> f64 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

fn xy(p: [f64; 3]) -> [f64; 2] {
    [p[0], p[1]]
}

/// A map's frame: its centre `at` (z: where its points look), the cell's side, columns and rows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    pub at: [f64; 3],
    pub cell: f64,
    pub nx: usize,
    pub ny: usize,
}

impl Frame {
    /// `size` (w, h) metres around `at` in cells of `cell` metres; refused over [`MAX_CELLS`].
    pub fn new(at: [f64; 3], size: [f64; 2], cell: f64) -> Result<Frame, String> {
        let n = |s: f64| (s / cell).round().max(1.0) as usize;
        let (nx, ny) = (n(size[0]), n(size[1]));
        if nx * ny > MAX_CELLS {
            return Err(format!(
                "{nx} x {ny} cells: at most {MAX_CELLS} (a larger cell or a smaller size)"
            ));
        }
        Ok(Frame { at, cell, nx, ny })
    }

    /// A frame around `points` with a 2 m margin, its cell doubled until it holds at most [`MAX_CELLS`]: the picture
    /// of a profile or of walkers, without a grid read.
    pub fn around(points: &[[f64; 2]], z: f64, cell: f64) -> Frame {
        let lo = |k: usize| points.iter().map(|p| p[k]).fold(f64::MAX, f64::min);
        let hi = |k: usize| points.iter().map(|p| p[k]).fold(f64::MIN, f64::max);
        let (x0, x1, y0, y1) = match points {
            [] => (0.0, 0.0, 0.0, 0.0),
            _ => (lo(0), hi(0), lo(1), hi(1)),
        };
        let size = [x1 - x0 + 4.0, y1 - y0 + 4.0];
        let at = [(x0 + x1) / 2.0, (y0 + y1) / 2.0, z];
        let mut cell = cell;
        loop {
            if let Ok(f) = Frame::new(at, size, cell) {
                return f;
            }
            cell *= 2.0;
        }
    }

    pub fn cells(&self) -> usize {
        self.nx * self.ny
    }

    /// [x0, y0, x1, y1].
    pub fn bbox(&self) -> [f64; 4] {
        let (w, h) = (self.nx as f64 * self.cell, self.ny as f64 * self.cell);
        [
            self.at[0] - w / 2.0,
            self.at[1] - h / 2.0,
            self.at[0] + w / 2.0,
            self.at[1] + h / 2.0,
        ]
    }

    /// Cell `i`'s centre.
    pub fn centre(&self, i: usize) -> [f64; 2] {
        let b = self.bbox();
        let (c, row) = ((i % self.nx) as f64, (i / self.nx) as f64);
        [b[0] + (c + 0.5) * self.cell, b[3] - (row + 0.5) * self.cell]
    }

    /// The cell holding `p`, if the map does.
    pub fn cell_of(&self, p: [f64; 2]) -> Option<usize> {
        let b = self.bbox();
        let c = ((p[0] - b[0]) / self.cell).floor();
        let row = ((b[3] - p[1]) / self.cell).floor();
        let inside = c >= 0.0 && row >= 0.0 && c < self.nx as f64 && row < self.ny as f64;
        inside.then(|| row as usize * self.nx + c as usize)
    }

    /// Every cell's centre at the map's z, row-major: the scene.ground points, 16 a call.
    pub fn points(&self) -> Vec<[f64; 3]> {
        (0..self.cells())
            .map(|i| {
                let [x, y] = self.centre(i);
                [x, y, self.at[2]]
            })
            .collect()
    }
}

/// A GetWaterDepth read-back as a depth: none for null and for the game's no-water value ([`NO_WATER_ABOVE`]). 0 or
/// below is no water at the point (−28.7 on Novigrad's square, the 0.1.11 live check).
pub fn water_depth(v: &Value) -> Option<f64> {
    v.as_f64().filter(|d| *d <= NO_WATER_ABOVE)
}

/// One cell's scene.ground reading, and its roof ray's.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Reading {
    pub nav_z: Option<f64>,
    /// The circle test at nav_z; none without nav_z.
    pub free: Option<bool>,
    /// The ray down's hit z; none without a hit.
    pub top_z: Option<f64>,
    /// GetWaterDepth ([`water_depth`]): water when above 0.
    pub water_m: Option<f64>,
    /// GetWaterLevel: the water surface's z.
    pub water_z: Option<f64>,
    /// The roof ray's hit z ([`roofs`]); none without a hit or a ray.
    pub roof_z: Option<f64>,
    /// The roof ray's hit faced up: it met a surface from below, as the terrain's underside reads.
    pub roof_up: bool,
}

impl Reading {
    /// One point of a scene.ground read-back.
    pub fn from_ground(v: &Value) -> Reading {
        Reading {
            nav_z: v["nav_z"].as_f64(),
            free: v["free"].as_bool(),
            top_z: v["ground"]["pos"][2].as_f64(),
            water_m: water_depth(&v["water_depth"]),
            water_z: v["water_level"].as_f64(),
            ..Reading::default()
        }
    }

    /// Its roof ray's scene.ray read-back ([`roofs`]): the hit's z, and whether the hit's normal faced up.
    pub fn roof(&mut self, v: &Value) {
        self.roof_z = v["pos"][2].as_f64().filter(|_| v["hit"] == true);
        self.roof_up = self.roof_z.is_some() && v["normal"][2].as_f64().is_some_and(|n| n > 0.0);
    }
}

/// Water by the ground reading: no navmesh, a water depth above 0 and its surface at or above the ground hit, or within
/// the band (the map's z ± `z_range`) with no hit. A deck or ground hit above the water wins (St. Gregory's Bridge, the
/// 0.1.15 live check: the canal 31 m below).
fn wet(frame: &Frame, z_range: f64, r: &Reading) -> bool {
    r.nav_z.is_none()
        && r.water_m.is_some_and(|d| d > 0.0)
        && r.water_z.is_some_and(|w| match r.top_z {
            Some(top) => w >= top,
            None => (w - frame.at[2]).abs() <= z_range,
        })
}

/// The roof rays a grid's readings need, [`ROOF_UP_M`] up: per walkable cell (navmesh, a free circle) not covered by
/// its ground ray, from its navmesh z, unless the ground ray started that high already; per cell that would class
/// water ([`wet`]), from its water surface. Cell index, from, to.
pub fn roofs(
    frame: &Frame,
    z_range: f64,
    cells: &[Option<Reading>],
) -> Vec<(usize, [f64; 3], [f64; 3])> {
    let top = frame.at[2] + z_range;
    cells
        .iter()
        .enumerate()
        .filter_map(|(i, c)| {
            let c = c.as_ref()?;
            let z = match c.nav_z {
                Some(z) => {
                    let covered = c.top_z.is_some_and(|t| t > z + COVER_M);
                    (c.free == Some(true) && !covered && top < z + ROOF_UP_M).then_some(z)?
                }
                None => c.water_z.filter(|_| wet(frame, z_range, c))?,
            };
            let [x, y] = frame.centre(i);
            Some((i, [x, y, z], [x, y, z + ROOF_UP_M]))
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// Navmesh, the body's circle free there.
    Walkable,
    /// Navmesh, the circle not free.
    Tight,
    /// Walkable under something: the ground or roof ray hit over [`COVER_M`] above the navmesh z.
    Covered,
    /// No navmesh, a water depth above 0 and its surface at or above the ground hit (with no hit: within the band), and
    /// the roof ray up from that surface not stopped by a surface facing up: water under the ground (beside the Velen
    /// cellar, the 0.1.16 live check) is classed by its ground hit, as if it had none.
    Water,
    /// No navmesh, a hit in the band away from the reference: its height is the hit's z over it.
    Obstacle,
    /// No navmesh and no hit in the band: inside geometry, or a drop below it.
    Tall,
    /// No navmesh, a hit within [`LEDGE_M`] of the reference.
    Ledge,
    /// Not read: the call stopped first.
    Unread,
}

pub const CLASSES: [Class; 8] = [
    Class::Walkable,
    Class::Tight,
    Class::Covered,
    Class::Water,
    Class::Obstacle,
    Class::Tall,
    Class::Ledge,
    Class::Unread,
];

impl Class {
    pub fn name(self) -> &'static str {
        match self {
            Class::Walkable => "walkable",
            Class::Tight => "tight",
            Class::Covered => "covered",
            Class::Water => "water",
            Class::Obstacle => "obstacle",
            Class::Tall => "tall",
            Class::Ledge => "ledge",
            Class::Unread => "unread",
        }
    }

    /// In the walkable mask: navmesh and a free circle.
    pub fn walkable(self) -> bool {
        matches!(self, Class::Walkable | Class::Covered)
    }

    fn index(self) -> usize {
        CLASSES.iter().position(|c| *c == self).unwrap_or(0)
    }
}

/// The cells' classes, from their readings (none: unread).
#[derive(Debug, Clone, PartialEq)]
pub struct Classified {
    pub class: Vec<Class>,
    /// The walking level: the median nav_z of the walkable cells.
    pub z_ref: Option<f64>,
    /// What heights are measured from: the median ground hit under the uncovered walkable cells (boot 190: nav_z ran
    /// 0.4 to 1.3 m above it), else `z_ref`, else the map's z.
    pub ground_ref: f64,
    /// An obstacle's or a ledge's hit over `ground_ref`.
    pub height: Vec<Option<f64>>,
}

/// The cells' classes; the band is the map's z ± `z_range`.
pub fn classify(frame: &Frame, z_range: f64, cells: &[Option<Reading>]) -> Classified {
    let nav = |r: &Reading| {
        r.nav_z.map(|z| {
            let over = |hit: Option<f64>| hit.is_some_and(|h| h > z + COVER_M);
            match r.free {
                Some(true) if over(r.top_z) || over(r.roof_z) => Class::Covered,
                Some(true) => Class::Walkable,
                _ => Class::Tight,
            }
        })
    };
    let walkable = |want: fn(Class) -> bool| {
        cells
            .iter()
            .flatten()
            .filter(move |r| nav(r).is_some_and(want))
    };
    let z_ref = median(walkable(Class::walkable).filter_map(|r| r.nav_z).collect());
    let ground = median(
        walkable(|c| c == Class::Walkable)
            .filter_map(|r| r.top_z)
            .collect(),
    );
    let ground_ref = ground.or(z_ref).unwrap_or(frame.at[2]);
    let mut height = vec![None; cells.len()];
    let class = cells
        .iter()
        .enumerate()
        .map(|(i, cell)| {
            let Some(r) = cell else { return Class::Unread };
            if let Some(c) = nav(r) {
                return c;
            }
            if wet(frame, z_range, r) && !r.roof_up {
                return Class::Water;
            }
            let Some(top) = r.top_z else {
                return Class::Tall;
            };
            height[i] = Some(top - ground_ref);
            match (top - ground_ref).abs() <= LEDGE_M {
                true => Class::Ledge,
                false => Class::Obstacle,
            }
        })
        .collect();
    Classified {
        class,
        z_ref,
        ground_ref,
        height,
    }
}

/// The 1-D squared distance transform (Felzenszwalb and Huttenlocher): per index x, the least (x − q)² + f[q] over the
/// finite f[q]; infinite where there is none.
fn dt1(f: &[f64]) -> Vec<f64> {
    let (mut v, mut z): (Vec<usize>, Vec<f64>) = (Vec::new(), Vec::new());
    let at = |p: usize| f[p] + (p * p) as f64;
    for q in (0..f.len()).filter(|&q| f[q].is_finite()) {
        while let Some(&p) = v.last() {
            let s = (at(q) - at(p)) / (2.0 * (q - p) as f64);
            if s > z[z.len() - 1] {
                v.push(q);
                z.push(s);
                break;
            }
            v.pop();
            z.pop();
        }
        if v.is_empty() {
            v.push(q);
            z.push(f64::NEG_INFINITY);
        }
    }
    let mut out = vec![f64::INFINITY; f.len()];
    let mut k = 0;
    for (x, o) in out.iter_mut().enumerate().filter(|_| !v.is_empty()) {
        while k + 1 < v.len() && z[k + 1] < x as f64 {
            k += 1;
        }
        let d = x as f64 - v[k] as f64;
        *o = d * d + f[v[k]];
    }
    out
}

/// Each walkable cell's clearance, metres: from its centre to the nearest edge of a non-walkable cell (the Euclidean
/// distance transform to the nearest non-walkable centre, less half a cell). None off the mask, or with no
/// non-walkable cell in the map: what lies beyond its edge is unseen.
pub fn clearance(frame: &Frame, mask: &[bool]) -> Vec<Option<f64>> {
    let (nx, ny) = (frame.nx, frame.ny);
    let mut g: Vec<f64> = mask
        .iter()
        .map(|&m| if m { f64::INFINITY } else { 0.0 })
        .collect();
    for x in 0..nx {
        let col: Vec<f64> = (0..ny).map(|y| g[y * nx + x]).collect();
        for (y, d) in dt1(&col).into_iter().enumerate() {
            g[y * nx + x] = d;
        }
    }
    let mut out = vec![None; nx * ny];
    for y in 0..ny {
        let row = dt1(&g[y * nx..(y + 1) * nx]);
        for (x, d) in row.into_iter().enumerate() {
            let i = y * nx + x;
            if mask[i] && d.is_finite() {
                out[i] = Some((d.sqrt() - 0.5) * frame.cell);
            }
        }
    }
    out
}

/// A narrowing of the walkable mask: where its ridge's clearance is least, with wider ridge on both sides.
#[derive(Debug, Clone, PartialEq)]
pub struct Chokepoint {
    pub at: [f64; 2],
    /// Twice the clearance: within a cell of the passage's width.
    pub width_m: f64,
    /// The passage's direction as a yaw, −90 to 90 (yaw 0 runs along y).
    pub dir_deg: f64,
}

impl Chokepoint {
    pub fn json(&self) -> Value {
        json!({"x": r(self.at[0], 3), "y": r(self.at[1], 3), "width_m": r(self.width_m, 2),
               "dir_deg": r(self.dir_deg, 1)})
    }
}

const NEIGHBOURS: [(isize, isize); 8] = [
    (-1, -1),
    (0, -1),
    (1, -1),
    (-1, 0),
    (1, 0),
    (-1, 1),
    (0, 1),
    (1, 1),
];

/// The cell `d` away from `i`, if in the map.
fn step(frame: &Frame, i: usize, d: (isize, isize)) -> Option<usize> {
    let (x, y) = ((i % frame.nx) as isize + d.0, (i / frame.nx) as isize + d.1);
    let inside = x >= 0 && y >= 0 && x < frame.nx as isize && y < frame.ny as isize;
    inside.then(|| y as usize * frame.nx + x as usize)
}

/// The `n` narrowest chokepoints. The ridge: walkable cells whose clearance is a local maximum across some axis
/// (horizontal, vertical, either diagonal), above one of the two neighbours; a neighbour off the map counts as wider,
/// one off the mask as narrower. A chokepoint: a connected run of ridge cells of equal clearance whose ridge neighbours
/// are all wider and lie on opposite sides of it (more than 90° apart), so a dead end isn't one.
pub fn chokepoints(frame: &Frame, clearance: &[Option<f64>], n: usize) -> Vec<Chokepoint> {
    const EPS: f64 = 1e-9;
    let c = |i: usize, d: (isize, isize)| match step(frame, i, d) {
        None => f64::INFINITY,
        Some(j) => clearance[j].unwrap_or(f64::NEG_INFINITY),
    };
    let ridge: Vec<bool> = (0..clearance.len())
        .map(|i| {
            clearance[i].is_some_and(|v| {
                [(1, 0), (0, 1), (1, 1), (1, -1)].iter().any(|&(dx, dy)| {
                    let (a, b) = (c(i, (dx, dy)), c(i, (-dx, -dy)));
                    // A diagonal step skips a diagonal band's centre line: there the two sides tie off it.
                    let diagonal = dx != 0 && dy != 0;
                    v >= a && v >= b && (v > a || v > b) && (!diagonal || (v > a && v > b))
                })
            })
        })
        .collect();
    let value = |i: usize| clearance[i].unwrap_or(f64::NEG_INFINITY);
    let mut seen = vec![false; clearance.len()];
    let mut out = Vec::new();
    for start in 0..clearance.len() {
        if !ridge[start] || seen[start] {
            continue;
        }
        let v = value(start);
        let (mut run, mut todo, mut wider) = (Vec::new(), vec![start], BTreeSet::new());
        let mut lower = false;
        seen[start] = true;
        while let Some(i) = todo.pop() {
            run.push(i);
            for j in NEIGHBOURS
                .iter()
                .filter_map(|&d| step(frame, i, d))
                .filter(|&j| ridge[j])
            {
                if (value(j) - v).abs() < EPS {
                    if !seen[j] {
                        seen[j] = true;
                        todo.push(j);
                    }
                } else if value(j) > v {
                    wider.insert(j);
                } else {
                    lower = true;
                }
            }
        }
        if lower || wider.len() < 2 {
            continue;
        }
        let centroid = |cells: &[usize]| {
            let sum = cells
                .iter()
                .map(|&i| frame.centre(i))
                .fold([0.0; 2], |a, p| [a[0] + p[0], a[1] + p[1]]);
            [sum[0] / cells.len() as f64, sum[1] / cells.len() as f64]
        };
        let mid = centroid(&run);
        let out_of = |j: usize| {
            let p = frame.centre(j);
            let (dx, dy) = (p[0] - mid[0], p[1] - mid[1]);
            let l = dx.hypot(dy).max(EPS);
            [dx / l, dy / l]
        };
        let wider: Vec<usize> = wider.into_iter().collect();
        let mut best: Option<(f64, usize, usize)> = None;
        for (k, &a) in wider.iter().enumerate() {
            for &b in &wider[k + 1..] {
                let (u, w) = (out_of(a), out_of(b));
                let cos = u[0] * w[0] + u[1] * w[1];
                if best.is_none_or(|(c, _, _)| cos < c) {
                    best = Some((cos, a, b));
                }
            }
        }
        let Some((_, _, b)) = best.filter(|(cos, _, _)| *cos < 0.0) else {
            continue;
        };
        let at = run
            .iter()
            .map(|&i| frame.centre(i))
            .min_by(|p, q| dist(*p, mid).total_cmp(&dist(*q, mid)))
            .unwrap_or(mid);
        // The passage runs between the wider ridge on either side: those sides' centroids, split along a to b.
        let axis = out_of(b);
        let side = |sign: f64| -> Vec<usize> {
            let on =
                |j: &&usize| sign * (out_of(**j)[0] * axis[0] + out_of(**j)[1] * axis[1]) > 0.0;
            wider.iter().filter(on).copied().collect()
        };
        let yaw = facing(centroid(&side(-1.0)), centroid(&side(1.0))).unwrap_or(0.0);
        let dir_deg = match yaw {
            y if y > 90.0 => y - 180.0,
            y if y <= -90.0 => y + 180.0,
            y => y,
        };
        out.push(Chokepoint {
            at,
            width_m: 2.0 * v,
            dir_deg,
        });
    }
    out.sort_by(|a, b| {
        (a.width_m, a.at[1], a.at[0])
            .partial_cmp(&(b.width_m, b.at[1], b.at[0]))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out.truncate(n);
    out
}

/// A connected walkable region (4-connected: a body doesn't pass between two corners).
#[derive(Debug, Clone, PartialEq)]
pub struct Component {
    pub cells: usize,
    pub area_m2: f64,
    /// Its centroid.
    pub at: [f64; 2],
}

impl Component {
    pub fn json(&self) -> Value {
        json!({"cells": self.cells, "area_m2": r(self.area_m2, 2), "x": r(self.at[0], 2), "y": r(self.at[1], 2)})
    }
}

/// The mask's connected regions, the largest first.
pub fn components(frame: &Frame, mask: &[bool]) -> Vec<Component> {
    let mut seen = vec![false; mask.len()];
    let mut out = Vec::new();
    for start in 0..mask.len() {
        if !mask[start] || seen[start] {
            continue;
        }
        seen[start] = true;
        let (mut todo, mut n, mut sum) = (vec![start], 0usize, [0.0; 2]);
        while let Some(i) = todo.pop() {
            let p = frame.centre(i);
            (n, sum) = (n + 1, [sum[0] + p[0], sum[1] + p[1]]);
            for j in [(0, -1), (-1, 0), (1, 0), (0, 1)]
                .iter()
                .filter_map(|&d| step(frame, i, d))
            {
                if mask[j] && !seen[j] {
                    seen[j] = true;
                    todo.push(j);
                }
            }
        }
        out.push(Component {
            cells: n,
            area_m2: n as f64 * frame.cell * frame.cell,
            at: [sum[0] / n as f64, sum[1] / n as f64],
        });
    }
    out.sort_by(|a, b| b.cells.cmp(&a.cells));
    out
}

/// The mask's boundary as polylines in metres (marching squares over the cell centres, a saddle split as 4-connected
/// regions are), straight runs merged; a boundary that leaves the map ends at its outer centres.
pub fn edges(frame: &Frame, mask: &[bool]) -> Vec<Vec<[f64; 2]>> {
    // Points in half cells: cell (c, r)'s centre is (2c, 2r).
    type P = (i64, i64);
    let mut links: BTreeMap<P, Vec<P>> = BTreeMap::new();
    let (nx, ny) = (frame.nx, frame.ny);
    for row in 0..ny.saturating_sub(1) {
        for c in 0..nx.saturating_sub(1) {
            let m = |dc: usize, dr: usize| mask[(row + dr) * nx + c + dc];
            let case =
                (m(0, 0) as u8) << 3 | (m(1, 0) as u8) << 2 | (m(1, 1) as u8) << 1 | m(0, 1) as u8;
            let (x, y) = (2 * c as i64, 2 * row as i64);
            let (t, rt, b, l) = ((x + 1, y), (x + 2, y + 1), (x + 1, y + 2), (x, y + 1));
            let segments: &[(P, P)] = match case {
                1 | 14 => &[(l, b)],
                2 | 13 => &[(b, rt)],
                3 | 12 => &[(l, rt)],
                4 | 11 => &[(t, rt)],
                5 => &[(l, b), (t, rt)],
                6 | 9 => &[(t, b)],
                7 | 8 => &[(t, l)],
                10 => &[(t, l), (b, rt)],
                _ => &[],
            };
            for &(p, q) in segments {
                links.entry(p).or_default().push(q);
                links.entry(q).or_default().push(p);
            }
        }
    }
    let world = |(k, l): P| {
        let b = frame.bbox();
        [
            r(b[0] + (k as f64 / 2.0 + 0.5) * frame.cell, 3),
            r(b[3] - (l as f64 / 2.0 + 0.5) * frame.cell, 3),
        ]
    };
    let mut used: BTreeSet<(P, P)> = BTreeSet::new();
    let mut out = Vec::new();
    // Open chains from their ends first, then the closed loops.
    let starts: Vec<P> = links
        .iter()
        .filter(|(_, n)| n.len() == 1)
        .map(|(p, _)| *p)
        .chain(links.keys().copied())
        .collect();
    for start in starts {
        let mut chain = vec![start];
        let mut at = start;
        while let Some(&next) = links[&at]
            .iter()
            .find(|&&q| !used.contains(&(at.min(q), at.max(q))))
        {
            used.insert((at.min(next), at.max(next)));
            chain.push(next);
            at = next;
        }
        if chain.len() < 2 {
            continue;
        }
        // Straight runs merged: keep a point where the direction turns.
        let mut kept = vec![chain[0]];
        for w in chain.windows(3) {
            let (a, b, c) = (w[0], w[1], w[2]);
            if (b.0 - a.0) * (c.1 - b.1) != (b.1 - a.1) * (c.0 - b.0) {
                kept.push(b);
            }
        }
        kept.push(chain[chain.len() - 1]);
        out.push(kept.into_iter().map(world).collect());
    }
    out
}

/// A polyline's length.
pub fn length(line: &[[f64; 2]]) -> f64 {
    line.windows(2).map(|w| dist(w[0], w[1])).sum()
}

/// A trace for the heat layers: its samples, the contact distance and radius of its sampler (the radius none when
/// unknown), and the captures id it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct Trace {
    pub id: i64,
    pub samples: Vec<Sample>,
    pub contact: f64,
    pub radius: Option<f64>,
}

/// The radius and contact distance a trace's sampler name gives (`actor_trace 10 Hz r8 max20 ... contact 0.75`, as
/// its measurement rows' notes carry it); contact [`trace::CONTACT_M`] when it names none.
pub fn sampler_of(note: &str) -> (Option<f64>, f64) {
    let Some(name) = note.split("; ").find(|p| p.starts_with("actor_trace ")) else {
        return (None, trace::CONTACT_M);
    };
    let words: Vec<&str> = name.split(' ').collect();
    let radius = words
        .iter()
        .find_map(|w| w.strip_prefix('r').and_then(|x| x.parse().ok()));
    let contact = words
        .windows(2)
        .find(|w| w[0] == "contact")
        .and_then(|w| w[1].parse().ok());
    (radius, contact.unwrap_or(trace::CONTACT_M))
}

/// Heat layers of traces over a map's cells: each subject's sample binned by its cell, within the map's height band,
/// by trace.rs's own rules ([`trace::marks`]): over the cells, each sums to its trace metric while every subject stays
/// in the map.
#[derive(Debug, Clone, PartialEq)]
pub struct Heat {
    pub sources: Vec<i64>,
    /// The traces' minutes, first sample to last, summed.
    pub minutes: f64,
    /// Subject-seconds per m² per minute.
    pub occupancy: Vec<f64>,
    /// Contacts begun per minute.
    pub contacts: Vec<f64>,
    /// Blocked seconds per minute.
    pub blocked: Vec<f64>,
    /// Samples moving above 0.3 m/s, and gliding among them.
    pub moving: Vec<u32>,
    pub gliding: Vec<u32>,
    /// The moving samples' mean velocity (x, y).
    pub flow: Vec<Option<[f64; 2]>>,
    /// Each trace's reference path.
    pub paths: Vec<Vec<[f64; 2]>>,
    /// Cells within a trace's radius of its reference in some sample: what the traces could see. None with no radius
    /// known.
    pub covered: Option<Vec<bool>>,
    pub radius: Vec<Option<f64>>,
    /// Subjects' samples binned, outside the map, off its level.
    pub binned: usize,
    pub outside: usize,
    pub off_level: usize,
}

pub fn heat(frame: &Frame, z_range: f64, traces: &[Trace]) -> Heat {
    let n = frame.cells();
    let mut h = Heat {
        sources: traces.iter().map(|t| t.id).collect(),
        minutes: 0.0,
        occupancy: vec![0.0; n],
        contacts: vec![0.0; n],
        blocked: vec![0.0; n],
        moving: vec![0; n],
        gliding: vec![0; n],
        flow: vec![None; n],
        paths: Vec::new(),
        covered: None,
        radius: traces.iter().map(|t| t.radius).collect(),
        binned: 0,
        outside: 0,
        off_level: 0,
    };
    let mut flow = vec![[0.0; 2]; n];
    let mut seen: Vec<([f64; 2], f64)> = Vec::new();
    for t in traces {
        let s = &t.samples;
        if let (Some(a), Some(b)) = (s.first(), s.last()) {
            h.minutes += (b.t - a.t) / 60.0;
        }
        let marks = trace::marks(s, &trace::tracks(s), t.contact);
        let mut path: Vec<[f64; 2]> = Vec::new();
        for (i, sample) in s.iter().enumerate() {
            let dt = s.get(i + 1).map_or(0.0, |next| next.t - sample.t);
            if let Some(reference) = &sample.reference {
                let p = xy(reference.pos);
                if path.last().is_none_or(|q| dist(*q, p) >= frame.cell / 2.0) {
                    path.push(p);
                    if let Some(radius) = t.radius {
                        seen.push((p, radius));
                    }
                }
            }
            for (b, m) in sample.subjects.iter().zip(&marks[i]) {
                if (b.pos[2] - frame.at[2]).abs() > z_range {
                    h.off_level += 1;
                    continue;
                }
                let Some(c) = frame.cell_of(xy(b.pos)) else {
                    h.outside += 1;
                    continue;
                };
                h.binned += 1;
                h.occupancy[c] += dt;
                h.contacts[c] += f64::from(u8::from(m.contact));
                h.blocked[c] += m.blocked_s;
                if m.moving {
                    h.moving[c] += 1;
                    h.gliding[c] += u32::from(m.gliding);
                    let v = b.velocity.unwrap_or_default();
                    flow[c] = [flow[c][0] + v[0], flow[c][1] + v[1]];
                }
            }
        }
        h.paths.push(path);
    }
    let (area, minutes) = (frame.cell * frame.cell, h.minutes);
    for layer in [&mut h.occupancy, &mut h.contacts, &mut h.blocked] {
        for x in layer.iter_mut() {
            *x = if minutes > 0.0 { *x / minutes } else { 0.0 };
        }
    }
    for x in &mut h.occupancy {
        *x /= area;
    }
    h.flow = flow
        .iter()
        .zip(&h.moving)
        .map(|(f, &k)| (k > 0).then(|| [f[0] / f64::from(k), f[1] / f64::from(k)]))
        .collect();
    if traces.iter().any(|t| t.radius.is_some()) {
        let covered = (0..n)
            .map(|c| {
                let p = frame.centre(c);
                seen.iter().any(|(q, radius)| dist(p, *q) <= *radius)
            })
            .collect();
        h.covered = Some(covered);
    }
    h
}

impl Heat {
    /// The glide share per cell: of its moving samples, the gliding.
    pub fn glide(&self) -> Vec<Option<f64>> {
        self.moving
            .iter()
            .zip(&self.gliding)
            .map(|(&m, &g)| (m > 0).then(|| f64::from(g) / f64::from(m)))
            .collect()
    }

    /// What the traces could see: their radius, the share of the map's cells within it of a reference, the references'
    /// samples kept, and where the subjects' samples went.
    pub fn coverage(&self) -> Value {
        let share = self.covered.as_ref().map(|c| {
            r(
                c.iter().filter(|&&x| x).count() as f64 / c.len().max(1) as f64,
                3,
            )
        });
        json!({"radius_m": self.radius, "covered_share": share,
               "reference_points": self.paths.iter().map(Vec::len).sum::<usize>(),
               "subject_samples": {"binned": self.binned, "outside": self.outside, "off_level": self.off_level}})
    }

    /// Each layer's name, unit, bins (cells with a value), max and total (summed over the cells: the trace metric of
    /// the subjects in the map; glide: its pooled share; flow: the moving samples; reference: the paths' metres).
    pub fn layers(&self, frame: &Frame) -> Vec<Value> {
        let area = frame.cell * frame.cell;
        let max = |v: &[f64]| v.iter().copied().fold(0.0, f64::max);
        let bins = |v: &[f64]| v.iter().filter(|&&x| x != 0.0).count();
        let moving: u32 = self.moving.iter().sum();
        let gliding: u32 = self.gliding.iter().sum();
        let glide = self.glide();
        let speeds: Vec<f64> = self
            .flow
            .iter()
            .flatten()
            .map(|v| v[0].hypot(v[1]))
            .collect();
        let paths: f64 = self.paths.iter().map(|p| length(p)).sum();
        let layer = |name: &str, unit: &str, bins: usize, max: f64, total: Value| {
            json!({"name": name, "unit": unit, "sources": self.sources, "bins": bins, "max": r(max, 4),
                   "total": total, "coverage": self.coverage()})
        };
        let sum = |v: &[f64]| v.iter().sum::<f64>();
        vec![
            layer(
                "occupancy",
                "subject-s/m²/min",
                bins(&self.occupancy),
                max(&self.occupancy),
                json!(r(sum(&self.occupancy) * area * self.minutes / 60.0, 4)),
            ),
            layer(
                "blocked",
                "s/min",
                bins(&self.blocked),
                max(&self.blocked),
                json!(r(sum(&self.blocked), 4)),
            ),
            layer(
                "contacts",
                "1/min",
                bins(&self.contacts),
                max(&self.contacts),
                json!(r(sum(&self.contacts), 4)),
            ),
            layer("flow", "m/s", speeds.len(), max(&speeds), json!(moving)),
            layer(
                "glide",
                "share",
                glide.iter().flatten().count(),
                glide.iter().flatten().copied().fold(0.0, f64::max),
                json!((moving > 0).then(|| r(f64::from(gliding) / f64::from(moving), 4))),
            ),
            layer(
                "reference",
                "m",
                self.paths.iter().map(Vec::len).sum(),
                0.0,
                json!(r(paths, 2)),
            ),
        ]
    }

    /// The layers' cell arrays, row-major, for the map's file.
    pub fn bins(&self) -> Value {
        let round = |v: &[f64]| v.iter().map(|&x| r(x, 5)).collect::<Vec<_>>();
        let flow: Vec<Value> = self
            .flow
            .iter()
            .map(|f| json!(f.map(|v| [r(v[0], 3), r(v[1], 3)])))
            .collect();
        json!({"occupancy": round(&self.occupancy), "blocked": round(&self.blocked), "contacts": round(&self.contacts),
               "flow": flow, "glide": self.glide().iter().map(|g| g.map(|x| r(x, 4))).collect::<Vec<_>>(),
               "covered": self.covered, "reference": self.paths.iter().map(|p| p.iter().map(|q| [r(q[0], 3), r(q[1], 3)]).collect::<Vec<_>>()).collect::<Vec<_>>()})
    }
}

/// B's occupancy less A's, per cell.
pub fn difference(a: &Heat, b: &Heat) -> Vec<f64> {
    a.occupancy
        .iter()
        .zip(&b.occupancy)
        .map(|(a, b)| b - a)
        .collect()
}

/// An actor of the occupants' world.actors read, placed on the map: off its level (outside at.z ± z_range: an
/// interior's map shouldn't show the street, nor another floor) and whether the map holds it.
#[derive(Debug, Clone, PartialEq)]
pub struct Occupant {
    pub body: Body,
    pub off_level: bool,
    pub on_map: bool,
}

impl Occupant {
    pub fn json(&self) -> Value {
        let b = &self.body;
        json!({"id": b.id, "x": r(b.pos[0], 3), "y": r(b.pos[1], 3), "z": r(b.pos[2], 3), "heading": r(b.heading, 1),
               "speed": b.speed().map(|s| r(s, 2)), "off_level": self.off_level, "on_map": self.on_map})
    }

    /// Drawn and binned: on the map's level.
    pub fn shown(&self) -> bool {
        !self.off_level && self.on_map
    }
}

/// A world.actors read's actors as occupants of `frame` (its z the band's centre).
pub fn occupants(actors: &Value, frame: &Frame, z_range: f64) -> Result<Vec<Occupant>, String> {
    let list = actors["actors"]
        .as_array()
        .ok_or("world.actors without actors")?;
    list.iter()
        .map(|a| {
            let body = Body::actor(a)?;
            Ok(Occupant {
                off_level: (body.pos[2] - frame.at[2]).abs() > z_range,
                on_map: frame.cell_of(xy(body.pos)).is_some(),
                body,
            })
        })
        .collect()
}

/// A profile's station: a point on the axis and the unit normal to the axis's left there.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Station {
    pub at: [f64; 3],
    pub left: [f64; 2],
}

/// Stations every `step` metres along the polyline `axis` from its first point, z interpolated; at a vertex the next
/// leg's direction. Refused over `max` stations, or with a leg under a millimetre.
pub fn stations(axis: &[[f64; 3]], step: f64, max: usize) -> Result<Vec<Station>, String> {
    let legs: Vec<([f64; 3], [f64; 3], f64)> = axis
        .windows(2)
        .map(|w| (w[0], w[1], dist(xy(w[0]), xy(w[1]))))
        .collect();
    if legs.iter().any(|l| l.2 < 1e-3) {
        return Err("axis: two points closer than a millimetre".into());
    }
    let total: f64 = legs.iter().map(|l| l.2).sum();
    let n = (total / step + 1e-9).floor() as usize + 1;
    if n > max {
        return Err(format!(
            "axis: {n} stations of {step} m, at most {max} (a longer step)"
        ));
    }
    let mut out = Vec::with_capacity(n);
    let (mut leg, mut before) = (0, 0.0);
    for k in 0..n {
        let s = k as f64 * step;
        while leg + 1 < legs.len() && s >= before + legs[leg].2 {
            before += legs[leg].2;
            leg += 1;
        }
        let (a, b, l) = legs[leg];
        let f = ((s - before) / l).min(1.0);
        let at = [0, 1, 2].map(|c| a[c] + (b[c] - a[c]) * f);
        let (dx, dy) = ((b[0] - a[0]) / l, (b[1] - a[1]) / l);
        out.push(Station {
            at,
            left: [-dy, dx],
        });
    }
    Ok(out)
}

/// A station's reach to one side from nav.line's read of the segment from it, `max_m` long: the whole segment when
/// NavigationLineTest finds it clear (open), else the distance in xy to NavigationClearLineInDirection's point (its
/// meaning unverified).
pub fn reach(from: [f64; 3], read: &Value, max_m: f64) -> (f64, bool) {
    if read["clear"] == true {
        return (max_m, true);
    }
    let point = trace::vec3(&read["reach"]).unwrap_or(from);
    (dist(xy(from), xy(point)).min(max_m), false)
}

/// A profile's step: its station and reaches.
#[derive(Debug, Clone, PartialEq)]
pub struct Rib {
    pub at: [f64; 3],
    pub left_m: f64,
    pub right_m: f64,
    pub left_open: bool,
    pub right_open: bool,
}

/// The profile's result: every step, the narrowest width and where.
pub fn profile(ribs: &[Rib]) -> Value {
    let steps: Vec<Value> = ribs
        .iter()
        .map(|b| {
            json!({"x": r(b.at[0], 3), "y": r(b.at[1], 3), "left_m": r(b.left_m, 2), "right_m": r(b.right_m, 2),
                   "width_m": r(b.left_m + b.right_m, 2), "left_open": b.left_open, "right_open": b.right_open})
        })
        .collect();
    let min = ribs
        .iter()
        .enumerate()
        .min_by(|a, b| (a.1.left_m + a.1.right_m).total_cmp(&(b.1.left_m + b.1.right_m)));
    json!({"steps": steps, "min_width_m": min.map(|(_, b)| r(b.left_m + b.right_m, 2)),
           "min_step": min.map(|(i, _)| i), "min_at": min.map(|(_, b)| [r(b.at[0], 3), r(b.at[1], 3)])})
}

/// A walker's look-ahead: from its position along its velocity for `ahead_m`; none when it moves at most
/// [`WALKING_MPS`].
pub fn ahead(b: &Body, ahead_m: f64) -> Option<[f64; 3]> {
    let v = b.velocity?;
    let s = b.speed().filter(|s| *s > WALKING_MPS)?;
    Some([
        b.pos[0] + v[0] / s * ahead_m,
        b.pos[1] + v[1] / s * ahead_m,
        b.pos[2],
    ])
}

/// The nearest of `others` (who, where) within [`IN_THE_WAY_M`] of the line from `from` to `to` in xy, ahead of
/// `from`: who, and how far along the line.
pub fn in_the_way(
    from: [f64; 3],
    to: [f64; 3],
    others: &[(Value, [f64; 3])],
) -> Option<(Value, f64)> {
    let (dx, dy) = (to[0] - from[0], to[1] - from[1]);
    let l = dx.hypot(dy);
    if l <= 0.0 {
        return None;
    }
    others
        .iter()
        .filter_map(|(who, p)| {
            let (px, py) = (p[0] - from[0], p[1] - from[1]);
            let along = (px * dx + py * dy) / l;
            let across = (px * dy - py * dx).abs() / l;
            (along > 0.0 && along <= l && across <= IN_THE_WAY_M).then(|| (who.clone(), along))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
}

/// A walker's verdict from its look-ahead's nav.line and scene.ray reads and the body in the way: the nearest of
/// navmesh_edge (the lane not clear: at the reach point), static (the ray's hit) and actor; else clear.
pub fn verdict(
    walker: &Body,
    to: [f64; 3],
    nav: &Value,
    ray: &Value,
    actor: Option<(Value, f64)>,
) -> Value {
    let from = walker.pos;
    let edge = (nav["clear"] == false).then(|| {
        let point = trace::vec3(&nav["reach"]).unwrap_or(from);
        dist(xy(from), xy(point))
    });
    let hit = (ray["hit"] == true).then(|| ray["distance"].as_f64().unwrap_or(0.0));
    let mut found: Vec<(&str, f64, Value)> = Vec::new();
    found.extend(edge.map(|d| ("navmesh_edge", d, Value::Null)));
    found.extend(hit.map(|d| ("static", d, Value::Null)));
    found.extend(actor.map(|(who, d)| ("actor", d, who)));
    let nearest = found.into_iter().min_by(|a, b| a.1.total_cmp(&b.1));
    let (what, at, by) = nearest.map_or(("clear", None, Value::Null), |(w, d, by)| {
        (w, Some(r(d, 2)), by)
    });
    json!({"id": walker.id, "x": r(from[0], 3), "y": r(from[1], 3), "speed": walker.speed().map(|s| r(s, 2)),
           "ahead": [r(to[0], 3), r(to[1], 3)], "verdict": what, "dist_m": at, "by": by,
           "nav_clear": nav["clear"], "edge_m": edge.map(|d| r(d, 2)), "ray_m": hit.map(|d| r(d, 2))})
}

/// The picture's colours (sRGB): each class, and what is drawn over them.
pub const WALKABLE: [u8; 3] = [236, 236, 226];
pub const TIGHT_HATCH: [u8; 3] = [196, 168, 96];
pub const COVERED: [u8; 3] = [184, 184, 168];
pub const WATER: [u8; 3] = [72, 128, 200];
/// An obstacle: from the low colour at 0 m to the high at 3 m or more over the reference.
pub const OBSTACLE_LOW: [u8; 3] = [200, 150, 100];
pub const OBSTACLE_HIGH: [u8; 3] = [100, 60, 30];
pub const TALL: [u8; 3] = [20, 20, 20];
pub const LEDGE: [u8; 3] = [128, 128, 128];
pub const UNREAD: [u8; 3] = [190, 80, 190];
/// No grid read (a profile's or walkers' picture).
pub const BLANK: [u8; 3] = [244, 244, 244];
pub const GRID: [u8; 3] = [150, 150, 150];
pub const SCALE: [u8; 3] = [0, 0, 0];
pub const CHOKE: [u8; 3] = [220, 30, 30];
pub const PLAYER: [u8; 3] = [0, 110, 240];
pub const ACTOR: [u8; 3] = [240, 130, 0];
pub const OCCUPANCY: [u8; 3] = [230, 40, 40];
/// The difference: B over A, A over B.
pub const MORE: [u8; 3] = [230, 40, 40];
pub const LESS: [u8; 3] = [40, 80, 230];
pub const BLOCKED: [u8; 3] = [140, 40, 170];
pub const CONTACTS: [u8; 3] = [250, 210, 0];
pub const FLOW: [u8; 3] = [0, 120, 60];
pub const REFERENCE: [u8; 3] = [0, 90, 200];
/// A profile's reach: open to max_m, stopped short, and the narrowest step's.
pub const OPEN: [u8; 3] = [60, 170, 60];
pub const SHORT: [u8; 3] = [230, 140, 0];
/// A walker's look-ahead by verdict.
pub const CLEAR: [u8; 3] = [60, 170, 60];
pub const STOPPED: [u8; 3] = [220, 30, 30];

/// The picture's legend, for the map's JSON (the picture has no text).
pub fn legend() -> Value {
    let hex = |c: [u8; 3]| format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2]);
    json!({
        "px_per_cell": PX, "up": "+y", "right": "+x", "grid_lines_m": 5, "scale_bar_m": 10,
        "scale_bar": "bottom left, black", "classes": {
            "walkable": hex(WALKABLE), "tight": format!("{} hatched on {}", hex(TIGHT_HATCH), hex(WALKABLE)),
            "covered": hex(COVERED), "water": hex(WATER),
            "obstacle": format!("{} (0 m) to {} (3 m and over)", hex(OBSTACLE_LOW), hex(OBSTACLE_HIGH)),
            "tall": hex(TALL), "ledge": hex(LEDGE), "unread": hex(UNREAD), "no grid": hex(BLANK)},
        "grid_lines": hex(GRID), "chokepoint": format!("{} bar across the passage, its width long", hex(CHOKE)),
        "player": format!("{} ring, a tick along the heading", hex(PLAYER)),
        "actor": format!("{} dot, a tick along the heading", hex(ACTOR)),
        "occupancy": format!("{} over the cell, opacity by value / max", hex(OCCUPANCY)),
        "occupancy_diff": format!("{} B over A, {} A over B, opacity by |value| / max", hex(MORE), hex(LESS)),
        "blocked": format!("{} inner square, opacity by value / max", hex(BLOCKED)),
        "contacts": format!("{} centre square, opacity by value / max", hex(CONTACTS)),
        "flow": format!("{} line from the cell's centre, a cell per m/s up to 2", hex(FLOW)),
        "reference": format!("{} line: the trace reference's path", hex(REFERENCE)),
        "profile": format!("{} reach open to max_m, {} stopped short", hex(OPEN), hex(SHORT)),
        "walker": format!("{} look-ahead clear, {} stopped (its verdict)", hex(CLEAR), hex(STOPPED)),
    })
}

/// What the picture shows.
#[derive(Debug, Clone, Default)]
pub struct Drawing<'a> {
    /// Per cell, its class and obstacle height; empty: no grid read (a plain background).
    pub classes: &'a [Class],
    pub heights: &'a [Option<f64>],
    pub heat: Option<&'a Heat>,
    /// B's occupancy less A's: drawn instead of the occupancy.
    pub difference: Option<&'a [f64]>,
    pub chokepoints: &'a [Chokepoint],
    /// Lines (from, to, colour): a profile's reaches, walkers' look-aheads.
    pub lines: &'a [([f64; 2], [f64; 2], [u8; 3])],
    /// Bodies: where, heading, whether the player.
    pub bodies: &'a [([f64; 2], f64, bool)],
}

struct Canvas {
    w: usize,
    h: usize,
    px: Vec<[u8; 3]>,
    frame: Frame,
}

impl Canvas {
    fn set(&mut self, x: i64, y: i64, c: [u8; 3]) {
        if x >= 0 && y >= 0 && (x as usize) < self.w && (y as usize) < self.h {
            self.px[y as usize * self.w + x as usize] = c;
        }
    }

    fn blend(&mut self, x: usize, y: usize, c: [u8; 3], alpha: f64) {
        let p = &mut self.px[y * self.w + x];
        for k in 0..3 {
            p[k] = (f64::from(p[k]) * (1.0 - alpha) + f64::from(c[k]) * alpha).round() as u8;
        }
    }

    /// World xy to pixels (fractional).
    fn to_px(&self, p: [f64; 2]) -> [f64; 2] {
        let b = self.frame.bbox();
        let k = PX as f64 / self.frame.cell;
        [(p[0] - b[0]) * k, (b[3] - p[1]) * k]
    }

    fn line_px(&mut self, a: [f64; 2], b: [f64; 2], c: [u8; 3], thick: bool) {
        let n = ((b[0] - a[0]).hypot(b[1] - a[1]) * 2.0).ceil().max(1.0) as usize;
        for s in 0..=n {
            let f = s as f64 / n as f64;
            let (x, y) = (a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f);
            self.set(x.floor() as i64, y.floor() as i64, c);
            if thick {
                self.set(x.floor() as i64 + 1, y.floor() as i64, c);
                self.set(x.floor() as i64, y.floor() as i64 + 1, c);
            }
        }
    }

    fn line(&mut self, a: [f64; 2], b: [f64; 2], c: [u8; 3], thick: bool) {
        let (a, b) = (self.to_px(a), self.to_px(b));
        self.line_px(a, b, c, thick);
    }

    /// A cell's square inset by `inset` of its side on each edge, blended.
    fn square(&mut self, i: usize, inset: f64, c: [u8; 3], alpha: f64) {
        let (x0, y0) = ((i % self.frame.nx) * PX, (i / self.frame.nx) * PX);
        let cut = (PX as f64 * inset).round() as usize;
        for y in y0 + cut..y0 + PX - cut {
            for x in x0 + cut..x0 + PX - cut {
                self.blend(x, y, c, alpha);
            }
        }
    }

    /// A body: a ring (the player) or a dot, and a tick along its heading.
    fn body(&mut self, p: [f64; 2], heading: f64, player: bool) {
        let [cx, cy] = self.to_px(p);
        let (c, outer, inner, tick) = match player {
            true => (PLAYER, 6.0, 4.0, 11.0),
            false => (ACTOR, 3.0, -1.0, 7.0),
        };
        for dy in -7i32..=7 {
            for dx in -7i32..=7 {
                let d = f64::from(dx).hypot(f64::from(dy));
                if d <= outer && d > inner {
                    let (x, y) = (cx.floor() as i64, cy.floor() as i64);
                    self.set(x + i64::from(dx), y + i64::from(dy), c);
                }
            }
        }
        let f = forward(heading);
        self.line_px([cx, cy], [cx + f[0] * tick, cy - f[1] * tick], c, false);
    }
}

fn mix(a: [u8; 3], b: [u8; 3], f: f64) -> [u8; 3] {
    [0, 1, 2].map(|k| (f64::from(a[k]) + (f64::from(b[k]) - f64::from(a[k])) * f).round() as u8)
}

/// The picture: `PX` pixels a cell, +y up, no text; an 8-bit RGB PNG.
pub fn render(frame: &Frame, d: &Drawing) -> Result<Vec<u8>, String> {
    let (w, h) = (frame.nx * PX, frame.ny * PX);
    let mut cv = Canvas {
        w,
        h,
        px: vec![BLANK; w * h],
        frame: *frame,
    };
    for (i, class) in d.classes.iter().enumerate() {
        let (x0, y0) = ((i % frame.nx) * PX, (i / frame.nx) * PX);
        for y in y0..y0 + PX {
            for x in x0..x0 + PX {
                cv.px[y * w + x] = match class {
                    Class::Walkable => WALKABLE,
                    Class::Tight if (x + y) % 4 < 2 => TIGHT_HATCH,
                    Class::Tight => WALKABLE,
                    Class::Covered => COVERED,
                    Class::Water => WATER,
                    Class::Obstacle => {
                        let height = d.heights.get(i).copied().flatten().unwrap_or(0.0);
                        mix(OBSTACLE_LOW, OBSTACLE_HIGH, (height / 3.0).clamp(0.0, 1.0))
                    }
                    Class::Tall => TALL,
                    Class::Ledge => LEDGE,
                    Class::Unread => UNREAD,
                };
            }
        }
    }
    let top = |v: &[f64]| v.iter().map(|x| x.abs()).fold(0.0, f64::max);
    if let Some(diff) = d.difference {
        let max = top(diff);
        for (i, &v) in diff.iter().enumerate().filter(|(_, v)| **v != 0.0) {
            let c = if v > 0.0 { MORE } else { LESS };
            cv.square(i, 0.0, c, 0.6 * v.abs() / max);
        }
    }
    if let Some(heat) = d.heat {
        let layers: [(&[f64], f64, [u8; 3]); 3] = [
            (&heat.occupancy, 0.0, OCCUPANCY),
            (&heat.blocked, 0.25, BLOCKED),
            (&heat.contacts, 0.375, CONTACTS),
        ];
        for (k, (v, inset, c)) in layers.into_iter().enumerate() {
            if k == 0 && d.difference.is_some() {
                continue;
            }
            let max = top(v);
            for (i, &x) in v.iter().enumerate().filter(|(_, x)| **x > 0.0) {
                cv.square(i, inset, c, 0.25 + 0.6 * x / max);
            }
        }
    }
    let b = frame.bbox();
    let k = PX as f64 / frame.cell;
    let first = |lo: f64| (lo / 5.0).ceil() as i64;
    for m in first(b[0])..=(b[2] / 5.0).floor() as i64 {
        let x = ((m as f64 * 5.0 - b[0]) * k).round() as i64;
        for y in 0..h as i64 {
            cv.set(x.min(w as i64 - 1), y, GRID);
        }
    }
    for m in first(b[1])..=(b[3] / 5.0).floor() as i64 {
        let y = ((b[3] - m as f64 * 5.0) * k).round() as i64;
        for x in 0..w as i64 {
            cv.set(x, y.min(h as i64 - 1), GRID);
        }
    }
    if let Some(heat) = d.heat {
        for (i, f) in heat.flow.iter().enumerate() {
            if let Some(v) = f {
                let p = frame.centre(i);
                let s = v[0].hypot(v[1]);
                let l = s.min(2.0) / 2.0 * frame.cell / s.max(1e-9);
                cv.line(p, [p[0] + v[0] * l, p[1] + v[1] * l], FLOW, false);
            }
        }
        for path in &heat.paths {
            for w in path.windows(2) {
                cv.line(w[0], w[1], REFERENCE, false);
            }
        }
    }
    for c in d.chokepoints {
        let across = forward(c.dir_deg + 90.0);
        let half = c.width_m / 2.0;
        let a = [c.at[0] - across[0] * half, c.at[1] - across[1] * half];
        let z = [c.at[0] + across[0] * half, c.at[1] + across[1] * half];
        cv.line(a, z, CHOKE, true);
    }
    for (a, z, c) in d.lines {
        cv.line(*a, *z, *c, false);
    }
    // The scale bar: 10 m, bottom left, 3 px of black on a white border.
    let bar = (10.0 * k).round() as i64;
    for y in h as i64 - 6..h as i64 - 1 {
        for x in 3..bar + 6 {
            let edge = y == h as i64 - 6 || y == h as i64 - 2 || x == 3 || x == bar + 5;
            cv.set(x, y, if edge { [255; 3] } else { SCALE });
        }
    }
    for &(p, heading, player) in d.bodies {
        cv.body(p, heading, player);
    }
    let raw: Vec<u8> = cv.px.iter().flatten().copied().collect();
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, w as u32, h as u32);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .and_then(|mut wr| wr.write_image_data(&raw))
        .map_err(|e| format!("PNG: {e}"))?;
    Ok(out)
}

/// A grid read and what it gives: the classes, the mask's clearance, chokepoints, regions and edges.
#[derive(Debug, Clone, PartialEq)]
pub struct Grid {
    pub frame: Frame,
    pub z_range: f64,
    pub radius: f64,
    /// Per cell its reading; none: unread.
    pub cells: Vec<Option<Reading>>,
    pub classified: Classified,
    pub mask: Vec<bool>,
    pub clearance: Vec<Option<f64>>,
    pub chokepoints: Vec<Chokepoint>,
    pub components: Vec<Component>,
    pub edges: Vec<Vec<[f64; 2]>>,
}

/// A cell's flags in the map's file: 1 nav_z, 2 free, 4 a ground hit, 8 a water depth above 0, 16 read, 32 walkable,
/// 64 the roof ray's hit faced up.
const NAV: u32 = 1;
const FREE: u32 = 2;
const HIT: u32 = 4;
const WET: u32 = 8;
const READ: u32 = 16;
const MASK: u32 = 32;
const UP: u32 = 64;
const BITS: &str = "1 nav_z, 2 free, 4 ground hit, 8 water depth above 0, 16 read, 32 walkable, 64 roof hit faced up";

impl Grid {
    pub fn new(frame: Frame, z_range: f64, radius: f64, cells: Vec<Option<Reading>>) -> Grid {
        let classified = classify(&frame, z_range, &cells);
        let mask: Vec<bool> = classified.class.iter().map(|c| c.walkable()).collect();
        let clearance = clearance(&frame, &mask);
        Grid {
            chokepoints: chokepoints(&frame, &clearance, CHOKEPOINTS),
            components: components(&frame, &mask),
            edges: edges(&frame, &mask),
            frame,
            z_range,
            radius,
            cells,
            classified,
            mask,
            clearance,
        }
    }

    /// The summary every result carries inline.
    pub fn summary(&self) -> Map<String, Value> {
        let f = &self.frame;
        let read = self.cells.iter().flatten().count();
        let mut classes = Map::new();
        for c in CLASSES {
            let n = self.classified.class.iter().filter(|k| **k == c).count();
            if n > 0 || c != Class::Unread {
                classes.insert(c.name().into(), json!(n));
            }
        }
        let walkable = self.mask.iter().filter(|&&m| m).count();
        let edges: f64 = self.edges.iter().map(|e| length(e)).sum();
        let mut out = Map::new();
        out.insert(
            "size".into(),
            json!([f.nx as f64 * f.cell, f.ny as f64 * f.cell]),
        );
        out.insert("cell".into(), json!(f.cell));
        out.insert("nx".into(), json!(f.nx));
        out.insert("ny".into(), json!(f.ny));
        out.insert("bbox".into(), json!(f.bbox().map(|x| r(x, 3))));
        out.insert("z_range".into(), json!(self.z_range));
        out.insert("radius".into(), json!(self.radius));
        out.insert(
            "z_ref".into(),
            json!(self.classified.z_ref.map(|z| r(z, 3))),
        );
        out.insert("ground_ref".into(), json!(r(self.classified.ground_ref, 3)));
        out.insert("read".into(), json!(read));
        out.insert("classes".into(), Value::Object(classes));
        out.insert(
            "walkable_share".into(),
            json!(r(walkable as f64 / read.max(1) as f64, 3)),
        );
        out.insert("components_found".into(), json!(self.components.len()));
        let components = self.components.iter().take(COMPONENTS).map(Component::json);
        out.insert("components".into(), json!(components.collect::<Vec<_>>()));
        let chokepoints = self.chokepoints.iter().map(Chokepoint::json);
        out.insert("chokepoints".into(), json!(chokepoints.collect::<Vec<_>>()));
        out.insert(
            "edges".into(),
            json!({"polylines": self.edges.len(), "length_m": r(edges, 2)}),
        );
        out
    }

    /// The map file's cell arrays, row-major, and the edge polylines.
    pub fn arrays(&self) -> Map<String, Value> {
        let z = |f: fn(&Reading) -> Option<f64>| -> Vec<Option<f64>> {
            self.cells
                .iter()
                .map(|c| c.as_ref().and_then(f).map(|z| r(z, 3)))
                .collect()
        };
        let flags: Vec<u32> = self
            .cells
            .iter()
            .zip(&self.mask)
            .map(|(c, &m)| {
                c.map_or(0, |c| {
                    READ | if c.nav_z.is_some() { NAV } else { 0 }
                        | if c.free == Some(true) { FREE } else { 0 }
                        | if c.top_z.is_some() { HIT } else { 0 }
                        | if c.water_m.is_some_and(|d| d > 0.0) {
                            WET
                        } else {
                            0
                        }
                        | if m { MASK } else { 0 }
                        | if c.roof_up { UP } else { 0 }
                })
            })
            .collect();
        let class: Vec<usize> = self.classified.class.iter().map(|c| c.index()).collect();
        let heights: Vec<Option<f64>> = self
            .classified
            .height
            .iter()
            .map(|h| h.map(|h| r(h, 3)))
            .collect();
        let clearance: Vec<Option<f64>> =
            self.clearance.iter().map(|c| c.map(|c| r(c, 3))).collect();
        let mut out = Map::new();
        out.insert(
            "order".into(),
            json!("row-major: row 0 at the top (+y), column 0 at the left (-x)"),
        );
        out.insert("nav_z".into(), json!(z(|c| c.nav_z)));
        out.insert("top_z".into(), json!(z(|c| c.top_z)));
        out.insert("water_m".into(), json!(z(|c| c.water_m)));
        out.insert("water_z".into(), json!(z(|c| c.water_z)));
        out.insert("roof_z".into(), json!(z(|c| c.roof_z)));
        out.insert("flags".into(), json!({"bits": BITS, "cells": flags}));
        out.insert(
            "class".into(),
            json!({"names": CLASSES.map(Class::name), "cells": class}),
        );
        out.insert("height_m".into(), json!(heights));
        out.insert("clearance_m".into(), json!(clearance));
        out.insert("edges_m".into(), json!(self.edges));
        out
    }

    /// A grid back from a map file of an earlier world_map ([`Grid::summary`] and [`Grid::arrays`] at its top level,
    /// `at` beside them): the same classes again from its stored readings.
    pub fn from_file(v: &Value) -> Result<Grid, String> {
        let bad = |what: &str| format!("not a world_map grid file: {what}");
        let at = trace::vec3(&v["at"]).ok_or_else(|| bad("at"))?;
        let num = |k: &str| v[k].as_f64().ok_or_else(|| bad(k));
        let (cell, z_range, radius) = (num("cell")?, num("z_range")?, num("radius")?);
        let whole = |k: &str| v[k].as_u64().map(|n| n as usize).ok_or_else(|| bad(k));
        let (nx, ny) = (whole("nx")?, whole("ny")?);
        let frame = Frame::new(at, [nx as f64 * cell, ny as f64 * cell], cell)?;
        if (frame.nx, frame.ny) != (nx, ny) {
            return Err(bad("nx, ny"));
        }
        let array = |k: &str| {
            v[k].as_array()
                .filter(|a| a.len() == nx * ny)
                .ok_or_else(|| bad(k))
        };
        let (nav, top, water) = (array("nav_z")?, array("top_z")?, array("water_m")?);
        let flags = v["flags"]["cells"]
            .as_array()
            .filter(|a| a.len() == nx * ny)
            .ok_or_else(|| bad("flags"))?;
        let cells = (0..nx * ny)
            .map(|i| {
                let f = flags[i].as_u64().unwrap_or(0) as u32;
                (f & READ != 0).then(|| {
                    let nav_z = nav[i].as_f64();
                    Reading {
                        nav_z,
                        free: nav_z.map(|_| f & FREE != 0),
                        top_z: top[i].as_f64(),
                        water_m: water_depth(&water[i]),
                        water_z: v["water_z"][i].as_f64(),
                        roof_z: v["roof_z"][i].as_f64(),
                        roof_up: f & UP != 0,
                    }
                })
            })
            .collect();
        Ok(Grid::new(frame, z_range, radius, cells))
    }
}

/// Trims the longest of `lists` (JSON pointers into `out`, each an array) by half until `out` is at most `limit` bytes
/// as JSON text; whether it trimmed any.
pub fn fit(out: &mut Value, lists: &[&str], limit: usize) -> bool {
    let mut trimmed = false;
    while out.to_string().len() > limit {
        let longest = lists
            .iter()
            .filter_map(|p| {
                let a = out.pointer(p)?.as_array()?;
                (!a.is_empty()).then(|| (Value::Array(a.clone()).to_string().len(), *p))
            })
            .max();
        let Some((_, p)) = longest else { break };
        if let Some(Value::Array(a)) = out.pointer_mut(p) {
            a.truncate(a.len() / 2);
        }
        trimmed = true;
    }
    trimmed
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mask from rows of text, the first row at the top: `.` walkable, `#` not.
    fn mask(rows: &[&str]) -> (Frame, Vec<bool>) {
        let (nx, ny) = (rows[0].len(), rows.len());
        let frame = Frame::new([0.0, 0.0, 0.0], [nx as f64, ny as f64], 1.0).unwrap();
        let m = rows
            .iter()
            .flat_map(|r| r.bytes().map(|b| b == b'.'))
            .collect();
        (frame, m)
    }

    /// The brute-force transform the fast one must equal.
    fn brute(frame: &Frame, m: &[bool]) -> Vec<Option<f64>> {
        (0..m.len())
            .map(|i| {
                let d = (0..m.len())
                    .filter(|&j| !m[j])
                    .map(|j| dist(frame.centre(i), frame.centre(j)))
                    .fold(f64::INFINITY, f64::min);
                (m[i] && d.is_finite()).then_some(d - 0.5)
            })
            .collect()
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    /// A corridor 7 m wide (rows 1-7 between walls) pinched to 3 m over columns 10-12, running off both map edges.
    fn pinched() -> Vec<String> {
        (0..9)
            .map(|row| {
                (0..25)
                    .map(|c| {
                        let wall = row == 0
                            || row == 8
                            || ((10..=12).contains(&c) && !(3..=5).contains(&row));
                        if wall { '#' } else { '.' }
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn the_distance_transform_is_exact_and_gives_a_corridor_s_width() {
        let rows = pinched();
        let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
        let (frame, m) = mask(&rows);
        let c = clearance(&frame, &m);
        assert_eq!(c, brute(&frame, &m));
        // The corridor's centre row: 3.5 m clear (7 m wide), 1.5 m in the pinch (3 m wide).
        assert!(close(c[4 * 25].unwrap(), 3.5), "{:?}", c[4 * 25]);
        assert!(close(c[4 * 25 + 11].unwrap(), 1.5));
        // Off the mask, none; no wall in the map, none.
        assert_eq!(c[0], None);
        let (open, all) = mask(&["....", "...."]);
        assert_eq!(clearance(&open, &all), vec![None; 8]);
        // Cells of 0.5 m halve it.
        let half = Frame { cell: 0.5, ..frame };
        assert!(close(clearance(&half, &m)[4 * 25].unwrap(), 1.75));
    }

    #[test]
    fn a_pinch_is_one_chokepoint_of_its_width() {
        let rows = pinched();
        let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
        let (frame, m) = mask(&rows);
        let c = clearance(&frame, &m);
        let found = chokepoints(&frame, &c, 10);
        assert_eq!(found.len(), 1, "{found:?}");
        let p = &found[0];
        assert!(close(p.width_m, 3.0), "{p:?}");
        assert_eq!(p.at, frame.centre(4 * 25 + 11), "the pinch's middle");
        assert!(close(p.dir_deg, 90.0), "along x: {p:?}");
    }

    #[test]
    fn a_pillar_in_a_square_gives_two_chokepoints() {
        // A street 13 m wide through a square (walls at rows 0 and 14), open at both map edges, a 3 m pillar in the
        // middle: two passages of 5 m, above and below it.
        let rows: Vec<String> = (0..15)
            .map(|row| {
                (0..31)
                    .map(|c| {
                        let pillar = (14..=16).contains(&c) && (6..=8).contains(&row);
                        if row == 0 || row == 14 || pillar {
                            '#'
                        } else {
                            '.'
                        }
                    })
                    .collect()
            })
            .collect();
        let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
        let (frame, m) = mask(&rows);
        let c = clearance(&frame, &m);
        let found = chokepoints(&frame, &c, 10);
        assert_eq!(found.len(), 2, "{found:?}");
        for p in &found {
            assert!(close(p.width_m, 5.0), "{p:?}");
            assert!(
                close(p.at[0], frame.centre(15)[0]),
                "beside the pillar: {p:?}"
            );
        }
        assert!(
            found[0].at[1] != found[1].at[1],
            "one above, one below: {found:?}"
        );
    }

    #[test]
    fn a_t_junction_s_arms_keep_their_widths_and_no_false_chokepoint() {
        // A bar 7 m wide (rows 1-7) across the map, a stem 3 m wide (columns 9-11) down from it to the bottom edge.
        let rows: Vec<String> = (0..20)
            .map(|row| {
                (0..21)
                    .map(|c| {
                        let open = (1..=7).contains(&row) || (row >= 8 && (9..=11).contains(&c));
                        if open { '.' } else { '#' }
                    })
                    .collect()
            })
            .collect();
        let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
        let (frame, m) = mask(&rows);
        let c = clearance(&frame, &m);
        assert!(close(c[4 * 21 + 2].unwrap(), 3.5), "the bar: 7 m");
        assert!(close(c[15 * 21 + 10].unwrap(), 1.5), "the stem: 3 m");
        assert_eq!(chokepoints(&frame, &c, 10), [], "arms run off the map");
        // Closed: the stem ends in a wall, the bar at both sides: dead ends aren't chokepoints either.
        let mut closed = m.clone();
        for row in 0..20 {
            closed[row * 21] = false;
            closed[row * 21 + 20] = false;
        }
        for col in 0..21 {
            closed[19 * 21 + col] = false;
        }
        let c = clearance(&frame, &closed);
        assert_eq!(chokepoints(&frame, &c, 10), []);
    }

    /// Beyond the brief's three: a room's two doors, an even pinch's direction, a diagonal street's pinch.
    #[test]
    fn doors_even_pinches_and_diagonal_streets() {
        let found = |f: &dyn Fn(usize, usize) -> bool, nx: usize, ny: usize| {
            let frame = Frame::new([0.0, 0.0, 0.0], [nx as f64, ny as f64], 1.0).unwrap();
            let m: Vec<bool> = (0..nx * ny).map(|i| f(i % nx, i / nx)).collect();
            let c = clearance(&frame, &m);
            chokepoints(&frame, &c, 10)
                .iter()
                .map(|p| (p.at, r(p.width_m, 2), r(p.dir_deg, 1)))
                .collect::<Vec<_>>()
        };
        // A room with 1 m walls (columns 5 and 21, rows 3 and 15), a 3 m door on the left and a 1 m one at the top.
        let room = |c: usize, row: usize| {
            let wall = ((c == 5 || c == 21) && (3..=15).contains(&row))
                || ((row == 3 || row == 15) && (5..=21).contains(&c));
            let door = (c == 5 && (8..=10).contains(&row)) || (row == 3 && c == 13);
            !wall || door
        };
        assert_eq!(
            found(&room, 27, 19),
            [([0.0, 6.0], 1.0, 0.0), ([-8.0, 0.0], 3.0, 90.0)]
        );
        // 6 m pinched to 4 m: within a cell of its width, along x.
        let even = |c: usize, row: usize| {
            (1..=6).contains(&row) && !((10..=12).contains(&c) && (row == 1 || row == 6))
        };
        assert_eq!(found(&even, 25, 8), [([-1.0, -0.5], 3.0, 90.0)]);
        // A street along x = -y, 3.7 m across, pinched to 2.1 m about its middle: one chokepoint there.
        let diagonal = |c: usize, row: usize| {
            let off = (c as f64 - row as f64).abs();
            let mid = (c + row) as f64 / 2.0;
            off < if (14.0..17.0).contains(&mid) {
                1.6
            } else {
                2.6
            }
        };
        assert_eq!(found(&diagonal, 30, 30), [([0.5, -0.5], 1.83, 45.0)]);
    }

    #[test]
    fn components_and_edges_follow_the_mask() {
        let (frame, m) = mask(&["#####", "#..##", "#..#.", "#####"]);
        let parts = components(&frame, &m);
        assert_eq!(parts.len(), 2);
        assert_eq!((parts[0].cells, parts[0].area_m2), (4, 4.0));
        assert_eq!(parts[0].at, [-0.5, 0.0]);
        let lines = edges(&frame, &m);
        // The 2 x 2 block's boundary: a closed diamond-cornered loop through the mid-points around it.
        let block: Vec<&Vec<[f64; 2]>> = lines.iter().filter(|l| l.len() > 3).collect();
        assert_eq!(block.len(), 1, "{lines:?}");
        let loop_ = block[0];
        assert_eq!(loop_.first(), loop_.last(), "closed: {loop_:?}");
        assert!(
            close(length(loop_), 4.0 + 4.0 * 0.5f64.sqrt()),
            "{}",
            length(loop_)
        );
        // The cell at the right edge: a boundary that leaves the map, open.
        assert!(lines.iter().any(|l| l.first() != l.last()), "{lines:?}");
    }

    #[test]
    fn cells_are_classed_by_navmesh_hit_and_water() {
        let frame = Frame::new([0.0, 0.0, 10.0], [4.0, 2.0], 1.0).unwrap();
        let read = |nav_z: Option<f64>, free: Option<bool>, top_z: Option<f64>, water_m: f64| {
            Some(Reading {
                nav_z,
                free,
                top_z,
                water_m: Some(water_m),
                water_z: Some(9.5),
                ..Reading::default()
            })
        };
        let cells = vec![
            read(Some(10.8), Some(true), Some(10.0), 0.0),
            read(Some(10.6), Some(true), Some(10.0), 0.0),
            read(Some(10.7), Some(false), Some(10.1), 0.0),
            read(Some(10.7), Some(true), Some(14.0), 0.0),
            read(None, None, Some(9.0), 1.2),
            read(None, None, Some(11.5), 0.0),
            read(None, None, None, 0.0),
            None,
        ];
        let c = classify(&frame, 2.0, &cells);
        use Class::*;
        assert_eq!(
            c.class,
            [
                Walkable, Walkable, Tight, Covered, Water, Obstacle, Tall, Unread
            ]
        );
        assert_eq!(
            c.z_ref,
            Some(10.7),
            "the walkable cells' (and covered) nav_z"
        );
        assert_eq!(
            c.ground_ref, 10.0,
            "their ground hit, the covered one's roof left out"
        );
        assert!(close(c.height[5].unwrap(), 1.5));
        // A hit within 0.3 m of the ground, no navmesh: a ledge.
        let ledge = vec![
            read(Some(10.8), Some(true), Some(10.0), 0.0),
            read(None, None, Some(10.2), 0.0),
        ];
        let frame = Frame::new([0.0, 0.0, 10.0], [2.0, 1.0], 1.0).unwrap();
        assert_eq!(classify(&frame, 2.0, &ledge).class, [Walkable, Ledge]);
        let read = Reading::from_ground(
            &json!({"nav_z": 3.5, "ground": {"pos": [1, 2, 3], "normal": [0, 0, 1]},
            "water_level": 0, "water_depth": -2.0, "free": false, "safe": null}),
        );
        assert_eq!(
            read,
            Reading {
                nav_z: Some(3.5),
                free: Some(false),
                top_z: Some(3.0),
                water_m: Some(-2.0),
                water_z: Some(0.0),
                roof_z: None,
                roof_up: false,
            }
        );
    }

    /// GetWaterDepth's no-water value (10000, the Velen cellar of the 0.1.15 live check) is no depth, and dry land's
    /// negative depth (Novigrad's square: −28.7) and 0 are no water: earth around a dry cellar reads as earth.
    #[test]
    fn the_no_water_value_and_dry_depths_are_not_water() {
        assert_eq!(water_depth(&json!(10000.0)), None);
        assert_eq!(water_depth(&json!(1000.5)), None);
        assert_eq!(water_depth(&Value::Null), None);
        assert_eq!(water_depth(&json!(-28.7)), Some(-28.7));
        assert_eq!(water_depth(&json!(2.5)), Some(2.5));
        let cellar = |depth: f64, top_z: Option<f64>| {
            Reading::from_ground(
                &json!({"nav_z": null, "ground": top_z.map(|z| json!({"pos": [0, 0, z]})),
                "water_level": -0.2, "water_depth": depth, "free": null, "safe": null}),
            )
        };
        let frame = Frame::new([0.0, 0.0, -1.0], [4.0, 1.0], 1.0).unwrap();
        let cells = [
            cellar(10000.0, None),
            cellar(10000.0, Some(-1.5)),
            cellar(-28.7, None),
            cellar(0.0, Some(-1.5)),
        ];
        let c = classify(&frame, 2.0, &cells.map(Some));
        use Class::*;
        assert_eq!(c.class, [Tall, Obstacle, Tall, Obstacle]);
        assert_eq!(cells[0].water_m, None);
    }

    /// Water below a deck: a ground hit above the water surface wins; with no hit in the band the water counts only
    /// when its surface is in the band (the canal 31 m below the bridge is a drop: tall). A surface at or above the
    /// hit (submerged ground) or in the band without a hit is water. The navmesh wins over water, as before.
    #[test]
    fn water_below_a_deck_is_the_deck() {
        let frame = Frame::new([0.0, 0.0, 31.0], [6.0, 1.0], 1.0).unwrap();
        let cell = |nav_z: Option<f64>, top_z: Option<f64>, water_z: f64| {
            Some(Reading {
                nav_z,
                free: nav_z.map(|_| true),
                top_z,
                water_m: Some(4.0),
                water_z: Some(water_z),
                ..Reading::default()
            })
        };
        let cells = [
            cell(None, Some(32.0), 0.1),
            cell(None, None, 0.1),
            cell(None, Some(29.5), 30.0),
            cell(None, None, 30.0),
            cell(None, Some(31.0), 31.0),
            cell(Some(31.5), Some(31.0), 32.0),
        ];
        let c = classify(&frame, 2.0, &cells);
        use Class::*;
        assert_eq!(c.class, [Obstacle, Tall, Water, Water, Water, Walkable]);
    }

    /// The 0.1.16 live check, beside the Velen cellar: no navmesh, a ground hit at −2.52 to −1.66, a water surface at
    /// −0.19 to +0.21 above it, and a ray straight up hitting the terrain 1.1 to 1.8 m up with a normal facing up (0.96
    /// to 0.99): its underside. Such a cell gets a roof ray from its water surface and is classed by its ground hit;
    /// water whose ray hits nothing, or a ceiling facing down (the cellar's: −1), stays water.
    #[test]
    fn water_under_the_ground_is_classed_by_its_ground_hit() {
        let frame = Frame::new([0.0, 0.0, 0.0], [4.0, 1.0], 1.0).unwrap();
        let cell = |nav_z: Option<f64>, top: f64, level: f64| {
            Some(Reading::from_ground(&json!({
                "nav_z": nav_z, "ground": {"pos": [0, 0, top], "normal": [0, 0, 1]}, "water_level": level,
                "water_depth": level - top, "free": nav_z.map(|_| true), "safe": null})))
        };
        let mut cells = [
            cell(Some(0.5), 0.0, -0.2),
            cell(None, -2.52, -0.19),
            cell(None, -1.66, 0.21),
            cell(None, -2.1, 0.0),
        ];
        let rays = roofs(&frame, 2.0, &cells);
        let up: Vec<usize> = rays.iter().map(|r| r.0).collect();
        assert_eq!(
            up,
            [0, 1, 2, 3],
            "the walkable cell's, then each would-be water's"
        );
        let [x, y] = frame.centre(1);
        assert_eq!(rays[1].1, [x, y, -0.19], "from the water surface");
        assert_eq!(rays[1].2, [x, y, -0.19 + ROOF_UP_M]);
        let hit = |z: f64, up: f64| json!({"hit": true, "pos": [x, y, z], "normal": [0, 0, up], "distance": 1.5});
        let miss = json!({"hit": false, "pos": null, "normal": null, "distance": null});
        let replies = [miss.clone(), hit(1.6, 0.97), miss, hit(0.72, -1.0)];
        for ((i, ..), v) in rays.iter().zip(&replies) {
            cells[*i].as_mut().unwrap().roof(v);
        }
        let c = classify(&frame, 2.0, &cells);
        use Class::*;
        assert_eq!(c.class, [Walkable, Obstacle, Water, Water]);
        assert!(close(c.height[1].unwrap(), -2.52), "{:?}", c.height);
        assert!(cells[1].unwrap().roof_up && !cells[3].unwrap().roof_up);
    }

    /// Cover past the band: a roof ray up ROOF_UP_M from each walkable cell's navmesh z the ground ray didn't cover, none
    /// for a tight cell, one the ground ray covered, or a band that already reaches that high; its hit over COVER_M
    /// above the navmesh z makes the cell covered, a lower one doesn't.
    #[test]
    fn the_roof_ray_sees_cover_above_the_band() {
        let frame = Frame::new([0.0, 0.0, 10.0], [5.0, 1.0], 1.0).unwrap();
        let cell = |free: bool, top_z: f64| {
            Some(Reading {
                nav_z: Some(10.5),
                free: Some(free),
                top_z: Some(top_z),
                ..Reading::default()
            })
        };
        let mut cells = vec![
            cell(true, 10.0),
            cell(true, 10.0),
            cell(false, 10.0),
            cell(true, 12.9),
            None,
        ];
        let rays = roofs(&frame, 2.0, &cells);
        let up: Vec<usize> = rays.iter().map(|r| r.0).collect();
        assert_eq!(up, [0, 1], "walkable, uncovered cells only");
        assert_eq!(rays[0].1, [-2.0, 0.0, 10.5]);
        assert_eq!(rays[0].2, [-2.0, 0.0, 10.5 + ROOF_UP_M]);
        assert!(
            roofs(&frame, 6.5, &cells).is_empty(),
            "the ground ray started at 16.5 m, ROOF_UP_M above the navmesh"
        );
        cells[0].as_mut().unwrap().roof_z = Some(13.0);
        cells[1].as_mut().unwrap().roof_z = Some(12.5);
        use Class::*;
        assert_eq!(
            classify(&frame, 2.0, &cells).class,
            [Covered, Walkable, Tight, Covered, Unread]
        );
    }

    /// The scene.ground points: every cell once, row-major from the top left, 16 a call.
    #[test]
    fn the_frame_pages_its_cells_row_major_from_the_top_left() {
        let frame = Frame::new([100.0, 200.0, 30.0], [5.0, 4.0], 1.0).unwrap();
        assert_eq!((frame.nx, frame.ny), (5, 4));
        assert_eq!(frame.bbox(), [97.5, 198.0, 102.5, 202.0]);
        let points = frame.points();
        assert_eq!(points.len(), 20);
        assert_eq!(points[0], [98.0, 201.5, 30.0]);
        assert_eq!(points[19], [102.0, 198.5, 30.0]);
        assert_eq!(points.chunks(16).count(), 2);
        for (i, p) in points.iter().enumerate() {
            assert_eq!(frame.cell_of([p[0], p[1]]), Some(i));
        }
        assert_eq!(frame.cell_of([97.4, 200.0]), None);
        assert!(
            Frame::new([0.0; 3], [200.0, 200.0], 1.0).is_err(),
            "40,000 cells"
        );
        assert_eq!(
            Frame::new([0.0; 3], [200.0, 200.0], 2.0).unwrap().cells(),
            10_000
        );
    }

    /// Walkers past a standing reference: heat layers binned from a trace's CSV sum to its metrics.
    #[test]
    fn heat_layers_sum_to_the_trace_metrics() {
        let body = |id: i64, x: f64, y: f64, heading: f64, v: [f64; 2]| Body {
            id: Some(id),
            pos: [x, y, 0.0],
            heading,
            velocity: Some([v[0], v[1], 0.0]),
            flags: 64,
            entity: "npc".into(),
        };
        let samples: Vec<Sample> = (0..=100)
            .map(|i| {
                let t = i as f64 / 10.0;
                let mut subjects = vec![
                    // Passes within 0.5 m along x at 1 m/s: a contact.
                    body(1, -5.0 + t, 0.5, -90.0, [1.0, 0.0]),
                    // Pushed sideways: gliding.
                    body(2, -2.0 + t * 0.5, 4.0, 0.0, [0.5, 0.0]),
                ];
                // Walks in for 2 s, then stands 1 m off from t = 2: blocked.
                let (x, v) = if t < 2.0 {
                    (-1.0 - 2.0 + t, 1.0)
                } else {
                    (-1.0, 0.0)
                };
                subjects.push(body(3, x, -0.3, -90.0, [v, 0.0]));
                Sample {
                    t,
                    reference: Some(Body {
                        id: None,
                        pos: [0.0, 0.0, 0.0],
                        heading: 0.0,
                        velocity: None,
                        flags: 0,
                        entity: "player".into(),
                    }),
                    found: subjects.len() as u32,
                    subjects,
                }
            })
            .collect();
        let samples = trace::parse_csv(&trace::csv(&samples)).unwrap();
        let m = trace::metrics(&samples, trace::CONTACT_M).unwrap();
        assert!(
            m.contacts_per_min > 0.0 && m.blocked_s_per_min > 0.0,
            "{m:?}"
        );
        let frame = Frame::new([0.0, 0.0, 0.0], [20.0, 20.0], 0.5).unwrap();
        let t = Trace {
            id: 7,
            samples: samples.clone(),
            contact: trace::CONTACT_M,
            radius: Some(8.0),
        };
        let h = heat(&frame, 2.0, &[t]);
        let area = 0.25;
        let occupancy: f64 = h.occupancy.iter().sum::<f64>() * area * h.minutes / 60.0;
        assert!(
            (occupancy - m.subject_minutes).abs() < 1e-9,
            "{occupancy} {m:?}"
        );
        assert!((h.contacts.iter().sum::<f64>() - m.contacts_per_min).abs() < 1e-9);
        assert!((h.blocked.iter().sum::<f64>() - m.blocked_s_per_min).abs() < 1e-9);
        let (moving, gliding): (u32, u32) = (h.moving.iter().sum(), h.gliding.iter().sum());
        assert!((f64::from(gliding) / f64::from(moving) - m.glide_share.unwrap()).abs() < 1e-12);
        let layers = h.layers(&frame);
        assert_eq!(layers[0]["name"], "occupancy");
        assert!((layers[0]["total"].as_f64().unwrap() - m.subject_minutes).abs() < 1e-3);
        assert!((layers[2]["total"].as_f64().unwrap() - m.contacts_per_min).abs() < 1e-3);
        assert_eq!(layers[0]["coverage"]["subject_samples"]["off_level"], 0);
        // The blocked walker's seconds lie where it stood.
        let stood = frame.cell_of([-1.0, -0.3]).unwrap();
        assert!((h.blocked[stood] - m.blocked_s_per_min).abs() < 1e-9);
        // Coverage: the cells within 8 m of the reference.
        let covered = h.covered.as_ref().unwrap();
        assert!(
            covered[frame.cell_of([0.0, 7.9]).unwrap()]
                && !covered[frame.cell_of([9.0, 9.0]).unwrap()]
        );
        // Subjects on another level: counted off_level, never binned.
        let up: Vec<Sample> = samples
            .iter()
            .map(|s| {
                let mut s = s.clone();
                for b in &mut s.subjects {
                    b.pos[2] = 4.0;
                }
                s
            })
            .collect();
        let t = Trace {
            id: 8,
            samples: up,
            contact: trace::CONTACT_M,
            radius: None,
        };
        let h = heat(&frame, 2.0, &[t]);
        assert_eq!(h.binned, 0);
        assert_eq!(h.off_level, 303);
        assert_eq!(h.occupancy.iter().sum::<f64>(), 0.0);
        assert_eq!(h.covered, None, "no radius known");
    }

    #[test]
    fn the_sampler_s_radius_and_contact_come_from_its_note() {
        let note = "s1/walk-trace.csv; actor_trace 10 Hz r12.5 max20 ref player contact 0.9; 21 samples over 2 s";
        assert_eq!(sampler_of(note), (Some(12.5), 0.9));
        assert_eq!(
            sampler_of("x.csv; actor_trace 10 Hz r8 max20 ref player; 3 samples"),
            (Some(8.0), trace::CONTACT_M)
        );
        assert_eq!(sampler_of("something else"), (None, trace::CONTACT_M));
    }

    #[test]
    fn occupants_off_the_level_are_flagged() {
        let frame = Frame::new([0.0, 0.0, 10.0], [20.0, 20.0], 1.0).unwrap();
        let actor = |x: f64, z: f64| json!({"id": 1, "pos": [x, 0.0, z], "heading": 0, "velocity": [0.5, 0.0, 0.0], "actor": true});
        let actors =
            json!({"found": 3, "actors": [actor(1.0, 10.5), actor(2.0, 13.0), actor(30.0, 10.0)]});
        let occ = occupants(&actors, &frame, 2.0).unwrap();
        let flags: Vec<(bool, bool, bool)> = occ
            .iter()
            .map(|o| (o.off_level, o.on_map, o.shown()))
            .collect();
        assert_eq!(
            flags,
            [
                (false, true, true),
                (true, true, false),
                (false, false, false)
            ]
        );
        assert_eq!(occ[1].json()["off_level"], true);
        assert_eq!(occ[0].json()["speed"], 0.5);
    }

    #[test]
    fn a_profile_s_stations_and_reaches() {
        let axis = [[0.0, 0.0, 5.0], [10.0, 0.0, 5.0], [10.0, 4.0, 7.0]];
        let s = stations(&axis, 2.0, 40).unwrap();
        assert_eq!(s.len(), 8, "14 m: stations at 0, 2, ... 14");
        assert_eq!(s[1].at, [2.0, 0.0, 5.0]);
        assert_eq!(s[1].left, [0.0, 1.0], "along +x, the left is +y");
        assert_eq!(s[6].at, [10.0, 2.0, 6.0]);
        assert_eq!(s[6].left, [-1.0, 0.0]);
        assert!(stations(&axis, 0.1, 40).is_err());
        assert!(stations(&[[0.0; 3], [0.0; 3]], 1.0, 40).is_err());
        let open =
            json!({"clear": true, "clear_in_direction": true, "reach": [0, 20, 5], "edge_m": 1});
        assert_eq!(reach([0.0, 0.0, 5.0], &open, 20.0), (20.0, true));
        let short =
            json!({"clear": false, "clear_in_direction": false, "reach": [0, 3.5, 5], "edge_m": 0});
        assert_eq!(reach([0.0, 0.0, 5.0], &short, 20.0), (3.5, false));
        let ribs = [
            Rib {
                at: [0.0; 3],
                left_m: 3.0,
                right_m: 4.0,
                left_open: false,
                right_open: false,
            },
            Rib {
                at: [2.0, 0.0, 0.0],
                left_m: 1.0,
                right_m: 2.0,
                left_open: false,
                right_open: false,
            },
        ];
        let p = profile(&ribs);
        assert_eq!(
            (p["min_width_m"].as_f64(), p["min_step"].as_u64()),
            (Some(3.0), Some(1))
        );
        assert_eq!(p["steps"][0]["width_m"], 7.0);
    }

    #[test]
    fn a_walker_s_verdict_is_the_nearest_thing_ahead() {
        let walker = Body::actor(
            &json!({"id": 5, "pos": [0, 0, 0], "heading": -90, "velocity": [1.0, 0, 0]}),
        )
        .unwrap();
        let to = ahead(&walker, 3.0).unwrap();
        assert_eq!(to, [3.0, 0.0, 0.0]);
        let standing =
            Body::actor(&json!({"id": 6, "pos": [0, 0, 0], "heading": 0, "velocity": [0.1, 0, 0]}))
                .unwrap();
        assert_eq!(ahead(&standing, 3.0), None);
        let others = [
            (json!(9), [2.0, 0.5, 0.0]),
            (json!("player"), [1.5, -0.4, 0.0]),
            (json!(10), [-1.0, 0.0, 0.0]),
            (json!(11), [1.0, 0.7, 0.0]),
        ];
        let by = in_the_way(walker.pos, to, &others);
        assert_eq!(by, Some((json!("player"), 1.5)));
        let clear_nav = json!({"clear": true, "reach": [3, 0, 0]});
        let no_hit = json!({"hit": false, "distance": null});
        let v = verdict(&walker, to, &clear_nav, &no_hit, by);
        assert_eq!(
            (
                v["verdict"].as_str(),
                v["by"].as_str(),
                v["dist_m"].as_f64()
            ),
            (Some("actor"), Some("player"), Some(1.5))
        );
        let hit = json!({"hit": true, "distance": 0.8});
        assert_eq!(
            verdict(
                &walker,
                to,
                &clear_nav,
                &hit,
                in_the_way(walker.pos, to, &others)
            )["verdict"],
            "static"
        );
        let edge = json!({"clear": false, "reach": [1.2, 0, 0]});
        let v = verdict(&walker, to, &edge, &no_hit, None);
        assert_eq!(
            (v["verdict"].as_str(), v["dist_m"].as_f64()),
            (Some("navmesh_edge"), Some(1.2))
        );
        assert_eq!(
            verdict(&walker, to, &clear_nav, &no_hit, None)["verdict"],
            "clear"
        );
    }

    /// The picture's pixels: each class its palette colour, the tight cell hatched, a chokepoint's bar red.
    #[test]
    fn the_picture_paints_the_palette() {
        let frame = Frame::new([0.5, 0.5, 0.0], [4.0, 2.0], 1.0).unwrap();
        use Class::*;
        let classes = [
            Walkable, Tight, Covered, Water, Obstacle, Tall, Ledge, Unread,
        ];
        let heights = [None, None, None, None, Some(3.0), None, Some(0.1), None];
        let d = Drawing {
            classes: &classes,
            heights: &heights,
            ..Drawing::default()
        };
        // Decoded, and a pixel's colour.
        let decode = |png: Vec<u8>| {
            let mut reader = png::Decoder::new(std::io::Cursor::new(png))
                .read_info()
                .unwrap();
            let mut rgb = vec![0; reader.output_buffer_size().unwrap()];
            let info = reader.next_frame(&mut rgb).unwrap();
            assert_eq!((info.width, info.height), (32, 16));
            rgb
        };
        let rgb = decode(render(&frame, &d).unwrap());
        let at = |rgb: &[u8], x: usize, y: usize| -> [u8; 3] {
            let i = (y * 32 + x) * 3;
            [rgb[i], rgb[i + 1], rgb[i + 2]]
        };
        // A pixel inside each cell, off the grid line at x = 0 m and above the scale bar.
        let inside = |i: usize| ((i % 4) * PX + 5, (i / 4) * PX + 1);
        let want = [
            WALKABLE,
            TIGHT_HATCH,
            COVERED,
            WATER,
            OBSTACLE_HIGH,
            TALL,
            LEDGE,
            UNREAD,
        ];
        for (i, c) in want.iter().enumerate().filter(|(i, _)| *i != 1) {
            let (x, y) = inside(i);
            assert_eq!(at(&rgb, x, y), *c, "cell {i}");
        }
        // The tight cell: hatched where (x + y) % 4 < 2.
        assert_eq!(at(&rgb, PX, 0), TIGHT_HATCH);
        assert_eq!(at(&rgb, PX, 2), WALKABLE, "the hatch's gap");
        // Grid lines every 5 m: x runs from -1.5 to 2.5 m, so one line, at x = 0.
        assert_eq!(at(&rgb, 12, 0), GRID, "x = 0 m");
        assert_eq!(at(&rgb, 4, 12), SCALE, "the scale bar");
        // A chokepoint across x at the map's middle: its bar red.
        let choke = [Chokepoint {
            at: [0.5, 0.5],
            width_m: 1.0,
            dir_deg: 90.0,
        }];
        let d = Drawing {
            classes: &classes,
            heights: &heights,
            chokepoints: &choke,
            ..Drawing::default()
        };
        let rgb = decode(render(&frame, &d).unwrap());
        let red = (0..16).filter(|&y| at(&rgb, 16, y) == CHOKE).count();
        assert!(
            red >= 6,
            "a vertical bar 1 m long at x = 0.5: {red} red pixels"
        );
    }

    #[test]
    fn a_grid_survives_its_file_and_fit_trims_the_longest_list() {
        let frame = Frame::new([0.0, 0.0, 1.0], [3.0, 2.0], 1.0).unwrap();
        let read = |free: bool| {
            Some(Reading {
                nav_z: Some(1.5),
                free: Some(free),
                top_z: Some(1.0),
                water_m: Some(0.0),
                water_z: Some(-0.5),
                roof_z: free.then_some(4.0),
                roof_up: false,
            })
        };
        let cells = vec![
            read(true),
            read(false),
            None,
            read(true),
            Some(Reading {
                roof_z: Some(2.0),
                roof_up: true,
                ..Reading::default()
            }),
            read(true),
        ];
        let grid = Grid::new(frame, 2.0, 0.4, cells);
        let mut file = grid.summary();
        file.extend(grid.arrays());
        file.insert("at".into(), json!(frame.at));
        let back = Grid::from_file(&Value::Object(file.clone())).unwrap();
        assert_eq!(back, grid);
        assert_eq!(file["classes"]["unread"], 1);
        assert!(Grid::from_file(&json!({"at": [0, 0, 0]})).is_err());
        let mut out = json!({"a": vec![1234567; 400], "b": {"list": vec![7; 20]}, "c": 1});
        assert!(fit(&mut out, &["/a", "/b/list"], 1000));
        assert!(out.to_string().len() <= 1000);
        assert!(
            out["a"].as_array().unwrap().len() < 400
                && out["b"]["list"].as_array().unwrap().len() == 20
        );
        assert!(!fit(&mut out, &["/a"], 100_000));
    }
}
