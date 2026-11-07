//! Decoders must never panic on hostile input: random bytes and bit-flipped valid packets.
use bytes::Bytes;
use net_common::Channel;
use net_transport::{packet::Header, Link};
use std::time::Instant;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn bytes(&mut self, max: usize) -> Vec<u8> {
        let n = (self.next() % (max as u64 + 1)) as usize;
        (0..n).map(|_| self.next() as u8).collect()
    }
}

#[test]
fn header_and_link_survive_garbage() {
    let mut rng = Rng(0x9e3779b97f4a7c15);
    let now = Instant::now();
    let mut link = Link::new(now);
    for _ in 0..20_000 {
        let b = rng.bytes(300);
        let _ = Header::decode(Bytes::from(b.clone()));
        let _ = link.receive(Bytes::from(b), now);
    }
    // valid traffic with random corruption on the receiving side
    let mut a = Link::new(now);
    let mut b = Link::new(now);
    for i in 0..5_000u32 {
        let ch = match i % 3 {
            0 => Channel::UnreliableSequenced,
            1 => Channel::ReliableOrdered,
            _ => Channel::ReliableUnordered,
        };
        let msg = rng.bytes(if i.is_multiple_of(50) { 3000 } else { 200 });
        let _ = a.send(ch, Bytes::from(msg), now);
        a.flush_batch();
        while let Some(d) = a.next_outgoing() {
            let mut v = d.to_vec();
            if !v.is_empty() && rng.next().is_multiple_of(4) {
                let idx = (rng.next() as usize) % v.len();
                v[idx] ^= 1 << (rng.next() % 8);
            }
            if rng.next().is_multiple_of(5) {
                v.truncate((rng.next() as usize) % (v.len() + 1));
            }
            let _ = b.receive(Bytes::from(v), now);
        }
        while let Some(d) = b.next_outgoing() {
            let _ = a.receive(d, now);
        }
    }
}
