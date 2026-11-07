//! Control plane: node registry and placement, Redis live state, project quotas.

pub mod nodes;
pub mod quotas;
pub mod redis_store;

pub use nodes::{NodeInfo, NodeRegistry};
pub use quotas::ProjectLimits;
pub use redis_store::LiveRoom;
pub use redis_store::RedisStore;
