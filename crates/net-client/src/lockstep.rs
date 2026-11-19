//! Client side of lockstep rooms, the same core as the managed (C#) and Web SDKs: confirmed input
//! bundles delivered in tick order, late-join catch-up, hash reports, checkpoint uploads when the
//! node asks the master, adaptive input delay from the node's margin reports and state dumps after a
//! desync. Pure logic: feed every data-path payload to [`LockstepClient::receive`], send the frames
//! it hands back. The game owns the deterministic simulation.

use std::collections::BTreeMap;

use bytes::{BufMut, Bytes, BytesMut};

use crate::margin::MarginControl;

pub const LS_INPUT: u8 = 0x10;
pub const LS_BUNDLE: u8 = 0x20;
pub const LS_HASH: u8 = 0x21;
pub const LS_COMMAND: u8 = 0x28;
pub const LS_ACK: u8 = 0x29;
/// Pseudo-actor whose per-tick input carries the commands.
pub const COMMAND_ACTOR: u16 = 0xFFFF;
pub const LS_CHECKPOINT: u8 = 0x22;
pub const LS_CHECKPOINT_REQ: u8 = 0x23;
pub const LS_CATCHUP: u8 = 0x24;
pub const LS_RESTORE: u8 = 0x2A;
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
    anchored: bool,
    next: u32,
    last_input: u32,
    margin: MarginControl,
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
            anchored: false,
            next: 0,
            last_input: 0,
            margin: MarginControl::default(),
            server_tick: 0,
            input_delay: 2,
            input_delay_min: 1,
            input_delay_max: 32,
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

    /// Feeds one data-path payload. `None` when it was not a lockstep frame. After a bundle
    /// frame (`LS_BUNDLE`) send [`Self::ack_frame`], so the room resends what was lost.
    pub fn receive(&mut self, payload: &[u8]) -> Option<Vec<LsEvent>> {
        let mut out = Vec::new();
        match *payload.first()? {
            kind @ (LS_CATCHUP | LS_RESTORE) => {
                if payload.len() >= 17 {
                    let tick = u32_at(payload, 1);
                    let hash = u64_at(payload, 5);
                    let len = (u32_at(payload, 13) as usize).min(payload.len() - 17);
                    // the catch-up of a join is reliable and can come after bundles that
                    // already took the client past it; a restore always rewinds
                    if kind == LS_CATCHUP && self.anchored && tick < self.next {
                        return Some(out);
                    }
                    self.pending.clear();
                    self.next = tick;
                    self.started = true;
                    self.anchored = true;
                    self.catchups += 1;
                    out.push(LsEvent::Restore {
                        tick,
                        hash,
                        state: Bytes::copy_from_slice(&payload[17..17 + len]),
                    });
                }
            }
            LS_BUNDLE => {
                if payload.len() >= 2 {
                    let count = payload[1] as usize;
                    let mut offset = 2;
                    for _ in 0..count {
                        if offset + 2 > payload.len() {
                            break;
                        }
                        let len =
                            u16::from_be_bytes([payload[offset], payload[offset + 1]]) as usize;
                        offset += 2;
                        if offset + len > payload.len() {
                            break;
                        }
                        self.decode_bundle(&payload[offset..offset + len]);
                        offset += len;
                    }
                    self.drain(&mut out);
                }
            }
            LS_CHECKPOINT_REQ => {
                if payload.len() >= 5 {
                    out.push(LsEvent::CheckpointRequested(u32_at(payload, 1)));
                }
            }
            LS_DUMP_REQ => {
                if payload.len() >= 5 {
                    out.push(LsEvent::DumpRequested(u32_at(payload, 1)));
                }
            }
            LS_INPUT_STATS => {
                if payload.len() >= 7 {
                    let tick = u32::from_be_bytes([payload[1], payload[2], payload[3], payload[4]]);
                    self.on_input_stats(tick, payload[5] as i8, payload[6]);
                }
            }
            LS_CHECKPOINT | LS_HASH | LS_INPUT => {}
            _ => return None,
        }
        Some(out)
    }

    /// Keeps one or two ticks of slack: a late report raises the delay by the lateness plus one
    /// tick; three consecutive reports with more than two ticks to spare lower it by the excess.
    /// A late report that still covers inputs stamped before the last raise does not raise again.
    pub fn on_input_stats(&mut self, tick: u32, margin: i8, late: u8) {
        self.last_margin = margin;
        self.late_inputs += late as u64;
        if !self.adaptive_delay {
            return;
        }
        let change = self.margin.adjust(tick, margin, self.last_input);
        self.input_delay = self
            .input_delay
            .saturating_add_signed(change)
            .clamp(self.input_delay_min, self.input_delay_max);
    }

    fn decode_bundle(&mut self, bundle: &[u8]) {
        if bundle.len() < 6 || bundle[0] != LS_BUNDLE {
            return;
        }
        let tick = u32_at(bundle, 1);
        let count = bundle[5] as usize;
        let mut offset = 6;
        self.server_tick = self.server_tick.max(tick + 1);
        self.started = true;
        // ticks are delivered from tick 0 or from a catch-up, never from the middle
        if !self.anchored {
            if tick != 0 {
                return;
            }
            self.anchored = true;
        }
        if tick < self.next || self.pending.contains_key(&tick) {
            return;
        }
        let mut inputs = TickInputs::new();
        for _ in 0..count {
            if offset + 3 > bundle.len() {
                break;
            }
            let actor = u16::from_be_bytes([bundle[offset], bundle[offset + 1]]);
            let len = bundle[offset + 2] as usize;
            offset += 3;
            if offset + len > bundle.len() {
                break;
            }
            inputs.insert(actor, Bytes::copy_from_slice(&bundle[offset..offset + len]));
            offset += len;
        }
        self.pending.insert(tick, inputs);
    }

    fn drain(&mut self, out: &mut Vec<LsEvent>) {
        while let Some(inputs) = self.pending.remove(&self.next) {
            let tick = self.next;
            self.next += 1;
            self.delivered += 1;
            out.push(LsEvent::Tick { tick, inputs });
        }
        let limit = self.next + MAX_PENDING_AHEAD;
        let far: Vec<u32> = self
            .pending
            .range(limit + 1..)
            .map(|(tick, _)| *tick)
            .collect();
        self.dropped += far.len() as u64;
        for tick in far {
            self.pending.remove(&tick);
        }
    }

    /// Queues this tick's local input (sent together with the previous ones for redundancy).
    /// Returns the tick it was stamped with and the unreliable frame to send. The room takes
    /// one input per tick: called more often than that it returns `None` and sends nothing, as
    /// it does before the first frame from the room, while the room's tick is unknown.
    pub fn send_input(&mut self, input: Bytes) -> Option<(u32, Outgoing)> {
        if !self.started {
            return None;
        }
        let tick = (self.server_tick + self.input_delay).max(self.last_input + 1);
        if tick > self.server_tick + self.input_delay + 1 {
            return None;
        }
        // a frame carries inputs for consecutive ticks, so earlier ones are resent only without a gap
        if tick != self.last_input + 1 {
            self.recent_inputs.clear();
        }
        self.last_input = tick;
        self.recent_inputs.push(input);
        let keep = self.input_redundancy.clamp(1, 8);
        while self.recent_inputs.len() > keep {
            self.recent_inputs.remove(0);
        }
        let first = tick - (self.recent_inputs.len() as u32 - 1);
        let mut frame = BytesMut::with_capacity(
            6 + self
                .recent_inputs
                .iter()
                .map(|queued| queued.len() + 1)
                .sum::<usize>(),
        );
        frame.put_u8(LS_INPUT);
        frame.put_u32(first);
        frame.put_u8(self.recent_inputs.len() as u8);
        for queued in &self.recent_inputs {
            frame.put_u8(queued.len().min(255) as u8);
            frame.extend_from_slice(&queued[..queued.len().min(255)]);
        }
        Some((
            tick,
            Outgoing {
                reliable: false,
                payload: frame.freeze(),
            },
        ))
    }

    /// Tells the room which tick this client needs next: the room then sends every bundle from
    /// that tick on instead of the last three, so a run of lost packets leaves no gap. Sent
    /// after every bundle frame, also while a gap is open; a lost ack is not repeated.
    pub fn ack_frame(&self) -> Outgoing {
        let mut frame = BytesMut::with_capacity(5);
        frame.put_u8(LS_ACK);
        frame.put_u32(if self.anchored { self.next } else { 0 });
        Outgoing {
            reliable: false,
            payload: frame.freeze(),
        }
    }

    /// Hash of the confirmed state after `tick`; the node compares it across players (and the
    /// server simulation when there is one).
    pub fn hash_frame(tick: u32, hash: u64) -> Outgoing {
        let mut frame = BytesMut::with_capacity(13);
        frame.put_u8(LS_HASH);
        frame.put_u32(tick);
        frame.put_u64(hash);
        Outgoing {
            reliable: false,
            payload: frame.freeze(),
        }
    }
    /// Reliable out-of-input-stream command; the server stamps it onto the next tick and every
    /// client (and the referee) receives it in that tick's inputs under `COMMAND_ACTOR` (65535)
    /// as `[u16 actor, u16 len, bytes]*`.
    pub fn command_frame(data: &[u8]) -> Outgoing {
        let mut frame = BytesMut::with_capacity(1 + data.len());
        frame.put_u8(LS_COMMAND);
        frame.extend_from_slice(data);
        Outgoing {
            reliable: true,
            payload: frame.freeze(),
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
        data.iter().fold(0xcbf29ce484222325u64, |hash, &byte| {
            (hash ^ byte as u64).wrapping_mul(0x100000001b3)
        })
    }
}

fn frame(kind: u8, tick: u32, hash: u64, state: &[u8]) -> Outgoing {
    let mut bytes = BytesMut::with_capacity(13 + state.len());
    bytes.put_u8(kind);
    bytes.put_u32(tick);
    bytes.put_u64(hash);
    bytes.extend_from_slice(state);
    Outgoing {
        reliable: true,
        payload: bytes.freeze(),
    }
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    ((u32_at(bytes, offset) as u64) << 32) | u32_at(bytes, offset + 4) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    type TickSpec<'a> = (u32, &'a [(u16, &'a [u8])]);

    fn bundle(ticks: &[TickSpec<'_>]) -> Vec<u8> {
        let mut frame = vec![LS_BUNDLE, ticks.len() as u8];
        for (tick, inputs) in ticks {
            let mut body = vec![LS_BUNDLE];
            body.extend_from_slice(&tick.to_be_bytes());
            body.push(inputs.len() as u8);
            for (actor, input) in *inputs {
                body.extend_from_slice(&actor.to_be_bytes());
                body.push(input.len() as u8);
                body.extend_from_slice(input);
            }
            frame.extend_from_slice(&(body.len() as u16).to_be_bytes());
            frame.extend_from_slice(&body);
        }
        frame
    }

    #[test]
    fn bundles_are_delivered_in_tick_order_and_once() {
        let mut client = LockstepClient::new();
        let ev = client.receive(&bundle(&[(0, &[(1, b"a")])])).unwrap();
        assert_eq!(ev.len(), 1);
        assert!(matches!(&ev[0], LsEvent::Tick { tick: 0, inputs } if inputs[&1].as_ref() == b"a"));
        // tick 2 before tick 1: held back, then both delivered in order
        assert!(client.receive(&bundle(&[(2, &[])])).unwrap().is_empty());
        let ev = client
            .receive(&bundle(&[(1, &[(2, b"xy")]), (0, &[])]))
            .unwrap();
        let ticks: Vec<u32> = ev
            .iter()
            .map(|event| match event {
                LsEvent::Tick { tick, .. } => *tick,
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(ticks, vec![1, 2]);
        assert_eq!(client.confirmed_tick(), 3);
        assert_eq!(client.server_tick, 3);
        assert_eq!(client.delivered, 3);
    }

    #[test]
    fn the_ack_names_the_tick_needed_next() {
        let mut client = LockstepClient::new();
        assert_eq!(client.ack_frame().payload.as_ref(), [LS_ACK, 0, 0, 0, 0]);
        client.receive(&bundle(&[(0, &[]), (1, &[])])).unwrap();
        assert_eq!(client.ack_frame().payload.as_ref(), [LS_ACK, 0, 0, 0, 2]);
        // ticks 2 to 4 were lost: the ack stays on 2 while later bundles wait
        client.receive(&bundle(&[(5, &[]), (6, &[])])).unwrap();
        assert_eq!(client.ack_frame().payload.as_ref(), [LS_ACK, 0, 0, 0, 2]);
        // the room answers with everything from 2 on, more than three bundles in one frame
        let resent: Vec<TickSpec<'_>> = (2..=7).map(|tick| (tick, &[][..])).collect();
        let ev = client.receive(&bundle(&resent)).unwrap();
        assert_eq!(ev.len(), 6);
        assert_eq!(client.ack_frame().payload.as_ref(), [LS_ACK, 0, 0, 0, 8]);
        assert!(!client.ack_frame().reliable);
    }

    fn catchup(tick: u32) -> Vec<u8> {
        let mut frame = vec![LS_CATCHUP];
        frame.extend_from_slice(&tick.to_be_bytes());
        frame.extend_from_slice(&[0; 12]);
        frame
    }

    #[test]
    fn ticks_start_at_tick_zero_or_at_a_catchup() {
        let mut client = LockstepClient::new();
        // a room past tick 0: its bundles come before the reliable catch-up
        assert!(client
            .receive(&bundle(&[(5, &[]), (6, &[])]))
            .unwrap()
            .is_empty());
        assert_eq!(client.ack_frame().payload.as_ref(), [LS_ACK, 0, 0, 0, 0]);
        assert!(matches!(
            client.receive(&catchup(6)).unwrap()[..],
            [LsEvent::Restore { tick: 6, .. }]
        ));
        assert_eq!(
            client
                .receive(&bundle(&[(6, &[]), (7, &[])]))
                .unwrap()
                .len(),
            2
        );
        assert_eq!(client.ack_frame().payload.as_ref(), [LS_ACK, 0, 0, 0, 8]);
    }

    #[test]
    fn a_catchup_behind_the_delivered_ticks_is_ignored() {
        let mut client = LockstepClient::new();
        let first: Vec<TickSpec<'_>> = (0..=3).map(|tick| (tick, &[][..])).collect();
        assert_eq!(client.receive(&bundle(&first)).unwrap().len(), 4);
        assert!(client.receive(&catchup(1)).unwrap().is_empty());
        assert_eq!(client.ack_frame().payload.as_ref(), [LS_ACK, 0, 0, 0, 4]);
        assert_eq!(client.receive(&bundle(&[(4, &[])])).unwrap().len(), 1);
        assert_eq!(client.catchups, 0);
    }

    #[test]
    fn a_restore_behind_the_delivered_ticks_rewinds() {
        let mut client = LockstepClient::new();
        let first: Vec<TickSpec<'_>> = (0..=3).map(|tick| (tick, &[][..])).collect();
        assert_eq!(client.receive(&bundle(&first)).unwrap().len(), 4);
        let mut restore = catchup(1);
        restore[0] = LS_RESTORE;
        assert!(matches!(
            client.receive(&restore).unwrap()[..],
            [LsEvent::Restore { tick: 1, .. }]
        ));
        assert_eq!(
            client
                .receive(&bundle(&[(1, &[]), (2, &[])]))
                .unwrap()
                .len(),
            2
        );
        assert_eq!(client.ack_frame().payload.as_ref(), [LS_ACK, 0, 0, 0, 3]);
    }

    #[test]
    fn catchup_restores_and_resets_the_cursor() {
        let mut client = LockstepClient::new();
        client.receive(&bundle(&[(0, &[])])).unwrap();
        let mut frame = vec![LS_CATCHUP];
        frame.extend_from_slice(&40u32.to_be_bytes());
        frame.extend_from_slice(&7u64.to_be_bytes());
        frame.extend_from_slice(&3u32.to_be_bytes());
        frame.extend_from_slice(b"abc");
        let ev = client.receive(&frame).unwrap();
        assert_eq!(
            ev,
            vec![LsEvent::Restore {
                tick: 40,
                hash: 7,
                state: Bytes::from_static(b"abc")
            }]
        );
        assert_eq!(client.confirmed_tick(), 40);
        assert!(client.receive(&bundle(&[(39, &[])])).unwrap().is_empty());
        assert_eq!(client.receive(&bundle(&[(40, &[])])).unwrap().len(), 1);
    }

    #[test]
    fn an_input_is_resent_only_under_its_own_tick() {
        let mut client = LockstepClient::new();
        client.receive(&bundle(&[(9, &[])])).unwrap();
        client.send_input(Bytes::from_static(b"A")).unwrap();
        client.receive(&bundle(&[(19, &[])])).unwrap();
        let (tick, frame) = client.send_input(Bytes::from_static(b"B")).unwrap();
        assert_eq!(tick, 22);
        assert_eq!(frame.payload.as_ref(), &[LS_INPUT, 0, 0, 0, 22, 1, 1, b'B']);
    }

    #[test]
    fn input_frames_carry_redundant_inputs_and_adaptive_delay() {
        let mut client = LockstepClient::new();
        client.input_redundancy = 2;
        client.receive(&bundle(&[(9, &[])])).unwrap();
        let (t1, _) = client.send_input(Bytes::from_static(b"A")).unwrap();
        assert_eq!(t1, 12);
        let (t2, frame) = client.send_input(Bytes::from_static(b"BB")).unwrap();
        // a third input before the room moves on would be stamped too far ahead
        assert!(client.send_input(Bytes::from_static(b"C")).is_none());
        assert_eq!(t2, 13);
        assert!(!frame.reliable);
        assert_eq!(
            frame.payload.as_ref(),
            &[LS_INPUT, 0, 0, 0, 12, 2, 1, b'A', 2, b'B', b'B']
        );
        // late report: delay covers the lateness plus one; three slack reports: the excess goes
        client
            .receive(&[LS_INPUT_STATS, 0, 0, 0, 30, 0xFE, 1])
            .unwrap();
        assert_eq!(
            (client.input_delay, client.last_margin, client.late_inputs),
            (5, -2, 1)
        );
        for _ in 0..3 {
            client
                .receive(&[LS_INPUT_STATS, 0, 0, 0, 60, 3, 0])
                .unwrap();
        }
        assert_eq!(client.input_delay, 4);
        // a round trip of 12 ticks is covered by one report and given back by three
        client.receive(&bundle(&[(159, &[])])).unwrap();
        client.send_input(Bytes::from_static(b"C")).unwrap();
        client.on_input_stats(170, -12, 30);
        assert_eq!(client.input_delay, 17);
        // the next report still covers inputs stamped before the raise
        client.on_input_stats(200, -12, 30);
        assert_eq!((client.input_delay, client.late_inputs), (17, 61));
        for (tick, margin) in [(230, 9), (260, 6), (290, 7)] {
            client.on_input_stats(tick, margin, 0);
        }
        assert_eq!(client.input_delay, 13);
        client.input_delay = 4;
        client.input_delay_max = 4;
        client.on_input_stats(330, -100, 0);
        assert_eq!(client.input_delay, 4);
        client.adaptive_delay = false;
        client.on_input_stats(360, -5, 2);
        assert_eq!((client.input_delay, client.late_inputs), (4, 63));
    }

    #[test]
    fn requests_and_answer_frames() {
        let mut client = LockstepClient::new();
        assert_eq!(
            client.receive(&[LS_CHECKPOINT_REQ, 0, 0, 1, 0]).unwrap(),
            vec![LsEvent::CheckpointRequested(256)]
        );
        assert_eq!(
            client.receive(&[LS_DUMP_REQ, 0, 0, 0, 9]).unwrap(),
            vec![LsEvent::DumpRequested(9)]
        );
        assert_eq!(client.receive(&[0x11, 1, 2]), None);
        let dump = LockstepClient::dump_frame(9, 0x0102030405060708, b"st");
        assert!(dump.reliable);
        assert_eq!(
            dump.payload.as_ref(),
            &[LS_DUMP, 0, 0, 0, 9, 1, 2, 3, 4, 5, 6, 7, 8, b's', b't']
        );
        assert_eq!(LockstepClient::hash_frame(1, 2).payload.len(), 13);
        assert_eq!(LockstepClient::fnv1a(b""), 0xcbf29ce484222325);
        assert_eq!(LockstepClient::fnv1a(b"a"), 0xaf63dc4c8601ec8c);
    }
}
