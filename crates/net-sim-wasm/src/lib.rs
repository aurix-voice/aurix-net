//! Flat ABI over `net-sim-physics`: ids and Q32.32 raw `i64` values only, so the same `.wasm`
//! runs in browsers, Unity (WebGL/IL2CPP via a loader) and on the node, and a C ABI build serves
//! C++/Godot. Worlds live in a slot table; every function takes a world handle.
#![deny(clippy::float_arithmetic)]

use net_fpmath::{Fx, FxVec2};
use net_sim_nav::{Agent, Crowd, CrowdConfig, FlowField, NavMesh};
use net_sim_physics::{
    Body, BodyId, BodyKind, CharacterController, Joint, JointId, JointKind, Shape, World,
    WorldConfig,
};
use std::cell::RefCell;

struct Slot {
    world: World,
    kcc: Vec<CharacterController>,
    snapshot: Vec<u8>,
    nav: Option<NavMesh>,
    crowd: Crowd,
    /// Output buffer for paths (x0, y0, x1, y1, ...) read with `sim_nav_point`.
    points: Vec<i64>,
}

thread_local! {
    static WORLDS: RefCell<Vec<Option<Slot>>> = const { RefCell::new(Vec::new()) };
}

fn with<R>(h: u32, f: impl FnOnce(&mut Slot) -> R) -> Option<R> {
    WORLDS.with(|w| {
        w.borrow_mut()
            .get_mut(h as usize)
            .and_then(|s| s.as_mut())
            .map(f)
    })
}

fn kind(k: u32) -> BodyKind {
    match k {
        0 => BodyKind::Static,
        1 => BodyKind::Kinematic,
        _ => BodyKind::Dynamic,
    }
}

/// Create a world: gravity (raw Fx), tick rate; returns a handle.
#[no_mangle]
pub extern "C" fn sim_world_new(gravity_x: i64, gravity_y: i64, tick_hz: u32) -> u32 {
    let cfg = WorldConfig {
        gravity: FxVec2::new(Fx(gravity_x), Fx(gravity_y)),
        dt: Fx::from_ratio(1, tick_hz.max(1) as i64),
        ..WorldConfig::default()
    };
    let slot = Slot {
        world: World::new(cfg),
        kcc: Vec::new(),
        snapshot: Vec::new(),
        nav: None,
        crowd: Crowd::new(CrowdConfig {
            dt: Fx::from_ratio(1, tick_hz.max(1) as i64),
            ..CrowdConfig::default()
        }),
        points: Vec::new(),
    };
    WORLDS.with(|w| {
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
pub extern "C" fn sim_world_free(h: u32) {
    WORLDS.with(|w| {
        if let Some(s) = w.borrow_mut().get_mut(h as usize) {
            *s = None;
        }
    });
}

#[no_mangle]
pub extern "C" fn sim_step(h: u32) {
    with(h, |s| s.world.step());
}

#[no_mangle]
pub extern "C" fn sim_tick(h: u32) -> u32 {
    with(h, |s| s.world.tick).unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn sim_hash(h: u32) -> u64 {
    with(h, |s| s.world.hash()).unwrap_or(0)
}

/// Snapshot into an internal buffer; returns its length (read with `sim_snapshot_byte`).
#[no_mangle]
pub extern "C" fn sim_snapshot(h: u32) -> u32 {
    with(h, |s| {
        s.snapshot = s.world.snapshot();
        s.snapshot.len() as u32
    })
    .unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn sim_snapshot_byte(h: u32, i: u32) -> u32 {
    with(h, |s| {
        s.snapshot.get(i as usize).copied().unwrap_or(0) as u32
    })
    .unwrap_or(0)
}

/// Restore from the internal buffer (fill it with `sim_snapshot_set_byte` first).
#[no_mangle]
pub extern "C" fn sim_snapshot_set_len(h: u32, len: u32) {
    with(h, |s| s.snapshot = vec![0; len as usize]);
}
#[no_mangle]
pub extern "C" fn sim_snapshot_set_byte(h: u32, i: u32, v: u32) {
    with(h, |s| {
        if let Some(b) = s.snapshot.get_mut(i as usize) {
            *b = v as u8;
        }
    });
}
#[no_mangle]
pub extern "C" fn sim_restore(h: u32) -> u32 {
    with(h, |s| match World::restore(&s.snapshot) {
        Some(w) => {
            s.world = w;
            1
        }
        None => 0,
    })
    .unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn sim_add_circle(h: u32, k: u32, x: i64, y: i64, radius: i64, density: i64) -> u32 {
    with(h, |s| {
        s.world
            .add(
                Body::new(
                    kind(k),
                    Shape::Circle { radius: Fx(radius) },
                    FxVec2::new(Fx(x), Fx(y)),
                )
                .with_density(Fx(density)),
            )
            .0
    })
    .unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn sim_add_box(
    h: u32,
    k: u32,
    x: i64,
    y: i64,
    half_w: i64,
    half_h: i64,
    density: i64,
) -> u32 {
    with(h, |s| {
        s.world
            .add(
                Body::new(
                    kind(k),
                    Shape::rect(Fx(half_w), Fx(half_h)),
                    FxVec2::new(Fx(x), Fx(y)),
                )
                .with_density(Fx(density)),
            )
            .0
    })
    .unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn sim_add_capsule(
    h: u32,
    k: u32,
    x: i64,
    y: i64,
    half_len: i64,
    radius: i64,
    density: i64,
) -> u32 {
    with(h, |s| {
        s.world
            .add(
                Body::new(
                    kind(k),
                    Shape::Capsule {
                        half_len: Fx(half_len),
                        radius: Fx(radius),
                    },
                    FxVec2::new(Fx(x), Fx(y)),
                )
                .with_density(Fx(density)),
            )
            .0
    })
    .unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn sim_remove_body(h: u32, id: u32) {
    with(h, |s| s.world.remove(BodyId(id)));
}

#[no_mangle]
pub extern "C" fn sim_set_material(h: u32, id: u32, friction: i64, restitution: i64) {
    with(h, |s| {
        if let Some(b) = s.world.body_mut(BodyId(id)) {
            b.friction = Fx(friction);
            b.restitution = Fx(restitution);
        }
    });
}

#[no_mangle]
pub extern "C" fn sim_set_layers(h: u32, id: u32, layer: u32, mask: u32, sensor: u32) {
    with(h, |s| {
        if let Some(b) = s.world.body_mut(BodyId(id)) {
            b.layer = layer;
            b.mask = mask;
            b.sensor = sensor != 0;
        }
    });
}

#[no_mangle]
pub extern "C" fn sim_set_velocity(h: u32, id: u32, vx: i64, vy: i64, w: i64) {
    with(h, |s| {
        if let Some(b) = s.world.body_mut(BodyId(id)) {
            b.velocity = FxVec2::new(Fx(vx), Fx(vy));
            b.angular_velocity = Fx(w);
        }
    });
}

#[no_mangle]
pub extern "C" fn sim_set_position(h: u32, id: u32, x: i64, y: i64, angle: i64) {
    with(h, |s| {
        if let Some(b) = s.world.body_mut(BodyId(id)) {
            b.position = FxVec2::new(Fx(x), Fx(y));
            b.angle = Fx(angle);
        }
    });
}

#[no_mangle]
pub extern "C" fn sim_apply_impulse(h: u32, id: u32, ix: i64, iy: i64) {
    with(h, |s| {
        s.world
            .apply_impulse(BodyId(id), FxVec2::new(Fx(ix), Fx(iy)))
    });
}

/// Body state accessors: field 0..=4 = x, y, angle, vx, vy, 5 = angular velocity.
#[no_mangle]
pub extern "C" fn sim_body_get(h: u32, id: u32, field: u32) -> i64 {
    with(h, |s| {
        s.world
            .body(BodyId(id))
            .map(|b| match field {
                0 => b.position.x.0,
                1 => b.position.y.0,
                2 => b.angle.0,
                3 => b.velocity.x.0,
                4 => b.velocity.y.0,
                _ => b.angular_velocity.0,
            })
            .unwrap_or(0)
    })
    .unwrap_or(0)
}

/// Raycast; returns body id (0 = miss) and stores fraction/point/normal for `sim_ray_get` (0..=4).
#[no_mangle]
pub extern "C" fn sim_raycast(h: u32, x: i64, y: i64, dx: i64, dy: i64, mask: u32) -> u32 {
    with(h, |s| {
        match s
            .world
            .raycast(FxVec2::new(Fx(x), Fx(y)), FxVec2::new(Fx(dx), Fx(dy)), mask)
        {
            Some(hit) => {
                LAST_RAY.with(|r| {
                    *r.borrow_mut() = [
                        hit.fraction.0,
                        hit.point.x.0,
                        hit.point.y.0,
                        hit.normal.x.0,
                        hit.normal.y.0,
                    ]
                });
                hit.body.0
            }
            None => 0,
        }
    })
    .unwrap_or(0)
}

thread_local! {
    static LAST_RAY: RefCell<[i64; 5]> = const { RefCell::new([0; 5]) };
}

#[no_mangle]
pub extern "C" fn sim_ray_get(field: u32) -> i64 {
    LAST_RAY.with(|r| r.borrow().get(field as usize).copied().unwrap_or(0))
}

/// Character controller over a kinematic body; returns a controller index.
#[no_mangle]
pub extern "C" fn sim_kcc_new(h: u32, body: u32, step_height: i64, min_ground_ny: i64) -> u32 {
    with(h, |s| {
        let mut k = CharacterController::new(BodyId(body));
        k.step_height = Fx(step_height);
        k.min_ground_normal_y = Fx(min_ground_ny);
        s.kcc.push(k);
        (s.kcc.len() - 1) as u32
    })
    .unwrap_or(0)
}

/// Move the controller by (dx, dy); returns 1 when grounded afterwards.
#[no_mangle]
pub extern "C" fn sim_kcc_move(h: u32, k: u32, dx: i64, dy: i64) -> u32 {
    with(h, |s| {
        let Some(mut kcc) = s.kcc.get(k as usize).cloned() else {
            return 0;
        };
        kcc.step(&mut s.world, FxVec2::new(Fx(dx), Fx(dy)));
        let g = kcc.grounded as u32;
        s.kcc[k as usize] = kcc;
        g
    })
    .unwrap_or(0)
}

/// Reference scenario used to prove cross-target determinism: a box pyramid plus bouncing balls
/// simulated for 240 ticks. Native tests and the wasm build must return the same value.
#[no_mangle]
pub extern "C" fn sim_golden_hash() -> u64 {
    let h = sim_world_new(0, Fx::from_int(-10).0, 60);
    sim_add_box(
        h,
        0,
        0,
        Fx::from_int(-1).0,
        Fx::from_int(50).0,
        Fx::ONE.0,
        Fx::ONE.0,
    );
    for row in 0..5i32 {
        for i in 0..(5 - row) {
            let x = Fx::from_ratio((2 * i + row) as i64 * 11, 10) - Fx::from_int(5);
            sim_add_box(
                h,
                2,
                x.0,
                Fx::from_ratio(2 * row as i64 + 1, 1).0,
                Fx::from_ratio(1, 2).0,
                Fx::from_ratio(1, 2).0,
                Fx::ONE.0,
            );
        }
    }
    for i in 0..4i32 {
        let id = sim_add_circle(
            h,
            2,
            Fx::from_int(10 + i * 3).0,
            Fx::from_int(8 + i).0,
            Fx::from_ratio(1, 2).0,
            Fx::ONE.0,
        );
        sim_set_material(h, id, Fx::from_ratio(3, 10).0, Fx::from_ratio(7, 10).0);
        sim_set_velocity(h, id, Fx::from_int(-i).0, 0, 0);
    }
    for _ in 0..240 {
        sim_step(h);
    }
    let hash = sim_hash(h);
    sim_world_free(h);
    hash
}

// ---- joints, contact events, shape casts -------------------------------------------------------

/// Distance/rope joint between local anchors; `min_len < len` makes it a rope (slack allowed).
#[no_mangle]
pub extern "C" fn sim_joint_distance(
    h: u32,
    a: u32,
    b: u32,
    ax: i64,
    ay: i64,
    bx: i64,
    by: i64,
    len: i64,
    min_len: i64,
) -> u32 {
    with(h, |s| {
        s.world
            .add_joint(Joint::new(
                BodyId(a),
                BodyId(b),
                FxVec2::new(Fx(ax), Fx(ay)),
                FxVec2::new(Fx(bx), Fx(by)),
                JointKind::Distance {
                    length: Fx(len),
                    min_length: Fx(min_len),
                },
            ))
            .0
    })
    .unwrap_or(0)
}

/// Revolute (pin) joint; `has_limits` != 0 enables the [lower, upper] angle range; the motor is
/// active when `max_torque` > 0.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn sim_joint_revolute(
    h: u32,
    a: u32,
    b: u32,
    ax: i64,
    ay: i64,
    bx: i64,
    by: i64,
    has_limits: u32,
    lower: i64,
    upper: i64,
    motor_speed: i64,
    max_torque: i64,
) -> u32 {
    with(h, |s| {
        s.world
            .add_joint(Joint::new(
                BodyId(a),
                BodyId(b),
                FxVec2::new(Fx(ax), Fx(ay)),
                FxVec2::new(Fx(bx), Fx(by)),
                JointKind::Revolute {
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
pub extern "C" fn sim_joint_weld(
    h: u32,
    a: u32,
    b: u32,
    ax: i64,
    ay: i64,
    bx: i64,
    by: i64,
    rel_angle: i64,
) -> u32 {
    with(h, |s| {
        s.world
            .add_joint(Joint::new(
                BodyId(a),
                BodyId(b),
                FxVec2::new(Fx(ax), Fx(ay)),
                FxVec2::new(Fx(bx), Fx(by)),
                JointKind::Weld {
                    rel_angle: Fx(rel_angle),
                },
            ))
            .0
    })
    .unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn sim_joint_remove(h: u32, id: u32) -> u32 {
    with(h, |s| s.world.remove_joint(JointId(id)).is_some() as u32).unwrap_or(0)
}

/// Retune a revolute motor at runtime (no-op for other joint kinds).
#[no_mangle]
pub extern "C" fn sim_joint_motor(h: u32, id: u32, speed: i64, max_torque: i64) {
    with(h, |s| {
        if let Some(j) = s.world.joint_mut(JointId(id)) {
            if let JointKind::Revolute {
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

/// Number of contact begin/end events produced by the last `sim_step`.
#[no_mangle]
pub extern "C" fn sim_events_len(h: u32) -> u32 {
    with(h, |s| s.world.events().len() as u32).unwrap_or(0)
}

/// Event field: 0 = body a, 1 = body b, 2 = begin (1) / end (0).
#[no_mangle]
pub extern "C" fn sim_event_get(h: u32, i: u32, field: u32) -> u32 {
    with(h, |s| {
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

/// Sweep a circle; result fields as `sim_raycast` via `sim_ray_get`.
#[no_mangle]
pub extern "C" fn sim_circlecast(
    h: u32,
    x: i64,
    y: i64,
    dx: i64,
    dy: i64,
    radius: i64,
    mask: u32,
) -> u32 {
    with(h, |s| {
        match s.world.circle_cast(
            FxVec2::new(Fx(x), Fx(y)),
            FxVec2::new(Fx(dx), Fx(dy)),
            Fx(radius),
            mask,
        ) {
            Some(hit) => {
                LAST_RAY.with(|r| {
                    *r.borrow_mut() = [
                        hit.fraction.0,
                        hit.point.x.0,
                        hit.point.y.0,
                        hit.normal.x.0,
                        hit.normal.y.0,
                    ];
                });
                hit.body.0 + 1
            }
            None => 0,
        }
    })
    .unwrap_or(0)
}

/// Joint reference scenario: pendulum on a distance joint, a slack rope, a motorised wheel on a
/// static axle and a welded pair falling onto them; 240 ticks. Equal on native and wasm32.
#[no_mangle]
pub extern "C" fn sim_joint_golden_hash() -> u64 {
    let one = Fx::ONE.0;
    let h = sim_world_new(0, Fx::from_int(-10).0, 60);
    let anchor = sim_add_circle(h, 0, 0, Fx::from_int(10).0, Fx::from_ratio(1, 10).0, one);
    let bob = sim_add_circle(h, 2, Fx::from_int(3).0, Fx::from_int(10).0, Fx::HALF.0, one);
    sim_joint_distance(
        h,
        anchor,
        bob,
        0,
        0,
        0,
        0,
        Fx::from_int(3).0,
        Fx::from_int(3).0,
    );
    let rope_end = sim_add_circle(h, 2, Fx::from_int(-2).0, Fx::from_int(8).0, Fx::HALF.0, one);
    sim_joint_distance(h, anchor, rope_end, 0, 0, 0, 0, Fx::from_int(5).0, 0);
    let axle = sim_add_circle(
        h,
        0,
        Fx::from_int(8).0,
        Fx::from_int(2).0,
        Fx::from_ratio(1, 10).0,
        one,
    );
    let wheel = sim_add_circle(
        h,
        2,
        Fx::from_int(8).0,
        Fx::from_int(2).0,
        Fx::from_int(1).0,
        one,
    );
    sim_joint_revolute(
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
        Fx::from_int(4).0,
        Fx::from_int(50).0,
    );
    let wa = sim_add_circle(h, 2, Fx::from_int(-8).0, Fx::from_int(6).0, Fx::HALF.0, one);
    let wb = sim_add_circle(h, 2, Fx::from_int(-7).0, Fx::from_int(6).0, Fx::HALF.0, one);
    sim_joint_weld(h, wa, wb, Fx::HALF.0, 0, -Fx::HALF.0, 0, 0);
    let mut events = 0u64;
    for _ in 0..240 {
        sim_step(h);
        events += sim_events_len(h) as u64;
    }
    let hash = sim_hash(h) ^ events.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    sim_world_free(h);
    hash
}

// ---- navigation -------------------------------------------------------------------------------

/// Grid navmesh: `cols × rows` cells of size `cell` from (ox, oy); blocked cells are given as
/// (col, row) pairs in the snapshot buffer (u32 LE pairs: fill with `sim_snapshot_set_*`).
#[no_mangle]
pub extern "C" fn sim_nav_grid(h: u32, ox: i64, oy: i64, cell: i64, cols: u32, rows: u32) -> u32 {
    with(h, |s| {
        let blocked: Vec<(u32, u32)> = s
            .snapshot
            .as_chunks::<8>()
            .0
            .iter()
            .map(|c| {
                (
                    u32::from_le_bytes([c[0], c[1], c[2], c[3]]),
                    u32::from_le_bytes([c[4], c[5], c[6], c[7]]),
                )
            })
            .collect();
        s.nav = Some(NavMesh::grid(
            FxVec2::new(Fx(ox), Fx(oy)),
            Fx(cell),
            cols,
            rows,
            &blocked,
        ));
        s.crowd.fields.clear();
        1
    })
    .unwrap_or(0)
}

/// Load a navmesh from its JSON bytes placed in the snapshot buffer (see `NavMesh::to_bytes`).
#[no_mangle]
pub extern "C" fn sim_nav_load(h: u32) -> u32 {
    with(h, |s| match NavMesh::from_bytes(&s.snapshot) {
        Some(m) => {
            s.nav = Some(m);
            s.crowd.fields.clear();
            1
        }
        None => 0,
    })
    .unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn sim_nav_hash(h: u32) -> u64 {
    with(h, |s| s.nav.as_ref().map(|m| m.hash()).unwrap_or(0)).unwrap_or(0)
}

/// Polygon containing (x, y), or u32::MAX.
#[no_mangle]
pub extern "C" fn sim_nav_locate(h: u32, x: i64, y: i64) -> u32 {
    with(h, |s| {
        s.nav
            .as_ref()
            .and_then(|m| m.locate(FxVec2::new(Fx(x), Fx(y))))
            .unwrap_or(u32::MAX)
    })
    .unwrap_or(u32::MAX)
}

/// Find a path; returns the number of waypoints stored for `sim_nav_point` (0 = no path).
#[no_mangle]
pub extern "C" fn sim_nav_path(h: u32, x0: i64, y0: i64, x1: i64, y1: i64, mask: u32) -> u32 {
    with(h, |s| {
        s.points.clear();
        let Some(m) = s.nav.as_ref() else { return 0 };
        match m.find_path(
            FxVec2::new(Fx(x0), Fx(y0)),
            FxVec2::new(Fx(x1), Fx(y1)),
            mask,
        ) {
            Some(path) => {
                for p in &path.points {
                    s.points.push(p.x.0);
                    s.points.push(p.y.0);
                }
                path.points.len() as u32
            }
            None => 0,
        }
    })
    .unwrap_or(0)
}

/// Coordinate `i` (0 = x0, 1 = y0, 2 = x1, ...) of the last path.
#[no_mangle]
pub extern "C" fn sim_nav_point(h: u32, i: u32) -> i64 {
    with(h, |s| s.points.get(i as usize).copied().unwrap_or(0)).unwrap_or(0)
}

/// Mesh raycast; returns 1 and stores the hit point in `sim_ray_get(1..=2)` when blocked.
#[no_mangle]
pub extern "C" fn sim_nav_raycast(h: u32, x0: i64, y0: i64, x1: i64, y1: i64) -> u32 {
    with(h, |s| {
        let Some(m) = s.nav.as_ref() else { return 0 };
        match m.raycast(FxVec2::new(Fx(x0), Fx(y0)), FxVec2::new(Fx(x1), Fx(y1))) {
            Some(hit) => {
                LAST_RAY.with(|r| {
                    *r.borrow_mut() = [
                        0,
                        hit.point.x.0,
                        hit.point.y.0,
                        hit.poly as i64,
                        hit.edge as i64,
                    ]
                });
                1
            }
            None => 0,
        }
    })
    .unwrap_or(0)
}

/// Build a flow field towards (x, y); returns its index (u32::MAX when unreachable).
#[no_mangle]
pub extern "C" fn sim_nav_field(h: u32, x: i64, y: i64, mask: u32) -> u32 {
    with(h, |s| {
        let Some(m) = s.nav.as_ref() else {
            return u32::MAX;
        };
        match FlowField::build(m, FxVec2::new(Fx(x), Fx(y)), mask) {
            Some(f) => s.crowd.add_field(f),
            None => u32::MAX,
        }
    })
    .unwrap_or(u32::MAX)
}

/// Add a crowd agent; returns its id (0 = no navmesh).
#[no_mangle]
pub extern "C" fn sim_agent_add(h: u32, x: i64, y: i64, radius: i64, max_speed: i64) -> u32 {
    with(h, |s| {
        let Some(m) = s.nav.as_ref() else { return 0 };
        s.crowd.add(
            m,
            Agent::new(FxVec2::new(Fx(x), Fx(y)), Fx(radius), Fx(max_speed)),
        )
    })
    .unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn sim_agent_remove(h: u32, id: u32) {
    with(h, |s| s.crowd.remove(id));
}

/// Steer towards a point (clears any flow field).
#[no_mangle]
pub extern "C" fn sim_agent_target(h: u32, id: u32, x: i64, y: i64) {
    with(h, |s| {
        if let Some(a) = s.crowd.agents.get_mut(&id) {
            a.target = Some(FxVec2::new(Fx(x), Fx(y)));
            a.field = None;
            a.arrived = false;
        }
    });
}

/// Follow a flow field (clears any point target).
#[no_mangle]
pub extern "C" fn sim_agent_follow(h: u32, id: u32, field: u32) {
    with(h, |s| {
        if let Some(a) = s.crowd.agents.get_mut(&id) {
            a.field = Some(field);
            a.target = None;
            a.arrived = false;
        }
    });
}

#[no_mangle]
pub extern "C" fn sim_agent_stop(h: u32, id: u32) {
    with(h, |s| {
        if let Some(a) = s.crowd.agents.get_mut(&id) {
            a.field = None;
            a.target = None;
            a.vel = FxVec2::ZERO;
        }
    });
}

/// Agent field: 0 = x, 1 = y, 2 = vx, 3 = vy, 4 = arrived (0/1), 5 = polygon.
#[no_mangle]
pub extern "C" fn sim_agent_get(h: u32, id: u32, field: u32) -> i64 {
    with(h, |s| {
        s.crowd
            .agents
            .get(&id)
            .map(|a| match field {
                0 => a.pos.x.0,
                1 => a.pos.y.0,
                2 => a.vel.x.0,
                3 => a.vel.y.0,
                4 => a.arrived as i64,
                _ => a.poly as i64,
            })
            .unwrap_or(0)
    })
    .unwrap_or(0)
}

/// Advance the crowd one tick (call alongside `sim_step`).
#[no_mangle]
pub extern "C" fn sim_crowd_step(h: u32) {
    with(h, |s| {
        if let Some(m) = s.nav.as_ref() {
            s.crowd.step(m);
        }
    });
}

#[no_mangle]
pub extern "C" fn sim_crowd_hash(h: u32) -> u64 {
    with(h, |s| s.crowd.hash()).unwrap_or(0)
}

/// Snapshot the crowd into the snapshot buffer; returns its length.
#[no_mangle]
pub extern "C" fn sim_crowd_snapshot(h: u32) -> u32 {
    with(h, |s| {
        s.snapshot = s.crowd.snapshot();
        s.snapshot.len() as u32
    })
    .unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn sim_crowd_restore(h: u32) -> u32 {
    with(h, |s| match Crowd::restore(&s.snapshot) {
        Some(c) => {
            s.crowd = c;
            1
        }
        None => 0,
    })
    .unwrap_or(0)
}

/// Navigation reference scenario: 8×6 grid with a wall, 6 agents on a flow field and two
/// head-on agents, 600 ticks; equal on native and wasm32.
#[no_mangle]
pub extern "C" fn sim_nav_golden_hash() -> u64 {
    let h = sim_world_new(0, 0, 60);
    sim_snapshot_set_len(h, 40);
    for r in 0..5u32 {
        for (k, b) in 4u32
            .to_le_bytes()
            .iter()
            .chain(r.to_le_bytes().iter())
            .enumerate()
        {
            sim_snapshot_set_byte(h, r * 8 + k as u32, *b as u32);
        }
    }
    sim_nav_grid(h, 0, 0, Fx::ONE.0, 8, 6);
    let goal = (Fx::from_ratio(15, 2).0, Fx::HALF.0);
    let field = sim_nav_field(h, goal.0, goal.1, u32::MAX);
    for i in 0..6i64 {
        let id = sim_agent_add(
            h,
            (Fx::HALF + Fx::from_ratio(i % 3, 2)).0,
            (Fx::HALF + Fx::from_ratio(i / 3, 1)).0,
            Fx::from_ratio(1, 4).0,
            Fx::from_int(3).0,
        );
        sim_agent_follow(h, id, field);
    }
    let l = sim_agent_add(
        h,
        Fx::from_ratio(11, 2).0,
        Fx::HALF.0,
        Fx::from_ratio(1, 4).0,
        Fx::from_int(2).0,
    );
    sim_agent_target(h, l, Fx::from_ratio(15, 2).0, Fx::from_ratio(11, 2).0);
    let r = sim_agent_add(
        h,
        Fx::from_ratio(15, 2).0,
        Fx::from_ratio(11, 2).0,
        Fx::from_ratio(1, 4).0,
        Fx::from_int(2).0,
    );
    sim_agent_target(h, r, Fx::from_ratio(11, 2).0, Fx::HALF.0);
    for _ in 0..600 {
        sim_crowd_step(h);
    }
    let hash = sim_crowd_hash(h) ^ sim_nav_hash(h);
    sim_world_free(h);
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn golden_hash_is_stable_across_runs() {
        let a = sim_golden_hash();
        let b = sim_golden_hash();
        assert_eq!(a, b);
        eprintln!("native golden hash: {a:#x}");
    }

    #[test]
    fn joint_golden_hash_is_stable_and_circlecast_works() {
        let a = sim_joint_golden_hash();
        assert_eq!(a, sim_joint_golden_hash());
        eprintln!("native joint golden hash: {a:#x}");
        let h = sim_world_new(0, 0, 60);
        let wall = sim_add_circle(h, 0, Fx::from_int(10).0, 0, Fx::ONE.0, Fx::ONE.0);
        assert_eq!(
            sim_circlecast(h, 0, 0, Fx::from_int(20).0, 0, Fx::HALF.0, u32::MAX),
            wall + 1
        );
        assert!((Fx(sim_ray_get(1)) - Fx::from_ratio(85, 10)).abs() < Fx::from_ratio(1, 1000));
        sim_world_free(h);
    }

    #[test]
    fn nav_golden_hash_is_stable_and_paths_work() {
        let a = sim_nav_golden_hash();
        assert_eq!(a, sim_nav_golden_hash());
        eprintln!("native nav golden hash: {a:#x}");
        let h = sim_world_new(0, 0, 60);
        sim_snapshot_set_len(h, 0);
        sim_nav_grid(h, 0, 0, Fx::ONE.0, 4, 4);
        let n = sim_nav_path(
            h,
            Fx::HALF.0,
            Fx::HALF.0,
            Fx::from_ratio(7, 2).0,
            Fx::from_ratio(7, 2).0,
            u32::MAX,
        );
        assert!((2..=4).contains(&n), "open grid path has {n} waypoints");
        assert_eq!(Fx(sim_nav_point(h, 2 * (n - 1))), Fx::from_ratio(7, 2));
        assert_eq!(Fx(sim_nav_point(h, 2 * (n - 1) + 1)), Fx::from_ratio(7, 2));
        assert_eq!(
            sim_nav_raycast(h, Fx::HALF.0, Fx::HALF.0, Fx::from_int(9).0, Fx::HALF.0),
            1
        );
        assert!((Fx(sim_ray_get(1)) - Fx::from_int(4)).abs() < Fx::from_ratio(1, 1000));
        sim_world_free(h);
    }

    #[test]
    fn flat_abi_round_trip() {
        let h = sim_world_new(0, Fx::from_int(-10).0, 60);
        sim_add_box(
            h,
            0,
            0,
            Fx::from_int(-1).0,
            Fx::from_int(50).0,
            Fx::ONE.0,
            Fx::ONE.0,
        );
        let ball = sim_add_circle(h, 2, 0, Fx::from_int(5).0, Fx::ONE.0, Fx::ONE.0);
        for _ in 0..120 {
            sim_step(h);
        }
        let y = Fx(sim_body_get(h, ball, 1)).to_f64();
        assert!((0.9..1.1).contains(&y), "ball at {y}");
        let len = sim_snapshot(h);
        let bytes: Vec<u8> = (0..len).map(|i| sim_snapshot_byte(h, i) as u8).collect();
        let before = sim_hash(h);
        sim_step(h);
        sim_snapshot_set_len(h, len);
        for (i, b) in bytes.iter().enumerate() {
            sim_snapshot_set_byte(h, i as u32, *b as u32);
        }
        assert_eq!(sim_restore(h), 1);
        assert_eq!(sim_hash(h), before);
        assert_ne!(
            sim_raycast(h, 0, Fx::from_int(5).0, 0, Fx::from_int(-10).0, u32::MAX),
            0
        );
        sim_world_free(h);
    }
}
