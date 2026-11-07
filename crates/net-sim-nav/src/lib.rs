//! Deterministic navigation mesh for Aurix Sim (phase 2 of `docs/sim/design.md`).
//!
//! Convex polygons share edges; A* runs over polygon portals (cost = distance between portal
//! midpoints) with a `BTreeSet` frontier so ties resolve identically on every platform, then
//! the simple-stupid-funnel pulls the corridor into a straight-line path. All math is Q32.32
//! `Fx`; floats are forbidden in this crate.
#![forbid(unsafe_code)]
#![deny(clippy::float_arithmetic)]

use net_fpmath::{Fx, FxVec2};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub type PolyId = u32;
const NONE: PolyId = u32::MAX;

/// One convex polygon: counter-clockwise vertex indices and, per edge `i -> i+1`, the neighbour
/// across that edge (`NONE` on the boundary).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Poly {
    pub verts: Vec<u32>,
    pub neighbors: Vec<PolyId>,
    /// Traversal cost multiplier (1 = normal, 2 = mud, ...); must be > 0.
    pub cost: Fx,
    /// Area flags for agent masks (walk/swim/door...).
    pub flags: u32,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct NavMesh {
    pub verts: Vec<FxVec2>,
    pub polys: Vec<Poly>,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Path {
    /// Polygons from start to goal inclusive.
    pub corridor: Vec<PolyId>,
    /// Straightened waypoints, first = start, last = goal.
    pub points: Vec<FxVec2>,
    pub cost: Fx,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MeshHit {
    pub point: FxVec2,
    pub poly: PolyId,
    pub edge: u32,
}

fn cross(a: FxVec2, b: FxVec2) -> Fx {
    a.x * b.y - a.y * b.x
}

fn tri_area2(a: FxVec2, b: FxVec2, c: FxVec2) -> Fx {
    cross(b - a, c - a)
}

impl NavMesh {
    /// Build from vertices and CCW polygons; neighbours are derived from shared edges
    /// (same two vertex indices in opposite order). Non-manifold edges keep the first match.
    pub fn new(verts: Vec<FxVec2>, polys: Vec<Vec<u32>>) -> NavMesh {
        Self::with_costs(verts, polys.into_iter().map(|v| (v, Fx::ONE, 0)).collect())
    }

    pub fn with_costs(verts: Vec<FxVec2>, polys: Vec<(Vec<u32>, Fx, u32)>) -> NavMesh {
        let mut edges: BTreeMap<(u32, u32), (PolyId, u32)> = BTreeMap::new();
        for (pi, (v, _, _)) in polys.iter().enumerate() {
            for i in 0..v.len() {
                let (a, b) = (v[i], v[(i + 1) % v.len()]);
                edges.entry((a, b)).or_insert((pi as PolyId, i as u32));
            }
        }
        let polys = polys
            .into_iter()
            .map(|(v, cost, flags)| {
                let n = v.len();
                let neighbors = (0..n)
                    .map(|i| {
                        let (a, b) = (v[i], v[(i + 1) % n]);
                        edges.get(&(b, a)).map(|(p, _)| *p).unwrap_or(NONE)
                    })
                    .collect();
                Poly {
                    verts: v,
                    neighbors,
                    cost: if cost > Fx::ZERO { cost } else { Fx::ONE },
                    flags,
                }
            })
            .collect();
        NavMesh { verts, polys }
    }

    /// Axis-aligned grid of `cols × rows` cells of size `cell` from `origin`, with `blocked`
    /// cells removed — handy for tests and tile-based games.
    pub fn grid(origin: FxVec2, cell: Fx, cols: u32, rows: u32, blocked: &[(u32, u32)]) -> NavMesh {
        let blocked: BTreeSet<(u32, u32)> = blocked.iter().copied().collect();
        let mut verts = Vec::new();
        for r in 0..=rows {
            for c in 0..=cols {
                verts.push(origin + FxVec2::new(cell * c as i32, cell * r as i32));
            }
        }
        let idx = |c: u32, r: u32| r * (cols + 1) + c;
        let mut polys = Vec::new();
        for r in 0..rows {
            for c in 0..cols {
                if blocked.contains(&(c, r)) {
                    continue;
                }
                polys.push(vec![
                    idx(c, r),
                    idx(c + 1, r),
                    idx(c + 1, r + 1),
                    idx(c, r + 1),
                ]);
            }
        }
        NavMesh::new(verts, polys)
    }

    pub fn centroid(&self, p: PolyId) -> FxVec2 {
        let poly = &self.polys[p as usize];
        let mut acc = FxVec2::ZERO;
        for &v in &poly.verts {
            acc = acc + self.verts[v as usize];
        }
        acc.scale(Fx::ONE / Fx::from_int(poly.verts.len() as i32))
    }

    fn edge(&self, p: PolyId, e: u32) -> (FxVec2, FxVec2) {
        let poly = &self.polys[p as usize];
        let n = poly.verts.len();
        (
            self.verts[poly.verts[e as usize] as usize],
            self.verts[poly.verts[(e as usize + 1) % n] as usize],
        )
    }

    /// True when `pt` is inside or on the border of convex polygon `p`.
    pub fn contains(&self, p: PolyId, pt: FxVec2) -> bool {
        let poly = &self.polys[p as usize];
        let n = poly.verts.len();
        (0..n).all(|i| {
            let (a, b) = (
                self.verts[poly.verts[i] as usize],
                self.verts[poly.verts[(i + 1) % n] as usize],
            );
            tri_area2(a, b, pt) >= Fx::ZERO
        })
    }

    /// Polygon containing `pt`, lowest id wins on shared edges.
    pub fn locate(&self, pt: FxVec2) -> Option<PolyId> {
        (0..self.polys.len() as PolyId).find(|&p| self.contains(p, pt))
    }

    /// Closest point on the mesh to `pt` (and its polygon); exact point when already on it.
    pub fn nearest(&self, pt: FxVec2) -> Option<(PolyId, FxVec2)> {
        if let Some(p) = self.locate(pt) {
            return Some((p, pt));
        }
        let mut best: Option<(Fx, PolyId, FxVec2)> = None;
        for p in 0..self.polys.len() as PolyId {
            let n = self.polys[p as usize].verts.len();
            for e in 0..n as u32 {
                let (a, b) = self.edge(p, e);
                let q = closest_on_segment(a, b, pt);
                let d = (q - pt).length_sq();
                if best.map(|(bd, ..)| d < bd).unwrap_or(true) {
                    best = Some((d, p, q));
                }
            }
        }
        best.map(|(_, p, q)| (p, q))
    }

    /// A* over polygon portals (midpoint-to-midpoint distance × area cost) then funnel
    /// straightening. Points outside the mesh snap to the nearest border. `mask` filters polygon
    /// flags (`flags & mask != 0` or flags == 0 always passes).
    pub fn find_path(&self, from: FxVec2, to: FxVec2, mask: u32) -> Option<Path> {
        let (start, from) = self.nearest(from)?;
        let (goal, to) = self.nearest(to)?;
        let pass = |p: PolyId| {
            let f = self.polys[p as usize].flags;
            f == 0 || f & mask != 0
        };
        if !pass(start) || !pass(goal) {
            return None;
        }
        if start == goal {
            return Some(Path {
                corridor: vec![start],
                points: vec![from, to],
                cost: (to - from).length(),
            });
        }
        // (f, g, poly, parent, entry edge of parent) — BTreeSet gives a deterministic frontier
        let mut open: BTreeSet<(Fx, Fx, PolyId)> = BTreeSet::new();
        let mut g: BTreeMap<PolyId, Fx> = BTreeMap::new();
        let mut came: BTreeMap<PolyId, (PolyId, u32)> = BTreeMap::new();
        let mut entry: BTreeMap<PolyId, FxVec2> = BTreeMap::new();
        g.insert(start, Fx::ZERO);
        entry.insert(start, from);
        open.insert(((to - from).length(), Fx::ZERO, start));
        let mut closed: BTreeSet<PolyId> = BTreeSet::new();
        while let Some(&(f, gc, cur)) = open.iter().next() {
            open.remove(&(f, gc, cur));
            if cur == goal {
                break;
            }
            if !closed.insert(cur) {
                continue;
            }
            let here = entry[&cur];
            let poly = &self.polys[cur as usize];
            for (e, &nb) in poly.neighbors.iter().enumerate() {
                if nb == NONE || closed.contains(&nb) || !pass(nb) {
                    continue;
                }
                let (a, b) = self.edge(cur, e as u32);
                let mid = (a + b).scale(Fx::HALF);
                let step = (mid - here).length() * poly.cost;
                let ng = gc + step;
                if g.get(&nb).map(|&old| ng >= old).unwrap_or(false) {
                    continue;
                }
                if let Some(&old) = g.get(&nb) {
                    open.remove(&(old + (to - entry[&nb]).length(), old, nb));
                }
                g.insert(nb, ng);
                came.insert(nb, (cur, e as u32));
                entry.insert(nb, mid);
                open.insert((ng + (to - mid).length(), ng, nb));
            }
        }
        if !g.contains_key(&goal) {
            return None;
        }
        let mut corridor = vec![goal];
        let mut portals: Vec<(FxVec2, FxVec2)> = Vec::new();
        let mut cur = goal;
        while let Some(&(parent, e)) = came.get(&cur) {
            let (a, b) = self.edge(parent, e);
            portals.push((a, b));
            corridor.push(parent);
            cur = parent;
        }
        corridor.reverse();
        portals.reverse();
        let points = funnel(from, to, &portals);
        let mut cost = Fx::ZERO;
        for w in points.windows(2) {
            cost += (w[1] - w[0]).length();
        }
        Some(Path {
            corridor,
            points,
            cost,
        })
    }

    /// Walk the mesh from `from` towards `to`; returns the first boundary crossing, or `None`
    /// when the segment stays on the mesh (line of sight for agents, no physics needed).
    pub fn raycast(&self, from: FxVec2, to: FxVec2) -> Option<MeshHit> {
        let mut cur = self.locate(from)?;
        let mut prev = NONE;
        for _ in 0..self.polys.len() + 1 {
            if self.contains(cur, to) {
                return None;
            }
            let poly = &self.polys[cur as usize];
            let n = poly.verts.len();
            let mut exit: Option<(u32, FxVec2)> = None;
            for e in 0..n as u32 {
                let (a, b) = self.edge(cur, e);
                // exit edge: `to` is on the outer side and the segment crosses the edge line
                if tri_area2(a, b, to) >= Fx::ZERO {
                    continue;
                }
                if let Some(q) = seg_intersect(from, to, a, b) {
                    exit = Some((e, q));
                    break;
                }
            }
            let (e, q) = exit?;
            let nb = poly.neighbors[e as usize];
            if nb == NONE {
                return Some(MeshHit {
                    point: q,
                    poly: cur,
                    edge: e,
                });
            }
            if nb == prev {
                return Some(MeshHit {
                    point: q,
                    poly: cur,
                    edge: e,
                });
            }
            prev = cur;
            cur = nb;
        }
        None
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }
    pub fn from_bytes(b: &[u8]) -> Option<NavMesh> {
        serde_json::from_slice(b).ok()
    }

    /// FNV-1a over vertices and polygons — equal meshes hash equal on every platform.
    pub fn hash(&self) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut mix = |v: i64| {
            for b in v.to_le_bytes() {
                h ^= b as u64;
                h = h.wrapping_mul(0x0000_0100_0000_01b3);
            }
        };
        for v in &self.verts {
            mix(v.x.0);
            mix(v.y.0);
        }
        for p in &self.polys {
            for &v in &p.verts {
                mix(v as i64);
            }
            for &n in &p.neighbors {
                mix(n as i64);
            }
            mix(p.cost.0);
            mix(p.flags as i64);
        }
        h
    }
}

pub fn hash_points(points: &[FxVec2]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for p in points {
        for v in [p.x.0, p.y.0] {
            for b in v.to_le_bytes() {
                h ^= b as u64;
                h = h.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
    }
    h
}

fn closest_on_segment(a: FxVec2, b: FxVec2, p: FxVec2) -> FxVec2 {
    let ab = b - a;
    let len2 = ab.length_sq();
    if len2 == Fx::ZERO {
        return a;
    }
    let t = ((p - a).dot(ab) / len2).clamp(Fx::ZERO, Fx::ONE);
    a + ab.scale(t)
}

/// Intersection of segment p0-p1 with segment a-b (None when parallel or outside either).
fn seg_intersect(p0: FxVec2, p1: FxVec2, a: FxVec2, b: FxVec2) -> Option<FxVec2> {
    let r = p1 - p0;
    let s = b - a;
    let den = cross(r, s);
    if den == Fx::ZERO {
        return None;
    }
    let qp = a - p0;
    let t = cross(qp, s) / den;
    let u = cross(qp, r) / den;
    let eps = Fx::from_ratio(1, 1_000_000);
    if t < -eps || t > Fx::ONE + eps || u < -eps || u > Fx::ONE + eps {
        return None;
    }
    Some(p0 + r.scale(t.clamp(Fx::ZERO, Fx::ONE)))
}

/// Simple stupid funnel over portals given as (left, right) when walking the corridor in CCW
/// polygons: edge `a -> b` of the current polygon has `a` on the right and `b` on the left.
fn funnel(start: FxVec2, goal: FxVec2, portals: &[(FxVec2, FxVec2)]) -> Vec<FxVec2> {
    // Mononen's simple stupid funnel; `area` has his sign convention (negative of our cross).
    let area = |a: FxVec2, b: FxVec2, c: FxVec2| -tri_area2(a, b, c);
    let mut ports: Vec<(FxVec2, FxVec2)> = Vec::with_capacity(portals.len() + 2);
    ports.push((start, start));
    for &(a, b) in portals {
        ports.push((b, a));
    }
    ports.push((goal, goal));
    let mut out = vec![start];
    let (mut apex, mut left, mut right) = (start, start, start);
    let (mut left_i, mut right_i) = (0usize, 0usize);
    let mut i = 1;
    while i < ports.len() {
        let (l, r) = ports[i];
        if area(apex, right, r) <= Fx::ZERO {
            if apex == right || area(apex, left, r) > Fx::ZERO {
                right = r;
                right_i = i;
            } else {
                out.push(left);
                apex = left;
                right = apex;
                right_i = left_i;
                i = left_i + 1;
                continue;
            }
        }
        if area(apex, left, l) >= Fx::ZERO {
            if apex == left || area(apex, right, l) < Fx::ZERO {
                left = l;
                left_i = i;
            } else {
                out.push(right);
                apex = right;
                left = apex;
                left_i = right_i;
                i = right_i + 1;
                continue;
            }
        }
        i += 1;
    }
    if *out.last().unwrap() != goal {
        out.push(goal);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fx(v: i32) -> Fx {
        Fx::from_int(v)
    }
    fn p(x: i32, y: i32) -> FxVec2 {
        FxVec2::new(fx(x), fx(y))
    }

    // 8×6 grid with a 1-wide vertical wall at column 4, rows 0..5 (gap at row 5).
    fn walled() -> NavMesh {
        let blocked: Vec<(u32, u32)> = (0..5).map(|r| (4, r)).collect();
        NavMesh::grid(p(0, 0), fx(1), 8, 6, &blocked)
    }

    #[test]
    fn adjacency_and_locate() {
        let m = NavMesh::grid(p(0, 0), fx(1), 2, 1, &[]);
        assert_eq!(m.polys.len(), 2);
        assert_eq!(m.polys[0].neighbors, vec![NONE, 1, NONE, NONE]);
        assert_eq!(m.polys[1].neighbors, vec![NONE, NONE, NONE, 0]);
        assert_eq!(
            m.locate(FxVec2::new(Fx::from_ratio(3, 2), Fx::HALF)),
            Some(1)
        );
        assert_eq!(m.locate(p(5, 5)), None);
        let (poly, q) = m.nearest(p(5, 5)).unwrap();
        assert_eq!((poly, q), (1, p(2, 1)));
    }

    #[test]
    fn path_goes_around_the_wall_and_is_straightened() {
        let m = walled();
        let from = FxVec2::new(Fx::HALF, Fx::HALF);
        let to = FxVec2::new(Fx::from_ratio(15, 2), Fx::HALF);
        let path = m.find_path(from, to, u32::MAX).expect("path");
        assert_eq!(path.points.first(), Some(&from));
        assert_eq!(path.points.last(), Some(&to));
        // corridor must pass through the gap row (y in 5..6) at the wall column
        assert!(
            path.corridor.iter().any(|&c| {
                let cen = m.centroid(c);
                cen.x > fx(4) && cen.x < fx(5) && cen.y > fx(5)
            }),
            "corridor {:?}",
            path.corridor
        );
        // funnel: a few corridor corners plus the two wall-top corners (grid cells bound the corridor)
        assert!(path.points.len() <= 8, "points {:?}", path.points);
        assert!(
            path.points.iter().any(|q| q.x == fx(4) && q.y == fx(5))
                && path.points.iter().any(|q| q.x == fx(5) && q.y == fx(5)),
            "corners missing: {:?}",
            path.points
        );
        let direct = (to - from).length();
        assert!(
            path.cost > direct && path.cost.to_f64() < 15.0,
            "cost {}",
            path.cost
        );
        assert_eq!(path.points, m.find_path(from, to, u32::MAX).unwrap().points);
        assert_eq!(
            hash_points(&path.points),
            hash_points(&walled().find_path(from, to, u32::MAX).unwrap().points)
        );
    }

    #[test]
    fn no_path_when_disconnected_and_flags_filter() {
        let blocked: Vec<(u32, u32)> = (0..6).map(|r| (4, r)).collect();
        let m = NavMesh::grid(p(0, 0), fx(1), 8, 6, &blocked);
        assert!(m
            .find_path(
                FxVec2::new(Fx::HALF, Fx::HALF),
                FxVec2::new(Fx::from_ratio(15, 2), Fx::HALF),
                u32::MAX
            )
            .is_none());
        let verts = vec![p(0, 0), p(1, 0), p(2, 0), p(2, 1), p(1, 1), p(0, 1)];
        let m = NavMesh::with_costs(
            verts,
            vec![
                (vec![0, 1, 4, 5], Fx::ONE, 1),
                (vec![1, 2, 3, 4], Fx::ONE, 2),
            ],
        );
        assert!(m
            .find_path(
                FxVec2::new(Fx::HALF, Fx::HALF),
                FxVec2::new(Fx::from_ratio(3, 2), Fx::HALF),
                1
            )
            .is_none());
        assert!(m
            .find_path(
                FxVec2::new(Fx::HALF, Fx::HALF),
                FxVec2::new(Fx::from_ratio(3, 2), Fx::HALF),
                3
            )
            .is_some());
    }

    #[test]
    fn mesh_raycast_stops_at_the_wall() {
        let m = walled();
        let hit = m
            .raycast(
                FxVec2::new(Fx::HALF, Fx::HALF),
                FxVec2::new(Fx::from_ratio(15, 2), Fx::HALF),
            )
            .expect("wall hit");
        assert_eq!(hit.point, FxVec2::new(fx(4), Fx::HALF));
        assert!(m
            .raycast(
                FxVec2::new(Fx::HALF, Fx::HALF),
                FxVec2::new(Fx::from_ratio(7, 2), Fx::from_ratio(11, 2))
            )
            .is_none());
        let out = m
            .raycast(FxVec2::new(Fx::HALF, Fx::HALF), p(-3, 0))
            .unwrap();
        assert!(
            out.point.x.abs() < Fx::from_ratio(1, 1000),
            "exit at {:?}",
            out.point
        );
    }

    #[test]
    fn mesh_serializes_and_hashes_stably() {
        let m = walled();
        let back = NavMesh::from_bytes(&m.to_bytes()).unwrap();
        assert_eq!(back, m);
        assert_eq!(back.hash(), m.hash());
        assert_ne!(m.hash(), NavMesh::grid(p(0, 0), fx(1), 8, 6, &[]).hash());
    }
}
