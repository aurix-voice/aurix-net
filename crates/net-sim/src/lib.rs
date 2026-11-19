//! Simulation frame: entities with schema components, per-actor inputs, events, fixed tick.
//! The same `Frame` type drives the authoritative server (sync mode) and client prediction;
//! `snapshot`/`restore` and `hash` make it usable for checkpoints and lockstep verification.

use net_common::types::{ActorId, Tick};
use net_wire::{BitReader, BitWriter, ComponentValue, Schema, Value};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};

/// Entity handle: slot index + generation, so stale handles never alias a reused slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EntityId {
    pub index: u32,
    pub generation: u16,
}

impl EntityId {
    pub fn to_u64(self) -> u64 {
        ((self.generation as u64) << 32) | self.index as u64
    }
    pub fn from_u64(raw: u64) -> Self {
        Self {
            index: raw as u32,
            generation: (raw >> 32) as u16,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entity {
    pub id: EntityId,
    /// Actor allowed to send inputs for this entity (None = server-owned).
    pub owner: Option<ActorId>,
    /// Components by schema component id.
    pub components: BTreeMap<u16, ComponentValue>,
    pub spawned_at: Tick,
}

/// One actor's input for one tick: an opaque schema-encoded blob chosen by the game.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Input {
    pub actor: ActorId,
    pub tick: Tick,
    pub data: Vec<u8>,
}

/// Game event raised by the simulation for clients (and the replay).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimEvent {
    pub tick: Tick,
    pub code: u16,
    pub target: Option<ActorId>,
    pub data: Vec<u8>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SimError {
    #[error("unknown entity")]
    NoEntity,
    #[error("unknown component {0}")]
    NoComponent(u16),
    #[error("actor {0:?} does not own the entity")]
    NotOwner(ActorId),
    #[error("entity limit {0} reached")]
    Full(u32),
    #[error("codec: {0}")]
    Codec(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrameConfig {
    pub tick_hz: u16,
    pub max_entities: u32,
    /// Inputs kept per actor for late/duplicate arrivals.
    pub input_window: u32,
}

impl Default for FrameConfig {
    fn default() -> Self {
        Self {
            tick_hz: 60,
            max_entities: 4096,
            input_window: 64,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Frame {
    pub cfg: FrameConfig,
    pub schema: Schema,
    pub tick: Tick,
    slots: Vec<Option<Entity>>,
    generations: Vec<u16>,
    free: Vec<u32>,
    inputs: BTreeMap<ActorId, BTreeMap<u32, Vec<u8>>>,
    /// Last input applied per actor; reused when a tick's input is missing.
    last_input: BTreeMap<ActorId, Vec<u8>>,
    events: VecDeque<SimEvent>,
    rng: u64,
}

impl Frame {
    pub fn new(schema: Schema, cfg: FrameConfig, seed: u64) -> Self {
        Self {
            cfg,
            schema,
            tick: Tick(0),
            slots: Vec::new(),
            generations: Vec::new(),
            free: Vec::new(),
            inputs: BTreeMap::new(),
            last_input: BTreeMap::new(),
            events: VecDeque::new(),
            rng: seed.max(1),
        }
    }

    pub fn dt(&self) -> f32 {
        1.0 / self.cfg.tick_hz as f32
    }

    // ---- entities -------------------------------------------------------------------

    pub fn spawn(&mut self, owner: Option<ActorId>) -> Result<EntityId, SimError> {
        let index = match self.free.pop() {
            Some(i) => i,
            None => {
                if self.slots.len() as u32 >= self.cfg.max_entities {
                    return Err(SimError::Full(self.cfg.max_entities));
                }
                self.slots.push(None);
                self.generations.push(0);
                (self.slots.len() - 1) as u32
            }
        };
        let id = EntityId {
            index,
            generation: self.generations[index as usize],
        };
        self.slots[index as usize] = Some(Entity {
            id,
            owner,
            components: BTreeMap::new(),
            spawned_at: self.tick,
        });
        Ok(id)
    }

    pub fn despawn(&mut self, id: EntityId) -> Result<(), SimError> {
        let slot = self
            .slots
            .get_mut(id.index as usize)
            .ok_or(SimError::NoEntity)?;
        match slot {
            Some(entity) if entity.id == id => {
                *slot = None;
                self.generations[id.index as usize] =
                    self.generations[id.index as usize].wrapping_add(1);
                self.free.push(id.index);
                Ok(())
            }
            _ => Err(SimError::NoEntity),
        }
    }

    /// Client side: makes sure `id` exists exactly as the server numbered it.
    /// Creates (or re-owns) an entity with a server-chosen id (delta/checkpoint application).
    /// Ids beyond `cfg.max_entities` are rejected instead of growing the slot table (a crafted
    /// id must not make a peer allocate gigabytes).
    pub fn ensure_entity(&mut self, id: EntityId, owner: Option<ActorId>) -> Result<(), SimError> {
        let index = id.index as usize;
        if id.index >= self.cfg.max_entities {
            return Err(SimError::Full(self.cfg.max_entities));
        }
        if index >= self.slots.len() {
            self.slots.resize(index + 1, None);
            self.generations.resize(index + 1, 0);
        }
        match &mut self.slots[index] {
            Some(entity) if entity.id == id => entity.owner = owner,
            slot => {
                *slot = Some(Entity {
                    id,
                    owner,
                    components: BTreeMap::new(),
                    spawned_at: self.tick,
                });
                self.generations[index] = id.generation;
                self.free.retain(|f| *f != id.index);
            }
        }
        Ok(())
    }

    pub fn entity(&self, id: EntityId) -> Option<&Entity> {
        self.slots
            .get(id.index as usize)
            .and_then(|s| s.as_ref())
            .filter(|e| e.id == id)
    }

    pub fn entity_mut(&mut self, id: EntityId) -> Option<&mut Entity> {
        self.slots
            .get_mut(id.index as usize)
            .and_then(|s| s.as_mut())
            .filter(|e| e.id == id)
    }

    pub fn entities(&self) -> impl Iterator<Item = &Entity> {
        self.slots.iter().filter_map(|s| s.as_ref())
    }

    pub fn entity_count(&self) -> usize {
        self.slots.iter().filter(|s| s.is_some()).count()
    }

    /// Sets a component (validated against the schema type by encoding once).
    pub fn set(
        &mut self,
        id: EntityId,
        component: &str,
        value: ComponentValue,
    ) -> Result<(), SimError> {
        let schema = self
            .schema
            .component(component)
            .ok_or(SimError::NoComponent(u16::MAX))?
            .clone();
        let mut writer = BitWriter::new();
        value
            .encode_full(&schema, &mut writer)
            .map_err(|e| SimError::Codec(e.to_string()))?;
        let entity = self.entity_mut(id).ok_or(SimError::NoEntity)?;
        entity.components.insert(schema.id, value);
        Ok(())
    }

    pub fn get(&self, id: EntityId, component: &str) -> Option<&ComponentValue> {
        let schema = self.schema.component(component)?;
        self.entity(id)?.components.get(&schema.id)
    }

    pub fn get_mut(&mut self, id: EntityId, component: &str) -> Option<&mut ComponentValue> {
        let cid = self.schema.component(component)?.id;
        self.entity_mut(id)?.components.get_mut(&cid)
    }

    pub fn remove_component(&mut self, id: EntityId, component: &str) -> Result<(), SimError> {
        let cid = self
            .schema
            .component(component)
            .ok_or(SimError::NoComponent(u16::MAX))?
            .id;
        self.entity_mut(id)
            .ok_or(SimError::NoEntity)?
            .components
            .remove(&cid);
        Ok(())
    }

    // ---- inputs ---------------------------------------------------------------------

    /// Stores an input for a future (or current) tick. Inputs older than the window or for
    /// ticks already simulated are ignored and reported as `false`.
    pub fn push_input(&mut self, input: Input) -> bool {
        if input.tick.0 < self.tick.0 {
            return false;
        }
        let queue = self.inputs.entry(input.actor).or_default();
        queue.insert(input.tick.0, input.data);
        while queue.len() > self.cfg.input_window as usize {
            let first = *queue.keys().next().unwrap();
            queue.remove(&first);
        }
        true
    }

    /// Input for `actor` at the current tick, or the last applied one (clients that lagged
    /// keep doing what they did). Returns the data and whether it was fresh.
    pub fn input_for(&mut self, actor: ActorId) -> (Vec<u8>, bool) {
        let fresh = self
            .inputs
            .get_mut(&actor)
            .and_then(|q| q.remove(&self.tick.0));
        match fresh {
            Some(data) => {
                self.last_input.insert(actor, data.clone());
                (data, true)
            }
            None => (
                self.last_input.get(&actor).cloned().unwrap_or_default(),
                false,
            ),
        }
    }

    pub fn has_input(&self, actor: ActorId, tick: Tick) -> bool {
        self.inputs
            .get(&actor)
            .is_some_and(|q| q.contains_key(&tick.0))
    }

    pub fn forget_actor(&mut self, actor: ActorId) {
        self.inputs.remove(&actor);
        self.last_input.remove(&actor);
    }

    // ---- events / rng / tick ----------------------------------------------------------

    pub fn raise(&mut self, code: u16, target: Option<ActorId>, data: Vec<u8>) {
        self.events.push_back(SimEvent {
            tick: self.tick,
            code,
            target,
            data,
        });
    }

    pub fn drain_events(&mut self) -> Vec<SimEvent> {
        self.events.drain(..).collect()
    }

    /// Deterministic xorshift64*; part of the frame state.
    pub fn rand_u32(&mut self) -> u32 {
        let mut x = self.rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng = x;
        (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 32) as u32
    }

    /// Runs one tick: `step` sees the frame with inputs available, then the tick advances
    /// and stale input slots are dropped.
    pub fn advance<F: FnOnce(&mut Frame)>(&mut self, step: F) {
        step(self);
        self.tick = Tick(self.tick.0.wrapping_add(1));
        let tick = self.tick.0;
        for queue in self.inputs.values_mut() {
            queue.retain(|k, _| *k >= tick);
        }
    }

    // ---- snapshot / hash --------------------------------------------------------------

    /// Full state: tick, rng, every live entity with all components. Schema-encoded so it is
    /// compact and stable across builds.
    pub fn snapshot(&self) -> Vec<u8> {
        let mut writer = BitWriter::new();
        writer.write_bits(self.tick.0 as u64, 32);
        writer.write_bits(self.rng, 64);
        writer.write_bits(self.slots.len() as u64, 32);
        writer.write_bits(self.entity_count() as u64, 32);
        let cid_bits = self.schema.component_id_bits();
        for entity in self.entities() {
            writer.write_bits(entity.id.index as u64, 32);
            writer.write_bits(entity.id.generation as u64, 16);
            writer.write_bits(entity.spawned_at.0 as u64, 32);
            match entity.owner {
                Some(owner) => {
                    writer.write_bool(true);
                    writer.write_bits(owner.0 as u64, 16);
                }
                None => writer.write_bool(false),
            }
            writer.write_bits(entity.components.len() as u64, 16);
            for (cid, v) in &entity.components {
                writer.write_bits(*cid as u64, cid_bits);
                if let Some(component) = self.schema.by_id(*cid) {
                    let _ = v.encode_full(component, &mut writer);
                }
            }
        }
        for generation in &self.generations {
            writer.write_bits(*generation as u64, 16);
        }
        writer.finish()
    }

    pub fn restore(&mut self, buf: &[u8]) -> Result<(), SimError> {
        let err = |e: net_wire::bits::Eof| SimError::Codec(e.to_string());
        let mut reader = BitReader::new(buf);
        let tick = Tick(reader.read_bits(32).map_err(err)? as u32);
        let rng = reader.read_bits(64).map_err(err)?;
        let nslots = reader.read_bits(32).map_err(err)? as usize;
        let nents = reader.read_bits(32).map_err(err)? as usize;
        if nslots > self.cfg.max_entities as usize || nents > nslots {
            return Err(SimError::Codec(format!(
                "snapshot claims {nslots} slots / {nents} entities (limit {})",
                self.cfg.max_entities
            )));
        }
        let cid_bits = self.schema.component_id_bits();
        let mut slots: Vec<Option<Entity>> = vec![None; nslots];
        for _ in 0..nents {
            let index = reader.read_bits(32).map_err(err)? as u32;
            let generation = reader.read_bits(16).map_err(err)? as u16;
            let spawned_at = Tick(reader.read_bits(32).map_err(err)? as u32);
            let owner = if reader.read_bool().map_err(err)? {
                Some(ActorId(reader.read_bits(16).map_err(err)? as u16))
            } else {
                None
            };
            let count = reader.read_bits(16).map_err(err)? as usize;
            let mut components = BTreeMap::new();
            for _ in 0..count {
                let cid = reader.read_bits(cid_bits).map_err(err)? as u16;
                let component = self.schema.by_id(cid).ok_or(SimError::NoComponent(cid))?;
                let value = ComponentValue::decode_full(component, &mut reader)
                    .map_err(|e| SimError::Codec(e.to_string()))?;
                components.insert(cid, value);
            }
            if index as usize >= nslots {
                return Err(SimError::Codec("entity index out of range".into()));
            }
            slots[index as usize] = Some(Entity {
                id: EntityId { index, generation },
                owner,
                components,
                spawned_at,
            });
        }
        let mut generations = Vec::with_capacity(nslots);
        for _ in 0..nslots {
            generations.push(reader.read_bits(16).map_err(err)? as u16);
        }
        self.free = (0..nslots as u32)
            .filter(|i| slots[*i as usize].is_none())
            .rev()
            .collect();
        self.slots = slots;
        self.generations = generations;
        self.tick = tick;
        self.rng = rng;
        self.inputs.clear();
        self.events.clear();
        Ok(())
    }

    /// Hash of the full state (what lockstep peers compare).
    pub fn hash(&self) -> [u8; 32] {
        *blake3::hash(&self.snapshot()).as_bytes()
    }
}

/// Convenience for games that keep a position vec3 in a component field.
pub fn vec3_of(value: &ComponentValue, field: usize) -> Option<[f32; 3]> {
    match value.fields.get(field) {
        Some(Value::Vec3(p)) => Some(*p),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn crafted_ids_and_snapshots_do_not_allocate() {
        let schema = net_wire::schema::Schema::parse("component T {\n  hp: u8\n}\n").unwrap();
        let mut frame = super::Frame::new(schema, super::FrameConfig::default(), 1);
        let huge = super::EntityId {
            index: u32::MAX - 1,
            generation: 0,
        };
        assert!(frame.ensure_entity(huge, None).is_err());
        assert_eq!(frame.entity_count(), 0);
        let mut writer = net_wire::bits::BitWriter::new();
        writer.write_bits(0, 32);
        writer.write_bits(1, 64);
        writer.write_bits(u64::from(u32::MAX), 32);
        writer.write_bits(0, 32);
        assert!(frame.restore(&writer.finish()).is_err());
        assert_eq!(frame.entity_count(), 0);
    }

    use super::*;

    fn schema() -> Schema {
        Schema::parse("component Transform {\n pos: vec3 @range(-1000, 1000, 0.01)\n rot: quat\n}\ncomponent Health {\n hp: u8\n}").unwrap()
    }

    fn transform(x: f32) -> ComponentValue {
        ComponentValue {
            fields: vec![
                Value::Vec3([x, 0.0, 0.0]),
                Value::Quat([0.0, 0.0, 0.0, 1.0]),
            ],
        }
    }

    #[test]
    fn spawn_despawn_generations() {
        let mut frame = Frame::new(
            schema(),
            FrameConfig {
                max_entities: 2,
                ..Default::default()
            },
            1,
        );
        let owned = frame.spawn(Some(ActorId(1))).unwrap();
        let free = frame.spawn(None).unwrap();
        assert_eq!(frame.spawn(None), Err(SimError::Full(2)));
        frame.despawn(owned).unwrap();
        assert_eq!(frame.despawn(owned), Err(SimError::NoEntity));
        let reused = frame.spawn(None).unwrap();
        assert_eq!(reused.index, owned.index);
        assert_ne!(reused.generation, owned.generation);
        assert!(frame.entity(owned).is_none());
        assert!(frame.entity(free).is_some() && frame.entity(reused).is_some());
        assert_eq!(EntityId::from_u64(reused.to_u64()), reused);
    }

    #[test]
    fn components_validate_against_schema() {
        let mut frame = Frame::new(schema(), FrameConfig::default(), 1);
        let entity = frame.spawn(None).unwrap();
        frame.set(entity, "Transform", transform(5.0)).unwrap();
        assert_eq!(
            vec3_of(frame.get(entity, "Transform").unwrap(), 0),
            Some([5.0, 0.0, 0.0])
        );
        assert!(matches!(
            frame.set(entity, "Health", transform(1.0)),
            Err(SimError::Codec(_))
        ));
        assert!(matches!(
            frame.set(entity, "Nope", transform(1.0)),
            Err(SimError::NoComponent(_))
        ));
    }

    #[test]
    fn inputs_fresh_then_repeat() {
        let mut frame = Frame::new(schema(), FrameConfig::default(), 1);
        let actor = ActorId(3);
        assert!(frame.push_input(Input {
            actor,
            tick: Tick(0),
            data: vec![1]
        }));
        assert!(frame.push_input(Input {
            actor,
            tick: Tick(2),
            data: vec![3]
        }));
        let mut seen = Vec::new();
        for _ in 0..4 {
            frame.advance(|fr| {
                let (data, fresh) = fr.input_for(actor);
                seen.push((data, fresh));
            });
        }
        assert_eq!(
            seen,
            vec![
                (vec![1], true),
                (vec![1], false),
                (vec![3], true),
                (vec![3], false)
            ]
        );
        assert!(!frame.push_input(Input {
            actor,
            tick: Tick(1),
            data: vec![9]
        }));
        assert_eq!(frame.tick, Tick(4));
    }

    #[test]
    fn snapshot_restore_hash() {
        let mut frame = Frame::new(schema(), FrameConfig::default(), 42);
        let e1 = frame.spawn(Some(ActorId(1))).unwrap();
        let e2 = frame.spawn(None).unwrap();
        frame.despawn(e2).unwrap();
        let e3 = frame.spawn(None).unwrap();
        frame.set(e1, "Transform", transform(1.25)).unwrap();
        frame
            .set(
                e3,
                "Health",
                ComponentValue {
                    fields: vec![Value::UInt(77)],
                },
            )
            .unwrap();
        frame.advance(|fr| {
            fr.rand_u32();
        });
        frame.raise(1, None, vec![]);
        let snap = frame.snapshot();
        let hash = frame.hash();

        let mut restored = Frame::new(schema(), FrameConfig::default(), 0);
        restored.restore(&snap).unwrap();
        assert_eq!(restored.tick, Tick(1));
        assert_eq!(restored.hash(), hash);
        assert_eq!(restored.entity_count(), 2);
        assert_eq!(restored.entity(e3).unwrap().components.len(), 1);
        assert_eq!(e3.index, e2.index); // e3 reused e2's slot
        assert_eq!(restored.spawn(None).unwrap().index, 2); // no free slots after restore
        assert_eq!(restored.rand_u32(), frame.rand_u32());
        restored.set(e1, "Transform", transform(2.0)).unwrap();
        assert_ne!(restored.hash(), hash);
    }
}
