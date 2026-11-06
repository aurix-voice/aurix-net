//! Data-path transport for Aurix Net: packet header, channels with their own reliability on
//! top of unreliable datagrams, fragmentation, QUIC endpoints and data tickets.
//!
//! Everything above datagrams is transport-agnostic: the same [`Link`] runs over QUIC
//! DATAGRAM frames, WebTransport datagrams or binary WebSocket frames (tunnel mode).

pub mod channel;
pub mod fragment;
pub mod link;
pub mod packet;
pub mod quic;
pub mod ticket;

pub use link::{Link, LinkStats, Received};
pub use packet::{Header, PROTOCOL_VERSION};
pub use ticket::{DataTicket, TicketSigner};
