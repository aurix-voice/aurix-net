//! A [`Link`] multiplexes the three channels (plus fragments) onto a stream of datagrams.
//! It is sans-IO: the caller feeds incoming datagrams and drains outgoing ones, which keeps
//! it identical over QUIC, WebTransport and the WebSocket tunnel and trivially testable.

use crate::channel::{ReliableRx, ReliableTx, SequencedRx};
use crate::fragment::{Fragmenter, Reassembler};
use crate::packet::{Header, Kind, PacketError};
use bytes::{Buf, Bytes, BytesMut};
use net_common::limits::{HEADER_LEN, MAX_UNFRAGMENTED, RELIABLE_BACKLOG_BYTES};
use net_common::Channel;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct LinkStats {
    pub rtt: Duration,
    pub rtt_var: Duration,
    pub sent_datagrams: u64,
    pub recv_datagrams: u64,
    pub sent_bytes: u64,
    pub recv_bytes: u64,
    pub retransmits: u64,
    pub dropped_old: u64,
    pub decode_errors: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Received {
    pub channel: Channel,
    pub tick: u16,
    pub payload: Bytes,
}

#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    #[error("message too large")]
    TooLarge,
    #[error("peer stopped acknowledging; link dead")]
    Dead,
    #[error("reliable messages outran the peer; link dead")]
    Backlog,
}

const MIN_RTO: Duration = Duration::from_millis(60);
const MAX_RTO: Duration = Duration::from_millis(1500);
const ACK_DELAY: Duration = Duration::from_millis(20);

#[derive(Debug)]
pub struct Link {
    seq_tx: u16,
    seq_rx: SequencedRx,
    ordered_tx: ReliableTx,
    ordered_rx: ReliableRx,
    unordered_tx: ReliableTx,
    unordered_rx: ReliableRx,
    frag_tx: ReliableTx,
    frag_rx: ReliableRx,
    fragmenter: Fragmenter,
    reassembler: Reassembler,
    out: VecDeque<Bytes>,
    /// Small unreliable messages waiting to share one datagram (`u16 len` prefixed).
    batch: BytesMut,
    batch_n: usize,
    /// The same for the unsequenced channel, which has datagrams and a counter of its own.
    loose: BytesMut,
    loose_n: usize,
    loose_tx: u16,
    stats: LinkStats,
    srtt: Option<Duration>,
    rttvar: Duration,
    /// The carrier's idle timeout.
    idle: Duration,
    /// Local tick stamped into outgoing headers.
    pub tick: u16,
    ack_dirty: bool,
    last_ack_sent: Instant,
    last_recv: Instant,
    dead: bool,
}

impl Link {
    /// `idle` is the idle timeout of what carries the datagrams: a link with a reliable packet
    /// that stays unacknowledged that long is dead.
    pub fn new(now: Instant, idle: Duration) -> Self {
        Self {
            seq_tx: 0,
            seq_rx: SequencedRx::default(),
            ordered_tx: ReliableTx::default(),
            ordered_rx: ReliableRx::new(true),
            unordered_tx: ReliableTx::default(),
            unordered_rx: ReliableRx::new(false),
            frag_tx: ReliableTx::default(),
            frag_rx: ReliableRx::new(false),
            fragmenter: Fragmenter::default(),
            reassembler: Reassembler::default(),
            out: VecDeque::new(),
            batch: BytesMut::with_capacity(MAX_UNFRAGMENTED),
            batch_n: 0,
            loose: BytesMut::with_capacity(MAX_UNFRAGMENTED),
            loose_n: 0,
            loose_tx: 0,
            stats: LinkStats::default(),
            srtt: None,
            rttvar: Duration::ZERO,
            idle,
            tick: 0,
            ack_dirty: false,
            last_ack_sent: now,
            last_recv: now,
            dead: false,
        }
    }

    pub fn stats(&self) -> LinkStats {
        self.stats
    }

    pub fn rto(&self) -> Duration {
        match self.srtt {
            None => Duration::from_millis(250),
            Some(srtt) => (srtt + 4 * self.rttvar).clamp(MIN_RTO, MAX_RTO),
        }
    }

    /// Bytes of reliable messages waiting for the window.
    pub fn backlog(&self) -> usize {
        self.ordered_tx.pending_bytes()
            + self.unordered_tx.pending_bytes()
            + self.frag_tx.pending_bytes()
    }

    /// Ends the link: nothing more is sent or queued, and what was queued is dropped.
    pub fn close(&mut self) {
        self.dead = true;
        for tx in [
            &mut self.ordered_tx,
            &mut self.unordered_tx,
            &mut self.frag_tx,
        ] {
            tx.clear();
        }
        self.out = VecDeque::new();
        self.batch.clear();
        self.batch_n = 0;
        self.loose.clear();
        self.loose_n = 0;
    }

    pub fn last_recv(&self) -> Instant {
        self.last_recv
    }

    pub fn is_dead(&self) -> bool {
        self.dead
    }

    /// Acks are shared across reliable channels via a single (ack, bits) pair per kind;
    /// to keep the header small each channel carries its own ack in packets of that kind.
    fn ack_for(&self, kind: Kind) -> Option<(u16, u32)> {
        match kind {
            Kind::Channel(Channel::ReliableOrdered) => self.ordered_rx.ack_state(),
            Kind::Channel(Channel::ReliableUnordered) => self.unordered_rx.ack_state(),
            Kind::Fragment => self.frag_rx.ack_state(),
            Kind::Channel(Channel::UnreliableSequenced | Channel::UnreliableUnsequenced) => None,
        }
    }

    fn header(&self, kind: Kind, seq: u16, ack_only: bool) -> Header {
        let ack = self.ack_for(kind);
        Header {
            kind,
            ack_only,
            has_ack: ack.is_some(),
            batch: false,
            seq,
            ack: ack.map(|a| a.0).unwrap_or(0),
            ack_bits: ack.map(|a| a.1).unwrap_or(0),
            tick: self.tick,
        }
    }

    fn push_datagram(&mut self, header: Header, payload: &[u8]) {
        let mut buf = BytesMut::with_capacity(HEADER_LEN + payload.len());
        header.encode(&mut buf);
        buf.extend_from_slice(payload);
        self.stats.sent_datagrams += 1;
        self.stats.sent_bytes += buf.len() as u64;
        self.out.push_back(buf.freeze());
    }

    fn emit(&mut self, kind: Kind, seq: u16, payload: &[u8], ack_only: bool) {
        let header = self.header(kind, seq, ack_only);
        self.push_datagram(header, payload);
    }

    /// Queues an application message on `channel`.
    pub fn send(&mut self, channel: Channel, msg: Bytes, now: Instant) -> Result<(), LinkError> {
        if self.dead {
            return Err(LinkError::Dead);
        }
        let queued = msg.len() > MAX_UNFRAGMENTED
            || matches!(
                channel,
                Channel::ReliableOrdered | Channel::ReliableUnordered
            );
        if queued && self.backlog() + msg.len() > RELIABLE_BACKLOG_BYTES {
            self.close();
            return Err(LinkError::Backlog);
        }
        if msg.len() > MAX_UNFRAGMENTED {
            let parts = self
                .fragmenter
                .split(channel, &msg)
                .ok_or(LinkError::TooLarge)?;
            for part in parts {
                for (seq, payload) in self.frag_tx.push(part, now) {
                    self.emit(Kind::Fragment, seq, &payload, false);
                }
            }
            return Ok(());
        }
        match channel {
            Channel::UnreliableSequenced => {
                // Coalesce: several small messages share one datagram until flush_batch().
                if self.batch.len() + 2 + msg.len() > MAX_UNFRAGMENTED {
                    self.flush_batch();
                }
                self.batch
                    .extend_from_slice(&(msg.len() as u16).to_be_bytes());
                self.batch.extend_from_slice(&msg);
                self.batch_n += 1;
            }
            Channel::UnreliableUnsequenced => {
                if self.loose.len() + 2 + msg.len() > MAX_UNFRAGMENTED {
                    self.flush_loose();
                }
                self.loose
                    .extend_from_slice(&(msg.len() as u16).to_be_bytes());
                self.loose.extend_from_slice(&msg);
                self.loose_n += 1;
            }
            Channel::ReliableOrdered => {
                for (seq, payload) in self.ordered_tx.push(msg, now) {
                    self.emit(Kind::Channel(channel), seq, &payload, false);
                }
            }
            Channel::ReliableUnordered => {
                for (seq, payload) in self.unordered_tx.push(msg, now) {
                    self.emit(Kind::Channel(channel), seq, &payload, false);
                }
            }
        }
        Ok(())
    }

    fn sample_rtt(&mut self, samples: Vec<Duration>) {
        for sample in samples {
            match self.srtt {
                None => {
                    self.srtt = Some(sample);
                    self.rttvar = sample / 2;
                }
                Some(srtt) => {
                    let diff = srtt.abs_diff(sample);
                    self.rttvar = (self.rttvar * 3 + diff) / 4;
                    self.srtt = Some((srtt * 7 + sample) / 8);
                }
            }
        }
        self.stats.rtt = self.srtt.unwrap_or_default();
        self.stats.rtt_var = self.rttvar;
    }

    /// The messages of one unreliable datagram.
    fn unbatch(
        &mut self,
        channel: Channel,
        header: &Header,
        payload: Bytes,
        out: &mut Vec<Received>,
    ) {
        if !header.batch {
            out.push(Received {
                channel,
                tick: header.tick,
                payload,
            });
            return;
        }
        let mut rest = payload;
        while rest.len() >= 2 {
            let len = u16::from_be_bytes([rest[0], rest[1]]) as usize;
            rest.advance(2);
            if rest.len() < len {
                self.stats.decode_errors += 1;
                break;
            }
            out.push(Received {
                channel,
                tick: header.tick,
                payload: rest.split_to(len),
            });
        }
    }

    /// Feeds one incoming datagram; returns messages ready for the application.
    pub fn receive(&mut self, datagram: Bytes, now: Instant) -> Vec<Received> {
        self.stats.recv_datagrams += 1;
        self.stats.recv_bytes += datagram.len() as u64;
        let (header, payload) = match Header::decode(datagram) {
            Ok(decoded) => decoded,
            Err(PacketError::TooShort)
            | Err(PacketError::Version(_))
            | Err(PacketError::Kind(_)) => {
                self.stats.decode_errors += 1;
                return Vec::new();
            }
        };
        self.last_recv = now;
        let mut out = Vec::new();
        match header.kind {
            Kind::Channel(Channel::UnreliableSequenced) => {
                if header.ack_only {
                    return out;
                }
                if self.seq_rx.accept(header.seq) {
                    self.unbatch(Channel::UnreliableSequenced, &header, payload, &mut out);
                } else {
                    self.stats.dropped_old += 1;
                }
            }
            Kind::Channel(Channel::UnreliableUnsequenced) => {
                if header.ack_only {
                    return out;
                }
                self.unbatch(Channel::UnreliableUnsequenced, &header, payload, &mut out);
            }
            Kind::Channel(Channel::ReliableOrdered) => {
                if header.has_ack {
                    let samples = self.ordered_tx.on_ack(header.ack, header.ack_bits, now);
                    self.sample_rtt(samples);
                }
                if !header.ack_only {
                    self.ack_dirty = true;
                    for message in self.ordered_rx.accept(header.seq, payload) {
                        out.push(Received {
                            channel: Channel::ReliableOrdered,
                            tick: header.tick,
                            payload: message,
                        });
                    }
                }
            }
            Kind::Channel(Channel::ReliableUnordered) => {
                if header.has_ack {
                    let samples = self.unordered_tx.on_ack(header.ack, header.ack_bits, now);
                    self.sample_rtt(samples);
                }
                if !header.ack_only {
                    self.ack_dirty = true;
                    for message in self.unordered_rx.accept(header.seq, payload) {
                        out.push(Received {
                            channel: Channel::ReliableUnordered,
                            tick: header.tick,
                            payload: message,
                        });
                    }
                }
            }
            Kind::Fragment => {
                if header.has_ack {
                    let samples = self.frag_tx.on_ack(header.ack, header.ack_bits, now);
                    self.sample_rtt(samples);
                }
                if !header.ack_only {
                    self.ack_dirty = true;
                    for frag in self.frag_rx.accept(header.seq, payload) {
                        if let Some((channel, msg)) = self.reassembler.accept(frag, now) {
                            out.push(Received {
                                channel,
                                tick: header.tick,
                                payload: msg,
                            });
                        }
                    }
                }
            }
        }
        out
    }

    /// Drives retransmits and delayed acks. Call every few milliseconds or after I/O.
    pub fn poll(&mut self, now: Instant) -> Result<(), LinkError> {
        if self.dead {
            return Err(LinkError::Dead);
        }
        self.flush_batch();
        let rto = self.rto();
        let mut waiting_since: Option<Instant> = None;
        let mut send: Vec<(Kind, u16, Bytes)> = Vec::new();
        for (kind, tx) in [
            (
                Kind::Channel(Channel::ReliableOrdered),
                &mut self.ordered_tx,
            ),
            (
                Kind::Channel(Channel::ReliableUnordered),
                &mut self.unordered_tx,
            ),
            (Kind::Fragment, &mut self.frag_tx),
        ] {
            let mut packets = tx.repeat(now, rto);
            self.stats.retransmits += packets.len() as u64;
            packets.extend(tx.drain_window(now));
            send.extend(
                packets
                    .into_iter()
                    .map(|(seq, payload)| (kind, seq, payload)),
            );
            waiting_since = match (waiting_since, tx.waiting_since()) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
        }
        let unacknowledged = waiting_since.map(|since| now.duration_since(since));
        if unacknowledged.is_some_and(|waited| waited >= self.idle) {
            self.close();
            return Err(LinkError::Dead);
        }
        for (kind, seq, payload) in send {
            let header = self.header(kind, seq, false);
            self.push_datagram(header, &payload);
        }
        if self.ack_dirty && now.duration_since(self.last_ack_sent) >= ACK_DELAY {
            self.ack_dirty = false;
            self.last_ack_sent = now;
            for kind in [
                Kind::Channel(Channel::ReliableOrdered),
                Kind::Channel(Channel::ReliableUnordered),
                Kind::Fragment,
            ] {
                if self.ack_for(kind).is_some() {
                    self.emit(kind, 0, &[], true);
                }
            }
        }
        Ok(())
    }

    /// Emits the pending unreliable batches, a datagram for each channel. Called by `poll()`;
    /// call it directly when a message must leave immediately.
    pub fn flush_batch(&mut self) {
        if self.batch_n > 0 {
            let seq = self.seq_tx;
            self.seq_tx = self.seq_tx.wrapping_add(1);
            let mut header = self.header(Kind::Channel(Channel::UnreliableSequenced), seq, false);
            header.batch = true;
            let payload = self.batch.split().freeze();
            self.batch_n = 0;
            self.push_datagram(header, &payload);
        }
        self.flush_loose();
    }

    fn flush_loose(&mut self) {
        if self.loose_n == 0 {
            return;
        }
        let seq = self.loose_tx;
        self.loose_tx = self.loose_tx.wrapping_add(1);
        let mut header = self.header(Kind::Channel(Channel::UnreliableUnsequenced), seq, false);
        header.batch = true;
        let payload = self.loose.split().freeze();
        self.loose_n = 0;
        self.push_datagram(header, &payload);
    }

    /// Next datagram to put on the wire.
    pub fn next_outgoing(&mut self) -> Option<Bytes> {
        self.out.pop_front()
    }

    pub fn outgoing_len(&self) -> usize {
        self.out.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDLE: Duration = Duration::from_secs(15);

    /// Delivers a's outgoing datagrams to b, dropping roughly `drop_pct` percent with a
    /// deterministic LCG so the test is reproducible.
    fn pump(
        from: &mut Link,
        to: &mut Link,
        now: Instant,
        drop_pct: u32,
        rng: &mut u32,
    ) -> Vec<Received> {
        let mut got = Vec::new();
        while let Some(datagram) = from.next_outgoing() {
            *rng = rng.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            if (*rng >> 16) % 100 < drop_pct {
                continue;
            }
            got.extend(to.receive(datagram, now));
        }
        got
    }

    #[test]
    fn reliable_ordered_survives_loss() {
        let t0 = Instant::now();
        let mut near = Link::new(t0, IDLE);
        let mut far = Link::new(t0, IDLE);
        for i in 0..100u32 {
            near.send(
                Channel::ReliableOrdered,
                Bytes::from(i.to_le_bytes().to_vec()),
                t0,
            )
            .unwrap();
        }
        let mut rng = 42u32;
        let mut received = pump(&mut near, &mut far, t0, 30, &mut rng);
        let mut now = t0;
        for _ in 0..200 {
            now += Duration::from_millis(300);
            far.poll(now).unwrap();
            pump(&mut far, &mut near, now, 30, &mut rng);
            near.poll(now).unwrap();
            received.extend(pump(&mut near, &mut far, now, 30, &mut rng));
            if received.len() == 100 {
                break;
            }
        }
        assert_eq!(received.len(), 100);
        for (i, got) in received.iter().enumerate() {
            assert_eq!(
                u32::from_le_bytes(got.payload[..].try_into().unwrap()),
                i as u32
            );
        }
    }

    #[test]
    fn large_message_is_fragmented_and_rebuilt() {
        let t0 = Instant::now();
        let mut near = Link::new(t0, IDLE);
        let mut far = Link::new(t0, IDLE);
        let msg = Bytes::from(vec![7u8; 20_000]);
        near.send(Channel::ReliableUnordered, msg.clone(), t0)
            .unwrap();
        assert!(near.outgoing_len() > 10);
        let got = pump(&mut near, &mut far, t0, 0, &mut 1u32);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].channel, Channel::ReliableUnordered);
        assert_eq!(got[0].payload, msg);
    }

    #[test]
    fn unsequenced_delivers_what_arrives_in_any_order() {
        let t0 = Instant::now();
        let mut near = Link::new(t0, IDLE);
        let mut far = Link::new(t0, IDLE);
        for msg in [&b"1"[..], b"2", b"3"] {
            near.send(Channel::UnreliableUnsequenced, Bytes::from_static(msg), t0)
                .unwrap();
            near.flush_batch();
        }
        let sent: Vec<Bytes> = std::iter::from_fn(|| near.next_outgoing()).collect();
        assert_eq!(sent.len(), 3);
        let mut got = Vec::new();
        for datagram in [&sent[2], &sent[0], &sent[1]] {
            got.extend(far.receive(datagram.clone(), t0));
        }
        let payloads: Vec<&[u8]> = got.iter().map(|m| &m.payload[..]).collect();
        assert_eq!(payloads, [&b"3"[..], b"1", b"2"]);
        assert!(got
            .iter()
            .all(|m| m.channel == Channel::UnreliableUnsequenced));
        assert_eq!(far.stats().dropped_old, 0);
    }

    #[test]
    fn sequenced_drops_reordered_old() {
        let t0 = Instant::now();
        let mut near = Link::new(t0, IDLE);
        let mut far = Link::new(t0, IDLE);
        near.send(Channel::UnreliableSequenced, Bytes::from_static(b"1"), t0)
            .unwrap();
        near.flush_batch();
        near.send(Channel::UnreliableSequenced, Bytes::from_static(b"2"), t0)
            .unwrap();
        near.flush_batch();
        let d1 = near.next_outgoing().unwrap();
        let d2 = near.next_outgoing().unwrap();
        assert_eq!(far.receive(d2, t0).len(), 1);
        assert_eq!(far.receive(d1, t0).len(), 0);
        assert_eq!(far.stats().dropped_old, 1);
    }

    #[test]
    fn unreliable_messages_coalesce() {
        let t0 = Instant::now();
        let mut near = Link::new(t0, IDLE);
        let mut far = Link::new(t0, IDLE);
        for i in 0..20u8 {
            near.send(Channel::UnreliableSequenced, Bytes::from(vec![i; 40]), t0)
                .unwrap();
        }
        assert_eq!(near.outgoing_len(), 0);
        near.flush_batch();
        assert_eq!(near.outgoing_len(), 1);
        let got = pump(&mut near, &mut far, t0, 0, &mut 1u32);
        assert_eq!(got.len(), 20);
        assert_eq!(got[7].payload, Bytes::from(vec![7u8; 40]));
        for _ in 0..40 {
            near.send(
                Channel::UnreliableSequenced,
                Bytes::from(vec![1u8; 100]),
                t0,
            )
            .unwrap();
        }
        near.flush_batch();
        assert!(near.outgoing_len() >= 4);
        assert_eq!(pump(&mut near, &mut far, t0, 0, &mut 1u32).len(), 40);
    }

    #[test]
    fn link_dies_without_acks() {
        let t0 = Instant::now();
        let mut near = Link::new(t0, IDLE);
        near.send(Channel::ReliableOrdered, Bytes::from_static(b"x"), t0)
            .unwrap();
        let mut now = t0;
        let mut dead = false;
        for _ in 0..40 {
            now += Duration::from_secs(2);
            while near.next_outgoing().is_some() {}
            if near.poll(now).is_err() {
                dead = true;
                break;
            }
        }
        assert!(dead);
    }

    /// What `link` puts on the wire now, as (reliable ordered seq, is an acknowledgement).
    fn sent(link: &mut Link) -> Vec<(u16, bool)> {
        std::iter::from_fn(|| link.next_outgoing())
            .map(|datagram| {
                let (header, _) = Header::decode(datagram).expect("header");
                (header.seq, header.ack_only)
            })
            .collect()
    }

    /// Sends `count` ordered messages from `near` and delivers all but `lost` to `far`.
    fn send_losing(near: &mut Link, far: &mut Link, count: u8, lost: &[u16], now: Instant) {
        for index in 0..count {
            near.send(Channel::ReliableOrdered, Bytes::from(vec![index]), now)
                .unwrap();
        }
        while let Some(datagram) = near.next_outgoing() {
            let (header, _) = Header::decode(datagram.clone()).expect("header");
            if !lost.contains(&header.seq) {
                far.receive(datagram, now);
            }
        }
    }

    /// Carries the acknowledgements of `far` to `near` at `now`.
    fn acknowledge(far: &mut Link, near: &mut Link, now: Instant) {
        far.poll(now).unwrap();
        while let Some(datagram) = far.next_outgoing() {
            near.receive(datagram, now);
        }
        near.poll(now).unwrap();
    }

    #[test]
    fn first_sends_from_the_queue_are_not_counted_as_repeats() {
        let t0 = Instant::now();
        let (mut near, mut far) = (Link::new(t0, IDLE), Link::new(t0, IDLE));
        send_losing(&mut near, &mut far, 40, &[], t0);
        acknowledge(&mut far, &mut near, t0 + Duration::from_millis(30));
        assert_eq!(sent(&mut near).len(), 8);
        assert_eq!(near.stats().retransmits, 0);
    }

    #[test]
    fn link_dies_when_nothing_is_heard_for_the_idle_timeout() {
        let t0 = Instant::now();
        let mut near = Link::new(t0, IDLE);
        near.poll(t0 + 4 * IDLE)
            .expect("nothing waits for the peer");
        let start = t0 + 4 * IDLE;
        near.send(Channel::ReliableOrdered, Bytes::from_static(b"x"), start)
            .unwrap();
        near.poll(start + IDLE - Duration::from_millis(1)).unwrap();
        assert!(matches!(near.poll(start + IDLE), Err(LinkError::Dead)));
        assert!(near.is_dead());
    }

    #[test]
    fn link_dies_when_a_packet_stays_unacknowledged_while_the_peer_talks() {
        let t0 = Instant::now();
        let (mut near, mut far) = (Link::new(t0, IDLE), Link::new(t0, IDLE));
        near.send(Channel::ReliableOrdered, Bytes::from_static(b"x"), t0)
            .unwrap();
        let mut now = t0;
        let died = loop {
            // the reliable packet never arrives; the peer keeps sending
            while near.next_outgoing().is_some() {}
            far.send(Channel::UnreliableSequenced, Bytes::from_static(b"y"), now)
                .unwrap();
            far.poll(now).unwrap();
            while let Some(datagram) = far.next_outgoing() {
                near.receive(datagram, now);
            }
            if near.poll(now).is_err() {
                break now;
            }
            now += Duration::from_millis(100);
        };
        assert_eq!(died, t0 + IDLE);
        assert!(near.is_dead());
    }

    #[test]
    fn sender_that_outruns_its_peer_loses_the_link() {
        let t0 = Instant::now();
        let mut near = Link::new(t0, IDLE);
        let message = Bytes::from(vec![0u8; 60_000]);
        let mut accepted = 0;
        let refused = loop {
            match near.send(Channel::ReliableOrdered, message.clone(), t0) {
                Ok(()) => accepted += 1,
                Err(error) => break error,
            }
        };
        assert!(matches!(refused, LinkError::Backlog));
        // the window takes 32 fragments of the first message; sixteen more fill the megabyte
        assert_eq!(accepted, 17);
        assert!(near.is_dead());
        assert_eq!(near.backlog(), 0);
        assert!(near.next_outgoing().is_none());
    }

    #[test]
    fn dead_link_refuses_to_send_and_keeps_nothing() {
        let t0 = Instant::now();
        let mut near = Link::new(t0, IDLE);
        for _ in 0..100 {
            near.send(Channel::ReliableOrdered, Bytes::from_static(b"x"), t0)
                .unwrap();
        }
        assert!(near.poll(t0 + IDLE).is_err());
        assert_eq!(near.ordered_tx.in_flight() + near.ordered_tx.pending(), 0);
        assert!(near.next_outgoing().is_none());
        for channel in [Channel::ReliableOrdered, Channel::UnreliableSequenced] {
            let refused = near.send(channel, Bytes::from_static(b"x"), t0 + IDLE);
            assert!(matches!(refused, Err(LinkError::Dead)));
        }
        assert_eq!(near.ordered_tx.pending(), 0);
        assert!(near.poll(t0 + 2 * IDLE).is_err());
    }
}
