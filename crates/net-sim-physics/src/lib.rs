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

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct JointId(pub u32);

/// Joint definitions; anchors are in each body's local space.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum JointKind {
    /// Keeps `|anchor_b - anchor_a|` at `length` (rope with `min_length` < `length` for slack).
    Distance { length: Fx, min_length: Fx },
    /// Pin: anchors coincide; optional angle limits (relative angle b - a) and motor.
    Revolute {
        lower: Option<Fx>,
        upper: Option<Fx>,
        motor_speed: Fx,
        max_motor_torque: Fx,
    },
    /// Pin plus fixed relative angle.
    Weld { rel_angle: Fx },
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Joint {
    pub a: BodyId,
    pub b: BodyId,
    pub anchor_a: FxVec2,
    pub anchor_b: FxVec2,
    pub kind: JointKind,
    /// Accumulated impulses for warm starting: linear (x, y), angular/motor, limit.
    acc: [Fx; 4],
}

impl Joint {
    pub fn new(a: BodyId, b: BodyId, anchor_a: FxVec2, anchor_b: FxVec2, kind: JointKind) -> Joint {
        Joint {
            a,
            b,
            anchor_a,
            anchor_b,
            kind,
            acc: [Fx::ZERO; 4],
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
    #[serde(default)]
    joints: BTreeMap<JointId, Joint>,
    #[serde(default)]
    next_joint: u32,
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
            joints: BTreeMap::new(),
            next_joint: 1,
        }
    }
    pub fn add_joint(&mut self, joint: Joint) -> JointId {
        let id = JointId(self.next_joint);
        self.next_joint += 1;
        self.joints.insert(id, joint);
        id
    }
    pub fn remove_joint(&mut self, id: JointId) -> Option<Joint> {
        self.joints.remove(&id)
    }
    pub fn joint(&self, id: JointId) -> Option<&Joint> {
        self.joints.get(&id)
    }
    pub fn joint_mut(&mut self, id: JointId) -> Option<&mut Joint> {
        self.joints.get_mut(&id)
    }
    pub fn add(&mut self, body: Body) -> BodyId {
        let id = BodyId(self.next_id);
        self.next_id += 1;
        self.bodies.insert(id, body);
        id
    }
    pub fn remove(&mut self, id: BodyId) -> Option<Body> {
        self.manifolds.retain(|m| m.a != id && m.b != id);
        self.joints.retain(|_, j| j.a != id && j.b != id);
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
        self.warm_start_joints();
        for _ in 0..self.cfg.velocity_iterations {
            self.solve_joints();
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
            self.solve_joint_positions();
        }
        self.tick += 1;
    }

    fn pairs(&self) -> Vec<(BodyId, BodyId)> {
        let cell = self.cfg.cell;
        // bodies connected by a joint never collide with each other (Box2D's collideConnected = false)
        let jointed: std::collections::BTreeSet<(BodyId, BodyId)> = self
            .joints
            .values()
            .map(|j| (j.a.min(j.b), j.a.max(j.b)))
            .collect();
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
                    if jointed.contains(&(a, b)) {
                        continue;
                    }
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

    fn warm_start_joints(&mut self) {
        let ids: Vec<JointId> = self.joints.keys().copied().collect();
        for id in ids {
            let j = self.joints[&id].clone();
            let (pa, pb) = (self.anchor(j.a, j.anchor_a), self.anchor(j.b, j.anchor_b));
            let p = FxVec2::new(j.acc[0], j.acc[1]);
            self.impulse_at(j.a, pa, FxVec2::new(-p.x, -p.y));
            self.impulse_at(j.b, pb, p);
            self.angular_impulse(j.a, -(j.acc[2] + j.acc[3]));
            self.angular_impulse(j.b, j.acc[2] + j.acc[3]);
        }
    }

    fn anchor(&self, id: BodyId, local: FxVec2) -> FxVec2 {
        let b = &self.bodies[&id];
        rotate(local, b.angle.sin(), b.angle.cos()) + b.position
    }
    fn impulse_at(&mut self, id: BodyId, point: FxVec2, p: FxVec2) {
        let b = self.bodies.get_mut(&id).unwrap();
        let r = point - b.position;
        b.velocity = b.velocity + p.scale(b.inv_mass);
        b.angular_velocity += cross(r, p) * b.inv_inertia;
    }
    fn angular_impulse(&mut self, id: BodyId, l: Fx) {
        let b = self.bodies.get_mut(&id).unwrap();
        b.angular_velocity += l * b.inv_inertia;
    }
    fn point_velocity(&self, id: BodyId, point: FxVec2) -> FxVec2 {
        let b = &self.bodies[&id];
        b.velocity + perp(point - b.position).scale(b.angular_velocity)
    }
    /// Effective mass of a unit impulse along `n` applied at the two anchors.
    fn pair_k(&self, j: &Joint, pa: FxVec2, pb: FxVec2, n: FxVec2) -> Fx {
        let (a, b) = (&self.bodies[&j.a], &self.bodies[&j.b]);
        a.inv_mass
            + b.inv_mass
            + sq(cross(pa - a.position, n)) * a.inv_inertia
            + sq(cross(pb - b.position, n)) * b.inv_inertia
    }
    fn angular_k(&self, j: &Joint) -> Fx {
        self.bodies[&j.a].inv_inertia + self.bodies[&j.b].inv_inertia
    }

    fn solve_joints(&mut self) {
        let dt = self.cfg.dt;
        let ids: Vec<JointId> = self.joints.keys().copied().collect();
        for id in ids {
            let mut j = self.joints[&id].clone();
            let (pa, pb) = (self.anchor(j.a, j.anchor_a), self.anchor(j.b, j.anchor_b));
            let rel = self.point_velocity(j.b, pb) - self.point_velocity(j.a, pa);
            match j.kind.clone() {
                JointKind::Distance { length, min_length } => {
                    let d = pb - pa;
                    let dist = d.length();
                    if min_length < length && dist < length {
                        // slack rope: no constraint this step
                        j.acc[0] = Fx::ZERO;
                        j.acc[1] = Fx::ZERO;
                        self.joints.insert(id, j);
                        continue;
                    }
                    if dist == Fx::ZERO {
                        continue;
                    }
                    let n = d.scale(Fx::ONE / dist);
                    let k = self.pair_k(&j, pa, pb, n);
                    if k == Fx::ZERO {
                        continue;
                    }
                    // Baumgarte on the length error, with slack between min_length and length
                    let err = if dist > length {
                        dist - length
                    } else if dist < min_length {
                        dist - min_length
                    } else {
                        Fx::ZERO
                    };
                    let bias = err * self.cfg.baumgarte / dt;
                    let mut lambda = -(rel.dot(n) + bias) / k;
                    let acc = FxVec2::new(j.acc[0], j.acc[1]);
                    let old = acc.dot(n);
                    if min_length < length {
                        // rope: only pull, never push
                        let new = (old + lambda).min(Fx::ZERO);
                        lambda = new - old;
                    }
                    let p = n.scale(lambda);
                    j.acc[0] = acc.x + p.x;
                    j.acc[1] = acc.y + p.y;
                    self.impulse_at(j.a, pa, FxVec2::new(-p.x, -p.y));
                    self.impulse_at(j.b, pb, p);
                }
                JointKind::Revolute {
                    lower,
                    upper,
                    motor_speed,
                    max_motor_torque,
                } => {
                    let ka = self.angular_k(&j);
                    if ka > Fx::ZERO {
                        let (wa, wb) = (
                            self.bodies[&j.a].angular_velocity,
                            self.bodies[&j.b].angular_velocity,
                        );
                        if max_motor_torque > Fx::ZERO {
                            let cdot = wb - wa - motor_speed;
                            let max = max_motor_torque * dt;
                            let old = j.acc[2];
                            j.acc[2] = (old - cdot / ka).clamp(-max, max);
                            let l = j.acc[2] - old;
                            self.angular_impulse(j.a, -l);
                            self.angular_impulse(j.b, l);
                        }
                        let angle = self.bodies[&j.b].angle - self.bodies[&j.a].angle;
                        let (wa, wb) = (
                            self.bodies[&j.a].angular_velocity,
                            self.bodies[&j.b].angular_velocity,
                        );
                        let limit = match (lower, upper) {
                            (Some(lo), _) if angle <= lo => Some((angle - lo, true)),
                            (_, Some(hi)) if angle >= hi => Some((angle - hi, false)),
                            _ => None,
                        };
                        if let Some((err, at_lower)) = limit {
                            let bias = err * self.cfg.baumgarte / dt;
                            let old = j.acc[3];
                            let raw = old - (wb - wa + bias) / ka;
                            j.acc[3] = if at_lower {
                                raw.max(Fx::ZERO)
                            } else {
                                raw.min(Fx::ZERO)
                            };
                            let l = j.acc[3] - old;
                            self.angular_impulse(j.a, -l);
                            self.angular_impulse(j.b, l);
                        } else {
                            j.acc[3] = Fx::ZERO;
                        }
                        let _ = (wa, wb);
                    }
                    self.solve_point(&mut j, pa, pb);
                }
                JointKind::Weld { rel_angle } => {
                    let ka = self.angular_k(&j);
                    if ka > Fx::ZERO {
                        let angle = self.bodies[&j.b].angle - self.bodies[&j.a].angle - rel_angle;
                        let (wa, wb) = (
                            self.bodies[&j.a].angular_velocity,
                            self.bodies[&j.b].angular_velocity,
                        );
                        let bias = angle * self.cfg.baumgarte / dt;
                        let l = -(wb - wa + bias) / ka;
                        j.acc[2] += l;
                        self.angular_impulse(j.a, -l);
                        self.angular_impulse(j.b, l);
                    }
                    self.solve_point(&mut j, pa, pb);
                }
            }
            self.joints.insert(id, j);
        }
    }

    /// Point-to-point constraint (both anchors coincide), solved per axis with Baumgarte bias.
    fn solve_point(&mut self, j: &mut Joint, pa: FxVec2, pb: FxVec2) {
        let dt = self.cfg.dt;
        let err = pb - pa;
        for (axis, n) in [
            FxVec2::new(Fx::ONE, Fx::ZERO),
            FxVec2::new(Fx::ZERO, Fx::ONE),
        ]
        .into_iter()
        .enumerate()
        {
            let k = self.pair_k(j, pa, pb, n);
            if k == Fx::ZERO {
                continue;
            }
            let rel = self.point_velocity(j.b, pb) - self.point_velocity(j.a, pa);
            let bias = err.dot(n) * self.cfg.baumgarte / dt;
            let lambda = -(rel.dot(n) + bias) / k;
            j.acc[axis] += lambda;
            let p = n.scale(lambda);
            self.impulse_at(j.a, pa, FxVec2::new(-p.x, -p.y));
            self.impulse_at(j.b, pb, p);
        }
    }

    /// Position-level cleanup of pin joints so drift does not accumulate over long games.
    fn solve_joint_positions(&mut self) {
        let ids: Vec<JointId> = self.joints.keys().copied().collect();
        for id in ids {
            let j = self.joints[&id].clone();
            if matches!(j.kind, JointKind::Distance { .. }) {
                continue;
            }
            let (pa, pb) = (self.anchor(j.a, j.anchor_a), self.anchor(j.b, j.anchor_b));
            let err = pb - pa;
            let (ia, ib) = (self.bodies[&j.a].inv_mass, self.bodies[&j.b].inv_mass);
            let total = ia + ib;
            if total == Fx::ZERO {
                continue;
            }
            let corr = err.scale(self.cfg.baumgarte / total);
            let a = self.bodies.get_mut(&j.a).unwrap();
            a.position = a.position + corr.scale(ia);
            let b = self.bodies.get_mut(&j.b).unwrap();
            b.position = b.position - corr.scale(ib);
        }
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

    /// Sweep a circle of `radius` along `from + dir * t` (t in 0..=1): the closest `t` at which
    /// it first touches a body whose layer matches `mask`. Equivalent to a raycast against every
    /// shape inflated by `radius` (rounded polygons/segments), so the hit normal is the surface
    /// normal at the touch point. Returns `None` when nothing is reached (or already overlapping).
    pub fn circle_cast(&self, from: FxVec2, dir: FxVec2, radius: Fx, mask: u32) -> Option<RayHit> {
        let mut best: Option<RayHit> = None;
        for (id, b) in &self.bodies {
            if b.layer & mask == 0 {
                continue;
            }
            let (pts, r) = b.world_points();
            let hit = match &b.shape {
                Shape::Circle { .. } => ray_circle(from, dir, b.position, r + radius),
                Shape::Polygon { .. } => (0..pts.len())
                    .filter_map(|i| {
                        ray_capsule(from, dir, pts[i], pts[(i + 1) % pts.len()], radius)
                    })
                    .min_by_key(|h| h.0),
                Shape::Capsule { .. } => ray_capsule(from, dir, pts[0], pts[1], r + radius),
                Shape::Chain { .. } => pts
                    .windows(2)
                    .filter_map(|s| ray_capsule(from, dir, s[0], s[1], radius))
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

/// Kinematic character controller: moves a kinematic body by a displacement, sliding along
/// static/kinematic geometry, stepping over small ledges and reporting ground contact.
/// Deterministic by construction (pure fixed-point geometry, no engine callbacks).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct CharacterController {
    pub body: BodyId,
    /// Highest ledge the character steps onto without jumping.
    pub step_height: Fx,
    /// cos(max slope): a contact normal with `n.y >= min_ground_normal_y` counts as ground.
    pub min_ground_normal_y: Fx,
    /// Extra pull towards the ground to keep contact walking down slopes/stairs.
    pub snap_distance: Fx,
    pub grounded: bool,
    pub ground_normal: FxVec2,
    pub ground_body: Option<BodyId>,
    pub velocity: FxVec2,
}

impl CharacterController {
    pub fn new(body: BodyId) -> CharacterController {
        CharacterController {
            body,
            step_height: Fx::from_ratio(1, 2),
            min_ground_normal_y: Fx::from_ratio(7, 10),
            snap_distance: Fx::from_ratio(1, 4),
            grounded: false,
            ground_normal: FxVec2::new(Fx::ZERO, Fx::ONE),
            ground_body: None,
            velocity: FxVec2::default(),
        }
    }

    /// Move by `delta` this tick: horizontal slide with step-up, then vertical move with ground
    /// detection. Returns the displacement actually applied.
    pub fn step(&mut self, world: &mut World, delta: FxVec2) -> FxVec2 {
        let start = match world.body(self.body) {
            Some(b) => b.position,
            None => return FxVec2::default(),
        };
        self.grounded = false;
        self.ground_body = None;
        let horizontal = FxVec2::new(delta.x, Fx::ZERO);
        let moved = self.slide(world, horizontal);
        if moved.x.abs() < horizontal.x.abs() - Fx::from_ratio(1, 100)
            && self.step_height > Fx::ZERO
        {
            // blocked: try from step_height above, then settle back down
            let before = world.body(self.body).unwrap().position;
            let up = FxVec2::new(Fx::ZERO, self.step_height);
            if self.slide(world, up).y >= self.step_height - Fx::from_ratio(1, 100) {
                let rest = FxVec2::new(horizontal.x - moved.x, Fx::ZERO);
                let gained = self.slide(world, rest);
                let down = self.slide(world, FxVec2::new(Fx::ZERO, -self.step_height));
                if gained.x.abs() <= Fx::from_ratio(1, 100) || down.y > -Fx::from_ratio(1, 100) {
                    world.body_mut(self.body).unwrap().position = before;
                }
            } else {
                world.body_mut(self.body).unwrap().position = before;
            }
        }
        let vertical = FxVec2::new(Fx::ZERO, delta.y);
        self.slide(world, vertical);
        if !self.grounded && delta.y <= Fx::ZERO && self.snap_distance > Fx::ZERO {
            let before = world.body(self.body).unwrap().position;
            self.slide(world, FxVec2::new(Fx::ZERO, -self.snap_distance));
            if !self.grounded {
                world.body_mut(self.body).unwrap().position = before;
            }
        }
        let end = world.body(self.body).unwrap().position;
        self.velocity = (end - start).scale(if world.cfg.dt > Fx::ZERO {
            Fx::ONE / world.cfg.dt
        } else {
            Fx::ZERO
        });
        end - start
    }

    /// Translate, then push out of every overlapping static/kinematic body (up to 4 passes),
    /// recording ground normals. Returns the net translation.
    fn slide(&mut self, world: &mut World, delta: FxVec2) -> FxVec2 {
        let start = world.body(self.body).unwrap().position;
        world.body_mut(self.body).unwrap().position = start + delta;
        for _ in 0..4 {
            let me = world.body(self.body).unwrap().clone();
            let (lo, hi) = me.aabb();
            let mut push = FxVec2::default();
            let mut hits = 0;
            for (id, other) in world.bodies.iter() {
                if *id == self.body || other.kind == BodyKind::Dynamic || other.sensor {
                    continue;
                }
                if me.layer & other.mask == 0 || other.layer & me.mask == 0 {
                    continue;
                }
                let (olo, ohi) = other.aabb();
                if hi.x < olo.x || ohi.x < lo.x || hi.y < olo.y || ohi.y < lo.y {
                    continue;
                }
                for c in collide(&me, other) {
                    // normal points from me to other: push the opposite way
                    let out = FxVec2::new(-c.normal.x, -c.normal.y);
                    if out.y >= self.min_ground_normal_y {
                        self.grounded = true;
                        self.ground_normal = out;
                        self.ground_body = Some(*id);
                    }
                    push = push + out.scale(c.penetration);
                    hits += 1;
                }
            }
            if hits == 0 {
                break;
            }
            let b = world.body_mut(self.body).unwrap();
            b.position = b.position + push;
        }
        world.body(self.body).unwrap().position - start
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
    fn joints_pendulum_rope_and_motor() {
        let build = || {
            let mut w = World::new(WorldConfig::default());
            let pivot = w.add(Body::new(
                BodyKind::Static,
                Shape::Circle {
                    radius: Fx::from_ratio(1, 10),
                },
                FxVec2::new(fx(0), fx(10)),
            ));
            let bob = w.add(Body::new(
                BodyKind::Dynamic,
                Shape::Circle {
                    radius: Fx::from_ratio(1, 2),
                },
                FxVec2::new(fx(4), fx(10)),
            ));
            w.add_joint(Joint::new(
                pivot,
                bob,
                FxVec2::default(),
                FxVec2::default(),
                JointKind::Revolute {
                    lower: None,
                    upper: None,
                    motor_speed: Fx::ZERO,
                    max_motor_torque: Fx::ZERO,
                },
            ));
            // the bob is pinned at its centre: a revolute with coincident anchors at the pivot
            w.joint_mut(JointId(1)).unwrap().anchor_b = FxVec2::new(fx(-4), fx(0));
            let hang = w.add(Body::new(
                BodyKind::Dynamic,
                Shape::Circle {
                    radius: Fx::from_ratio(1, 2),
                },
                FxVec2::new(fx(20), fx(10)),
            ));
            let hook = w.add(Body::new(
                BodyKind::Static,
                Shape::Circle {
                    radius: Fx::from_ratio(1, 10),
                },
                FxVec2::new(fx(20), fx(13)),
            ));
            w.add_joint(Joint::new(
                hook,
                hang,
                FxVec2::default(),
                FxVec2::default(),
                JointKind::Distance {
                    length: fx(5),
                    min_length: fx(0),
                },
            ));
            let wheel = w.add(Body::new(
                BodyKind::Dynamic,
                Shape::Circle { radius: Fx::ONE },
                FxVec2::new(fx(40), fx(10)),
            ));
            let axle = w.add(Body::new(
                BodyKind::Static,
                Shape::Circle {
                    radius: Fx::from_ratio(1, 10),
                },
                FxVec2::new(fx(40), fx(10)),
            ));
            w.add_joint(Joint::new(
                axle,
                wheel,
                FxVec2::default(),
                FxVec2::default(),
                JointKind::Revolute {
                    lower: None,
                    upper: None,
                    motor_speed: fx(3),
                    max_motor_torque: fx(1000),
                },
            ));
            (w, bob, hang, wheel)
        };
        let (mut w, bob, hang, wheel) = build();
        let (mut w2, ..) = build();
        let mut max_len_err = Fx::ZERO;
        let mut min_y = fx(100);
        for _ in 0..300 {
            w.step();
            w2.step();
            min_y = min_y.min(w.body(bob).unwrap().position.y);
            let len = (w.body(bob).unwrap().position - FxVec2::new(fx(0), fx(10))).length();
            max_len_err = max_len_err.max((len - fx(4)).abs());
        }
        assert_eq!(w.hash(), w2.hash());
        assert!(
            max_len_err.to_f64() < 0.15,
            "pendulum arm drifted by {max_len_err}"
        );
        assert!(
            min_y.to_f64() < 7.0,
            "pendulum did not swing down: min y {min_y}"
        );
        let h = w.body(hang).unwrap().position;
        assert!(
            (h.y.to_f64() - 8.0).abs() < 0.2 && (h.x.to_f64() - 20.0).abs() < 0.1,
            "rope bob at {:?}",
            h
        );
        let wv = w.body(wheel).unwrap().angular_velocity.to_f64();
        assert!((wv - 3.0).abs() < 0.1, "motor speed {wv}");
    }

    #[test]
    fn character_walks_steps_and_stops_at_walls() {
        let build = || {
            let mut w = World::new(WorldConfig::default());
            ground(&mut w);
            // small step (0.4 high) at x=5..7, then a tall wall at x=12
            w.add(Body::new(
                BodyKind::Static,
                Shape::rect(Fx::ONE, Fx::from_ratio(2, 10)),
                FxVec2::new(fx(6), Fx::from_ratio(2, 10)),
            ));
            w.add(Body::new(
                BodyKind::Static,
                Shape::rect(Fx::from_ratio(1, 2), fx(3)),
                FxVec2::new(fx(12), fx(3)),
            ));
            let hero = w.add(Body::new(
                BodyKind::Kinematic,
                Shape::Capsule {
                    half_len: Fx::from_ratio(1, 2),
                    radius: Fx::from_ratio(1, 2),
                },
                FxVec2::new(fx(0), fx(1)),
            ));
            w.body_mut(hero).unwrap().angle = PI / Fx::from_int(2); // upright capsule
            (w, CharacterController::new(hero))
        };
        let (mut w, mut kcc) = build();
        let (mut w2, mut kcc2) = build();
        let mut vy = Fx::ZERO;
        let mut grounded_ticks = 0;
        for _ in 0..240 {
            vy -= fx(10) * w.cfg.dt;
            let d = FxVec2::new(Fx::from_ratio(6, 60), vy * w.cfg.dt);
            kcc.step(&mut w, d);
            kcc2.step(&mut w2, d);
            w.step();
            w2.step();
            if kcc.grounded {
                grounded_ticks += 1;
                vy = Fx::ZERO;
            }
        }
        assert_eq!(w.hash(), w2.hash());
        let p = w.body(kcc.body).unwrap().position;
        assert!(grounded_ticks > 200, "grounded only {grounded_ticks} ticks");
        assert!(
            (10.9..11.6).contains(&p.x.to_f64()),
            "hero should stop at the wall, x={}",
            p.x
        );
        assert!(
            (0.9..1.1).contains(&p.y.to_f64()),
            "hero should be back on the floor after the step, y={}",
            p.y
        );
        // mid-way check: on top of the step the hero stands 0.4 higher
        let (mut w3, mut k3) = build();
        let dt = w3.cfg.dt;
        let mut vy = Fx::ZERO;
        let mut top = Fx::ZERO;
        for _ in 0..120 {
            vy -= fx(10) * dt;
            k3.step(&mut w3, FxVec2::new(Fx::from_ratio(6, 60), vy * dt));
            if k3.grounded {
                vy = Fx::ZERO;
            }
            top = top.max(w3.body(k3.body).unwrap().position.y);
        }
        assert!(
            (1.3..1.5).contains(&top.to_f64()),
            "hero never stood on the step: max y {top}"
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
    fn circle_cast_stops_one_radius_before_the_wall() {
        let mut w = World::new(WorldConfig::default());
        let wall = Body::new(
            BodyKind::Static,
            Shape::rect(fx(1), fx(5)),
            FxVec2::new(fx(10), fx(0)),
        );
        w.add(wall);
        let hit = w
            .circle_cast(
                FxVec2::new(fx(0), fx(0)),
                FxVec2::new(fx(20), fx(0)),
                Fx::HALF,
                u32::MAX,
            )
            .expect("hit");
        // wall face at x = 9, circle radius 0.5 -> centre stops at 8.5, 20 units of travel -> t = 0.425
        assert!(
            (hit.point.x - Fx::from_ratio(85, 10)).abs() < Fx::from_ratio(1, 1000),
            "{:?}",
            hit.point
        );
        assert_eq!(hit.normal, FxVec2::new(-Fx::ONE, Fx::ZERO));
        assert!(
            w.raycast(
                FxVec2::new(fx(0), fx(0)),
                FxVec2::new(fx(20), fx(0)),
                u32::MAX
            )
            .unwrap()
            .fraction
                > hit.fraction
        );
        assert!(w
            .circle_cast(
                FxVec2::new(fx(0), fx(0)),
                FxVec2::new(fx(0), fx(20)),
                Fx::HALF,
                u32::MAX
            )
            .is_none());
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
