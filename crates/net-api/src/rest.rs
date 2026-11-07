//! REST for developer backends (API key) and operators (admin token).

use crate::state::Shared;
use crate::ws::close_room;
use axum::extract::DefaultBodyLimit;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use net_auth::api_key::{hash_key, looks_like_key, ApiKeyMaterial, Scope};
use net_common::{NetError, ProjectId, Result, RoomId};
use net_db::models::ApiKeyRow;
use net_db::queries;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

pub struct ApiError(NetError);

impl From<NetError> for ApiError {
    fn from(e: NetError) -> Self {
        ApiError(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status =
            StatusCode::from_u16(self.0.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        (
            status,
            Json(json!({"error": self.0.code(), "message": self.0.to_string()})),
        )
            .into_response()
    }
}

type ApiResult<T> = std::result::Result<T, ApiError>;

pub fn router(st: Shared) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/metrics", get(metrics))
        .route("/v1/me", get(me))
        .route("/v1/audit", get(list_audit))
        .route("/v1/regions", get(regions))
        .route("/v1/nodes", get(nodes))
        .route("/v1/grants", post(create_grant))
        .route("/v1/rooms", get(list_rooms).post(create_room))
        .route("/v1/rooms/:id", get(get_room).delete(delete_room))
        .route("/v1/rooms/:id/replay", get(get_replay))
        .route("/v1/usage", get(usage))
        .route("/v1/keys", get(list_keys).post(create_key))
        .route("/v1/keys/:id", delete(revoke_key))
        .route("/v1/webhooks", get(list_webhooks).post(create_webhook))
        .route("/v1/matchmaking/tickets", post(mm_file))
        .route("/v1/matchmaking/tickets/:id", get(mm_get).delete(mm_cancel))
        .route("/v1/webhooks/:id", delete(delete_webhook))
        .route("/v1/modules", get(list_modules))
        .route(
            "/v1/modules/:name",
            put(put_module)
                .delete(delete_module)
                .layer(DefaultBodyLimit::max(net_game::MAX_MODULE_BYTES + 1024)),
        )
        .route(
            "/v1/operator/projects",
            get(op_list_projects).post(op_create_project),
        )
        .route("/v1/operator/projects/:id/keys", post(op_create_key))
        .route("/v1/operator/projects/:id/keys/:kid", delete(op_revoke_key))
        .route("/v1/operator/projects/:id/limits", post(op_set_limits))
        .route("/v1/operator/projects/:id/suspend", post(op_set_suspended))
        .route("/v1/operator/nodes", get(op_nodes))
        .route("/v1/operator/rooms", get(op_rooms))
        .route(
            "/v1/operator/rooms/:id",
            get(op_room).delete(op_delete_room),
        )
        .route("/v1/operator/audit", get(op_audit))
        .route("/v1/operator/usage", get(op_fleet_usage))
        .route(
            "/v1/operator/projects/:id/webhooks/:wid",
            delete(op_delete_webhook),
        )
        .route(
            "/v1/operator/projects/:id/modules/:name",
            delete(op_delete_module),
        )
        .route("/v1/internal/rooms/:id/players", get(internal_players))
        .route("/v1/internal/drain", post(internal_drain))
        .route("/v1/webhooks/:id/deliveries", get(webhook_deliveries))
        .route("/v1/rejections", get(rejections))
        .route("/v1/operator/projects/:id/rejections", get(op_rejections))
        .route("/v1/webhooks/:id/test", post(webhook_test))
        .route(
            "/v1/operator/projects/:id/webhooks/:wid/deliveries",
            get(op_webhook_deliveries),
        )
        .route(
            "/v1/operator/projects/:id/webhooks/:wid/test",
            post(op_webhook_test),
        )
        .route("/v1/internal/rooms/:id", delete(internal_close_room))
        .route("/v1/operator/nodes/:id/drain", post(op_drain_node))
        .route("/v1/operator/rooms/:id/replay", get(op_replay))
        .route(
            "/v1/operator/projects/:id",
            get(op_project).delete(op_delete_project),
        )
        .route("/v1/operator/projects/:id/usage", get(op_usage))
        .route("/v1/operator/projects/:id/rooms", get(op_project_rooms))
        .with_state(st)
}

fn bearer(h: &HeaderMap) -> Option<&str> {
    h.get("authorization")?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
}

async fn api_key(st: &Shared, h: &HeaderMap, scope: Scope) -> Result<ApiKeyRow> {
    let k = bearer(h).ok_or_else(|| NetError::Unauthorized("missing bearer api key".into()))?;
    if !looks_like_key(k) {
        return Err(NetError::Unauthorized("malformed api key".into()));
    }
    let auth = queries::find_api_key(&st.pool, &hash_key(k))
        .await?
        .ok_or_else(|| NetError::Unauthorized("unknown api key".into()))?;
    // A suspended project keeps read access so the team can see its state; everything that
    // creates load (grants, rooms, matchmaking, uploads) is refused.
    if auth.project_disabled_at.is_some() && scope != Scope::Read {
        reject(st, ProjectId::from_uuid(auth.key.project_id), "suspended");
        return Err(NetError::Forbidden("project suspended".into()));
    }
    let row = auth.key;
    let has = row
        .scopes
        .iter()
        .any(|s| s == scope.as_str() || s == "admin");
    if !has {
        return Err(NetError::Forbidden(format!(
            "key lacks scope {}",
            scope.as_str()
        )));
    }
    let _ = queries::touch_api_key(&st.pool, row.id).await;
    Ok(row)
}

fn operator(st: &Shared, h: &HeaderMap) -> Result<()> {
    let t = bearer(h).unwrap_or("");
    if net_auth::admin::admin_token_ok(&st.cfg.auth.admin_token, t) {
        Ok(())
    } else {
        Err(NetError::Unauthorized("operator token required".into()))
    }
}

/// Audit row for an admin/operator mutation; a failed insert is logged, never fails the request.
async fn audit_log(
    st: &Shared,
    project: Option<Uuid>,
    actor: &str,
    action: &str,
    target: &str,
    details: Value,
) {
    if let Err(e) = queries::audit(&st.pool, project, actor, action, target, details).await {
        tracing::warn!(error = %e, action, "audit insert failed");
    }
}

/// Per-key write budget (`limits.writes_per_sec_per_key`).
fn key_write(st: &Shared, key: &ApiKeyRow) -> Result<()> {
    if st.guards.key_writes.allow(key.id) {
        Ok(())
    } else {
        reject(st, ProjectId::from_uuid(key.project_id), "rate_limited");
        Err(NetError::RateLimited)
    }
}

/// Records why a project's player/key was turned away (hourly Redis buckets, see
/// `RedisStore::incr_rejection`). Off the request path; failures are only logged.
pub(crate) fn reject(st: &Shared, project: ProjectId, reason: &'static str) {
    let redis = st.redis.clone();
    tokio::spawn(async move {
        if let Err(e) = redis.incr_rejection(project, reason).await {
            tracing::debug!(error = %e, reason, "rejection counter");
        }
    });
}

#[derive(Deserialize)]
struct RejectionsQuery {
    #[serde(default)]
    hours: Option<u32>,
}

async fn rejections_json(st: &Shared, project: Uuid, hours: Option<u32>) -> Result<Value> {
    let hours = hours.unwrap_or(24).clamp(1, 26);
    let buckets = st
        .redis
        .get_rejections(ProjectId::from_uuid(project), hours)
        .await?;
    let mut total: std::collections::BTreeMap<String, i64> = Default::default();
    let series: Vec<Value> = buckets
        .into_iter()
        .map(|(t, counts)| {
            for (k, v) in &counts {
                *total.entry(k.clone()).or_default() += v;
            }
            let ts = chrono::DateTime::<chrono::Utc>::from_timestamp(t, 0)
                .map(|d| d.to_rfc3339())
                .unwrap_or_default();
            json!({"t": ts, "counts": counts})
        })
        .collect();
    Ok(json!({"hours": hours, "total": total, "series": series}))
}

async fn rejections(
    State(st): State<Shared>,
    h: HeaderMap,
    Query(q): Query<RejectionsQuery>,
) -> ApiResult<Json<Value>> {
    let key = api_key(&st, &h, Scope::Read).await?;
    Ok(Json(rejections_json(&st, key.project_id, q.hours).await?))
}

async fn op_rejections(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<RejectionsQuery>,
) -> ApiResult<Json<Value>> {
    operator(&st, &h)?;
    Ok(Json(rejections_json(&st, id, q.hours).await?))
}

/// Per-key grant budget (`limits.grants_per_sec_per_key`): one grant per player login.
fn key_grant(st: &Shared, key: &ApiKeyRow) -> Result<()> {
    if st.guards.key_grants.allow(key.id) {
        Ok(())
    } else {
        reject(st, ProjectId::from_uuid(key.project_id), "rate_limited");
        Err(NetError::RateLimited)
    }
}

async fn healthz(State(st): State<Shared>) -> Response {
    let draining = st.is_draining();
    let status = if draining {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    };
    (
        status,
        Json(json!({
            "ok": !draining,
            "draining": draining,
            "version": env!("CARGO_PKG_VERSION"),
            "node_id": st.node_id,
            "region": st.region,
            "rooms": st.host.len(),
            "players": st.host.players(),
            "uptime_secs": st.started.elapsed().as_secs(),
        })),
    )
        .into_response()
}

async fn metrics(State(st): State<Shared>, h: HeaderMap) -> Response {
    let want = &st.cfg.server.metrics_token;
    if !want.is_empty() && !net_auth::admin::admin_token_ok(want, bearer(&h).unwrap_or("")) {
        return (StatusCode::UNAUTHORIZED, "metrics token required").into_response();
    }
    st.metrics.rooms.set(st.host.len() as i64);
    st.metrics.players.set(st.host.players() as i64);
    st.metrics.render().into_response()
}

#[derive(Serialize)]
struct RegionOut {
    id: String,
    display_name: String,
    nodes: usize,
    /// HTTP base URLs of live nodes in the region; clients pick one (lowest RTT) and use it
    /// for grants, rooms and the WebSocket control channel. Room lookups on the wrong node
    /// answer `room_elsewhere:<url>`.
    endpoints: Vec<String>,
}

async fn regions(State(st): State<Shared>) -> ApiResult<Json<Vec<RegionOut>>> {
    let rows = queries::list_regions(&st.pool, true).await?;
    let nodes = st.registry.snapshot();
    Ok(Json(
        rows.into_iter()
            .map(|r| RegionOut {
                nodes: nodes
                    .iter()
                    .filter(|n| n.region.as_str() == r.id && !n.draining)
                    .count(),
                endpoints: nodes
                    .iter()
                    .filter(|n| n.region.as_str() == r.id && !n.draining)
                    .map(|n| n.http_url.clone())
                    .collect(),
                id: r.id,
                display_name: r.display_name,
            })
            .filter(|r| r.nodes > 0 || r.id == st.region.as_str())
            .collect(),
    ))
}

#[derive(Deserialize)]
struct NodesQuery {
    region: Option<String>,
}

async fn nodes(State(st): State<Shared>, Query(q): Query<NodesQuery>) -> Json<Vec<Value>> {
    Json(
        st.registry
            .snapshot()
            .into_iter()
            .filter(|n| !n.draining && q.region.as_deref().is_none_or(|r| r == n.region.as_str()))
            .map(|n| {
                json!({
                    "id": n.id, "region": n.region, "host": n.public_host, "quic_port": n.quic_port,
                    "http_url": n.http_url, "cert_sha256": n.cert_sha256, "load_factor": n.load_factor,
                })
            })
            .collect(),
    )
}

#[derive(Deserialize)]
struct GrantReq {
    player_id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    room: Option<String>,
    #[serde(default)]
    scopes: Option<Vec<String>>,
    #[serde(default)]
    ttl_secs: Option<u64>,
    #[serde(default)]
    meta: Option<Value>,
}

async fn create_grant(
    State(st): State<Shared>,
    h: HeaderMap,
    Json(req): Json<GrantReq>,
) -> ApiResult<Json<Value>> {
    let key = api_key(&st, &h, Scope::Rooms).await?;
    key_grant(&st, &key)?;
    let scopes: Vec<&str> = req
        .scopes
        .as_ref()
        .map(|v| v.iter().map(String::as_str).collect())
        .unwrap_or_else(|| vec!["join", "create"]);
    let (token, claims) = st.grants.issue(
        ProjectId::from_uuid(key.project_id),
        &req.player_id,
        &req.name,
        req.room.as_deref(),
        &scopes,
        req.meta,
        req.ttl_secs,
    )?;
    Ok(Json(
        json!({"grant": token, "expires_at": claims.exp, "player_id": claims.sub}),
    ))
}

async fn list_rooms(State(st): State<Shared>, h: HeaderMap) -> ApiResult<Json<Value>> {
    let key = api_key(&st, &h, Scope::Read).await?;
    let project = ProjectId::from_uuid(key.project_id);
    let live = st.redis.list_rooms(project).await.unwrap_or_default();
    Ok(Json(json!({"rooms": live})))
}

#[derive(Deserialize)]
struct CreateRoomReq {
    name: String,
    #[serde(default)]
    max_players: Option<u16>,
    #[serde(default = "t")]
    visible: bool,
    #[serde(default = "t")]
    open: bool,
    #[serde(default)]
    props: serde_json::Map<String, Value>,
    #[serde(default)]
    lobby_props: Vec<String>,
}
fn t() -> bool {
    true
}

/// Server-side room creation (no player joins). The room lives on this node.
async fn create_room(
    State(st): State<Shared>,
    h: HeaderMap,
    Json(req): Json<CreateRoomReq>,
) -> ApiResult<Json<Value>> {
    let key = api_key(&st, &h, Scope::Rooms).await?;
    key_write(&st, &key)?;
    let project = ProjectId::from_uuid(key.project_id);
    if st.host.by_name(project, &req.name).is_some() {
        return Err(NetError::Conflict("room name already in use".into()).into());
    }
    let prow = queries::get_project(&st.pool, key.project_id).await?;
    let limits = net_control::ProjectLimits::from_json(&prow.limits).effective(&st.cfg.limits);
    let max_players = req
        .max_players
        .unwrap_or(limits.max_players_per_room.min(16))
        .clamp(1, limits.max_players_per_room);
    let cfg = net_rooms::RoomConfig {
        id: RoomId::new(),
        project,
        name: req.name.clone(),
        mode: net_common::RoomMode::Relay,
        max_players,
        visible: req.visible,
        open: req.open,
        props: req.props.clone(),
        lobby_props: req.lobby_props.clone(),
        max_events_per_sec: limits.max_events_per_sec_per_room,
        max_events_per_sec_per_player: limits.max_events_per_sec_per_player,
        max_event_bytes: limits.max_event_bytes,
        max_props_bytes: limits.max_props_bytes,
        max_player_props_bytes: limits.max_player_props_bytes,
        max_cached_events: limits.max_cached_events_per_room,
        offline_ttl: std::time::Duration::from_secs(crate::ws::prop_secs(
            &req.props,
            "player_ttl_secs",
            30,
        )),
        // Server-created rooms wait longer for their first player.
        empty_ttl: std::time::Duration::from_secs(crate::ws::prop_secs(
            &req.props,
            "empty_ttl_secs",
            300,
        )),
    };
    queries::insert_room(
        &st.pool,
        &queries::NewRoom {
            id: cfg.id.0,
            project_id: key.project_id,
            node_id: st.node_id.0,
            region: st.region.to_string(),
            name: req.name,
            mode: "relay".into(),
            max_players: max_players as i32,
            visible: req.visible,
            open: req.open,
            schema_hash: None,
            props: Value::Object(req.props),
            lobby_props: req.lobby_props,
        },
    )
    .await?;
    let id = cfg.id;
    let hr = st.host.insert(
        project,
        net_rooms::Room::new(cfg, std::time::Instant::now()),
    );
    crate::ws::publish_room(&st, &hr).await;
    st.webhooks.emit(
        project,
        "room.created",
        json!({"room": id.to_string(), "name": hr.lock().room.cfg.name, "mode": "relay", "region": st.region.to_string()}),
    );
    Ok(Json(
        json!({"id": id, "node": st.node_id, "region": st.region}),
    ))
}

async fn get_room(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let key = api_key(&st, &h, Scope::Read).await?;
    let row = queries::get_room(&st.pool, id).await?;
    if row.project_id != key.project_id {
        return Err(NetError::NotFound("room".into()).into());
    }
    Ok(Json(room_detail(&st, row).await?))
}

/// Players of a room hosted on this node, or `None` when it is not hosted here.
fn local_players(st: &Shared, id: Uuid) -> Option<Vec<Value>> {
    st.host.get(RoomId::from_uuid(id)).map(|hr| {
        hr.lock()
            .room
            .player_infos()
            .into_iter()
            .map(|p| json!({"actor": p.actor, "player_id": p.player_id, "name": p.name, "online": p.online}))
            .collect()
    })
}

/// Node-to-node: player list of a room hosted here, for consoles pointed at a different node.
/// Guarded by the fleet-wide admin token; never proxied further (a room is hosted by one node).
async fn internal_players(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    operator(&st, &h)?;
    let players =
        local_players(&st, id).ok_or_else(|| NetError::NotFound("room not hosted here".into()))?;
    Ok(Json(json!({"players": players})))
}

/// Persisted row + live Redis state + players. Players come from this node when it hosts the
/// room, otherwise from the hosting node over its HTTP API (admin token, 3 s budget); when that
/// fails `players_source` says so instead of silently showing an empty list.
async fn room_detail(st: &Shared, row: net_db::models::RoomRow) -> Result<Value> {
    let id = row.id;
    let live = st
        .redis
        .get_room(ProjectId::from_uuid(row.project_id), RoomId::from_uuid(id))
        .await?;
    let (players, source): (Vec<Value>, &str) = match local_players(st, id) {
        Some(p) => (p, "local"),
        None => match row
            .node_id
            .and_then(|n| st.registry.get(net_common::NodeId::from_uuid(n)))
        {
            Some(n) if n.id != st.node_id && live.is_some() => {
                match remote_players(st, &n.http_url, id).await {
                    Ok(p) => (p, "remote"),
                    Err(e) => {
                        tracing::warn!(room = %id, node = %n.http_url, error = %e, "remote players unavailable");
                        (Vec::new(), "unavailable")
                    }
                }
            }
            _ => (Vec::new(), "none"),
        },
    };
    Ok(json!({"room": row, "live": live, "players": players, "players_source": source}))
}

async fn remote_players(st: &Shared, http_url: &str, id: Uuid) -> Result<Vec<Value>> {
    if st.cfg.auth.admin_token.is_empty() {
        return Err(NetError::Internal(
            "admin_token unset; cannot query hosting node".into(),
        ));
    }
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| NetError::Internal(e.to_string()))?;
    let url = format!(
        "{}/v1/internal/rooms/{id}/players",
        http_url.trim_end_matches('/')
    );
    let res = http
        .get(url)
        .bearer_auth(&st.cfg.auth.admin_token)
        .send()
        .await
        .map_err(|e| NetError::Internal(e.to_string()))?;
    if !res.status().is_success() {
        return Err(NetError::Internal(format!(
            "hosting node answered {}",
            res.status()
        )));
    }
    let v: Value = res
        .json()
        .await
        .map_err(|e| NetError::Internal(e.to_string()))?;
    Ok(v.get("players")
        .and_then(|p| p.as_array())
        .cloned()
        .unwrap_or_default())
}

/// Operator view of any room in the cluster.
async fn op_room(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    operator(&st, &h)?;
    let row = queries::get_room(&st.pool, id).await?;
    Ok(Json(room_detail(&st, row).await?))
}

/// Operator force-close of any room (abuse response); audited.
async fn op_delete_room(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    operator(&st, &h)?;
    let row = queries::get_room(&st.pool, id).await?;
    close_room_anywhere(&st, ProjectId::from_uuid(row.project_id), id).await;
    audit_log(
        &st,
        Some(row.project_id),
        "operator",
        "room.close",
        &id.to_string(),
        json!({}),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

/// Lockstep replay (input journal) of a closed room, kept for 24 h.
async fn get_replay(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let key = api_key(&st, &h, Scope::Read).await?;
    let row = queries::get_room(&st.pool, id).await?;
    if row.project_id != key.project_id {
        return Err(NetError::NotFound("room".into()).into());
    }
    let body = st
        .redis
        .get_replay(ProjectId::from_uuid(key.project_id), RoomId::from_uuid(id))
        .await?
        .ok_or_else(|| NetError::NotFound("replay".into()))?;
    let v: Value = serde_json::from_slice(&body).map_err(|e| NetError::Internal(e.to_string()))?;
    Ok(Json(v))
}

/// Files a matchmaking ticket. Poll `GET /v1/matchmaking/tickets/:id` until `matched`, then
/// `join_or_create` the returned room name with the same mode.
async fn mm_file(
    State(st): State<Shared>,
    h: HeaderMap,
    Json(req): Json<crate::matchmaking::TicketReq>,
) -> ApiResult<Json<Value>> {
    let key = api_key(&st, &h, Scope::Rooms).await?;
    key_write(&st, &key)?;
    if req.max_players == 0 || req.min_players == 0 || req.min_players > req.max_players {
        return Err(NetError::Invalid("min_players must be 1..=max_players".into()).into());
    }
    let t = st
        .matchmaker
        .file(&st.redis, ProjectId::from_uuid(key.project_id), req)
        .await?;
    Ok(Json(json!({"ticket": t.id, "state": t.state})))
}

async fn mm_get(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let key = api_key(&st, &h, Scope::Read).await?;
    let t = st
        .matchmaker
        .get(&st.redis, ProjectId::from_uuid(key.project_id), id)
        .await?
        .ok_or_else(|| NetError::NotFound("ticket".into()))?;
    Ok(Json(
        json!({"ticket": t.id, "state": t.state, "age_secs": t.age_secs(crate::matchmaking::now_ms())}),
    ))
}

async fn mm_cancel(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let key = api_key(&st, &h, Scope::Rooms).await?;
    if st
        .matchmaker
        .cancel(&st.redis, ProjectId::from_uuid(key.project_id), id)
        .await?
    {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(NetError::NotFound("ticket".into()).into())
    }
}

async fn delete_room(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let key = api_key(&st, &h, Scope::Rooms).await?;
    let row = queries::get_room(&st.pool, id).await?;
    if row.project_id != key.project_id {
        return Err(NetError::NotFound("room".into()).into());
    }
    close_room(
        &st,
        ProjectId::from_uuid(key.project_id),
        RoomId::from_uuid(id),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct UsageQuery {
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
}

async fn usage(
    State(st): State<Shared>,
    h: HeaderMap,
    Query(q): Query<UsageQuery>,
) -> ApiResult<Json<Value>> {
    let key = api_key(&st, &h, Scope::Read).await?;
    let to = q.to.unwrap_or_else(Utc::now);
    let from = q.from.unwrap_or(to - chrono::Duration::days(30));
    let rows = queries::usage_between(&st.pool, key.project_id, from, to).await?;
    Ok(Json(json!({"from": from, "to": to, "usage": rows})))
}

#[derive(Deserialize)]
struct KeyReq {
    #[serde(default)]
    name: String,
    #[serde(default)]
    scopes: Vec<String>,
}

fn validate_scopes(s: &[String]) -> Result<Vec<String>> {
    if s.is_empty() {
        return Ok(vec!["rooms".into(), "read".into()]);
    }
    for x in s {
        if Scope::parse(x).is_none() {
            return Err(NetError::Invalid(format!("unknown scope {x}")));
        }
    }
    Ok(s.to_vec())
}

async fn issue_key(st: &Shared, project: Uuid, name: &str, scopes: &[String]) -> Result<Value> {
    let scopes = validate_scopes(scopes)?;
    let m = ApiKeyMaterial::generate();
    let row = queries::insert_api_key(&st.pool, project, &m.hash, &m.prefix, name, &scopes).await?;
    Ok(
        json!({"id": row.id, "key": m.plaintext, "prefix": row.prefix, "scopes": row.scopes, "name": row.name}),
    )
}

async fn create_key(
    State(st): State<Shared>,
    h: HeaderMap,
    Json(req): Json<KeyReq>,
) -> ApiResult<Json<Value>> {
    let key = api_key(&st, &h, Scope::Admin).await?;
    key_write(&st, &key)?;
    let out = issue_key(&st, key.project_id, &req.name, &req.scopes).await?;
    audit_log(
        &st,
        Some(key.project_id),
        &format!("key:{}", key.id),
        "key.create",
        out["id"].as_str().unwrap_or_default(),
        json!({"name": req.name, "scopes": req.scopes}),
    )
    .await;
    Ok(Json(out))
}

async fn list_keys(State(st): State<Shared>, h: HeaderMap) -> ApiResult<Json<Value>> {
    let key = api_key(&st, &h, Scope::Admin).await?;
    let rows = queries::list_api_keys(&st.pool, key.project_id).await?;
    Ok(Json(
        json!({"keys": rows.iter().map(key_json).collect::<Vec<_>>()}),
    ))
}

async fn revoke_key(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let key = api_key(&st, &h, Scope::Admin).await?;
    if queries::revoke_api_key(&st.pool, key.project_id, id).await? {
        audit_log(
            &st,
            Some(key.project_id),
            &format!("key:{}", key.id),
            "key.revoke",
            &id.to_string(),
            json!({}),
        )
        .await;
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(NetError::NotFound("key".into()).into())
    }
}

#[derive(Deserialize)]
struct WebhookReq {
    url: String,
    #[serde(default)]
    events: Vec<String>,
}

async fn create_webhook(
    State(st): State<Shared>,
    h: HeaderMap,
    Json(req): Json<WebhookReq>,
) -> ApiResult<Json<Value>> {
    let key = api_key(&st, &h, Scope::Admin).await?;
    key_write(&st, &key)?;
    crate::webhooks::validate_webhook_url(&req.url, st.cfg.server.environment == "development")?;
    let secret = ApiKeyMaterial::generate().plaintext;
    let row =
        queries::insert_webhook(&st.pool, key.project_id, &req.url, &secret, &req.events).await?;
    audit_log(
        &st,
        Some(key.project_id),
        &format!("key:{}", key.id),
        "webhook.create",
        &row.id.to_string(),
        json!({"url": row.url, "events": row.events}),
    )
    .await;
    Ok(Json(
        json!({"id": row.id, "url": row.url, "events": row.events, "secret": row.secret}),
    ))
}

async fn list_webhooks(State(st): State<Shared>, h: HeaderMap) -> ApiResult<Json<Value>> {
    let key = api_key(&st, &h, Scope::Admin).await?;
    let rows = queries::list_webhooks(&st.pool, key.project_id).await?;
    Ok(Json(
        json!({"webhooks": rows.into_iter().map(|r| json!({"id": r.id, "url": r.url, "events": r.events, "created_at": r.created_at})).collect::<Vec<_>>()}),
    ))
}

async fn delete_webhook(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let key = api_key(&st, &h, Scope::Admin).await?;
    if queries::delete_webhook(&st.pool, key.project_id, id).await? {
        audit_log(
            &st,
            Some(key.project_id),
            &format!("key:{}", key.id),
            "webhook.delete",
            &id.to_string(),
            json!({}),
        )
        .await;
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(NetError::NotFound("webhook".into()).into())
    }
}

#[derive(Deserialize)]
struct ProjectReq {
    name: String,
    #[serde(default = "default_plan")]
    plan: String,
}
fn default_plan() -> String {
    "self_hosted".into()
}

async fn op_create_project(
    State(st): State<Shared>,
    h: HeaderMap,
    Json(req): Json<ProjectReq>,
) -> ApiResult<Json<Value>> {
    operator(&st, &h)?;
    let p = queries::create_project(&st.pool, &req.name, &req.plan).await?;
    let key = issue_key(
        &st,
        p.id,
        "default",
        &["rooms".into(), "read".into(), "admin".into()],
    )
    .await?;
    queries::audit(
        &st.pool,
        Some(p.id),
        "operator",
        "project.create",
        &p.id.to_string(),
        json!({}),
    )
    .await?;
    Ok(Json(json!({"project": p, "key": key})))
}

async fn op_list_projects(State(st): State<Shared>, h: HeaderMap) -> ApiResult<Json<Value>> {
    operator(&st, &h)?;
    Ok(Json(
        json!({"projects": queries::list_projects(&st.pool).await?}),
    ))
}

async fn op_create_key(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(req): Json<KeyReq>,
) -> ApiResult<Json<Value>> {
    operator(&st, &h)?;
    queries::get_project(&st.pool, id).await?;
    let out = issue_key(&st, id, &req.name, &req.scopes).await?;
    audit_log(
        &st,
        Some(id),
        "operator",
        "key.create",
        out["id"].as_str().unwrap_or_default(),
        json!({"name": req.name, "scopes": req.scopes}),
    )
    .await;
    Ok(Json(out))
}

async fn op_revoke_key(
    State(st): State<Shared>,
    h: HeaderMap,
    Path((id, kid)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    operator(&st, &h)?;
    if !queries::revoke_api_key(&st.pool, id, kid).await? {
        return Err(NetError::NotFound("key".into()).into());
    }
    audit_log(
        &st,
        Some(id),
        "operator",
        "key.revoke",
        &kid.to_string(),
        json!({}),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct SuspendReq {
    suspended: bool,
}

/// Suspend (or resume) a project. While suspended: no grants, no joins, no room creation, no
/// writes with its keys; reads keep working. Live rooms are not torn down — they drain as players
/// leave — so the response carries the current live count for the console to show.
async fn op_set_suspended(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(req): Json<SuspendReq>,
) -> ApiResult<Json<Value>> {
    operator(&st, &h)?;
    if !queries::set_project_disabled(&st.pool, id, req.suspended).await? {
        return Err(NetError::NotFound("project not found".into()).into());
    }
    let live = st
        .redis
        .list_rooms(ProjectId::from_uuid(id))
        .await
        .map(|r| r.len())
        .unwrap_or(0);
    audit_log(
        &st,
        Some(id),
        "operator",
        if req.suspended {
            "project.suspend"
        } else {
            "project.resume"
        },
        &id.to_string(),
        json!({"live_rooms": live}),
    )
    .await;
    let p = queries::get_project(&st.pool, id).await?;
    Ok(Json(json!({"project": p, "live_rooms": live})))
}

async fn op_set_limits(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(limits): Json<Value>,
) -> ApiResult<StatusCode> {
    operator(&st, &h)?;
    let _ = net_control::ProjectLimits::from_json(&limits);
    queries::set_project_limits(&st.pool, id, &limits).await?;
    audit_log(
        &st,
        Some(id),
        "operator",
        "project.limits",
        &id.to_string(),
        limits,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct DeliveriesQuery {
    #[serde(default)]
    limit: Option<i64>,
}

async fn deliveries_json(
    st: &Shared,
    project: Uuid,
    wid: Uuid,
    limit: Option<i64>,
) -> Result<Value> {
    queries::get_webhook(&st.pool, project, wid)
        .await?
        .ok_or_else(|| NetError::NotFound("webhook not found".into()))?;
    let rows =
        queries::list_webhook_deliveries(&st.pool, project, wid, limit.unwrap_or(50)).await?;
    Ok(json!({"deliveries": rows}))
}

async fn test_webhook(st: &Shared, project: Uuid, wid: Uuid) -> Result<Value> {
    queries::get_webhook(&st.pool, project, wid)
        .await?
        .ok_or_else(|| NetError::NotFound("webhook not found".into()))?;
    st.webhooks.test(ProjectId::from_uuid(project), wid);
    Ok(json!({"queued": true}))
}

/// Delivery log of one webhook (project key, read scope).
async fn webhook_deliveries(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<DeliveriesQuery>,
) -> ApiResult<Json<Value>> {
    let key = api_key(&st, &h, Scope::Read).await?;
    Ok(Json(
        deliveries_json(&st, key.project_id, id, q.limit).await?,
    ))
}

/// Queue a `test` event to one webhook; the result shows up in its delivery log.
async fn webhook_test(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let key = api_key(&st, &h, Scope::Admin).await?;
    key_write(&st, &key)?;
    Ok(Json(test_webhook(&st, key.project_id, id).await?))
}

async fn op_webhook_deliveries(
    State(st): State<Shared>,
    h: HeaderMap,
    Path((id, wid)): Path<(Uuid, Uuid)>,
    Query(q): Query<DeliveriesQuery>,
) -> ApiResult<Json<Value>> {
    operator(&st, &h)?;
    Ok(Json(deliveries_json(&st, id, wid, q.limit).await?))
}

async fn op_webhook_test(
    State(st): State<Shared>,
    h: HeaderMap,
    Path((id, wid)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    operator(&st, &h)?;
    let out = test_webhook(&st, id, wid).await?;
    audit_log(
        &st,
        Some(id),
        "operator",
        "webhook.test",
        &wid.to_string(),
        json!({}),
    )
    .await;
    Ok(Json(out))
}

#[derive(Deserialize)]
struct DrainReq {
    draining: bool,
}

/// Drain this node without stopping the process: refuse new joins, hand every live room off
/// to the fleet through Redis checkpoints, answer 503 on `/healthz` so the balancer pulls it.
/// `draining=false` puts the node back into rotation (it starts adopting rooms again).
async fn set_local_draining(st: &Shared, draining: bool) -> Result<usize> {
    st.draining
        .store(draining, std::sync::atomic::Ordering::Relaxed);
    let handed = if draining {
        crate::ws::handoff_all(st).await
    } else {
        0
    };
    queries::set_node_draining(&st.pool, st.node_id.0, draining).await?;
    tracing::info!(draining, rooms_handed_off = handed, "drain state changed");
    Ok(handed)
}

/// Closes a live room no matter which node hosts it: locally through `close_room`, otherwise by
/// asking the hosting node (so its players get `left` and its memory is freed). Falls back to
/// the DB/Redis close when the hosting node is unreachable.
async fn close_room_anywhere(st: &Shared, project: ProjectId, id: Uuid) {
    let room = RoomId::from_uuid(id);
    if st.host.get(room).is_some() {
        close_room(st, project, room).await;
        return;
    }
    let live = st.redis.get_room(project, room).await.ok().flatten();
    let node = live
        .as_ref()
        .and_then(|l| Uuid::parse_str(&l.node).ok())
        .and_then(|n| st.registry.get(net_common::NodeId::from_uuid(n)));
    if let Some(n) = node.filter(|n| n.id != st.node_id) {
        match internal_delete(st, &n.http_url, &format!("/v1/internal/rooms/{id}")).await {
            Ok(()) => return,
            Err(e) => {
                tracing::warn!(room = %id, node = %n.http_url, error = %e, "remote close failed; closing locally")
            }
        }
    }
    close_room(st, project, room).await;
}

async fn internal_close_room(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    operator(&st, &h)?;
    let room = RoomId::from_uuid(id);
    let project = st
        .host
        .get(room)
        .map(|h| h.lock().project)
        .ok_or_else(|| NetError::NotFound("room not hosted here".into()))?;
    close_room(&st, project, room).await;
    Ok(StatusCode::NO_CONTENT)
}

async fn internal_delete(st: &Shared, http_url: &str, path: &str) -> Result<()> {
    if st.cfg.auth.admin_token.is_empty() {
        return Err(NetError::Internal(
            "admin_token unset; cannot reach other nodes".into(),
        ));
    }
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| NetError::Internal(e.to_string()))?;
    let res = http
        .delete(format!("{}{path}", http_url.trim_end_matches('/')))
        .bearer_auth(&st.cfg.auth.admin_token)
        .send()
        .await
        .map_err(|e| NetError::Internal(e.to_string()))?;
    if !res.status().is_success() {
        return Err(NetError::Internal(format!(
            "node answered {}",
            res.status()
        )));
    }
    Ok(())
}

/// Delete a project: every live room is closed fleet-wide, all keys revoked, webhooks and
/// modules removed. The project row stays (soft delete) so usage and audit history survive.
async fn op_delete_project(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    operator(&st, &h)?;
    if !queries::soft_delete_project(&st.pool, id).await? {
        return Err(NetError::NotFound("project not found".into()).into());
    }
    let project = ProjectId::from_uuid(id);
    let live = st.redis.list_rooms(project).await.unwrap_or_default();
    let closed = live.len();
    for r in live {
        close_room_anywhere(&st, project, r.room.0).await;
    }
    let revoked = queries::revoke_all_api_keys(&st.pool, id).await?;
    let webhooks = queries::delete_webhooks_of_project(&st.pool, id).await?;
    let modules = queries::delete_modules_of_project(&st.pool, id).await?;
    let details = json!({"closed_rooms": closed, "revoked_keys": revoked, "webhooks": webhooks, "modules": modules});
    audit_log(
        &st,
        Some(id),
        "operator",
        "project.delete",
        &id.to_string(),
        details.clone(),
    )
    .await;
    Ok(Json(details))
}

async fn internal_drain(
    State(st): State<Shared>,
    h: HeaderMap,
    Json(req): Json<DrainReq>,
) -> ApiResult<Json<Value>> {
    operator(&st, &h)?;
    let handed = set_local_draining(&st, req.draining).await?;
    Ok(Json(
        json!({"node_id": st.node_id.0, "draining": req.draining, "rooms_handed_off": handed}),
    ))
}

async fn op_drain_node(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(req): Json<DrainReq>,
) -> ApiResult<Json<Value>> {
    operator(&st, &h)?;
    let out = if id == st.node_id.0 {
        let handed = set_local_draining(&st, req.draining).await?;
        json!({"node_id": id, "draining": req.draining, "rooms_handed_off": handed})
    } else {
        let node = queries::get_node(&st.pool, id)
            .await?
            .ok_or_else(|| NetError::NotFound("node not found".into()))?;
        internal_post(
            &st,
            &node.http_url,
            "/v1/internal/drain",
            json!({"draining": req.draining}),
        )
        .await?
    };
    audit_log(
        &st,
        None,
        "operator",
        if req.draining {
            "node.drain"
        } else {
            "node.undrain"
        },
        &id.to_string(),
        out.clone(),
    )
    .await;
    Ok(Json(out))
}

/// Node-to-node call with the fleet admin token: short timeout, no redirects.
async fn internal_post(st: &Shared, http_url: &str, path: &str, body: Value) -> Result<Value> {
    if st.cfg.auth.admin_token.is_empty() {
        return Err(NetError::Internal(
            "admin_token unset; cannot reach other nodes".into(),
        ));
    }
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| NetError::Internal(e.to_string()))?;
    let res = http
        .post(format!("{}{path}", http_url.trim_end_matches('/')))
        .bearer_auth(&st.cfg.auth.admin_token)
        .json(&body)
        .send()
        .await
        .map_err(|e| NetError::Internal(e.to_string()))?;
    if !res.status().is_success() {
        return Err(NetError::Internal(format!(
            "node answered {}",
            res.status()
        )));
    }
    res.json()
        .await
        .map_err(|e| NetError::Internal(e.to_string()))
}

async fn op_nodes(State(st): State<Shared>, h: HeaderMap) -> ApiResult<Json<Value>> {
    operator(&st, &h)?;
    let rows = queries::list_all_nodes(&st.pool).await?;
    Ok(Json(json!({"nodes": rows})))
}

pub fn shared(st: AppStateArc) -> Shared {
    st
}
pub type AppStateArc = Arc<crate::state::AppState>;

// ---------- game modules (server-side WASM logic for sync rooms) ----------

fn valid_module_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 64
        && n.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

/// Upload (or replace) a WebAssembly module; binary `.wasm` or WAT text. Validated and
/// compiled before it is stored. Rooms created afterwards with `props.logic = name` use it.
async fn put_module(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(name): Path<String>,
    body: axum::body::Bytes,
) -> ApiResult<Json<Value>> {
    let key = api_key(&st, &h, Scope::Admin).await?;
    key_write(&st, &key)?;
    if !valid_module_name(&name) {
        return Err(NetError::Invalid("module name: 1..=64 chars [A-Za-z0-9._-]".into()).into());
    }
    if body.is_empty() {
        return Err(NetError::Invalid("empty module".into()).into());
    }
    let games = st.games.clone();
    let bytes = body.to_vec();
    let (hash, _) = tokio::task::spawn_blocking(move || games.load(&bytes))
        .await
        .map_err(|e| NetError::Internal(e.to_string()))?
        .map_err(|e| NetError::Invalid(format!("module rejected: {e}")))?;
    let row = queries::upsert_game_module(&st.pool, key.project_id, &name, &hash, &body).await?;
    audit_log(
        &st,
        Some(key.project_id),
        &format!("key:{}", key.id),
        "module.put",
        &name,
        json!({"hash": row.hash, "size": row.size}),
    )
    .await;
    Ok(Json(
        json!({"name": row.name, "hash": row.hash, "size": row.size, "updated_at": row.updated_at}),
    ))
}

async fn list_modules(State(st): State<Shared>, h: HeaderMap) -> ApiResult<Json<Value>> {
    let key = api_key(&st, &h, Scope::Read).await?;
    let rows = queries::list_game_modules(&st.pool, key.project_id).await?;
    Ok(Json(json!({ "modules": rows })))
}

async fn delete_module(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(name): Path<String>,
) -> ApiResult<StatusCode> {
    let key = api_key(&st, &h, Scope::Admin).await?;
    if queries::delete_game_module(&st.pool, key.project_id, &name).await? {
        audit_log(
            &st,
            Some(key.project_id),
            &format!("key:{}", key.id),
            "module.delete",
            &name,
            json!({}),
        )
        .await;
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(NetError::NotFound(format!("module {name}")).into())
    }
}

fn key_json(r: &ApiKeyRow) -> Value {
    json!({
        "id": r.id, "prefix": r.prefix, "name": r.name, "scopes": r.scopes,
        "created_at": r.created_at, "last_used_at": r.last_used_at, "revoked_at": r.revoked_at
    })
}

/// Who the bearer is: the operator, or an API key with its project. The console signs in with it.
async fn me(State(st): State<Shared>, h: HeaderMap) -> ApiResult<Json<Value>> {
    if operator(&st, &h).is_ok() {
        return Ok(Json(
            json!({"kind": "operator", "region": st.region, "node_id": st.node_id}),
        ));
    }
    let k = bearer(&h).ok_or_else(|| NetError::Unauthorized("missing bearer token".into()))?;
    if !looks_like_key(k) {
        return Err(NetError::Unauthorized("malformed api key".into()).into());
    }
    let row = queries::find_api_key(&st.pool, &hash_key(k))
        .await?
        .ok_or_else(|| NetError::Unauthorized("unknown api key".into()))?
        .key;
    let p = queries::get_project(&st.pool, row.project_id).await?;
    Ok(Json(json!({
        "kind": "project",
        "project": {"id": p.id, "name": p.name, "plan": p.plan, "limits": p.limits, "created_at": p.created_at, "disabled_at": p.disabled_at},
        "key": key_json(&row),
        "region": st.region,
    })))
}

#[derive(Deserialize)]
struct AuditQuery {
    #[serde(default)]
    limit: Option<i64>,
    #[serde(default)]
    project: Option<Uuid>,
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    actor: Option<String>,
}

async fn list_audit(
    State(st): State<Shared>,
    h: HeaderMap,
    Query(q): Query<AuditQuery>,
) -> ApiResult<Json<Value>> {
    let key = api_key(&st, &h, Scope::Read).await?;
    let rows = queries::list_audit(
        &st.pool,
        Some(key.project_id),
        q.action.as_deref(),
        q.actor.as_deref(),
        q.limit.unwrap_or(100),
    )
    .await?;
    Ok(Json(json!({"audit": rows})))
}

async fn op_audit(
    State(st): State<Shared>,
    h: HeaderMap,
    Query(q): Query<AuditQuery>,
) -> ApiResult<Json<Value>> {
    operator(&st, &h)?;
    let rows = queries::list_audit(
        &st.pool,
        q.project,
        q.action.as_deref(),
        q.actor.as_deref(),
        q.limit.unwrap_or(100),
    )
    .await?;
    Ok(Json(json!({"audit": rows})))
}

/// Operator removes a project's webhook (e.g. a leaked secret or an abusive endpoint); audited.
async fn op_delete_webhook(
    State(st): State<Shared>,
    h: HeaderMap,
    Path((id, wid)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    operator(&st, &h)?;
    if queries::delete_webhook(&st.pool, id, wid).await? {
        audit_log(
            &st,
            Some(id),
            "operator",
            "webhook.delete",
            &wid.to_string(),
            json!({}),
        )
        .await;
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(NetError::NotFound("webhook".into()).into())
    }
}

/// Operator removes a project's WASM module; audited.
async fn op_delete_module(
    State(st): State<Shared>,
    h: HeaderMap,
    Path((id, name)): Path<(Uuid, String)>,
) -> ApiResult<StatusCode> {
    operator(&st, &h)?;
    if queries::delete_game_module(&st.pool, id, &name).await? {
        audit_log(&st, Some(id), "operator", "module.delete", &name, json!({})).await;
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(NetError::NotFound(format!("module {name}")).into())
    }
}

/// Project detail for the operator: row, keys (hashes never leave the DB), webhooks, modules.
async fn op_project(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    operator(&st, &h)?;
    let p = queries::get_project(&st.pool, id).await?;
    if p.deleted_at.is_some() {
        return Err(NetError::NotFound("project deleted".into()).into());
    }
    let keys: Vec<Value> = queries::list_api_keys(&st.pool, id)
        .await?
        .iter()
        .map(key_json)
        .collect();
    let webhooks: Vec<Value> = queries::list_webhooks(&st.pool, id)
        .await?
        .into_iter()
        .map(|r| json!({"id": r.id, "url": r.url, "events": r.events, "enabled": r.enabled, "created_at": r.created_at}))
        .collect();
    let modules: Vec<Value> = queries::list_game_modules(&st.pool, id)
        .await?
        .into_iter()
        .map(
            |r| json!({"name": r.name, "hash": r.hash, "size": r.size, "updated_at": r.updated_at}),
        )
        .collect();
    let rooms = st
        .redis
        .list_rooms(ProjectId::from_uuid(id))
        .await
        .unwrap_or_default();
    Ok(Json(json!({
        "project": p,
        "effective_limits": net_control::ProjectLimits::from_json(&p.limits).effective(&st.cfg.limits),
        "keys": keys,
        "webhooks": webhooks,
        "modules": modules,
        "rooms": rooms,
    })))
}

async fn op_fleet_usage(
    State(st): State<Shared>,
    h: HeaderMap,
    Query(q): Query<UsageQuery>,
) -> ApiResult<Json<Value>> {
    operator(&st, &h)?;
    let to = q.to.unwrap_or_else(Utc::now);
    let from = q.from.unwrap_or(to - chrono::Duration::days(30));
    let rows = queries::usage_fleet_between(&st.pool, from, to).await?;
    Ok(Json(json!({"from": from, "to": to, "usage": rows})))
}

async fn op_replay(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    operator(&st, &h)?;
    let row = queries::get_room(&st.pool, id).await?;
    let body = st
        .redis
        .get_replay(ProjectId::from_uuid(row.project_id), RoomId::from_uuid(id))
        .await?
        .ok_or_else(|| NetError::NotFound("replay".into()))?;
    let v: Value = serde_json::from_slice(&body).map_err(|e| NetError::Internal(e.to_string()))?;
    Ok(Json(v))
}

async fn op_usage(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<UsageQuery>,
) -> ApiResult<Json<Value>> {
    operator(&st, &h)?;
    let to = q.to.unwrap_or_else(Utc::now);
    let from = q.from.unwrap_or(to - chrono::Duration::days(30));
    let rows = queries::usage_between(&st.pool, id, from, to).await?;
    Ok(Json(json!({"from": from, "to": to, "usage": rows})))
}

async fn op_project_rooms(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    operator(&st, &h)?;
    let rooms = st
        .redis
        .list_rooms(ProjectId::from_uuid(id))
        .await
        .unwrap_or_default();
    Ok(Json(json!({"rooms": rooms})))
}

/// Every live room in the region, across projects (operator view).
async fn op_rooms(State(st): State<Shared>, h: HeaderMap) -> ApiResult<Json<Value>> {
    operator(&st, &h)?;
    let mut out = Vec::new();
    for p in queries::list_projects(&st.pool).await? {
        for r in st
            .redis
            .list_rooms(ProjectId::from_uuid(p.id))
            .await
            .unwrap_or_default()
        {
            let mut v = serde_json::to_value(&r).unwrap_or_default();
            if let Some(o) = v.as_object_mut() {
                o.insert("project_id".into(), json!(p.id));
                o.insert("project_name".into(), json!(p.name));
            }
            out.push(v);
        }
    }
    Ok(Json(json!({"rooms": out})))
}
