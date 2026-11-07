//! Client side of lockstep rooms, the same core as the managed (C#) and Web SDKs: confirmed input
//! bundles delivered in tick order, late-join catch-up, hash reports, checkpoint uploads when the
//! node asks the master, adaptive input delay from the node's margin reports and state dumps after a
//! desync. Pure logic: feed every data-path payload to [`LockstepClient::receive`], send the frames
//! it hands back. The game owns the deterministic simulation.

use std::collections::BTreeMap;

use bytes::{BufMut, Bytes, BytesMut};

pub const LS_INPUT: u8 = 0x10;
pub const LS_BUNDLE: u8 = 0x20;
pub const LS_HASH: u8 = 0x21;
pub const LS_CHECKPOINT: u8 = 0x22;
pub const LS_CHECKPOINT_REQ: u8 = 0x23;
pub const LS_CATCHUP: u8 = 0x24;
pub const LS_INPUT_STATS: u8 = 0x25;
pub const LS_DUMP_REQ: u8 = 0x26;
pub const LS_DUMP: u8 = 0x27;

/// Ticks of future bundles kept while waiting for a gap to fill before they are dropped.
const MAX_PENDING_AHEAD: u32 = 600;

/// Confirmed inputs of one tick: actor id → input bytes (absent actor = sent nothing).
pub type TickInputs = BTreeMap<u16, Bytes>;

/// What the node told us, in order. `Tick` arrives for every tick exactly once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LsEvent {
    Tick {
        tick: u32,
        inputs: TickInputs,
    },
    /// Catch-up start: restore the game from this checkpoint (empty state at tick 0 = fresh start).
    Restore {
        tick: u32,
        hash: u64,
        state: Bytes,
    },
    /// The node asked us (the master) for the confirmed state at a tick <= this one: answer with
    /// [`LockstepClient::checkpoint_frame`].
    CheckpointRequested(u32),
    /// The node flagged a desync and wants our state at this tick: answer with
    /// [`LockstepClient::dump_frame`] or ignore.
    DumpRequested(u32),
}

/// A frame to send on the data path; `reliable` picks the channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outgoing {
    pub reliable: bool,
    pub payload: Bytes,
}

#[derive(Debug)]
pub struct LockstepClient {
    pending: BTreeMap<u32, TickInputs>,
    recent_inputs: Vec<Bytes>,
    started: bool,
    next: u32,
    last_input: u32,
    slack_streak: u8,
    /// Newest tick the node has bundled (+1).
    pub server_tick: u32,
    /// Ticks ahead of the server clock inputs are stamped; tuned between min/max from margin
    /// reports when `adaptive_delay` is on.
    pub input_delay: u32,
    pub input_delay_min: u32,
    pub input_delay_max: u32,
    pub adaptive_delay: bool,
    /// Inputs repeated per datagram (1..=8).
    pub input_redundancy: usize,
    pub last_margin: i8,
    pub late_inputs: u64,
    pub delivered: u64,
    pub catchups: u64,
    pub dropped: u64,
}

impl Default for LockstepClient {
    fn default() -> Self {
        Self::new()
    }
}

impl LockstepClient {
    pub fn new() -> Self {
        Self {
            pending: BTreeMap::new(),
            recent_inputs: Vec::new(),
            started: false,
            next: 0,
            last_input: 0,
            slack_streak: 0,
            server_tick: 0,
            input_delay: 2,
            input_delay_min: 1,
            input_delay_max: 8,
            adaptive_delay: true,
            input_redundancy: 3,
            last_margin: 0,
            late_inputs: 0,
            delivered: 0,
            catchups: 0,
            dropped: 0,
        }
    }

    /// Next tick to be delivered as `LsEvent::Tick`.
    pub fn confirmed_tick(&self) -> u32 {
        self.next
    }

    /// Feeds one data-path payload. `None` when it was not a lockstep frame.
    pub fn receive(&mut self, p: &[u8]) -> Option<Vec<LsEvent>> {
        let mut out = Vec::new();
        match *p.first()? {
            LS_CATCHUP => {
                if p.len() >= 17 {
                    let tick = u32_at(p, 1);
                    let hash = u64_at(p, 5);
                    let len = (u32_at(p, 13) as usize).min(p.len() - 17);
                    self.pending.clear();
                    self.next = tick;
                    self.started = true;
                    self.catchups += 1;
                    out.push(LsEvent::Restore {
                        tick,
                        hash,
                        state: Bytes::copy_from_slice(&p[17..17 + len]),
                    });
                }
            }
            LS_BUNDLE => {
                if p.len() >= 2 {
                    let n = p[1] as usize;
                    let mut o = 2;
                    for _ in 0..n {
                        if o + 2 > p.len() {
                            break;
                        }
                        let len = u16::from_be_bytes([p[o], p[o + 1]]) as usize;
                        o += 2;
                        if o + len > p.len() {
                            break;
                        }
                        self.decode_bundle(&p[o..o + len]);
                        o += len;
                    }
                    self.drain(&mut out);
                }
            }
            LS_CHECKPOINT_REQ => {
                if p.len() >= 5 {
                    out.push(LsEvent::CheckpointRequested(u32_at(p, 1)));
                }
            }
            LS_DUMP_REQ => {
                if p.len() >= 5 {
                    out.push(LsEvent::DumpRequested(u32_at(p, 1)));
                }
            }
            LS_INPUT_STATS => {
                if p.len() >= 7 {
                    self.on_input_stats(p[5] as i8, p[6]);
                }
            }
            LS_CHECKPOINT | LS_HASH | LS_INPUT => {}
            _ => return None,
        }
        Some(out)
    }

    /// Raises the delay as soon as inputs arrive late; lowers it one tick after three consecutive
    /// reports with >= 2 ticks of slack.
    pub fn on_input_stats(&mut self, margin: i8, late: u8) {
        self.last_margin = margin;
        self.late_inputs += late as u64;
        if !self.adaptive_delay {
            return;
        }
        if margin < 0 {
            let bump = (-(margin as i32)).min(4) as u32;
            self.input_delay = (self.input_delay + bump).min(self.input_delay_max);
            self.slack_streak = 0;
        } else if margin >= 2 {
            self.slack_streak += 1;
            if self.slack_streak >= 3 {
                self.slack_streak = 0;
                if self.input_delay > self.input_delay_min {
                    self.input_delay -= 1;
                }
            }
        } else {
            self.slack_streak = 0;
        }
    }

    fn decode_bundle(&mut self, b: &[u8]) {
        if b.len() < 6 || b[0] != LS_BUNDLE {
            return;
        }
        let t = u32_at(b, 1);
        let k = b[5] as usize;
        let mut o = 6;
        self.server_tick = self.server_tick.max(t + 1);
        if !self.started {
            self.started = true;
            self.next = t;
        }
        if t < self.next || self.pending.contains_key(&t) {
            return;
        }
        let mut inputs = TickInputs::new();
        for _ in 0..k {
            if o + 3 > b.len() {
                break;
            }
            let a = u16::from_be_bytes([b[o], b[o + 1]]);
            let l = b[o + 2] as usize;
            o += 3;
            if o + l > b.len() {
                break;
            }
            inputs.insert(a, Bytes::copy_from_slice(&b[o..o + l]));
            o += l;
        }
        self.pending.insert(t, inputs);
    }

    fn drain(&mut self, out: &mut Vec<LsEvent>) {
        while let Some(inputs) = self.pending.remove(&self.next) {
            let tick = self.next;
            self.next += 1;
            self.delivered += 1;
            out.push(LsEvent::Tick { tick, inputs });
        }
        let limit = self.next + MAX_PENDING_AHEAD;
        let far: Vec<u32> = self.pending.range(limit + 1..).map(|(t, _)| *t).collect();
        self.dropped += far.len() as u64;
        for t in far {
            self.pending.remove(&t);
        }
    }

    /// Queues this tick's local input (sent together with the previous ones for redundancy).
    /// Returns the tick it was stamped with and the unreliable frame to send.
    pub fn send_input(&mut self, input: Bytes) -> (u32, Outgoing) {
        let tick = (self.server_tick + self.input_delay).max(self.last_input + 1);
        self.last_input = tick;
        self.recent_inputs.push(input);
        let keep = self.input_redundancy.clamp(1, 8);
        while self.recent_inputs.len() > keep {
            self.recent_inputs.remove(0);
        }
        let first = tick - (self.recent_inputs.len() as u32 - 1);
        let mut b = BytesMut::with_capacity(
            6 + self
                .recent_inputs
                .iter()
                .map(|i| i.len() + 1)
                .sum::<usize>(),
        );
        b.put_u8(LS_INPUT);
        b.put_u32(first);
        b.put_u8(self.recent_inputs.len() as u8);
        for i in &self.recent_inputs {
            b.put_u8(i.len().min(255) as u8);
            b.extend_from_slice(&i[..i.len().min(255)]);
        }
        (
            tick,
            Outgoing {
                reliable: false,
                payload: b.freeze(),
            },
        )
    }

    /// Hash of the confirmed state after `tick`; the node compares it across players (and the
    /// server simulation when there is one).
    pub fn hash_frame(tick: u32, hash: u64) -> Outgoing {
        let mut b = BytesMut::with_capacity(13);
        b.put_u8(LS_HASH);
        b.put_u32(tick);
        b.put_u64(hash);
        Outgoing {
            reliable: false,
            payload: b.freeze(),
        }
    }

    /// Master only, in answer to `CheckpointRequested`: confirmed state at `tick` (<= requested).
    pub fn checkpoint_frame(tick: u32, hash: u64, state: &[u8]) -> Outgoing {
        frame(LS_CHECKPOINT, tick, hash, state)
    }

    /// Answer to `DumpRequested`: your state at `tick` (<= 60 KiB), kept by the node for 24 h next
    /// to the master's for offline diffing.
    pub fn dump_frame(tick: u32, hash: u64, state: &[u8]) -> Outgoing {
        frame(LS_DUMP, tick, hash, state)
    }

    /// FNV-1a 64 over bytes: the portable state hash the other SDKs use.
    pub fn fnv1a(data: &[u8]) -> u64 {
        data.iter().fold(0xcbf29ce484222325u64, |h, &x| {
            (h ^ x as u64).wrapping_mul(0x100000001b3)
        })
    }
}

fn frame(kind: u8, tick: u32, hash: u64, state: &[u8]) -> Outgoing {
    let mut b = BytesMut::with_capacity(13 + state.len());
    b.put_u8(kind);
    b.put_u32(tick);
    b.put_u64(hash);
    b.extend_from_slice(state);
    Outgoing {
        reliable: true,
        payload: b.freeze(),
    }
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn u64_at(b: &[u8], o: usize) -> u64 {
    ((u32_at(b, o) as u64) << 32) | u32_at(b, o + 4) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    type TickSpec<'a> = (u32, &'a [(u16, &'a [u8])]);

    fn bundle(ticks: &[TickSpec<'_>]) -> Vec<u8> {
        let mut p = vec![LS_BUNDLE, ticks.len() as u8];
        for (t, inputs) in ticks {
            let mut b = vec![LS_BUNDLE];
            b.extend_from_slice(&t.to_be_bytes());
            b.push(inputs.len() as u8);
            for (a, d) in *inputs {
                b.extend_from_slice(&a.to_be_bytes());
                b.push(d.len() as u8);
                b.extend_from_slice(d);
            }
            p.extend_from_slice(&(b.len() as u16).to_be_bytes());
            p.extend_from_slice(&b);
        }
        p
    }

    #[test]
    fn bundles_are_delivered_in_tick_order_and_once() {
        let mut c = LockstepClient::new();
        let ev = c.receive(&bundle(&[(0, &[(1, b"a")])])).unwrap();
        assert_eq!(ev.len(), 1);
        assert!(matches!(&ev[0], LsEvent::Tick { tick: 0, inputs } if inputs[&1].as_ref() == b"a"));
        // tick 2 before tick 1: held back, then both delivered in order
        assert!(c.receive(&bundle(&[(2, &[])])).unwrap().is_empty());
        let ev = c.receive(&bundle(&[(1, &[(2, b"xy")]), (0, &[])])).unwrap();
        let ticks: Vec<u32> = ev
            .iter()
            .map(|e| match e {
                LsEvent::Tick { tick, .. } => *tick,
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(ticks, vec![1, 2]);
        assert_eq!(c.confirmed_tick(), 3);
        assert_eq!(c.server_tick, 3);
        assert_eq!(c.delivered, 3);
    }

    #[test]
    fn catchup_restores_and_resets_the_cursor() {
        let mut c = LockstepClient::new();
        c.receive(&bundle(&[(0, &[])])).unwrap();
        let mut p = vec![LS_CATCHUP];
        p.extend_from_slice(&40u32.to_be_bytes());
        p.extend_from_slice(&7u64.to_be_bytes());
        p.extend_from_slice(&3u32.to_be_bytes());
        p.extend_from_slice(b"abc");
        let ev = c.receive(&p).unwrap();
        assert_eq!(
            ev,
            vec![LsEvent::Restore {
                tick: 40,
                hash: 7,
                state: Bytes::from_static(b"abc")
            }]
        );
        assert_eq!(c.confirmed_tick(), 40);
        assert!(c.receive(&bundle(&[(39, &[])])).unwrap().is_empty());
        assert_eq!(c.receive(&bundle(&[(40, &[])])).unwrap().len(), 1);
    }

    #[test]
    fn input_frames_carry_redundant_inputs_and_adaptive_delay() {
        let mut c = LockstepClient::new();
        c.input_redundancy = 2;
        c.server_tick = 10;
        let (t1, _) = c.send_input(Bytes::from_static(b"A"));
        assert_eq!(t1, 12);
        let (t2, f) = c.send_input(Bytes::from_static(b"BB"));
        assert_eq!(t2, 13);
        assert!(!f.reliable);
        assert_eq!(
            f.payload.as_ref(),
            &[LS_INPUT, 0, 0, 0, 12, 2, 1, b'A', 2, b'B', b'B']
        );
        // late report: delay jumps; three slack reports: delay drops by one
        c.receive(&[LS_INPUT_STATS, 0, 0, 0, 30, 0xFE, 1]).unwrap();
        assert_eq!((c.input_delay, c.last_margin, c.late_inputs), (4, -2, 1));
        for _ in 0..3 {
            c.receive(&[LS_INPUT_STATS, 0, 0, 0, 60, 3, 0]).unwrap();
        }
        assert_eq!(c.input_delay, 3);
        c.input_delay_max = 4;
        c.on_input_stats(-100, 0);
        assert_eq!(c.input_delay, 4);
        c.adaptive_delay = false;
        c.on_input_stats(-5, 2);
        assert_eq!((c.input_delay, c.late_inputs), (4, 3));
    }

    #[test]
    fn requests_and_answer_frames() {
        let mut c = LockstepClient::new();
        assert_eq!(
            c.receive(&[LS_CHECKPOINT_REQ, 0, 0, 1, 0]).unwrap(),
            vec![LsEvent::CheckpointRequested(256)]
        );
        assert_eq!(
            c.receive(&[LS_DUMP_REQ, 0, 0, 0, 9]).unwrap(),
            vec![LsEvent::DumpRequested(9)]
        );
        assert_eq!(c.receive(&[0x11, 1, 2]), None);
        let d = LockstepClient::dump_frame(9, 0x0102030405060708, b"st");
        assert!(d.reliable);
        assert_eq!(
            d.payload.as_ref(),
            &[LS_DUMP, 0, 0, 0, 9, 1, 2, 3, 4, 5, 6, 7, 8, b's', b't']
        );
        assert_eq!(LockstepClient::hash_frame(1, 2).payload.len(), 13);
        assert_eq!(LockstepClient::fnv1a(b""), 0xcbf29ce484222325);
        assert_eq!(LockstepClient::fnv1a(b"a"), 0xaf63dc4c8601ec8c);
    }
}
