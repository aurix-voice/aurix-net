//! Server -> client state replication for sync mode: per-client baselines, area-of-interest,
//! priority accumulation and a byte budget per tick. Output is a `Delta` packet the client
//! applies to its own `Frame`.
//!
//! Packet layout (bit-packed, after the 12-byte transport header):
//! ```text
//! u32 tick, u32 baseline_tick (0 = full), u16 n_despawn, n * u64 entity
//! u16 n_entities, for each: u64 entity, u1 owner?, [u16 owner], u8 n_components,
//!   for each: cid, u1 removed, [u1 full, body]
//! ```

use net_common::types::{ActorId, Tick};
use net_sim::{Entity, EntityId, Frame};
use net_wire::{BitReader, BitWriter, ComponentValue, Schema};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

#[derive(Debug, Clone)]
pub struct ReplicationConfig {
    /// AOI grid cell size in world units; entities within `radius` cells of the viewer are
    /// relevant. 0 disables AOI (everything relevant).
    pub aoi_cell: f32,
    pub aoi_radius: u32,
    /// Component name and field index holding the vec3 position used for AOI.
    pub position: Option<(String, usize)>,
    /// Max payload bytes per client per tick.
    pub budget_bytes: usize,
    /// Entities not updated for this many ticks get top priority regardless of distance.
    pub stale_after: u32,
}

impl Default for ReplicationConfig {
    fn default() -> Self {
        Self {
            aoi_cell: 32.0,
            aoi_radius: 2,
            position: Some(("Transform".into(), 0)),
            budget_bytes: 1100,
            stale_after: 30,
        }
    }
}

/// What one client believes about one entity.
#[derive(Debug, Clone, Default)]
struct Known {
    components: BTreeMap<u16, ComponentValue>,
    owner: Option<ActorId>,
    last_sent: u32,
    priority: f32,
}

/// Replication state for one connected client.
#[derive(Debug)]
pub struct ClientView {
    pub actor: ActorId,
    known: HashMap<EntityId, Known>,
    /// Entities the game forces into view (team mates, objectives) regardless of AOI.
    pub interest: BTreeSet<EntityId>,
    /// Entity whose position defines the viewpoint (the player's avatar).
    pub viewpoint: Option<EntityId>,
    pub last_tick: Option<Tick>,
}

impl ClientView {
    pub fn new(actor: ActorId) -> Self {
        Self {
            actor,
            known: HashMap::new(),
            interest: BTreeSet::new(),
            viewpoint: None,
            last_tick: None,
        }
    }

    pub fn known_count(&self) -> usize {
        self.known.len()
    }
}

pub struct Replicator {
    pub cfg: ReplicationConfig,
    pos_cid: Option<(u16, usize)>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ApplyError {
    #[error("truncated delta")]
    Truncated,
    #[error("unknown component {0}")]
    Component(u16),
    #[error("codec: {0}")]
    Codec(String),
}

impl Replicator {
    pub fn new(cfg: ReplicationConfig, schema: &Schema) -> Self {
        let pos_cid = cfg
            .position
            .as_ref()
            .and_then(|(name, idx)| schema.component(name).map(|c| (c.id, *idx)));
        Self { cfg, pos_cid }
    }

    fn position(&self, e: &Entity) -> Option<[f32; 3]> {
        let (cid, idx) = self.pos_cid?;
        net_sim::vec3_of(e.components.get(&cid)?, idx)
    }

    fn cell(&self, p: [f32; 3]) -> (i32, i32) {
        (
            (p[0] / self.cfg.aoi_cell).floor() as i32,
            (p[2] / self.cfg.aoi_cell).floor() as i32,
        )
    }

    /// Entities relevant to the viewer this tick: own entities, explicit interest, server
    /// entities without position, and everything within the AOI radius of the viewpoint.
    pub fn relevant(&self, frame: &Frame, view: &ClientView) -> Vec<EntityId> {
        let origin = view
            .viewpoint
            .and_then(|v| frame.entity(v))
            .and_then(|e| self.position(e))
            .map(|p| self.cell(p));
        let r = self.cfg.aoi_radius as i32;
        frame
            .entities()
            .filter(|e| {
                if e.owner == Some(view.actor)
                    || view.interest.contains(&e.id)
                    || self.cfg.aoi_cell <= 0.0
                {
                    return true;
                }
                match (self.position(e), origin) {
                    (Some(p), Some(o)) => {
                        let c = self.cell(p);
                        (c.0 - o.0).abs() <= r && (c.1 - o.1).abs() <= r
                    }
                    (None, _) => true,
                    (Some(_), None) => true,
                }
            })
            .map(|e| e.id)
            .collect()
    }

    /// Builds the delta for one client and advances its baseline for everything included.
    /// Entities that no longer exist or left the AOI are sent as despawns.
    pub fn build(&self, frame: &Frame, view: &mut ClientView) -> Vec<u8> {
        let tick = frame.tick.0;
        let relevant: Vec<EntityId> = self.relevant(frame, view);
        let rel_set: HashSet<EntityId> = relevant.iter().copied().collect();
        let origin = view
            .viewpoint
            .and_then(|v| frame.entity(v))
            .and_then(|e| self.position(e));

        let despawn: Vec<EntityId> = view
            .known
            .keys()
            .filter(|id| !rel_set.contains(id))
            .copied()
            .collect();
        for id in &despawn {
            view.known.remove(id);
        }

        // Priority: distance-based accumulator plus staleness; new entities first.
        let mut order: Vec<(f32, EntityId)> = relevant
            .iter()
            .map(|id| {
                let e = frame.entity(*id).unwrap();
                let k = view.known.entry(*id).or_insert_with(|| Known {
                    last_sent: tick.wrapping_sub(u32::MAX / 2),
                    ..Default::default()
                });
                let dist = match (origin, self.position(e)) {
                    (Some(o), Some(p)) => {
                        ((p[0] - o[0]).powi(2) + (p[1] - o[1]).powi(2) + (p[2] - o[2]).powi(2))
                            .sqrt()
                    }
                    _ => 0.0,
                };
                let changed = e.owner != k.owner || e.components != k.components;
                let age = tick.wrapping_sub(k.last_sent);
                k.priority += if changed {
                    1.0 / (1.0 + dist / self.cfg.aoi_cell.max(1.0))
                } else {
                    0.0
                };
                let p = if k.components.is_empty() && !e.components.is_empty() {
                    f32::MAX
                } else if age >= self.cfg.stale_after && changed {
                    1e6 + k.priority
                } else {
                    k.priority
                };
                (p, *id)
            })
            .collect();
        order.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        let full = view.last_tick.is_none();
        let mut w = BitWriter::new();
        w.write_bits(tick as u64, 32);
        w.write_bits(view.last_tick.map(|t| t.0 as u64 + 1).unwrap_or(0), 32);
        w.write_bits(despawn.len().min(65535) as u64, 16);
        for id in despawn.iter().take(65535) {
            w.write_bits(id.to_u64(), 48);
        }
        let count_pos = w.bit_len();
        w.write_bits(0, 16);
        let cid_bits = frame.schema.component_id_bits();
        let mut sent = 0u16;
        for (prio, id) in order {
            if prio <= 0.0 && !full {
                continue;
            }
            let e = frame.entity(id).unwrap();
            let k = view.known.get(&id).unwrap();
            let mut ew = BitWriter::new();
            ew.write_bits(id.to_u64(), 48);
            match e.owner {
                Some(a) => {
                    ew.write_bool(true);
                    ew.write_bits(a.0 as u64, 16);
                }
                None => ew.write_bool(false),
            }
            let mut comps: Vec<(u16, Option<&ComponentValue>)> = Vec::new();
            for (cid, v) in &e.components {
                if k.components.get(cid) != Some(v) {
                    comps.push((*cid, Some(v)));
                }
            }
            for cid in k.components.keys() {
                if !e.components.contains_key(cid) {
                    comps.push((*cid, None));
                }
            }
            if comps.is_empty() && e.owner == k.owner && !k.components.is_empty() {
                continue;
            }
            ew.write_bits(comps.len().min(255) as u64, 8);
            for (cid, v) in comps.iter().take(255) {
                ew.write_bits(*cid as u64, cid_bits);
                match v {
                    None => ew.write_bool(true),
                    Some(v) => {
                        ew.write_bool(false);
                        let c = frame.schema.by_id(*cid).unwrap();
                        match k.components.get(cid) {
                            Some(base) => {
                                ew.write_bool(false);
                                let _ = v.encode_delta(base, c, &mut ew);
                            }
                            None => {
                                ew.write_bool(true);
                                let _ = v.encode_full(c, &mut ew);
                            }
                        }
                    }
                }
            }
            if (w.bit_len() + ew.bit_len()).div_ceil(8) > self.cfg.budget_bytes && sent > 0 {
                break;
            }
            let nbits = ew.bit_len();
            append_bits(&mut w, &ew.finish(), nbits);
            let k = view.known.get_mut(&id).unwrap();
            k.components = e.components.clone();
            k.owner = e.owner;
            k.last_sent = tick;
            k.priority = 0.0;
            sent += 1;
            if sent == u16::MAX {
                break;
            }
        }
        let mut out = w.finish();
        out[count_pos / 8..count_pos / 8 + 2].copy_from_slice(&sent.to_be_bytes());
        view.last_tick = Some(frame.tick);
        out
    }

    /// Applies a delta to a client-side frame. Unknown entities are created; components are
    /// decoded against the client's current value (its baseline).
    pub fn apply(frame: &mut Frame, buf: &[u8]) -> Result<Tick, ApplyError> {
        let mut r = BitReader::new(buf);
        let rd = |r: &mut BitReader, n: u32| r.read_bits(n).map_err(|_| ApplyError::Truncated);
        let tick = Tick(rd(&mut r, 32)? as u32);
        let _baseline = rd(&mut r, 32)?;
        let nd = rd(&mut r, 16)? as usize;
        for _ in 0..nd {
            let id = EntityId::from_u64(rd(&mut r, 48)?);
            let _ = frame.despawn(id);
        }
        let n = rd(&mut r, 16)? as usize;
        let cid_bits = frame.schema.component_id_bits();
        for _ in 0..n {
            let id = EntityId::from_u64(rd(&mut r, 48)?);
            let owner = if r.read_bool().map_err(|_| ApplyError::Truncated)? {
                Some(ActorId(rd(&mut r, 16)? as u16))
            } else {
                None
            };
            frame.ensure_entity(id, owner);
            let nc = rd(&mut r, 8)? as usize;
            for _ in 0..nc {
                let cid = rd(&mut r, cid_bits)? as u16;
                let removed = r.read_bool().map_err(|_| ApplyError::Truncated)?;
                let c = frame
                    .schema
                    .by_id(cid)
                    .ok_or(ApplyError::Component(cid))?
                    .clone();
                let e = frame.entity_mut(id).unwrap();
                if removed {
                    e.components.remove(&cid);
                    continue;
                }
                let is_full = r.read_bool().map_err(|_| ApplyError::Truncated)?;
                let v = if is_full {
                    ComponentValue::decode_full(&c, &mut r)
                } else {
                    let base = e
                        .components
                        .get(&cid)
                        .cloned()
                        .unwrap_or_else(|| ComponentValue::zero(&c));
                    ComponentValue::decode_delta(&base, &c, &mut r)
                }
                .map_err(|e| ApplyError::Codec(e.to_string()))?;
                e.components.insert(cid, v);
            }
        }
        frame.tick = tick;
        Ok(tick)
    }
}

/// Concatenates a byte-padded sub-writer bit-exactly.
fn append_bits(w: &mut BitWriter, bytes: &[u8], nbits: usize) {
    let mut r = BitReader::new(bytes);
    let mut left = nbits;
    while left > 0 {
        let n = left.min(32) as u32;
        w.write_bits(r.read_bits(n).unwrap(), n);
        left -= n as usize;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use net_sim::FrameConfig;
    use net_wire::Value;

    fn schema() -> Schema {
        Schema::parse("component Transform {\n pos: vec3 @range(-1000, 1000, 0.01)\n}\ncomponent Health {\n hp: u8\n}").unwrap()
    }
    fn tf(x: f32, z: f32) -> ComponentValue {
        ComponentValue {
            fields: vec![Value::Vec3([x, 0.0, z])],
        }
    }
    fn hp(v: u64) -> ComponentValue {
        ComponentValue {
            fields: vec![Value::UInt(v)],
        }
    }
    fn pos(f: &Frame, id: EntityId) -> [f32; 3] {
        net_sim::vec3_of(f.get(id, "Transform").unwrap(), 0).unwrap()
    }

    #[test]
    fn full_then_delta_roundtrip() {
        let mut server = Frame::new(schema(), FrameConfig::default(), 1);
        let mut client = Frame::new(schema(), FrameConfig::default(), 1);
        let rep = Replicator::new(
            ReplicationConfig {
                aoi_cell: 0.0,
                ..Default::default()
            },
            &server.schema,
        );
        let a = server.spawn(Some(ActorId(1))).unwrap();
        let b = server.spawn(None).unwrap();
        server.set(a, "Transform", tf(1.0, 2.0)).unwrap();
        server.set(a, "Health", hp(100)).unwrap();
        server.set(b, "Transform", tf(-5.0, 0.0)).unwrap();
        let mut view = ClientView::new(ActorId(1));
        let d1 = rep.build(&server, &mut view);
        assert_eq!(Replicator::apply(&mut client, &d1).unwrap(), Tick(0));
        assert_eq!(client.entity_count(), 2);
        assert_eq!(client.entity(a).unwrap().owner, Some(ActorId(1)));
        assert_eq!(client.get(a, "Health"), Some(&hp(100)));

        // nothing changed -> header only
        server.advance(|_| {});
        let d2 = rep.build(&server, &mut view);
        assert_eq!(d2.len(), 12); // tick + baseline + two zero counts
        Replicator::apply(&mut client, &d2).unwrap();

        // move a, drop Health, despawn b
        server.get_mut(a, "Transform").unwrap().fields[0] = Value::Vec3([1.5, 0.0, 2.0]);
        server.remove_component(a, "Health").unwrap();
        server.despawn(b).unwrap();
        server.advance(|_| {});
        let d3 = rep.build(&server, &mut view);
        assert!(d3.len() < 40, "{}", d3.len()); // 1 despawn + 1 entity (vec3 delta, Health removed)
        assert_eq!(Replicator::apply(&mut client, &d3).unwrap(), Tick(2));
        assert_eq!(client.entity_count(), 1);
        assert!(client.get(a, "Health").is_none());
        assert!((pos(&client, a)[0] - 1.5).abs() < 0.01);
        assert_eq!(view.known_count(), 1);
    }

    #[test]
    fn aoi_filters_and_despawns_on_leave() {
        let mut server = Frame::new(schema(), FrameConfig::default(), 1);
        let mut client = Frame::new(schema(), FrameConfig::default(), 1);
        let rep = Replicator::new(
            ReplicationConfig {
                aoi_cell: 10.0,
                aoi_radius: 1,
                ..Default::default()
            },
            &server.schema,
        );
        let me = server.spawn(Some(ActorId(7))).unwrap();
        server.set(me, "Transform", tf(0.0, 0.0)).unwrap();
        let near = server.spawn(None).unwrap();
        server.set(near, "Transform", tf(15.0, 0.0)).unwrap();
        let far = server.spawn(None).unwrap();
        server.set(far, "Transform", tf(100.0, 0.0)).unwrap();
        let global = server.spawn(None).unwrap();
        server.set(global, "Health", hp(1)).unwrap();
        let mut view = ClientView::new(ActorId(7));
        view.viewpoint = Some(me);
        Replicator::apply(&mut client, &rep.build(&server, &mut view)).unwrap();
        assert!(client.entity(near).is_some() && client.entity(global).is_some());
        assert!(client.entity(far).is_none());
        // near walks away
        server.get_mut(near, "Transform").unwrap().fields[0] = Value::Vec3([50.0, 0.0, 0.0]);
        server.advance(|_| {});
        Replicator::apply(&mut client, &rep.build(&server, &mut view)).unwrap();
        assert!(client.entity(near).is_none());
        // explicit interest overrides AOI
        view.interest.insert(far);
        server.advance(|_| {});
        Replicator::apply(&mut client, &rep.build(&server, &mut view)).unwrap();
        assert!(client.entity(far).is_some());
    }

    #[test]
    fn budget_spreads_over_ticks() {
        let mut server = Frame::new(schema(), FrameConfig::default(), 1);
        let mut client = Frame::new(schema(), FrameConfig::default(), 1);
        let rep = Replicator::new(
            ReplicationConfig {
                aoi_cell: 0.0,
                budget_bytes: 200,
                ..Default::default()
            },
            &server.schema,
        );
        for i in 0..100 {
            let e = server.spawn(None).unwrap();
            server.set(e, "Transform", tf(i as f32, 0.0)).unwrap();
        }
        let mut view = ClientView::new(ActorId(1));
        let mut ticks = 0;
        while client.entity_count() < 100 {
            let d = rep.build(&server, &mut view);
            assert!(d.len() <= 200 + 20, "{}", d.len());
            Replicator::apply(&mut client, &d).unwrap();
            server.advance(|_| {});
            ticks += 1;
            assert!(ticks < 50);
        }
        assert!(ticks > 3);
        for e in server.entities() {
            assert_eq!(pos(&client, e.id)[0], pos(&server, e.id)[0]);
        }
    }
}
