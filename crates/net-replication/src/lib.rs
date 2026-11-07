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
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

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

/// One delta we sent: whose baselines it advanced, so a lost delta can be undone once the
/// client's ack history shows it never arrived.
#[derive(Debug)]
struct Sent {
    tick: u32,
    entities: Vec<EntityId>,
    despawned: Vec<EntityId>,
}

/// Sent deltas remembered per client while waiting for acks.
const SENT_LOG: usize = 128;

/// Replication state for one connected client.
#[derive(Debug)]
pub struct ClientView {
    pub actor: ActorId,
    known: HashMap<EntityId, Known>,
    sent: VecDeque<Sent>,
    acked_any: bool,
    /// Despawns from lost deltas, repeated in the next delta.
    pending_despawn: BTreeSet<EntityId>,
    /// Deltas the client lost (reported or assumed); drained by the room for metrics.
    pub lost_deltas: u64,
    /// Ticks where the byte budget deferred relevant entities; drained for metrics.
    pub starved: u64,
    /// Entities the game forces into view (team mates, objectives) regardless of AOI.
    pub interest: BTreeSet<EntityId>,
    /// Entity whose position defines the viewpoint (the player's avatar).
    pub viewpoint: Option<EntityId>,
    /// Extra viewpoints (camera, scope target); at most `MAX_VIEWPOINTS` in total are used.
    pub extra_viewpoints: Vec<EntityId>,
    pub last_tick: Option<Tick>,
}

impl ClientView {
    pub fn new(actor: ActorId) -> Self {
        Self {
            actor,
            known: HashMap::new(),
            sent: VecDeque::new(),
            acked_any: false,
            pending_despawn: BTreeSet::new(),
            lost_deltas: 0,
            starved: 0,
            interest: BTreeSet::new(),
            viewpoint: None,
            extra_viewpoints: Vec::new(),
            last_tick: None,
        }
    }

    /// Forget everything so the next build is a full snapshot (client asked to resync).
    pub fn reset(&mut self) {
        self.known.clear();
        self.sent.clear();
        self.pending_despawn.clear();
        self.last_tick = None;
    }

    /// Client ack: `last` is the newest delta tick it received and bit `i` of `bits` says it
    /// also received tick `last - 1 - i`. Every remembered delta at or before `last` is
    /// resolved; a lost one has its entities forgotten (the next delta re-sends them in full,
    /// since later deltas were encoded against state the client never got) and its despawns
    /// queued again. Returns how many deltas were lost.
    pub fn ack(&mut self, last: u32, bits: u32) -> u32 {
        self.acked_any = true;
        let mut lost = 0;
        while let Some(s) = self.sent.front() {
            let d = last.wrapping_sub(s.tick);
            if d >= 1 << 31 {
                break;
            }
            let s = self.sent.pop_front().unwrap();
            let received = d == 0 || (d <= 32 && (bits >> (d - 1)) & 1 == 1);
            if !received {
                lost += 1;
                self.forget(s);
            }
        }
        self.lost_deltas += lost as u64;
        lost
    }

    fn forget(&mut self, s: Sent) {
        for id in s.entities {
            self.known.remove(&id);
        }
        self.pending_despawn.extend(s.despawned);
    }

    pub fn known_count(&self) -> usize {
        self.known.len()
    }
}

pub struct Replicator {
    pub cfg: ReplicationConfig,
    pos_cid: Option<(u16, usize)>,
    /// Component ids flagged `@owner_only` / `@global` in the schema.
    owner_only: HashSet<u16>,
    global: HashSet<u16>,
}

/// Viewpoints per client (`viewpoint` + `extra_viewpoints`).
pub const MAX_VIEWPOINTS: usize = 4;

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
        Self {
            cfg,
            pos_cid,
            owner_only: schema
                .components
                .iter()
                .filter(|c| c.owner_only)
                .map(|c| c.id)
                .collect(),
            global: schema
                .components
                .iter()
                .filter(|c| c.global)
                .map(|c| c.id)
                .collect(),
        }
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
        let origins: Vec<(i32, i32)> = view
            .viewpoint
            .iter()
            .chain(view.extra_viewpoints.iter())
            .take(MAX_VIEWPOINTS)
            .filter_map(|v| frame.entity(*v))
            .filter_map(|e| self.position(e))
            .map(|p| self.cell(p))
            .collect();
        let r = self.cfg.aoi_radius as i32;
        frame
            .entities()
            .filter(|e| {
                if e.owner == Some(view.actor)
                    || view.interest.contains(&e.id)
                    || self.cfg.aoi_cell <= 0.0
                    || e.components.keys().any(|c| self.global.contains(c))
                {
                    return true;
                }
                match self.position(e) {
                    None => true,
                    Some(_) if origins.is_empty() => true,
                    Some(p) => {
                        // Hysteresis: something already in view stays until one cell further.
                        let r = if view.known.contains_key(&e.id) {
                            r + 1
                        } else {
                            r
                        };
                        let c = self.cell(p);
                        origins
                            .iter()
                            .any(|o| (c.0 - o.0).abs() <= r && (c.1 - o.1).abs() <= r)
                    }
                }
            })
            .map(|e| e.id)
            .collect()
    }

    /// Components of `e` this viewer may see (`@owner_only` hides from non-owners).
    fn visible<'a>(
        &self,
        e: &'a Entity,
        viewer: ActorId,
    ) -> std::borrow::Cow<'a, BTreeMap<u16, ComponentValue>> {
        if self.owner_only.is_empty() || e.owner == Some(viewer) {
            return std::borrow::Cow::Borrowed(&e.components);
        }
        std::borrow::Cow::Owned(
            e.components
                .iter()
                .filter(|(c, _)| !self.owner_only.contains(c))
                .map(|(c, v)| (*c, v.clone()))
                .collect(),
        )
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

        let mut despawn: Vec<EntityId> = view
            .known
            .keys()
            .filter(|id| !rel_set.contains(id))
            .copied()
            .collect();
        for id in &despawn {
            view.known.remove(id);
        }
        for id in std::mem::take(&mut view.pending_despawn) {
            if !rel_set.contains(&id) && !despawn.contains(&id) {
                despawn.push(id);
            }
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
                let changed = e.owner != k.owner || *self.visible(e, view.actor) != k.components;
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
        let mut sent_ids = Vec::new();
        for (prio, id) in order {
            if prio <= 0.0 && !full {
                continue;
            }
            let e = frame.entity(id).unwrap();
            let vis = self.visible(e, view.actor);
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
            for (cid, v) in vis.iter() {
                if k.components.get(cid) != Some(v) {
                    comps.push((*cid, Some(v)));
                }
            }
            for cid in k.components.keys() {
                if !vis.contains_key(cid) {
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
                view.starved += 1;
                break;
            }
            let nbits = ew.bit_len();
            append_bits(&mut w, &ew.finish(), nbits);
            let vis = vis.into_owned();
            let k = view.known.get_mut(&id).unwrap();
            k.components = vis;
            k.owner = e.owner;
            k.last_sent = tick;
            k.priority = 0.0;
            sent_ids.push(id);
            sent += 1;
            if sent == u16::MAX {
                break;
            }
        }
        let mut out = w.finish();
        out[count_pos / 8..count_pos / 8 + 2].copy_from_slice(&sent.to_be_bytes());
        view.last_tick = Some(frame.tick);
        view.sent.push_back(Sent {
            tick,
            entities: sent_ids,
            despawned: despawn,
        });
        if view.sent.len() > SENT_LOG {
            let s = view.sent.pop_front().unwrap();
            // A client that acks never let a delta get this old: assume it was lost. Clients
            // that do not ack at all (older SDKs) just get the pre-ack behaviour.
            if view.acked_any {
                view.lost_deltas += 1;
                view.forget(s);
            }
        }
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

/// Reads `(tick, baseline_tick)` from a delta payload (without the kind byte).
pub fn delta_header(buf: &[u8]) -> Option<(u32, u32)> {
    if buf.len() < 8 {
        return None;
    }
    Some((
        u32::from_be_bytes(buf[0..4].try_into().unwrap()),
        u32::from_be_bytes(buf[4..8].try_into().unwrap()),
    ))
}

/// Client side: which delta ticks arrived, in the shape the server's `ClientView::ack` wants.
#[derive(Debug, Default)]
pub struct AckTracker {
    last: Option<u32>,
    bits: u32,
    /// Deltas the server sent (per its `baseline_tick` header) that never arrived.
    pub gaps: u64,
}

impl AckTracker {
    /// Records a received delta header. Returns true when the delta before it was missed.
    pub fn on_delta(&mut self, tick: u32, baseline_tick: u32) -> bool {
        let Some(l) = self.last else {
            self.last = Some(tick);
            return false;
        };
        let d = tick.wrapping_sub(l);
        if d == 0 || d >= 1 << 31 {
            // duplicate or late arrival: fill its slot in the history
            let back = l.wrapping_sub(tick);
            if (1..=32).contains(&back) {
                self.bits |= 1 << (back - 1);
            }
            return false;
        }
        let gap = baseline_tick != 0 && l != baseline_tick.wrapping_sub(1);
        if gap {
            self.gaps += 1;
        }
        self.bits = if d >= 32 {
            0
        } else {
            (self.bits << d) | (1 << (d - 1))
        };
        self.last = Some(tick);
        gap
    }

    /// `(last, bits)` for the ack frame; None before the first delta.
    pub fn ack(&self) -> Option<(u32, u32)> {
        self.last.map(|l| (l, self.bits))
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

    #[test]
    fn ack_tracker_bits() {
        let mut t = AckTracker::default();
        assert!(!t.on_delta(0, 0));
        assert!(!t.on_delta(1, 1));
        assert!(t.on_delta(3, 3)); // tick 2 missing
        assert_eq!(t.ack(), Some((3, 0b110)));
        assert!(!t.on_delta(2, 2)); // late arrival fills the hole
        assert_eq!(t.ack(), Some((3, 0b111)));
        assert_eq!(t.gaps, 1);
    }

    #[test]
    fn lost_deltas_are_repaired_after_ack() {
        let mut server = Frame::new(schema(), FrameConfig::default(), 1);
        let mut client = Frame::new(schema(), FrameConfig::default(), 1);
        let rep = Replicator::new(
            ReplicationConfig {
                aoi_cell: 0.0,
                ..Default::default()
            },
            &server.schema,
        );
        let a = server.spawn(None).unwrap();
        let b = server.spawn(None).unwrap();
        server.set(a, "Transform", tf(1.0, 0.0)).unwrap();
        server.set(b, "Transform", tf(2.0, 0.0)).unwrap();
        let mut view = ClientView::new(ActorId(1));
        let mut tracker = AckTracker::default();
        let d0 = rep.build(&server, &mut view);
        tracker.on_delta(0, 0);
        Replicator::apply(&mut client, &d0).unwrap();
        // tick 1: a moves (lost); tick 2: b despawns (lost)
        server.get_mut(a, "Transform").unwrap().fields[0] = Value::Vec3([5.0, 0.0, 0.0]);
        server.advance(|_| {});
        let _lost1 = rep.build(&server, &mut view);
        server.despawn(b).unwrap();
        server.advance(|_| {});
        let _lost2 = rep.build(&server, &mut view);
        // tick 3 arrives: nothing changed, but the client notices the hole
        server.advance(|_| {});
        let d3 = rep.build(&server, &mut view);
        let (t, base) = delta_header(&d3).unwrap();
        assert!(tracker.on_delta(t, base));
        Replicator::apply(&mut client, &d3).unwrap();
        assert_eq!(client.entity_count(), 2);
        assert_eq!(pos(&client, a)[0], 1.0);
        let (last, bits) = tracker.ack().unwrap();
        assert_eq!(view.ack(last, bits), 2);
        assert_eq!(view.lost_deltas, 2);
        // next delta repairs both: a in full, b despawned again
        server.advance(|_| {});
        let d4 = rep.build(&server, &mut view);
        Replicator::apply(&mut client, &d4).unwrap();
        assert_eq!(client.entity_count(), 1);
        assert_eq!(pos(&client, a)[0], 5.0);
        assert_eq!(view.known_count(), 1);
        // everything acked -> nothing more to repair
        tracker.on_delta(4, 4);
        let (last, bits) = tracker.ack().unwrap();
        assert_eq!(view.ack(last, bits), 0);
    }

    #[test]
    fn owner_only_and_global_flags() {
        let sch = Schema::parse(
            "component Transform {\n pos: vec3 @range(-1000, 1000, 0.01)\n}\ncomponent Inv @owner_only {\n n: u8\n}\ncomponent Score @global {\n s: u8\n}",
        )
        .unwrap();
        let mut server = Frame::new(sch.clone(), FrameConfig::default(), 1);
        let mut owner = Frame::new(sch.clone(), FrameConfig::default(), 1);
        let mut other = Frame::new(sch.clone(), FrameConfig::default(), 1);
        let rep = Replicator::new(
            ReplicationConfig {
                aoi_cell: 10.0,
                aoi_radius: 1,
                ..Default::default()
            },
            &server.schema,
        );
        let me = server.spawn(Some(ActorId(1))).unwrap();
        server.set(me, "Transform", tf(0.0, 0.0)).unwrap();
        server.set(me, "Inv", hp(7)).unwrap();
        let you = server.spawn(Some(ActorId(2))).unwrap();
        server.set(you, "Transform", tf(5.0, 0.0)).unwrap();
        let score = server.spawn(None).unwrap();
        server.set(score, "Transform", tf(900.0, 0.0)).unwrap();
        server.set(score, "Score", hp(3)).unwrap();
        let mut v1 = ClientView::new(ActorId(1));
        v1.viewpoint = Some(me);
        let mut v2 = ClientView::new(ActorId(2));
        v2.viewpoint = Some(you);
        Replicator::apply(&mut owner, &rep.build(&server, &mut v1)).unwrap();
        Replicator::apply(&mut other, &rep.build(&server, &mut v2)).unwrap();
        assert_eq!(owner.get(me, "Inv"), Some(&hp(7)));
        assert!(other.entity(me).is_some() && other.get(me, "Inv").is_none());
        // far away, but @global
        assert_eq!(other.get(score, "Score"), Some(&hp(3)));
        // a change to the hidden component sends nothing to the non-owner
        server.get_mut(me, "Inv").unwrap().fields[0] = Value::UInt(8);
        server.advance(|_| {});
        let d = rep.build(&server, &mut v2);
        assert_eq!(d.len(), 12);
        let d = rep.build(&server, &mut v1);
        assert!(d.len() > 12);
        Replicator::apply(&mut owner, &d).unwrap();
        assert_eq!(owner.get(me, "Inv"), Some(&hp(8)));
    }

    #[test]
    fn aoi_hysteresis_and_extra_viewpoints() {
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
        let edge = server.spawn(None).unwrap();
        server.set(edge, "Transform", tf(15.0, 0.0)).unwrap();
        let scope = server.spawn(None).unwrap();
        server.set(scope, "Transform", tf(200.0, 0.0)).unwrap();
        let near_scope = server.spawn(None).unwrap();
        server.set(near_scope, "Transform", tf(205.0, 0.0)).unwrap();
        let mut view = ClientView::new(ActorId(7));
        view.viewpoint = Some(me);
        Replicator::apply(&mut client, &rep.build(&server, &mut view)).unwrap();
        assert!(client.entity(edge).is_some() && client.entity(near_scope).is_none());
        // one cell past the radius: still in view (hysteresis)
        server.get_mut(edge, "Transform").unwrap().fields[0] = Value::Vec3([25.0, 0.0, 0.0]);
        server.advance(|_| {});
        Replicator::apply(&mut client, &rep.build(&server, &mut view)).unwrap();
        assert!(client.entity(edge).is_some());
        // two cells past: gone
        server.get_mut(edge, "Transform").unwrap().fields[0] = Value::Vec3([35.0, 0.0, 0.0]);
        server.advance(|_| {});
        Replicator::apply(&mut client, &rep.build(&server, &mut view)).unwrap();
        assert!(client.entity(edge).is_none());
        // a second viewpoint pulls in what is around it
        view.extra_viewpoints.push(scope);
        server.advance(|_| {});
        Replicator::apply(&mut client, &rep.build(&server, &mut view)).unwrap();
        assert!(client.entity(near_scope).is_some());
    }
}
