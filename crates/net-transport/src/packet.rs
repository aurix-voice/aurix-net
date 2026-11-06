//! Fixed 12-byte header in front of every datagram.
//!
//! ```text
//! u8  version        PROTOCOL_VERSION
//! u8  kind           low 2 bits: channel (0..=2) or 3 = fragment; bit 7 = ack-only;
//!                    bit 6 = ack fields are valid (receiver has seen at least one packet)
//! u16 seq            per-channel sequence number
//! u16 ack            highest seq received on the peer's reliable stream (shared ack space)
//! u32 ack_bits       bitmask of the 32 seqs before `ack`
//! u16 tick           simulation tick hint (0 in relay mode)
//! ```

use bytes::{Buf, BufMut, Bytes, BytesMut};
use net_common::limits::HEADER_LEN;
use net_common::Channel;

pub const PROTOCOL_VERSION: u8 = 1;
pub const KIND_FRAGMENT: u8 = 3;
const FLAG_ACK_ONLY: u8 = 0x80;
const FLAG_HAS_ACK: u8 = 0x40;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Channel(Channel),
    Fragment,
}

impl Kind {
    fn to_u8(self) -> u8 {
        match self {
            Kind::Channel(c) => c as u8,
            Kind::Fragment => KIND_FRAGMENT,
        }
    }
    fn from_u8(v: u8) -> Option<Self> {
        if v == KIND_FRAGMENT {
            Some(Kind::Fragment)
        } else {
            Channel::from_u8(v).map(Kind::Channel)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub kind: Kind,
    pub ack_only: bool,
    /// When false, `ack`/`ack_bits` carry nothing.
    pub has_ack: bool,
    pub seq: u16,
    pub ack: u16,
    pub ack_bits: u32,
    pub tick: u16,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PacketError {
    #[error("datagram shorter than header")]
    TooShort,
    #[error("unsupported protocol version {0}")]
    Version(u8),
    #[error("unknown packet kind {0}")]
    Kind(u8),
}

impl Header {
    pub fn encode(&self, out: &mut BytesMut) {
        out.put_u8(PROTOCOL_VERSION);
        let mut k = self.kind.to_u8();
        if self.ack_only {
            k |= FLAG_ACK_ONLY;
        }
        if self.has_ack {
            k |= FLAG_HAS_ACK;
        }
        out.put_u8(k);
        out.put_u16(self.seq);
        out.put_u16(self.ack);
        out.put_u32(self.ack_bits);
        out.put_u16(self.tick);
    }

    /// Splits a datagram into header and payload.
    pub fn decode(mut data: Bytes) -> Result<(Header, Bytes), PacketError> {
        if data.len() < HEADER_LEN {
            return Err(PacketError::TooShort);
        }
        let version = data.get_u8();
        if version != PROTOCOL_VERSION {
            return Err(PacketError::Version(version));
        }
        let k = data.get_u8();
        let kind = Kind::from_u8(k & 0x3f).ok_or(PacketError::Kind(k & 0x3f))?;
        let h = Header {
            kind,
            ack_only: k & FLAG_ACK_ONLY != 0,
            has_ack: k & FLAG_HAS_ACK != 0,
            seq: data.get_u16(),
            ack: data.get_u16(),
            ack_bits: data.get_u32(),
            tick: data.get_u16(),
        };
        Ok((h, data))
    }
}

/// `a` is newer than `b` with u16 wraparound.
#[inline]
pub fn seq_newer(a: u16, b: u16) -> bool {
    ((a > b) && (a - b <= 32768)) || ((a < b) && (b - a > 32768))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let h = Header {
            kind: Kind::Channel(Channel::ReliableOrdered),
            ack_only: false,
            has_ack: true,
            seq: 65535,
            ack: 12,
            ack_bits: 0xdead_beef,
            tick: 777,
        };
        let mut b = BytesMut::new();
        h.encode(&mut b);
        b.extend_from_slice(b"xyz");
        assert_eq!(b.len(), HEADER_LEN + 3);
        let (d, p) = Header::decode(b.freeze()).unwrap();
        assert_eq!(d, h);
        assert_eq!(&p[..], b"xyz");
    }

    #[test]
    fn wraparound() {
        assert!(seq_newer(1, 0));
        assert!(seq_newer(0, 65535));
        assert!(!seq_newer(65535, 0));
        assert!(!seq_newer(5, 5));
    }

    #[test]
    fn rejects_bad_version() {
        let mut b = BytesMut::zeroed(HEADER_LEN);
        b[0] = 9;
        assert_eq!(
            Header::decode(b.freeze()).unwrap_err(),
            PacketError::Version(9)
        );
    }
}
