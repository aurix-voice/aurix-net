//! Flat ABI over `net-sim-physics3d` (`sim3_*`): raw Q32.32 `i64`s, u32 handles/ids, its own
//! slot table so 2D and 3D worlds can coexist in one module.

use net_fpmath::{Fx, FxQuat, FxVec3};
use net_sim_physics3d::{
    Body, BodyId, BodyKind, CharacterController, Joint, JointId, JointKind, Shape, World,
    WorldConfig,
};
use std::cell::RefCell;

struct Slot3 {
    world: World,
    snapshot: Vec<u8>,
    kcc: Vec<CharacterController>,
    hull_verts: Vec<FxVec3>,
    hull_faces: Vec<Vec<u32>>,
    hull_face: Vec<u32>,
}

thread_local! {
    static WORLDS3: RefCell<Vec<Option<Slot3>>> = const { RefCell::new(Vec::new()) };
    static LAST_RAY3: RefCell<[i64; 7]> = const { RefCell::new([0; 7]) };
}

fn with3<R>(h: u32, f: impl FnOnce(&mut Slot3) -> R) -> Option<R> {
    WORLDS3.with(|w| {
        w.borrow_mut()
            .get_mut(h as usize)
            .and_then(|s| s.as_mut())
            .map(f)
    })
}

fn kind3(k: u32) -> BodyKind {
    match k {
        0 => BodyKind::Static,
        1 => BodyKind::Kinematic,
        _ => BodyKind::Dynamic,
    }
}

fn v3(x: i64, y: i64, z: i64) -> FxVec3 {
    FxVec3::new(Fx(x), Fx(y), Fx(z))
}

/// Create a 3D world: gravity (raw Fx), tick rate; returns a handle.
#[no_mangle]
pub extern "C" fn sim3_world_new(gx: i64, gy: i64, gz: i64, tick_hz: u32) -> u32 {
    let cfg = WorldConfig {
        gravity: v3(gx, gy, gz),
        dt: Fx::from_ratio(1, tick_hz.max(1) as i64),
        ..WorldConfig::default()
    };
    let slot = Slot3 {
        world: World::new(cfg),
        snapshot: Vec::new(),
        kcc: Vec::new(),
        hull_verts: Vec::new(),
        hull_faces: Vec::new(),
        hull_face: Vec::new(),
    };
    WORLDS3.with(|w| {
        let mut w = w.borrow_mut();
        if let Some(i) = w.iter().position(|s| s.is_none()) {
            w[i] = Some(slot);
            i as u32
        } else {
            w.push(Some(slot));
            (w.len() - 1) as u32
        }
    })
}

#[no_mangle]
pub extern "C" fn sim3_world_free(h: u32) {
    WORLDS3.with(|w| {
        if let Some(s) = w.borrow_mut().get_mut(h as usize) {
            *s = None;
        }
    });
}

#[no_mangle]
pub extern "C" fn sim3_step(h: u32) {
    with3(h, |s| s.world.step());
}

#[no_mangle]
pub extern "C" fn sim3_tick(h: u32) -> u64 {
    with3(h, |s| s.world.tick).unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn sim3_hash(h: u32) -> u64 {
    with3(h, |s| s.world.hash()).unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn sim3_snapshot(h: u32) -> u32 {
    with3(h, |s| {
        s.snapshot = s.world.snapshot();
        s.snapshot.len() as u32
    })
    .unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn sim3_snapshot_byte(h: u32, i: u32) -> u32 {
    with3(h, |s| {
        s.snapshot.get(i as usize).copied().unwrap_or(0) as u32
    })
    .unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn sim3_snapshot_set_len(h: u32, len: u32) {
    with3(h, |s| {
        s.snapshot.clear();
        s.snapshot.resize(len as usize, 0);
    });
}

#[no_mangle]
pub extern "C" fn sim3_snapshot_set_byte(h: u32, i: u32, b: u32) {
    with3(h, |s| {
        if let Some(x) = s.snapshot.get_mut(i as usize) {
            *x = b as u8;
        }
    });
}

#[no_mangle]
pub extern "C" fn sim3_restore(h: u32) -> u32 {
    with3(h, |s| match World::restore(&s.snapshot) {
        Some(w) => {
            s.world = w;
            1
        }
        None => 0,
    })
    .unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn sim3_add_sphere(
    h: u32,
    k: u32,
    x: i64,
    y: i64,
    z: i64,
    radius: i64,
    density: i64,
) -> u32 {
    with3(h, |s| {
        s.world
            .add(Body::with_density(
                kind3(k),
                Shape::Sphere { radius: Fx(radius) },
                v3(x, y, z),
                Fx(density),
            ))
            .0
    })
    .unwrap_or(0)
}

#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn sim3_add_box(
    h: u32,
    k: u32,
    x: i64,
    y: i64,
    z: i64,
    hx: i64,
    hy: i64,
    hz: i64,
    density: i64,
) -> u32 {
    with3(h, |s| {
        s.world
            .add(Body::with_density(
                kind3(k),
                Shape::Box {
                    half: v3(hx, hy, hz),
                },
                v3(x, y, z),
                Fx(density),
            ))
            .0
    })
    .unwrap_or(0)
}

/// Capsule along the body y axis.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn sim3_add_capsule(
    h: u32,
    k: u32,
    x: i64,
    y: i64,
    z: i64,
    half_len: i64,
    radius: i64,
    density: i64,
) -> u32 {
    with3(h, |s| {
        s.world
            .add(Body::with_density(
                kind3(k),
                Shape::Capsule {
                    half_len: Fx(half_len),
                    radius: Fx(radius),
                },
                v3(x, y, z),
                Fx(density),
            ))
            .0
    })
    .unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn sim3_remove_body(h: u32, id: u32) -> u32 {
    with3(h, |s| s.world.remove(BodyId(id)).is_some() as u32).unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn sim3_set_material(h: u32, id: u32, friction: i64, restitution: i64, sensor: u32) {
    with3(h, |s| {
        if let Some(b) = s.world.body_mut(BodyId(id)) {
            b.friction = Fx(friction);
            b.restitution = Fx(restitution);
            b.sensor = sensor != 0;
        }
    });
}

#[no_mangle]
pub extern "C" fn sim3_set_layers(h: u32, id: u32, layer: u32, mask: u32) {
    with3(h, |s| {
        if let Some(b) = s.world.body_mut(BodyId(id)) {
            b.layer = layer;
            b.mask = mask;
        }
    });
}

#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn sim3_set_velocity(
    h: u32,
    id: u32,
    vx: i64,
    vy: i64,
    vz: i64,
    wx: i64,
    wy: i64,
    wz: i64,
) {
    with3(h, |s| {
        if let Some(b) = s.world.body_mut(BodyId(id)) {
            b.velocity = v3(vx, vy, vz);
            b.angular_velocity = v3(wx, wy, wz);
        }
    });
}

#[no_mangle]
pub extern "C" fn sim3_set_position(h: u32, id: u32, x: i64, y: i64, z: i64) {
    with3(h, |s| {
        if let Some(b) = s.world.body_mut(BodyId(id)) {
            b.position = v3(x, y, z);
        }
    });
}

/// Set orientation as a quaternion (x, y, z, w), normalised on entry.
#[no_mangle]
pub extern "C" fn sim3_set_rotation(h: u32, id: u32, qx: i64, qy: i64, qz: i64, qw: i64) {
    with3(h, |s| {
        if let Some(b) = s.world.body_mut(BodyId(id)) {
            b.rotation = FxQuat::new(Fx(qx), Fx(qy), Fx(qz), Fx(qw)).normalized();
        }
    });
}

/// Impulse at the centre of mass (`at_point` == 0) or at world point (px, py, pz).
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn sim3_apply_impulse(
    h: u32,
    id: u32,
    ix: i64,
    iy: i64,
    iz: i64,
    at_point: u32,
    px: i64,
    py: i64,
    pz: i64,
) {
    with3(h, |s| {
        s.world.apply_impulse(
            BodyId(id),
            v3(ix, iy, iz),
            (at_point != 0).then(|| v3(px, py, pz)),
        );
    });
}

/// Body state: 0..=2 position, 3..=6 rotation (x, y, z, w), 7..=9 velocity, 10..=12 angular velocity.
#[no_mangle]
pub extern "C" fn sim3_body_get(h: u32, id: u32, field: u32) -> i64 {
    with3(h, |s| {
        s.world
            .body(BodyId(id))
            .map(|b| match field {
                0 => b.position.x.0,
                1 => b.position.y.0,
                2 => b.position.z.0,
                3 => b.rotation.x.0,
                4 => b.rotation.y.0,
                5 => b.rotation.z.0,
                6 => b.rotation.w.0,
                7 => b.velocity.x.0,
                8 => b.velocity.y.0,
                9 => b.velocity.z.0,
                10 => b.angular_velocity.x.0,
                11 => b.angular_velocity.y.0,
                _ => b.angular_velocity.z.0,
            })
            .unwrap_or(0)
    })
    .unwrap_or(0)
}

/// Raycast; returns body id + 1 (0 = miss) and fills `sim3_ray_get`: 0 fraction, 1..=3 point, 4..=6 normal.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn sim3_raycast(
    h: u32,
    x: i64,
    y: i64,
    z: i64,
    dx: i64,
    dy: i64,
    dz: i64,
    mask: u32,
) -> u32 {
    with3(h, |s| {
        match s.world.raycast(v3(x, y, z), v3(dx, dy, dz), mask) {
            Some(hit) => {
                LAST_RAY3.with(|r| {
                    *r.borrow_mut() = [
                        hit.fraction.0,
                        hit.point.x.0,
                        hit.point.y.0,
                        hit.point.z.0,
                        hit.normal.x.0,
                        hit.normal.y.0,
                        hit.normal.z.0,
                    ];
                });
                hit.body.0 + 1
            }
            None => 0,
        }
    })
    .unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn sim3_ray_get(field: u32) -> i64 {
    LAST_RAY3.with(|r| r.borrow().get(field as usize).copied().unwrap_or(0))
}

#[no_mangle]
pub extern "C" fn sim3_events_len(h: u32) -> u32 {
    with3(h, |s| s.world.events().len() as u32).unwrap_or(0)
}

/// Event field: 0 = body a, 1 = body b, 2 = begin (1) / end (0).
#[no_mangle]
pub extern "C" fn sim3_event_get(h: u32, i: u32, field: u32) -> u32 {
    with3(h, |s| {
        s.world
            .events()
            .get(i as usize)
            .map(|e| match field {
                0 => e.a.0,
                1 => e.b.0,
                _ => e.begin as u32,
            })
            .unwrap_or(0)
    })
    .unwrap_or(0)
}

/// 3D reference scenario: floor, a 3-level box pyramid, a tilted box, a lying capsule and two
/// spheres (one bouncy, one rolling into the pyramid); 240 ticks. Equal on native and wasm32.
#[no_mangle]
pub extern "C" fn sim3_golden_hash() -> u64 {
    let one = Fx::ONE.0;
    let half = Fx::HALF.0;
    let h = sim3_world_new(0, Fx::from_int(-10).0, 0, 60);
    sim3_add_box(
        h,
        0,
        0,
        -half,
        0,
        Fx::from_int(20).0,
        half,
        Fx::from_int(20).0,
        one,
    );
    for level in 0..3i32 {
        for i in 0..(3 - level) {
            let x = Fx::from_int(2 * i - (2 - level)).0;
            let y = (Fx::HALF + Fx::from_int(level)).0;
            sim3_add_box(h, 2, x, y, 0, half, half, half, one);
        }
    }
    let tilted = sim3_add_box(
        h,
        2,
        Fx::from_int(5).0,
        Fx::from_int(3).0,
        0,
        half,
        half,
        half,
        one,
    );
    let q = FxQuat::from_axis_angle(
        FxVec3::new(Fx::ZERO, Fx::ZERO, Fx::ONE),
        Fx::from_ratio(3, 10),
    );
    sim3_set_rotation(h, tilted, q.x.0, q.y.0, q.z.0, q.w.0);
    let cap = sim3_add_capsule(
        h,
        2,
        Fx::from_int(-5).0,
        Fx::from_int(3).0,
        0,
        one,
        half,
        one,
    );
    let qc = FxQuat::from_axis_angle(FxVec3::new(Fx::ZERO, Fx::ZERO, Fx::ONE), Fx::HALF_PI);
    sim3_set_rotation(h, cap, qc.x.0, qc.y.0, qc.z.0, qc.w.0);
    let bouncy = sim3_add_sphere(h, 2, 0, Fx::from_int(6).0, Fx::from_int(5).0, half, one);
    sim3_set_material(h, bouncy, half, Fx::from_ratio(8, 10).0, 0);
    let rolling = sim3_add_sphere(h, 2, 0, half, Fx::from_int(-8).0, half, one);
    sim3_set_velocity(h, rolling, 0, 0, Fx::from_int(6).0, 0, 0, 0);
    let mut events = 0u64;
    for _ in 0..240 {
        sim3_step(h);
        events += sim3_events_len(h) as u64;
    }
    let hash = sim3_hash(h) ^ events.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    sim3_world_free(h);
    hash
}

// ---- joints and character controller --------------------------------------------------------

#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn sim3_joint_ball(
    h: u32,
    a: u32,
    b: u32,
    ax: i64,
    ay: i64,
    az: i64,
    bx: i64,
    by: i64,
    bz: i64,
) -> u32 {
    with3(h, |s| {
        s.world
            .add_joint(Joint::new(
                BodyId(a),
                BodyId(b),
                v3(ax, ay, az),
                v3(bx, by, bz),
                JointKind::Ball,
            ))
            .0
    })
    .unwrap_or(0)
}

/// Hinge around local axes (ua, ub); `has_limits` != 0 enables [lower, upper]; motor when `max_torque` > 0.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn sim3_joint_hinge(
    h: u32,
    a: u32,
    b: u32,
    ax: i64,
    ay: i64,
    az: i64,
    bx: i64,
    by: i64,
    bz: i64,
    uax: i64,
    uay: i64,
    uaz: i64,
    ubx: i64,
    uby: i64,
    ubz: i64,
    has_limits: u32,
    lower: i64,
    upper: i64,
    motor_speed: i64,
    max_torque: i64,
) -> u32 {
    with3(h, |s| {
        s.world
            .add_joint(Joint::new(
                BodyId(a),
                BodyId(b),
                v3(ax, ay, az),
                v3(bx, by, bz),
                JointKind::Hinge {
                    axis_a: v3(uax, uay, uaz),
                    axis_b: v3(ubx, uby, ubz),
                    lower: (has_limits != 0).then_some(Fx(lower)),
                    upper: (has_limits != 0).then_some(Fx(upper)),
                    motor_speed: Fx(motor_speed),
                    max_motor_torque: Fx(max_torque),
                },
            ))
            .0
    })
    .unwrap_or(0)
}

#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn sim3_joint_distance(
    h: u32,
    a: u32,
    b: u32,
    ax: i64,
    ay: i64,
    az: i64,
    bx: i64,
    by: i64,
    bz: i64,
    len: i64,
    min_len: i64,
) -> u32 {
    with3(h, |s| {
        s.world
            .add_joint(Joint::new(
                BodyId(a),
                BodyId(b),
                v3(ax, ay, az),
                v3(bx, by, bz),
                JointKind::Distance {
                    length: Fx(len),
                    min_length: Fx(min_len),
                },
            ))
            .0
    })
    .unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn sim3_joint_remove(h: u32, id: u32) -> u32 {
    with3(h, |s| s.world.remove_joint(JointId(id)).is_some() as u32).unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn sim3_joint_motor(h: u32, id: u32, speed: i64, max_torque: i64) {
    with3(h, |s| {
        if let Some(j) = s.world.joint_mut(JointId(id)) {
            if let JointKind::Hinge {
                motor_speed,
                max_motor_torque,
                ..
            } = &mut j.kind
            {
                *motor_speed = Fx(speed);
                *max_motor_torque = Fx(max_torque);
            }
        }
    });
}

/// Hinge angle in radians (raw Fx), 0 for other joints.
#[no_mangle]
pub extern "C" fn sim3_hinge_angle(h: u32, id: u32) -> i64 {
    with3(h, |s| {
        s.world.hinge_angle(JointId(id)).map(|a| a.0).unwrap_or(0)
    })
    .unwrap_or(0)
}

/// Capsule character controller over a kinematic capsule body; returns its index.
#[no_mangle]
pub extern "C" fn sim3_kcc_new(
    h: u32,
    body: u32,
    step_height: i64,
    min_ground_ny: i64,
    snap: i64,
) -> u32 {
    with3(h, |s| {
        let mut k = CharacterController::new(BodyId(body));
        k.step_height = Fx(step_height);
        k.min_ground_normal_y = Fx(min_ground_ny);
        k.snap_distance = Fx(snap);
        s.kcc.push(k);
        (s.kcc.len() - 1) as u32
    })
    .unwrap_or(0)
}

/// Move the controller by (dx, dy, dz) this tick; returns grounded (1/0). Read the body for position.
#[no_mangle]
pub extern "C" fn sim3_kcc_move(h: u32, k: u32, dx: i64, dy: i64, dz: i64) -> u32 {
    with3(h, |s| {
        let Some(kcc) = s.kcc.get_mut(k as usize) else {
            return 0;
        };
        kcc.step(&mut s.world, v3(dx, dy, dz));
        kcc.grounded as u32
    })
    .unwrap_or(0)
}

/// Controller state: 0..=2 ground normal, 3..=5 velocity, 6 grounded, 7 ground body (+1, 0 = none).
#[no_mangle]
pub extern "C" fn sim3_kcc_get(h: u32, k: u32, field: u32) -> i64 {
    with3(h, |s| {
        s.kcc
            .get(k as usize)
            .map(|c| match field {
                0 => c.ground_normal.x.0,
                1 => c.ground_normal.y.0,
                2 => c.ground_normal.z.0,
                3 => c.velocity.x.0,
                4 => c.velocity.y.0,
                5 => c.velocity.z.0,
                6 => c.grounded as i64,
                _ => c.ground_body.map(|b| b.0 as i64 + 1).unwrap_or(0),
            })
            .unwrap_or(0)
    })
    .unwrap_or(0)
}

// ---- convex hulls (staged per world: verts, then faces as index loops) and sweeps ----

/// Clear the staged hull.
#[no_mangle]
pub extern "C" fn sim3_convex_reset(h: u32) {
    with3(h, |s| {
        s.hull_verts.clear();
        s.hull_faces.clear();
        s.hull_face.clear();
    });
}

/// Push a body-space vertex (raw Fx); at most 32.
#[no_mangle]
pub extern "C" fn sim3_convex_vert(h: u32, x: i64, y: i64, z: i64) -> u32 {
    with3(h, |s| {
        if s.hull_verts.len() < 32 {
            s.hull_verts.push(v3(x, y, z));
        }
        s.hull_verts.len() as u32
    })
    .unwrap_or(0)
}

/// Push a vertex index onto the face being built.
#[no_mangle]
pub extern "C" fn sim3_convex_index(h: u32, i: u32) {
    with3(h, |s| {
        if s.hull_face.len() < 32 {
            s.hull_face.push(i);
        }
    });
}

/// Close the current face (>= 3 indices, any winding); at most 32 faces.
#[no_mangle]
pub extern "C" fn sim3_convex_face_end(h: u32) -> u32 {
    with3(h, |s| {
        let f = std::mem::take(&mut s.hull_face);
        if f.len() >= 3
            && s.hull_faces.len() < 32
            && f.iter().all(|i| (*i as usize) < s.hull_verts.len())
        {
            s.hull_faces.push(f);
        }
        s.hull_faces.len() as u32
    })
    .unwrap_or(0)
}

/// Add a convex body from the staged hull (which stays staged for reuse); 0 if the hull is invalid.
#[no_mangle]
pub extern "C" fn sim3_add_convex(h: u32, k: u32, x: i64, y: i64, z: i64, density: i64) -> u32 {
    with3(h, |s| {
        if s.hull_verts.len() < 4 || s.hull_faces.len() < 4 {
            return 0;
        }
        s.world
            .add(Body::with_density(
                kind3(k),
                Shape::Convex {
                    verts: s.hull_verts.clone(),
                    faces: s.hull_faces.clone(),
                },
                v3(x, y, z),
                Fx(density),
            ))
            .0
            + 1
    })
    .unwrap_or(0)
}

/// Sphere sweep; result fields as `sim3_ray_get`. Returns body id + 1 or 0.
#[no_mangle]
pub extern "C" fn sim3_spherecast(
    h: u32,
    x: i64,
    y: i64,
    z: i64,
    dx: i64,
    dy: i64,
    dz: i64,
    radius: i64,
    mask: u32,
) -> u32 {
    with3(h, |s| {
        match s
            .world
            .sphere_cast(v3(x, y, z), v3(dx, dy, dz), Fx(radius), mask)
        {
            Some(hit) => {
                LAST_RAY3.with(|r| {
                    *r.borrow_mut() = [
                        hit.fraction.0,
                        hit.point.x.0,
                        hit.point.y.0,
                        hit.point.z.0,
                        hit.normal.x.0,
                        hit.normal.y.0,
                        hit.normal.z.0,
                    ];
                });
                hit.body.0 + 1
            }
            None => 0,
        }
    })
    .unwrap_or(0)
}

/// Convex reference scenario: three pyramids dropped in a column, a box sliding off a static
/// wedge, a sphere rolling onto a pyramid, plus a sphere sweep mixed into the hash; 300 ticks.
#[no_mangle]
pub extern "C" fn sim3_hull_golden_hash() -> u64 {
    let h = sim3_world_new(0, Fx::from_int(-10).0, 0, 60);
    let one = Fx::ONE.0;
    let half = Fx::HALF.0;
    let q = Fx::from_ratio(1, 4).0;
    sim3_add_box(
        h,
        0,
        0,
        -half,
        0,
        Fx::from_int(30).0,
        half,
        Fx::from_int(30).0,
        one,
    );
    sim3_convex_reset(h);
    for (x, z) in [(-half, -half), (half, -half), (half, half), (-half, half)] {
        sim3_convex_vert(h, x, -q, z);
    }
    sim3_convex_vert(h, 0, Fx::from_ratio(3, 4).0, 0);
    for f in [
        [0, 1, 2, 3],
        [0, 1, 4, u32::MAX],
        [1, 2, 4, u32::MAX],
        [2, 3, 4, u32::MAX],
        [3, 0, 4, u32::MAX],
    ] {
        for i in f {
            if i != u32::MAX {
                sim3_convex_index(h, i);
            }
        }
        sim3_convex_face_end(h);
    }
    for i in 0..3i32 {
        sim3_add_convex(
            h,
            2,
            Fx::from_ratio(i as i64, 20).0,
            Fx::from_int(1 + 2 * i).0,
            0,
            one,
        );
    }
    sim3_add_convex(h, 0, Fx::from_int(6).0, 0, 0, one);
    sim3_add_box(
        h,
        2,
        Fx::from_ratio(62, 10).0,
        Fx::from_int(2).0,
        0,
        q,
        q,
        q,
        one,
    );
    let ball = sim3_add_sphere(h, 2, Fx::from_int(-4).0, half, 0, half, one) - 1;
    sim3_set_velocity(h, ball, Fx::from_int(4).0, 0, 0, 0, 0, 0);
    for _ in 0..300 {
        sim3_step(h);
    }
    let mut x = sim3_hash(h);
    if sim3_spherecast(
        h,
        Fx::from_int(-8).0,
        half,
        0,
        Fx::from_int(30).0,
        0,
        0,
        q,
        u32::MAX,
    ) != 0
    {
        for i in 0..7 {
            x ^= (sim3_ray_get(i) as u64).rotate_left(i * 7);
        }
    }
    sim3_world_free(h);
    x
}

/// 3D joint/character reference scenario: ball pendulum, rope, limited hinge door, motor wheel and
/// a capsule character walking over a step into a wall; 300 ticks. Equal on native and wasm32.
#[no_mangle]
pub extern "C" fn sim3_joint_golden_hash() -> u64 {
    let one = Fx::ONE.0;
    let half = Fx::HALF.0;
    let f = |v: i32| Fx::from_int(v).0;
    let h = sim3_world_new(0, f(-10), 0, 60);
    sim3_add_box(h, 0, 0, -half, 0, f(20), half, f(20), one);
    let anchor = sim3_add_sphere(h, 0, 0, f(10), 0, Fx::from_ratio(1, 10).0, one);
    let bob = sim3_add_sphere(h, 2, f(3), f(10), 0, Fx::from_ratio(1, 4).0, one);
    sim3_joint_ball(h, anchor, bob, 0, 0, 0, f(-3), 0, 0);
    let anchor2 = sim3_add_sphere(h, 0, f(10), f(10), 0, Fx::from_ratio(1, 10).0, one);
    let rope_end = sim3_add_sphere(h, 2, f(10), f(9), 0, Fx::from_ratio(1, 4).0, one);
    sim3_joint_distance(h, anchor2, rope_end, 0, 0, 0, 0, 0, 0, f(3), 0);
    let post = sim3_add_box(
        h,
        0,
        f(-10),
        f(1),
        0,
        Fx::from_ratio(1, 10).0,
        f(1),
        Fx::from_ratio(1, 10).0,
        one,
    );
    let door = sim3_add_box(
        h,
        2,
        (Fx::from_int(-10) + Fx::from_ratio(6, 10)).0,
        f(1),
        0,
        half,
        f(1),
        Fx::from_ratio(1, 20).0,
        one,
    );
    sim3_joint_hinge(
        h,
        post,
        door,
        Fx::from_ratio(1, 10).0,
        0,
        0,
        -half,
        0,
        0,
        0,
        one,
        0,
        0,
        one,
        0,
        1,
        0,
        Fx::HALF_PI.0,
        0,
        0,
    );
    sim3_apply_impulse(
        h,
        door,
        0,
        0,
        f(8),
        1,
        (Fx::from_int(-10) + Fx::ONE).0,
        f(1),
        0,
    );
    let axle = sim3_add_sphere(h, 0, f(20), f(5), 0, Fx::from_ratio(1, 10).0, one);
    let wheel = sim3_add_sphere(h, 2, f(20), f(5), 0, one, one);
    sim3_joint_hinge(
        h,
        axle,
        wheel,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        one,
        0,
        0,
        one,
        0,
        0,
        0,
        f(4),
        f(50),
    );
    sim3_add_box(
        h,
        0,
        f(4),
        Fx::from_ratio(15, 100).0,
        f(10),
        one,
        Fx::from_ratio(15, 100).0,
        f(2),
        one,
    );
    sim3_add_box(
        h,
        0,
        f(8),
        f(1),
        f(10),
        Fx::from_ratio(1, 4).0,
        one,
        f(2),
        one,
    );
    let hero = sim3_add_capsule(
        h,
        1,
        0,
        Fx::from_ratio(8, 10).0,
        f(10),
        half,
        Fx::from_ratio(3, 10).0,
        one,
    );
    let k = sim3_kcc_new(
        h,
        hero,
        half,
        Fx::from_ratio(7, 10).0,
        Fx::from_ratio(1, 4).0,
    );
    let dt = Fx::from_ratio(1, 60);
    for _ in 0..300 {
        sim3_kcc_move(
            h,
            k,
            (Fx::from_int(3) * dt).0,
            (Fx::from_int(-10) * dt).0,
            0,
        );
        sim3_step(h);
    }
    let hash = sim3_hash(h) ^ (sim3_hinge_angle(h, 3) as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    sim3_world_free(h);
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joint3_golden_is_stable() {
        let a = sim3_joint_golden_hash();
        assert_eq!(a, sim3_joint_golden_hash());
        eprintln!("native 3d joint golden hash: {a:#x}");
    }

    #[test]
    fn golden3_is_stable_and_abi_round_trips() {
        let a = sim3_golden_hash();
        assert_eq!(a, sim3_golden_hash());
        eprintln!("native 3d golden hash: {a:#x}");
        let h = sim3_world_new(0, Fx::from_int(-10).0, 0, 60);
        sim3_add_box(
            h,
            0,
            0,
            -Fx::HALF.0,
            0,
            Fx::from_int(10).0,
            Fx::HALF.0,
            Fx::from_int(10).0,
            Fx::ONE.0,
        );
        let ball = sim3_add_sphere(h, 2, 0, Fx::from_int(3).0, 0, Fx::HALF.0, Fx::ONE.0);
        for _ in 0..240 {
            sim3_step(h);
        }
        assert!((Fx(sim3_body_get(h, ball, 1)) - Fx::HALF).abs() < Fx::from_ratio(3, 100));
        let n = sim3_snapshot(h);
        assert!(n > 0);
        let hash = sim3_hash(h);
        sim3_set_position(h, ball, 0, Fx::from_int(9).0, 0);
        assert_ne!(sim3_hash(h), hash);
        assert_eq!(sim3_restore(h), 1);
        assert_eq!(sim3_hash(h), hash);
        assert_eq!(
            sim3_raycast(
                h,
                0,
                Fx::from_int(5).0,
                0,
                0,
                Fx::from_int(-10).0,
                0,
                u32::MAX
            ),
            ball + 1
        );
        assert!(
            (Fx(sim3_ray_get(2)) - Fx::ONE).abs() < Fx::from_ratio(5, 100),
            "{}",
            Fx(sim3_ray_get(2))
        );
        sim3_world_free(h);
    }
}
