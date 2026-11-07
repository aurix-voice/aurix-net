//! Shared identifiers, errors and configuration for Aurix Net.

pub mod config;
pub mod error;
pub mod ipnet;
pub mod limits;
pub mod types;

pub use config::NetConfig;
pub use error::{NetError, Result};
pub use types::*;
