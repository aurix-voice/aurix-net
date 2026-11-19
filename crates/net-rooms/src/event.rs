//! Binary event frame carried on the data path (relay mode).
//!
//! ```text
//! u8   code          application event code (0..=255)
//! u8   flags         bit0..1 target kind, bit2 cache-add, bit3 cache-remove, bit4 has-group
//! u16  from          actor id (server fills in; clients send 0)
//! [u16 group]        when has-group
//! [u16 n, n x u16]   explicit actor list when target kind == 3
//! bytes payload
//! ```

use bytes::{Buf, BufMut, Bytes, BytesMut};
use net_common::ActorId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "actors")]
pub enum EventTarget {
    /// Everyone including the sender.
    All,
    /// Everyone except the sender.
    Others,
    /// The master client only.
    Master,
    /// Explicit list.
    Actors(Vec<ActorId>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CacheOp {
    #[default]
    None,
    /// Store so late joiners receive it.
    Add,
    /// Remove previously cached events with the same code from this sender.
    Remove,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventFrame {
    pub code: u8,
    pub target: EventTarget,
    pub cache: CacheOp,
    pub group: Option<u16>,
    pub from: ActorId,
    pub payload: Bytes,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FrameError {
    #[error("frame too short")]
    TooShort,
    #[error("bad target kind")]
    Target,
}

impl EventFrame {
    pub fn encode(&self) -> Bytes {
        let mut buf = BytesMut::with_capacity(8 + self.payload.len());
        buf.put_u8(self.code);
        let kind = match self.target {
            EventTarget::All => 0,
            EventTarget::Others => 1,
            EventTarget::Master => 2,
            EventTarget::Actors(_) => 3,
        };
        let mut flags = kind;
        match self.cache {
            CacheOp::None => {}
            CacheOp::Add => flags |= 0x04,
            CacheOp::Remove => flags |= 0x08,
        }
        if self.group.is_some() {
            flags |= 0x10;
        }
        buf.put_u8(flags);
        buf.put_u16(self.from.0);
        if let Some(group) = self.group {
            buf.put_u16(group);
        }
        if let EventTarget::Actors(actors) = &self.target {
            buf.put_u16(actors.len().min(u16::MAX as usize) as u16);
            for actor in actors.iter().take(u16::MAX as usize) {
                buf.put_u16(actor.0);
            }
        }
        buf.extend_from_slice(&self.payload);
        buf.freeze()
    }

    pub fn decode(mut data: Bytes) -> Result<Self, FrameError> {
        if data.len() < 4 {
            return Err(FrameError::TooShort);
        }
        let code = data.get_u8();
        let flags = data.get_u8();
        let from = ActorId(data.get_u16());
        let group = if flags & 0x10 != 0 {
            if data.len() < 2 {
                return Err(FrameError::TooShort);
            }
            Some(data.get_u16())
        } else {
            None
        };
        let target = match flags & 0x03 {
            0 => EventTarget::All,
            1 => EventTarget::Others,
            2 => EventTarget::Master,
            _ => {
                if data.len() < 2 {
                    return Err(FrameError::TooShort);
                }
                let count = data.get_u16() as usize;
                if data.len() < count * 2 {
                    return Err(FrameError::TooShort);
                }
                EventTarget::Actors((0..count).map(|_| ActorId(data.get_u16())).collect())
            }
        };
        let cache = if flags & 0x04 != 0 {
            CacheOp::Add
        } else if flags & 0x08 != 0 {
            CacheOp::Remove
        } else {
            CacheOp::None
        };
        Ok(Self {
            code,
            target,
            cache,
            group,
            from,
            payload: data,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_all_shapes() {
        for (target, group, cache) in [
            (EventTarget::All, None, CacheOp::None),
            (EventTarget::Others, Some(3), CacheOp::Add),
            (EventTarget::Master, None, CacheOp::Remove),
            (
                EventTarget::Actors(vec![ActorId(1), ActorId(9)]),
                Some(0),
                CacheOp::None,
            ),
        ] {
            let frame = EventFrame {
                code: 42,
                target,
                cache,
                group,
                from: ActorId(5),
                payload: Bytes::from_static(b"payload"),
            };
            assert_eq!(EventFrame::decode(frame.encode()).unwrap(), frame);
        }
    }
}
