//! Lockstep mode: the server is the clock and the input relay. Every tick it bundles the
//! inputs that arrived for that tick and fans the bundle out to everyone; clients run the
//! deterministic simulation themselves (net-rollback) and report state hashes, which the
//! server compares to flag desyncs. The server keeps the canonical input journal (replays).
//!
//! Frames on the unreliable channel (first byte = kind):
//! ```text
//! 0x10 Input        client->server  u32 first_tick, u8 n, n x (u8 len, bytes)   (same as sync)
//! 0x20 InputBundle  server->client  u32 tick, u8 n, n x (u16 actor, u8 len, bytes)
//! 0x21 Hash         client->server  u32 tick, u64 hash
//! ```
//! Bundles for the last `BUNDLE_REDUNDANCY` ticks are re-sent each tick, so a lost datagram
//! costs nothing unless several in a row disappear.
//!
//! Late join / rejoin (reliable-ordered channel):
//! ```text
//! 0x23 CheckpointReq server->master  u32 tick            "upload your confirmed state"
//! 0x22 Checkpoint    master->server  u32 tick, u64 hash, bytes state (<= 60 KiB)
//! 0x24 Catchup       server->joiner  u32 tick, u64 hash, u32 len, bytes state
//! 0x20 InputBundle   server->joiner  (same layout as live bundles) ticks tick..now-1
//! ```
//! When a data link attaches to a room that is past tick 0 the server sends `Catchup`
//! (the newest master checkpoint, or tick 0 with an empty state = "start from the seed")
//! followed by every journal bundle up to the current tick, then the live stream continues.
//! The master is asked for a checkpoint every `CHECKPOINT_EVERY_SECS`, and immediately when
//! a joiner had to replay from a stale one, so catch-up stays bounded without any host
//! migration or peer-to-peer state transfer.

use crate::sync::GameLogic;
use bytes::{Buf, BufMut, Bytes, BytesMut};
use net_common::types::{ActorId, Tick};
use net_rollback::{hash_bytes, Journal, TickInputs};
use net_sim::{EntityId, Frame, FrameConfig};
use net_wire::Schema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

/// Everything another node needs to continue this room: the newest master checkpoint and
/// the journal from that tick on. Stored in Redis every few seconds (see `ws::run_lockstep`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockstepSnapshot {
    pub hz: u16,
    pub seed: u64,
    pub tick: u32,
    pub checkpoint: Option<(u32, u64, Vec<u8>)>,
    pub journal_from: u32,
    pub journal: Vec<TickInputs>,
}

pub const KIND_INPUT: u8 = crate::sync::KIND_INPUT;
pub const KIND_BUNDLE: u8 = 0x20;
pub const KIND_HASH: u8 = 0x21;
pub const KIND_CHECKPOINT: u8 = 0x22;
pub const KIND_CHECKPOINT_REQ: u8 = 0x23;
pub const KIND_CATCHUP: u8 = 0x24;
pub const BUNDLE_REDUNDANCY: usize = 3;
pub const CHECKPOINT_EVERY_SECS: u32 = 5;
pub const MAX_CHECKPOINT_BYTES: usize = 60 * 1024;
/// Reliable catch-up frames are split at about this size.
const CATCHUP_CHUNK_BYTES: usize = 32 * 1024;

/// Deterministic state the master uploaded; `tick` is the tick the state is *entering*
/// (all inputs of earlier ticks applied), i.e. `Rollback::current_tick()` when captured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkpoint {
    pub tick: u32,
    pub hash: u64,
    pub state: Bytes,
}
/// Inputs may arrive this many ticks ahead of the server clock.
pub const MAX_AHEAD: u32 = 16;

pub struct LockstepRoom {
    pub hz: u16,
    /// Next tick to be bundled.
    pub tick: Tick,
    pub actors: BTreeMap<ActorId, ()>,
    pending: BTreeMap<u32, TickInputs>,
    pub journal: Journal,
    recent: VecDeque<Bytes>,
    /// Hash reports per tick: actor -> hash.
    hashes: BTreeMap<u32, HashMap<ActorId, u64>>,
    pub desyncs: Vec<(u32, ActorId)>,
    pub seed: u64,
    /// Current master (set by the host loop); only its checkpoints are accepted.
    pub master: Option<ActorId>,
    pub checkpoint: Option<Checkpoint>,
    last_checkpoint_req: Option<u32>,
    want_checkpoint: bool,
    /// Desyncs per actor this match; at `UNTRUSTED_AFTER` the actor is reported untrusted.
    strikes: HashMap<ActorId, u8>,
    /// Watch-only actors: get the input stream `spectator_delay` ticks late, send nothing.
    spectators: BTreeSet<ActorId>,
    past: VecDeque<Bytes>,
    pub spectator_delay: u32,
    /// Server-stepped simulation (room prop `logic`): its hash is the truth clients are
    /// compared against, and checkpoints come from it instead of the master.
    sim: Option<ServerSim>,
    server_hashes: BTreeMap<u32, u64>,
    /// Actors that have appeared in a bundle (see `tick`).
    seen: BTreeSet<ActorId>,
}

/// Ticks between server state hashes; matches `RollbackConfig::default().hash_every`.
pub const SERVER_HASH_EVERY: u32 = 10;
/// Journal ticks kept in memory (4 h at 60 Hz). Beyond that the oldest ticks before the last
/// checkpoint are dropped: late join and failover still work (checkpoint + tail), the replay
/// of such a match starts at `from_tick`.
pub const MAX_JOURNAL_TICKS: usize = 4 * 3600 * 60;

/// The game logic stepped on the node over the canonical input journal. Rule shared with
/// clients: an actor's avatar is spawned (`on_join`) at the first tick it appears in a bundle;
/// the hash is `hash_bytes(frame.snapshot())` after the tick's inputs were applied.
pub struct ServerSim {
    pub frame: Frame,
    pub logic: Box<dyn GameLogic>,
    avatars: BTreeMap<ActorId, Option<EntityId>>,
}

impl ServerSim {
    pub fn new(frame: Frame, logic: Box<dyn GameLogic>) -> Self {
        Self {
            frame,
            logic,
            avatars: BTreeMap::new(),
        }
    }

    pub fn step(&mut self, inputs: &TickInputs) {
        for a in inputs.keys() {
            if !self.avatars.contains_key(a) {
                // After a restore the avatar already exists in the frame: re-link, do not spawn.
                let existing = self
                    .frame
                    .entities()
                    .find(|e| e.owner == Some(*a))
                    .map(|e| e.id);
                let av = existing.or_else(|| self.logic.on_join(&mut self.frame, *a));
                self.avatars.insert(*a, av);
            }
        }
        let step: Vec<(ActorId, Option<EntityId>, Vec<u8>, bool)> = self
            .avatars
            .iter()
            .map(|(a, av)| {
                (
                    *a,
                    *av,
                    inputs.get(a).cloned().unwrap_or_default(),
                    inputs.contains_key(a),
                )
            })
            .collect();
        let logic = &mut self.logic;
        self.frame.advance(|f| logic.step(f, &step));
    }

    pub fn hash(&self) -> u64 {
        hash_bytes(&self.frame.snapshot())
    }
}

/// Desyncs after which an actor's results should not be trusted (its hash keeps disagreeing).
pub const UNTRUSTED_AFTER: u8 = 3;

impl LockstepRoom {
    pub fn new(hz: u16, seed: u64) -> Self {
        Self {
            hz: hz.clamp(8, 128),
            tick: Tick(0),
            actors: BTreeMap::new(),
            pending: BTreeMap::new(),
            journal: Journal::default(),
            recent: VecDeque::new(),
            hashes: BTreeMap::new(),
            desyncs: Vec::new(),
            seed,
            master: None,
            checkpoint: None,
            last_checkpoint_req: None,
            want_checkpoint: false,
            strikes: HashMap::new(),
            spectators: BTreeSet::new(),
            past: VecDeque::new(),
            spectator_delay: hz.clamp(8, 128) as u32 * 3,
            sim: None,
            server_hashes: BTreeMap::new(),
            seen: BTreeSet::new(),
        }
    }

    /// Runs `logic` on the node from the last checkpoint (or tick 0) through the journal to the
    /// current tick; from here on hashes and checkpoints are the server's.
    pub fn attach_sim(&mut self, schema: Schema, logic: Box<dyn GameLogic>) -> Result<(), String> {
        let frame = Frame::new(
            schema,
            FrameConfig {
                tick_hz: self.hz,
                ..Default::default()
            },
            self.seed,
        );
        let mut sim = ServerSim::new(frame, logic);
        let start = match &self.checkpoint {
            Some(c) => {
                sim.frame.restore(&c.state).map_err(|e| e.to_string())?;
                // Every owned entity is somebody's avatar: keep stepping them like before.
                let owned: Vec<(ActorId, EntityId)> = sim
                    .frame
                    .entities()
                    .filter_map(|e| e.owner.map(|o| (o, e.id)))
                    .collect();
                for (o, id) in owned {
                    sim.avatars.entry(o).or_insert(Some(id));
                }
                c.tick
            }
            None if self.journal.tick(Tick(0)).is_some() || self.tick.0 == 0 => 0,
            None => return Err("no checkpoint and journal does not start at 0".into()),
        };
        let empty = TickInputs::new();
        for t in start..self.tick.0 {
            sim.step(self.journal.tick(Tick(t)).unwrap_or(&empty));
        }
        self.sim = Some(sim);
        self.server_hashes.clear();
        Ok(())
    }

    pub fn has_sim(&self) -> bool {
        self.sim.is_some()
    }

    pub fn server_hash(&self, tick: u32) -> Option<u64> {
        self.server_hashes.get(&tick).copied()
    }

    /// Delay of the spectator stream in seconds (default 3, against stream sniping).
    pub fn with_spectator_delay(mut self, secs: u32) -> Self {
        self.spectator_delay = self.hz as u32 * secs;
        self
    }

    pub fn spectators(&self) -> Vec<ActorId> {
        self.spectators.iter().copied().collect()
    }

    /// The datagram spectators should get now: the one from `spectator_delay` ticks ago.
    pub fn spectator_datagram(&self) -> Option<Bytes> {
        (self.past.len() > self.spectator_delay as usize)
            .then(|| self.past.front().cloned())
            .flatten()
    }

    /// Records a desync for `actor`; returns its strike count so far.
    pub fn strike(&mut self, actor: ActorId) -> u8 {
        let n = self.strikes.entry(actor).or_default();
        *n = n.saturating_add(1);
        *n
    }

    pub fn is_untrusted(&self, actor: ActorId) -> bool {
        self.strikes
            .get(&actor)
            .is_some_and(|n| *n >= UNTRUSTED_AFTER)
    }

    /// The bundle produced by the last [`tick`]: one input-journal entry for Redis, so a node
    /// that adopts this room after a crash loses no inputs between checkpoints.
    pub fn last_bundle(&self) -> Option<Bytes> {
        self.recent.back().cloned()
    }

    /// Applies journal entries (from `last_bundle`) stored after the snapshot this room was
    /// restored from: inputs go into the journal and the clock moves past them. Entries at or
    /// before the current tick are already covered and skipped. Returns ticks applied.
    pub fn apply_journal(&mut self, entries: &[Vec<u8>]) -> u32 {
        let mut n = 0;
        for e in entries {
            let Some((t, inputs)) = Self::decode_bundle(e) else {
                continue;
            };
            if t.0.wrapping_sub(self.tick.0) >= 1 << 31 {
                continue;
            }
            if t != self.tick {
                // A gap means journal batches were lost; everything after it is unusable.
                tracing::warn!(
                    expected = self.tick.0,
                    got = t.0,
                    "journal gap, stopping replay"
                );
                break;
            }
            for (a, d) in inputs {
                self.journal.record(t, a, d);
            }
            self.tick = Tick(t.0.wrapping_add(1));
            n += 1;
        }
        n
    }

    /// Parses one bundle as produced by `encode_bundle`.
    pub fn decode_bundle(mut b: &[u8]) -> Option<(Tick, TickInputs)> {
        if b.len() < 6 || b.get_u8() != KIND_BUNDLE {
            return None;
        }
        let t = b.get_u32();
        let k = b.get_u8() as usize;
        let mut inputs = TickInputs::new();
        for _ in 0..k {
            if b.len() < 3 {
                return None;
            }
            let a = ActorId(b.get_u16());
            let l = b.get_u8() as usize;
            if b.len() < l {
                return None;
            }
            inputs.insert(a, b[..l].to_vec());
            b.advance(l);
        }
        Some((Tick(t), inputs))
    }

    pub fn snapshot(&self) -> LockstepSnapshot {
        let from = self.checkpoint.as_ref().map(|c| c.tick).unwrap_or(0);
        LockstepSnapshot {
            hz: self.hz,
            seed: self.seed,
            tick: self.tick.0,
            checkpoint: self
                .checkpoint
                .as_ref()
                .map(|c| (c.tick, c.hash, c.state.to_vec())),
            journal_from: from,
            journal: self.journal.tail(from),
        }
    }

    /// Re-hosts a room on this node: the clock continues from the snapshot tick, late
    /// joiners catch up from the stored checkpoint + journal tail as usual.
    pub fn restore(snap: LockstepSnapshot) -> Self {
        let mut r = Self::new(snap.hz, snap.seed);
        r.tick = Tick(snap.tick);
        r.checkpoint = snap.checkpoint.map(|(tick, hash, state)| Checkpoint {
            tick,
            hash,
            state: Bytes::from(state),
        });
        r.journal = Journal::from_parts(snap.journal_from, snap.journal);
        r.want_checkpoint = true;
        r
    }

    fn checkpoint_every(&self) -> u32 {
        (self.hz as u32 * CHECKPOINT_EVERY_SECS).max(1)
    }

    /// Called once per tick after [`tick`]: a `CheckpointReq` frame for the master, if due.
    pub fn checkpoint_request(&mut self) -> Option<Bytes> {
        if self.sim.is_some() {
            return None;
        }
        self.master?;
        let t = self.tick.0;
        let periodic = t > 0 && t.is_multiple_of(self.checkpoint_every());
        let urgent = self.want_checkpoint
            && self
                .last_checkpoint_req
                .is_none_or(|r| t.wrapping_sub(r) >= self.hz as u32);
        if !(periodic || urgent) {
            return None;
        }
        self.last_checkpoint_req = Some(t);
        let mut b = BytesMut::with_capacity(5);
        b.put_u8(KIND_CHECKPOINT_REQ);
        b.put_u32(t);
        Some(b.freeze())
    }

    /// A data link attached (join or rejoin) while the room is running: everything the
    /// client needs to reach the current tick, as reliable-ordered frames. Empty at tick 0.
    pub fn on_late_join(&mut self) -> Vec<Bytes> {
        self.catchup_until(self.tick.0)
    }

    /// Catch-up for a joining player, or for a spectator only up to the delayed tick.
    pub fn catchup_for(&mut self, spectator: bool) -> Vec<Bytes> {
        let end = if spectator {
            self.tick.0.saturating_sub(self.spectator_delay)
        } else {
            self.tick.0
        };
        self.catchup_until(end)
    }

    fn catchup_until(&mut self, end: u32) -> Vec<Bytes> {
        if end == 0 {
            return Vec::new();
        }
        let (from, hash, state) = match &self.checkpoint {
            Some(c) => (c.tick, c.hash, c.state.clone()),
            None => (0, 0, Bytes::new()),
        };
        if self.tick.0.saturating_sub(from) > 2 * self.checkpoint_every() {
            self.want_checkpoint = true;
        }
        let mut frames = Vec::new();
        let mut b = BytesMut::with_capacity(17 + state.len());
        b.put_u8(KIND_CATCHUP);
        b.put_u32(from);
        b.put_u64(hash);
        b.put_u32(state.len() as u32);
        b.extend_from_slice(&state);
        frames.push(b.freeze());
        let empty = TickInputs::new();
        let mut chunk: Vec<Bytes> = Vec::new();
        let mut chunk_bytes = 0usize;
        let flush = |chunk: &mut Vec<Bytes>, frames: &mut Vec<Bytes>| {
            if chunk.is_empty() {
                return;
            }
            let mut out = BytesMut::new();
            out.put_u8(KIND_BUNDLE);
            out.put_u8(chunk.len() as u8);
            for r in chunk.drain(..) {
                out.put_u16(r.len() as u16);
                out.extend_from_slice(&r);
            }
            frames.push(out.freeze());
        };
        for t in from..end {
            let inputs = self.journal.tick(Tick(t)).unwrap_or(&empty);
            let bundle = Self::encode_bundle(t, inputs);
            if chunk.len() == 255 || chunk_bytes + bundle.len() + 2 > CATCHUP_CHUNK_BYTES {
                flush(&mut chunk, &mut frames);
                chunk_bytes = 0;
            }
            chunk_bytes += bundle.len() + 2;
            chunk.push(bundle);
        }
        flush(&mut chunk, &mut frames);
        frames
    }

    fn encode_bundle(t: u32, inputs: &TickInputs) -> Bytes {
        let mut b = BytesMut::new();
        b.put_u8(KIND_BUNDLE);
        b.put_u32(t);
        b.put_u8(inputs.len().min(255) as u8);
        for (a, d) in inputs.iter().take(255) {
            b.put_u16(a.0);
            b.put_u8(d.len().min(255) as u8);
            b.extend_from_slice(&d[..d.len().min(255)]);
        }
        b.freeze()
    }

    /// Client side: parses a `Catchup` frame.
    pub fn decode_catchup(mut d: Bytes) -> Option<Checkpoint> {
        if d.len() < 17 || d.get_u8() != KIND_CATCHUP {
            return None;
        }
        let tick = d.get_u32();
        let hash = d.get_u64();
        let len = d.get_u32() as usize;
        if d.len() < len {
            return None;
        }
        Some(Checkpoint {
            tick,
            hash,
            state: d.split_to(len),
        })
    }

    /// Client side: parses a `CheckpointReq` frame (the server's current tick).
    pub fn decode_checkpoint_request(mut d: Bytes) -> Option<u32> {
        if d.len() < 5 || d.get_u8() != KIND_CHECKPOINT_REQ {
            return None;
        }
        Some(d.get_u32())
    }

    /// Client (master) side: a `Checkpoint` upload frame.
    pub fn encode_checkpoint(tick: Tick, hash: u64, state: &[u8]) -> Bytes {
        let mut b = BytesMut::with_capacity(13 + state.len());
        b.put_u8(KIND_CHECKPOINT);
        b.put_u32(tick.0);
        b.put_u64(hash);
        b.extend_from_slice(state);
        b.freeze()
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
        } else {
            self.actors.insert(actor, ());
        }
    }

    pub fn on_leave(&mut self, actor: ActorId) {
        self.strikes.remove(&actor);
        self.spectators.remove(&actor);
        self.actors.remove(&actor);
    }

    /// Client data frame. Returns true when handled here.
    pub fn handle(&mut self, actor: ActorId, mut payload: Bytes) -> bool {
        let Some(&kind) = payload.first() else {
            return false;
        };
        if self.spectators.contains(&actor)
            && matches!(kind, KIND_INPUT | KIND_HASH | KIND_CHECKPOINT)
        {
            return true;
        }
        match kind {
            KIND_INPUT => {
                payload.advance(1);
                if payload.len() < 5 {
                    return true;
                }
                let first = payload.get_u32();
                let n = payload.get_u8() as usize;
                for i in 0..n.min(8) {
                    if payload.is_empty() {
                        break;
                    }
                    let len = payload.get_u8() as usize;
                    if payload.len() < len {
                        break;
                    }
                    let data = payload.split_to(len).to_vec();
                    let t = first.wrapping_add(i as u32);
                    // Late inputs are stamped onto the next tick so the player keeps acting.
                    let t = t.max(self.tick.0);
                    if t > self.tick.0 + MAX_AHEAD {
                        continue;
                    }
                    self.pending
                        .entry(t)
                        .or_default()
                        .entry(actor)
                        .or_insert(data);
                }
                true
            }
            KIND_HASH => {
                payload.advance(1);
                if payload.len() < 12 {
                    return true;
                }
                let t = payload.get_u32();
                let h = payload.get_u64();
                if self.sim.is_some() {
                    if let Some(&sh) = self.server_hashes.get(&t) {
                        if h != sh {
                            self.desyncs.push((t, actor));
                        }
                    }
                } else if t < self.tick.0 && self.tick.0 - t <= 600 {
                    self.hashes.entry(t).or_default().insert(actor, h);
                    self.check_hashes(t);
                }
                true
            }
            KIND_CHECKPOINT => {
                payload.advance(1);
                if self.sim.is_some()
                    || Some(actor) != self.master
                    || payload.len() < 12
                    || payload.len() - 12 > MAX_CHECKPOINT_BYTES
                {
                    return true;
                }
                let t = payload.get_u32();
                let h = payload.get_u64();
                if t > self.tick.0 || self.checkpoint.as_ref().is_some_and(|c| c.tick >= t) {
                    return true;
                }
                self.checkpoint = Some(Checkpoint {
                    tick: t,
                    hash: h,
                    state: payload,
                });
                self.want_checkpoint = false;
                true
            }
            _ => false,
        }
    }

    /// Majority hash wins; anyone else is flagged once all current actors reported.
    fn check_hashes(&mut self, t: u32) {
        let Some(reports) = self.hashes.get(&t) else {
            return;
        };
        if reports.len() < self.actors.len().max(2) {
            return;
        }
        let mut counts: HashMap<u64, usize> = HashMap::new();
        for h in reports.values() {
            *counts.entry(*h).or_default() += 1;
        }
        let (majority, _) = counts.iter().max_by_key(|(_, n)| **n).unwrap();
        for (a, h) in reports {
            if h != majority {
                self.desyncs.push((t, *a));
            }
        }
        self.hashes.remove(&t);
        // forget very old partial reports
        let cutoff = self.tick.0.saturating_sub(600);
        self.hashes.retain(|k, _| *k >= cutoff);
    }

    /// The canonical replay so far (consumes the journal copy).
    pub fn replay(&self, schema_hash: &str) -> net_rollback::Replay {
        // Actors from the journal: by the time a room closes everyone has left.
        let actors = self.journal.actors();
        self.journal
            .clone()
            .into_replay(self.seed, schema_hash.to_string(), self.hz, actors, 10)
    }

    pub fn take_desyncs(&mut self) -> Vec<(u32, ActorId)> {
        std::mem::take(&mut self.desyncs)
    }

    /// Closes the current tick: records inputs, builds the bundle, advances the clock.
    /// Returns the datagram to send to every actor (current + redundant bundles).
    pub fn tick(&mut self) -> Bytes {
        let t = self.tick.0;
        let mut inputs = self.pending.remove(&t).unwrap_or_default();
        // A new actor appears in its first bundle even without input, so every simulation
        // (server and clients) spawns the avatar at the same tick.
        for a in self.actors.keys() {
            if self.seen.insert(*a) {
                inputs.entry(*a).or_default();
            }
        }
        for (a, d) in inputs.iter().take(255) {
            self.journal.record(Tick(t), *a, d.clone());
        }
        if self.journal.live_len() > MAX_JOURNAL_TICKS {
            if let Some(c) = &self.checkpoint {
                let keep_from = c
                    .tick
                    .saturating_sub(self.hz as u32 * CHECKPOINT_EVERY_SECS);
                self.journal.trim_before(keep_from);
            }
        }
        if let Some(sim) = self.sim.as_mut() {
            sim.step(&inputs);
            let now = t.wrapping_add(1);
            let every = (self.hz as u32 * CHECKPOINT_EVERY_SECS).max(1);
            if now.is_multiple_of(SERVER_HASH_EVERY) || now.is_multiple_of(every) {
                let h = sim.hash();
                self.server_hashes.insert(now, h);
                let cutoff = now.saturating_sub(600);
                self.server_hashes.retain(|k, _| *k >= cutoff);
                if now.is_multiple_of(every) {
                    let state = sim.frame.snapshot();
                    if state.len() <= MAX_CHECKPOINT_BYTES {
                        self.checkpoint = Some(Checkpoint {
                            tick: now,
                            hash: h,
                            state: Bytes::from(state),
                        });
                        self.want_checkpoint = false;
                    }
                }
            }
        }
        let b = Self::encode_bundle(t, &inputs);
        let bundle = b;
        self.recent.push_back(bundle);
        while self.recent.len() > BUNDLE_REDUNDANCY {
            self.recent.pop_front();
        }
        self.tick = Tick(t.wrapping_add(1));
        // One datagram: u8 count, then bundles each prefixed by u16 len.
        let mut out = BytesMut::new();
        out.put_u8(KIND_BUNDLE);
        out.put_u8(self.recent.len() as u8);
        for r in &self.recent {
            out.put_u16(r.len() as u16);
            out.extend_from_slice(r);
        }
        let out = out.freeze();
        self.past.push_back(out.clone());
        while self.past.len() > self.spectator_delay as usize + 1 {
            self.past.pop_front();
        }
        out
    }

    /// Client side: splits a server datagram into (tick, inputs) bundles.
    pub fn decode_bundles(mut d: Bytes) -> Vec<(Tick, TickInputs)> {
        let mut out = Vec::new();
        if d.len() < 2 || d.get_u8() != KIND_BUNDLE {
            return out;
        }
        let n = d.get_u8() as usize;
        for _ in 0..n {
            if d.len() < 2 {
                break;
            }
            let len = d.get_u16() as usize;
            if d.len() < len {
                break;
            }
            let mut b = d.split_to(len);
            if b.len() < 6 || b.get_u8() != KIND_BUNDLE {
                break;
            }
            let t = b.get_u32();
            let k = b.get_u8() as usize;
            let mut inputs = TickInputs::new();
            for _ in 0..k {
                if b.len() < 3 {
                    break;
                }
                let a = ActorId(b.get_u16());
                let l = b.get_u8() as usize;
                if b.len() < l {
                    break;
                }
                inputs.insert(a, b.split_to(l).to_vec());
            }
            out.push((Tick(t), inputs));
        }
        out
    }

    pub fn encode_hash(tick: Tick, hash: u64) -> Bytes {
        let mut b = BytesMut::with_capacity(13);
        b.put_u8(KIND_HASH);
        b.put_u32(tick.0);
        b.put_u64(hash);
        b.freeze()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::SyncRoom;

    #[test]
    fn bundles_inputs_and_detects_desync() {
        let mut r = LockstepRoom::new(60, 1);
        let (a, b, c) = (ActorId(1), ActorId(2), ActorId(3));
        for x in [a, b, c] {
            r.on_join(x);
        }
        assert!(r.handle(a, SyncRoom::encode_input(Tick(0), &[vec![1], vec![2]])));
        assert!(r.handle(b, SyncRoom::encode_input(Tick(1), &[vec![9]])));
        let d0 = r.tick();
        let got = LockstepRoom::decode_bundles(d0);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, Tick(0));
        assert_eq!(got[0].1.get(&a), Some(&vec![1]));
        assert_eq!(got[0].1.get(&b), Some(&vec![])); // first appearance, no input yet
        let d1 = r.tick();
        let got = LockstepRoom::decode_bundles(d1);
        assert_eq!(got.len(), 2); // redundancy: tick 0 again + tick 1
        assert_eq!(got[1].1.get(&a), Some(&vec![2]));
        assert_eq!(got[1].1.get(&b), Some(&vec![9]));
        assert_eq!(r.journal.len(), 2);
        // late input lands on the current tick, far-ahead input is dropped
        assert!(r.handle(c, SyncRoom::encode_input(Tick(0), &[vec![5]])));
        assert!(r.handle(c, SyncRoom::encode_input(Tick(500), &[vec![6]])));
        let got = LockstepRoom::decode_bundles(r.tick());
        assert_eq!(got.last().unwrap().1.get(&c), Some(&vec![5]));
        // hashes: a and b agree, c differs
        r.handle(a, LockstepRoom::encode_hash(Tick(1), 42));
        r.handle(b, LockstepRoom::encode_hash(Tick(1), 42));
        assert!(r.take_desyncs().is_empty());
        r.handle(c, LockstepRoom::encode_hash(Tick(1), 43));
        assert_eq!(r.take_desyncs(), vec![(1, c)]);
    }

    #[test]
    fn late_join_gets_checkpoint_and_journal_tail() {
        let mut r = LockstepRoom::new(60, 7);
        let (a, b) = (ActorId(1), ActorId(2));
        r.on_join(a);
        r.on_join(b);
        r.master = Some(a);
        for t in 0..400u32 {
            r.handle(a, SyncRoom::encode_input(Tick(t), &[vec![t as u8]]));
            r.handle(b, SyncRoom::encode_input(Tick(t), &[vec![1, t as u8]]));
            let _ = r.tick();
            let req = r.checkpoint_request();
            // Periodic request every 5s (300 ticks).
            assert_eq!(req.is_some(), t + 1 == 300, "tick {t}");
            if let Some(req) = req {
                assert_eq!(LockstepRoom::decode_checkpoint_request(req), Some(300));
                // Non-master uploads are ignored.
                assert!(r.handle(b, LockstepRoom::encode_checkpoint(Tick(300), 1, b"x")));
                assert!(r.checkpoint.is_none());
                assert!(r.handle(
                    a,
                    LockstepRoom::encode_checkpoint(Tick(300), 0xC0FFEE, b"state")
                ));
            }
        }
        assert_eq!(r.checkpoint.as_ref().map(|c| c.tick), Some(300));
        let frames = r.on_late_join();
        let ck = LockstepRoom::decode_catchup(frames[0].clone()).unwrap();
        assert_eq!(
            (ck.tick, ck.hash, &ck.state[..]),
            (300, 0xC0FFEE, &b"state"[..])
        );
        let bundles: Vec<_> = frames[1..]
            .iter()
            .flat_map(|f| LockstepRoom::decode_bundles(f.clone()))
            .collect();
        assert_eq!(bundles.len(), 100);
        for (i, (t, inputs)) in bundles.iter().enumerate() {
            assert_eq!(t.0, 300 + i as u32);
            assert_eq!(inputs.get(&a), Some(&vec![(300 + i) as u8]));
            assert_eq!(inputs.get(&b).map(|v| v.len()), Some(2));
        }
        // Fresh checkpoint: no urgent request.
        assert!(r.checkpoint_request().is_none());

        // Without any checkpoint the joiner replays from tick 0 and a request becomes urgent.
        let mut r2 = LockstepRoom::new(60, 1);
        r2.on_join(a);
        r2.master = Some(a);
        for _ in 0..700 {
            let _ = r2.tick();
            let _ = r2.checkpoint_request();
        }
        let frames = r2.on_late_join();
        let ck = LockstepRoom::decode_catchup(frames[0].clone()).unwrap();
        assert_eq!((ck.tick, ck.state.len()), (0, 0));
        let n: usize = frames[1..]
            .iter()
            .map(|f| LockstepRoom::decode_bundles(f.clone()).len())
            .sum();
        assert_eq!(n, 700);
        assert!(frames.iter().all(|f| f.len() <= 64 * 1024));
        for _ in 0..60 {
            let _ = r2.tick();
        }
        assert!(r2.checkpoint_request().is_some());

        // Snapshot -> restore on another node continues the clock and serves catch-up.
        let snap = r.snapshot();
        assert_eq!(
            (snap.tick, snap.journal_from, snap.journal.len()),
            (400, 300, 100)
        );
        let json = serde_json::to_vec(&snap).unwrap();
        let mut r3 = LockstepRoom::restore(serde_json::from_slice(&json).unwrap());
        assert_eq!(r3.tick, Tick(400));
        let _ = r3.tick();
        let frames = r3.on_late_join();
        let ck = LockstepRoom::decode_catchup(frames[0].clone()).unwrap();
        assert_eq!(ck.tick, 300);
        let n: usize = frames[1..]
            .iter()
            .map(|f| LockstepRoom::decode_bundles(f.clone()).len())
            .sum();
        assert_eq!(n, 101);
        // The restored node only knows the journal from the checkpoint on; its replay says so.
        let (full, partial) = (r.replay(""), r3.replay(""));
        assert_eq!((full.from_tick, full.inputs.len()), (0, 400));
        assert_eq!((partial.from_tick, partial.inputs.len()), (300, 100));
    }

    #[test]
    fn catchup_is_contiguous_across_chunks_and_past_journal_end() {
        // Journal has inputs only up to tick 553, room is at 600, checkpoint at 300: the tail
        // must still cover 300..599 contiguously (empty bundles past the journal end), and the
        // 255-bundles-per-frame limit must not drop or duplicate anything.
        let mut r = LockstepRoom::new(60, 1);
        let a = ActorId(1);
        r.on_join(a);
        r.master = Some(a);
        for t in 0..600u32 {
            if t < 554 {
                r.handle(a, SyncRoom::encode_input(Tick(t), &[vec![t as u8, 1, 2]]));
            }
            let _ = r.tick();
            if t == 299 {
                assert!(r.handle(a, LockstepRoom::encode_checkpoint(Tick(300), 1, b"s")));
            }
        }
        let snap = r.snapshot();
        assert_eq!(
            (snap.tick, snap.journal_from, snap.journal.len()),
            (600, 300, 254)
        );
        let mut r2 = LockstepRoom::restore(snap);
        let frames = r2.on_late_join();
        assert_eq!(
            LockstepRoom::decode_catchup(frames[0].clone())
                .unwrap()
                .tick,
            300
        );
        let mut expect = 300u32;
        for f in &frames[1..] {
            let bundles = LockstepRoom::decode_bundles(f.clone());
            assert!(!bundles.is_empty());
            for (t, inputs) in bundles {
                assert_eq!(t.0, expect, "gap/dup at {expect}");
                assert_eq!(inputs.contains_key(&a), t.0 < 554);
                expect += 1;
            }
        }
        assert_eq!(expect, 600);
    }

    #[test]
    fn journal_entries_fill_the_gap_after_a_snapshot() {
        let mut r = LockstepRoom::new(60, 1);
        let a = ActorId(1);
        r.on_join(a);
        r.master = Some(a);
        let mut snap = None;
        let mut entries = Vec::new();
        for t in 0..100u32 {
            r.handle(a, SyncRoom::encode_input(Tick(t), &[vec![t as u8]]));
            let _ = r.tick();
            if t == 49 {
                snap = Some(r.snapshot());
            } else if t > 49 {
                entries.push(r.last_bundle().unwrap().to_vec());
            }
        }
        let mut r2 = LockstepRoom::restore(snap.unwrap());
        assert_eq!(r2.tick.0, 50);
        // stale and duplicate entries are ignored, the tail is applied once
        let mut with_noise = vec![entries[0].clone()];
        with_noise.extend(entries.iter().cloned());
        assert_eq!(r2.apply_journal(&with_noise), 50);
        assert_eq!(r2.tick.0, 100);
        assert_eq!(r2.journal.tail(0), r.journal.tail(0));
        assert_eq!(r2.apply_journal(&entries), 0);
    }

    #[test]
    fn strikes_make_an_actor_untrusted() {
        let mut r = LockstepRoom::new(60, 1);
        let a = ActorId(1);
        assert_eq!(r.strike(a), 1);
        assert_eq!(r.strike(a), 2);
        assert!(!r.is_untrusted(a));
        assert_eq!(r.strike(a), 3);
        assert!(r.is_untrusted(a));
        r.on_leave(a);
        assert!(!r.is_untrusted(a));
    }

    #[test]
    fn spectators_get_the_stream_late_and_send_nothing() {
        let mut r = LockstepRoom::new(60, 1).with_spectator_delay(1);
        let (a, w) = (ActorId(1), ActorId(9));
        r.on_join(a);
        r.on_join_as(w, true);
        assert_eq!(r.actors.len(), 1);
        assert!(r.handle(w, SyncRoom::encode_input(Tick(0), &[vec![5]])));
        let mut live = Vec::new();
        for t in 0..70u32 {
            r.handle(a, SyncRoom::encode_input(Tick(t), &[vec![t as u8]]));
            live.push(r.tick());
            if t < 60 {
                assert!(r.spectator_datagram().is_none(), "t={t}");
            }
        }
        // 60 ticks of delay: at tick 69 the spectator gets the datagram of tick 9
        assert_eq!(r.spectator_datagram(), Some(live[9].clone()));
        assert!(r.journal.tick(Tick(0)).unwrap().get(&w).is_none());
        // catch-up for a spectator stops at the delayed tick
        let frames = r.catchup_for(true);
        let mut last = 0;
        for f in &frames[1..] {
            for (t, _) in LockstepRoom::decode_bundles(f.clone()) {
                last = t.0;
            }
        }
        assert_eq!(last, 9);
    }

    #[test]
    fn server_sim_is_the_truth_and_survives_restore() {
        use crate::sync::{ArenaLogic, ARENA_SCHEMA};
        let schema = Schema::parse(ARENA_SCHEMA).unwrap();
        let logic = || -> Box<dyn GameLogic> { Box::new(ArenaLogic { speed: 6.0 }) };
        let mut r = LockstepRoom::new(60, 7);
        r.attach_sim(schema.clone(), logic()).unwrap();
        let (a, b) = (ActorId(1), ActorId(2));
        r.on_join(a);
        r.on_join(b);
        // a client mirror steps the same bundles with the same rule
        let mut mirror = ServerSim::new(
            Frame::new(
                schema.clone(),
                FrameConfig {
                    tick_hz: 60,
                    ..Default::default()
                },
                7,
            ),
            logic(),
        );
        let mut mirror_hashes = BTreeMap::new();
        for t in 0..320u32 {
            r.handle(a, SyncRoom::encode_input(Tick(t), &[vec![127, 0, 0]]));
            let dg = r.tick();
            for (tk, inputs) in LockstepRoom::decode_bundles(dg) {
                if tk.0 == t {
                    mirror.step(&inputs);
                    if (t + 1).is_multiple_of(SERVER_HASH_EVERY) {
                        mirror_hashes.insert(t + 1, mirror.hash());
                    }
                }
            }
        }
        assert_eq!(r.server_hash(320), Some(mirror_hashes[&320]));
        assert_eq!(r.server_hash(10), Some(mirror_hashes[&10]));
        assert!(r.checkpoint.as_ref().is_some_and(|c| c.tick == 300));
        assert!(r.checkpoint_request().is_none());
        // both clients agree on a wrong hash: a majority vote would pass, the server does not
        for x in [a, b] {
            r.handle(x, LockstepRoom::encode_hash(Tick(310), 12345));
        }
        assert_eq!(r.take_desyncs(), vec![(310, a), (310, b)]);
        r.handle(a, LockstepRoom::encode_hash(Tick(310), mirror_hashes[&310]));
        assert!(r.take_desyncs().is_empty());
        // re-host: checkpoint at 300 + journal tail resimulated to 320 gives the same hash
        let snap = r.snapshot();
        let mut r2 = LockstepRoom::restore(snap);
        r2.attach_sim(schema, logic()).unwrap();
        assert_eq!(r2.sim.as_ref().unwrap().hash(), mirror_hashes[&320]);
    }
}
