//! Deterministic agents on a navmesh: flow fields (one Dijkstra per goal, shared by any number
//! of agents) and a crowd stepper with arrive, separation and sampled velocity-obstacle
//! avoidance. Agents are kept on the mesh by sliding along boundary edges (mesh raycast), so no
//! physics world is required for units; use `net-sim-physics` only for things that need to collide.

use crate::{hash_points, NavMesh, PolyId};
use net_fpmath::{Fx, FxVec2};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Per-polygon "next hop" towards a goal point; `None` = unreachable.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct FlowField {
    pub goal: FxVec2,
    pub goal_poly: PolyId,
    /// For every polygon: (next polygon, point to steer towards) — the goal itself in its polygon.
    pub next: Vec<Option<(PolyId, FxVec2)>>,
    pub dist: Vec<Fx>,
}

impl FlowField {
    /// Dijkstra from the goal over portal midpoints (`mask` filters polygon flags like `find_path`).
    pub fn build(mesh: &NavMesh, goal: FxVec2, mask: u32) -> Option<FlowField> {
        let (goal_poly, goal) = mesh.nearest(goal)?;
        let n = mesh.polys.len();
        let mut next: Vec<Option<(PolyId, FxVec2)>> = vec![None; n];
        let mut dist: Vec<Fx> = vec![Fx::MAX; n];
        let mut entry: Vec<FxVec2> = vec![FxVec2::ZERO; n];
        let pass = |p: PolyId| {
            let f = mesh.polys[p as usize].flags;
            f == 0 || f & mask != 0
        };
        if !pass(goal_poly) {
            return None;
        }
        dist[goal_poly as usize] = Fx::ZERO;
        entry[goal_poly as usize] = goal;
        next[goal_poly as usize] = Some((goal_poly, goal));
        let mut open: BTreeSet<(Fx, PolyId)> = BTreeSet::new();
        open.insert((Fx::ZERO, goal_poly));
        while let Some(&(d, cur)) = open.iter().next() {
            open.remove(&(d, cur));
            if d > dist[cur as usize] {
                continue;
            }
            let here = entry[cur as usize];
            let poly = &mesh.polys[cur as usize];
            for (e, &nb) in poly.neighbors.iter().enumerate() {
                if nb == u32::MAX || !pass(nb) {
                    continue;
                }
                let (a, b) = mesh.edge_pub(cur, e as u32);
                let mid = (a + b).scale(Fx::HALF);
                let nd = d + (mid - here).length() * mesh.polys[nb as usize].cost;
                if nd < dist[nb as usize] {
                    dist[nb as usize] = nd;
                    entry[nb as usize] = mid;
                    next[nb as usize] = Some((cur, mid));
                    open.insert((nd, nb));
                }
            }
        }
        Some(FlowField {
            goal,
            goal_poly,
            next,
            dist,
        })
    }

    /// Unit direction an agent standing in `poly` at `pos` should move in (zero at the goal).
    pub fn direction(&self, poly: PolyId, pos: FxVec2) -> FxVec2 {
        match self.next.get(poly as usize).copied().flatten() {
            Some((_, target)) => {
                let d = target - pos;
                if d.length_sq() == Fx::ZERO {
                    FxVec2::ZERO
                } else {
                    d.normalized()
                }
            }
            None => FxVec2::ZERO,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Agent {
    pub pos: FxVec2,
    pub vel: FxVec2,
    pub radius: Fx,
    pub max_speed: Fx,
    /// Acceleration limit per second.
    pub max_accel: Fx,
    /// Goal point; `None` = idle (still avoids others).
    pub target: Option<FxVec2>,
    /// Flow field index in `Crowd::fields` when following a shared field instead of a target.
    pub field: Option<u32>,
    pub poly: PolyId,
    pub user: u64,
    pub arrived: bool,
}

impl Agent {
    pub fn new(pos: FxVec2, radius: Fx, max_speed: Fx) -> Agent {
        Agent {
            pos,
            vel: FxVec2::ZERO,
            radius,
            max_speed,
            max_accel: max_speed * Fx::from_int(8),
            target: None,
            field: None,
            poly: u32::MAX,
            user: 0,
            arrived: false,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct CrowdConfig {
    pub dt: Fx,
    /// Neighbours closer than `radius_a + radius_b + separation` push each other apart.
    pub separation: Fx,
    pub separation_weight: Fx,
    /// Look-ahead for velocity-obstacle avoidance (seconds).
    pub horizon: Fx,
    /// Distance at which arrive starts slowing down.
    pub slow_radius: Fx,
    pub arrive_radius: Fx,
}

impl Default for CrowdConfig {
    fn default() -> Self {
        CrowdConfig {
            dt: Fx::from_ratio(1, 60),
            separation: Fx::HALF,
            separation_weight: Fx::from_int(2),
            horizon: Fx::from_int(2),
            slow_radius: Fx::from_int(2),
            arrive_radius: Fx::from_ratio(1, 4),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Crowd {
    pub cfg: CrowdConfig,
    pub agents: BTreeMap<u32, Agent>,
    pub fields: Vec<FlowField>,
    next_id: u32,
    pub tick: u32,
}

impl Crowd {
    pub fn new(cfg: CrowdConfig) -> Crowd {
        Crowd {
            cfg,
            agents: BTreeMap::new(),
            fields: Vec::new(),
            next_id: 1,
            tick: 0,
        }
    }
    pub fn add(&mut self, mesh: &NavMesh, mut agent: Agent) -> u32 {
        if let Some((p, q)) = mesh.nearest(agent.pos) {
            agent.poly = p;
            agent.pos = q;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.agents.insert(id, agent);
        id
    }
    pub fn remove(&mut self, id: u32) -> Option<Agent> {
        self.agents.remove(&id)
    }
    pub fn add_field(&mut self, field: FlowField) -> u32 {
        self.fields.push(field);
        (self.fields.len() - 1) as u32
    }

    /// Preferred velocity: flow field direction or arrive towards `target`, scaled by max speed.
    fn preferred(&self, mesh: &NavMesh, a: &Agent) -> FxVec2 {
        let (dir, remaining) = if let Some(f) = a.field.and_then(|i| self.fields.get(i as usize)) {
            let d = f.direction(a.poly, a.pos);
            let remaining = f.dist.get(a.poly as usize).copied().unwrap_or(Fx::ZERO)
                + (f.goal - a.pos).length();
            if a.poly == f.goal_poly {
                let to = f.goal - a.pos;
                (
                    if to.length_sq() == Fx::ZERO {
                        FxVec2::ZERO
                    } else {
                        to.normalized()
                    },
                    to.length(),
                )
            } else {
                (d, remaining)
            }
        } else if let Some(t) = a.target {
            if a.poly != u32::MAX && mesh.contains(a.poly, t) || mesh.raycast(a.pos, t).is_none() {
                let to = t - a.pos;
                (
                    if to.length_sq() == Fx::ZERO {
                        FxVec2::ZERO
                    } else {
                        to.normalized()
                    },
                    to.length(),
                )
            } else {
                match mesh.find_path(a.pos, t, u32::MAX) {
                    Some(path) if path.points.len() >= 2 => {
                        let to = path.points[1] - a.pos;
                        (
                            if to.length_sq() == Fx::ZERO {
                                FxVec2::ZERO
                            } else {
                                to.normalized()
                            },
                            path.cost,
                        )
                    }
                    _ => (FxVec2::ZERO, Fx::ZERO),
                }
            }
        } else {
            return FxVec2::ZERO;
        };
        let speed = if remaining < self.cfg.slow_radius {
            a.max_speed * remaining / self.cfg.slow_radius
        } else {
            a.max_speed
        };
        dir.scale(speed)
    }

    /// One tick: steer, avoid, move, slide along mesh borders, update polygons.
    pub fn step(&mut self, mesh: &NavMesh) {
        let dt = self.cfg.dt;
        let ids: Vec<u32> = self.agents.keys().copied().collect();
        let snapshot: Vec<(u32, FxVec2, FxVec2, Fx)> = ids
            .iter()
            .map(|&i| {
                (
                    i,
                    self.agents[&i].pos,
                    self.agents[&i].vel,
                    self.agents[&i].radius,
                )
            })
            .collect();
        let mut new_vel: Vec<(u32, FxVec2)> = Vec::with_capacity(ids.len());
        for &id in &ids {
            let a = &self.agents[&id];
            let mut desired = self.preferred(mesh, a);
            // separation
            let mut push = FxVec2::ZERO;
            for &(oid, opos, _, orad) in &snapshot {
                if oid == id {
                    continue;
                }
                let d = a.pos - opos;
                let min = a.radius + orad + self.cfg.separation;
                let len = d.length();
                if len < min && len > Fx::ZERO {
                    push = push + d.scale((min - len) / len);
                }
            }
            desired = desired + push.scale(self.cfg.separation_weight);
            if desired.length_sq() > a.max_speed * a.max_speed {
                desired = desired.normalized().scale(a.max_speed);
            }
            // sampled velocity-obstacle avoidance: pick the candidate closest to `desired` whose
            // earliest collision with a neighbour is beyond the horizon (or the latest one)
            let best = self.avoid(id, a, desired, &snapshot);
            // acceleration limit
            let dv = best - a.vel;
            let max_dv = a.max_accel * dt;
            let v = if dv.length_sq() > max_dv * max_dv {
                a.vel + dv.normalized().scale(max_dv)
            } else {
                best
            };
            new_vel.push((id, v));
        }
        for (id, v) in new_vel {
            let a = self.agents.get_mut(&id).unwrap();
            a.vel = v;
            let want = a.pos + v.scale(dt);
            // stay on the mesh: slide along the boundary edge that blocks the move
            let mut pos = want;
            if mesh.locate(want).is_none() {
                if let Some(hit) = mesh.raycast(a.pos, want) {
                    let (ea, eb) = mesh.edge_pub(hit.poly, hit.edge);
                    let edge = (eb - ea).normalized();
                    let slide = edge.scale((want - hit.point).dot(edge));
                    let cand = hit.point + slide;
                    pos = if mesh.locate(cand).is_some() {
                        cand
                    } else {
                        mesh.nearest(cand).map(|(_, q)| q).unwrap_or(hit.point)
                    };
                } else {
                    pos = mesh.nearest(want).map(|(_, q)| q).unwrap_or(a.pos);
                }
            }
            // nudge inside so border points do not fail `locate` on the next tick
            a.pos = pos;
            a.poly = mesh
                .locate(pos)
                .or_else(|| mesh.nearest(pos).map(|(p, _)| p))
                .unwrap_or(a.poly);
            if let Some(t) = a.target {
                if (t - a.pos).length() <= self.cfg.arrive_radius {
                    a.arrived = true;
                    a.vel = FxVec2::ZERO;
                }
            }
            if let Some(f) = a.field.and_then(|i| self.fields.get(i as usize)) {
                if (f.goal - a.pos).length() <= self.cfg.arrive_radius {
                    a.arrived = true;
                    a.vel = FxVec2::ZERO;
                }
            }
        }
        self.depenetrate(mesh);
        self.tick += 1;
    }

    /// Hard overlap resolution: one pass pushing each overlapping pair apart (id order), then
    /// re-snapping to the mesh, so crowding at a goal never stacks agents on top of each other.
    fn depenetrate(&mut self, mesh: &NavMesh) {
        let ids: Vec<u32> = self.agents.keys().copied().collect();
        for i in 0..ids.len() {
            for j in i + 1..ids.len() {
                let (a, b) = (&self.agents[&ids[i]], &self.agents[&ids[j]]);
                let d = b.pos - a.pos;
                let min = a.radius + b.radius;
                let len = d.length();
                if len >= min {
                    continue;
                }
                let push = if len == Fx::ZERO {
                    FxVec2::new(min * Fx::HALF, Fx::ZERO)
                } else {
                    d.scale((min - len) * Fx::HALF / len)
                };
                let (pa, pb) = (a.pos - push, b.pos + push);
                let snap = |p: FxVec2, fallback: FxVec2| {
                    if mesh.locate(p).is_some() {
                        p
                    } else {
                        mesh.nearest(p).map(|(_, q)| q).unwrap_or(fallback)
                    }
                };
                let (pa, pb) = (snap(pa, a.pos), snap(pb, b.pos));
                let (qa, qb) = (mesh.locate(pa), mesh.locate(pb));
                let a = self.agents.get_mut(&ids[i]).unwrap();
                a.pos = pa;
                if let Some(q) = qa {
                    a.poly = q;
                }
                let b = self.agents.get_mut(&ids[j]).unwrap();
                b.pos = pb;
                if let Some(q) = qb {
                    b.poly = q;
                }
            }
        }
    }

    fn avoid(
        &self,
        id: u32,
        a: &Agent,
        desired: FxVec2,
        others: &[(u32, FxVec2, FxVec2, Fx)],
    ) -> FxVec2 {
        if desired.length_sq() == Fx::ZERO {
            return desired;
        }
        let ttc = |v: FxVec2| -> Fx {
            let mut earliest = self.cfg.horizon;
            for &(oid, opos, ovel, orad) in others {
                if oid == id {
                    continue;
                }
                let rel_p = opos - a.pos;
                if rel_p.length_sq()
                    > (a.max_speed * self.cfg.horizon * Fx::from_int(2))
                        .saturating_mul(a.max_speed * self.cfg.horizon * Fx::from_int(2))
                {
                    continue;
                }
                let rel_v = v - ovel;
                let r = a.radius + orad;
                // solve |rel_p + rel_v t| = r
                let aa = rel_v.length_sq();
                if aa == Fx::ZERO {
                    continue;
                }
                let bb = rel_p.dot(rel_v);
                let cc = rel_p.length_sq() - r * r;
                if cc < Fx::ZERO {
                    earliest = Fx::ZERO;
                    continue;
                }
                let disc = bb * bb - aa * cc;
                if disc < Fx::ZERO || bb >= Fx::ZERO {
                    continue;
                }
                let t = (-bb - disc.sqrt()) / aa;
                if t < earliest {
                    earliest = t;
                }
            }
            earliest
        };
        if ttc(desired) >= self.cfg.horizon {
            return desired;
        }
        let speed = desired.length();
        let dir = desired.scale(Fx::ONE / speed);
        let perp = FxVec2::new(-dir.y, dir.x);
        let mut best = desired;
        let mut best_score = Fx::MIN;
        // candidates: rotate ±20°, ±45°, ±70° at full and half speed, plus stop
        let angles = [
            (Fx::from_ratio(94, 100), Fx::from_ratio(34, 100)),
            (Fx::from_ratio(71, 100), Fx::from_ratio(71, 100)),
            (Fx::from_ratio(34, 100), Fx::from_ratio(94, 100)),
        ];
        let consider = |v: FxVec2, best: &mut FxVec2, best_score: &mut Fx| {
            let t = ttc(v);
            let penalty = (v - desired).length();
            let score = t * Fx::from_int(4) - penalty;
            if score > *best_score {
                *best_score = score;
                *best = v;
            }
        };
        for &(c, s) in &angles {
            for sign in [Fx::ONE, -Fx::ONE] {
                let d = dir.scale(c) + perp.scale(s * sign);
                consider(d.scale(speed), &mut best, &mut best_score);
                consider(d.scale(speed * Fx::HALF), &mut best, &mut best_score);
            }
        }
        consider(desired.scale(Fx::HALF), &mut best, &mut best_score);
        consider(FxVec2::ZERO, &mut best, &mut best_score);
        best
    }

    /// FNV hash of positions and velocities in id order.
    pub fn hash(&self) -> u64 {
        let pts: Vec<FxVec2> = self.agents.values().flat_map(|a| [a.pos, a.vel]).collect();
        hash_points(&pts) ^ (self.tick as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15)
    }
    pub fn snapshot(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }
    pub fn restore(b: &[u8]) -> Option<Crowd> {
        serde_json::from_slice(b).ok()
    }
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
    fn walled() -> NavMesh {
        let blocked: Vec<(u32, u32)> = (0..5).map(|r| (4, r)).collect();
        NavMesh::grid(p(0, 0), fx(1), 8, 6, &blocked)
    }

    #[test]
    fn flow_field_descends_to_goal() {
        let m = walled();
        let goal = FxVec2::new(Fx::from_ratio(15, 2), Fx::HALF);
        let f = FlowField::build(&m, goal, u32::MAX).unwrap();
        for (i, n) in f.next.iter().enumerate() {
            let (nb, _) = n.expect("every cell reachable");
            if i as PolyId != f.goal_poly {
                assert!(
                    f.dist[nb as usize] < f.dist[i],
                    "poly {i} -> {nb} does not descend"
                );
            }
        }
        let far = m.locate(FxVec2::new(Fx::HALF, Fx::HALF)).unwrap();
        assert!(
            f.dist[far as usize].to_f64() > 11.0,
            "wall not respected: {}",
            f.dist[far as usize]
        );
    }

    #[test]
    fn crowd_crosses_the_gap_without_leaving_the_mesh() {
        let m = walled();
        let build = || {
            let mut c = Crowd::new(CrowdConfig::default());
            let field = c.add_field(
                FlowField::build(&m, FxVec2::new(Fx::from_ratio(15, 2), Fx::HALF), u32::MAX)
                    .unwrap(),
            );
            for i in 0..6 {
                let mut a = Agent::new(
                    FxVec2::new(
                        Fx::HALF + Fx::from_ratio(i % 3, 2),
                        Fx::HALF + Fx::from_ratio(i / 3, 1),
                    ),
                    Fx::from_ratio(1, 4),
                    fx(3),
                );
                a.field = Some(field);
                c.add(&m, a);
            }
            // two agents walking head-on along the bottom-right corridor, by target
            let mut l = Agent::new(
                FxVec2::new(Fx::from_ratio(11, 2), Fx::HALF),
                Fx::from_ratio(1, 4),
                fx(2),
            );
            l.target = Some(FxVec2::new(Fx::from_ratio(15, 2), Fx::from_ratio(11, 2)));
            c.add(&m, l);
            let mut r = Agent::new(
                FxVec2::new(Fx::from_ratio(15, 2), Fx::from_ratio(11, 2)),
                Fx::from_ratio(1, 4),
                fx(2),
            );
            r.target = Some(FxVec2::new(Fx::from_ratio(11, 2), Fx::HALF));
            c.add(&m, r);
            c
        };
        let mut c = build();
        let mut c2 = build();
        let mut min_pair = Fx::MAX;
        for _ in 0..900 {
            c.step(&m);
            c2.step(&m);
            let v: Vec<&Agent> = c.agents.values().collect();
            for a in &v {
                assert!(
                    m.locate(a.pos).is_some()
                        || m.nearest(a.pos)
                            .map(|(_, q)| (q - a.pos).length() < Fx::from_ratio(1, 100))
                            .unwrap_or(false),
                    "agent left the mesh at {:?}",
                    a.pos
                );
            }
            for i in 0..v.len() {
                for j in i + 1..v.len() {
                    min_pair = min_pair.min((v[i].pos - v[j].pos).length());
                }
            }
        }
        assert_eq!(c.hash(), c2.hash());
        let goal = FxVec2::new(Fx::from_ratio(15, 2), Fx::HALF);
        let near_goal = c
            .agents
            .values()
            .filter(|a| a.field.is_some() && (a.pos - goal).length() < Fx::from_ratio(3, 2))
            .count();
        assert_eq!(
            near_goal,
            6,
            "flow-field agents not around the goal: {:?}",
            c.agents.values().map(|a| a.pos).collect::<Vec<_>>()
        );
        let arrived = c
            .agents
            .values()
            .filter(|a| a.target.is_some() && a.arrived)
            .count();
        assert_eq!(arrived, 2, "head-on agents did not swap places");
        assert!(
            min_pair.to_f64() > 0.2,
            "agents overlapped: min distance {min_pair}"
        );
        let snap = c.snapshot();
        let back = Crowd::restore(&snap).unwrap();
        assert_eq!(back.hash(), c.hash());
    }
}
