use crate::host::RoomHost;
use crate::metrics::Metrics;
use net_auth::GrantSigner;
use net_common::{NetConfig, NodeId, RegionId};
use net_control::{NodeRegistry, RedisStore};
use net_db::DbPool;
use net_transport::TicketSigner;
use std::sync::Arc;

pub struct AppState {
    pub cfg: NetConfig,
    pub pool: DbPool,
    pub redis: RedisStore,
    pub registry: Arc<NodeRegistry>,
    pub grants: GrantSigner,
    pub tickets: TicketSigner,
    pub host: RoomHost,
    pub node_id: NodeId,
    pub region: RegionId,
    pub cert_sha256: String,
    pub quic_port: u16,
    pub metrics: Metrics,
    pub started: std::time::Instant,
}

pub type Shared = Arc<AppState>;
