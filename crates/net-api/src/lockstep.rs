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

use bytes::{Buf, BufMut, Bytes, BytesMut};
use net_common::types::{ActorId, Tick};
use net_rollback::{Journal, TickInputs};
use std::collections::{BTreeMap, HashMap, VecDeque};

pub const KIND_INPUT: u8 = crate::sync::KIND_INPUT;
pub const KIND_BUNDLE: u8 = 0x20;
pub const KIND_HASH: u8 = 0x21;
pub const BUNDLE_REDUNDANCY: usize = 3;
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
}

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
        }
    }

    pub fn tick_period(&self) -> std::time::Duration {
        std::time::Duration::from_micros(1_000_000 / self.hz as u64)
    }

    pub fn on_join(&mut self, actor: ActorId) {
        self.actors.insert(actor, ());
    }

    pub fn on_leave(&mut self, actor: ActorId) {
        self.actors.remove(&actor);
    }

    /// Client data frame. Returns true when handled here.
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
                if t < self.tick.0 && self.tick.0 - t <= 600 {
                    self.hashes.entry(t).or_default().insert(actor, h);
                    self.check_hashes(t);
                }
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
        let inputs = self.pending.remove(&t).unwrap_or_default();
        let mut b = BytesMut::new();
        b.put_u8(KIND_BUNDLE);
        b.put_u32(t);
        b.put_u8(inputs.len().min(255) as u8);
        for (a, d) in inputs.iter().take(255) {
            b.put_u16(a.0);
            b.put_u8(d.len().min(255) as u8);
            b.extend_from_slice(&d[..d.len().min(255)]);
            self.journal.record(Tick(t), *a, d.clone());
        }
        let bundle = b.freeze();
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
        out.freeze()
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
        assert!(!got[0].1.contains_key(&b));
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
}
