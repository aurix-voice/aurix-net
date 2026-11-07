//! `physics-arena`: a room-logic module (see `net-game` for the host ABI) whose avatars are
//! deterministic fixed-point circles in a walled `net-sim-physics` world. Input = i8 move_x,
//! i8 move_z, u8 buttons; the module sets the avatar's velocity, steps the world and writes
//! `Transform.pos` (x, 0, z) for replication, so the node's frame hash — the lockstep referee's
//! truth — is driven by the same physics clients run through `net-sim-wasm`.
//!
//! Build: `cargo build --release --target wasm32-unknown-unknown -p net-sim-guest`, upload with
//! `aurix-net module-put physics-arena target/wasm32-unknown-unknown/release/net_sim_guest.wasm`,
//! room props: `{"schema": "component Transform {\n pos: vec3 @range(-4096, 4096, 0.01)\n speed: f32\n}",
//! "logic": "physics-arena", "position": "Transform.0"}`.
//! Guest memory is not checkpointed across failover; the world is rebuilt from the frame's
//! `Transform.pos` on `aurix_init` with zero velocity (a documented approximation).
#![allow(clippy::missing_safety_doc)]

use net_fpmath::{Fx, FxVec2};
use net_sim_physics::{Body, BodyId, BodyKind, Shape, World, WorldConfig};
use std::cell::RefCell;

#[cfg(target_arch = "wasm32")]
mod host {
    #[link(wasm_import_module = "aurix")]
    extern "C" {
        pub fn spawn(owner: i32) -> i64;
        pub fn despawn(id: i64) -> i32;
        pub fn component_id(ptr: *const u8, len: i32) -> i32;
        pub fn set_vec3(id: i64, comp: i32, field: i32, x: f32, y: f32, z: f32) -> i32;
        pub fn set_f32(id: i64, comp: i32, field: i32, v: f32) -> i32;
        pub fn input_count() -> i32;
        pub fn input_entity(i: i32) -> i64;
        pub fn input_read(i: i32, ptr: *mut u8, cap: i32) -> i32;
        pub fn hitbox(id: i64, cx: f32, cy: f32, cz: f32, hx: f32, hy: f32, hz: f32);
    }
}

/// Native stand-ins so the crate type-checks, lints and unit-tests on the host; only the wasm32
/// build is ever instantiated by a node.
#[cfg(not(target_arch = "wasm32"))]
mod host {
    pub unsafe fn spawn(_owner: i32) -> i64 {
        1
    }
    pub unsafe fn despawn(_id: i64) -> i32 {
        1
    }
    pub unsafe fn component_id(_ptr: *const u8, _len: i32) -> i32 {
        0
    }
    pub unsafe fn set_vec3(_id: i64, _c: i32, _f: i32, _x: f32, _y: f32, _z: f32) -> i32 {
        1
    }
    pub unsafe fn set_f32(_id: i64, _c: i32, _f: i32, _v: f32) -> i32 {
        1
    }
    pub unsafe fn input_count() -> i32 {
        0
    }
    pub unsafe fn input_entity(_i: i32) -> i64 {
        0
    }
    pub unsafe fn input_read(_i: i32, _ptr: *mut u8, _cap: i32) -> i32 {
        0
    }
    pub unsafe fn hitbox(_id: i64, _cx: f32, _cy: f32, _cz: f32, _hx: f32, _hy: f32, _hz: f32) {}
}

pub const ARENA_HALF: i32 = 40;
pub const RADIUS_CENTI: i64 = 50;
pub const SPEED: i32 = 6;
pub const TICK_HZ: u32 = 30;

pub struct Arena {
    pub world: World,
    pub avatars: Vec<(i64, i32, BodyId)>,
    transform: i32,
}

impl Arena {
    pub fn new() -> Self {
        let mut world = World::new(WorldConfig {
            gravity: FxVec2::ZERO,
            dt: Fx::from_ratio(1, TICK_HZ as i64),
            ..WorldConfig::default()
        });
        let h = Fx::from_int(ARENA_HALF);
        let t = Fx::ONE;
        for (x, y, hw, hh) in [
            (Fx::ZERO, h + t, h + t + t, t),
            (Fx::ZERO, -(h + t), h + t + t, t),
            (h + t, Fx::ZERO, t, h),
            (-(h + t), Fx::ZERO, t, h),
        ] {
            world.add(Body::new(
                BodyKind::Static,
                Shape::rect(hw, hh),
                FxVec2::new(x, y),
            ));
        }
        Self {
            world,
            avatars: Vec::new(),
            transform: -1,
        }
    }

    /// Spawn position on the same grid the built-in arena uses.
    pub fn spawn_at(actor: i32) -> FxVec2 {
        let a = actor.rem_euclid(1 << 16);
        FxVec2::new(
            Fx::from_int((a % 16) * 4 - 32),
            Fx::from_int(((a / 16) % 16) * 4 - 32),
        )
    }

    pub fn join(&mut self, entity: i64, actor: i32) -> BodyId {
        let mut b = Body::new(
            BodyKind::Dynamic,
            Shape::Circle {
                radius: Fx::from_ratio(RADIUS_CENTI, 100),
            },
            Self::spawn_at(actor),
        )
        .with_density(Fx::ONE);
        b.linear_damping = Fx::from_ratio(1, 10);
        let id = self.world.add(b);
        self.avatars.push((entity, actor, id));
        id
    }

    pub fn leave(&mut self, entity: i64) {
        if let Some(i) = self.avatars.iter().position(|a| a.0 == entity) {
            let (_, _, id) = self.avatars.remove(i);
            self.world.remove(id);
        }
    }

    /// `(move_x, move_z)` in -127..=127 → velocity `SPEED * v / 127`.
    pub fn steer(&mut self, entity: i64, mx: i8, mz: i8) {
        if let Some(&(_, _, id)) = self.avatars.iter().find(|a| a.0 == entity) {
            let v = FxVec2::new(
                Fx::from_ratio(SPEED as i64 * mx as i64, 127),
                Fx::from_ratio(SPEED as i64 * mz as i64, 127),
            );
            if let Some(b) = self.world.body_mut(id) {
                b.velocity = v;
            }
        }
    }

    pub fn step(&mut self) {
        self.world.step();
    }

    pub fn position(&self, id: BodyId) -> FxVec2 {
        self.world
            .body(id)
            .map(|b| b.position)
            .unwrap_or(FxVec2::ZERO)
    }
}

impl Default for Arena {
    fn default() -> Self {
        Self::new()
    }
}

thread_local! {
    static ARENA: RefCell<Option<Arena>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Arena) -> R) -> R {
    ARENA.with(|a| {
        let mut a = a.borrow_mut();
        f(a.get_or_insert_with(Arena::new))
    })
}

fn f32_of(v: Fx) -> f32 {
    // exact for the arena's range: integer part + 32-bit fraction, rounded once
    (v.0 as f64 / 4294967296.0) as f32
}

#[no_mangle]
pub extern "C" fn aurix_init(_seed: i64) {
    with(|a| {
        *a = Arena::new();
        a.transform = unsafe { host::component_id(b"Transform".as_ptr(), 9) };
    });
}

#[no_mangle]
pub extern "C" fn aurix_join(actor: i32) -> i64 {
    with(|a| {
        let e = unsafe { host::spawn(actor) };
        if e == 0 {
            return 0;
        }
        let id = a.join(e, actor);
        let p = a.position(id);
        unsafe {
            host::set_vec3(e, a.transform, 0, f32_of(p.x), 0.0, f32_of(p.y));
            host::set_f32(e, a.transform, 1, SPEED as f32);
        }
        e
    })
}

#[no_mangle]
pub extern "C" fn aurix_leave(_actor: i32, entity: i64) {
    with(|a| {
        a.leave(entity);
        unsafe { host::despawn(entity) };
    });
}

#[no_mangle]
pub extern "C" fn aurix_step() {
    with(|a| {
        let n = unsafe { host::input_count() };
        for i in 0..n {
            let e = unsafe { host::input_entity(i) };
            if e == 0 {
                continue;
            }
            let mut buf = [0u8; 3];
            let got = unsafe { host::input_read(i, buf.as_mut_ptr(), 3) };
            if got >= 2 {
                a.steer(e, buf[0] as i8, buf[1] as i8);
            }
        }
        a.step();
        let r = RADIUS_CENTI as f32 / 100.0;
        for &(e, _, id) in &a.avatars {
            let p = a.position(id);
            let (x, z) = (f32_of(p.x), f32_of(p.y));
            unsafe {
                host::set_vec3(e, a.transform, 0, x, 0.0, z);
                host::hitbox(e, x, 1.0, z, r, 1.0, r);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_arenas_agree_and_walls_hold() {
        let run = || {
            let mut a = Arena::new();
            let e1 = a.join(1, 0);
            let _e2 = a.join(2, 1);
            for t in 0..600 {
                a.steer(1, 127, 0);
                a.steer(2, if t % 2 == 0 { -127 } else { 127 }, 64);
                a.step();
            }
            (a.position(e1), a.world.hash())
        };
        let (p, h) = run();
        assert_eq!(h, run().1);
        assert!(
            p.x < Fx::from_int(ARENA_HALF) && p.x > Fx::from_int(30),
            "{p:?}"
        );
    }
}
