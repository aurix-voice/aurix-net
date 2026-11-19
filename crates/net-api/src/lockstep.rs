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
//! 0x25 InputStats   server->client  u32 tick, i8 min_margin, u8 late   every 30 ticks per actor:
//!                                   how early its inputs arrived (negative = late) -> clients
//!                                   tune their input delay instead of guessing from ping
//! 0x26 DumpReq      server->client  u32 tick            after a desync: "send your state at tick"
//! 0x27 Dump         client->server  u32 tick, u64 hash, bytes state (<= 60 KiB, reliable)
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
/// server->client  u32 tick, i8 min_margin (ticks the actor's inputs arrived before they were
/// needed over the last report window; negative = late), u8 late_count. Clients tune their
/// input delay from it.
pub const KIND_INPUT_STATS: u8 = 0x25;
/// server->client  u32 tick: "send me your state at this tick" (after a desync).
pub const KIND_DUMP_REQ: u8 = 0x26;
/// client->server  u32 tick, u64 hash, bytes state (<= MAX_CHECKPOINT_BYTES).
pub const KIND_DUMP: u8 = 0x27;
/// Command: client -> server (reliable). Out-of-input-stream reliable messages (buy, surrender,
/// pause) that must still be deterministic: the server stamps them onto the next tick and ships
/// them inside that tick's bundle as the input of the pseudo-actor `COMMAND_ACTOR`, so they are in
/// the journal, in catch-ups, in replays and in the referee's input stream.
pub const KIND_COMMAND: u8 = 0x28;
/// Pseudo-actor carrying the tick's commands: `[u16 actor, u16 len, bytes]*`.
pub const COMMAND_ACTOR: ActorId = ActorId(0xFFFF);
/// Commands kept per tick (bytes) and per command; more are dropped (the sender sees no effect).
pub const MAX_COMMANDS_BYTES_PER_TICK: usize = 4096;
pub const MAX_COMMAND_BYTES: usize = 1024;
/// Ticks between input-margin reports (per actor).
pub const INPUT_REPORT_EVERY: u32 = 30;
const MAX_PENDING_DUMPS: usize = 16;

/// A `KIND_INPUT_STATS` frame; sync rooms send the same one.
pub(crate) fn input_stats_frame(tick: u32, min_margin: i32, late: u16) -> Bytes {
    let mut buf = BytesMut::with_capacity(7);
    buf.put_u8(KIND_INPUT_STATS);
    buf.put_u32(tick);
    buf.put_i8(min_margin.clamp(-127, 127) as i8);
    buf.put_u8(late.min(255) as u8);
    buf.freeze()
}

/// A client's state at the tick a desync was detected, for offline comparison.
#[derive(Debug, Clone)]
pub struct Dump {
    pub tick: u32,
    pub actor: ActorId,
    pub hash: u64,
    pub state: Bytes,
}
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
    /// Commands received since the last tick, already in `COMMAND_ACTOR` wire form.
    pending_commands: Vec<u8>,
    pub journal: Journal,
    recent: VecDeque<Bytes>,
    /// Hash reports per tick: actor -> hash.
    hashes: BTreeMap<u32, HashMap<ActorId, u64>>,
    pub desyncs: Vec<(u32, ActorId)>,
    /// Per actor since the last report: (min input margin in ticks, late inputs).
    margins: HashMap<ActorId, (i32, u16)>,
    pending_dumps: std::collections::HashSet<(u32, ActorId)>,
    dumps: Vec<Dump>,
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
    /// Recent referee states at `SERVER_HASH_EVERY` ticks (bounded by `MAX_SERVER_STATES_BYTES`),
    /// so a desync can be answered with the node's own state instead of trusting the master.
    server_states: BTreeMap<u32, Bytes>,
    /// Actors that have appeared in a bundle (see `tick`).
    seen: BTreeSet<ActorId>,
}

/// Ticks between server state hashes; matches `RollbackConfig::default().hash_every`.
pub const SERVER_HASH_EVERY: u32 = 10;
/// Total bytes of recent referee states kept for desync dumps (the newest is always kept).
pub const MAX_SERVER_STATES_BYTES: usize = 512 * 1024;
/// Actor id under which the node's own (referee) state is stored among desync dumps.
pub const REFEREE_ACTOR: ActorId = ActorId(0);
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
        for actor in inputs.keys() {
            if !self.avatars.contains_key(actor) {
                // After a restore the avatar already exists in the frame: re-link, do not spawn.
                let existing = self
                    .frame
                    .entities()
                    .find(|e| e.owner == Some(*actor))
                    .map(|e| e.id);
                let av = existing.or_else(|| self.logic.on_join(&mut self.frame, *actor));
                self.avatars.insert(*actor, av);
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
            pending_commands: Vec::new(),
            journal: Journal::default(),
            recent: VecDeque::new(),
            hashes: BTreeMap::new(),
            desyncs: Vec::new(),
            margins: HashMap::new(),
            pending_dumps: Default::default(),
            dumps: Vec::new(),
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
            server_states: BTreeMap::new(),
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
            Some(checkpoint) => {
                sim.frame
                    .restore(&checkpoint.state)
                    .map_err(|e| e.to_string())?;
                // Every owned entity is somebody's avatar: keep stepping them like before.
                let owned: Vec<(ActorId, EntityId)> = sim
                    .frame
                    .entities()
                    .filter_map(|e| e.owner.map(|o| (o, e.id)))
                    .collect();
                for (owner, id) in owned {
                    sim.avatars.entry(owner).or_insert(Some(id));
                }
                checkpoint.tick
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
        self.server_states.clear();
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
        let strikes = self.strikes.entry(actor).or_default();
        *strikes = strikes.saturating_add(1);
        *strikes
    }

    pub fn is_untrusted(&self, actor: ActorId) -> bool {
        self.strikes
            .get(&actor)
            .is_some_and(|n| *n >= UNTRUSTED_AFTER)
    }

    /// The bundle produced by the last [`Self::tick`]: one input-journal entry for Redis, so a node
    /// that adopts this room after a crash loses no inputs between checkpoints.
    pub fn last_bundle(&self) -> Option<Bytes> {
        self.recent.back().cloned()
    }

    /// Applies journal entries (from `last_bundle`) stored after the snapshot this room was
    /// restored from: inputs go into the journal and the clock moves past them. Entries at or
    /// before the current tick are already covered and skipped. Returns ticks applied.
    pub fn apply_journal(&mut self, entries: &[Vec<u8>]) -> u32 {
        let mut total = 0;
        for entry in entries {
            let Some((t, inputs)) = Self::decode_bundle(entry) else {
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
            for (actor, data) in inputs {
                self.journal.record(t, actor, data);
            }
            self.tick = Tick(t.0.wrapping_add(1));
            total += 1;
        }
        total
    }

    /// Parses one bundle as produced by `encode_bundle`.
    pub fn decode_bundle(mut buf: &[u8]) -> Option<(Tick, TickInputs)> {
        if buf.len() < 6 || buf.get_u8() != KIND_BUNDLE {
            return None;
        }
        let tick = buf.get_u32();
        let count = buf.get_u8() as usize;
        let mut inputs = TickInputs::new();
        for _ in 0..count {
            if buf.len() < 3 {
                return None;
            }
            let actor = ActorId(buf.get_u16());
            let len = buf.get_u8() as usize;
            if buf.len() < len {
                return None;
            }
            inputs.insert(actor, buf[..len].to_vec());
            buf.advance(len);
        }
        Some((Tick(tick), inputs))
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
        let mut room = Self::new(snap.hz, snap.seed);
        room.tick = Tick(snap.tick);
        room.checkpoint = snap.checkpoint.map(|(tick, hash, state)| Checkpoint {
            tick,
            hash,
            state: Bytes::from(state),
        });
        room.journal = Journal::from_parts(snap.journal_from, snap.journal);
        room.want_checkpoint = true;
        room
    }

    fn checkpoint_every(&self) -> u32 {
        (self.hz as u32 * CHECKPOINT_EVERY_SECS).max(1)
    }

    /// Called once per tick after [`Self::tick`]: a `CheckpointReq` frame for the master, if due.
    pub fn checkpoint_request(&mut self) -> Option<Bytes> {
        if self.sim.is_some() {
            return None;
        }
        self.master?;
        let tick = self.tick.0;
        let periodic = tick > 0 && tick.is_multiple_of(self.checkpoint_every());
        let urgent = self.want_checkpoint
            && self
                .last_checkpoint_req
                .is_none_or(|r| tick.wrapping_sub(r) >= self.hz as u32);
        if !(periodic || urgent) {
            return None;
        }
        self.last_checkpoint_req = Some(tick);
        let mut buf = BytesMut::with_capacity(5);
        buf.put_u8(KIND_CHECKPOINT_REQ);
        buf.put_u32(tick);
        Some(buf.freeze())
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
            Some(checkpoint) => (checkpoint.tick, checkpoint.hash, checkpoint.state.clone()),
            None => (0, 0, Bytes::new()),
        };
        if self.tick.0.saturating_sub(from) > 2 * self.checkpoint_every() {
            self.want_checkpoint = true;
        }
        let mut frames = Vec::new();
        let mut buf = BytesMut::with_capacity(17 + state.len());
        buf.put_u8(KIND_CATCHUP);
        buf.put_u32(from);
        buf.put_u64(hash);
        buf.put_u32(state.len() as u32);
        buf.extend_from_slice(&state);
        frames.push(buf.freeze());
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
            for bundle in chunk.drain(..) {
                out.put_u16(bundle.len() as u16);
                out.extend_from_slice(&bundle);
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

    fn encode_bundle(tick: u32, inputs: &TickInputs) -> Bytes {
        let mut buf = BytesMut::new();
        buf.put_u8(KIND_BUNDLE);
        buf.put_u32(tick);
        buf.put_u8(inputs.len().min(255) as u8);
        for (actor, data) in inputs.iter().take(255) {
            buf.put_u16(actor.0);
            buf.put_u8(data.len().min(255) as u8);
            buf.extend_from_slice(&data[..data.len().min(255)]);
        }
        buf.freeze()
    }

    /// Client side: parses a `Catchup` frame.
    pub fn decode_catchup(mut data: Bytes) -> Option<Checkpoint> {
        if data.len() < 17 || data.get_u8() != KIND_CATCHUP {
            return None;
        }
        let tick = data.get_u32();
        let hash = data.get_u64();
        let len = data.get_u32() as usize;
        if data.len() < len {
            return None;
        }
        Some(Checkpoint {
            tick,
            hash,
            state: data.split_to(len),
        })
    }

    /// Client side: parses a `CheckpointReq` frame (the server's current tick).
    pub fn decode_checkpoint_request(mut data: Bytes) -> Option<u32> {
        if data.len() < 5 || data.get_u8() != KIND_CHECKPOINT_REQ {
            return None;
        }
        Some(data.get_u32())
    }

    /// Client (master) side: a `Checkpoint` upload frame.
    pub fn encode_checkpoint(tick: Tick, hash: u64, state: &[u8]) -> Bytes {
        let mut buf = BytesMut::with_capacity(13 + state.len());
        buf.put_u8(KIND_CHECKPOINT);
        buf.put_u32(tick.0);
        buf.put_u64(hash);
        buf.extend_from_slice(state);
        buf.freeze()
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
            && matches!(kind, KIND_INPUT | KIND_HASH | KIND_CHECKPOINT | KIND_DUMP)
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
                let count = payload.get_u8() as usize;
                for i in 0..count.min(8) {
                    if payload.is_empty() {
                        break;
                    }
                    let len = payload.get_u8() as usize;
                    if payload.len() < len {
                        break;
                    }
                    let data = payload.split_to(len).to_vec();
                    let stamped = first.wrapping_add(i as u32);
                    if i + 1 == count.min(8) {
                        // the newest input in the packet tells how much slack this actor has
                        let margin = (stamped as i64 - self.tick.0 as i64).clamp(-127, 127) as i32;
                        let slack = self.margins.entry(actor).or_insert((i32::MAX, 0));
                        slack.0 = slack.0.min(margin);
                        if margin < 0 {
                            slack.1 = slack.1.saturating_add(1);
                        }
                    }
                    // Late inputs are stamped onto the next tick so the player keeps acting.
                    let tick = stamped.max(self.tick.0);
                    if tick > self.tick.0 + MAX_AHEAD {
                        continue;
                    }
                    self.pending
                        .entry(tick)
                        .or_default()
                        .entry(actor)
                        .or_insert(data);
                }
                true
            }
            KIND_COMMAND => {
                payload.advance(1);
                if payload.is_empty()
                    || payload.len() > MAX_COMMAND_BYTES
                    || self.pending_commands.len() + 4 + payload.len() > MAX_COMMANDS_BYTES_PER_TICK
                    || !self.actors.contains_key(&actor)
                {
                    return true;
                }
                self.pending_commands.put_u16(actor.0);
                self.pending_commands.put_u16(payload.len() as u16);
                self.pending_commands.extend_from_slice(&payload);
                true
            }
            KIND_HASH => {
                payload.advance(1);
                if payload.len() < 12 {
                    return true;
                }
                let tick = payload.get_u32();
                let hash = payload.get_u64();
                if self.sim.is_some() {
                    if let Some(&sh) = self.server_hashes.get(&tick) {
                        if hash != sh {
                            self.desyncs.push((tick, actor));
                            if let Some(dump) = self.referee_dump(tick) {
                                if !self
                                    .dumps
                                    .iter()
                                    .any(|x| x.tick == tick && x.actor == REFEREE_ACTOR)
                                {
                                    self.dumps.push(dump);
                                }
                            }
                        }
                    }
                } else if tick < self.tick.0 && self.tick.0 - tick <= 600 {
                    self.hashes.entry(tick).or_default().insert(actor, hash);
                    self.check_hashes(tick);
                }
                true
            }
            KIND_DUMP => {
                payload.advance(1);
                if payload.len() < 12 || payload.len() - 12 > MAX_CHECKPOINT_BYTES {
                    return true;
                }
                let tick = payload.get_u32();
                let hash = payload.get_u64();
                if self.pending_dumps.remove(&(tick, actor)) {
                    self.dumps.push(Dump {
                        tick,
                        actor,
                        hash,
                        state: payload,
                    });
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
                let tick = payload.get_u32();
                let hash = payload.get_u64();
                if tick > self.tick.0 || self.checkpoint.as_ref().is_some_and(|c| c.tick >= tick) {
                    return true;
                }
                self.checkpoint = Some(Checkpoint {
                    tick,
                    hash,
                    state: payload,
                });
                self.want_checkpoint = false;
                true
            }
            _ => false,
        }
    }

    /// Majority hash wins; anyone else is flagged once all current actors reported.
    fn check_hashes(&mut self, tick: u32) {
        let Some(reports) = self.hashes.get(&tick) else {
            return;
        };
        if reports.len() < self.actors.len().max(2) {
            return;
        }
        let mut counts: HashMap<u64, usize> = HashMap::new();
        for hash in reports.values() {
            *counts.entry(*hash).or_default() += 1;
        }
        let (majority, _) = counts.iter().max_by_key(|(_, n)| **n).unwrap();
        for (actor, hash) in reports {
            if hash != majority {
                self.desyncs.push((tick, *actor));
            }
        }
        self.hashes.remove(&tick);
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

    /// Instant replay: the confirmed inputs of a live room for ticks `from..to` (what the journal
    /// still holds; `from_tick` in the result says where it really starts).
    pub fn replay_window(&self, from: u32, to: Option<u32>) -> net_rollback::Replay {
        let mut replay = self.replay("");
        window_replay(&mut replay, from, to);
        replay
    }

    pub fn take_desyncs(&mut self) -> Vec<(u32, ActorId)> {
        std::mem::take(&mut self.desyncs)
    }

    /// Every `INPUT_REPORT_EVERY` ticks: one `KIND_INPUT_STATS` frame per actor that sent input
    /// since the last report. Call right after `tick()`.
    pub fn input_reports(&mut self) -> Vec<(ActorId, Bytes)> {
        if !self.tick.0.is_multiple_of(INPUT_REPORT_EVERY) || self.margins.is_empty() {
            return Vec::new();
        }
        let tick = self.tick.0;
        std::mem::take(&mut self.margins)
            .into_iter()
            .map(|(a, (min, late))| (a, input_stats_frame(tick, min, late)))
            .collect()
    }

    pub fn decode_input_stats(mut data: Bytes) -> Option<(u32, i8, u8)> {
        if data.len() < 7 || data.get_u8() != KIND_INPUT_STATS {
            return None;
        }
        Some((data.get_u32(), data.get_i8(), data.get_u8()))
    }

    /// Asks `actor` for its state at `tick` (after a desync); the answer lands in `take_dumps`.
    pub fn request_dump(&mut self, tick: u32, actor: ActorId) -> Option<Bytes> {
        if self.pending_dumps.len() >= MAX_PENDING_DUMPS
            || !self.pending_dumps.insert((tick, actor))
        {
            return None;
        }
        let mut buf = BytesMut::with_capacity(5);
        buf.put_u8(KIND_DUMP_REQ);
        buf.put_u32(tick);
        Some(buf.freeze())
    }

    /// The node's own state at `tick` (lockstep rooms with a `logic` module), stored among the
    /// desync dumps as actor `REFEREE_ACTOR` so operators diff clients against the truth.
    pub fn referee_dump(&self, tick: u32) -> Option<Dump> {
        let state = self.server_states.get(&tick)?.clone();
        let hash = self.server_hashes.get(&tick).copied()?;
        Some(Dump {
            tick,
            actor: REFEREE_ACTOR,
            hash,
            state,
        })
    }

    pub fn take_dumps(&mut self) -> Vec<Dump> {
        std::mem::take(&mut self.dumps)
    }

    pub fn encode_dump(tick: u32, hash: u64, state: &[u8]) -> Bytes {
        let mut buf = BytesMut::with_capacity(13 + state.len());
        buf.put_u8(KIND_DUMP);
        buf.put_u32(tick);
        buf.put_u64(hash);
        buf.extend_from_slice(state);
        buf.freeze()
    }

    /// Closes the current tick: records inputs, builds the bundle, advances the clock.
    /// Returns the datagram to send to every actor (current + redundant bundles).
    pub fn tick(&mut self) -> Bytes {
        let tick = self.tick.0;
        let mut inputs = self.pending.remove(&tick).unwrap_or_default();
        // A new actor appears in its first bundle even without input, so every simulation
        // (server and clients) spawns the avatar at the same tick.
        for actor in self.actors.keys() {
            if self.seen.insert(*actor) {
                inputs.entry(*actor).or_default();
            }
        }
        if !self.pending_commands.is_empty() {
            inputs.insert(COMMAND_ACTOR, std::mem::take(&mut self.pending_commands));
        }
        for (actor, data) in inputs.iter().take(255) {
            self.journal.record(Tick(tick), *actor, data.clone());
        }
        if self.journal.live_len() > MAX_JOURNAL_TICKS {
            if let Some(checkpoint) = &self.checkpoint {
                let keep_from = checkpoint
                    .tick
                    .saturating_sub(self.hz as u32 * CHECKPOINT_EVERY_SECS);
                self.journal.trim_before(keep_from);
            }
        }
        if let Some(sim) = self.sim.as_mut() {
            sim.step(&inputs);
            let now = tick.wrapping_add(1);
            let every = (self.hz as u32 * CHECKPOINT_EVERY_SECS).max(1);
            if now.is_multiple_of(SERVER_HASH_EVERY) || now.is_multiple_of(every) {
                let hash = sim.hash();
                self.server_hashes.insert(now, hash);
                let cutoff = now.saturating_sub(600);
                self.server_hashes.retain(|k, _| *k >= cutoff);
                let state = sim.frame.snapshot();
                if state.len() <= MAX_CHECKPOINT_BYTES {
                    self.server_states.insert(now, Bytes::from(state.clone()));
                    let mut total: usize = self.server_states.values().map(|b| b.len()).sum();
                    while total > MAX_SERVER_STATES_BYTES && self.server_states.len() > 1 {
                        if let Some((&oldest, _)) = self.server_states.iter().next() {
                            total -= self.server_states.remove(&oldest).map_or(0, |b| b.len());
                        }
                    }
                }
                if now.is_multiple_of(every) && state.len() <= MAX_CHECKPOINT_BYTES {
                    self.checkpoint = Some(Checkpoint {
                        tick: now,
                        hash,
                        state: Bytes::from(state),
                    });
                    self.want_checkpoint = false;
                }
            }
        }
        let encoded = Self::encode_bundle(tick, &inputs);
        let bundle = encoded;
        self.recent.push_back(bundle);
        while self.recent.len() > BUNDLE_REDUNDANCY {
            self.recent.pop_front();
        }
        self.tick = Tick(tick.wrapping_add(1));
        // One datagram: u8 count, then bundles each prefixed by u16 len.
        let mut out = BytesMut::new();
        out.put_u8(KIND_BUNDLE);
        out.put_u8(self.recent.len() as u8);
        for bundle in &self.recent {
            out.put_u16(bundle.len() as u16);
            out.extend_from_slice(bundle);
        }
        let out = out.freeze();
        self.past.push_back(out.clone());
        while self.past.len() > self.spectator_delay as usize + 1 {
            self.past.pop_front();
        }
        out
    }

    /// Client side: splits a server datagram into (tick, inputs) bundles.
    pub fn decode_bundles(mut data: Bytes) -> Vec<(Tick, TickInputs)> {
        let mut out = Vec::new();
        if data.len() < 2 || data.get_u8() != KIND_BUNDLE {
            return out;
        }
        let count = data.get_u8() as usize;
        for _ in 0..count {
            if data.len() < 2 {
                break;
            }
            let len = data.get_u16() as usize;
            if data.len() < len {
                break;
            }
            let mut buf = data.split_to(len);
            if buf.len() < 6 || buf.get_u8() != KIND_BUNDLE {
                break;
            }
            let tick = buf.get_u32();
            let count = buf.get_u8() as usize;
            let mut inputs = TickInputs::new();
            for _ in 0..count {
                if buf.len() < 3 {
                    break;
                }
                let actor = ActorId(buf.get_u16());
                let len = buf.get_u8() as usize;
                if buf.len() < len {
                    break;
                }
                inputs.insert(actor, buf.split_to(len).to_vec());
            }
            out.push((Tick(tick), inputs));
        }
        out
    }

    pub fn encode_hash(tick: Tick, hash: u64) -> Bytes {
        let mut buf = BytesMut::with_capacity(13);
        buf.put_u8(KIND_HASH);
        buf.put_u32(tick.0);
        buf.put_u64(hash);
        buf.freeze()
    }
}

/// Cuts a replay down to ticks `from..to` (exclusive); keeps `from_tick`/`hashes` consistent.
pub fn window_replay(replay: &mut net_rollback::Replay, from: u32, to: Option<u32>) {
    if from > replay.from_tick {
        let drop = ((from - replay.from_tick) as usize).min(replay.inputs.len());
        replay.inputs.drain(..drop);
        replay.from_tick += drop as u32;
    }
    if let Some(to) = to {
        let keep = to.saturating_sub(replay.from_tick) as usize;
        replay.inputs.truncate(keep);
    }
    let end = replay.from_tick + replay.inputs.len() as u32;
    replay
        .hashes
        .retain(|(t, _)| *t >= replay.from_tick && *t < end);
}

#[cfg(test)]
mod tests {
    #[test]
    fn commands_ride_the_next_bundle_as_the_command_actor() {
        let mut room = LockstepRoom::new(30, 1);
        let (alice, bob) = (ActorId(1), ActorId(2));
        room.on_join(alice);
        room.on_join(bob);
        let _ = room.tick();
        assert!(room.handle(alice, Bytes::from_static(&[KIND_COMMAND, b'x', b'y'])));
        assert!(room.handle(bob, Bytes::from_static(&[KIND_COMMAND, 7])));
        // unknown actor: dropped
        assert!(room.handle(ActorId(9), Bytes::from_static(&[KIND_COMMAND, 1])));
        let out = room.tick();
        let bundles = LockstepRoom::decode_bundles(out);
        let (_, inputs) = bundles.last().unwrap();
        assert_eq!(
            inputs[&COMMAND_ACTOR],
            vec![0, 1, 0, 2, b'x', b'y', 0, 2, 0, 1, 7]
        );
        // nothing pending: the next bundle has no command actor
        let out = room.tick();
        let bundles = LockstepRoom::decode_bundles(out);
        assert!(!bundles.last().unwrap().1.contains_key(&COMMAND_ACTOR));
    }

    #[test]
    fn replay_window_cuts_inputs_and_hashes_consistently() {
        let mut replay = net_rollback::Replay {
            seed: 1,
            schema_hash: String::new(),
            tick_hz: 30,
            actors: vec![],
            from_tick: 0,
            inputs: (0..10).map(|_| Default::default()).collect(),
            hash_every: 5,
            hashes: vec![(0, 1), (5, 2), (9, 3)],
        };
        window_replay(&mut replay, 3, Some(7));
        assert_eq!((replay.from_tick, replay.inputs.len()), (3, 4));
        assert_eq!(replay.hashes, vec![(5, 2)]);
        // a window that starts before what the journal still holds keeps the journal's start
        let mut r2 = replay.clone();
        window_replay(&mut r2, 0, None);
        assert_eq!((r2.from_tick, r2.inputs.len()), (3, 4));
        window_replay(&mut replay, 100, Some(200));
        assert_eq!(
            (replay.from_tick, replay.inputs.len(), replay.hashes.len()),
            (7, 0, 0)
        );
    }

    use super::*;
    use crate::sync::SyncRoom;

    #[test]
    fn bundles_inputs_and_detects_desync() {
        let mut room = LockstepRoom::new(60, 1);
        let (alice, bob, carol) = (ActorId(1), ActorId(2), ActorId(3));
        for actor in [alice, bob, carol] {
            room.on_join(actor);
        }
        assert!(room.handle(alice, SyncRoom::encode_input(Tick(0), &[vec![1], vec![2]])));
        assert!(room.handle(bob, SyncRoom::encode_input(Tick(1), &[vec![9]])));
        let d0 = room.tick();
        let got = LockstepRoom::decode_bundles(d0);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, Tick(0));
        assert_eq!(got[0].1.get(&alice), Some(&vec![1]));
        assert_eq!(got[0].1.get(&bob), Some(&vec![])); // first appearance, no input yet
        let d1 = room.tick();
        let got = LockstepRoom::decode_bundles(d1);
        assert_eq!(got.len(), 2); // redundancy: tick 0 again + tick 1
        assert_eq!(got[1].1.get(&alice), Some(&vec![2]));
        assert_eq!(got[1].1.get(&bob), Some(&vec![9]));
        assert_eq!(room.journal.len(), 2);
        // late input lands on the current tick, far-ahead input is dropped
        assert!(room.handle(carol, SyncRoom::encode_input(Tick(0), &[vec![5]])));
        assert!(room.handle(carol, SyncRoom::encode_input(Tick(500), &[vec![6]])));
        let got = LockstepRoom::decode_bundles(room.tick());
        assert_eq!(got.last().unwrap().1.get(&carol), Some(&vec![5]));
        // hashes: a and b agree, c differs
        room.handle(alice, LockstepRoom::encode_hash(Tick(1), 42));
        room.handle(bob, LockstepRoom::encode_hash(Tick(1), 42));
        assert!(room.take_desyncs().is_empty());
        room.handle(carol, LockstepRoom::encode_hash(Tick(1), 43));
        assert_eq!(room.take_desyncs(), vec![(1, carol)]);
    }

    #[test]
    fn late_join_gets_checkpoint_and_journal_tail() {
        let mut room = LockstepRoom::new(60, 7);
        let (alice, bob) = (ActorId(1), ActorId(2));
        room.on_join(alice);
        room.on_join(bob);
        room.master = Some(alice);
        for t in 0..400u32 {
            room.handle(alice, SyncRoom::encode_input(Tick(t), &[vec![t as u8]]));
            room.handle(bob, SyncRoom::encode_input(Tick(t), &[vec![1, t as u8]]));
            let _ = room.tick();
            let req = room.checkpoint_request();
            // Periodic request every 5s (300 ticks).
            assert_eq!(req.is_some(), t + 1 == 300, "tick {t}");
            if let Some(req) = req {
                assert_eq!(LockstepRoom::decode_checkpoint_request(req), Some(300));
                // Non-master uploads are ignored.
                assert!(room.handle(bob, LockstepRoom::encode_checkpoint(Tick(300), 1, b"x")));
                assert!(room.checkpoint.is_none());
                assert!(room.handle(
                    alice,
                    LockstepRoom::encode_checkpoint(Tick(300), 0xC0FFEE, b"state")
                ));
            }
        }
        assert_eq!(room.checkpoint.as_ref().map(|c| c.tick), Some(300));
        let frames = room.on_late_join();
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
            assert_eq!(inputs.get(&alice), Some(&vec![(300 + i) as u8]));
            assert_eq!(inputs.get(&bob).map(|v| v.len()), Some(2));
        }
        // Fresh checkpoint: no urgent request.
        assert!(room.checkpoint_request().is_none());

        // Without any checkpoint the joiner replays from tick 0 and a request becomes urgent.
        let mut r2 = LockstepRoom::new(60, 1);
        r2.on_join(alice);
        r2.master = Some(alice);
        for _ in 0..700 {
            let _ = r2.tick();
            let _ = r2.checkpoint_request();
        }
        let frames = r2.on_late_join();
        let ck = LockstepRoom::decode_catchup(frames[0].clone()).unwrap();
        assert_eq!((ck.tick, ck.state.len()), (0, 0));
        let total: usize = frames[1..]
            .iter()
            .map(|f| LockstepRoom::decode_bundles(f.clone()).len())
            .sum();
        assert_eq!(total, 700);
        assert!(frames.iter().all(|f| f.len() <= 64 * 1024));
        for _ in 0..60 {
            let _ = r2.tick();
        }
        assert!(r2.checkpoint_request().is_some());

        // Snapshot -> restore on another node continues the clock and serves catch-up.
        let snap = room.snapshot();
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
        let total: usize = frames[1..]
            .iter()
            .map(|f| LockstepRoom::decode_bundles(f.clone()).len())
            .sum();
        assert_eq!(total, 101);
        // The restored node only knows the journal from the checkpoint on; its replay says so.
        let (full, partial) = (room.replay(""), r3.replay(""));
        assert_eq!((full.from_tick, full.inputs.len()), (0, 400));
        assert_eq!((partial.from_tick, partial.inputs.len()), (300, 100));
    }

    #[test]
    fn catchup_is_contiguous_across_chunks_and_past_journal_end() {
        // Journal has inputs only up to tick 553, room is at 600, checkpoint at 300: the tail
        // must still cover 300..599 contiguously (empty bundles past the journal end), and the
        // 255-bundles-per-frame limit must not drop or duplicate anything.
        let mut room = LockstepRoom::new(60, 1);
        let alice = ActorId(1);
        room.on_join(alice);
        room.master = Some(alice);
        for t in 0..600u32 {
            if t < 554 {
                room.handle(
                    alice,
                    SyncRoom::encode_input(Tick(t), &[vec![t as u8, 1, 2]]),
                );
            }
            let _ = room.tick();
            if t == 299 {
                assert!(room.handle(alice, LockstepRoom::encode_checkpoint(Tick(300), 1, b"s")));
            }
        }
        let snap = room.snapshot();
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
        for frame in &frames[1..] {
            let bundles = LockstepRoom::decode_bundles(frame.clone());
            assert!(!bundles.is_empty());
            for (tick, inputs) in bundles {
                assert_eq!(tick.0, expect, "gap/dup at {expect}");
                assert_eq!(inputs.contains_key(&alice), tick.0 < 554);
                expect += 1;
            }
        }
        assert_eq!(expect, 600);
    }

    #[test]
    fn journal_entries_fill_the_gap_after_a_snapshot() {
        let mut room = LockstepRoom::new(60, 1);
        let alice = ActorId(1);
        room.on_join(alice);
        room.master = Some(alice);
        let mut snap = None;
        let mut entries = Vec::new();
        for t in 0..100u32 {
            room.handle(alice, SyncRoom::encode_input(Tick(t), &[vec![t as u8]]));
            let _ = room.tick();
            if t == 49 {
                snap = Some(room.snapshot());
            } else if t > 49 {
                entries.push(room.last_bundle().unwrap().to_vec());
            }
        }
        let mut r2 = LockstepRoom::restore(snap.unwrap());
        assert_eq!(r2.tick.0, 50);
        // stale and duplicate entries are ignored, the tail is applied once
        let mut with_noise = vec![entries[0].clone()];
        with_noise.extend(entries.iter().cloned());
        assert_eq!(r2.apply_journal(&with_noise), 50);
        assert_eq!(r2.tick.0, 100);
        assert_eq!(r2.journal.tail(0), room.journal.tail(0));
        assert_eq!(r2.apply_journal(&entries), 0);
    }

    #[test]
    fn strikes_make_an_actor_untrusted() {
        let mut room = LockstepRoom::new(60, 1);
        let alice = ActorId(1);
        assert_eq!(room.strike(alice), 1);
        assert_eq!(room.strike(alice), 2);
        assert!(!room.is_untrusted(alice));
        assert_eq!(room.strike(alice), 3);
        assert!(room.is_untrusted(alice));
        room.on_leave(alice);
        assert!(!room.is_untrusted(alice));
    }

    #[test]
    fn spectators_get_the_stream_late_and_send_nothing() {
        let mut room = LockstepRoom::new(60, 1).with_spectator_delay(1);
        let (alice, watcher) = (ActorId(1), ActorId(9));
        room.on_join(alice);
        room.on_join_as(watcher, true);
        assert_eq!(room.actors.len(), 1);
        assert!(room.handle(watcher, SyncRoom::encode_input(Tick(0), &[vec![5]])));
        let mut live = Vec::new();
        for t in 0..70u32 {
            room.handle(alice, SyncRoom::encode_input(Tick(t), &[vec![t as u8]]));
            live.push(room.tick());
            if t < 60 {
                assert!(room.spectator_datagram().is_none(), "t={t}");
            }
        }
        // 60 ticks of delay: at tick 69 the spectator gets the datagram of tick 9
        assert_eq!(room.spectator_datagram(), Some(live[9].clone()));
        assert!(room.journal.tick(Tick(0)).unwrap().get(&watcher).is_none());
        // catch-up for a spectator stops at the delayed tick
        let frames = room.catchup_for(true);
        let mut last = 0;
        for frame in &frames[1..] {
            for (tick, _) in LockstepRoom::decode_bundles(frame.clone()) {
                last = tick.0;
            }
        }
        assert_eq!(last, 9);
    }

    #[test]
    fn server_sim_is_the_truth_and_survives_restore() {
        use crate::sync::{ArenaLogic, ARENA_SCHEMA};
        let schema = Schema::parse(ARENA_SCHEMA).unwrap();
        let logic = || -> Box<dyn GameLogic> { Box::new(ArenaLogic { speed: 6.0 }) };
        let mut room = LockstepRoom::new(60, 7);
        room.attach_sim(schema.clone(), logic()).unwrap();
        let (alice, bob) = (ActorId(1), ActorId(2));
        room.on_join(alice);
        room.on_join(bob);
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
            room.handle(alice, SyncRoom::encode_input(Tick(t), &[vec![127, 0, 0]]));
            let dg = room.tick();
            for (tk, inputs) in LockstepRoom::decode_bundles(dg) {
                if tk.0 == t {
                    mirror.step(&inputs);
                    if (t + 1).is_multiple_of(SERVER_HASH_EVERY) {
                        mirror_hashes.insert(t + 1, mirror.hash());
                    }
                }
            }
        }
        assert_eq!(room.server_hash(320), Some(mirror_hashes[&320]));
        assert_eq!(room.server_hash(10), Some(mirror_hashes[&10]));
        assert!(room.checkpoint.as_ref().is_some_and(|c| c.tick == 300));
        assert!(room.checkpoint_request().is_none());
        // both clients agree on a wrong hash: a majority vote would pass, the server does not
        for actor in [alice, bob] {
            room.handle(actor, LockstepRoom::encode_hash(Tick(310), 12345));
        }
        assert_eq!(room.take_desyncs(), vec![(310, alice), (310, bob)]);
        // the referee's own state is the reference dump (once per tick, actor 0)
        let dumps = room.take_dumps();
        assert_eq!(dumps.len(), 1);
        assert_eq!(dumps[0].actor, REFEREE_ACTOR);
        assert_eq!(dumps[0].tick, 310);
        assert_eq!(dumps[0].hash, mirror_hashes[&310]);
        assert_eq!(hash_bytes(&dumps[0].state), mirror_hashes[&310]);
        room.handle(
            alice,
            LockstepRoom::encode_hash(Tick(310), mirror_hashes[&310]),
        );
        assert!(room.take_desyncs().is_empty());
        // re-host: checkpoint at 300 + journal tail resimulated to 320 gives the same hash
        let snap = room.snapshot();
        let mut r2 = LockstepRoom::restore(snap);
        r2.attach_sim(schema, logic()).unwrap();
        assert_eq!(r2.sim.as_ref().unwrap().hash(), mirror_hashes[&320]);
    }

    #[test]
    fn input_margins_are_reported_and_dumps_only_when_requested() {
        let mut room = LockstepRoom::new(60, 1);
        let alice = ActorId(1);
        room.on_join_as(alice, false);
        for _ in 0..5 {
            room.tick();
        }
        // input stamped 3 ticks ahead, then one late (stamped at an old tick)
        let early = crate::sync::SyncRoom::encode_input(
            net_common::types::Tick(room.tick.0 + 3),
            &[vec![1]],
        );
        room.handle(alice, early);
        let late = crate::sync::SyncRoom::encode_input(
            net_common::types::Tick(room.tick.0.saturating_sub(2)),
            &[vec![2]],
        );
        room.handle(alice, late);
        let mut reports = Vec::new();
        for _ in 0..(INPUT_REPORT_EVERY * 2) {
            room.tick();
            reports = room.input_reports();
            if !reports.is_empty() {
                break;
            }
        }
        assert_eq!(reports.len(), 1);
        let (_, min, late_n) = LockstepRoom::decode_input_stats(reports[0].1.clone()).unwrap();
        assert_eq!(min, -2);
        assert_eq!(late_n, 1);
        assert!(room.input_reports().is_empty(), "report window reset");

        // an unsolicited dump is ignored; a requested one is kept once
        room.handle(alice, LockstepRoom::encode_dump(7, 0xabc, b"state"));
        assert!(room.take_dumps().is_empty());
        let req = room.request_dump(7, alice).expect("request frame");
        assert_eq!(req[0], KIND_DUMP_REQ);
        assert!(
            room.request_dump(7, alice).is_none(),
            "no duplicate request"
        );
        room.handle(alice, LockstepRoom::encode_dump(7, 0xabc, b"state"));
        let dumps = room.take_dumps();
        assert_eq!(dumps.len(), 1);
        assert_eq!(
            (
                dumps[0].tick,
                dumps[0].actor,
                dumps[0].hash,
                &dumps[0].state[..]
            ),
            (7, alice, 0xabc, &b"state"[..])
        );
        room.handle(alice, LockstepRoom::encode_dump(7, 0xabc, b"again"));
        assert!(room.take_dumps().is_empty());
    }
}
