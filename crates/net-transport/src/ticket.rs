//! Data tickets: the first datagram on a fresh QUIC connection proves which session it
//! belongs to. 48 bytes: `u64 exp_unix, 16 session uuid, 16 room uuid(?)` is overkill;
//! we sign `session_id || room_id || actor || exp` with HMAC-SHA256 truncated to 16 bytes.

use hmac::{Hmac, KeyInit, Mac};
use net_common::limits::DATA_TICKET_TTL_SECS;
use net_common::{ActorId, RoomId, SessionId};
use sha2::Sha256;
use std::time::{SystemTime, UNIX_EPOCH};
use subtle::ConstantTimeEq;

pub const TICKET_LEN: usize = 16 + 16 + 2 + 8 + 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataTicket {
    pub session: SessionId,
    pub room: RoomId,
    pub actor: ActorId,
    pub exp_unix: u64,
}

#[derive(Clone)]
pub struct TicketSigner {
    key: Vec<u8>,
}

impl TicketSigner {
    pub fn new(secret: &[u8]) -> Self {
        Self {
            key: secret.to_vec(),
        }
    }

    fn mac(&self, body: &[u8]) -> [u8; 16] {
        let mut m = <Hmac<Sha256>>::new_from_slice(&self.key).expect("hmac accepts any key length");
        m.update(b"aurix-net-ticket-v1");
        m.update(body);
        let out = m.finalize().into_bytes();
        let mut t = [0u8; 16];
        t.copy_from_slice(&out[..16]);
        t
    }

    pub fn issue(&self, session: SessionId, room: RoomId, actor: ActorId) -> Vec<u8> {
        let exp = now_unix() + DATA_TICKET_TTL_SECS;
        let t = DataTicket {
            session,
            room,
            actor,
            exp_unix: exp,
        };
        self.encode(&t)
    }

    pub fn encode(&self, t: &DataTicket) -> Vec<u8> {
        let mut body = Vec::with_capacity(TICKET_LEN);
        body.extend_from_slice(t.session.0.as_bytes());
        body.extend_from_slice(t.room.0.as_bytes());
        body.extend_from_slice(&t.actor.0.to_be_bytes());
        body.extend_from_slice(&t.exp_unix.to_be_bytes());
        let tag = self.mac(&body);
        body.extend_from_slice(&tag);
        body
    }

    /// Verifies signature and expiry.
    pub fn verify(&self, data: &[u8]) -> Option<DataTicket> {
        if data.len() != TICKET_LEN {
            return None;
        }
        let (body, tag) = data.split_at(TICKET_LEN - 16);
        let expect = self.mac(body);
        if expect.ct_eq(tag).unwrap_u8() != 1 {
            return None;
        }
        let session = SessionId(uuid::Uuid::from_slice(&body[0..16]).ok()?);
        let room = RoomId(uuid::Uuid::from_slice(&body[16..32]).ok()?);
        let actor = ActorId(u16::from_be_bytes([body[32], body[33]]));
        let exp_unix = u64::from_be_bytes(body[34..42].try_into().ok()?);
        if exp_unix < now_unix() {
            return None;
        }
        Some(DataTicket {
            session,
            room,
            actor,
            exp_unix,
        })
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_tamper() {
        let s = TicketSigner::new(b"secret");
        let t = s.issue(SessionId::new(), RoomId::new(), ActorId(7));
        let v = s.verify(&t).unwrap();
        assert_eq!(v.actor, ActorId(7));
        let mut bad = t.clone();
        bad[33] ^= 1;
        assert!(s.verify(&bad).is_none());
        assert!(TicketSigner::new(b"other").verify(&t).is_none());
    }

    #[test]
    fn expired_rejected() {
        let s = TicketSigner::new(b"k");
        let t = DataTicket {
            session: SessionId::new(),
            room: RoomId::new(),
            actor: ActorId(1),
            exp_unix: now_unix() - 1,
        };
        assert!(s.verify(&s.encode(&t)).is_none());
    }
}
