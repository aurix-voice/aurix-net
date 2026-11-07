//! aurix-sim phase 1: deterministic 2D rigid bodies in Q32.32 fixed point (see docs/sim/design.md).
//! No floats, no hash-map iteration, no time: the same inputs give the same bytes on every CPU and
//! on wasm32, so a world can be hashed for lockstep verification, snapshotted and rolled back.
//!
//! Milestone 1: dynamic/static bodies, circles and convex polygons, uniform-grid broadphase with
//! sorted pairs, SAT narrowphase with two-point manifolds, sequential-impulse solver with warm
//! starting, raycasts, snapshot/restore/hash.
#![forbid(unsafe_code)]
#![deny(clippy::float_arithmetic)]

use net_fpmath::{Fx, FxVec2};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct BodyId(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum BodyKind {
    Static,
    Kinematic,
    Dynamic,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Shape {
    Circle {
        radius: Fx,
    },
    /// Convex, counter-clockwise, <= 8 vertices in body space.
    Polygon {
        vertices: Vec<FxVec2>,
    },
    /// Segment along the body x axis from -half_len to +half_len, inflated by `radius`.
    Capsule {
        half_len: Fx,
        radius: Fx,
    },
    /// Open polyline for terrain (static bodies); every segment collides on both sides.
    Chain {
        points: Vec<FxVec2>,
    },
}

impl Shape {
    pub fn rect(half_w: Fx, half_h: Fx) -> Shape {
        Shape::Polygon {
            vertices: vec![
                FxVec2::new(-half_w, -half_h),
                FxVec2::new(half_w, -half_h),
                FxVec2::new(half_w, half_h),
                FxVec2::new(-half_w, half_h),
            ],
        }
    }
    fn radius_bound(&self) -> Fx {
        match self {
            Shape::Circle { radius } => *radius,
            Shape::Polygon { vertices } => vertices
                .iter()
                .map(|v| v.length())
                .fold(Fx::ZERO, |a, b| a.max(b)),
            Shape::Capsule { half_len, radius } => *half_len + *radius,
            Shape::Chain { points } => points
                .iter()
                .map(|v| v.length())
                .fold(Fx::ZERO, |a, b| a.max(b)),
        }
    }
    /// Core points in body space plus the rounding radius (circle = 1 point, capsule = 2).
    fn rounded(&self) -> (Vec<FxVec2>, Fx) {
        match self {
            Shape::Circle { radius } => (vec![FxVec2::default()], *radius),
            Shape::Polygon { vertices } => (vertices.clone(), Fx::ZERO),
            Shape::Capsule { half_len, radius } => (
                vec![
                    FxVec2::new(-*half_len, Fx::ZERO),
                    FxVec2::new(*half_len, Fx::ZERO),
                ],
                *radius,
            ),
            Shape::Chain { points } => (points.clone(), Fx::ZERO),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Body {
    pub kind: BodyKind,
    pub shape: Shape,
    pub position: FxVec2,
    /// Rotation angle in radians (Fx); sin/cos are recomputed deterministically when it changes.
    pub angle: Fx,
    pub velocity: FxVec2,
    pub angular_velocity: Fx,
    pub density: Fx,
    pub friction: Fx,
    pub restitution: Fx,
    pub linear_damping: Fx,
    pub layer: u32,
    pub mask: u32,
    pub sensor: bool,
    pub user: u64,
    inv_mass: Fx,
    inv_inertia: Fx,
}

impl Body {
    pub fn new(kind: BodyKind, shape: Shape, position: FxVec2) -> Body {
        let mut b = Body {
            kind,
            shape,
            position,
            angle: Fx::ZERO,
            velocity: FxVec2::default(),
            angular_velocity: Fx::ZERO,
            density: Fx::ONE,
            friction: Fx::from_ratio(1, 2),
            restitution: Fx::ZERO,
            linear_damping: Fx::ZERO,
            layer: 1,
            mask: u32::MAX,
            sensor: false,
            user: 0,
            inv_mass: Fx::ZERO,
            inv_inertia: Fx::ZERO,
        };
        b.recompute_mass();
        b
    }
    pub fn with_density(mut self, d: Fx) -> Body {
        self.density = d;
        self.recompute_mass();
        self
    }
    pub fn with_material(mut self, friction: Fx, restitution: Fx) -> Body {
        self.friction = friction;
        self.restitution = restitution;
        self
    }
    pub fn mass(&self) -> Fx {
        if self.inv_mass == Fx::ZERO {
            Fx::ZERO
        } else {
            Fx::ONE / self.inv_mass
        }
    }
    fn recompute_mass(&mut self) {
        if self.kind != BodyKind::Dynamic {
            self.inv_mass = Fx::ZERO;
            self.inv_inertia = Fx::ZERO;
            return;
        }
        let (area, inertia_per_mass) = match &self.shape {
            Shape::Circle { radius } => {
                (PI * *radius * *radius, *radius * *radius / Fx::from_int(2))
            }
            Shape::Chain { .. } => (Fx::ZERO, Fx::ZERO),
            Shape::Capsule { half_len, radius } => {
                let (w, h) = (*half_len * Fx::from_int(2), *radius * Fx::from_int(2));
                let area = w * h + PI * *radius * *radius;
                (area, (w * w + h * h) / Fx::from_int(12))
            }
            Shape::Polygon { vertices } => {
                // shoelace area and second moment about the origin
                let mut area = Fx::ZERO;
                let mut i2 = Fx::ZERO;
                for i in 0..vertices.len() {
                    let a = vertices[i];
                    let b = vertices[(i + 1) % vertices.len()];
                    let c = cross(a, b);
                    area += c;
                    i2 += c * (a.dot(a) + a.dot(b) + b.dot(b));
                }
                let area = area / Fx::from_int(2);
                let i2 = i2 / Fx::from_int(12);
                (
                    area.abs(),
                    if area == Fx::ZERO {
                        Fx::ZERO
                    } else {
                        (i2 / area).abs()
                    },
                )
            }
        };
        let mass = area * self.density;
        self.inv_mass = if mass > Fx::ZERO {
            Fx::ONE / mass
        } else {
            Fx::ZERO
        };
        let inertia = mass * inertia_per_mass;
        self.inv_inertia = if inertia > Fx::ZERO {
            Fx::ONE / inertia
        } else {
            Fx::ZERO
        };
    }
    fn aabb(&self) -> (FxVec2, FxVec2) {
        let r = self.shape.radius_bound();
        (
            FxVec2::new(self.position.x - r, self.position.y - r),
            FxVec2::new(self.position.x + r, self.position.y + r),
        )
    }
    fn world_points(&self) -> (Vec<FxVec2>, Fx) {
        let (pts, r) = self.shape.rounded();
        let (sn, cs) = (self.angle.sin(), self.angle.cos());
        (
            pts.iter()
                .map(|v| rotate(*v, sn, cs) + self.position)
                .collect(),
            r,
        )
    }
}

const PI: Fx = Fx(0x3_243F_6A88);

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct WorldConfig {
    pub gravity: FxVec2,
    /// Fixed step, e.g. `Fx::from_ratio(1, 60)`.
    pub dt: Fx,
    pub velocity_iterations: u8,
    pub position_iterations: u8,
    /// Broadphase grid cell size; defaults to 4 world units.
    pub cell: Fx,
    pub slop: Fx,
    pub baumgarte: Fx,
    pub restitution_threshold: Fx,
}

impl Default for WorldConfig {
    fn default() -> Self {
        Self {
            gravity: FxVec2::new(Fx::ZERO, Fx::from_int(-10)),
            dt: Fx::from_ratio(1, 60),
            velocity_iterations: 8,
            position_iterations: 3,
            cell: Fx::from_int(4),
            slop: Fx::from_ratio(1, 200),
            baumgarte: Fx::from_ratio(1, 5),
            restitution_threshold: Fx::ONE,
        }
    }
}

/// One contact point with its accumulated impulses (kept between steps for warm starting).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
struct Contact {
    point: FxVec2,
    normal: FxVec2,
    penetration: Fx,
    feature: u8,
    normal_impulse: Fx,
    tangent_impulse: Fx,
    /// Restitution target from the pre-solve approach speed (Box2D velocity bias).
    bias: Fx,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
struct Manifold {
    a: BodyId,
    b: BodyId,
    contacts: Vec<Contact>,
    touching: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ContactEvent {
    pub a: BodyId,
    pub b: BodyId,
    pub begin: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RayHit {
    pub body: BodyId,
    pub point: FxVec2,
    pub normal: FxVec2,
    pub fraction: Fx,
}

/// The whole simulation state; `Clone` is the snapshot, `hash()` the lockstep checksum.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct World {
    pub cfg: WorldConfig,
    bodies: BTreeMap<BodyId, Body>,
    next_id: u32,
    manifolds: Vec<Manifold>,
    pub tick: u32,
    events: Vec<ContactEvent>,
}

impl World {
    pub fn new(cfg: WorldConfig) -> World {
        World {
            cfg,
            bodies: BTreeMap::new(),
            next_id: 1,
            manifolds: Vec::new(),
            tick: 0,
            events: Vec::new(),
        }
    }
    pub fn add(&mut self, body: Body) -> BodyId {
        let id = BodyId(self.next_id);
        self.next_id += 1;
        self.bodies.insert(id, body);
        id
    }
    pub fn remove(&mut self, id: BodyId) -> Option<Body> {
        self.manifolds.retain(|m| m.a != id && m.b != id);
        self.bodies.remove(&id)
    }
    pub fn body(&self, id: BodyId) -> Option<&Body> {
        self.bodies.get(&id)
    }
    pub fn body_mut(&mut self, id: BodyId) -> Option<&mut Body> {
        self.bodies.get_mut(&id)
    }
    pub fn bodies(&self) -> impl Iterator<Item = (BodyId, &Body)> {
        self.bodies.iter().map(|(k, v)| (*k, v))
    }
    /// Contact begin/end events of the last step, in (a, b) order.
    pub fn events(&self) -> &[ContactEvent] {
        &self.events
    }
    pub fn apply_impulse(&mut self, id: BodyId, impulse: FxVec2) {
        if let Some(b) = self.bodies.get_mut(&id) {
            b.velocity = b.velocity + impulse.scale(b.inv_mass);
        }
    }
    pub fn snapshot(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }
    pub fn restore(bytes: &[u8]) -> Option<World> {
        serde_json::from_slice(bytes).ok()
    }
    /// FNV-1a 64 over the canonical snapshot bytes (same hash family as the SDK lockstep cores).
    pub fn hash(&self) -> u64 {
        self.snapshot().iter().fold(0xcbf29ce484222325u64, |h, &x| {
            (h ^ x as u64).wrapping_mul(0x100000001b3)
        })
    }

    pub fn step(&mut self) {
        let dt = self.cfg.dt;
        let g = self.cfg.gravity;
        for b in self.bodies.values_mut() {
            if b.kind == BodyKind::Dynamic {
                b.velocity = b.velocity + g.scale(dt);
                let damp = (Fx::ONE - b.linear_damping * dt).clamp(Fx::ZERO, Fx::ONE);
                b.velocity = b.velocity.scale(damp);
            }
        }
        self.update_manifolds();
        self.prepare_contacts();
        self.warm_start();
        for _ in 0..self.cfg.velocity_iterations {
            self.solve_velocities();
        }
        for b in self.bodies.values_mut() {
            if b.kind != BodyKind::Static {
                b.position = b.position + b.velocity.scale(dt);
                b.angle += b.angular_velocity * dt;
            }
        }
        for _ in 0..self.cfg.position_iterations {
            self.solve_positions();
        }
        self.tick += 1;
    }

    fn pairs(&self) -> Vec<(BodyId, BodyId)> {
        let cell = self.cfg.cell;
        let mut grid: BTreeMap<(i32, i32), Vec<BodyId>> = BTreeMap::new();
        for (id, b) in &self.bodies {
            let (lo, hi) = b.aabb();
            let (x0, y0) = ((lo.x / cell).floor_int(), (lo.y / cell).floor_int());
            let (x1, y1) = ((hi.x / cell).floor_int(), (hi.y / cell).floor_int());
            for x in x0..=x1 {
                for y in y0..=y1 {
                    grid.entry((x, y)).or_default().push(*id);
                }
            }
        }
        let mut pairs = Vec::new();
        for ids in grid.values() {
            for i in 0..ids.len() {
                for j in i + 1..ids.len() {
                    let (a, b) = (ids[i].min(ids[j]), ids[i].max(ids[j]));
                    let (ba, bb) = (&self.bodies[&a], &self.bodies[&b]);
                    if ba.kind != BodyKind::Dynamic && bb.kind != BodyKind::Dynamic {
                        continue;
                    }
                    if ba.layer & bb.mask == 0 || bb.layer & ba.mask == 0 {
                        continue;
                    }
                    let (la, ha) = ba.aabb();
                    let (lb, hb) = bb.aabb();
                    if ha.x < lb.x || hb.x < la.x || ha.y < lb.y || hb.y < la.y {
                        continue;
                    }
                    pairs.push((a, b));
                }
            }
        }
        pairs.sort();
        pairs.dedup();
        pairs
    }

    fn update_manifolds(&mut self) {
        let pairs = self.pairs();
        let old = std::mem::take(&mut self.manifolds);
        let mut events = Vec::new();
        let mut oi = 0;
        for (a, b) in pairs {
            let contacts = collide(&self.bodies[&a], &self.bodies[&b]);
            let sensor = self.bodies[&a].sensor || self.bodies[&b].sensor;
            // old manifolds are sorted the same way: advance to this pair, dropping ended ones
            let mut prev: Option<&Manifold> = None;
            while oi < old.len() && (old[oi].a, old[oi].b) < (a, b) {
                if old[oi].touching {
                    events.push(ContactEvent {
                        a: old[oi].a,
                        b: old[oi].b,
                        begin: false,
                    });
                }
                oi += 1;
            }
            if oi < old.len() && (old[oi].a, old[oi].b) == (a, b) {
                prev = Some(&old[oi]);
                oi += 1;
            }
            let touching = !contacts.is_empty();
            let was = prev.is_some_and(|m| m.touching);
            if touching != was {
                events.push(ContactEvent {
                    a,
                    b,
                    begin: touching,
                });
            }
            let contacts = if sensor {
                Vec::new()
            } else {
                contacts
                    .into_iter()
                    .map(|mut c| {
                        if let Some(p) =
                            prev.and_then(|m| m.contacts.iter().find(|o| o.feature == c.feature))
                        {
                            c.normal_impulse = p.normal_impulse;
                            c.tangent_impulse = p.tangent_impulse;
                        }
                        c
                    })
                    .collect()
            };
            self.manifolds.push(Manifold {
                a,
                b,
                contacts,
                touching,
            });
        }
        for m in &old[oi..] {
            if m.touching {
                events.push(ContactEvent {
                    a: m.a,
                    b: m.b,
                    begin: false,
                });
            }
        }
        events.sort_by_key(|e| (e.a, e.b, e.begin));
        self.events = events;
    }

    /// Restitution uses the approach speed before any impulse of this step is applied.
    fn prepare_contacts(&mut self) {
        let threshold = self.cfg.restitution_threshold;
        for m in &mut self.manifolds {
            let a = &self.bodies[&m.a];
            let b = &self.bodies[&m.b];
            let restitution = a.restitution.max(b.restitution);
            for c in &mut m.contacts {
                let ra = c.point - a.position;
                let rb = c.point - b.position;
                let rel = (b.velocity + perp(rb).scale(b.angular_velocity))
                    - (a.velocity + perp(ra).scale(a.angular_velocity));
                let vn = rel.dot(c.normal);
                c.bias = if -vn > threshold {
                    -vn * restitution
                } else {
                    Fx::ZERO
                };
            }
        }
    }

    fn warm_start(&mut self) {
        for m in &self.manifolds {
            for c in &m.contacts {
                let p = c.normal.scale(c.normal_impulse) + perp(c.normal).scale(c.tangent_impulse);
                let a = &self.bodies[&m.a];
                let b = &self.bodies[&m.b];
                let (va, wa, vb, wb) = apply_pair(a, b, c.point, p);
                let ba = self.bodies.get_mut(&m.a).unwrap();
                ba.velocity = va;
                ba.angular_velocity = wa;
                let bb = self.bodies.get_mut(&m.b).unwrap();
                bb.velocity = vb;
                bb.angular_velocity = wb;
            }
        }
    }

    fn solve_velocities(&mut self) {
        let mut manifolds = std::mem::take(&mut self.manifolds);
        for m in &mut manifolds {
            let friction = self.bodies[&m.a].friction.max(self.bodies[&m.b].friction);
            for c in &mut m.contacts {
                let a = &self.bodies[&m.a];
                let b = &self.bodies[&m.b];
                let ra = c.point - a.position;
                let rb = c.point - b.position;
                let rel = (b.velocity + perp(rb).scale(b.angular_velocity))
                    - (a.velocity + perp(ra).scale(a.angular_velocity));
                let vn = rel.dot(c.normal);
                let k = a.inv_mass
                    + b.inv_mass
                    + sq(cross(ra, c.normal)) * a.inv_inertia
                    + sq(cross(rb, c.normal)) * b.inv_inertia;
                if k == Fx::ZERO {
                    continue;
                }
                let mut j = (-vn + c.bias) / k;
                let old = c.normal_impulse;
                c.normal_impulse = (old + j).max(Fx::ZERO);
                j = c.normal_impulse - old;
                let p = c.normal.scale(j);
                let (va, wa, vb, wb) = apply_pair(a, b, c.point, p);
                set_vel(&mut self.bodies, m.a, va, wa);
                set_vel(&mut self.bodies, m.b, vb, wb);
                // friction along the tangent
                let a = &self.bodies[&m.a];
                let b = &self.bodies[&m.b];
                let t = perp(c.normal);
                let rel = (b.velocity + perp(rb).scale(b.angular_velocity))
                    - (a.velocity + perp(ra).scale(a.angular_velocity));
                let kt = a.inv_mass
                    + b.inv_mass
                    + sq(cross(ra, t)) * a.inv_inertia
                    + sq(cross(rb, t)) * b.inv_inertia;
                if kt == Fx::ZERO {
                    continue;
                }
                let jt = -rel.dot(t) / kt;
                let max = friction * c.normal_impulse;
                let old = c.tangent_impulse;
                c.tangent_impulse = (old + jt).clamp(-max, max);
                let p = t.scale(c.tangent_impulse - old);
                let (va, wa, vb, wb) = apply_pair(a, b, c.point, p);
                set_vel(&mut self.bodies, m.a, va, wa);
                set_vel(&mut self.bodies, m.b, vb, wb);
            }
        }
        self.manifolds = manifolds;
    }

    fn solve_positions(&mut self) {
        let (slop, k) = (self.cfg.slop, self.cfg.baumgarte);
        let manifolds = std::mem::take(&mut self.manifolds);
        for m in &manifolds {
            for c in &m.contacts {
                let a = &self.bodies[&m.a];
                let b = &self.bodies[&m.b];
                let total = a.inv_mass + b.inv_mass;
                if total == Fx::ZERO {
                    continue;
                }
                let depth = (c.penetration - slop).max(Fx::ZERO);
                let corr = c.normal.scale(depth * k / total);
                let (ia, ib, pa, pb) = (a.inv_mass, b.inv_mass, a.position, b.position);
                self.bodies.get_mut(&m.a).unwrap().position = pa - corr.scale(ia);
                self.bodies.get_mut(&m.b).unwrap().position = pb + corr.scale(ib);
            }
        }
        self.manifolds = manifolds;
    }

    /// Closest hit along `from + dir * t`, `t` in 0..=1, bodies whose layer matches `mask`.
    pub fn raycast(&self, from: FxVec2, dir: FxVec2, mask: u32) -> Option<RayHit> {
        let mut best: Option<RayHit> = None;
        for (id, b) in &self.bodies {
            if b.layer & mask == 0 {
                continue;
            }
            let (pts, r) = b.world_points();
            let hit = match &b.shape {
                Shape::Circle { .. } => ray_circle(from, dir, b.position, r),
                Shape::Polygon { .. } => ray_polygon(from, dir, &pts),
                Shape::Capsule { .. } => ray_capsule(from, dir, pts[0], pts[1], r),
                Shape::Chain { .. } => pts
                    .windows(2)
                    .filter_map(|s| ray_segment(from, dir, s[0], s[1]))
                    .min_by_key(|h| h.0),
            };
            if let Some((t, n)) = hit {
                if best.is_none_or(|h| t < h.fraction) {
                    best = Some(RayHit {
                        body: *id,
                        point: from + dir.scale(t),
                        normal: n,
                        fraction: t,
                    });
                }
            }
        }
        best
    }
}

fn set_vel(bodies: &mut BTreeMap<BodyId, Body>, id: BodyId, v: FxVec2, w: Fx) {
    let b = bodies.get_mut(&id).unwrap();
    b.velocity = v;
    b.angular_velocity = w;
}

fn apply_pair(a: &Body, b: &Body, point: FxVec2, p: FxVec2) -> (FxVec2, Fx, FxVec2, Fx) {
    let ra = point - a.position;
    let rb = point - b.position;
    (
        a.velocity - p.scale(a.inv_mass),
        a.angular_velocity - cross(ra, p) * a.inv_inertia,
        b.velocity + p.scale(b.inv_mass),
        b.angular_velocity + cross(rb, p) * b.inv_inertia,
    )
}

fn sq(x: Fx) -> Fx {
    x * x
}
fn cross(a: FxVec2, b: FxVec2) -> Fx {
    a.x * b.y - a.y * b.x
}
fn perp(v: FxVec2) -> FxVec2 {
    FxVec2::new(-v.y, v.x)
}
fn rotate(v: FxVec2, s: Fx, c: Fx) -> FxVec2 {
    FxVec2::new(v.x * c - v.y * s, v.x * s + v.y * c)
}

/// Contacts between `a` and `b` with the normal pointing from a to b.
fn collide(a: &Body, b: &Body) -> Vec<Contact> {
    let (pa, ra) = a.world_points();
    let (pb, rb) = b.world_points();
    match (&a.shape, &b.shape) {
        (Shape::Chain { .. }, Shape::Chain { .. }) => Vec::new(),
        (Shape::Chain { .. }, _) => pa
            .windows(2)
            .enumerate()
            .flat_map(|(i, seg)| {
                collide_rounded(seg, Fx::ZERO, &pb, rb)
                    .into_iter()
                    .map(move |mut c| {
                        c.feature = (i as u8).wrapping_mul(16).wrapping_add(c.feature);
                        c
                    })
            })
            .collect(),
        (_, Shape::Chain { .. }) => pb
            .windows(2)
            .enumerate()
            .flat_map(|(i, seg)| {
                collide_rounded(&pa, ra, seg, Fx::ZERO)
                    .into_iter()
                    .map(move |mut c| {
                        c.feature = (i as u8).wrapping_mul(16).wrapping_add(c.feature);
                        c
                    })
            })
            .collect(),
        _ => collide_rounded(&pa, ra, &pb, rb),
    }
}

fn contact(point: FxVec2, normal: FxVec2, penetration: Fx, feature: u8) -> Contact {
    Contact {
        point,
        normal,
        penetration,
        feature,
        normal_impulse: Fx::ZERO,
        tangent_impulse: Fx::ZERO,
        bias: Fx::ZERO,
    }
}

/// Rounded convex cores: a point (circle), a segment (capsule, chain edge) or a polygon, each
/// inflated by its radius.
fn collide_rounded(pa: &[FxVec2], ra: Fx, pb: &[FxVec2], rb: Fx) -> Vec<Contact> {
    match (pa.len(), pb.len()) {
        (1, 1) => {
            let d = pb[0] - pa[0];
            let dist = d.length();
            let pen = ra + rb - dist;
            if pen <= Fx::ZERO {
                return Vec::new();
            }
            let n = if dist > Fx::ZERO {
                d.scale(Fx::ONE / dist)
            } else {
                FxVec2::new(Fx::ZERO, Fx::ONE)
            };
            vec![contact(
                pa[0] + n.scale(ra - pen / Fx::from_int(2)),
                n,
                pen,
                0,
            )]
        }
        (1, _) => circle_polygon(pa[0], ra, pb, rb, false),
        (_, 1) => circle_polygon(pb[0], rb, pa, ra, true),
        _ => polygon_polygon(pa, ra, pb, rb),
    }
}

fn edge_normal(v: &[FxVec2], i: usize) -> FxVec2 {
    let e = v[(i + 1) % v.len()] - v[i];
    FxVec2::new(e.y, -e.x).normalized()
}

/// `flip`: the circle is body b (normal must point from polygon to circle); `rp` inflates the polygon.
fn circle_polygon(c: FxVec2, rc: Fx, v: &[FxVec2], rp: Fx, flip: bool) -> Vec<Contact> {
    let r = rc + rp;
    // deepest edge: largest separation of the center along outward normals
    let mut best = (Fx::from_int(-1_000_000), 0);
    for i in 0..v.len() {
        let s = (c - v[i]).dot(edge_normal(v, i));
        if s > r {
            return Vec::new();
        }
        if s > best.0 {
            best = (s, i);
        }
    }
    let (sep, i) = best;
    let (p1, p2) = (v[i], v[(i + 1) % v.len()]);
    let n_out = edge_normal(v, i);
    // voronoi regions of the edge's vertices: (closest point, unit normal polygon->circle, distance)
    let vertex = |p: FxVec2| {
        let d = c - p;
        let len = d.length();
        let n = if len > Fx::ZERO {
            d.scale(Fx::ONE / len)
        } else {
            n_out
        };
        (p, n, len)
    };
    let (closest, n_pc, dist) = if (c - p1).dot(p2 - p1) <= Fx::ZERO {
        vertex(p1)
    } else if (c - p2).dot(p1 - p2) <= Fx::ZERO {
        vertex(p2)
    } else {
        (c - n_out.scale(sep), n_out, sep)
    };
    if dist > r {
        return Vec::new();
    }
    let pen = r - dist;
    // normal from a to b: polygon->circle is n_pc; circle->polygon is -n_pc
    let normal = if flip {
        n_pc
    } else {
        FxVec2::new(-n_pc.x, -n_pc.y)
    };
    let point = closest + n_pc.scale((rp + (dist - rc)) / Fx::from_int(2));
    vec![contact(point, normal, pen, i as u8)]
}

fn least_penetration(a: &[FxVec2], b: &[FxVec2]) -> (Fx, usize) {
    let mut best = (Fx::from_int(-1_000_000), 0);
    for i in 0..a.len() {
        let n = edge_normal(a, i);
        let support = b
            .iter()
            .map(|p| (*p - a[i]).dot(n))
            .fold(Fx::from_int(1_000_000), |m, d| m.min(d));
        if support > best.0 {
            best = (support, i);
        }
    }
    best
}

fn clip(v1: FxVec2, v2: FxVec2, n: FxVec2, offset: Fx) -> Vec<FxVec2> {
    let d1 = n.dot(v1) - offset;
    let d2 = n.dot(v2) - offset;
    let mut out = Vec::with_capacity(2);
    if d1 <= Fx::ZERO {
        out.push(v1);
    }
    if d2 <= Fx::ZERO {
        out.push(v2);
    }
    if d1 * d2 < Fx::ZERO {
        let t = d1 / (d1 - d2);
        out.push(v1 + (v2 - v1).scale(t));
    }
    out
}

fn polygon_polygon(a: &[FxVec2], ra: Fx, b: &[FxVec2], rb: Fx) -> Vec<Contact> {
    let total = ra + rb;
    let (sa, ia) = least_penetration(a, b);
    if sa > total {
        return Vec::new();
    }
    let (sb, ib) = least_penetration(b, a);
    if sb > total {
        return Vec::new();
    }
    // reference face on the polygon with the smaller penetration (bias towards a for stability)
    let (reference, incident, ri, flip, rr, ri_r) = if sb > sa + Fx::from_ratio(1, 1000) {
        (b, a, ib, true, rb, ra)
    } else {
        (a, b, ia, false, ra, rb)
    };
    let n = edge_normal(reference, ri);
    // incident edge: the one most anti-parallel to the reference normal
    let mut inc = 0;
    let mut min_dot = Fx::from_int(1_000_000);
    for i in 0..incident.len() {
        let d = edge_normal(incident, i).dot(n);
        if d < min_dot {
            min_dot = d;
            inc = i;
        }
    }
    let (r1, r2) = (reference[ri], reference[(ri + 1) % reference.len()]);
    let (i1, i2) = (incident[inc], incident[(inc + 1) % incident.len()]);
    let side = (r2 - r1).normalized();
    let mut pts = clip(i1, i2, FxVec2::new(-side.x, -side.y), -side.dot(r1));
    if pts.len() < 2 {
        return Vec::new();
    }
    pts = clip(pts[0], pts[1], side, side.dot(r2));
    if pts.len() < 2 {
        return Vec::new();
    }
    let front = n.dot(r1);
    let normal = if flip { FxVec2::new(-n.x, -n.y) } else { n };
    let mut out = Vec::new();
    for (k, p) in pts.into_iter().enumerate() {
        let sep = n.dot(p) - front;
        if sep <= total {
            // midpoint between the two inflated surfaces
            let point = p - n.scale((sep - rr + ri_r) / Fx::from_int(2));
            out.push(contact(
                point,
                normal,
                total - sep,
                (ri * 8 + inc) as u8 ^ ((k as u8) << 7),
            ));
        }
    }
    out
}

fn ray_circle(from: FxVec2, dir: FxVec2, c: FxVec2, r: Fx) -> Option<(Fx, FxVec2)> {
    let m = from - c;
    let a = dir.dot(dir);
    if a == Fx::ZERO {
        return None;
    }
    let b = m.dot(dir);
    let cc = m.dot(m) - r * r;
    let disc = b * b - a * cc;
    if disc < Fx::ZERO {
        return None;
    }
    let t = (-b - disc.sqrt()) / a;
    if t < Fx::ZERO || t > Fx::ONE {
        return None;
    }
    let p = from + dir.scale(t);
    Some((t, (p - c).normalized()))
}

fn ray_segment(from: FxVec2, dir: FxVec2, a: FxVec2, b: FxVec2) -> Option<(Fx, FxVec2)> {
    let e = b - a;
    let den = cross(dir, e);
    if den == Fx::ZERO {
        return None;
    }
    let t = cross(a - from, e) / den;
    let u = cross(a - from, dir) / den;
    if t < Fx::ZERO || t > Fx::ONE || u < Fx::ZERO || u > Fx::ONE {
        return None;
    }
    let n = FxVec2::new(e.y, -e.x).normalized();
    Some((
        t,
        if n.dot(dir) > Fx::ZERO {
            FxVec2::new(-n.x, -n.y)
        } else {
            n
        },
    ))
}

fn ray_capsule(from: FxVec2, dir: FxVec2, a: FxVec2, b: FxVec2, r: Fx) -> Option<(Fx, FxVec2)> {
    let n = FxVec2::new((b - a).y, -(b - a).x).normalized().scale(r);
    [
        ray_segment(from, dir, a + n, b + n),
        ray_segment(from, dir, a - n, b - n),
        ray_circle(from, dir, a, r),
        ray_circle(from, dir, b, r),
    ]
    .into_iter()
    .flatten()
    .min_by_key(|h| h.0)
}

fn ray_polygon(from: FxVec2, dir: FxVec2, v: &[FxVec2]) -> Option<(Fx, FxVec2)> {
    let (mut lo, mut hi) = (Fx::ZERO, Fx::ONE);
    let mut normal = FxVec2::default();
    for i in 0..v.len() {
        let n = edge_normal(v, i);
        let num = n.dot(v[i] - from);
        let den = n.dot(dir);
        if den == Fx::ZERO {
            if num < Fx::ZERO {
                return None;
            }
        } else {
            let t = num / den;
            if den < Fx::ZERO {
                if t > lo {
                    lo = t;
                    normal = n;
                }
            } else {
                hi = hi.min(t);
            }
        }
        if lo > hi {
            return None;
        }
    }
    if normal == FxVec2::default() {
        return None;
    }
    Some((lo, normal))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fx(v: i32) -> Fx {
        Fx::from_int(v)
    }
    fn ground(w: &mut World) -> BodyId {
        w.add(Body::new(
            BodyKind::Static,
            Shape::rect(fx(50), fx(1)),
            FxVec2::new(fx(0), fx(-1)),
        ))
    }

    #[test]
    fn ball_falls_lands_and_rests() {
        let mut w = World::new(WorldConfig::default());
        ground(&mut w);
        let ball = w.add(Body::new(
            BodyKind::Dynamic,
            Shape::Circle { radius: Fx::ONE },
            FxVec2::new(fx(0), fx(10)),
        ));
        let mut landed_at = None;
        for t in 0..300 {
            w.step();
            if landed_at.is_none() && w.events().iter().any(|e| e.begin && e.b == ball) {
                landed_at = Some(t);
            }
        }
        let b = w.body(ball).unwrap();
        assert!(
            landed_at.is_some_and(|t| (60..100).contains(&t)),
            "landed at {landed_at:?}"
        );
        let y = b.position.y.to_f64();
        assert!((0.95..1.05).contains(&y), "resting height {y}");
        assert!(
            b.velocity.length().to_f64() < 0.05,
            "still moving {:?}",
            b.velocity
        );
    }

    #[test]
    fn boxes_stack_and_stay_deterministic() {
        let build = || {
            let mut w = World::new(WorldConfig::default());
            ground(&mut w);
            for i in 0..5 {
                w.add(Body::new(
                    BodyKind::Dynamic,
                    Shape::rect(Fx::ONE, Fx::ONE),
                    FxVec2::new(Fx::from_ratio(i as i64, 10), fx(1 + 2 * i)),
                ));
            }
            w
        };
        let (mut a, mut b) = (build(), build());
        for _ in 0..240 {
            a.step();
            b.step();
        }
        assert_eq!(a.hash(), b.hash());
        let top = a.bodies().last().unwrap().1;
        assert!(
            top.position.y.to_f64() > 8.0,
            "stack collapsed: top at {}",
            top.position.y
        );
        assert!(top.position.x.to_f64().abs() < 2.0);
        // snapshot/restore round-trips to the same hash and keeps stepping identically
        let snap = a.snapshot();
        let mut c = World::restore(&snap).unwrap();
        assert_eq!(c.hash(), a.hash());
        a.step();
        c.step();
        assert_eq!(c.hash(), a.hash());
        eprintln!("golden stack hash after 241 ticks: {:#x}", a.hash());
    }

    #[test]
    fn capsule_rests_flat_and_ball_rolls_down_chain() {
        let build = || {
            let mut w = World::new(WorldConfig::default());
            ground(&mut w);
            let cap = w.add(Body::new(
                BodyKind::Dynamic,
                Shape::Capsule {
                    half_len: fx(2),
                    radius: Fx::from_ratio(1, 2),
                },
                FxVec2::new(fx(20), fx(3)),
            ));
            let slope = w.add(Body::new(
                BodyKind::Static,
                Shape::Chain {
                    points: vec![
                        FxVec2::new(fx(-12), fx(8)),
                        FxVec2::new(fx(-2), fx(2)),
                        FxVec2::new(fx(8), fx(2)),
                        FxVec2::new(fx(8), fx(6)),
                    ],
                },
                FxVec2::new(fx(-20), fx(0)),
            ));
            let ball = w.add(Body::new(
                BodyKind::Dynamic,
                Shape::Circle { radius: Fx::ONE },
                FxVec2::new(fx(-30), fx(10)),
            ));
            (w, cap, slope, ball)
        };
        let (mut w, cap, _slope, ball) = build();
        let (mut w2, ..) = build();
        for _ in 0..360 {
            w.step();
            w2.step();
        }
        assert_eq!(w.hash(), w2.hash());
        let c = w.body(cap).unwrap();
        assert!(
            (0.45..0.55).contains(&c.position.y.to_f64()),
            "capsule height {}",
            c.position.y
        );
        assert!(c.angle.abs().to_f64() < 0.05, "capsule tilted {}", c.angle);
        let b = w.body(ball).unwrap();
        assert!(
            (-22.0..-12.0).contains(&b.position.x.to_f64()),
            "ball not between slope and wall: x={}",
            b.position.x
        );
        assert!(
            (2.9..3.1).contains(&b.position.y.to_f64()),
            "ball not on the chain floor: y={}",
            b.position.y
        );
        let hit = w
            .raycast(
                FxVec2::new(fx(20), fx(5)),
                FxVec2::new(fx(0), fx(-10)),
                u32::MAX,
            )
            .unwrap();
        assert_eq!(hit.body, cap);
        assert!(
            (hit.point.y.to_f64() - 1.0).abs() < 0.1,
            "capsule top hit at {}",
            hit.point.y
        );
    }

    #[test]
    fn raycast_hits_nearest_with_normal() {
        let mut w = World::new(WorldConfig::default());
        let near = w.add(Body::new(
            BodyKind::Static,
            Shape::Circle { radius: Fx::ONE },
            FxVec2::new(fx(5), fx(0)),
        ));
        w.add(Body::new(
            BodyKind::Static,
            Shape::rect(Fx::ONE, Fx::ONE),
            FxVec2::new(fx(9), fx(0)),
        ));
        let h = w
            .raycast(
                FxVec2::new(fx(0), fx(0)),
                FxVec2::new(fx(20), fx(0)),
                u32::MAX,
            )
            .unwrap();
        assert_eq!(h.body, near);
        assert!((h.point.x.to_f64() - 4.0).abs() < 0.01);
        assert!(h.normal.x.to_f64() < -0.99);
        let h = w
            .raycast(FxVec2::new(fx(0), fx(0)), FxVec2::new(fx(20), fx(0)), 0)
            .is_none();
        assert!(h);
        let boxhit = w
            .raycast(
                FxVec2::new(fx(7), fx(0)),
                FxVec2::new(fx(5), fx(0)),
                u32::MAX,
            )
            .unwrap();
        assert!((boxhit.point.x.to_f64() - 8.0).abs() < 0.01);
    }

    #[test]
    fn bouncy_ball_bounces_and_sensor_only_reports() {
        let mut w = World::new(WorldConfig::default());
        ground(&mut w);
        let ball = w.add(
            Body::new(
                BodyKind::Dynamic,
                Shape::Circle { radius: Fx::ONE },
                FxVec2::new(fx(0), fx(6)),
            )
            .with_material(Fx::ZERO, Fx::from_ratio(8, 10)),
        );
        let mut sensor = Body::new(
            BodyKind::Static,
            Shape::rect(fx(3), fx(1)),
            FxVec2::new(fx(0), fx(3)),
        );
        sensor.sensor = true;
        let sensor = w.add(sensor);
        let (mut max_after_bounce, mut bounced, mut sensed) = (Fx::ZERO, false, false);
        for _ in 0..400 {
            w.step();
            let b = w.body(ball).unwrap();
            if w.events()
                .iter()
                .any(|e| e.begin && e.a == ball && e.b == sensor)
            {
                sensed = true;
            }
            if bounced {
                max_after_bounce = max_after_bounce.max(b.position.y);
            } else if b.velocity.y > Fx::ZERO {
                bounced = true;
            }
        }
        assert!(
            bounced && max_after_bounce.to_f64() > 2.5,
            "bounce peak {max_after_bounce}"
        );
        assert!(sensed, "sensor saw the ball pass");
    }
}
