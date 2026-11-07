//! Flat ABI over `net-sim-physics`: ids and Q32.32 raw `i64` values only, so the same `.wasm`
//! runs in browsers, Unity (WebGL/IL2CPP via a loader) and on the node, and a C ABI build serves
//! C++/Godot. Worlds live in a slot table; every function takes a world handle.
#![deny(clippy::float_arithmetic)]

use net_fpmath::{Fx, FxVec2};
use net_sim_physics::{Body, BodyId, BodyKind, CharacterController, Shape, World, WorldConfig};
use std::cell::RefCell;

struct Slot {
    world: World,
    kcc: Vec<CharacterController>,
    snapshot: Vec<u8>,
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
