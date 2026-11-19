//! Per-channel send/receive state. Reliability is our own (acks + retransmit) on top of
//! unreliable datagrams so that nothing blocks behind a lost packet.

use crate::packet::seq_newer;
use bytes::Bytes;
use net_common::limits::RELIABLE_WINDOW;
use net_common::Channel;
use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

/// Receive side of `UnreliableSequenced`: deliver only if newer than the last delivered.
#[derive(Debug, Default)]
pub struct SequencedRx {
    last: Option<u16>,
}

impl SequencedRx {
    pub fn accept(&mut self, seq: u16) -> bool {
        match self.last {
            Some(last) if !seq_newer(seq, last) => false,
            _ => {
                self.last = Some(seq);
                true
            }
        }
    }
}

#[derive(Debug)]
pub struct InFlight {
    pub seq: u16,
    pub payload: Bytes,
    pub first_sent: Instant,
    pub sent_at: Instant,
    pub resent: bool,
}

/// A reliable message and where its packets are among those queued on a channel.
#[derive(Debug)]
pub struct Queued {
    /// Its place among the reliable messages of the link.
    pub order: u64,
    pub mark: u32,
    pub channel: Channel,
    pub msg: Bytes,
    first: u64,
    count: u64,
}

/// Send side of a reliable channel: a sliding window of unacked packets.
#[derive(Debug, Default)]
pub struct ReliableTx {
    next_seq: u16,
    in_flight: VecDeque<InFlight>,
    pending: VecDeque<Bytes>,
    pending_bytes: usize,
    /// Packets ever queued: the nth of them gets seq n mod 65536.
    queued: u64,
    /// The reliable messages queued, from the oldest one not acknowledged yet.
    log: VecDeque<Queued>,
}

impl ReliableTx {
    /// Queues a payload; returns packets that may be sent now (window permitting).
    pub fn push(&mut self, payload: Bytes, now: Instant) -> Vec<(u16, Bytes)> {
        self.queued += 1;
        self.pending_bytes += payload.len();
        self.pending.push_back(payload);
        self.drain_window(now)
    }

    /// Notes a reliable message whose `count` packets are queued next.
    pub fn note(&mut self, order: u64, mark: u32, channel: Channel, msg: Bytes, count: usize) {
        self.log.push_back(Queued {
            order,
            mark,
            channel,
            msg,
            first: self.queued,
            count: count as u64,
        });
    }

    /// Whether a packet of `message` waits for the window or for its acknowledgement.
    fn holds(&self, message: &Queued) -> bool {
        let sent = self.queued - self.pending.len() as u64;
        (message.first..message.first + message.count)
            .any(|n| n >= sent || self.in_flight.iter().any(|p| p.seq == n as u16))
    }

    /// The reliable messages with a packet the peer has not acknowledged.
    pub fn waiting(&self) -> impl Iterator<Item = &Queued> {
        self.log.iter().filter(|message| self.holds(message))
    }

    /// Sends what waits for the window, as far as the window allows.
    pub fn drain_window(&mut self, now: Instant) -> Vec<(u16, Bytes)> {
        let mut out = Vec::new();
        loop {
            // Bound the *span* of outstanding seqs, not just their count: the receiver can
            // only acknowledge 32 seqs behind the newest one it has seen.
            let oldest = self
                .in_flight
                .front()
                .map(|p| p.seq)
                .unwrap_or(self.next_seq);
            if self.next_seq.wrapping_sub(oldest) >= RELIABLE_WINDOW {
                break;
            }
            let Some(packet) = self.pending.pop_front() else {
                break;
            };
            self.pending_bytes -= packet.len();
            let seq = self.next_seq;
            self.next_seq = self.next_seq.wrapping_add(1);
            self.in_flight.push_back(InFlight {
                seq,
                payload: packet.clone(),
                first_sent: now,
                sent_at: now,
                resent: false,
            });
            out.push((seq, packet));
        }
        out
    }

    /// Processes a cumulative ack + bitfield. Returns RTT samples for acked packets.
    pub fn on_ack(&mut self, ack: u16, ack_bits: u32, now: Instant) -> Vec<Duration> {
        let mut rtts = Vec::new();
        self.in_flight.retain(|p| {
            let acked = if p.seq == ack {
                true
            } else if seq_newer(ack, p.seq) {
                let behind = ack.wrapping_sub(p.seq);
                behind <= 32 && (ack_bits >> (behind - 1)) & 1 == 1
            } else {
                false
            };
            if acked && !p.resent {
                rtts.push(now.duration_since(p.sent_at));
            }
            !acked
        });
        while self.log.front().is_some_and(|message| !self.holds(message)) {
            self.log.pop_front();
        }
        rtts
    }

    /// Returns the packets to retransmit: those sent `rto` ago or earlier.
    pub fn repeat(&mut self, now: Instant, rto: Duration) -> Vec<(u16, Bytes)> {
        let mut out = Vec::new();
        for packet in self.in_flight.iter_mut() {
            if now.duration_since(packet.sent_at) >= rto {
                packet.resent = true;
                packet.sent_at = now;
                out.push((packet.seq, packet.payload.clone()));
            }
        }
        out
    }

    /// The seq the next packet queued will get.
    pub fn queued_up_to(&self) -> u16 {
        self.next_seq.wrapping_add(self.pending.len() as u16)
    }

    /// Whether every packet before `seq` is acknowledged.
    pub fn acknowledged_up_to(&self, seq: u16) -> bool {
        let oldest = self.in_flight.front().map_or(self.next_seq, |p| p.seq);
        !seq_newer(seq, oldest)
    }

    /// Whether a packet that was repeated has waited `idle` for its acknowledgement. One that
    /// has waited as long without a repeat, the timer having run late, waits anew from `now`:
    /// the next [`Self::repeat`] sends it.
    pub fn given_up(&mut self, now: Instant, idle: Duration) -> bool {
        let mut given_up = false;
        for packet in self.in_flight.iter_mut() {
            if now.duration_since(packet.first_sent) >= idle {
                if packet.resent {
                    given_up = true;
                } else {
                    packet.first_sent = now;
                }
            }
        }
        given_up
    }

    /// Drops everything queued and in flight.
    pub fn clear(&mut self) {
        self.in_flight = VecDeque::new();
        self.pending = VecDeque::new();
        self.pending_bytes = 0;
        self.log = VecDeque::new();
    }

    pub fn in_flight(&self) -> usize {
        self.in_flight.len()
    }
    pub fn pending(&self) -> usize {
        self.pending.len()
    }
    /// Bytes waiting for the window.
    pub fn pending_bytes(&self) -> usize {
        self.pending_bytes
    }
}

/// Receive side of a reliable channel: dedupe, ack bookkeeping and (optionally) reordering.
#[derive(Debug)]
pub struct ReliableRx {
    ordered: bool,
    /// Highest seq seen (ack) and bitmask of 32 before it.
    ack: Option<u16>,
    ack_bits: u32,
    /// Next seq to deliver when ordered.
    next_deliver: u16,
    reorder: BTreeMap<u16, Bytes>,
    /// Recently delivered seqs for dedupe in unordered mode (relative to ack).
    seen: [u64; 4],
}

impl ReliableRx {
    pub fn new(ordered: bool) -> Self {
        Self {
            ordered,
            ack: None,
            ack_bits: 0,
            next_deliver: 0,
            reorder: BTreeMap::new(),
            seen: [0; 4],
        }
    }

    /// `None` until the first packet arrives.
    pub fn ack_state(&self) -> Option<(u16, u32)> {
        self.ack.map(|a| (a, self.ack_bits))
    }

    fn mark(&mut self, seq: u16) -> bool {
        match self.ack {
            None => {
                self.ack = Some(seq);
                self.ack_bits = 0;
                true
            }
            Some(newest) if seq == newest => false,
            Some(newest) if seq_newer(seq, newest) => {
                let shift = seq.wrapping_sub(newest) as u32;
                self.ack_bits = if shift >= 32 {
                    0
                } else {
                    (self.ack_bits << shift) | (1 << (shift - 1))
                };
                self.ack = Some(seq);
                true
            }
            Some(newest) => {
                let behind = newest.wrapping_sub(seq) as u32;
                if behind > 32 {
                    // Too old to track in the bitfield; treat as duplicate.
                    return false;
                }
                let bit = 1u32 << (behind - 1);
                if self.ack_bits & bit != 0 {
                    false
                } else {
                    self.ack_bits |= bit;
                    true
                }
            }
        }
    }

    /// Feeds a packet; returns payloads ready for the application.
    pub fn accept(&mut self, seq: u16, payload: Bytes) -> Vec<Bytes> {
        if !self.mark(seq) {
            return Vec::new();
        }
        if !self.ordered {
            let idx = (seq as usize / 64) % 4;
            let bit = 1u64 << (seq % 64);
            if self.seen[idx] & bit != 0 {
                return Vec::new();
            }
            // Clear the bucket two ahead so old seqs can be reused after wraparound.
            self.seen[(idx + 2) % 4] = 0;
            self.seen[idx] |= bit;
            return vec![payload];
        }
        if seq == self.next_deliver {
            let mut out = vec![payload];
            self.next_deliver = self.next_deliver.wrapping_add(1);
            while let Some(payload) = self.reorder.remove(&self.next_deliver) {
                out.push(payload);
                self.next_deliver = self.next_deliver.wrapping_add(1);
            }
            out
        } else if seq_newer(seq, self.next_deliver) {
            if self.reorder.len() < RELIABLE_WINDOW as usize * 2 {
                self.reorder.insert(seq, payload);
            }
            Vec::new()
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequenced_drops_old() {
        let mut rx = SequencedRx::default();
        assert!(rx.accept(5));
        assert!(!rx.accept(4));
        assert!(rx.accept(6));
        assert!(!rx.accept(6));
    }

    #[test]
    fn ordered_reorders_and_dedupes() {
        let mut rx = ReliableRx::new(true);
        assert!(rx.accept(1, Bytes::from_static(b"b")).is_empty());
        let got = rx.accept(0, Bytes::from_static(b"a"));
        assert_eq!(
            got,
            vec![Bytes::from_static(b"a"), Bytes::from_static(b"b")]
        );
        assert!(rx.accept(0, Bytes::from_static(b"a")).is_empty());
        assert_eq!(rx.ack_state(), Some((1, 0b1)));
    }

    #[test]
    fn unordered_delivers_immediately_once() {
        let mut rx = ReliableRx::new(false);
        assert_eq!(rx.accept(3, Bytes::from_static(b"d")).len(), 1);
        assert_eq!(rx.accept(1, Bytes::from_static(b"b")).len(), 1);
        assert!(rx.accept(3, Bytes::from_static(b"d")).is_empty());
        assert!(rx.accept(1, Bytes::from_static(b"b")).is_empty());
    }

    #[test]
    fn tx_acks_and_retransmits() {
        let now = Instant::now();
        let mut tx = ReliableTx::default();
        let sent = tx.push(Bytes::from_static(b"x"), now);
        assert_eq!(sent.len(), 1);
        let sent2 = tx.push(Bytes::from_static(b"y"), now);
        assert_eq!(sent2[0].0, 1);
        let samples = tx.on_ack(1, 0b1, now + Duration::from_millis(40));
        assert_eq!(samples.len(), 2);
        assert_eq!(tx.in_flight(), 0);
        tx.push(Bytes::from_static(b"z"), now);
        let again = tx.repeat(now + Duration::from_millis(500), Duration::from_millis(200));
        assert_eq!(again.len(), 1);
        assert_eq!(again[0].0, 2);
    }

    #[test]
    fn tx_window_limits_in_flight() {
        let now = Instant::now();
        let mut tx = ReliableTx::default();
        for _ in 0..300 {
            tx.push(Bytes::from_static(b"p"), now);
        }
        assert_eq!(tx.in_flight(), RELIABLE_WINDOW as usize);
        assert_eq!(tx.pending(), 300 - RELIABLE_WINDOW as usize);
    }
}
