//! Which node hosts a new room. Nodes heartbeat into Postgres; every node keeps a cached
//! view and picks the least loaded live node of the requested region.

use net_common::{NetError, NodeId, RegionId, Result};
use net_db::models::NodeRow;
use net_db::{queries, DbPool};
use parking_lot::RwLock;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct NodeInfo {
    pub id: NodeId,
    pub region: RegionId,
    pub public_host: String,
    pub quic_port: u16,
    pub http_url: String,
    pub cert_sha256: String,
    pub load_factor: f32,
    pub draining: bool,
    pub rooms: u32,
    pub players: u32,
}

impl From<NodeRow> for NodeInfo {
    fn from(r: NodeRow) -> Self {
        Self {
            id: NodeId::from_uuid(r.id),
            region: RegionId::new(r.region),
            public_host: r.public_host,
            quic_port: r.quic_port as u16,
            http_url: r.http_url,
            cert_sha256: r.cert_sha256,
            load_factor: r.load_factor,
            draining: r.draining,
            rooms: r.rooms.max(0) as u32,
            players: r.players.max(0) as u32,
        }
    }
}

pub struct NodeRegistry {
    pool: DbPool,
    cache: RwLock<Vec<NodeInfo>>,
    stale_secs: i64,
}

impl NodeRegistry {
    pub fn new(pool: DbPool, heartbeat_secs: u64) -> Arc<Self> {
        Arc::new(Self {
            pool,
            cache: RwLock::new(Vec::new()),
            stale_secs: (heartbeat_secs * 3) as i64,
        })
    }

    pub async fn refresh(&self) -> Result<()> {
        let rows = queries::list_live_nodes(&self.pool, self.stale_secs).await?;
        *self.cache.write() = rows.into_iter().map(NodeInfo::from).collect();
        Ok(())
    }

    pub fn snapshot(&self) -> Vec<NodeInfo> {
        self.cache.read().clone()
    }

    pub fn get(&self, id: NodeId) -> Option<NodeInfo> {
        self.cache.read().iter().find(|n| n.id == id).cloned()
    }

    /// Regions that currently have at least one live, non-draining node.
    pub fn live_regions(&self) -> Vec<RegionId> {
        let mut v: Vec<RegionId> = self
            .cache
            .read()
            .iter()
            .filter(|n| !n.draining)
            .map(|n| n.region.clone())
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v.dedup();
        v
    }

    /// Least loaded live node in `region`; `prefer` wins ties (keeps rooms local).
    pub fn pick(&self, region: &RegionId, prefer: Option<NodeId>) -> Result<NodeInfo> {
        let cache = self.cache.read();
        let mut best: Option<&NodeInfo> = None;
        for n in cache.iter().filter(|n| &n.region == region && !n.draining) {
            best = match best {
                None => Some(n),
                Some(b) => {
                    let better = n.load_factor < b.load_factor - 0.05
                        || ((n.load_factor - b.load_factor).abs() <= 0.05 && Some(n.id) == prefer);
                    if better {
                        Some(n)
                    } else {
                        Some(b)
                    }
                }
            };
        }
        best.cloned()
            .ok_or_else(|| NetError::NotFound(format!("no live node in region {region}")))
    }

    /// Removes nodes whose heartbeat is stale and closes their rooms. Returns removed ids.
    pub async fn reap_stale(&self) -> Result<Vec<NodeId>> {
        let live: Vec<Uuid> = queries::list_live_nodes(&self.pool, self.stale_secs)
            .await?
            .into_iter()
            .map(|r| r.id)
            .collect();
        let all = queries::list_live_nodes(&self.pool, i64::MAX / 4).await?;
        let mut removed = Vec::new();
        for n in all {
            if !live.contains(&n.id) {
                let closed = queries::close_rooms_of_node(&self.pool, n.id).await?;
                queries::delete_node(&self.pool, n.id).await?;
                tracing::warn!(node = %n.id, closed_rooms = closed, "node reaped after missed heartbeats");
                removed.push(NodeId::from_uuid(n.id));
            }
        }
        Ok(removed)
    }

    /// Background loop: refresh cache and reap on every heartbeat interval.
    pub fn spawn_refresh(self: &Arc<Self>, every: Duration) {
        let me = self.clone();
        tokio::spawn(async move {
            loop {
                if let Err(e) = me.refresh().await {
                    tracing::warn!(error = %e, "node registry refresh failed");
                }
                if let Err(e) = me.reap_stale().await {
                    tracing::warn!(error = %e, "node reap failed");
                }
                tokio::time::sleep(every).await;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(region: &str, load: f32, draining: bool) -> NodeInfo {
        NodeInfo {
            id: NodeId::new(),
            region: RegionId::new(region),
            public_host: "h".into(),
            quic_port: 7000,
            http_url: "http://h".into(),
            cert_sha256: String::new(),
            load_factor: load,
            draining,
            rooms: 0,
            players: 0,
        }
    }

    fn registry(nodes: Vec<NodeInfo>) -> NodeRegistry {
        NodeRegistry {
            pool: sqlx_lazy_pool(),
            cache: RwLock::new(nodes),
            stale_secs: 90,
        }
    }

    fn sqlx_lazy_pool() -> DbPool {
        sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://x:y@127.0.0.1:1/none")
            .unwrap()
    }

    #[tokio::test]
    async fn picks_least_loaded_non_draining() {
        let a = node("eu_central", 0.9, false);
        let b = node("eu_central", 0.2, false);
        let c = node("eu_central", 0.0, true);
        let d = node("us_east", 0.0, false);
        let r = registry(vec![a, b.clone(), c, d]);
        assert_eq!(r.pick(&RegionId::new("eu_central"), None).unwrap().id, b.id);
        assert!(r.pick(&RegionId::new("sa_east"), None).is_err());
        assert_eq!(r.live_regions().len(), 2);
    }

    #[tokio::test]
    async fn prefers_local_on_tie() {
        let a = node("eu_central", 0.30, false);
        let b = node("eu_central", 0.32, false);
        let r = registry(vec![a, b.clone()]);
        assert_eq!(
            r.pick(&RegionId::new("eu_central"), Some(b.id)).unwrap().id,
            b.id
        );
    }
}
