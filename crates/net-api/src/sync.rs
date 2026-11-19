//! Sync mode: the server runs the simulation at a fixed tick, clients send inputs, the
//! server replicates state deltas with interest management.
//!
//! Data frames on the unreliable channel (first byte = kind):
//! ```text
//! 0x10 Input    client->server  u32 first_tick, u8 n, n x (u8 len, bytes)   (redundant ticks)
//! 0x11 Delta    server->client  replication delta (see net-replication)
//! 0x12 Resync   client->server  drop baselines, next delta is a full snapshot
//! 0x13 Ack      client->server  u32 last delta tick, u32 history bits (see `AckTracker`)
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
    /// Called after the frame was restored from a checkpoint (node failover).
    fn reset(&mut self) {}
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
/// actor, at most `MAX_OWNED` per actor) · `OP_DESPAWN: u48 entity` · `OP_TRANSFER: u48 entity,
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
}
pub const OPS_MARKER: u8 = 0xFF;
pub const OP_WRITE: u8 = 0;
pub const OP_SPAWN: u8 = 1;
pub const OP_DESPAWN: u8 = 2;
pub const OP_TRANSFER: u8 = 3;
pub const OP_SET_POLICY: u8 = 4;
pub const OP_REQUEST: u8 = 5;
pub const OP_RELEASE: u8 = 6;
pub const MAX_OWNED: usize = 32;

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
fn read_writes(frame: &mut Frame, entity: EntityId, reader: &mut BitReader) -> bool {
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
        if let Some(ent) = frame.entity_mut(entity) {
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
                    read_writes(frame, *entity, &mut reader);
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
                        owns(frame, id, *actor) && read_writes(frame, id, &mut reader)
                    }
                    OP_SPAWN => {
                        let owned = frame.entities().filter(|e| e.owner == Some(*actor)).count();
                        if owned >= MAX_OWNED {
                            break;
                        }
                        match frame.spawn(Some(*actor)) {
                            Ok(id) => read_writes(frame, id, &mut reader),
                            Err(_) => break,
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
        if self.avatars.contains_key(&actor) {
            return;
        }
        let existing = self
            .frame
            .entities()
            .find(|e| e.owner == Some(actor))
            .map(|e| e.id);
        let avatar = match existing {
            Some(entity) => Some(entity),
            None => self.logic.on_join(&mut self.frame, actor),
        };
        self.avatars.insert(actor, avatar);
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

    pub fn on_leave(&mut self, actor: ActorId) {
        self.input_tokens.remove(&actor);
        self.margins.remove(&actor);
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
                let (mut read, mut taken) = (0, 0);
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
                    taken += u32::from(self.frame.push_input(Input {
                        actor,
                        tick: Tick(first.wrapping_add(i as u32)),
                        data,
                    }));
                }
                if read > 0 {
                    let newest = first.wrapping_add(read - 1);
                    let margin = (newest as i64 - self.frame.tick.0 as i64).clamp(-127, 127) as i32;
                    let slack = self.margins.entry(actor).or_insert((i32::MAX, 0));
                    slack.0 = slack.0.min(margin);
                    // A frame repeats the last few inputs, so the older ones are late by design.
                    if taken == 0 {
                        self.late_inputs += 1;
                        slack.1 = slack.1.saturating_add(1);
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
        let logic = &mut self.logic;
        self.frame.advance(|f| logic.step(f, &inputs));
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
    /// (feed them in order to a fresh `SyncWorld`/`SyncSession`). `from_tick` is the first tick
    /// actually held.
    pub fn replay_window(&self, from: u32, to: Option<u32>) -> serde_json::Value {
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD;
        let frames: Vec<serde_json::Value> = self
            .replay
            .iter()
            .filter(|(t, _)| *t >= from && to.is_none_or(|hi| *t <= hi))
            .map(|(t, d)| serde_json::json!({ "tick": t, "delta": b64.encode(d) }))
            .collect();
        serde_json::json!({
            "mode": "sync",
            "tick_hz": self.hz,
            "schema_hash": self.schema_hash,
            "every": self.replay_every,
            "from_tick": self.replay.front().map(|(t, _)| *t).unwrap_or(from).max(from),
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

    /// Checkpoint body: frame snapshot (tick, rng, entities). Views are rebuilt on restore.
    pub fn checkpoint(&self) -> Vec<u8> {
        self.frame.snapshot()
    }

    /// Re-host from a checkpoint: avatars are re-linked by owner on the next on_join.
    pub fn restore(&mut self, body: &[u8]) -> Result<(), String> {
        self.frame.restore(body).map_err(|e| e.to_string())?;
        self.views.clear();
        self.avatars.clear();
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
        // The newest input is still due: the older ones in the frame are the usual repeats.
        let due = SyncRoom::encode_input(Tick(now - 2), &vec![vec![1, 0, 0]; 3]);
        room.handle(alice, due);
        assert_eq!(room.take_input_stats(), (0, 0, 0));
        let late = SyncRoom::encode_input(Tick(now - 3), &vec![vec![1, 0, 0]; 3]);
        room.handle(alice, late);
        assert_eq!(room.take_input_stats(), (0, 0, 1));
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
            SyncRoom::encode_input(Tick(now + 3), &[vec![1, 0, 0]]),
        );
        room.handle(
            alice,
            SyncRoom::encode_input(Tick(now - 2), &[vec![1, 0, 0]]),
        );
        room.handle(
            alice,
            SyncRoom::encode_input(Tick(now - 1), &vec![vec![1, 0, 0]; 2]),
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
