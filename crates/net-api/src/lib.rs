//! HTTP/WebSocket control plane and QUIC data plane of a node.

pub mod data;
pub mod host;
pub mod metrics;
pub mod rest;
pub mod state;
pub mod sync;
pub mod ws;

pub use state::AppState;
