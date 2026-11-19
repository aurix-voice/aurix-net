//! Sync mode: the server runs the simulation at a fixed tick, clients send inputs, the
//! server replicates state deltas with interest management.
//!
//! Data frames on the unreliable channel (first byte = kind):
//! ```text
//! 0x10 Input    client->server  u32 first_tick, u8 n, n x (u8 len, bytes)   (redundant ticks)
//! 0x11 Delta    server->client  replication delta (see net-replication)
//! 0x12 Resync   client->server  drop baselines, next delta is a full snapshot
//! 0x13 Ack      client->server  u32 last delta tick, u32 history bits (see `AckTracker`)
//! 0x14 Spawned  server->client  u32 tag, u8 status (0 created, 1 refused, 2 invalid op),
//!                               u64 entity, u32 tick
//!                               reliable, to the actor alone: its avatar (tag 0) and the
//!                               answer to each `OP_SPAWN_TAGGED`
//! 0x25 InputStats server->client u32 tick, i8 min_margin, u8 late   (same as lockstep) every 30
//!                               ticks per actor: how early its inputs arrived (negative = late)
//! ```

use crate::lockstep::{input_stats_frame, INPUT_REPORT_EVERY};
use bytes::{Buf, BufMut, Bytes, BytesMut};
use net_common::types::{ActorId, Tick};
use net_lagcomp::{History, Hitbox};
use net_replication::{ClientView, ReplicationConfig, Replicator};
use net_sim::{EntityId, Frame, FrameConfig, Input};
use net_wire::{BitReader, ComponentValue, Schema, Value};
use std::collections::{HashMap, HashSet, VecDeque};

pub const KIND_INPUT: u8 = 0x10;
pub const KIND_DELTA: u8 = 0x11;
pub const KIND_RESYNC: u8 = 0x12;
pub const KIND_ACK: u8 = 0x13;
pub const KIND_SPAWNED: u8 = 0x14;

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
    /// A lockstep command of `actor`, before the `step` of its tick.
    fn on_command(&mut self, _frame: &mut Frame, _actor: ActorId, _command: &[u8]) {}
    /// Called after the frame was restored from a checkpoint (node failover).
    fn reset(&mut self) {}
    /// Answers to tagged spawns since the last call.
    fn take_spawned(&mut self) -> Vec<(ActorId, u32, Spawned)> {
        Vec::new()
    }
    /// The first tag `actor` has not used; `None` when the logic takes no tagged spawns.
    fn next_spawn_tag(&self, _actor: ActorId) -> Option<u32> {
        None
    }
    /// The spawn tags remembered for each actor, oldest first: (actor, tag, entity).
    fn spawn_tags(&self) -> Vec<(ActorId, u32, EntityId)> {
        Vec::new()
    }
    fn restore_spawn_tags(&mut self, _tags: Vec<(ActorId, u32, EntityId)>) {}
    /// Entities whose ownership policy is not the default one.
    fn policies(&self) -> Vec<(EntityId, OwnerPolicy)> {
        Vec::new()
    }
    fn restore_policies(&mut self, _policies: Vec<(EntityId, OwnerPolicy)>) {}
}

/// Project-supplied logic in the WebAssembly sandbox (`net-game`).
impl GameLogic for net_game::WasmLogic {
    fn on_join(&mut self, frame: &mut Frame, actor: ActorId) -> Option<EntityId> {
        net_game::WasmLogic::on_join(self, frame, actor)
    }
    fn on_leave(&mut self, frame: &mut Frame, actor: ActorId, avatar: Option<EntityId>) {
        net_game::WasmLogic::on_leave(self, frame, actor, avatar)
    }
    fn step(&mut self, frame: &mut Frame, inputs: &[(ActorId, Option<EntityId>, Vec<u8>, bool)]) {
        net_game::WasmLogic::step(self, frame, inputs)
    }
    fn on_command(&mut self, frame: &mut Frame, actor: ActorId, command: &[u8]) {
        net_game::WasmLogic::on_command(self, frame, actor, command)
    }
    fn hitboxes(&self, _frame: &Frame) -> Vec<(EntityId, Hitbox)> {
        net_game::WasmLogic::hitboxes(self)
    }
    fn reset(&mut self) {
        net_game::WasmLogic::reset(self)
    }
}

/// Entities one client may force into view with `set_view`.
pub const MAX_INTEREST: usize = 64;

/// Arena: input = i8 move_x, i8 move_z, u8 buttons; server integrates velocity.
pub struct ArenaLogic {
    pub speed: f32,
}

impl GameLogic for ArenaLogic {
    fn on_join(&mut self, frame: &mut Frame, actor: ActorId) -> Option<EntityId> {
        let entity = frame.spawn(Some(actor)).ok()?;
        let x = (actor.0 % 16) as f32 * 4.0 - 32.0;
        let z = (actor.0 / 16) as f32 * 4.0 - 32.0;
        frame
            .set(
                entity,
                "Transform",
                ComponentValue {
                    fields: vec![Value::Vec3([x, 0.0, z]), Value::Vec3([0.0; 3])],
                },
            )
            .ok()?;
        frame
            .set(
                entity,
                "Avatar",
                ComponentValue {
                    fields: vec![Value::UInt(actor.0 as u64), Value::UInt(100)],
                },
            )
            .ok()?;
        Some(entity)
    }

    fn on_leave(&mut self, frame: &mut Frame, _actor: ActorId, avatar: Option<EntityId>) {
        if let Some(entity) = avatar {
            let _ = frame.despawn(entity);
        }
    }

    fn step(&mut self, frame: &mut Frame, inputs: &[(ActorId, Option<EntityId>, Vec<u8>, bool)]) {
        let dt = frame.dt();
        for (_, avatar, data, _) in inputs {
            let Some(entity) = avatar else { continue };
            let (mx, mz) = if data.len() >= 2 {
                (data[0] as i8 as f32 / 127.0, data[1] as i8 as f32 / 127.0)
            } else {
                (0.0, 0.0)
            };
            if let Some(transform) = frame.get_mut(*entity, "Transform") {
                let vel = [mx * self.speed, 0.0, mz * self.speed];
                if let Value::Vec3(position) = &mut transform.fields[0] {
                    position[0] = (position[0] + vel[0] * dt).clamp(-512.0, 512.0);
                    position[2] = (position[2] + vel[2] * dt).clamp(-512.0, 512.0);
                }
                transform.fields[1] = Value::Vec3(vel);
            }
        }
    }

    fn hitboxes(&self, frame: &Frame) -> Vec<(EntityId, Hitbox)> {
        frame
            .entities()
            .filter_map(|e| {
                let position = net_sim::vec3_of(e.components.get(&0)?, 0)?;
                Some((
                    e.id,
                    Hitbox {
                        center: [position[0], position[1] + 1.0, position[2]],
                        half: [0.5, 1.0, 0.5],
                    },
                ))
            })
            .collect()
    }
}

/// Owner-authoritative: every actor gets one entity (its avatar) and may spawn more. The input
/// body is either the plain form (`u8 n, n × (cid, encode_full)` applied to the avatar) or,
/// when the first byte is `OPS_MARKER`, a list of ops: `u8 n_ops`, each `u8 op` followed by
/// `OP_WRITE: u48 entity, u8 n, writes` · `OP_SPAWN: u8 n, writes` (new entity owned by the
/// actor, at most `MAX_OWNED` per actor) · `OP_SPAWN_TAGGED: u32 tag, u8 n, writes` (the same,
/// and the room answers the actor with `0x14`: the entity, or that the op was refused or
/// invalid, in which case nothing is created; a tag among the actor's last `SPAWN_TAGS` creates
/// nothing and is answered again, so the op can be repeated; tag 0 is the avatar's and invalid
/// here) · `OP_DESPAWN: u48 entity` · `OP_TRANSFER: u48 entity,
/// u16 actor` (hand ownership to another joined actor) · `OP_SET_POLICY: u48 entity, u8 policy`
/// (owner only) · `OP_REQUEST: u48 entity` (take an unowned entity, or any entity whose policy is
/// `dynamic`; the cell arbitrates, the first request in a tick wins and the loser sees the owner
/// unchanged) · `OP_RELEASE: u48 entity` (owner drops ownership; the entity stays, unowned).
/// Only the owner may write, despawn, transfer, set the policy or release an entity; the first
/// invalid op ends the input. What happens to a leaving actor's entities is the per-entity policy:
/// `destroy_on_leave` (default) despawns them, `to_master` hands them to the oldest remaining
/// player (the room master), `orphan` and `dynamic` leave them alive without an owner, so anyone
/// may `OP_REQUEST` them. For games that bring their own schema but keep simulation on the clients.
#[derive(Default)]
pub struct OwnerLogic {
    /// Ownership policy per entity; absent = `DestroyOnLeave`.
    pub policies: HashMap<EntityId, OwnerPolicy>,
    /// The last `SPAWN_TAGS` tags each actor spawned under, oldest first, with their entities.
    tags: HashMap<ActorId, VecDeque<(u32, EntityId)>>,
    /// Spawn answers not yet handed to the room.
    spawned: Vec<(ActorId, u32, Spawned)>,
}
pub const OPS_MARKER: u8 = 0xFF;
pub const OP_WRITE: u8 = 0;
pub const OP_SPAWN: u8 = 1;
pub const OP_DESPAWN: u8 = 2;
pub const OP_TRANSFER: u8 = 3;
pub const OP_SET_POLICY: u8 = 4;
pub const OP_REQUEST: u8 = 5;
pub const OP_RELEASE: u8 = 6;
pub const OP_SPAWN_TAGGED: u8 = 7;
pub const MAX_OWNED: usize = 32;
/// Tags of an actor the room remembers, so that a repeated spawn creates nothing.
pub const SPAWN_TAGS: usize = 64;

/// How a tagged spawn ended: the status byte of `0x14`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spawned {
    /// The entity the tag stands for, created now or by an earlier input with the same tag.
    Created(EntityId),
    /// The actor owns `MAX_OWNED` entities already, or the room has no room for another.
    Refused,
    /// The writes of the op do not follow the schema.
    Invalid,
}

/// Per-entity ownership policy (`OP_SET_POLICY`): what the cell does when the owner leaves and who
/// may take the entity over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OwnerPolicy {
    /// Despawned with its owner.
    #[default]
    DestroyOnLeave = 0,
    /// Handed to the oldest remaining player when the owner leaves.
    ToMaster = 1,
    /// Stays alive without an owner; anyone may request it.
    Orphan = 2,
    /// Anyone may take it at any time, owner or not (physics hand-offs).
    Dynamic = 3,
}

impl OwnerPolicy {
    pub fn from_u8(byte: u8) -> Option<Self> {
        Some(match byte {
            0 => Self::DestroyOnLeave,
            1 => Self::ToMaster,
            2 => Self::Orphan,
            3 => Self::Dynamic,
            _ => return None,
        })
    }
}

fn owns(frame: &mut Frame, id: EntityId, actor: ActorId) -> bool {
    frame.entity_mut(id).is_some_and(|e| e.owner == Some(actor))
}

/// `u8 n, n × (cid, encode_full)` applied to `entity`; false on a malformed list.
/// Reads `u8 n, n × (cid, encode_full)` and writes the components to `entity`; without one the
/// writes are only read past.
fn read_writes(frame: &mut Frame, entity: Option<EntityId>, reader: &mut BitReader) -> bool {
    let cid_bits = frame.schema.component_id_bits();
    let Ok(count) = reader.read_bits(8) else {
        return false;
    };
    for _ in 0..count {
        let Ok(cid) = reader.read_bits(cid_bits) else {
            return false;
        };
        let Some(component) = frame.schema.by_id(cid as u16).cloned() else {
            return false;
        };
        let Ok(value) = ComponentValue::decode_full(&component, reader) else {
            return false;
        };
        if let Some(ent) = entity.and_then(|id| frame.entity_mut(id)) {
            ent.components.insert(component.id, value);
        }
    }
    true
}

impl GameLogic for OwnerLogic {
    fn on_join(&mut self, frame: &mut Frame, actor: ActorId) -> Option<EntityId> {
        frame.spawn(Some(actor)).ok()
    }
    fn on_leave(&mut self, frame: &mut Frame, actor: ActorId, _avatar: Option<EntityId>) {
        self.tags.remove(&actor);
        self.spawned.retain(|(of, ..)| *of != actor);
        let owned: Vec<EntityId> = frame
            .entities()
            .filter(|e| e.owner == Some(actor))
            .map(|e| e.id)
            .collect();
        // Oldest remaining player = lowest actor id still owning an avatar (actor ids follow join order).
        let master = frame
            .entities()
            .filter_map(|e| e.owner)
            .filter(|a| *a != actor)
            .min_by_key(|a| a.0);
        for entity in owned {
            match self.policies.get(&entity).copied().unwrap_or_default() {
                OwnerPolicy::DestroyOnLeave => {
                    let _ = frame.despawn(entity);
                    self.policies.remove(&entity);
                }
                OwnerPolicy::ToMaster => {
                    if let Some(ent) = frame.entity_mut(entity) {
                        ent.owner = master;
                    }
                    if master.is_none() {
                        let _ = frame.despawn(entity);
                        self.policies.remove(&entity);
                    }
                }
                OwnerPolicy::Orphan | OwnerPolicy::Dynamic => {
                    if let Some(ent) = frame.entity_mut(entity) {
                        ent.owner = None;
                    }
                }
            }
        }
    }
    fn step(&mut self, frame: &mut Frame, inputs: &[(ActorId, Option<EntityId>, Vec<u8>, bool)]) {
        for (actor, avatar, data, fresh) in inputs {
            if !*fresh {
                continue;
            }
            let mut reader = BitReader::new(data);
            if data.first() != Some(&OPS_MARKER) {
                if let Some(entity) = avatar {
                    read_writes(frame, Some(*entity), &mut reader);
                }
                continue;
            }
            let _ = reader.read_bits(8);
            let Ok(n_ops) = reader.read_bits(8) else {
                continue;
            };
            for _ in 0..n_ops {
                let Ok(op) = reader.read_bits(8) else {
                    break;
                };
                let ok = match op as u8 {
                    OP_WRITE => {
                        let Ok(id) = reader.read_bits(48) else {
                            break;
                        };
                        let id = EntityId::from_u64(id);
                        owns(frame, id, *actor) && read_writes(frame, Some(id), &mut reader)
                    }
                    OP_SPAWN => {
                        let owned = frame.entities().filter(|e| e.owner == Some(*actor)).count();
                        if owned >= MAX_OWNED {
                            break;
                        }
                        match frame.spawn(Some(*actor)) {
                            Ok(id) => read_writes(frame, Some(id), &mut reader),
                            Err(_) => break,
                        }
                    }
                    OP_SPAWN_TAGGED => {
                        let Ok(tag) = reader.read_bits(32) else {
                            break;
                        };
                        let tag = tag as u32;
                        if tag == 0 {
                            break;
                        }
                        let known = self
                            .tags
                            .get(actor)
                            .and_then(|tags| tags.iter().find(|(seen, _)| *seen == tag))
                            .map(|(_, entity)| *entity);
                        if let Some(entity) = known {
                            self.spawned.push((*actor, tag, Spawned::Created(entity)));
                            // The entity has moved on since: its first writes are only read past.
                            read_writes(frame, None, &mut reader)
                        } else if !read_writes(frame, None, &mut reader.clone()) {
                            self.spawned.push((*actor, tag, Spawned::Invalid));
                            false
                        } else {
                            let owned =
                                frame.entities().filter(|e| e.owner == Some(*actor)).count();
                            let created = (owned < MAX_OWNED)
                                .then(|| frame.spawn(Some(*actor)).ok())
                                .flatten();
                            match created {
                                Some(entity) => {
                                    let tags = self.tags.entry(*actor).or_default();
                                    if tags.len() == SPAWN_TAGS {
                                        tags.pop_front();
                                    }
                                    tags.push_back((tag, entity));
                                    self.spawned.push((*actor, tag, Spawned::Created(entity)));
                                    read_writes(frame, Some(entity), &mut reader)
                                }
                                None => {
                                    self.spawned.push((*actor, tag, Spawned::Refused));
                                    false
                                }
                            }
                        }
                    }
                    OP_DESPAWN => {
                        let Ok(id) = reader.read_bits(48) else {
                            break;
                        };
                        let id = EntityId::from_u64(id);
                        let ok = owns(frame, id, *actor)
                            && Some(id) != *avatar
                            && frame.despawn(id).is_ok();
                        if ok {
                            self.policies.remove(&id);
                        }
                        ok
                    }
                    OP_SET_POLICY => {
                        let (Ok(id), Ok(p)) = (reader.read_bits(48), reader.read_bits(8)) else {
                            break;
                        };
                        let id = EntityId::from_u64(id);
                        match OwnerPolicy::from_u8(p as u8) {
                            Some(policy) if owns(frame, id, *actor) && Some(id) != *avatar => {
                                self.policies.insert(id, policy);
                                true
                            }
                            _ => false,
                        }
                    }
                    OP_REQUEST => {
                        let Ok(id) = reader.read_bits(48) else {
                            break;
                        };
                        let id = EntityId::from_u64(id);
                        let policy = self.policies.get(&id).copied().unwrap_or_default();
                        let owned = frame.entities().filter(|e| e.owner == Some(*actor)).count();
                        match frame.entity_mut(id) {
                            Some(ent)
                                if owned < MAX_OWNED
                                    && (ent.owner.is_none() || policy == OwnerPolicy::Dynamic) =>
                            {
                                ent.owner = Some(*actor);
                                true
                            }
                            _ => false,
                        }
                    }
                    OP_RELEASE => {
                        let Ok(id) = reader.read_bits(48) else {
                            break;
                        };
                        let id = EntityId::from_u64(id);
                        if owns(frame, id, *actor) && Some(id) != *avatar {
                            if let Some(ent) = frame.entity_mut(id) {
                                ent.owner = None;
                            }
                            // A released entity must survive without an owner.
                            if self.policies.get(&id).copied().unwrap_or_default()
                                == OwnerPolicy::DestroyOnLeave
                            {
                                self.policies.insert(id, OwnerPolicy::Orphan);
                            }
                            true
                        } else {
                            false
                        }
                    }
                    OP_TRANSFER => {
                        let (Ok(id), Ok(to)) = (reader.read_bits(48), reader.read_bits(16)) else {
                            break;
                        };
                        let (id, to) = (EntityId::from_u64(id), ActorId(to as _));
                        let joined = frame.entities().any(|e| e.owner == Some(to));
                        if owns(frame, id, *actor) && joined {
                            if let Some(entity) = frame.entity_mut(id) {
                                entity.owner = Some(to);
                            }
                            true
                        } else {
                            false
                        }
                    }
                    _ => false,
                };
                if !ok {
                    break;
                }
            }
        }
    }
    fn hitboxes(&self, _frame: &Frame) -> Vec<(EntityId, Hitbox)> {
        Vec::new()
    }
    fn take_spawned(&mut self) -> Vec<(ActorId, u32, Spawned)> {
        std::mem::take(&mut self.spawned)
    }
    fn next_spawn_tag(&self, actor: ActorId) -> Option<u32> {
        let newest = self
            .tags
            .get(&actor)
            .and_then(|tags| tags.iter().map(|(tag, _)| *tag).max());
        Some(newest.map_or(1, |tag| tag.wrapping_add(1).max(1)))
    }
    fn spawn_tags(&self) -> Vec<(ActorId, u32, EntityId)> {
        self.tags
            .iter()
            .flat_map(|(actor, tags)| tags.iter().map(|(tag, entity)| (*actor, *tag, *entity)))
            .collect()
    }
    fn restore_spawn_tags(&mut self, tags: Vec<(ActorId, u32, EntityId)>) {
        self.tags.clear();
        for (actor, tag, entity) in tags {
            self.tags.entry(actor).or_default().push_back((tag, entity));
        }
    }
    fn policies(&self) -> Vec<(EntityId, OwnerPolicy)> {
        self.policies.iter().map(|(id, p)| (*id, *p)).collect()
    }
    fn restore_policies(&mut self, policies: Vec<(EntityId, OwnerPolicy)>) {
        self.policies = policies.into_iter().collect();
    }
}

/// `0x14`: the entity `tag` stands for (the avatar under tag 0), or why there is none.
fn spawned_frame(tag: u32, spawned: Spawned, tick: u32) -> Bytes {
    let (status, entity) = match spawned {
        Spawned::Created(entity) => (0, entity.to_u64()),
        Spawned::Refused => (1, 0),
        Spawned::Invalid => (2, 0),
    };
    let mut frame = BytesMut::with_capacity(18);
    frame.put_u8(KIND_SPAWNED);
    frame.put_u32(tag);
    frame.put_u8(status);
    frame.put_u64(entity);
    frame.put_u32(tick);
    frame.freeze()
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
    /// Per-actor input-frame tokens: +1.1 per tick (tick rate + 10 %), burst `INPUT_BURST`.
    input_tokens: HashMap<ActorId, f32>,
    pub inputs_rate_limited: u64,
    pub invalid_inputs: u64,
    /// Input frames whose every input was for a tick already simulated.
    pub late_inputs: u64,
    /// Per actor since the last report: (min input margin in ticks, late input frames).
    margins: HashMap<ActorId, (i32, u16)>,
    /// `0x14` frames not yet sent: an actor gets its own once its data path is up.
    spawned: Vec<(ActorId, Bytes)>,
    /// Avatars from the checkpoint the room was continued from, of players not back yet.
    moved_avatars: HashMap<ActorId, EntityId>,
    seed: u64,
    /// Newest input taken from each actor: (tick it was sent for, tick it runs on). An input for
    /// a tick already simulated runs on the current tick instead, so its ops are not lost: once,
    /// in the order sent, one input a tick. The inputs after it wait for the next ticks, up to
    /// `INPUT_REDUNDANCY` ticks; the repeats in later frames bring what did not fit.
    taken: HashMap<ActorId, (u32, u32)>,
    pub resyncs: u64,
    /// Watch-only actors: a view of the whole world, no avatar, inputs ignored.
    spectators: HashSet<ActorId>,
    /// Instant replay: full-world delta frames (`0x11`, fresh baseline) every `replay_every`
    /// ticks, the last `replay_keep` of them. Room props `replay_every` / `replay_secs`.
    replay: VecDeque<(u32, Bytes)>,
    replay_every: u32,
    replay_keep: usize,
}

/// Input frames a client may send in a burst before the per-tick allowance applies.
pub const INPUT_BURST: f32 = 8.0;
/// Inputs further ahead of the room tick than this are dropped as invalid.
pub const INPUT_MAX_AHEAD: u32 = 64;

impl SyncRoom {
    /// `schema`: IDL text from room props, or None for the built-in arena.
    pub fn new(schema: Option<&str>, hz: u16, seed: u64) -> Result<Self, String> {
        Self::with_replication(schema, hz, seed, ReplicationConfig::default())
    }

    /// Replication settings from room props: `budget_bytes`, `aoi_cell`, `aoi_radius`,
    /// `position` ("Component.field_index"; empty string disables AOI).
    pub fn replication_from_props(
        props: &serde_json::Map<String, serde_json::Value>,
    ) -> ReplicationConfig {
        let mut cfg = ReplicationConfig::default();
        if let Some(bytes) = props.get("budget_bytes").and_then(|v| v.as_u64()) {
            cfg.budget_bytes = (bytes as usize).clamp(200, 60_000);
        }
        if let Some(cell) = props.get("aoi_cell").and_then(|v| v.as_f64()) {
            cfg.aoi_cell = cell.max(0.0) as f32;
        }
        if let Some(radius) = props.get("aoi_radius").and_then(|v| v.as_u64()) {
            cfg.aoi_radius = radius.min(64) as u32;
        }
        if let Some(ticks) = props.get("stale_after").and_then(|v| v.as_u64()) {
            cfg.stale_after = ticks.clamp(1, 3600) as u32;
        }
        if let Some(name) = props.get("position").and_then(|v| v.as_str()) {
            cfg.position = name
                .split_once('.')
                .and_then(|(c, f)| f.parse().ok().map(|f| (c.to_string(), f)));
        }
        cfg
    }

    pub fn with_replication(
        schema: Option<&str>,
        hz: u16,
        seed: u64,
        rep: ReplicationConfig,
    ) -> Result<Self, String> {
        Self::with_logic(schema, hz, seed, rep, None)
    }

    /// `logic`: server-side game logic (WASM module); None = owner-authoritative writes for a
    /// custom schema, or the built-in arena when there is no schema either.
    pub fn with_logic(
        schema: Option<&str>,
        hz: u16,
        seed: u64,
        rep: ReplicationConfig,
        logic: Option<Box<dyn GameLogic>>,
    ) -> Result<Self, String> {
        let (schema, logic): (Schema, Box<dyn GameLogic>) = match (schema, logic) {
            (Some(src), Some(l)) => (Schema::parse(src).map_err(|e| e.to_string())?, l),
            (None, Some(l)) => (Schema::parse(ARENA_SCHEMA).unwrap(), l),
            (Some(src), None) => (
                Schema::parse(src).map_err(|e| e.to_string())?,
                Box::new(OwnerLogic::default()),
            ),
            (None, None) => (
                Schema::parse(ARENA_SCHEMA).unwrap(),
                Box::new(ArenaLogic { speed: 6.0 }),
            ),
        };
        let hz = hz.clamp(8, 128);
        let rep = Replicator::new(rep, &schema);
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
            input_tokens: HashMap::new(),
            inputs_rate_limited: 0,
            invalid_inputs: 0,
            late_inputs: 0,
            margins: HashMap::new(),
            spawned: Vec::new(),
            moved_avatars: HashMap::new(),
            seed,
            taken: HashMap::new(),
            resyncs: 0,
            spectators: HashSet::new(),
            replay: VecDeque::new(),
            replay_every: (hz as u32 / 2).max(1),
            replay_keep: 60,
        })
    }

    pub fn tick_period(&self) -> std::time::Duration {
        std::time::Duration::from_micros(1_000_000 / self.hz as u64)
    }

    pub fn on_join(&mut self, actor: ActorId) {
        self.on_join_as(actor, false)
    }

    pub fn on_join_as(&mut self, actor: ActorId, spectator: bool) {
        if spectator {
            self.spectators.insert(actor);
            self.views
                .entry(actor)
                .or_insert_with(|| ClientView::new(actor));
            return;
        }
        if let Some(avatar) = self.avatars.get(&actor) {
            // A second connection of the player took the slot over: it has to be told as well.
            self.announce_avatar(actor, *avatar);
            return;
        }
        let owned = |id: &EntityId| {
            self.frame
                .entity(*id)
                .is_some_and(|e| e.owner == Some(actor))
        };
        let moved = self.moved_avatars.remove(&actor).filter(owned);
        let existing = moved.or_else(|| {
            let mut entities = self.frame.entities();
            entities.find(|e| e.owner == Some(actor)).map(|e| e.id)
        });
        let avatar = match existing {
            Some(entity) => Some(entity),
            None => self.logic.on_join(&mut self.frame, actor),
        };
        self.avatars.insert(actor, avatar);
        self.announce_avatar(actor, avatar);
        let mut view = ClientView::new(actor);
        view.viewpoint = avatar;
        self.views.insert(actor, view);
    }

    /// `set_view` control message: extra viewpoints (spectators get the first as their main
    /// viewpoint) and forced-interest entities; unknown entities are ignored. Returns what was
    /// applied.
    pub fn set_view(
        &mut self,
        actor: ActorId,
        viewpoints: &[u64],
        interest: &[u64],
    ) -> (usize, usize) {
        let Some(view) = self.views.get_mut(&actor) else {
            return (0, 0);
        };
        let live = |id: u64| {
            let entity = net_sim::EntityId::from_u64(id);
            self.frame.entity(entity).is_some().then_some(entity)
        };
        let mut vps: Vec<net_sim::EntityId> =
            viewpoints.iter().filter_map(|&id| live(id)).collect();
        vps.dedup();
        if view.viewpoint.is_none() {
            view.viewpoint = (!vps.is_empty()).then(|| vps.remove(0));
        }
        vps.truncate(net_replication::MAX_VIEWPOINTS - 1);
        view.extra_viewpoints = vps;
        view.interest = interest
            .iter()
            .take(MAX_INTEREST)
            .filter_map(|&id| live(id))
            .collect();
        (
            view.extra_viewpoints.len() + view.viewpoint.is_some() as usize,
            view.interest.len(),
        )
    }

    fn announce_avatar(&mut self, actor: ActorId, avatar: Option<EntityId>) {
        if let Some(entity) = avatar {
            self.spawned
                .retain(|(of, frame)| *of != actor || frame[1..5] != [0; 4]);
            let frame = spawned_frame(0, Spawned::Created(entity), self.frame.tick.0);
            self.spawned.push((actor, frame));
        }
    }

    pub fn on_leave(&mut self, actor: ActorId) {
        self.input_tokens.remove(&actor);
        self.spawned.retain(|(of, _)| *of != actor);
        self.moved_avatars.remove(&actor);
        self.margins.remove(&actor);
        self.taken.remove(&actor);
        self.spectators.remove(&actor);
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
                if self.spectators.contains(&actor) {
                    return true;
                }
                payload.advance(1);
                if payload.len() < 5 {
                    self.invalid_inputs += 1;
                    return true;
                }
                let tokens = self.input_tokens.entry(actor).or_insert(INPUT_BURST);
                if *tokens < 1.0 {
                    self.inputs_rate_limited += 1;
                    return true;
                }
                *tokens -= 1.0;
                let first = payload.get_u32();
                let count = payload.get_u8() as usize;
                if first.wrapping_sub(self.frame.tick.0) < 1 << 31
                    && first > self.frame.tick.0 + INPUT_MAX_AHEAD
                {
                    self.invalid_inputs += 1;
                    return true;
                }
                let now = self.frame.tick.0;
                let before = self.taken.get(&actor).map(|&(newest, _)| newest);
                let mut read = 0;
                for i in 0..count.min(INPUT_REDUNDANCY + 1) {
                    if payload.is_empty() {
                        break;
                    }
                    let len = payload.get_u8() as usize;
                    if payload.len() < len {
                        break;
                    }
                    let data = payload.split_to(len).to_vec();
                    read += 1;
                    let sent = first.wrapping_add(i as u32);
                    let last = self.taken.get(&actor).copied();
                    let newer = last.is_none_or(|(newest, _)| sent > newest);
                    let due = sent.max(now);
                    let runs = match last {
                        Some((_, earlier)) if newer => due.max(earlier + 1),
                        _ => due,
                    };
                    let free = newer || (sent >= now && !self.frame.has_input(actor, Tick(sent)));
                    if !free || runs - due > INPUT_REDUNDANCY as u32 {
                        continue;
                    }
                    let input = Input {
                        actor,
                        tick: Tick(runs),
                        data,
                    };
                    if self.frame.push_input(input) && newer {
                        self.taken.insert(actor, (sent, runs));
                    }
                }
                if read > 0 {
                    let newest = first.wrapping_add(read - 1);
                    // A repeat of what the room has taken says nothing of the client's timing.
                    if before.is_none_or(|taken| newest > taken) {
                        let margin = (newest as i64 - now as i64).clamp(-127, 127) as i32;
                        let slack = self.margins.entry(actor).or_insert((i32::MAX, 0));
                        slack.0 = slack.0.min(margin);
                        if margin < 0 {
                            self.late_inputs += 1;
                            slack.1 = slack.1.saturating_add(1);
                        }
                    }
                }
                true
            }
            KIND_RESYNC => {
                self.resyncs += 1;
                if let Some(view) = self.views.get_mut(&actor) {
                    view.reset();
                }
                true
            }
            KIND_ACK => {
                payload.advance(1);
                if payload.len() < 8 {
                    return true;
                }
                let last = payload.get_u32();
                let bits = payload.get_u32();
                if let Some(view) = self.views.get_mut(&actor) {
                    view.ack(last, bits);
                }
                true
            }
            _ => false,
        }
    }

    /// One simulation step. Returns the delta frame for each actor with a view.
    pub fn tick(&mut self) -> Vec<(ActorId, Bytes)> {
        for bucket in self.input_tokens.values_mut() {
            *bucket = (*bucket + 1.1).min(INPUT_BURST);
        }
        let actors: Vec<ActorId> = self.avatars.keys().copied().collect();
        let mut inputs = Vec::with_capacity(actors.len());
        for actor in &actors {
            let (data, fresh) = self.frame.input_for(*actor);
            inputs.push((*actor, self.avatars[actor], data, fresh));
        }
        let ran = self.frame.tick.0;
        let logic = &mut self.logic;
        self.frame.advance(|f| logic.step(f, &inputs));
        for (actor, tag, spawned) in self.logic.take_spawned() {
            self.spawned.push((actor, spawned_frame(tag, spawned, ran)));
        }
        let boxes = self.logic.hitboxes(&self.frame);
        self.history.record(self.frame.tick, boxes);
        let mut out = Vec::with_capacity(self.views.len());
        for (actor, view) in self.views.iter_mut() {
            let delta = self.rep.build(&self.frame, view);
            let mut buf = BytesMut::with_capacity(delta.len() + 1);
            buf.put_u8(KIND_DELTA);
            buf.extend_from_slice(&delta);
            out.push((*actor, buf.freeze()));
        }
        if self.replay_keep > 0 && self.frame.tick.0.is_multiple_of(self.replay_every) {
            // A fresh view has no baseline and no viewpoint: the delta is the whole world.
            let mut view = ClientView::new(ActorId(0xFFFF));
            let delta = self.rep.build(&self.frame, &mut view);
            let mut buf = BytesMut::with_capacity(delta.len() + 1);
            buf.put_u8(KIND_DELTA);
            buf.extend_from_slice(&delta);
            self.replay.push_back((self.frame.tick.0, buf.freeze()));
            while self.replay.len() > self.replay_keep {
                self.replay.pop_front();
            }
        }
        out
    }

    /// The delta the last tick built for `actor` had no data path to leave by. Its next one
    /// starts from nothing, or what only that delta carried would never reach the client.
    pub fn undelivered(&mut self, actor: ActorId) {
        if let Some(view) = self.views.get_mut(&actor) {
            view.reset();
        }
    }

    /// Room props `replay_every` (ticks between snapshots, default hz/2) and `replay_secs`
    /// (window kept, default 30; 0 disables the ring).
    pub fn set_replay(&mut self, every: Option<u32>, secs: Option<u32>) {
        if let Some(ticks) = every {
            self.replay_every = ticks.max(1);
        }
        if let Some(seconds) = secs {
            self.replay_keep =
                (seconds as usize * self.hz as usize / self.replay_every as usize).min(4096);
        }
        self.replay.truncate(self.replay_keep);
    }

    /// Instant replay of a live sync room: full-world `0x11` frames for ticks `from..to`, each
    /// decodable by any SDK's sync client as if it were the first delta after joining
    /// (feed them in order to a fresh `SyncWorld`/`SyncSession`). `from_tick` is the tick of the
    /// first frame given, or `from` when there is none.
    pub fn replay_window(&self, from: u32, to: Option<u32>) -> serde_json::Value {
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD;
        let mut window = self
            .replay
            .iter()
            .filter(|(t, _)| *t >= from && to.is_none_or(|to| *t < to))
            .peekable();
        let from_tick = window.peek().map_or(from, |(t, _)| *t);
        let frames: Vec<serde_json::Value> = window
            .map(|(t, d)| serde_json::json!({ "tick": t, "delta": b64.encode(d) }))
            .collect();
        serde_json::json!({
            "mode": "sync",
            "tick_hz": self.hz,
            "schema_hash": self.schema_hash,
            "every": self.replay_every,
            "from_tick": from_tick,
            "frames": frames,
        })
    }

    /// Simulation events raised during the last tick(s), as relay frames: `target` None means
    /// everyone in the room.
    pub fn take_events(&mut self) -> Vec<(Option<ActorId>, u8, Bytes)> {
        self.frame
            .drain_events()
            .into_iter()
            .map(|e| (e.target, e.code as u8, Bytes::from(e.data)))
            .collect()
    }

    /// The `0x14` frames of the actors `connected` holds for, in the order they were raised; the
    /// rest wait for their actor's data path.
    pub fn take_spawned(&mut self, connected: impl Fn(ActorId) -> bool) -> Vec<(ActorId, Bytes)> {
        let (ready, waiting) = std::mem::take(&mut self.spawned)
            .into_iter()
            .partition(|(actor, _)| connected(*actor));
        self.spawned = waiting;
        ready
    }

    /// Checkpoint body: frame snapshot (tick, rng, entities). Views are rebuilt on restore.
    pub fn checkpoint(&self) -> Vec<u8> {
        self.frame.snapshot()
    }

    /// The first spawn tag `actor` has not used, for `joined`; `None` for a spectator and in a
    /// room that takes no tagged spawns.
    pub fn next_spawn_tag(&self, actor: ActorId) -> Option<u32> {
        if self.spectators.contains(&actor) {
            return None;
        }
        self.logic.next_spawn_tag(actor)
    }

    /// The spawn tags the room remembers, for the checkpoint: (actor, tag, entity).
    pub fn spawn_tags(&self) -> Vec<(ActorId, u32, u64)> {
        let tags = self.logic.spawn_tags().into_iter();
        tags.map(|(actor, tag, entity)| (actor, tag, entity.to_u64()))
            .collect()
    }

    pub fn restore_spawn_tags(&mut self, tags: Vec<(ActorId, u32, u64)>) {
        let tags = tags.into_iter();
        self.logic.restore_spawn_tags(
            tags.map(|(actor, tag, entity)| (actor, tag, EntityId::from_u64(entity)))
                .collect(),
        );
    }

    /// The seed the room and its logic were started with.
    pub fn seed(&self) -> u64 {
        self.seed
    }

    /// Which entity is whose avatar, for the checkpoint: (actor, entity).
    pub fn avatar_ids(&self) -> Vec<(ActorId, u64)> {
        let joined = self
            .avatars
            .iter()
            .filter_map(|(actor, avatar)| Some((*actor, (*avatar)?)));
        let waiting = self
            .moved_avatars
            .iter()
            .map(|(actor, avatar)| (*actor, *avatar));
        joined
            .chain(waiting)
            .map(|(actor, avatar)| (actor, avatar.to_u64()))
            .collect()
    }

    pub fn restore_avatars(&mut self, avatars: Vec<(ActorId, u64)>) {
        let avatars = avatars.into_iter();
        self.moved_avatars = avatars
            .map(|(actor, avatar)| (actor, EntityId::from_u64(avatar)))
            .collect();
    }

    /// Ownership policies other than the default, for the checkpoint: (entity, policy).
    pub fn policies(&self) -> Vec<(u64, u8)> {
        let policies = self.logic.policies().into_iter();
        policies
            .map(|(entity, policy)| (entity.to_u64(), policy as u8))
            .collect()
    }

    pub fn restore_policies(&mut self, policies: Vec<(u64, u8)>) {
        let known = policies.into_iter().filter_map(|(entity, policy)| {
            Some((EntityId::from_u64(entity), OwnerPolicy::from_u8(policy)?))
        });
        self.logic.restore_policies(known.collect());
    }

    /// Re-host from a checkpoint: avatars are re-linked on the next on_join, by `restore_avatars`
    /// where the checkpoint has them, else by owner.
    pub fn restore(&mut self, body: &[u8]) -> Result<(), String> {
        self.frame.restore(body).map_err(|e| e.to_string())?;
        self.views.clear();
        self.avatars.clear();
        self.spawned.clear();
        self.history = History::new(self.hz as usize * 2);
        self.logic.reset();
        Ok(())
    }

    /// (resync requests, starved client-ticks) since the last call.
    pub fn take_replication_stats(&mut self) -> (u64, u64) {
        let starved = self
            .views
            .values_mut()
            .map(|v| std::mem::take(&mut v.starved))
            .sum();
        (std::mem::take(&mut self.resyncs), starved)
    }

    /// Every `INPUT_REPORT_EVERY` ticks: one `KIND_INPUT_STATS` frame per actor that sent input
    /// since the last report. Call right after `tick()`.
    pub fn input_reports(&mut self) -> Vec<(ActorId, Bytes)> {
        if !self.frame.tick.0.is_multiple_of(INPUT_REPORT_EVERY) || self.margins.is_empty() {
            return Vec::new();
        }
        let tick = self.frame.tick.0;
        std::mem::take(&mut self.margins)
            .into_iter()
            .map(|(actor, (min, late))| (actor, input_stats_frame(tick, min, late)))
            .collect()
    }

    /// (rate-limited, invalid, late) input frames since the last call.
    pub fn take_input_stats(&mut self) -> (u64, u64, u64) {
        (
            std::mem::take(&mut self.inputs_rate_limited),
            std::mem::take(&mut self.invalid_inputs),
            std::mem::take(&mut self.late_inputs),
        )
    }

    /// Deltas clients lost since the last call (from their acks).
    pub fn take_lost_deltas(&mut self) -> u64 {
        self.views
            .values_mut()
            .map(|v| std::mem::take(&mut v.lost_deltas))
            .sum()
    }

    /// Encodes a client ack frame from `AckTracker::ack()`.
    pub fn encode_ack(last: u32, bits: u32) -> Bytes {
        let mut buf = BytesMut::with_capacity(9);
        buf.put_u8(KIND_ACK);
        buf.put_u32(last);
        buf.put_u32(bits);
        buf.freeze()
    }

    /// Encodes a client input packet (used by SDK/loadtest): the last K inputs ending at `tick`.
    pub fn encode_input(first_tick: Tick, inputs: &[Vec<u8>]) -> Bytes {
        let mut buf = BytesMut::new();
        buf.put_u8(KIND_INPUT);
        buf.put_u32(first_tick.0);
        buf.put_u8(inputs.len().min(255) as u8);
        for input in inputs.iter().take(255) {
            buf.put_u8(input.len().min(255) as u8);
            buf.extend_from_slice(&input[..input.len().min(255)]);
        }
        buf.freeze()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_view_steers_viewpoints_and_interest() {
        let mut room = SyncRoom::new(None, 60, 7).unwrap();
        let (alice, bob, spec) = (ActorId(1), ActorId(2), ActorId(3));
        room.on_join(alice);
        room.on_join(bob);
        room.on_join_as(spec, true);
        let av_a = room.avatars[&alice].unwrap().to_u64();
        let av_b = room.avatars[&bob].unwrap().to_u64();
        // spectator: first viewpoint becomes the main one, unknown entities are ignored
        assert_eq!(
            room.set_view(spec, &[av_a, 0xdead_beef], &[av_b, 0xdead_beef]),
            (1, 1)
        );
        assert_eq!(room.views[&spec].viewpoint.map(|e| e.to_u64()), Some(av_a));
        assert!(room.views[&spec].extra_viewpoints.is_empty());
        assert_eq!(room.views[&spec].interest.len(), 1);
        // player keeps the avatar as main viewpoint; extras capped at MAX_VIEWPOINTS - 1
        let many: Vec<u64> = (0..10)
            .map(|i| if i % 2 == 0 { av_a } else { av_b })
            .collect();
        assert_eq!(
            room.set_view(alice, &many, &[]),
            (net_replication::MAX_VIEWPOINTS, 0)
        );
        assert_eq!(
            room.views[&alice].viewpoint.map(|e| e.to_u64()),
            Some(room.avatars[&alice].unwrap().to_u64())
        );
        // clearing
        assert_eq!(room.set_view(alice, &[], &[]), (1, 0));
        assert_eq!(room.set_view(ActorId(99), &[av_a], &[]), (0, 0));
    }

    #[test]
    fn arena_moves_avatars_and_replicates() {
        let mut room = SyncRoom::new(None, 60, 7).unwrap();
        let (alice, bob) = (ActorId(1), ActorId(2));
        room.on_join(alice);
        room.on_join(bob);
        let mut client_a = Frame::new(room.frame.schema.clone(), FrameConfig::default(), 0);
        // actor a pushes right for 60 ticks, b idle
        for t in 0..60u32 {
            let inp = SyncRoom::encode_input(Tick(t), &[vec![127, 0, 0]]);
            assert!(room.handle(alice, inp));
            let out = room.tick();
            assert_eq!(out.len(), 2);
            for (actor, delta) in out {
                if actor == alice {
                    assert_eq!(delta[0], KIND_DELTA);
                    Replicator::apply(&mut client_a, &delta[1..]).unwrap();
                }
            }
        }
        let ea = room.avatars[&alice].unwrap();
        let position = net_sim::vec3_of(room.frame.get(ea, "Transform").unwrap(), 0).unwrap();
        assert!((position[0] - (-28.0 + 6.0)).abs() < 0.05, "{position:?}");
        let pc = net_sim::vec3_of(client_a.get(ea, "Transform").unwrap(), 0).unwrap();
        assert!((pc[0] - position[0]).abs() < 0.02);
        assert_eq!(client_a.entity_count(), 2);
        assert_eq!(client_a.tick, room.frame.tick);
        // leave removes the avatar on the next delta
        room.on_leave(bob);
        for (actor, delta) in room.tick() {
            if actor == alice {
                Replicator::apply(&mut client_a, &delta[1..]).unwrap();
            }
        }
        assert_eq!(client_a.entity_count(), 1);
        // resync -> full snapshot again
        room.handle(alice, Bytes::from_static(&[KIND_RESYNC]));
        let out = room.tick();
        let full = &out.iter().find(|(x, _)| *x == alice).unwrap().1;
        assert!(full.len() > 12);
        // lag comp sees the avatar where it was
        let hit = room
            .history
            .raycast_at(
                Tick(30),
                [position[0] - 20.0, 1.0, position[2]],
                [1.0, 0.0, 0.0],
                100.0,
                None,
            )
            .unwrap();
        assert_eq!(hit.entity, ea);
        assert!(hit.distance < 20.0);
    }

    #[test]
    fn checkpoint_restore_keeps_avatars() {
        let mut room = SyncRoom::new(None, 60, 3).unwrap();
        room.on_join(ActorId(1));
        for t in 0..10u32 {
            room.handle(
                ActorId(1),
                SyncRoom::encode_input(Tick(t), &[vec![127, 0, 0]]),
            );
            room.tick();
        }
        let ea = room.avatars[&ActorId(1)].unwrap();
        let position = net_sim::vec3_of(room.frame.get(ea, "Transform").unwrap(), 0).unwrap();
        let ck = room.checkpoint();
        let mut fresh = SyncRoom::new(None, 60, 0).unwrap();
        fresh.restore(&ck).unwrap();
        assert_eq!(fresh.frame.tick, Tick(10));
        fresh.on_join(ActorId(1));
        assert_eq!(fresh.avatars[&ActorId(1)], Some(ea));
        let p2 = net_sim::vec3_of(fresh.frame.get(ea, "Transform").unwrap(), 0).unwrap();
        assert!((position[0] - p2[0]).abs() < 0.02);
        room.frame.raise(7, Some(ActorId(1)), vec![1, 2]);
        room.frame.raise(8, None, vec![]);
        let ev = room.take_events();
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[0].1, 7);
        let mut props = serde_json::Map::new();
        props.insert("budget_bytes".into(), serde_json::json!(5000));
        props.insert("stale_after".into(), serde_json::json!(90));
        props.insert("position".into(), serde_json::json!("Transform.0"));
        let cfg = SyncRoom::replication_from_props(&props);
        assert_eq!(cfg.budget_bytes, 5000);
        assert_eq!(cfg.stale_after, 90);
        assert_eq!(cfg.position, Some(("Transform".into(), 0)));
    }

    #[test]
    fn owner_logic_applies_component_writes() {
        let src = "component Pose {\n x: f32 @range(-100, 100, 0.1)\n}";
        let mut room = SyncRoom::new(Some(src), 30, 1).unwrap();
        let alice = ActorId(5);
        room.on_join(alice);
        let ea = room.avatars[&alice].unwrap();
        let pose = room.frame.schema.component("Pose").unwrap().clone();
        let mut writer = net_wire::BitWriter::new();
        writer.write_bits(1, 8);
        writer.write_bits(pose.id as u64, room.frame.schema.component_id_bits());
        ComponentValue {
            fields: vec![Value::F32(42.0)],
        }
        .encode_full(&pose, &mut writer)
        .unwrap();
        room.handle(alice, SyncRoom::encode_input(Tick(0), &[writer.finish()]));
        room.tick();
        assert_eq!(
            room.frame.get(ea, "Pose"),
            Some(&ComponentValue {
                fields: vec![Value::F32(42.0)]
            })
        );
        assert!(SyncRoom::new(Some("component {"), 30, 1).is_err());
    }

    #[test]
    fn ownership_policies_orphan_master_dynamic() {
        let src = "component Pose {\n x: f32 @range(-100, 100, 0.1)\n}";
        let mut room = SyncRoom::new(Some(src), 30, 1).unwrap();
        let (alice, bob, carol) = (ActorId(1), ActorId(2), ActorId(3));
        room.on_join(alice);
        room.on_join(bob);
        room.on_join(carol);
        let send = |room: &mut SyncRoom, actor: ActorId, w: net_wire::BitWriter| {
            let tick = room.frame.tick;
            room.handle(actor, SyncRoom::encode_input(tick, &[w.finish()]));
            room.tick();
        };
        // (op, entity, trailing value and its bit width)
        type Op = (u8, Option<u64>, Option<(u64, u32)>);
        let ops = |items: &[Op]| {
            let mut writer = net_wire::BitWriter::new();
            writer.write_bits(OPS_MARKER as u64, 8);
            writer.write_bits(items.len() as u64, 8);
            for (op, entity, extra) in items {
                writer.write_bits(*op as u64, 8);
                if let Some(spawned) = entity {
                    writer.write_bits(*spawned, 48);
                }
                if let Some((v, bits)) = extra {
                    writer.write_bits(*v, *bits);
                }
            }
            writer
        };
        // a spawns three entities (no initial writes) and gives each a policy.
        send(
            &mut room,
            alice,
            ops(&[
                (OP_SPAWN, None, Some((0, 8))),
                (OP_SPAWN, None, Some((0, 8))),
                (OP_SPAWN, None, Some((0, 8))),
            ]),
        );
        let spawned: Vec<EntityId> = room
            .frame
            .entities()
            .filter(|e| e.owner == Some(alice) && Some(e.id) != room.avatars[&alice])
            .map(|e| e.id)
            .collect();
        assert_eq!(spawned.len(), 3);
        let (orphan, to_master, dynamic) = (spawned[0], spawned[1], spawned[2]);
        send(
            &mut room,
            alice,
            ops(&[
                (
                    OP_SET_POLICY,
                    Some(orphan.to_u64()),
                    Some((OwnerPolicy::Orphan as u64, 8)),
                ),
                (
                    OP_SET_POLICY,
                    Some(to_master.to_u64()),
                    Some((OwnerPolicy::ToMaster as u64, 8)),
                ),
                (
                    OP_SET_POLICY,
                    Some(dynamic.to_u64()),
                    Some((OwnerPolicy::Dynamic as u64, 8)),
                ),
            ]),
        );
        // c takes the dynamic entity while a still owns it; the orphan one is refused (a owns it).
        send(
            &mut room,
            carol,
            ops(&[
                (OP_REQUEST, Some(dynamic.to_u64()), None),
                (OP_REQUEST, Some(orphan.to_u64()), None),
            ]),
        );
        assert_eq!(room.frame.entity_mut(dynamic).unwrap().owner, Some(carol));
        assert_eq!(room.frame.entity_mut(orphan).unwrap().owner, Some(alice));
        // a leaves: orphan stays unowned, to_master goes to the oldest remaining player (b).
        room.on_leave(alice);
        assert_eq!(room.frame.entity_mut(orphan).unwrap().owner, None);
        assert_eq!(room.frame.entity_mut(to_master).unwrap().owner, Some(bob));
        // b requests the orphan and gets it, then releases it: unowned again, still alive.
        send(
            &mut room,
            bob,
            ops(&[(OP_REQUEST, Some(orphan.to_u64()), None)]),
        );
        assert_eq!(room.frame.entity_mut(orphan).unwrap().owner, Some(bob));
        send(
            &mut room,
            bob,
            ops(&[(OP_RELEASE, Some(orphan.to_u64()), None)]),
        );
        assert_eq!(room.frame.entity_mut(orphan).unwrap().owner, None);
    }

    #[test]
    fn owner_ops_spawn_write_transfer_despawn_and_leave() {
        let src = "component Pose {\n x: f32 @range(-100, 100, 0.1)\n}";
        let mut room = SyncRoom::new(Some(src), 30, 1).unwrap();
        let (alice, bob) = (ActorId(1), ActorId(2));
        room.on_join(alice);
        room.on_join(bob);
        let pose_schema = room.frame.schema.component("Pose").unwrap().clone();
        let cb = room.frame.schema.component_id_bits();
        let pose = |x: f32| ComponentValue {
            fields: vec![Value::F32(x)],
        };
        let send = |room: &mut SyncRoom, actor: ActorId, w: net_wire::BitWriter| {
            let tick = room.frame.tick;
            room.handle(actor, SyncRoom::encode_input(tick, &[w.finish()]));
            room.tick();
        };
        let mut writer = net_wire::BitWriter::new();
        writer.write_bits(OPS_MARKER as u64, 8);
        writer.write_bits(1, 8);
        writer.write_bits(OP_SPAWN as u64, 8);
        writer.write_bits(1, 8);
        writer.write_bits(pose_schema.id as u64, cb);
        pose(7.0).encode_full(&pose_schema, &mut writer).unwrap();
        send(&mut room, alice, writer);
        let spawned = room
            .frame
            .entities()
            .find(|e| e.owner == Some(alice) && Some(e.id) != room.avatars[&alice])
            .unwrap()
            .id;
        assert_eq!(room.frame.get(spawned, "Pose"), Some(&pose(7.0)));

        // b does not own it: the write is ignored
        let mut writer = net_wire::BitWriter::new();
        writer.write_bits(OPS_MARKER as u64, 8);
        writer.write_bits(1, 8);
        writer.write_bits(OP_WRITE as u64, 8);
        writer.write_bits(spawned.to_u64(), 48);
        writer.write_bits(1, 8);
        writer.write_bits(pose_schema.id as u64, cb);
        pose(9.0).encode_full(&pose_schema, &mut writer).unwrap();
        send(&mut room, bob, writer);
        assert_eq!(room.frame.get(spawned, "Pose"), Some(&pose(7.0)));

        // a hands it to b; b writes and despawns it
        let mut writer = net_wire::BitWriter::new();
        writer.write_bits(OPS_MARKER as u64, 8);
        writer.write_bits(1, 8);
        writer.write_bits(OP_TRANSFER as u64, 8);
        writer.write_bits(spawned.to_u64(), 48);
        writer.write_bits(2, 16);
        send(&mut room, alice, writer);
        assert_eq!(room.frame.entity_mut(spawned).unwrap().owner, Some(bob));
        let mut writer = net_wire::BitWriter::new();
        writer.write_bits(OPS_MARKER as u64, 8);
        writer.write_bits(2, 8);
        writer.write_bits(OP_WRITE as u64, 8);
        writer.write_bits(spawned.to_u64(), 48);
        writer.write_bits(1, 8);
        writer.write_bits(pose_schema.id as u64, cb);
        pose(9.0).encode_full(&pose_schema, &mut writer).unwrap();
        writer.write_bits(OP_DESPAWN as u64, 8);
        writer.write_bits(spawned.to_u64(), 48);
        send(&mut room, bob, writer);
        assert!(room.frame.entity_mut(spawned).is_none());

        // transfer to an actor that is not in the room is refused; avatars cannot be despawned
        let av = room.avatars[&alice].unwrap();
        let mut writer = net_wire::BitWriter::new();
        writer.write_bits(OPS_MARKER as u64, 8);
        writer.write_bits(2, 8);
        writer.write_bits(OP_TRANSFER as u64, 8);
        writer.write_bits(av.to_u64(), 48);
        writer.write_bits(77, 16);
        writer.write_bits(OP_DESPAWN as u64, 8);
        writer.write_bits(av.to_u64(), 48);
        send(&mut room, alice, writer);
        assert_eq!(room.frame.entity_mut(av).unwrap().owner, Some(alice));

        // the cap holds and leaving despawns everything the actor owns
        for _ in 0..MAX_OWNED + 2 {
            let mut writer = net_wire::BitWriter::new();
            writer.write_bits(OPS_MARKER as u64, 8);
            writer.write_bits(1, 8);
            writer.write_bits(OP_SPAWN as u64, 8);
            writer.write_bits(0, 8);
            send(&mut room, alice, writer);
        }
        let owned = room
            .frame
            .entities()
            .filter(|e| e.owner == Some(alice))
            .count();
        assert_eq!(owned, MAX_OWNED);
        room.on_leave(alice);
        assert_eq!(
            room.frame
                .entities()
                .filter(|e| e.owner == Some(alice))
                .count(),
            0
        );
        assert_eq!(room.frame.entities().count(), 1);
    }

    #[test]
    fn input_frames_are_rate_limited_per_actor() {
        let mut room = SyncRoom::new(None, 60, 1).unwrap();
        let alice = ActorId(1);
        room.on_join(alice);
        let frame = SyncRoom::encode_input(Tick(1), &[vec![1, 0, 0]]);
        for _ in 0..20 {
            room.handle(alice, frame.clone());
        }
        assert_eq!(room.take_input_stats(), (20 - INPUT_BURST as u64, 0, 0));
        let _ = room.tick();
        assert!(room.handle(alice, frame.clone()));
        assert!(room.handle(alice, frame.clone()));
        assert_eq!(room.take_input_stats(), (1, 0, 0));
        let far = SyncRoom::encode_input(Tick(1000), &[vec![1, 0, 0]]);
        let _ = room.tick();
        room.handle(alice, far);
        assert_eq!(room.take_input_stats(), (0, 1, 0));
    }

    #[test]
    fn input_frame_with_nothing_but_past_ticks_is_counted_late() {
        let mut room = SyncRoom::new(None, 60, 1).unwrap();
        let alice = ActorId(1);
        room.on_join(alice);
        for _ in 0..5 {
            let _ = room.tick();
        }
        let now = room.frame.tick.0;
        let late = SyncRoom::encode_input(Tick(now - 3), &vec![vec![1, 0, 0]; 3]);
        room.handle(alice, late);
        assert_eq!(room.take_input_stats(), (0, 0, 1));
        // The newest input is still due: the older ones in the frame are the usual repeats.
        let due = SyncRoom::encode_input(Tick(now - 2), &vec![vec![1, 0, 0]; 3]);
        room.handle(alice, due);
        assert_eq!(room.take_input_stats(), (0, 0, 0));
    }

    #[test]
    fn repeat_of_a_frame_is_not_counted_late() {
        let mut room = SyncRoom::new(None, 30, 1).unwrap();
        let alice = ActorId(1);
        room.on_join(alice);
        let ahead = room.frame.tick.0 + 2;
        let tail = SyncRoom::encode_input(Tick(ahead - 2), &vec![vec![1, 0, 0]; 3]);
        room.handle(alice, tail.clone());
        for _ in 0..3 {
            let _ = room.tick();
            room.handle(alice, tail.clone());
        }
        assert_eq!(room.take_input_stats(), (0, 0, 0));
        assert_eq!(room.margins[&alice], (2, 0));

        // The input after it, behind the room as the repeats were, is.
        let _ = room.tick();
        let next = SyncRoom::encode_input(Tick(ahead - 2), &vec![vec![1, 0, 0]; 4]);
        room.handle(alice, next);
        assert_eq!(room.take_input_stats(), (0, 0, 1));
        assert_eq!(room.margins[&alice], (-1, 1));
    }

    #[test]
    fn ops_sent_for_past_ticks_run_once_each_on_the_ticks_that_follow() {
        let src = "component Pose {\n x: f32 @range(-100, 100, 0.1)\n}";
        let mut room = SyncRoom::new(Some(src), 30, 1).unwrap();
        let alice = ActorId(1);
        room.on_join(alice);
        for _ in 0..5 {
            let _ = room.tick();
        }
        assert_eq!(room.frame.entities().count(), 1);
        let spawn = vec![OPS_MARKER, 1, OP_SPAWN, 0];
        // What a client sends at once after a stall: three frames, each with one more input.
        let behind = Tick(room.frame.tick.0 - 4);
        for inputs in 1..=3 {
            room.handle(
                alice,
                SyncRoom::encode_input(behind, &vec![spawn.clone(); inputs]),
            );
        }
        let _ = room.tick();
        assert_eq!(room.frame.entities().count(), 2);
        // The client sends nothing more: the other two run on the next two ticks.
        for _ in 0..2 {
            let _ = room.tick();
        }
        assert_eq!(room.frame.entities().count(), 4);
        // A repeat of what has run makes nothing twice.
        for _ in 0..4 {
            room.handle(
                alice,
                SyncRoom::encode_input(behind, &vec![spawn.clone(); 3]),
            );
            let _ = room.tick();
        }
        assert_eq!(room.frame.entities().count(), 4);
    }

    #[test]
    fn input_for_a_tick_that_late_inputs_hold_runs_after_them() {
        let src = "component Pose {\n x: f32 @range(-100, 100, 0.1)\n}";
        let mut room = SyncRoom::new(Some(src), 30, 1).unwrap();
        let alice = ActorId(1);
        room.on_join(alice);
        for _ in 0..5 {
            let _ = room.tick();
        }
        let avatar = room.avatars[&alice].unwrap();
        let schema = room.frame.schema.component("Pose").unwrap().clone();
        let bits = room.frame.schema.component_id_bits();
        let pose = |x: f32| ComponentValue {
            fields: vec![Value::F32(x)],
        };
        let write = |x: f32| {
            let mut writer = net_wire::BitWriter::new();
            writer.write_bits(1, 8);
            writer.write_bits(schema.id as u64, bits);
            pose(x).encode_full(&schema, &mut writer).unwrap();
            writer.finish()
        };
        let now = room.frame.tick;
        let late = SyncRoom::encode_input(Tick(now.0 - 4), &[write(1.0), write(2.0), write(3.0)]);
        let due = SyncRoom::encode_input(now, &[write(4.0)]);
        for _ in 0..2 {
            room.handle(alice, late.clone());
            room.handle(alice, due.clone());
        }
        let mut seen = Vec::new();
        for _ in 0..5 {
            let _ = room.tick();
            seen.push(room.frame.get(avatar, "Pose").cloned());
        }
        let expected = [1.0, 2.0, 3.0, 4.0, 4.0].map(|x| Some(pose(x)));
        assert_eq!(seen, expected);
    }

    /// (tag, status, entity, tick) of a `0x14` frame.
    fn spawn_answer(frame: &Bytes) -> (u32, u8, u64, u32) {
        assert_eq!((frame.len(), frame[0]), (18, KIND_SPAWNED));
        (
            u32::from_be_bytes(frame[1..5].try_into().unwrap()),
            frame[5],
            u64::from_be_bytes(frame[6..14].try_into().unwrap()),
            u32::from_be_bytes(frame[14..18].try_into().unwrap()),
        )
    }

    #[test]
    fn tagged_spawn_is_answered_and_a_repeat_creates_nothing() {
        let src = "component Pose {\n x: f32 @range(-100, 100, 0.1)\n}";
        let mut room = SyncRoom::new(Some(src), 30, 1).unwrap();
        let alice = ActorId(1);
        room.on_join(alice);
        let pose_schema = room.frame.schema.component("Pose").unwrap().clone();
        let cb = room.frame.schema.component_id_bits();
        let pose = |x: f32| ComponentValue {
            fields: vec![Value::F32(x)],
        };
        let op = |code: u8| {
            let mut writer = net_wire::BitWriter::new();
            writer.write_bits(OPS_MARKER as u64, 8);
            writer.write_bits(1, 8);
            writer.write_bits(code as u64, 8);
            writer
        };
        let write_pose = |writer: &mut net_wire::BitWriter, x: f32| {
            writer.write_bits(1, 8);
            writer.write_bits(pose_schema.id as u64, cb);
            pose(x).encode_full(&pose_schema, writer).unwrap();
        };
        let spawn = |tag: u32, x: f32| {
            let mut writer = op(OP_SPAWN_TAGGED);
            writer.write_bits(tag as u64, 32);
            write_pose(&mut writer, x);
            writer
        };
        // Runs one input with one op; returns the answers it raised.
        let run = |room: &mut SyncRoom, writer: net_wire::BitWriter| {
            let tick = room.frame.tick;
            room.handle(alice, SyncRoom::encode_input(tick, &[writer.finish()]));
            room.tick();
            let answers = room.take_spawned(|_| true);
            answers
                .iter()
                .map(|(to, frame)| {
                    assert_eq!(*to, alice);
                    spawn_answer(frame)
                })
                .collect::<Vec<_>>()
        };

        // The avatar is announced under tag 0 once the data path is there.
        let avatar = room.avatars[&alice].unwrap();
        assert!(room.take_spawned(|_| false).is_empty());
        let announced = room.take_spawned(|_| true);
        assert_eq!(spawn_answer(&announced[0].1), (0, 0, avatar.to_u64(), 0));
        assert_eq!(room.next_spawn_tag(alice), Some(1));

        let tick = room.frame.tick.0;
        let answers = run(&mut room, spawn(7, 7.0));
        let [(7, 0, entity, at)] = answers[..] else {
            panic!("{answers:?}");
        };
        assert_eq!(at, tick);
        let entity = EntityId::from_u64(entity);
        assert_eq!(room.frame.entity_mut(entity).unwrap().owner, Some(alice));
        assert_eq!(room.frame.get(entity, "Pose"), Some(&pose(7.0)));
        assert_eq!(room.next_spawn_tag(alice), Some(8));

        // The entity moves on; the same op again names it and leaves it alone.
        let mut writer = op(OP_WRITE);
        writer.write_bits(entity.to_u64(), 48);
        write_pose(&mut writer, 9.0);
        assert_eq!(run(&mut room, writer), []);
        let answers = run(&mut room, spawn(7, 7.0));
        assert_eq!(answers.len(), 1);
        assert_eq!(answers[0].0, 7);
        assert_eq!((answers[0].1, answers[0].2), (0, entity.to_u64()));
        assert_eq!(room.frame.get(entity, "Pose"), Some(&pose(9.0)));
        assert_eq!(room.frame.entities().count(), 2);

        // Tag 0 is the avatar's; writes cut short are answered as invalid. Neither creates.
        assert_eq!(run(&mut room, spawn(0, 1.0)), []);
        let mut writer = op(OP_SPAWN_TAGGED);
        writer.write_bits(8, 32);
        writer.write_bits(1, 8);
        let tick = room.frame.tick.0;
        assert_eq!(run(&mut room, writer), [(8, 2, 0, tick)]);
        assert_eq!(room.frame.entities().count(), 2);
        assert_eq!(room.next_spawn_tag(alice), Some(8));

        // Past the limit of owned entities the spawn is refused, and may be sent again.
        for tag in 10..40 {
            assert_eq!(run(&mut room, spawn(tag, 0.0))[0].1, 0, "tag {tag}");
        }
        assert_eq!(room.frame.entities().count(), MAX_OWNED);
        let tick = room.frame.tick.0;
        assert_eq!(run(&mut room, spawn(40, 0.0)), [(40, 1, 0, tick)]);
        let mut writer = op(OP_DESPAWN);
        writer.write_bits(entity.to_u64(), 48);
        assert_eq!(run(&mut room, writer), []);
        assert_eq!(run(&mut room, spawn(40, 0.0))[0].1, 0);
        assert_eq!(room.frame.entities().count(), MAX_OWNED);
    }

    #[test]
    fn spawn_tags_are_remembered_past_the_entity_up_to_a_number() {
        let src = "component Pose {\n x: f32 @range(-100, 100, 0.1)\n}";
        let mut room = SyncRoom::new(Some(src), 30, 1).unwrap();
        let alice = ActorId(1);
        room.on_join(alice);
        let op = |code: u8, tail: &[(u64, u32)]| {
            let mut writer = net_wire::BitWriter::new();
            writer.write_bits(OPS_MARKER as u64, 8);
            writer.write_bits(1, 8);
            writer.write_bits(code as u64, 8);
            for (value, bits) in tail {
                writer.write_bits(*value, *bits);
            }
            writer.finish()
        };
        // Spawns under `tag` and returns the entity the room answers with.
        let spawn = |room: &mut SyncRoom, tag: u32| {
            let tick = room.frame.tick;
            let body = op(OP_SPAWN_TAGGED, &[(tag as u64, 32), (0, 8)]);
            room.handle(alice, SyncRoom::encode_input(tick, &[body]));
            room.tick();
            let answers = room.take_spawned(|_| true);
            let (answered, status, entity, _) = spawn_answer(&answers.last().unwrap().1);
            assert_eq!((answered, status), (tag, 0));
            entity
        };
        let mut first = Vec::new();
        for tag in 1..=SPAWN_TAGS as u32 + 1 {
            let entity = spawn(&mut room, tag);
            first.push(entity);
            let tick = room.frame.tick;
            let body = op(OP_DESPAWN, &[(entity, 48)]);
            room.handle(alice, SyncRoom::encode_input(tick, &[body]));
            room.tick();
        }
        assert_eq!(room.frame.entities().count(), 1);

        // Tag 2 is among the last 64 and still stands for its entity, despawned since.
        assert_eq!(spawn(&mut room, 2), first[1]);
        assert_eq!(room.frame.entities().count(), 1);
        // Tag 1 has been pushed out: the room takes it for a new spawn.
        spawn(&mut room, 1);
        assert_eq!(room.frame.entities().count(), 2);
    }

    #[test]
    fn input_margins_are_reported_to_the_actor() {
        let mut room = SyncRoom::new(None, 60, 1).unwrap();
        let (alice, bob) = (ActorId(1), ActorId(2));
        room.on_join(alice);
        room.on_join(bob);
        for _ in 0..5 {
            let _ = room.tick();
            assert!(room.input_reports().is_empty());
        }
        let now = room.frame.tick.0;
        room.handle(
            alice,
            SyncRoom::encode_input(Tick(now - 2), &[vec![1, 0, 0]]),
        );
        room.handle(
            alice,
            SyncRoom::encode_input(Tick(now - 1), &vec![vec![1, 0, 0]; 2]),
        );
        room.handle(
            alice,
            SyncRoom::encode_input(Tick(now + 3), &[vec![1, 0, 0]]),
        );
        let mut reports = Vec::new();
        while reports.is_empty() {
            let _ = room.tick();
            reports = room.input_reports();
        }
        assert_eq!(reports.len(), 1, "bob sent nothing");
        assert_eq!(reports[0].0, alice);
        let stats = crate::lockstep::LockstepRoom::decode_input_stats(reports[0].1.clone());
        assert_eq!(stats, Some((INPUT_REPORT_EVERY, -2, 1)));
        for _ in 0..INPUT_REPORT_EVERY {
            let _ = room.tick();
            assert!(room.input_reports().is_empty(), "nothing sent since");
        }
    }

    #[test]
    fn spectator_sees_everything_and_cannot_move() {
        let mut room = SyncRoom::new(None, 60, 3).unwrap();
        let (alice, watcher) = (ActorId(1), ActorId(9));
        room.on_join(alice);
        room.on_join_as(watcher, true);
        assert_eq!(room.avatars.len(), 1);
        room.handle(watcher, SyncRoom::encode_input(Tick(0), &[vec![127, 0, 0]]));
        let out = room.tick();
        let to_w = out
            .iter()
            .find(|(x, _)| *x == watcher)
            .map(|(_, d)| d.len())
            .unwrap();
        assert!(to_w > 12, "spectator got a delta with the avatar");
        assert_eq!(room.frame.entity_count(), 1);
    }
}
