use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

macro_rules! uuid_id {
    ($name:ident) => {
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }
            pub fn from_uuid(u: Uuid) -> Self {
                Self(u)
            }
            pub fn as_uuid(&self) -> Uuid {
                self.0
            }
        }
        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }
        impl std::str::FromStr for $name {
            type Err = uuid::Error;
            fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
                Ok(Self(Uuid::parse_str(s)?))
            }
        }
    };
}

uuid_id!(ProjectId);
uuid_id!(RoomId);
uuid_id!(NodeId);
uuid_id!(TicketId);
uuid_id!(SessionId);

/// Player number inside a room: small, dense, reused only after the room closes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ActorId(pub u16);

/// Simulation tick (sync/lockstep modes). Wraps after ~2 years at 60 Hz.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Default,
)]
#[serde(transparent)]
pub struct Tick(pub u32);

/// Region identifier, snake_case, e.g. `eu_central`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RegionId(pub String);

impl RegionId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RegionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// How a room is driven.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RoomMode {
    /// Server forwards events; no game logic on the node.
    #[default]
    Relay,
    /// Server-authoritative simulation on the node, client prediction.
    Sync,
    /// Deterministic lockstep, server keeps the verified frame.
    Lockstep,
}

/// Delivery guarantees of a data channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum Channel {
    /// Newest wins; older-than-last packets are dropped.
    UnreliableSequenced = 0,
    /// Delivered exactly once, in send order.
    ReliableOrdered = 1,
    /// Delivered exactly once, any order.
    ReliableUnordered = 2,
}

impl Channel {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::UnreliableSequenced),
            1 => Some(Self::ReliableOrdered),
            2 => Some(Self::ReliableUnordered),
            _ => None,
        }
    }
    pub fn is_reliable(self) -> bool {
        !matches!(self, Self::UnreliableSequenced)
    }
}

/// Lifecycle of a room as seen by the control plane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoomState {
    Open,
    Closed,
    Migrating,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Region {
    pub id: RegionId,
    pub display_name: String,
    pub dns_label: String,
    pub enabled: bool,
}
