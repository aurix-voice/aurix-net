//! Deterministic fixed-point 3D rigid bodies: spheres, capsules and boxes with a uniform-grid
//! broadphase, analytic/SAT narrowphase (box-box reference-face clipping), sequential impulses
//! with warm starting, friction, restitution and positional correction, raycasts and
//! snapshot/restore/hash. Same determinism rules as the 2D crate: Q32.32 only, `BTreeMap`
//! ordering, no floats.
#![forbid(unsafe_code)]
#![deny(clippy::float_arithmetic)]

use net_fpmath::{Fx, FxMat3, FxQuat, FxVec3};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

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
    Sphere {
        radius: Fx,
    },
    /// Segment along the body y axis from -half_len to +half_len, inflated by `radius`.
    Capsule {
        half_len: Fx,
        radius: Fx,
    },
    Box {
        half: FxVec3,
    },
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Body {
    pub kind: BodyKind,
    pub shape: Shape,
    pub position: FxVec3,
    pub rotation: FxQuat,
    pub velocity: FxVec3,
    pub angular_velocity: FxVec3,
    pub friction: Fx,
    pub restitution: Fx,
    pub layer: u32,
    pub mask: u32,
    pub sensor: bool,
    pub inv_mass: Fx,
    /// Inverse inertia in body space (diagonal).
    pub inv_inertia: FxVec3,
    #[serde(skip)]
    inv_inertia_world: FxMat3,
}

impl Body {
    pub fn new(kind: BodyKind, shape: Shape, position: FxVec3) -> Body {
        Body::with_density(kind, shape, position, Fx::ONE)
    }
    pub fn with_density(kind: BodyKind, shape: Shape, position: FxVec3, density: Fx) -> Body {
        let (inv_mass, inv_inertia) = if kind == BodyKind::Dynamic {
            let (m, i) = mass_props(&shape, density);
            let inv = |v: Fx| if v.0 == 0 { Fx::ZERO } else { Fx::ONE / v };
            (inv(m), FxVec3::new(inv(i.x), inv(i.y), inv(i.z)))
        } else {
            (Fx::ZERO, FxVec3::ZERO)
        };
        Body {
            kind,
            shape,
            position,
            rotation: FxQuat::IDENTITY,
            velocity: FxVec3::ZERO,
            angular_velocity: FxVec3::ZERO,
            friction: Fx::from_ratio(1, 2),
            restitution: Fx::ZERO,
            layer: 1,
            mask: u32::MAX,
            sensor: false,
            inv_mass,
            inv_inertia,
            inv_inertia_world: FxMat3::ZERO,
        }
    }
    fn update_inertia(&mut self) {
        let r = self.rotation.to_mat3();
        self.inv_inertia_world = FxMat3::diagonal(self.inv_inertia).rotated(r);
    }
    fn radius(&self) -> Fx {
        match &self.shape {
            Shape::Sphere { radius } | Shape::Capsule { radius, .. } => *radius,
            Shape::Box { .. } => Fx::ZERO,
        }
    }
    /// Capsule segment endpoints (world).
    fn segment(&self) -> (FxVec3, FxVec3) {
        let h = match &self.shape {
            Shape::Capsule { half_len, .. } => *half_len,
            _ => Fx::ZERO,
        };
        let a = self.rotation.rotate(FxVec3::new(Fx::ZERO, h, Fx::ZERO));
        (self.position - a, self.position + a)
    }
    fn aabb(&self) -> (FxVec3, FxVec3) {
        match &self.shape {
            Shape::Sphere { radius } => (
                self.position - splat(*radius),
                self.position + splat(*radius),
            ),
            Shape::Capsule { radius, .. } => {
                let (a, b) = self.segment();
                (vmin(a, b) - splat(*radius), vmax(a, b) + splat(*radius))
            }
            Shape::Box { half } => {
                let m = self.rotation.to_mat3();
                let e = FxVec3::new(
                    m.row(0).x.abs() * half.x
                        + m.row(0).y.abs() * half.y
                        + m.row(0).z.abs() * half.z,
                    m.row(1).x.abs() * half.x
                        + m.row(1).y.abs() * half.y
                        + m.row(1).z.abs() * half.z,
                    m.row(2).x.abs() * half.x
                        + m.row(2).y.abs() * half.y
                        + m.row(2).z.abs() * half.z,
                );
                (self.position - e, self.position + e)
            }
        }
    }
}

fn splat(v: Fx) -> FxVec3 {
    FxVec3::new(v, v, v)
}
fn vmin(a: FxVec3, b: FxVec3) -> FxVec3 {
    FxVec3::new(a.x.min(b.x), a.y.min(b.y), a.z.min(b.z))
}
fn vmax(a: FxVec3, b: FxVec3) -> FxVec3 {
    FxVec3::new(a.x.max(b.x), a.y.max(b.y), a.z.max(b.z))
}
fn neg(v: FxVec3) -> FxVec3 {
    FxVec3::new(-v.x, -v.y, -v.z)
}

fn mass_props(shape: &Shape, density: Fx) -> (Fx, FxVec3) {
    match shape {
        Shape::Sphere { radius } => {
            let r2 = *radius * *radius;
            let m = Fx::from_ratio(4, 3) * Fx::PI * r2 * *radius * density;
            let i = Fx::from_ratio(2, 5) * m * r2;
            (m, FxVec3::new(i, i, i))
        }
        Shape::Capsule { half_len, radius } => {
            let r2 = *radius * *radius;
            let h = *half_len * Fx::from_int(2);
            let m_cyl = Fx::PI * r2 * h * density;
            let m_sph = Fx::from_ratio(4, 3) * Fx::PI * r2 * *radius * density;
            let m = m_cyl + m_sph;
            let iy = m_cyl * r2 * Fx::HALF + Fx::from_ratio(2, 5) * m_sph * r2;
            let ix = m_cyl * (Fx::from_int(3) * r2 + h * h) / Fx::from_int(12)
                + m_sph * (Fx::from_ratio(2, 5) * r2 + *half_len * *half_len);
            (m, FxVec3::new(ix, iy, ix))
        }
        Shape::Box { half } => {
            let m = Fx::from_int(8) * half.x * half.y * half.z * density;
            let k = m / Fx::from_int(3);
            (
                m,
                FxVec3::new(
                    k * (half.y * half.y + half.z * half.z),
                    k * (half.x * half.x + half.z * half.z),
                    k * (half.x * half.x + half.y * half.y),
                ),
            )
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct JointId(pub u32);

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum JointKind {
    /// Point-to-point (ball-and-socket).
    Ball,
    /// Ball joint plus a shared rotation axis; optional angle limits (radians) and motor.
    Hinge {
        axis_a: FxVec3,
        axis_b: FxVec3,
        lower: Option<Fx>,
        upper: Option<Fx>,
        motor_speed: Fx,
        max_motor_torque: Fx,
    },
    /// Keeps the anchors between `min_length` and `length` (equal = rod, `min_length` 0 = rope).
    Distance { length: Fx, min_length: Fx },
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Joint {
    pub a: BodyId,
    pub b: BodyId,
    /// Anchors in each body's local space.
    pub anchor_a: FxVec3,
    pub anchor_b: FxVec3,
    pub kind: JointKind,
    acc_lin: FxVec3,
    /// Accumulated angular impulses: two perpendicular axes, limit, motor.
    acc_ang: [Fx; 4],
}

impl Joint {
    pub fn new(a: BodyId, b: BodyId, anchor_a: FxVec3, anchor_b: FxVec3, kind: JointKind) -> Joint {
        Joint {
            a,
            b,
            anchor_a,
            anchor_b,
            kind,
            acc_lin: FxVec3::ZERO,
            acc_ang: [Fx::ZERO; 4],
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct WorldConfig {
    pub dt: Fx,
    pub gravity: FxVec3,
    pub iterations: u32,
    pub cell: Fx,
    pub slop: Fx,
    pub baumgarte: Fx,
    pub restitution_threshold: Fx,
}

impl Default for WorldConfig {
    fn default() -> Self {
        WorldConfig {
            dt: Fx::from_ratio(1, 60),
            gravity: FxVec3::new(Fx::ZERO, Fx::from_int(-10), Fx::ZERO),
            iterations: 8,
            cell: Fx::from_int(2),
            slop: Fx::from_ratio(1, 100),
            baumgarte: Fx::from_ratio(2, 10),
            restitution_threshold: Fx::ONE,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
struct ContactPoint {
    point: FxVec3,
    depth: Fx,
    acc_n: Fx,
    acc_t1: Fx,
    acc_t2: Fx,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
struct Manifold {
    a: BodyId,
    b: BodyId,
    /// From a to b.
    normal: FxVec3,
    points: Vec<ContactPoint>,
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
    pub point: FxVec3,
    pub normal: FxVec3,
    pub fraction: Fx,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct World {
    pub cfg: WorldConfig,
    bodies: BTreeMap<BodyId, Body>,
    next_id: u32,
    manifolds: Vec<Manifold>,
    touching: BTreeSet<(BodyId, BodyId)>,
    joints: BTreeMap<JointId, Joint>,
    next_joint: u32,
    #[serde(skip)]
    events: Vec<ContactEvent>,
    pub tick: u64,
}

impl World {
    pub fn new(cfg: WorldConfig) -> World {
        World {
            cfg,
            bodies: BTreeMap::new(),
            next_id: 1,
            manifolds: Vec::new(),
            touching: BTreeSet::new(),
            joints: BTreeMap::new(),
            next_joint: 1,
            events: Vec::new(),
            tick: 0,
        }
    }
    pub fn add(&mut self, mut body: Body) -> BodyId {
        let id = BodyId(self.next_id);
        self.next_id += 1;
        body.update_inertia();
        self.bodies.insert(id, body);
        id
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
    /// Current hinge angle (radians) of a hinge joint, measured around the shared axis.
    pub fn hinge_angle(&self, id: JointId) -> Option<Fx> {
        let j = self.joints.get(&id)?;
        let (a, b) = (self.bodies.get(&j.a)?, self.bodies.get(&j.b)?);
        match &j.kind {
            JointKind::Hinge { axis_a, .. } => Some(hinge_angle(a, b, *axis_a)),
            _ => None,
        }
    }
    pub fn remove(&mut self, id: BodyId) -> Option<Body> {
        self.manifolds.retain(|m| m.a != id && m.b != id);
        self.touching.retain(|(a, b)| *a != id && *b != id);
        self.joints.retain(|_, j| j.a != id && j.b != id);
        self.bodies.remove(&id)
    }
    pub fn body(&self, id: BodyId) -> Option<&Body> {
        self.bodies.get(&id)
    }
    pub fn body_mut(&mut self, id: BodyId) -> Option<&mut Body> {
        self.bodies.get_mut(&id)
    }
    pub fn bodies(&self) -> impl Iterator<Item = (&BodyId, &Body)> {
        self.bodies.iter()
    }
    pub fn events(&self) -> &[ContactEvent] {
        &self.events
    }
    pub fn apply_impulse(&mut self, id: BodyId, impulse: FxVec3, at: Option<FxVec3>) {
        if let Some(b) = self.bodies.get_mut(&id) {
            if b.kind == BodyKind::Dynamic {
                b.velocity = b.velocity + impulse.scale(b.inv_mass);
                if let Some(p) = at {
                    let r = p - b.position;
                    b.angular_velocity =
                        b.angular_velocity + b.inv_inertia_world.mul_vec(r.cross(impulse));
                }
            }
        }
    }

    pub fn step(&mut self) {
        let dt = self.cfg.dt;
        let g = self.cfg.gravity;
        for b in self.bodies.values_mut() {
            if b.kind == BodyKind::Dynamic {
                b.velocity = b.velocity + g.scale(dt);
            }
            b.update_inertia();
        }
        let pairs = self.broadphase();
        let mut manifolds = Vec::new();
        for (a, b) in pairs {
            if let Some(m) = self.collide(a, b) {
                manifolds.push(m);
            }
        }
        // warm start from last tick's manifolds
        for m in &mut manifolds {
            if let Some(old) = self.manifolds.iter().find(|o| o.a == m.a && o.b == m.b) {
                for p in &mut m.points {
                    if let Some(op) = old
                        .points
                        .iter()
                        .min_by_key(|op| (op.point - p.point).length_sq())
                    {
                        if (op.point - p.point).length_sq() < Fx::from_ratio(1, 100) {
                            p.acc_n = op.acc_n;
                            p.acc_t1 = op.acc_t1;
                            p.acc_t2 = op.acc_t2;
                        }
                    }
                }
            }
        }
        self.manifolds = manifolds;
        let solid: Vec<usize> = (0..self.manifolds.len())
            .filter(|&i| {
                let m = &self.manifolds[i];
                !self.bodies[&m.a].sensor && !self.bodies[&m.b].sensor
            })
            .collect();
        // restitution targets from pre-solve velocities
        let mut bias: Vec<Vec<Fx>> = Vec::with_capacity(self.manifolds.len());
        for m in &self.manifolds {
            let (a, b) = (&self.bodies[&m.a], &self.bodies[&m.b]);
            let e = a.restitution.max(b.restitution);
            bias.push(
                m.points
                    .iter()
                    .map(|p| {
                        let vn = rel_vel(a, b, p.point).dot(m.normal);
                        if vn < -self.cfg.restitution_threshold {
                            -e * vn
                        } else {
                            Fx::ZERO
                        }
                    })
                    .collect(),
            );
        }
        for &i in &solid {
            let m = self.manifolds[i].clone();
            let (t1, t2) = tangents(m.normal);
            for p in &m.points {
                let imp = m.normal.scale(p.acc_n) + t1.scale(p.acc_t1) + t2.scale(p.acc_t2);
                self.apply_pair(m.a, m.b, p.point, imp);
            }
        }
        self.warm_start_joints();
        for _ in 0..self.cfg.iterations {
            self.solve_joints();
            for &i in &solid {
                let (a_id, b_id, n) = (
                    self.manifolds[i].a,
                    self.manifolds[i].b,
                    self.manifolds[i].normal,
                );
                let (t1, t2) = tangents(n);
                let friction = {
                    let (a, b) = (&self.bodies[&a_id], &self.bodies[&b_id]);
                    (a.friction * b.friction).sqrt()
                };
                let bias_i = bias[i].clone();
                for (k, &bias_k) in bias_i.iter().enumerate() {
                    let point = self.manifolds[i].points[k].point;
                    let (a, b) = (&self.bodies[&a_id], &self.bodies[&b_id]);
                    let (ra, rb) = (point - a.position, point - b.position);
                    // normal
                    let vn = rel_vel(a, b, point).dot(n);
                    let kn = eff_mass(a, b, ra, rb, n);
                    let mut dj = if kn.0 == 0 {
                        Fx::ZERO
                    } else {
                        (bias_k - vn) / kn
                    };
                    let p = &mut self.manifolds[i].points[k];
                    let old = p.acc_n;
                    p.acc_n = (old + dj).max(Fx::ZERO);
                    dj = p.acc_n - old;
                    self.apply_pair(a_id, b_id, point, n.scale(dj));
                    // friction
                    let (a, b) = (&self.bodies[&a_id], &self.bodies[&b_id]);
                    let v = rel_vel(a, b, point);
                    let max_f = friction * self.manifolds[i].points[k].acc_n;
                    for (t, which) in [(t1, 0), (t2, 1)] {
                        let (a, b) = (&self.bodies[&a_id], &self.bodies[&b_id]);
                        let kt = eff_mass(a, b, ra, rb, t);
                        let vt = v.dot(t);
                        let mut dj = if kt.0 == 0 { Fx::ZERO } else { -vt / kt };
                        let p = &mut self.manifolds[i].points[k];
                        let acc = if which == 0 {
                            &mut p.acc_t1
                        } else {
                            &mut p.acc_t2
                        };
                        let old = *acc;
                        *acc = (old + dj).clamp(-max_f, max_f);
                        dj = *acc - old;
                        self.apply_pair(a_id, b_id, point, t.scale(dj));
                    }
                }
            }
        }
        for b in self.bodies.values_mut() {
            if b.kind == BodyKind::Static {
                continue;
            }
            b.position = b.position + b.velocity.scale(dt);
            if b.angular_velocity.length_sq().0 != 0 {
                b.rotation = b.rotation.integrate(b.angular_velocity, dt);
            }
        }
        self.correct_joint_positions();
        // positional correction (linear split by inverse mass)
        for &i in &solid {
            let m = &self.manifolds[i];
            let (ima, imb) = (self.bodies[&m.a].inv_mass, self.bodies[&m.b].inv_mass);
            let sum = ima + imb;
            if sum.0 == 0 {
                continue;
            }
            let mut max_depth = Fx::ZERO;
            for p in &m.points {
                max_depth = max_depth.max(p.depth);
            }
            let pen = (max_depth - self.cfg.slop).max(Fx::ZERO);
            if pen.0 == 0 {
                continue;
            }
            let corr = m.normal.scale(self.cfg.baumgarte * pen / sum);
            let (a_id, b_id) = (m.a, m.b);
            let a = self.bodies.get_mut(&a_id).unwrap();
            a.position = a.position - corr.scale(ima);
            let b = self.bodies.get_mut(&b_id).unwrap();
            b.position = b.position + corr.scale(imb);
        }
        // events
        let now: BTreeSet<(BodyId, BodyId)> = self.manifolds.iter().map(|m| (m.a, m.b)).collect();
        self.events.clear();
        for &(a, b) in now.difference(&self.touching) {
            self.events.push(ContactEvent { a, b, begin: true });
        }
        for &(a, b) in self.touching.difference(&now) {
            self.events.push(ContactEvent { a, b, begin: false });
        }
        self.touching = now;
        self.tick += 1;
    }

    fn apply_angular(&mut self, a_id: BodyId, b_id: BodyId, l: FxVec3) {
        let a = self.bodies.get_mut(&a_id).unwrap();
        if a.kind == BodyKind::Dynamic {
            a.angular_velocity = a.angular_velocity - a.inv_inertia_world.mul_vec(l);
        }
        let b = self.bodies.get_mut(&b_id).unwrap();
        if b.kind == BodyKind::Dynamic {
            b.angular_velocity = b.angular_velocity + b.inv_inertia_world.mul_vec(l);
        }
    }

    fn warm_start_joints(&mut self) {
        let ids: Vec<JointId> = self.joints.keys().copied().collect();
        for id in ids {
            let j = self.joints[&id].clone();
            if !self.bodies.contains_key(&j.a) || !self.bodies.contains_key(&j.b) {
                continue;
            }
            let (pa, pb) = self.anchors(&j);
            match &j.kind {
                JointKind::Distance { .. } => {
                    let d = pb - pa;
                    if d.length_sq().0 != 0 {
                        self.apply_pair_two(j.a, j.b, pa, pb, d.normalized().scale(j.acc_lin.x));
                    }
                }
                _ => self.apply_pair_two(j.a, j.b, pa, pb, j.acc_lin),
            }
            if let JointKind::Hinge { axis_a, .. } = &j.kind {
                let u = self.bodies[&j.a].rotation.rotate(*axis_a).normalized();
                let (t1, t2) = tangents(u);
                let l = t1.scale(j.acc_ang[0])
                    + t2.scale(j.acc_ang[1])
                    + u.scale(j.acc_ang[2] + j.acc_ang[3]);
                self.apply_angular(j.a, j.b, l);
            }
        }
    }

    fn anchors(&self, j: &Joint) -> (FxVec3, FxVec3) {
        let (a, b) = (&self.bodies[&j.a], &self.bodies[&j.b]);
        (
            a.position + a.rotation.rotate(j.anchor_a),
            b.position + b.rotation.rotate(j.anchor_b),
        )
    }

    /// Impulse `imp` pushes b at `pb` and pulls a at `pa`.
    fn apply_pair_two(&mut self, a_id: BodyId, b_id: BodyId, pa: FxVec3, pb: FxVec3, imp: FxVec3) {
        let a = self.bodies.get_mut(&a_id).unwrap();
        if a.kind == BodyKind::Dynamic {
            let ra = pa - a.position;
            a.velocity = a.velocity - imp.scale(a.inv_mass);
            a.angular_velocity = a.angular_velocity - a.inv_inertia_world.mul_vec(ra.cross(imp));
        }
        let b = self.bodies.get_mut(&b_id).unwrap();
        if b.kind == BodyKind::Dynamic {
            let rb = pb - b.position;
            b.velocity = b.velocity + imp.scale(b.inv_mass);
            b.angular_velocity = b.angular_velocity + b.inv_inertia_world.mul_vec(rb.cross(imp));
        }
    }

    fn solve_joints(&mut self) {
        let dt = self.cfg.dt;
        let ids: Vec<JointId> = self.joints.keys().copied().collect();
        for id in ids {
            let mut j = self.joints[&id].clone();
            let (Some(a), Some(b)) = (self.bodies.get(&j.a), self.bodies.get(&j.b)) else {
                continue;
            };
            if a.kind != BodyKind::Dynamic && b.kind != BodyKind::Dynamic {
                continue;
            }
            let (pa, pb) = self.anchors(&j);
            let (ra, rb) = (pa - a.position, pb - b.position);
            let va = a.velocity + a.angular_velocity.cross(ra);
            let vb = b.velocity + b.angular_velocity.cross(rb);
            let rel = vb - va;
            match &j.kind {
                JointKind::Ball | JointKind::Hinge { .. } => {
                    let k = k_matrix(a, b, ra, rb);
                    if let Some(inv) = k.inverse() {
                        let imp = inv.mul_vec(neg(rel));
                        j.acc_lin = j.acc_lin + imp;
                        self.apply_pair_two(j.a, j.b, pa, pb, imp);
                    }
                }
                JointKind::Distance { length, min_length } => {
                    let d = pb - pa;
                    let len = d.length();
                    if len.0 == 0 {
                        continue;
                    }
                    let n = d.scale(Fx::ONE / len);
                    let rod = *min_length >= *length;
                    if !rod && len < *length && len > *min_length {
                        j.acc_lin = FxVec3::ZERO;
                        self.joints.insert(id, j);
                        continue;
                    }
                    let k = eff_mass(a, b, ra, rb, n);
                    if k.0 == 0 {
                        continue;
                    }
                    let target = if len > *length { *length } else { *min_length };
                    let bias = (target - len) * self.cfg.baumgarte / dt;
                    let mut dj = (bias - rel.dot(n)) / k;
                    let old = j.acc_lin.x;
                    let acc = if rod {
                        old + dj
                    } else if len > *length {
                        (old + dj).min(Fx::ZERO)
                    } else {
                        (old + dj).max(Fx::ZERO)
                    };
                    dj = acc - old;
                    j.acc_lin.x = acc;
                    self.apply_pair_two(j.a, j.b, pa, pb, n.scale(dj));
                }
            }
            if let JointKind::Hinge {
                axis_a,
                axis_b,
                lower,
                upper,
                motor_speed,
                max_motor_torque,
            } = &j.kind
            {
                let (ua, align) = {
                    let (a, b) = (&self.bodies[&j.a], &self.bodies[&j.b]);
                    let ua = a.rotation.rotate(*axis_a).normalized();
                    let ub = b.rotation.rotate(*axis_b).normalized();
                    (ua, ub.cross(ua)) // rotate b by `align` to line its axis up with a's
                };
                let (t1, t2) = tangents(ua);
                for (t, slot) in [(t1, 0usize), (t2, 1usize)] {
                    let (kt, wt) = {
                        let (a, b) = (&self.bodies[&j.a], &self.bodies[&j.b]);
                        (
                            ang_mass(a, b, t),
                            (b.angular_velocity - a.angular_velocity).dot(t),
                        )
                    };
                    if kt.0 == 0 {
                        continue;
                    }
                    let target = align.dot(t) * self.cfg.baumgarte / dt;
                    let dj = (target - wt) / kt;
                    j.acc_ang[slot] += dj;
                    self.apply_angular(j.a, j.b, t.scale(dj));
                }
                let ku = {
                    let (a, b) = (&self.bodies[&j.a], &self.bodies[&j.b]);
                    ang_mass(a, b, ua)
                };
                if ku.0 != 0 {
                    let (a, b) = (&self.bodies[&j.a], &self.bodies[&j.b]);
                    let wrel = b.angular_velocity - a.angular_velocity;
                    let angle = hinge_angle(a, b, *axis_a);
                    if let Some(lo) = lower {
                        if angle < *lo {
                            let bias = (*lo - angle) * self.cfg.baumgarte / dt;
                            let mut dj = (bias - wrel.dot(ua)) / ku;
                            let old = j.acc_ang[2];
                            j.acc_ang[2] = (old + dj).max(Fx::ZERO);
                            dj = j.acc_ang[2] - old;
                            self.apply_angular(j.a, j.b, ua.scale(dj));
                        }
                    }
                    if let Some(hi) = upper {
                        if angle > *hi {
                            let (a, b) = (&self.bodies[&j.a], &self.bodies[&j.b]);
                            let wrel = b.angular_velocity - a.angular_velocity;
                            let bias = (*hi - angle) * self.cfg.baumgarte / dt;
                            let mut dj = (bias - wrel.dot(ua)) / ku;
                            let old = j.acc_ang[2];
                            j.acc_ang[2] = (old + dj).min(Fx::ZERO);
                            dj = j.acc_ang[2] - old;
                            self.apply_angular(j.a, j.b, ua.scale(dj));
                        }
                    }
                    if max_motor_torque.0 > 0 {
                        let (a, b) = (&self.bodies[&j.a], &self.bodies[&j.b]);
                        let wrel = b.angular_velocity - a.angular_velocity;
                        let max = *max_motor_torque * dt;
                        let mut dj = (*motor_speed - wrel.dot(ua)) / ku;
                        let old = j.acc_ang[3];
                        j.acc_ang[3] = (old + dj).clamp(-max, max);
                        dj = j.acc_ang[3] - old;
                        self.apply_angular(j.a, j.b, ua.scale(dj));
                    }
                }
            }
            self.joints.insert(id, j);
        }
    }

    /// Pull ball/hinge anchors together (linear split by inverse mass, like contacts).
    fn correct_joint_positions(&mut self) {
        let ids: Vec<JointId> = self.joints.keys().copied().collect();
        for id in ids {
            let j = self.joints[&id].clone();
            if !matches!(j.kind, JointKind::Ball | JointKind::Hinge { .. }) {
                continue;
            }
            let (Some(a), Some(b)) = (self.bodies.get(&j.a), self.bodies.get(&j.b)) else {
                continue;
            };
            let (ima, imb) = (a.inv_mass, b.inv_mass);
            let sum = ima + imb;
            if sum.0 == 0 {
                continue;
            }
            let (pa, pb) = self.anchors(&j);
            let err = pb - pa;
            if err.length_sq() < Fx::from_ratio(1, 1_000_000) {
                continue;
            }
            let corr = err.scale(self.cfg.baumgarte / sum);
            let a = self.bodies.get_mut(&j.a).unwrap();
            if a.kind == BodyKind::Dynamic {
                a.position = a.position + corr.scale(ima);
            }
            let b = self.bodies.get_mut(&j.b).unwrap();
            if b.kind == BodyKind::Dynamic {
                b.position = b.position - corr.scale(imb);
            }
        }
    }

    fn apply_pair(&mut self, a_id: BodyId, b_id: BodyId, point: FxVec3, imp: FxVec3) {
        let a = self.bodies.get_mut(&a_id).unwrap();
        if a.kind == BodyKind::Dynamic {
            let ra = point - a.position;
            a.velocity = a.velocity - imp.scale(a.inv_mass);
            a.angular_velocity = a.angular_velocity - a.inv_inertia_world.mul_vec(ra.cross(imp));
        }
        let b = self.bodies.get_mut(&b_id).unwrap();
        if b.kind == BodyKind::Dynamic {
            let rb = point - b.position;
            b.velocity = b.velocity + imp.scale(b.inv_mass);
            b.angular_velocity = b.angular_velocity + b.inv_inertia_world.mul_vec(rb.cross(imp));
        }
    }

    fn broadphase(&self) -> Vec<(BodyId, BodyId)> {
        let cell = self.cfg.cell;
        let mut cells: BTreeMap<(i64, i64, i64), Vec<BodyId>> = BTreeMap::new();
        for (id, b) in &self.bodies {
            let (lo, hi) = b.aabb();
            let c = |v: Fx| (v / cell).0 >> 32;
            for x in c(lo.x)..=c(hi.x) {
                for y in c(lo.y)..=c(hi.y) {
                    for z in c(lo.z)..=c(hi.z) {
                        cells.entry((x, y, z)).or_default().push(*id);
                    }
                }
            }
        }
        let jointed: BTreeSet<(BodyId, BodyId)> = self
            .joints
            .values()
            .map(|j| (j.a.min(j.b), j.a.max(j.b)))
            .collect();
        let mut pairs = BTreeSet::new();
        for ids in cells.values() {
            for i in 0..ids.len() {
                for j in i + 1..ids.len() {
                    let (a, b) = (&self.bodies[&ids[i]], &self.bodies[&ids[j]]);
                    if a.kind != BodyKind::Dynamic && b.kind != BodyKind::Dynamic {
                        continue;
                    }
                    if a.layer & b.mask == 0 || b.layer & a.mask == 0 {
                        continue;
                    }
                    let (la, ha) = a.aabb();
                    let (lb, hb) = b.aabb();
                    if la.x > hb.x
                        || lb.x > ha.x
                        || la.y > hb.y
                        || lb.y > ha.y
                        || la.z > hb.z
                        || lb.z > ha.z
                    {
                        continue;
                    }
                    let key = (ids[i].min(ids[j]), ids[i].max(ids[j]));
                    if jointed.contains(&key) {
                        continue;
                    }
                    pairs.insert(key);
                }
            }
        }
        pairs.into_iter().collect()
    }

    fn collide(&self, ia: BodyId, ib: BodyId) -> Option<Manifold> {
        let (a, b) = (&self.bodies[&ia], &self.bodies[&ib]);
        let points: Vec<(FxVec3, FxVec3, Fx)> = match (&a.shape, &b.shape) {
            (Shape::Sphere { .. }, Shape::Sphere { .. }) => {
                sphere_sphere(a.position, a.radius(), b.position, b.radius())
                    .into_iter()
                    .collect()
            }
            (Shape::Sphere { .. }, Shape::Capsule { .. }) => {
                let (p, q) = b.segment();
                let c = closest_on_segment(a.position, p, q);
                sphere_sphere(a.position, a.radius(), c, b.radius())
                    .into_iter()
                    .collect()
            }
            (Shape::Capsule { .. }, Shape::Sphere { .. }) => {
                let (p, q) = a.segment();
                let c = closest_on_segment(b.position, p, q);
                sphere_sphere(c, a.radius(), b.position, b.radius())
                    .into_iter()
                    .collect()
            }
            (Shape::Capsule { .. }, Shape::Capsule { .. }) => {
                let (p1, q1) = a.segment();
                let (p2, q2) = b.segment();
                let (c1, c2) = closest_segments(p1, q1, p2, q2);
                sphere_sphere(c1, a.radius(), c2, b.radius())
                    .into_iter()
                    .collect()
            }
            (Shape::Sphere { .. }, Shape::Box { half }) => {
                sphere_box(a.position, a.radius(), b, *half)
                    .into_iter()
                    .collect()
            }
            (Shape::Box { half }, Shape::Sphere { .. }) => {
                sphere_box(b.position, b.radius(), a, *half)
                    .into_iter()
                    .map(|(p, n, d)| (p, neg(n), d))
                    .collect()
            }
            (Shape::Capsule { .. }, Shape::Box { half }) => {
                capsule_box(a, b, *half).into_iter().collect()
            }
            (Shape::Box { half }, Shape::Capsule { .. }) => capsule_box(b, a, *half)
                .into_iter()
                .map(|(p, n, d)| (p, neg(n), d))
                .collect(),
            (Shape::Box { half: ha }, Shape::Box { half: hb }) => box_box(a, *ha, b, *hb),
        };
        if points.is_empty() {
            return None;
        }
        let normal = points[0].1;
        Some(Manifold {
            a: ia,
            b: ib,
            normal,
            points: points
                .into_iter()
                .map(|(point, _, depth)| ContactPoint {
                    point,
                    depth,
                    acc_n: Fx::ZERO,
                    acc_t1: Fx::ZERO,
                    acc_t2: Fx::ZERO,
                })
                .collect(),
        })
    }

    /// Closest hit along `from + dir * t`, t in 0..=1.
    pub fn raycast(&self, from: FxVec3, dir: FxVec3, mask: u32) -> Option<RayHit> {
        let mut best: Option<RayHit> = None;
        for (id, b) in &self.bodies {
            if b.layer & mask == 0 {
                continue;
            }
            let hit = match &b.shape {
                Shape::Sphere { radius } => ray_sphere(from, dir, b.position, *radius),
                Shape::Capsule { radius, .. } => {
                    let (p, q) = b.segment();
                    ray_capsule(from, dir, p, q, *radius)
                }
                Shape::Box { half } => ray_box(from, dir, b, *half),
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

    pub fn hash(&self) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut mix = |v: i64| {
            for b in v.to_le_bytes() {
                h ^= b as u64;
                h = h.wrapping_mul(0x0000_0100_0000_01b3);
            }
        };
        mix(self.tick as i64);
        for (id, b) in &self.bodies {
            mix(id.0 as i64);
            for v in [b.position, b.velocity, b.angular_velocity] {
                mix(v.x.0);
                mix(v.y.0);
                mix(v.z.0);
            }
            mix(b.rotation.x.0);
            mix(b.rotation.y.0);
            mix(b.rotation.z.0);
            mix(b.rotation.w.0);
        }
        for (id, j) in &self.joints {
            mix(id.0 as i64);
            mix(j.acc_lin.x.0);
            mix(j.acc_lin.y.0);
            mix(j.acc_lin.z.0);
            for a in j.acc_ang {
                mix(a.0);
            }
        }
        h
    }
    pub fn snapshot(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }
    pub fn restore(bytes: &[u8]) -> Option<World> {
        let mut w: World = serde_json::from_slice(bytes).ok()?;
        for b in w.bodies.values_mut() {
            b.update_inertia();
        }
        Some(w)
    }
}

fn rel_vel(a: &Body, b: &Body, p: FxVec3) -> FxVec3 {
    let va = a.velocity + a.angular_velocity.cross(p - a.position);
    let vb = b.velocity + b.angular_velocity.cross(p - b.position);
    vb - va
}

fn eff_mass(a: &Body, b: &Body, ra: FxVec3, rb: FxVec3, n: FxVec3) -> Fx {
    let mut k = a.inv_mass + b.inv_mass;
    if a.kind == BodyKind::Dynamic {
        let rn = ra.cross(n);
        k += a.inv_inertia_world.mul_vec(rn).cross(ra).dot(n);
    }
    if b.kind == BodyKind::Dynamic {
        let rn = rb.cross(n);
        k += b.inv_inertia_world.mul_vec(rn).cross(rb).dot(n);
    }
    k
}

/// Point-constraint effective-mass matrix K so that K·P = Δ(relative anchor velocity).
fn k_matrix(a: &Body, b: &Body, ra: FxVec3, rb: FxVec3) -> FxMat3 {
    let axes = [
        FxVec3::new(Fx::ONE, Fx::ZERO, Fx::ZERO),
        FxVec3::new(Fx::ZERO, Fx::ONE, Fx::ZERO),
        FxVec3::new(Fx::ZERO, Fx::ZERO, Fx::ONE),
    ];
    let col = |e: FxVec3| {
        let mut v = e.scale(a.inv_mass + b.inv_mass);
        if a.kind == BodyKind::Dynamic {
            v = v + a.inv_inertia_world.mul_vec(ra.cross(e)).cross(ra);
        }
        if b.kind == BodyKind::Dynamic {
            v = v + b.inv_inertia_world.mul_vec(rb.cross(e)).cross(rb);
        }
        v
    };
    FxMat3::from_cols(col(axes[0]), col(axes[1]), col(axes[2]))
}

/// Angular effective mass of the pair along unit axis `t`.
fn ang_mass(a: &Body, b: &Body, t: FxVec3) -> Fx {
    let mut k = Fx::ZERO;
    if a.kind == BodyKind::Dynamic {
        k += t.dot(a.inv_inertia_world.mul_vec(t));
    }
    if b.kind == BodyKind::Dynamic {
        k += t.dot(b.inv_inertia_world.mul_vec(t));
    }
    k
}

/// Angle of b relative to a around the hinge axis, using a reference perpendicular shared at
/// creation (bodies are assumed aligned when the joint is made).
fn hinge_angle(a: &Body, b: &Body, axis_a: FxVec3) -> Fx {
    let (perp, _) = tangents(axis_a.normalized());
    let ua = a.rotation.rotate(axis_a).normalized();
    let pa = a.rotation.rotate(perp);
    let pb = b.rotation.rotate(perp);
    let pb = (pb - ua.scale(pb.dot(ua))).normalized();
    Fx::atan2(pa.cross(pb).dot(ua), pa.dot(pb))
}

fn tangents(n: FxVec3) -> (FxVec3, FxVec3) {
    let helper = if n.x.abs() < Fx::from_ratio(57, 100) {
        FxVec3::new(Fx::ONE, Fx::ZERO, Fx::ZERO)
    } else {
        FxVec3::new(Fx::ZERO, Fx::ONE, Fx::ZERO)
    };
    let t1 = n.cross(helper).normalized();
    (t1, n.cross(t1))
}

/// Contact between two spheres: (point, normal a→b, depth).
fn sphere_sphere(ca: FxVec3, ra: Fx, cb: FxVec3, rb: Fx) -> Option<(FxVec3, FxVec3, Fx)> {
    let d = cb - ca;
    let dist = d.length();
    let sum = ra + rb;
    if dist >= sum {
        return None;
    }
    let n = if dist.0 == 0 {
        FxVec3::new(Fx::ZERO, Fx::ONE, Fx::ZERO)
    } else {
        d.scale(Fx::ONE / dist)
    };
    let depth = sum - dist;
    Some((ca + n.scale(ra - depth * Fx::HALF), n, depth))
}

fn closest_on_segment(p: FxVec3, a: FxVec3, b: FxVec3) -> FxVec3 {
    let ab = b - a;
    let l2 = ab.length_sq();
    if l2.0 == 0 {
        return a;
    }
    let t = ((p - a).dot(ab) / l2).clamp(Fx::ZERO, Fx::ONE);
    a + ab.scale(t)
}

/// Closest points between segments (Ericson 5.1.9), clamped; parallel case picks s = 0.
fn closest_segments(p1: FxVec3, q1: FxVec3, p2: FxVec3, q2: FxVec3) -> (FxVec3, FxVec3) {
    let d1 = q1 - p1;
    let d2 = q2 - p2;
    let r = p1 - p2;
    let a = d1.length_sq();
    let e = d2.length_sq();
    let f = d2.dot(r);
    let eps = Fx::from_ratio(1, 1_000_000);
    let (mut s, mut t);
    if a <= eps && e <= eps {
        return (p1, p2);
    }
    if a <= eps {
        s = Fx::ZERO;
        t = (f / e).clamp(Fx::ZERO, Fx::ONE);
    } else {
        let c = d1.dot(r);
        if e <= eps {
            t = Fx::ZERO;
            s = (-c / a).clamp(Fx::ZERO, Fx::ONE);
        } else {
            let b = d1.dot(d2);
            let denom = a * e - b * b;
            s = if denom > eps {
                ((b * f - c * e) / denom).clamp(Fx::ZERO, Fx::ONE)
            } else {
                Fx::ZERO
            };
            t = (b * s + f) / e;
            if t < Fx::ZERO {
                t = Fx::ZERO;
                s = (-c / a).clamp(Fx::ZERO, Fx::ONE);
            } else if t > Fx::ONE {
                t = Fx::ONE;
                s = ((b - c) / a).clamp(Fx::ZERO, Fx::ONE);
            }
        }
    }
    (p1 + d1.scale(s), p2 + d2.scale(t))
}

/// Sphere vs oriented box: (point, normal sphere→box, depth).
fn sphere_box(c: FxVec3, r: Fx, bx: &Body, half: FxVec3) -> Option<(FxVec3, FxVec3, Fx)> {
    let l = bx.rotation.inverse_rotate(c - bx.position);
    let q = FxVec3::new(
        l.x.clamp(-half.x, half.x),
        l.y.clamp(-half.y, half.y),
        l.z.clamp(-half.z, half.z),
    );
    let d = l - q;
    let dist = d.length();
    if dist > r {
        return None;
    }
    let (n_local, depth) = if dist.0 == 0 {
        // centre inside: push out along the axis of least penetration
        let px = half.x - l.x.abs();
        let py = half.y - l.y.abs();
        let pz = half.z - l.z.abs();
        let sign = |v: Fx| if v < Fx::ZERO { -Fx::ONE } else { Fx::ONE };
        if px <= py && px <= pz {
            (FxVec3::new(sign(l.x), Fx::ZERO, Fx::ZERO), r + px)
        } else if py <= pz {
            (FxVec3::new(Fx::ZERO, sign(l.y), Fx::ZERO), r + py)
        } else {
            (FxVec3::new(Fx::ZERO, Fx::ZERO, sign(l.z)), r + pz)
        }
    } else {
        (d.scale(Fx::ONE / dist), r - dist)
    };
    // n_local points from box surface towards the sphere; manifold normal is sphere→box
    let n_world = bx.rotation.rotate(n_local);
    let point = bx.position + bx.rotation.rotate(q);
    Some((point, neg(n_world), depth))
}

/// Capsule vs box: sphere tests at the segment ends and at the point nearest the box centre.
fn capsule_box(cap: &Body, bx: &Body, half: FxVec3) -> Vec<(FxVec3, FxVec3, Fx)> {
    let (p, q) = cap.segment();
    let mid = closest_on_segment(bx.position, p, q);
    let r = cap.radius();
    let mut out: Vec<(FxVec3, FxVec3, Fx)> = Vec::new();
    for c in [mid, p, q] {
        if let Some(h) = sphere_box(c, r, bx, half) {
            if !out
                .iter()
                .any(|o| (o.0 - h.0).length_sq() < Fx::from_ratio(1, 10_000))
            {
                out.push(h);
            }
        }
    }
    if out.len() > 1 {
        let n = out[0].1;
        for o in &mut out {
            o.1 = n;
        }
    }
    out
}

fn box_axes(b: &Body) -> [FxVec3; 3] {
    let m = b.rotation.to_mat3();
    [m.col(0), m.col(1), m.col(2)]
}

fn project_box(axes: &[FxVec3; 3], half: FxVec3, l: FxVec3) -> Fx {
    axes[0].dot(l).abs() * half.x + axes[1].dot(l).abs() * half.y + axes[2].dot(l).abs() * half.z
}

fn half_i(h: FxVec3, i: usize) -> Fx {
    match i {
        0 => h.x,
        1 => h.y,
        _ => h.z,
    }
}

/// Box vs box via SAT (15 axes) with reference-face clipping; (point, normal a→b, depth).
fn box_box(a: &Body, ha: FxVec3, b: &Body, hb: FxVec3) -> Vec<(FxVec3, FxVec3, Fx)> {
    let aa = box_axes(a);
    let ab = box_axes(b);
    let d = b.position - a.position;
    let mut best: Option<(Fx, FxVec3, usize)> = None; // (overlap, axis a→b, kind: 0..3 face a, 3..6 face b, 6+ edge)
    let mut test = |axis: FxVec3, kind: usize, bias: Fx| {
        let l2 = axis.length_sq();
        if l2 < Fx::from_ratio(1, 1_000_000) {
            return true;
        }
        let axis = axis.normalized();
        let dist = d.dot(axis);
        let overlap = project_box(&aa, ha, axis) + project_box(&ab, hb, axis) - dist.abs();
        if overlap < Fx::ZERO {
            return false;
        }
        let scored = overlap * bias;
        if best.is_none_or(|(o, _, _)| scored < o) {
            let axis = if dist < Fx::ZERO { neg(axis) } else { axis };
            best = Some((scored, axis, kind));
        }
        true
    };
    for (i, &axis) in aa.iter().enumerate() {
        if !test(axis, i, Fx::ONE) {
            return Vec::new();
        }
    }
    for (i, &axis) in ab.iter().enumerate() {
        if !test(axis, 3 + i, Fx::ONE) {
            return Vec::new();
        }
    }
    for (i, &ea) in aa.iter().enumerate() {
        for (j, &eb) in ab.iter().enumerate() {
            if !test(ea.cross(eb), 6 + i * 3 + j, Fx::from_ratio(105, 100)) {
                return Vec::new();
            }
        }
    }
    let (_, n, kind) = best.unwrap();
    if kind >= 6 {
        // edge-edge: single contact between support points
        let sa = support(a, &aa, ha, n);
        let sb = support(b, &ab, hb, neg(n));
        let depth = (sa - sb).dot(n);
        return vec![((sa + sb).scale(Fx::HALF), n, depth.max(Fx::ZERO))];
    }
    // reference face on the box owning the axis, incident face on the other
    let (rb, rax, rh, ib, iax, ih, ref_n) = if kind < 3 {
        (a, &aa, ha, b, &ab, hb, n)
    } else {
        (b, &ab, hb, a, &aa, ha, neg(n))
    };
    // reference face: axis most aligned with ref_n
    let mut rk = 0;
    for i in 1..3 {
        if rax[i].dot(ref_n).abs() > rax[rk].dot(ref_n).abs() {
            rk = i;
        }
    }
    let rsign = if rax[rk].dot(ref_n) < Fx::ZERO {
        -Fx::ONE
    } else {
        Fx::ONE
    };
    let ref_centre = rb.position + rax[rk].scale(rsign * half_i(rh, rk));
    let ref_normal = rax[rk].scale(rsign);
    // incident face: most anti-parallel to ref_normal
    let mut ik = 0;
    let mut best_dot = Fx::MAX;
    for (i, axis) in iax.iter().enumerate() {
        let dd = axis.dot(ref_normal);
        if dd < best_dot {
            best_dot = dd;
            ik = i;
        }
        if -dd < best_dot {
            best_dot = -dd;
            ik = i;
        }
    }
    let isign = if iax[ik].dot(ref_normal) > Fx::ZERO {
        -Fx::ONE
    } else {
        Fx::ONE
    };
    let ic = ib.position + iax[ik].scale(isign * half_i(ih, ik));
    let (iu, iv) = ((ik + 1) % 3, (ik + 2) % 3);
    let (eu, ev) = (iax[iu].scale(half_i(ih, iu)), iax[iv].scale(half_i(ih, iv)));
    let mut poly = vec![ic + eu + ev, ic - eu + ev, ic - eu - ev, ic + eu - ev];
    // clip against the four side planes of the reference face
    for s in [(rk + 1) % 3, (rk + 2) % 3] {
        for sign in [Fx::ONE, -Fx::ONE] {
            let pn = rax[s].scale(sign);
            let off = pn.dot(rb.position) + half_i(rh, s);
            poly = clip(&poly, pn, off);
            if poly.is_empty() {
                return Vec::new();
            }
        }
    }
    let mut out: Vec<(FxVec3, FxVec3, Fx)> = Vec::new();
    for p in poly {
        let depth = (ref_centre - p).dot(ref_normal);
        if depth > Fx::ZERO {
            out.push((p + ref_normal.scale(depth * Fx::HALF), n, depth));
        }
    }
    out.sort_by_key(|x| std::cmp::Reverse(x.2));
    out.truncate(4);
    out
}

fn support(b: &Body, axes: &[FxVec3; 3], half: FxVec3, dir: FxVec3) -> FxVec3 {
    let mut p = b.position;
    for (i, axis) in axes.iter().enumerate() {
        let s = if axis.dot(dir) < Fx::ZERO {
            -Fx::ONE
        } else {
            Fx::ONE
        };
        p = p + axis.scale(s * half_i(half, i));
    }
    p
}

/// Keep the part of `poly` with `n·p <= off` (Sutherland–Hodgman against one plane).
fn clip(poly: &[FxVec3], n: FxVec3, off: Fx) -> Vec<FxVec3> {
    let mut out = Vec::with_capacity(poly.len() + 1);
    for i in 0..poly.len() {
        let (p, q) = (poly[i], poly[(i + 1) % poly.len()]);
        let (dp, dq) = (n.dot(p) - off, n.dot(q) - off);
        if dp <= Fx::ZERO {
            out.push(p);
        }
        if (dp < Fx::ZERO && dq > Fx::ZERO) || (dp > Fx::ZERO && dq < Fx::ZERO) {
            let t = dp / (dp - dq);
            out.push(p + (q - p).scale(t));
        }
    }
    out
}

fn ray_sphere(from: FxVec3, dir: FxVec3, c: FxVec3, r: Fx) -> Option<(Fx, FxVec3)> {
    let m = from - c;
    let a = dir.length_sq();
    if a.0 == 0 {
        return None;
    }
    let b = m.dot(dir);
    let cc = m.length_sq() - r * r;
    if cc < Fx::ZERO {
        return None;
    }
    let disc = b * b - a * cc;
    if disc < Fx::ZERO || b >= Fx::ZERO {
        return None;
    }
    let t = (-b - disc.sqrt()) / a;
    if t > Fx::ONE {
        return None;
    }
    let p = from + dir.scale(t);
    Some((t, (p - c).normalized()))
}

fn ray_capsule(from: FxVec3, dir: FxVec3, p: FxVec3, q: FxVec3, r: Fx) -> Option<(Fx, FxVec3)> {
    let mut best: Option<(Fx, FxVec3)> = None;
    for c in [p, q] {
        if let Some(h) = ray_sphere(from, dir, c, r) {
            if best.is_none_or(|b| h.0 < b.0) {
                best = Some(h);
            }
        }
    }
    let axis = q - p;
    let len = axis.length();
    if len.0 != 0 {
        let u = axis.scale(Fx::ONE / len);
        let c = (p + q).scale(Fx::HALF);
        let m = from - c;
        let mp = m - u.scale(m.dot(u));
        let dp = dir - u.scale(dir.dot(u));
        let a = dp.length_sq();
        if a.0 != 0 {
            let b = mp.dot(dp);
            let cc = mp.length_sq() - r * r;
            let disc = b * b - a * cc;
            if disc >= Fx::ZERO && b < Fx::ZERO && cc >= Fx::ZERO {
                let t = (-b - disc.sqrt()) / a;
                if t <= Fx::ONE {
                    let hit = from + dir.scale(t);
                    let along = (hit - c).dot(u);
                    if along.abs() <= len * Fx::HALF && best.is_none_or(|bst| t < bst.0) {
                        let n = (hit - (c + u.scale(along))).normalized();
                        best = Some((t, n));
                    }
                }
            }
        }
    }
    best
}

fn ray_box(from: FxVec3, dir: FxVec3, b: &Body, half: FxVec3) -> Option<(Fx, FxVec3)> {
    let o = b.rotation.inverse_rotate(from - b.position);
    let d = b.rotation.inverse_rotate(dir);
    let mut tmin = Fx::ZERO;
    let mut tmax = Fx::ONE;
    let mut n_local = FxVec3::ZERO;
    for i in 0..3 {
        let (oi, di, hi) = match i {
            0 => (o.x, d.x, half.x),
            1 => (o.y, d.y, half.y),
            _ => (o.z, d.z, half.z),
        };
        if di.abs() < Fx::from_ratio(1, 1_000_000) {
            if oi.abs() > hi {
                return None;
            }
            continue;
        }
        let mut t1 = (-hi - oi) / di;
        let mut t2 = (hi - oi) / di;
        let mut sign = -Fx::ONE;
        if t1 > t2 {
            std::mem::swap(&mut t1, &mut t2);
            sign = Fx::ONE;
        }
        if t1 > tmin {
            tmin = t1;
            n_local = match i {
                0 => FxVec3::new(sign, Fx::ZERO, Fx::ZERO),
                1 => FxVec3::new(Fx::ZERO, sign, Fx::ZERO),
                _ => FxVec3::new(Fx::ZERO, Fx::ZERO, sign),
            };
        }
        tmax = tmax.min(t2);
        if tmin > tmax {
            return None;
        }
    }
    if n_local == FxVec3::ZERO {
        return None; // started inside
    }
    Some((tmin, b.rotation.rotate(n_local)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fx(v: i32) -> Fx {
        Fx::from_int(v)
    }
    fn v(x: i32, y: i32, z: i32) -> FxVec3 {
        FxVec3::new(fx(x), fx(y), fx(z))
    }
    fn floor() -> Body {
        Body::new(
            BodyKind::Static,
            Shape::Box {
                half: FxVec3::new(fx(20), Fx::HALF, fx(20)),
            },
            FxVec3::new(Fx::ZERO, -Fx::HALF, Fx::ZERO),
        )
    }
    fn run_twice(build: impl Fn() -> World, ticks: u32) -> World {
        let (mut a, mut b) = (build(), build());
        for _ in 0..ticks {
            a.step();
            b.step();
        }
        assert_eq!(a.hash(), b.hash(), "two identical worlds diverged");
        let back = World::restore(&a.snapshot()).unwrap();
        assert_eq!(back.hash(), a.hash(), "snapshot round-trip");
        a
    }

    #[test]
    fn sphere_falls_and_rests_on_the_floor() {
        let w = run_twice(
            || {
                let mut w = World::new(WorldConfig::default());
                w.add(floor());
                w.add(Body::new(
                    BodyKind::Dynamic,
                    Shape::Sphere { radius: Fx::HALF },
                    v(0, 5, 0),
                ));
                w
            },
            300,
        );
        let s = w.body(BodyId(2)).unwrap();
        assert!(
            (s.position.y - Fx::HALF).abs() < Fx::from_ratio(3, 100),
            "sphere y {}",
            s.position.y
        );
        assert!(
            s.velocity.length() < Fx::from_ratio(5, 100),
            "sphere still moving {:?}",
            s.velocity
        );
    }

    #[test]
    fn box_stack_stays_up() {
        let w = run_twice(
            || {
                let mut w = World::new(WorldConfig::default());
                w.add(floor());
                for i in 0..4 {
                    w.add(Body::new(
                        BodyKind::Dynamic,
                        Shape::Box {
                            half: splat(Fx::HALF),
                        },
                        FxVec3::new(Fx::ZERO, Fx::HALF + fx(i), Fx::ZERO),
                    ));
                }
                w
            },
            300,
        );
        let top = w.body(BodyId(5)).unwrap();
        assert!(
            (top.position.y - Fx::from_ratio(7, 2)).abs() < Fx::from_ratio(15, 100),
            "top box y {}",
            top.position.y
        );
        assert!(
            top.position.x.abs() < Fx::from_ratio(1, 10)
                && top.position.z.abs() < Fx::from_ratio(1, 10),
            "top box drifted {:?}",
            top.position
        );
        assert!(top.velocity.length() < Fx::from_ratio(1, 10));
    }

    #[test]
    fn capsules_rest_and_spheres_bounce() {
        let w = run_twice(
            || {
                let mut w = World::new(WorldConfig::default());
                w.add(floor());
                let mut cap = Body::new(
                    BodyKind::Dynamic,
                    Shape::Capsule {
                        half_len: Fx::ONE,
                        radius: Fx::HALF,
                    },
                    v(0, 3, 0),
                );
                cap.rotation = FxQuat::from_axis_angle(v(0, 0, 1), Fx::HALF_PI);
                w.add(cap);
                let mut ball = Body::new(
                    BodyKind::Dynamic,
                    Shape::Sphere { radius: Fx::HALF },
                    v(5, 4, 0),
                );
                ball.restitution = Fx::from_ratio(8, 10);
                w.add(ball);
                let mut rolling = Body::new(
                    BodyKind::Dynamic,
                    Shape::Sphere { radius: Fx::HALF },
                    FxVec3::new(fx(-5), Fx::HALF, fx(5)),
                );
                rolling.velocity = v(3, 0, 0);
                w.add(rolling);
                w
            },
            240,
        );
        let cap = w.body(BodyId(2)).unwrap();
        assert!(
            (cap.position.y - Fx::HALF).abs() < Fx::from_ratio(5, 100),
            "capsule y {}",
            cap.position.y
        );
        let ball = w.body(BodyId(3)).unwrap();
        assert!(
            ball.position.y > Fx::HALF + Fx::from_ratio(1, 10)
                || ball.velocity.y.abs() > Fx::from_ratio(1, 2),
            "bouncy ball should still be bouncing: {:?} {:?}",
            ball.position,
            ball.velocity
        );
        let rolling = w.body(BodyId(4)).unwrap();
        assert!(
            rolling.position.x > fx(-4),
            "rolling ball moved: {:?}",
            rolling.position
        );
        assert!(
            rolling.angular_velocity.z.abs() > Fx::from_ratio(1, 10),
            "rolling ball spins: {:?}",
            rolling.angular_velocity
        );
    }

    #[test]
    fn tilted_box_settles_flat() {
        let w = run_twice(
            || {
                let mut w = World::new(WorldConfig::default());
                w.add(floor());
                let mut b = Body::new(
                    BodyKind::Dynamic,
                    Shape::Box {
                        half: splat(Fx::HALF),
                    },
                    v(0, 3, 0),
                );
                b.rotation = FxQuat::from_axis_angle(v(0, 0, 1), Fx::from_ratio(3, 10));
                w.add(b);
                w
            },
            420,
        );
        let b = w.body(BodyId(2)).unwrap();
        assert!(
            (b.position.y - Fx::HALF).abs() < Fx::from_ratio(5, 100),
            "box y {}",
            b.position.y
        );
        let up = b.rotation.rotate(v(0, 1, 0));
        assert!(
            up.y.abs() > Fx::from_ratio(98, 100),
            "box face not flat: {:?}",
            up
        );
    }

    #[test]
    fn joints_pendulum_rope_hinge_and_motor() {
        let w = run_twice(
            || {
                let mut w = World::new(WorldConfig::default());
                let anchor = w.add(Body::new(
                    BodyKind::Static,
                    Shape::Sphere {
                        radius: Fx::from_ratio(1, 10),
                    },
                    v(0, 10, 0),
                ));
                // ball-joint pendulum: bob 3 m to the side of the anchor
                let bob = w.add(Body::new(
                    BodyKind::Dynamic,
                    Shape::Sphere {
                        radius: Fx::from_ratio(1, 4),
                    },
                    v(3, 10, 0),
                ));
                w.add_joint(Joint::new(
                    anchor,
                    bob,
                    FxVec3::ZERO,
                    v(-3, 0, 0),
                    JointKind::Ball,
                ));
                // rope: starts slack 1 m under a second anchor, max length 3
                let anchor2 = w.add(Body::new(
                    BodyKind::Static,
                    Shape::Sphere {
                        radius: Fx::from_ratio(1, 10),
                    },
                    v(10, 10, 0),
                ));
                let rope_end = w.add(Body::new(
                    BodyKind::Dynamic,
                    Shape::Sphere {
                        radius: Fx::from_ratio(1, 4),
                    },
                    v(10, 9, 0),
                ));
                w.add_joint(Joint::new(
                    anchor2,
                    rope_end,
                    FxVec3::ZERO,
                    FxVec3::ZERO,
                    JointKind::Distance {
                        length: fx(3),
                        min_length: Fx::ZERO,
                    },
                ));
                // hinged door on a static post, limits 0..=90°, pushed open
                let post = w.add(Body::new(
                    BodyKind::Static,
                    Shape::Box {
                        half: FxVec3::new(Fx::from_ratio(1, 10), fx(1), Fx::from_ratio(1, 10)),
                    },
                    v(-10, 1, 0),
                ));
                let mut door = Body::new(
                    BodyKind::Dynamic,
                    Shape::Box {
                        half: FxVec3::new(Fx::HALF, fx(1), Fx::from_ratio(1, 20)),
                    },
                    FxVec3::new(fx(-10) + Fx::from_ratio(6, 10), fx(1), Fx::ZERO),
                );
                door.velocity = FxVec3::ZERO;
                let door = w.add(door);
                w.add_joint(Joint::new(
                    post,
                    door,
                    FxVec3::new(Fx::from_ratio(1, 10), Fx::ZERO, Fx::ZERO),
                    FxVec3::new(-Fx::HALF, Fx::ZERO, Fx::ZERO),
                    JointKind::Hinge {
                        axis_a: v(0, 1, 0),
                        axis_b: v(0, 1, 0),
                        lower: Some(Fx::ZERO),
                        upper: Some(Fx::HALF_PI),
                        motor_speed: Fx::ZERO,
                        max_motor_torque: Fx::ZERO,
                    },
                ));
                w.apply_impulse(
                    door,
                    v(0, 0, 8),
                    Some(FxVec3::new(fx(-10) + Fx::ONE, fx(1), Fx::ZERO)),
                );
                // motorised wheel on a static axle
                let axle = w.add(Body::new(
                    BodyKind::Static,
                    Shape::Sphere {
                        radius: Fx::from_ratio(1, 10),
                    },
                    v(20, 5, 0),
                ));
                let wheel = w.add(Body::new(
                    BodyKind::Dynamic,
                    Shape::Sphere { radius: Fx::ONE },
                    v(20, 5, 0),
                ));
                w.add_joint(Joint::new(
                    axle,
                    wheel,
                    FxVec3::ZERO,
                    FxVec3::ZERO,
                    JointKind::Hinge {
                        axis_a: v(0, 0, 1),
                        axis_b: v(0, 0, 1),
                        lower: None,
                        upper: None,
                        motor_speed: fx(4),
                        max_motor_torque: fx(50),
                    },
                ));
                w
            },
            300,
        );
        let bob = w.body(BodyId(2)).unwrap();
        let dist = (bob.position + bob.rotation.rotate(v(-3, 0, 0)) - v(0, 10, 0)).length();
        assert!(dist < Fx::from_ratio(5, 100), "ball joint drifted {dist}");
        assert!(
            bob.position.y < fx(9),
            "pendulum did not swing: {:?}",
            bob.position
        );
        let rope_end = w.body(BodyId(4)).unwrap();
        let rl = (rope_end.position - v(10, 10, 0)).length();
        assert!(
            rl > Fx::from_ratio(29, 10) && rl < Fx::from_ratio(31, 10),
            "rope length {rl}"
        );
        let angle = w.hinge_angle(JointId(3)).unwrap();
        assert!(
            angle > Fx::from_ratio(3, 10) && angle < Fx::HALF_PI + Fx::from_ratio(1, 10),
            "door angle {angle}"
        );
        let wheel = w.body(BodyId(8)).unwrap();
        assert!(
            (wheel.angular_velocity.z - fx(4)).abs() < Fx::from_ratio(2, 10),
            "motor speed {:?}",
            wheel.angular_velocity
        );
        assert!(
            (wheel.position - v(20, 5, 0)).length() < Fx::from_ratio(5, 100),
            "wheel stayed on the axle"
        );
    }

    #[test]
    fn raycasts_hit_every_shape() {
        let mut w = World::new(WorldConfig::default());
        w.add(floor());
        let ball = w.add(Body::new(
            BodyKind::Dynamic,
            Shape::Sphere { radius: Fx::HALF },
            FxVec3::new(fx(0), Fx::HALF, fx(0)),
        ));
        let mut cap = Body::new(
            BodyKind::Dynamic,
            Shape::Capsule {
                half_len: Fx::ONE,
                radius: Fx::HALF,
            },
            FxVec3::new(fx(3), Fx::from_ratio(3, 2), fx(0)),
        );
        cap.rotation = FxQuat::IDENTITY;
        let cap = w.add(cap);
        let bx = w.add(Body::new(
            BodyKind::Dynamic,
            Shape::Box {
                half: splat(Fx::HALF),
            },
            FxVec3::new(fx(-3), Fx::HALF, fx(0)),
        ));
        let hit = w.raycast(v(0, 5, 0), v(0, -10, 0), u32::MAX).unwrap();
        assert_eq!(hit.body, ball);
        assert!(
            (hit.point.y - Fx::ONE).abs() < Fx::from_ratio(1, 1000)
                && (hit.normal.y - Fx::ONE).abs() < Fx::from_ratio(1, 1000)
        );
        let hit = w.raycast(v(6, 1, 0), v(-10, 0, 0), u32::MAX).unwrap();
        assert_eq!(hit.body, cap, "{hit:?}");
        assert!(
            (hit.point.x - Fx::from_ratio(7, 2)).abs() < Fx::from_ratio(1, 1000),
            "{hit:?}"
        );
        let hit = w.raycast(v(-6, 0, 0), v(10, 0, 0), u32::MAX).unwrap();
        assert_eq!(hit.body, bx);
        assert!(
            (hit.point.x + Fx::from_ratio(7, 2)).abs() < Fx::from_ratio(1, 1000)
                && (hit.normal.x + Fx::ONE).abs() < Fx::from_ratio(1, 1000),
            "{hit:?}"
        );
        assert!(w.raycast(v(0, 5, 0), v(0, 1, 0), u32::MAX).is_none());
    }
}
