//! Which node hosts a new room. Nodes heartbeat into Postgres; every node keeps a cached
//! view and picks the least loaded live node of the requested region.

use net_common::{NetError, NodeId, RegionId, Result};
use net_db::models::NodeRow;
use net_db::{queries, DbPool};
use parking_lot::RwLock;
use std::sync::Arc;
use std::time::Duration;

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
    fn from(row: NodeRow) -> Self {
        Self {
            id: NodeId::from_uuid(row.id),
            region: RegionId::new(row.region),
            public_host: row.public_host,
            quic_port: row.quic_port as u16,
            http_url: row.http_url,
            cert_sha256: row.cert_sha256,
            load_factor: row.load_factor,
            draining: row.draining,
            rooms: row.rooms.max(0) as u32,
            players: row.players.max(0) as u32,
        }
    }
}

/// Orphaned rooms (node died) stay adoptable this long; matches the checkpoint TTL.
pub const ORPHAN_TTL_SECS: i64 = 600;

pub struct NodeRegistry {
    pool: DbPool,
    cache: RwLock<Vec<NodeInfo>>,
}

impl NodeRegistry {
    pub fn new(pool: DbPool) -> Arc<Self> {
        Arc::new(Self {
            pool,
            cache: RwLock::new(Vec::new()),
        })
    }

    pub async fn refresh(&self) -> Result<()> {
        let rows = queries::list_live_nodes(&self.pool).await?;
        *self.cache.write() = rows.into_iter().map(NodeInfo::from).collect();
        Ok(())
    }

    pub fn snapshot(&self) -> Vec<NodeInfo> {
        self.cache.read().clone()
    }

    pub fn get(&self, id: NodeId) -> Option<NodeInfo> {
        self.cache.read().iter().find(|n| n.id == id).cloned()
    }

    /// [`Self::get`] that re-reads the node list on a miss: the cache is up to one heartbeat
    /// old, so a node that registered since then is not in it yet.
    pub async fn resolve(&self, id: NodeId) -> Result<Option<NodeInfo>> {
        if let Some(node) = self.get(id) {
            return Ok(Some(node));
        }
        self.refresh().await?;
        Ok(self.get(id))
    }

    /// Regions that currently have at least one live, non-draining node.
    pub fn live_regions(&self) -> Vec<RegionId> {
        let mut regions: Vec<RegionId> = self
            .cache
            .read()
            .iter()
            .filter(|n| !n.draining)
            .map(|n| n.region.clone())
            .collect();
        regions.sort_by(|a, b| a.0.cmp(&b.0));
        regions.dedup();
        regions
    }

    /// Least loaded live node in `region`; `prefer` wins ties (keeps rooms local).
    pub fn pick(&self, region: &RegionId, prefer: Option<NodeId>) -> Result<NodeInfo> {
        let cache = self.cache.read();
        let mut best: Option<&NodeInfo> = None;
        for node in cache.iter().filter(|n| &n.region == region && !n.draining) {
            best = match best {
                None => Some(node),
                Some(chosen) => {
                    let better = node.load_factor < chosen.load_factor - 0.05
                        || ((node.load_factor - chosen.load_factor).abs() <= 0.05
                            && Some(node.id) == prefer);
                    if better {
                        Some(node)
                    } else {
                        Some(chosen)
                    }
                }
            };
        }
        best.cloned()
            .ok_or_else(|| NetError::NotFound(format!("no live node in region {region}")))
    }

    /// Removes nodes that missed three of their own heartbeats and closes their rooms. Returns removed ids.
    pub async fn reap_stale(&self) -> Result<Vec<NodeId>> {
        let stale = queries::list_stale_nodes(&self.pool).await?;
        let expired = queries::close_stale_orphans(&self.pool, ORPHAN_TTL_SECS).await?;
        if expired > 0 {
            tracing::info!(rooms = expired, "closed orphaned rooms nobody adopted");
        }
        let mut removed = Vec::new();
        for id in stale {
            let orphaned = queries::orphan_rooms_of_node(&self.pool, id).await?;
            queries::delete_node(&self.pool, id).await?;
            tracing::warn!(node = %id, orphaned_rooms = orphaned, "node reaped after missed heartbeats");
            removed.push(NodeId::from_uuid(id));
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
        }
    }

    fn sqlx_lazy_pool() -> DbPool {
        sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://x:y@127.0.0.1:1/none")
            .unwrap()
    }

    #[tokio::test]
    async fn picks_least_loaded_non_draining() {
        let busy = node("eu_central", 0.9, false);
        let idle = node("eu_central", 0.2, false);
        let draining = node("eu_central", 0.0, true);
        let elsewhere = node("us_east", 0.0, false);
        let fleet = registry(vec![busy, idle.clone(), draining, elsewhere]);
        assert_eq!(
            fleet.pick(&RegionId::new("eu_central"), None).unwrap().id,
            idle.id
        );
        assert!(fleet.pick(&RegionId::new("sa_east"), None).is_err());
        assert_eq!(fleet.live_regions().len(), 2);
    }

    #[tokio::test]
    async fn prefers_local_on_tie() {
        let lighter = node("eu_central", 0.30, false);
        let heavier = node("eu_central", 0.32, false);
        let fleet = registry(vec![lighter, heavier.clone()]);
        assert_eq!(
            fleet
                .pick(&RegionId::new("eu_central"), Some(heavier.id))
                .unwrap()
                .id,
            heavier.id
        );
    }
}
