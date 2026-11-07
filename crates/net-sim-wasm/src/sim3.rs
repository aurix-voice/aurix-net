//! Flat ABI over `net-sim-physics3d` (`sim3_*`): raw Q32.32 `i64`s, u32 handles/ids, its own
//! slot table so 2D and 3D worlds can coexist in one module.

use net_fpmath::{Fx, FxQuat, FxVec3};
use net_sim_physics3d::{Body, BodyId, BodyKind, Shape, World, WorldConfig};
use std::cell::RefCell;

struct Slot3 {
    world: World,
    snapshot: Vec<u8>,
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

#[cfg(test)]
mod tests {
    use super::*;

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
