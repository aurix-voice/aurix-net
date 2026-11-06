//! Per-channel send/receive state. Reliability is our own (acks + retransmit) on top of
//! unreliable datagrams so that nothing blocks behind a lost packet.

use crate::packet::seq_newer;
use bytes::Bytes;
use net_common::limits::RELIABLE_WINDOW;
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
            Some(l) if !seq_newer(seq, l) => false,
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
    pub sent_at: Instant,
    pub retries: u8,
}

/// Send side of a reliable channel: a sliding window of unacked packets.
#[derive(Debug)]
pub struct ReliableTx {
    next_seq: u16,
    in_flight: VecDeque<InFlight>,
    pending: VecDeque<Bytes>,
}

impl Default for ReliableTx {
    fn default() -> Self {
        Self {
            next_seq: 0,
            in_flight: VecDeque::new(),
            pending: VecDeque::new(),
        }
    }
}

pub const MAX_RETRIES: u8 = 20;

impl ReliableTx {
    /// Queues a payload; returns packets that may be sent now (window permitting).
    pub fn push(&mut self, payload: Bytes, now: Instant) -> Vec<(u16, Bytes)> {
        self.pending.push_back(payload);
        self.drain_window(now)
    }

    fn drain_window(&mut self, now: Instant) -> Vec<(u16, Bytes)> {
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
            let Some(p) = self.pending.pop_front() else {
                break;
            };
            let seq = self.next_seq;
            self.next_seq = self.next_seq.wrapping_add(1);
            self.in_flight.push_back(InFlight {
                seq,
                payload: p.clone(),
                sent_at: now,
                retries: 0,
            });
            out.push((seq, p));
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
                let d = ack.wrapping_sub(p.seq);
                d <= 32 && (ack_bits >> (d - 1)) & 1 == 1
            } else {
                false
            };
            if acked && p.retries == 0 {
                rtts.push(now.duration_since(p.sent_at));
            }
            !acked
        });
        rtts
    }

    /// Returns packets to retransmit (older than `rto`) and newly windowed packets.
    /// `Err(())` when a packet exceeded MAX_RETRIES: the link is dead.
    pub fn poll(&mut self, now: Instant, rto: Duration) -> Result<Vec<(u16, Bytes)>, ()> {
        let mut out = Vec::new();
        for p in self.in_flight.iter_mut() {
            if now.duration_since(p.sent_at) >= rto {
                if p.retries >= MAX_RETRIES {
                    return Err(());
                }
                p.retries += 1;
                p.sent_at = now;
                out.push((p.seq, p.payload.clone()));
            }
        }
        out.extend(self.drain_window(now));
        Ok(out)
    }

    pub fn in_flight(&self) -> usize {
        self.in_flight.len()
    }
    pub fn pending(&self) -> usize {
        self.pending.len()
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
            Some(a) if seq == a => false,
            Some(a) if seq_newer(seq, a) => {
                let shift = seq.wrapping_sub(a) as u32;
                self.ack_bits = if shift >= 32 {
                    0
                } else {
                    (self.ack_bits << shift) | (1 << (shift - 1))
                };
                self.ack = Some(seq);
                true
            }
            Some(a) => {
                let d = a.wrapping_sub(seq) as u32;
                if d > 32 {
                    // Too old to track in the bitfield; treat as duplicate.
                    return false;
                }
                let bit = 1u32 << (d - 1);
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
            while let Some(p) = self.reorder.remove(&self.next_deliver) {
                out.push(p);
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
        let mut r = SequencedRx::default();
        assert!(r.accept(5));
        assert!(!r.accept(4));
        assert!(r.accept(6));
        assert!(!r.accept(6));
    }

    #[test]
    fn ordered_reorders_and_dedupes() {
        let mut r = ReliableRx::new(true);
        assert!(r.accept(1, Bytes::from_static(b"b")).is_empty());
        let got = r.accept(0, Bytes::from_static(b"a"));
        assert_eq!(
            got,
            vec![Bytes::from_static(b"a"), Bytes::from_static(b"b")]
        );
        assert!(r.accept(0, Bytes::from_static(b"a")).is_empty());
        assert_eq!(r.ack_state(), Some((1, 0b1)));
    }

    #[test]
    fn unordered_delivers_immediately_once() {
        let mut r = ReliableRx::new(false);
        assert_eq!(r.accept(3, Bytes::from_static(b"d")).len(), 1);
        assert_eq!(r.accept(1, Bytes::from_static(b"b")).len(), 1);
        assert!(r.accept(3, Bytes::from_static(b"d")).is_empty());
        assert!(r.accept(1, Bytes::from_static(b"b")).is_empty());
    }

    #[test]
    fn tx_acks_and_retransmits() {
        let now = Instant::now();
        let mut tx = ReliableTx::default();
        let sent = tx.push(Bytes::from_static(b"x"), now);
        assert_eq!(sent.len(), 1);
        let sent2 = tx.push(Bytes::from_static(b"y"), now);
        assert_eq!(sent2[0].0, 1);
        let r = tx.on_ack(1, 0b1, now + Duration::from_millis(40));
        assert_eq!(r.len(), 2);
        assert_eq!(tx.in_flight(), 0);
        tx.push(Bytes::from_static(b"z"), now);
        let again = tx
            .poll(now + Duration::from_millis(500), Duration::from_millis(200))
            .unwrap();
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
