//! Room logic for relay mode: membership, properties, events, interest groups, event cache,
//! master client. Pure data structures (no I/O) so every rule is unit-testable; the server
//! wires them to WebSocket/QUIC sessions.

pub mod event;
pub mod protocol;
pub mod room;

pub use event::{EventFrame, EventTarget};
pub use room::{Outgoing, Player, Room, RoomConfig, RoomError};
