//! Sync mode: the server runs the simulation at a fixed tick, clients send inputs, the
//! server replicates state deltas with interest management.
//!
//! Data frames on the unreliable channel (first byte = kind):
//! ```text
//! 0x10 Input    client->server  u32 first_tick, u8 n, n x (u8 len, bytes)   (redundant ticks)
//! 0x11 Delta    server->client  replication delta (see net-replication)
//! 0x12 Resync   client->server  drop baselines, next delta is a full snapshot
//! ```

use bytes::{Buf, BufMut, Bytes, BytesMut};
use net_common::types::{ActorId, Tick};
use net_lagcomp::{History, Hitbox};
use net_replication::{ClientView, ReplicationConfig, Replicator};
use net_sim::{EntityId, Frame, FrameConfig, Input};
use net_wire::{BitReader, ComponentValue, Schema, Value};
use std::collections::HashMap;

pub const KIND_INPUT: u8 = 0x10;
pub const KIND_DELTA: u8 = 0x11;
pub const KIND_RESYNC: u8 = 0x12;

/// Redundant input ticks per packet (client resends the last K).
pub const INPUT_REDUNDANCY: usize = 3;

/// Built-in schema used when a room gives none: a moving avatar per actor.
pub const ARENA_SCHEMA: &str = "\
component Transform {\n    pos: vec3 @range(-512, 512, 0.01)\n    vel: vec3 @range(-64, 64, 0.01)\n}\n\
component Avatar {\n    actor: u16\n    hp: u8\n}\n";

/// Server-side game rules. The built-in ones below cover the two common cases; WASM-hosted
/// game logic plugs in here later.
pub trait GameLogic: Send {
    fn on_join(&mut self, frame: &mut Frame, actor: ActorId) -> Option<EntityId>;
    fn on_leave(&mut self, frame: &mut Frame, actor: ActorId, avatar: Option<EntityId>);
    /// `inputs`: (actor, avatar, input bytes, fresh) for every connected actor.
    fn step(&mut self, frame: &mut Frame, inputs: &[(ActorId, Option<EntityId>, Vec<u8>, bool)]);
    fn hitboxes(&self, frame: &Frame) -> Vec<(EntityId, Hitbox)>;
}

/// Arena: input = i8 move_x, i8 move_z, u8 buttons; server integrates velocity.
pub struct ArenaLogic {
    pub speed: f32,
}

impl GameLogic for ArenaLogic {
    fn on_join(&mut self, frame: &mut Frame, actor: ActorId) -> Option<EntityId> {
        let e = frame.spawn(Some(actor)).ok()?;
        let x = (actor.0 % 16) as f32 * 4.0 - 32.0;
        let z = (actor.0 / 16) as f32 * 4.0 - 32.0;
        frame
            .set(
                e,
                "Transform",
                ComponentValue {
                    fields: vec![Value::Vec3([x, 0.0, z]), Value::Vec3([0.0; 3])],
                },
            )
            .ok()?;
        frame
            .set(
                e,
                "Avatar",
                ComponentValue {
                    fields: vec![Value::UInt(actor.0 as u64), Value::UInt(100)],
                },
            )
            .ok()?;
        Some(e)
    }

    fn on_leave(&mut self, frame: &mut Frame, _actor: ActorId, avatar: Option<EntityId>) {
        if let Some(e) = avatar {
            let _ = frame.despawn(e);
        }
    }

    fn step(&mut self, frame: &mut Frame, inputs: &[(ActorId, Option<EntityId>, Vec<u8>, bool)]) {
        let dt = frame.dt();
        for (_, avatar, data, _) in inputs {
            let Some(e) = avatar else { continue };
            let (mx, mz) = if data.len() >= 2 {
                (data[0] as i8 as f32 / 127.0, data[1] as i8 as f32 / 127.0)
            } else {
                (0.0, 0.0)
            };
            if let Some(t) = frame.get_mut(*e, "Transform") {
                let vel = [mx * self.speed, 0.0, mz * self.speed];
                if let Value::Vec3(p) = &mut t.fields[0] {
                    p[0] = (p[0] + vel[0] * dt).clamp(-512.0, 512.0);
                    p[2] = (p[2] + vel[2] * dt).clamp(-512.0, 512.0);
                }
                t.fields[1] = Value::Vec3(vel);
            }
        }
    }

    fn hitboxes(&self, frame: &Frame) -> Vec<(EntityId, Hitbox)> {
        frame
            .entities()
            .filter_map(|e| {
                let p = net_sim::vec3_of(e.components.get(&0)?, 0)?;
                Some((
                    e.id,
                    Hitbox {
                        center: [p[0], p[1] + 1.0, p[2]],
                        half: [0.5, 1.0, 0.5],
                    },
                ))
            })
            .collect()
    }
}

/// Owner-authoritative: every actor gets one entity; its input is a list of full component
/// writes (`u16 cid, encode_full`) which the server applies verbatim and replicates. For
/// games that bring their own schema but keep simulation on the clients.
pub struct OwnerLogic;

impl GameLogic for OwnerLogic {
    fn on_join(&mut self, frame: &mut Frame, actor: ActorId) -> Option<EntityId> {
        frame.spawn(Some(actor)).ok()
    }
    fn on_leave(&mut self, frame: &mut Frame, _actor: ActorId, avatar: Option<EntityId>) {
        if let Some(e) = avatar {
            let _ = frame.despawn(e);
        }
    }
    fn step(&mut self, frame: &mut Frame, inputs: &[(ActorId, Option<EntityId>, Vec<u8>, bool)]) {
        let cid_bits = frame.schema.component_id_bits();
        for (_, avatar, data, fresh) in inputs {
            let (Some(e), true) = (avatar, *fresh) else {
                continue;
            };
            let mut r = BitReader::new(data);
            let Ok(n) = r.read_bits(8) else { continue };
            for _ in 0..n {
                let Ok(cid) = r.read_bits(cid_bits) else {
                    break;
                };
                let Some(c) = frame.schema.by_id(cid as u16).cloned() else {
                    break;
                };
                let Ok(v) = ComponentValue::decode_full(&c, &mut r) else {
                    break;
                };
                if let Some(ent) = frame.entity_mut(*e) {
                    ent.components.insert(c.id, v);
                }
            }
        }
    }
    fn hitboxes(&self, _frame: &Frame) -> Vec<(EntityId, Hitbox)> {
        Vec::new()
    }
}

pub struct SyncRoom {
    pub frame: Frame,
    pub rep: Replicator,
    pub history: History,
    pub logic: Box<dyn GameLogic>,
    pub views: HashMap<ActorId, ClientView>,
    pub avatars: HashMap<ActorId, Option<EntityId>>,
    pub hz: u16,
    pub schema_hash: String,
}

impl SyncRoom {
    /// `schema`: IDL text from room props, or None for the built-in arena.
    pub fn new(schema: Option<&str>, hz: u16, seed: u64) -> Result<Self, String> {
        let (schema, logic): (Schema, Box<dyn GameLogic>) = match schema {
            Some(src) => (
                Schema::parse(src).map_err(|e| e.to_string())?,
                Box::new(OwnerLogic),
            ),
            None => (
                Schema::parse(ARENA_SCHEMA).unwrap(),
                Box::new(ArenaLogic { speed: 6.0 }),
            ),
        };
        let hz = hz.clamp(8, 128);
        let rep = Replicator::new(ReplicationConfig::default(), &schema);
        let frame = Frame::new(
            schema.clone(),
            FrameConfig {
                tick_hz: hz,
                ..Default::default()
            },
            seed,
        );
        Ok(Self {
            schema_hash: schema.hash.clone(),
            frame,
            rep,
            history: History::new(hz as usize * 2),
            logic,
            views: HashMap::new(),
            avatars: HashMap::new(),
            hz,
        })
    }

    pub fn tick_period(&self) -> std::time::Duration {
        std::time::Duration::from_micros(1_000_000 / self.hz as u64)
    }

    pub fn on_join(&mut self, actor: ActorId) {
        if self.avatars.contains_key(&actor) {
            return;
        }
        let avatar = self.logic.on_join(&mut self.frame, actor);
        self.avatars.insert(actor, avatar);
        let mut v = ClientView::new(actor);
        v.viewpoint = avatar;
        self.views.insert(actor, v);
    }

    pub fn on_leave(&mut self, actor: ActorId) {
        if let Some(avatar) = self.avatars.remove(&actor) {
            self.logic.on_leave(&mut self.frame, actor, avatar);
        }
        self.views.remove(&actor);
        self.frame.forget_actor(actor);
    }

    /// Client data frame. Returns true when it was a sync frame (handled here).
    pub fn handle(&mut self, actor: ActorId, mut payload: Bytes) -> bool {
        let Some(&kind) = payload.first() else {
            return false;
        };
        match kind {
            KIND_INPUT => {
                payload.advance(1);
                if payload.len() < 5 {
                    return true;
                }
                let first = payload.get_u32();
                let n = payload.get_u8() as usize;
                for i in 0..n.min(INPUT_REDUNDANCY + 1) {
                    if payload.is_empty() {
                        break;
                    }
                    let len = payload.get_u8() as usize;
                    if payload.len() < len {
                        break;
                    }
                    let data = payload.split_to(len).to_vec();
                    self.frame.push_input(Input {
                        actor,
                        tick: Tick(first.wrapping_add(i as u32)),
                        data,
                    });
                }
                true
            }
            KIND_RESYNC => {
                if let Some(v) = self.views.get_mut(&actor) {
                    v.reset();
                }
                true
            }
            _ => false,
        }
    }

    /// One simulation step. Returns the delta frame for each actor with a view.
    pub fn tick(&mut self) -> Vec<(ActorId, Bytes)> {
        let actors: Vec<ActorId> = self.avatars.keys().copied().collect();
        let mut inputs = Vec::with_capacity(actors.len());
        for a in &actors {
            let (data, fresh) = self.frame.input_for(*a);
            inputs.push((*a, self.avatars[a], data, fresh));
        }
        let logic = &mut self.logic;
        self.frame.advance(|f| logic.step(f, &inputs));
        let boxes = self.logic.hitboxes(&self.frame);
        self.history.record(self.frame.tick, boxes);
        let mut out = Vec::with_capacity(self.views.len());
        for (a, v) in self.views.iter_mut() {
            let d = self.rep.build(&self.frame, v);
            let mut b = BytesMut::with_capacity(d.len() + 1);
            b.put_u8(KIND_DELTA);
            b.extend_from_slice(&d);
            out.push((*a, b.freeze()));
        }
        out
    }

    /// Encodes a client input packet (used by SDK/loadtest): the last K inputs ending at `tick`.
    pub fn encode_input(first_tick: Tick, inputs: &[Vec<u8>]) -> Bytes {
        let mut b = BytesMut::new();
        b.put_u8(KIND_INPUT);
        b.put_u32(first_tick.0);
        b.put_u8(inputs.len().min(255) as u8);
        for i in inputs.iter().take(255) {
            b.put_u8(i.len().min(255) as u8);
            b.extend_from_slice(&i[..i.len().min(255)]);
        }
        b.freeze()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arena_moves_avatars_and_replicates() {
        let mut room = SyncRoom::new(None, 60, 7).unwrap();
        let (a, b) = (ActorId(1), ActorId(2));
        room.on_join(a);
        room.on_join(b);
        let mut client_a = Frame::new(room.frame.schema.clone(), FrameConfig::default(), 0);
        // actor a pushes right for 60 ticks, b idle
        for t in 0..60u32 {
            let inp = SyncRoom::encode_input(Tick(t), &[vec![127, 0, 0]]);
            assert!(room.handle(a, inp));
            let out = room.tick();
            assert_eq!(out.len(), 2);
            for (actor, d) in out {
                if actor == a {
                    assert_eq!(d[0], KIND_DELTA);
                    Replicator::apply(&mut client_a, &d[1..]).unwrap();
                }
            }
        }
        let ea = room.avatars[&a].unwrap();
        let p = net_sim::vec3_of(room.frame.get(ea, "Transform").unwrap(), 0).unwrap();
        assert!((p[0] - (-28.0 + 6.0)).abs() < 0.05, "{p:?}");
        let pc = net_sim::vec3_of(client_a.get(ea, "Transform").unwrap(), 0).unwrap();
        assert!((pc[0] - p[0]).abs() < 0.02);
        assert_eq!(client_a.entity_count(), 2);
        assert_eq!(client_a.tick, room.frame.tick);
        // leave removes the avatar on the next delta
        room.on_leave(b);
        for (actor, d) in room.tick() {
            if actor == a {
                Replicator::apply(&mut client_a, &d[1..]).unwrap();
            }
        }
        assert_eq!(client_a.entity_count(), 1);
        // resync -> full snapshot again
        room.handle(a, Bytes::from_static(&[KIND_RESYNC]));
        let out = room.tick();
        let full = &out.iter().find(|(x, _)| *x == a).unwrap().1;
        assert!(full.len() > 12);
        // lag comp sees the avatar where it was
        let hit = room
            .history
            .raycast_at(
                Tick(30),
                [p[0] - 20.0, 1.0, p[2]],
                [1.0, 0.0, 0.0],
                100.0,
                None,
            )
            .unwrap();
        assert_eq!(hit.entity, ea);
        assert!(hit.distance < 20.0);
    }

    #[test]
    fn owner_logic_applies_component_writes() {
        let src = "component Pose {\n x: f32 @range(-100, 100, 0.1)\n}";
        let mut room = SyncRoom::new(Some(src), 30, 1).unwrap();
        let a = ActorId(5);
        room.on_join(a);
        let ea = room.avatars[&a].unwrap();
        let c = room.frame.schema.component("Pose").unwrap().clone();
        let mut w = net_wire::BitWriter::new();
        w.write_bits(1, 8);
        w.write_bits(c.id as u64, room.frame.schema.component_id_bits());
        ComponentValue {
            fields: vec![Value::F32(42.0)],
        }
        .encode_full(&c, &mut w)
        .unwrap();
        room.handle(a, SyncRoom::encode_input(Tick(0), &[w.finish()]));
        room.tick();
        assert_eq!(
            room.frame.get(ea, "Pose"),
            Some(&ComponentValue {
                fields: vec![Value::F32(42.0)]
            })
        );
        assert!(SyncRoom::new(Some("component {"), 30, 1).is_err());
    }
}
