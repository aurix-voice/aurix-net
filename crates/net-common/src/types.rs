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
            pub fn from_uuid(uuid: Uuid) -> Self {
                Self(uuid)
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
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
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

impl RoomMode {
    /// The name used on the wire, in the database and in room listings.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Relay => "relay",
            Self::Sync => "sync",
            Self::Lockstep => "lockstep",
        }
    }
}

impl fmt::Display for RoomMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for RoomMode {
    type Err = UnknownRoomMode;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "relay" => Ok(Self::Relay),
            "sync" => Ok(Self::Sync),
            "lockstep" => Ok(Self::Lockstep),
            _ => Err(UnknownRoomMode),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnknownRoomMode;

impl fmt::Display for UnknownRoomMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("unknown room mode")
    }
}

impl std::error::Error for UnknownRoomMode {}

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
    /// Whatever arrives is delivered, in the order it arrives. 3 is a fragment on the wire.
    UnreliableUnsequenced = 4,
}

impl Channel {
    pub fn from_u8(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(Self::UnreliableSequenced),
            1 => Some(Self::ReliableOrdered),
            2 => Some(Self::ReliableUnordered),
            4 => Some(Self::UnreliableUnsequenced),
            _ => None,
        }
    }
    pub fn is_reliable(self) -> bool {
        matches!(self, Self::ReliableOrdered | Self::ReliableUnordered)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_mode_name_matches_its_serde_name() {
        for mode in [RoomMode::Relay, RoomMode::Sync, RoomMode::Lockstep] {
            let json = serde_json::to_value(mode).unwrap();
            assert_eq!(json, mode.as_str());
            assert_eq!(mode.to_string().parse(), Ok(mode));
        }
        assert_eq!("Relay".parse::<RoomMode>(), Err(UnknownRoomMode));
    }
}
