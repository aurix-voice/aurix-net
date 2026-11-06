//! Messages larger than one datagram are split into fragments carried on the
//! `Fragment` kind over the reliable-unordered machinery and reassembled by message id.
//!
//! Fragment payload layout: `u16 msg_id, u8 channel, u8 index, u8 count, bytes…`

use bytes::{Buf, BufMut, Bytes, BytesMut};
use net_common::limits::{MAX_MESSAGE, MAX_UNFRAGMENTED};
use net_common::Channel;
use std::collections::HashMap;
use std::time::{Duration, Instant};

const FRAG_HDR: usize = 5;
pub const FRAG_PAYLOAD: usize = MAX_UNFRAGMENTED - FRAG_HDR;
const REASSEMBLY_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_PARTIAL: usize = 64;

#[derive(Debug, Default)]
pub struct Fragmenter {
    next_id: u16,
}

impl Fragmenter {
    /// Returns `None` if the message exceeds MAX_MESSAGE.
    pub fn split(&mut self, channel: Channel, msg: &[u8]) -> Option<Vec<Bytes>> {
        if msg.len() > MAX_MESSAGE {
            return None;
        }
        let count = msg.len().div_ceil(FRAG_PAYLOAD).max(1);
        if count > u8::MAX as usize {
            return None;
        }
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        Some(
            msg.chunks(FRAG_PAYLOAD)
                .enumerate()
                .map(|(i, c)| {
                    let mut b = BytesMut::with_capacity(FRAG_HDR + c.len());
                    b.put_u16(id);
                    b.put_u8(channel as u8);
                    b.put_u8(i as u8);
                    b.put_u8(count as u8);
                    b.extend_from_slice(c);
                    b.freeze()
                })
                .collect(),
        )
    }
}

#[derive(Debug)]
struct Partial {
    channel: Channel,
    parts: Vec<Option<Bytes>>,
    received: usize,
    started: Instant,
}

#[derive(Debug, Default)]
pub struct Reassembler {
    partial: HashMap<u16, Partial>,
}

impl Reassembler {
    /// Feeds one fragment; returns the full message when complete.
    pub fn accept(&mut self, mut frag: Bytes, now: Instant) -> Option<(Channel, Bytes)> {
        if frag.len() < FRAG_HDR {
            return None;
        }
        let id = frag.get_u16();
        let channel = Channel::from_u8(frag.get_u8())?;
        let index = frag.get_u8() as usize;
        let count = frag.get_u8() as usize;
        if count == 0 || index >= count {
            return None;
        }
        self.partial
            .retain(|_, p| now.duration_since(p.started) < REASSEMBLY_TIMEOUT);
        if !self.partial.contains_key(&id) && self.partial.len() >= MAX_PARTIAL {
            return None;
        }
        let p = self.partial.entry(id).or_insert_with(|| Partial {
            channel,
            parts: vec![None; count],
            received: 0,
            started: now,
        });
        if p.parts.len() != count || p.channel != channel {
            self.partial.remove(&id);
            return None;
        }
        if p.parts[index].is_none() {
            p.parts[index] = Some(frag);
            p.received += 1;
        }
        if p.received == count {
            let p = self.partial.remove(&id).unwrap();
            let mut out = BytesMut::new();
            for part in p.parts {
                out.extend_from_slice(&part.unwrap());
            }
            Some((p.channel, out.freeze()))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_and_reassemble_out_of_order() {
        let msg: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
        let mut f = Fragmenter::default();
        let mut parts = f.split(Channel::ReliableOrdered, &msg).unwrap();
        assert_eq!(parts.len(), 5000usize.div_ceil(FRAG_PAYLOAD));
        parts.reverse();
        let mut r = Reassembler::default();
        let now = Instant::now();
        let mut got = None;
        for p in parts {
            got = r.accept(p, now).or(got);
        }
        let (ch, data) = got.unwrap();
        assert_eq!(ch, Channel::ReliableOrdered);
        assert_eq!(&data[..], &msg[..]);
    }

    #[test]
    fn rejects_oversize() {
        let mut f = Fragmenter::default();
        assert!(f
            .split(Channel::ReliableOrdered, &vec![0u8; MAX_MESSAGE + 1])
            .is_none());
    }
}
