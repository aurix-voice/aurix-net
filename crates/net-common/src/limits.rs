//! Hard protocol limits. Project quotas live in the control plane; these never move.

/// Maximum datagram payload we emit (QUIC/WebTransport safe on the public internet).
pub const MAX_DATAGRAM: usize = 1200;
/// Bytes of the fixed data-packet header.
pub const HEADER_LEN: usize = 12;
/// Largest message a channel accepts; larger ones are fragmented.
pub const MAX_MESSAGE: usize = 64 * 1024;
/// Largest message that fits one datagram without fragmentation.
pub const MAX_UNFRAGMENTED: usize = MAX_DATAGRAM - HEADER_LEN;
/// Reliable window: unacked packets in flight per channel. Must not exceed the 32-bit ack
/// bitfield plus one, otherwise a packet could fall behind the acknowledgeable range.
pub const RELIABLE_WINDOW: u16 = 32;
/// Redundant input ticks sent with every input packet.
pub const INPUT_REDUNDANCY: usize = 3;
/// Data ticket lifetime after a successful join.
pub const DATA_TICKET_TTL_SECS: u64 = 30;
