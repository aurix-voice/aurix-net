use crate::models::*;
use crate::{db_err, DbPool};
use chrono::{DateTime, Utc};
use net_common::Result;
use uuid::Uuid;

// ---------- projects ----------

pub async fn create_project(pool: &DbPool, name: &str, plan: &str) -> Result<ProjectRow> {
    sqlx::query_as::<_, ProjectRow>(
        "INSERT INTO projects (id, name, plan) VALUES ($1, $2, $3) RETURNING *",
    )
    .bind(Uuid::now_v7())
    .bind(name)
    .bind(plan)
    .fetch_one(pool)
    .await
    .map_err(db_err)
}

pub async fn get_project(pool: &DbPool, id: Uuid) -> Result<ProjectRow> {
    sqlx::query_as::<_, ProjectRow>("SELECT * FROM projects WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .map_err(db_err)
}

pub async fn list_projects(pool: &DbPool) -> Result<Vec<ProjectRow>> {
    sqlx::query_as::<_, ProjectRow>(
        "SELECT * FROM projects WHERE deleted_at IS NULL ORDER BY created_at",
    )
    .fetch_all(pool)
    .await
    .map_err(db_err)
}

pub async fn set_project_limits(pool: &DbPool, id: Uuid, limits: &serde_json::Value) -> Result<()> {
    sqlx::query("UPDATE projects SET limits = $2 WHERE id = $1")
        .bind(id)
        .bind(limits)
        .execute(pool)
        .await
        .map_err(db_err)?;
    Ok(())
}

// ---------- api keys ----------

pub async fn insert_api_key(
    pool: &DbPool,
    project_id: Uuid,
    key_hash: &str,
    prefix: &str,
    name: &str,
    scopes: &[String],
    expires_at: Option<DateTime<Utc>>,
) -> Result<ApiKeyRow> {
    sqlx::query_as::<_, ApiKeyRow>(
        "INSERT INTO api_keys (id, project_id, key_hash, prefix, name, scopes, expires_at) VALUES ($1,$2,$3,$4,$5,$6,$7) RETURNING *",
    )
    .bind(Uuid::now_v7())
    .bind(project_id)
    .bind(key_hash)
    .bind(prefix)
    .bind(name)
    .bind(scopes)
    .bind(expires_at)
    .fetch_one(pool)
    .await
    .map_err(db_err)
}

/// Marks the project deleted and disabled in one step; keys/webhooks/modules are handled by the
/// caller so the operator gets counts back. Returns false if already deleted or unknown.
pub async fn soft_delete_project(pool: &DbPool, id: Uuid) -> Result<bool> {
    let r = sqlx::query(
        "UPDATE projects SET deleted_at = now(), disabled_at = COALESCE(disabled_at, now()) \
         WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(id)
    .execute(pool)
    .await
    .map_err(db_err)?;
    Ok(r.rows_affected() > 0)
}

pub async fn revoke_all_api_keys(pool: &DbPool, project_id: Uuid) -> Result<u64> {
    let r = sqlx::query(
        "UPDATE api_keys SET revoked_at = now() WHERE project_id = $1 AND revoked_at IS NULL",
    )
    .bind(project_id)
    .execute(pool)
    .await
    .map_err(db_err)?;
    Ok(r.rows_affected())
}

pub async fn delete_webhooks_of_project(pool: &DbPool, project_id: Uuid) -> Result<u64> {
    let r = sqlx::query("DELETE FROM webhooks WHERE project_id = $1")
        .bind(project_id)
        .execute(pool)
        .await
        .map_err(db_err)?;
    Ok(r.rows_affected())
}

pub async fn delete_modules_of_project(pool: &DbPool, project_id: Uuid) -> Result<u64> {
    let r = sqlx::query("DELETE FROM game_modules WHERE project_id = $1")
        .bind(project_id)
        .execute(pool)
        .await
        .map_err(db_err)?;
    Ok(r.rows_affected())
}

pub async fn set_project_disabled(pool: &DbPool, id: Uuid, disabled: bool) -> Result<bool> {
    let r = sqlx::query(
        "UPDATE projects SET disabled_at = CASE WHEN $2 THEN COALESCE(disabled_at, now()) ELSE NULL END WHERE id = $1",
    )
    .bind(id)
    .bind(disabled)
    .execute(pool)
    .await
    .map_err(db_err)?;
    Ok(r.rows_affected() > 0)
}

/// Key row plus the owning project's suspension marker, so one lookup authenticates both.
pub async fn find_api_key(pool: &DbPool, key_hash: &str) -> Result<Option<ApiKeyAuth>> {
    sqlx::query_as::<_, ApiKeyAuth>(
        "SELECT k.*, p.disabled_at AS project_disabled_at FROM api_keys k \
         JOIN projects p ON p.id = k.project_id \
         WHERE k.key_hash = $1 AND k.revoked_at IS NULL AND (k.expires_at IS NULL OR k.expires_at > now())",
    )
    .bind(key_hash)
    .fetch_optional(pool)
    .await
    .map_err(db_err)
}

pub async fn touch_api_key(pool: &DbPool, id: Uuid) -> Result<()> {
    sqlx::query("UPDATE api_keys SET last_used_at = now() WHERE id = $1 AND (last_used_at IS NULL OR last_used_at < now() - interval '1 minute')")
        .bind(id)
        .execute(pool)
        .await
        .map_err(db_err)?;
    Ok(())
}

pub async fn list_api_keys(pool: &DbPool, project_id: Uuid) -> Result<Vec<ApiKeyRow>> {
    sqlx::query_as::<_, ApiKeyRow>(
        "SELECT * FROM api_keys WHERE project_id = $1 ORDER BY created_at",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
    .map_err(db_err)
}

pub async fn revoke_api_key(pool: &DbPool, project_id: Uuid, id: Uuid) -> Result<bool> {
    let r = sqlx::query("UPDATE api_keys SET revoked_at = now() WHERE id = $1 AND project_id = $2 AND revoked_at IS NULL")
        .bind(id)
        .bind(project_id)
        .execute(pool)
        .await
        .map_err(db_err)?;
    Ok(r.rows_affected() == 1)
}

// ---------- regions ----------

pub async fn list_regions(pool: &DbPool, enabled_only: bool) -> Result<Vec<RegionRow>> {
    let q = if enabled_only {
        "SELECT * FROM regions WHERE enabled ORDER BY id"
    } else {
        "SELECT * FROM regions ORDER BY id"
    };
    sqlx::query_as::<_, RegionRow>(q)
        .fetch_all(pool)
        .await
        .map_err(db_err)
}

pub async fn upsert_region(pool: &DbPool, r: &RegionRow) -> Result<()> {
    sqlx::query(
        "INSERT INTO regions (id, display_name, dns_label, enabled) VALUES ($1,$2,$3,$4)
         ON CONFLICT (id) DO UPDATE SET display_name = EXCLUDED.display_name, dns_label = EXCLUDED.dns_label, enabled = EXCLUDED.enabled",
    )
    .bind(&r.id)
    .bind(&r.display_name)
    .bind(&r.dns_label)
    .bind(r.enabled)
    .execute(pool)
    .await
    .map_err(db_err)?;
    Ok(())
}

// ---------- nodes ----------

#[derive(Debug, Clone)]
pub struct NodeHeartbeat {
    pub id: Uuid,
    pub region: String,
    pub public_host: String,
    pub quic_port: i32,
    pub http_url: String,
    pub cert_sha256: String,
    pub version: String,
    pub rooms: i32,
    pub players: i32,
    pub load_factor: f32,
    pub draining: bool,
}

pub async fn upsert_node(pool: &DbPool, n: &NodeHeartbeat) -> Result<()> {
    sqlx::query(
        "INSERT INTO nodes (id, region, public_host, quic_port, http_url, cert_sha256, version, rooms, players, load_factor, draining, last_heartbeat)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11, now())
         ON CONFLICT (id) DO UPDATE SET region = EXCLUDED.region, public_host = EXCLUDED.public_host, quic_port = EXCLUDED.quic_port,
            http_url = EXCLUDED.http_url, cert_sha256 = EXCLUDED.cert_sha256, version = EXCLUDED.version, rooms = EXCLUDED.rooms,
            players = EXCLUDED.players, load_factor = EXCLUDED.load_factor, draining = EXCLUDED.draining, last_heartbeat = now()",
    )
    .bind(n.id)
    .bind(&n.region)
    .bind(&n.public_host)
    .bind(n.quic_port)
    .bind(&n.http_url)
    .bind(&n.cert_sha256)
    .bind(&n.version)
    .bind(n.rooms)
    .bind(n.players)
    .bind(n.load_factor)
    .bind(n.draining)
    .execute(pool)
    .await
    .map_err(db_err)?;
    Ok(())
}

/// Nodes with a heartbeat within `stale_secs`.
pub async fn list_live_nodes(pool: &DbPool, stale_secs: i64) -> Result<Vec<NodeRow>> {
    sqlx::query_as::<_, NodeRow>(
        "SELECT * FROM nodes WHERE last_heartbeat > now() - make_interval(secs => $1) ORDER BY region, load_factor",
    )
    .bind(stale_secs as f64)
    .fetch_all(pool)
    .await
    .map_err(db_err)
}

/// Every registered node, live or not.
pub async fn set_node_draining(pool: &DbPool, id: Uuid, draining: bool) -> Result<bool> {
    let r = sqlx::query("UPDATE nodes SET draining = $2 WHERE id = $1")
        .bind(id)
        .bind(draining)
        .execute(pool)
        .await
        .map_err(db_err)?;
    Ok(r.rows_affected() > 0)
}

pub async fn get_node(pool: &DbPool, id: Uuid) -> Result<Option<NodeRow>> {
    sqlx::query_as::<_, NodeRow>("SELECT * FROM nodes WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(db_err)
}

pub async fn list_all_nodes(pool: &DbPool) -> Result<Vec<NodeRow>> {
    sqlx::query_as::<_, NodeRow>("SELECT * FROM nodes ORDER BY region, load_factor")
        .fetch_all(pool)
        .await
        .map_err(db_err)
}

pub async fn delete_node(pool: &DbPool, id: Uuid) -> Result<()> {
    sqlx::query("DELETE FROM nodes WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await
        .map_err(db_err)?;
    Ok(())
}

// ---------- rooms ----------

#[derive(Debug, Clone)]
pub struct NewRoom {
    pub id: Uuid,
    pub project_id: Uuid,
    pub node_id: Uuid,
    pub region: String,
    pub name: String,
    pub mode: String,
    pub max_players: i32,
    pub visible: bool,
    pub open: bool,
    pub schema_hash: Option<String>,
    pub props: serde_json::Value,
    pub lobby_props: Vec<String>,
}

pub async fn insert_room(pool: &DbPool, r: &NewRoom) -> Result<RoomRow> {
    sqlx::query_as::<_, RoomRow>(
        "INSERT INTO rooms (id, project_id, node_id, region, name, mode, max_players, visible, open, schema_hash, props, lobby_props)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12) RETURNING *",
    )
    .bind(r.id)
    .bind(r.project_id)
    .bind(r.node_id)
    .bind(&r.region)
    .bind(&r.name)
    .bind(&r.mode)
    .bind(r.max_players)
    .bind(r.visible)
    .bind(r.open)
    .bind(&r.schema_hash)
    .bind(&r.props)
    .bind(&r.lobby_props)
    .fetch_one(pool)
    .await
    .map_err(db_err)
}

pub async fn get_room(pool: &DbPool, id: Uuid) -> Result<RoomRow> {
    sqlx::query_as::<_, RoomRow>("SELECT * FROM rooms WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .map_err(db_err)
}

pub async fn find_open_room(
    pool: &DbPool,
    project_id: Uuid,
    name: &str,
) -> Result<Option<RoomRow>> {
    sqlx::query_as::<_, RoomRow>(
        "SELECT * FROM rooms WHERE project_id = $1 AND name = $2 AND state IN ('open', 'orphaned') ORDER BY state LIMIT 1",
    )
    .bind(project_id)
    .bind(name)
    .fetch_optional(pool)
    .await
    .map_err(db_err)
}

pub async fn list_open_rooms(pool: &DbPool, project_id: Uuid, limit: i64) -> Result<Vec<RoomRow>> {
    sqlx::query_as::<_, RoomRow>(
        "SELECT * FROM rooms WHERE project_id = $1 AND state = 'open' ORDER BY created_at DESC LIMIT $2",
    )
    .bind(project_id)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(db_err)
}

pub async fn update_room_flags(
    pool: &DbPool,
    id: Uuid,
    visible: Option<bool>,
    open: Option<bool>,
    props: Option<&serde_json::Value>,
    lobby_props: Option<&[String]>,
) -> Result<()> {
    sqlx::query(
        "UPDATE rooms SET visible = COALESCE($2, visible), open = COALESCE($3, open),
            props = COALESCE($4, props), lobby_props = COALESCE($5, lobby_props) WHERE id = $1",
    )
    .bind(id)
    .bind(visible)
    .bind(open)
    .bind(props)
    .bind(lobby_props)
    .execute(pool)
    .await
    .map_err(db_err)?;
    Ok(())
}

pub async fn close_room(pool: &DbPool, id: Uuid) -> Result<()> {
    sqlx::query(
        "UPDATE rooms SET state = 'closed', closed_at = now() WHERE id = $1 AND state <> 'closed'",
    )
    .bind(id)
    .execute(pool)
    .await
    .map_err(db_err)?;
    Ok(())
}

/// Rooms left open by a node that disappeared.
pub async fn close_rooms_of_node(pool: &DbPool, node_id: Uuid) -> Result<u64> {
    let r = sqlx::query("UPDATE rooms SET state = 'closed', closed_at = now() WHERE node_id = $1 AND state = 'open'")
        .bind(node_id)
        .execute(pool)
        .await
        .map_err(db_err)?;
    Ok(r.rows_affected())
}

/// Rooms of a dead node become `orphaned`: the next node a player of that room reaches
/// adopts them (see `adopt_room`) and re-hosts from the Redis checkpoint.
pub async fn orphan_rooms_of_node(pool: &DbPool, node_id: Uuid) -> Result<u64> {
    let r = sqlx::query(
        "UPDATE rooms SET state = 'orphaned', node_id = NULL, closed_at = now() WHERE node_id = $1 AND state = 'open'",
    )
    .bind(node_id)
    .execute(pool)
    .await
    .map_err(db_err)?;
    Ok(r.rows_affected())
}

/// Atomically takes over an orphaned room; false when another node won the race.
pub async fn adopt_room(pool: &DbPool, room_id: Uuid, node_id: Uuid, region: &str) -> Result<bool> {
    let r = sqlx::query(
        "UPDATE rooms SET state = 'open', node_id = $2, region = $3, closed_at = NULL WHERE id = $1 AND state = 'orphaned'",
    )
    .bind(room_id)
    .bind(node_id)
    .bind(region)
    .execute(pool)
    .await
    .map_err(db_err)?;
    Ok(r.rows_affected() == 1)
}

/// Orphaned rooms nobody adopted within `max_age_secs` (closed_at = when they were
/// orphaned) are closed for good.
pub async fn close_stale_orphans(pool: &DbPool, max_age_secs: i64) -> Result<u64> {
    let r = sqlx::query(
        "UPDATE rooms SET state = 'closed', closed_at = now() WHERE state = 'orphaned' AND closed_at < now() - ($1 * interval '1 second')",
    )
    .bind(max_age_secs)
    .execute(pool)
    .await
    .map_err(db_err)?;
    Ok(r.rows_affected())
}

pub async fn count_open_rooms(pool: &DbPool, project_id: Uuid) -> Result<i64> {
    sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM rooms WHERE project_id = $1 AND state = 'open'",
    )
    .bind(project_id)
    .fetch_one(pool)
    .await
    .map_err(db_err)
}

// ---------- schemas ----------

pub async fn upsert_schema(
    pool: &DbPool,
    project_id: Uuid,
    hash: &str,
    version: &str,
    idl: &str,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO schemas (project_id, hash, version, idl) VALUES ($1,$2,$3,$4) ON CONFLICT (project_id, hash) DO UPDATE SET version = EXCLUDED.version",
    )
    .bind(project_id)
    .bind(hash)
    .bind(version)
    .bind(idl)
    .execute(pool)
    .await
    .map_err(db_err)?;
    Ok(())
}

pub async fn schema_exists(pool: &DbPool, project_id: Uuid, hash: &str) -> Result<bool> {
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM schemas WHERE project_id = $1 AND hash = $2)",
    )
    .bind(project_id)
    .bind(hash)
    .fetch_one(pool)
    .await
    .map_err(db_err)
}

// ---------- webhooks ----------

pub async fn insert_webhook(
    pool: &DbPool,
    project_id: Uuid,
    url: &str,
    secret: &str,
    events: &[String],
) -> Result<WebhookRow> {
    sqlx::query_as::<_, WebhookRow>(
        "INSERT INTO webhooks (id, project_id, url, secret, events) VALUES ($1,$2,$3,$4,$5) RETURNING *",
    )
    .bind(Uuid::now_v7())
    .bind(project_id)
    .bind(url)
    .bind(secret)
    .bind(events)
    .fetch_one(pool)
    .await
    .map_err(db_err)
}

pub async fn list_webhooks(pool: &DbPool, project_id: Uuid) -> Result<Vec<WebhookRow>> {
    sqlx::query_as::<_, WebhookRow>(
        "SELECT * FROM webhooks WHERE project_id = $1 AND enabled ORDER BY created_at",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
    .map_err(db_err)
}

pub async fn delete_webhook(pool: &DbPool, project_id: Uuid, id: Uuid) -> Result<bool> {
    let r = sqlx::query("DELETE FROM webhooks WHERE id = $1 AND project_id = $2")
        .bind(id)
        .bind(project_id)
        .execute(pool)
        .await
        .map_err(db_err)?;
    Ok(r.rows_affected() == 1)
}

// ---------- game modules (WASM logic) ----------

pub async fn upsert_game_module(
    pool: &DbPool,
    project_id: Uuid,
    name: &str,
    hash: &str,
    wasm: &[u8],
) -> Result<GameModuleRow> {
    sqlx::query_as::<_, GameModuleRow>(
        "INSERT INTO game_modules (project_id, name, hash, wasm, size) VALUES ($1,$2,$3,$4,$5) \
         ON CONFLICT (project_id, name) DO UPDATE SET hash = EXCLUDED.hash, wasm = EXCLUDED.wasm, size = EXCLUDED.size, updated_at = now() \
         RETURNING *",
    )
    .bind(project_id)
    .bind(name)
    .bind(hash)
    .bind(wasm)
    .bind(wasm.len() as i32)
    .fetch_one(pool)
    .await
    .map_err(db_err)
}

pub async fn get_game_module(
    pool: &DbPool,
    project_id: Uuid,
    name: &str,
) -> Result<Option<GameModuleRow>> {
    sqlx::query_as::<_, GameModuleRow>(
        "SELECT * FROM game_modules WHERE project_id = $1 AND name = $2",
    )
    .bind(project_id)
    .bind(name)
    .fetch_optional(pool)
    .await
    .map_err(db_err)
}

/// Metadata only (no bytes).
pub async fn list_game_modules(pool: &DbPool, project_id: Uuid) -> Result<Vec<GameModuleRow>> {
    sqlx::query_as::<_, GameModuleRow>(
        "SELECT project_id, name, hash, ''::bytea AS wasm, size, created_at, updated_at FROM game_modules WHERE project_id = $1 ORDER BY name",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
    .map_err(db_err)
}

pub async fn delete_game_module(pool: &DbPool, project_id: Uuid, name: &str) -> Result<bool> {
    let r = sqlx::query("DELETE FROM game_modules WHERE project_id = $1 AND name = $2")
        .bind(project_id)
        .bind(name)
        .execute(pool)
        .await
        .map_err(db_err)?;
    Ok(r.rows_affected() == 1)
}

// ---------- usage / audit ----------

pub async fn add_usage(
    pool: &DbPool,
    project_id: Uuid,
    hour: DateTime<Utc>,
    metric: &str,
    qty: f64,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO usage_hours (project_id, hour, metric, quantity) VALUES ($1,$2,$3,$4)
         ON CONFLICT (project_id, hour, metric) DO UPDATE SET quantity = usage_hours.quantity + EXCLUDED.quantity",
    )
    .bind(project_id)
    .bind(hour)
    .bind(metric)
    .bind(qty)
    .execute(pool)
    .await
    .map_err(db_err)?;
    Ok(())
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize)]
pub struct UsageRow {
    pub hour: DateTime<Utc>,
    pub metric: String,
    pub quantity: f64,
}

pub async fn usage_between(
    pool: &DbPool,
    project_id: Uuid,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<Vec<UsageRow>> {
    sqlx::query_as::<_, UsageRow>(
        "SELECT hour, metric, quantity FROM usage_hours WHERE project_id = $1 AND hour >= $2 AND hour < $3 ORDER BY hour, metric",
    )
    .bind(project_id)
    .bind(from)
    .bind(to)
    .fetch_all(pool)
    .await
    .map_err(db_err)
}

/// Fleet-wide usage: the same hourly rows summed over every project.
pub async fn usage_fleet_between(
    pool: &DbPool,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<Vec<UsageRow>> {
    sqlx::query_as::<_, UsageRow>(
        "SELECT hour, metric, SUM(quantity)::float8 AS quantity FROM usage_hours WHERE hour >= $1 AND hour < $2 GROUP BY hour, metric ORDER BY hour, metric",
    )
    .bind(from)
    .bind(to)
    .fetch_all(pool)
    .await
    .map_err(db_err)
}

pub async fn audit(
    pool: &DbPool,
    project_id: Option<Uuid>,
    actor: &str,
    action: &str,
    target: &str,
    details: serde_json::Value,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO audit (project_id, actor, action, target, details) VALUES ($1,$2,$3,$4,$5)",
    )
    .bind(project_id)
    .bind(actor)
    .bind(action)
    .bind(target)
    .bind(details)
    .execute(pool)
    .await
    .map_err(db_err)?;
    Ok(())
}

/// Rows closed longer than `days` ago; 0 disables. Returns the number deleted.
pub struct NewDelivery<'a> {
    pub webhook_id: Uuid,
    pub project_id: Uuid,
    pub event: &'a str,
    pub ok: bool,
    pub status: Option<i16>,
    pub attempts: i16,
    pub latency_ms: i32,
    pub error: Option<&'a str>,
}

pub async fn insert_webhook_delivery(pool: &DbPool, d: &NewDelivery<'_>) -> Result<()> {
    sqlx::query(
        "INSERT INTO webhook_deliveries (id, webhook_id, project_id, event, ok, status, attempts, latency_ms, error) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(Uuid::now_v7())
    .bind(d.webhook_id)
    .bind(d.project_id)
    .bind(d.event)
    .bind(d.ok)
    .bind(d.status)
    .bind(d.attempts)
    .bind(d.latency_ms)
    .bind(d.error)
    .execute(pool)
    .await
    .map_err(db_err)?;
    Ok(())
}

pub async fn list_webhook_deliveries(
    pool: &DbPool,
    project_id: Uuid,
    webhook_id: Uuid,
    limit: i64,
) -> Result<Vec<WebhookDeliveryRow>> {
    sqlx::query_as::<_, WebhookDeliveryRow>(
        "SELECT * FROM webhook_deliveries WHERE project_id = $1 AND webhook_id = $2 \
         ORDER BY created_at DESC LIMIT $3",
    )
    .bind(project_id)
    .bind(webhook_id)
    .bind(limit.clamp(1, 500))
    .fetch_all(pool)
    .await
    .map_err(db_err)
}

pub async fn get_webhook(pool: &DbPool, project_id: Uuid, id: Uuid) -> Result<Option<WebhookRow>> {
    sqlx::query_as::<_, WebhookRow>("SELECT * FROM webhooks WHERE project_id = $1 AND id = $2")
        .bind(project_id)
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(db_err)
}

pub async fn purge_webhook_deliveries(pool: &DbPool, days: u32) -> Result<u64> {
    if days == 0 {
        return Ok(0);
    }
    let r = sqlx::query(
        "DELETE FROM webhook_deliveries WHERE created_at < now() - make_interval(days => $1)",
    )
    .bind(days as i32)
    .execute(pool)
    .await
    .map_err(db_err)?;
    Ok(r.rows_affected())
}

pub async fn upsert_ban(
    pool: &DbPool,
    project_id: Uuid,
    player_id: &str,
    reason: &str,
    created_by: &str,
    expires_at: Option<DateTime<Utc>>,
) -> Result<BanRow> {
    sqlx::query_as::<_, BanRow>(
        "INSERT INTO bans (project_id, player_id, reason, created_by, expires_at) VALUES ($1, $2, $3, $4, $5) \
         ON CONFLICT (project_id, player_id) DO UPDATE SET reason = EXCLUDED.reason, created_by = EXCLUDED.created_by, \
         expires_at = EXCLUDED.expires_at, created_at = now() RETURNING *",
    )
    .bind(project_id)
    .bind(player_id)
    .bind(reason)
    .bind(created_by)
    .bind(expires_at)
    .fetch_one(pool)
    .await
    .map_err(db_err)
}

pub async fn delete_ban(pool: &DbPool, project_id: Uuid, player_id: &str) -> Result<bool> {
    let r = sqlx::query("DELETE FROM bans WHERE project_id = $1 AND player_id = $2")
        .bind(project_id)
        .bind(player_id)
        .execute(pool)
        .await
        .map_err(db_err)?;
    Ok(r.rows_affected() > 0)
}

pub async fn list_bans(pool: &DbPool, project_id: Uuid) -> Result<Vec<BanRow>> {
    sqlx::query_as::<_, BanRow>(
        "SELECT * FROM bans WHERE project_id = $1 AND (expires_at IS NULL OR expires_at > now()) \
         ORDER BY created_at DESC LIMIT 1000",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
    .map_err(db_err)
}

/// Active ban for this player, if any (expired rows are ignored and reaped lazily).
pub async fn get_ban(pool: &DbPool, project_id: Uuid, player_id: &str) -> Result<Option<BanRow>> {
    sqlx::query_as::<_, BanRow>(
        "SELECT * FROM bans WHERE project_id = $1 AND player_id = $2 AND (expires_at IS NULL OR expires_at > now())",
    )
    .bind(project_id)
    .bind(player_id)
    .fetch_optional(pool)
    .await
    .map_err(db_err)
}

pub async fn purge_expired_bans(pool: &DbPool) -> Result<u64> {
    let r = sqlx::query("DELETE FROM bans WHERE expires_at IS NOT NULL AND expires_at < now()")
        .execute(pool)
        .await
        .map_err(db_err)?;
    Ok(r.rows_affected())
}

pub async fn purge_closed_rooms(pool: &DbPool, days: u32) -> Result<u64> {
    if days == 0 {
        return Ok(0);
    }
    let r = sqlx::query(
        "DELETE FROM rooms WHERE state = 'closed' AND closed_at < now() - ($1 * interval '1 day')",
    )
    .bind(days as f64)
    .execute(pool)
    .await
    .map_err(db_err)?;
    Ok(r.rows_affected())
}

pub async fn purge_audit(pool: &DbPool, days: u32) -> Result<u64> {
    if days == 0 {
        return Ok(0);
    }
    let r = sqlx::query("DELETE FROM audit WHERE created_at < now() - ($1 * interval '1 day')")
        .bind(days as f64)
        .execute(pool)
        .await
        .map_err(db_err)?;
    Ok(r.rows_affected())
}

pub async fn purge_usage(pool: &DbPool, days: u32) -> Result<u64> {
    if days == 0 {
        return Ok(0);
    }
    let r = sqlx::query("DELETE FROM usage_hours WHERE hour < now() - ($1 * interval '1 day')")
        .bind(days as f64)
        .execute(pool)
        .await
        .map_err(db_err)?;
    Ok(r.rows_affected())
}

/// Newest audit rows first; `project` = None lists every project (operator view).
/// Audit rows, newest first. `action` / `actor` are prefix filters (`key.` matches every key
/// action; `operator` matches the operator actor).
pub async fn list_audit(
    pool: &DbPool,
    project: Option<Uuid>,
    action: Option<&str>,
    actor: Option<&str>,
    limit: i64,
) -> Result<Vec<AuditRow>> {
    let like = |s: Option<&str>| {
        s.filter(|s| !s.is_empty()).map(|s| {
            format!(
                "{}%",
                s.replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_")
            )
        })
    };
    sqlx::query_as::<_, AuditRow>(
        "SELECT id, project_id, actor, action, target, details, created_at FROM audit \
         WHERE ($1::uuid IS NULL OR project_id = $1) \
           AND ($3::text IS NULL OR action LIKE $3) \
           AND ($4::text IS NULL OR actor LIKE $4) \
         ORDER BY id DESC LIMIT $2",
    )
    .bind(project)
    .bind(limit.clamp(1, 1000))
    .bind(like(action))
    .bind(like(actor))
    .fetch_all(pool)
    .await
    .map_err(db_err)
}
