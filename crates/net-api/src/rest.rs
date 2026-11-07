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
        .route("/v1/operator/projects/:id/limits", post(op_set_limits))
        .route("/v1/operator/nodes", get(op_nodes))
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
    let row = queries::find_api_key(&st.pool, &hash_key(k))
        .await?
        .ok_or_else(|| NetError::Unauthorized("unknown api key".into()))?;
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
        Err(NetError::RateLimited)
    }
}

/// Per-key grant budget (`limits.grants_per_sec_per_key`): one grant per player login.
fn key_grant(st: &Shared, key: &ApiKeyRow) -> Result<()> {
    if st.guards.key_grants.allow(key.id) {
        Ok(())
    } else {
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
    let live = st
        .redis
        .get_room(ProjectId::from_uuid(key.project_id), RoomId::from_uuid(id))
        .await?;
    let players: Vec<Value> = st
        .host
        .get(RoomId::from_uuid(id))
        .map(|hr| {
            hr.lock()
                .room
                .player_infos()
                .into_iter()
                .map(|p| json!({"actor": p.actor, "player_id": p.player_id, "name": p.name, "online": p.online}))
                .collect()
        })
        .unwrap_or_default();
    Ok(Json(json!({"room": row, "live": live, "players": players})))
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
    Ok(Json(json!({"keys": rows.into_iter().map(|r| json!({
        "id": r.id, "prefix": r.prefix, "name": r.name, "scopes": r.scopes,
        "created_at": r.created_at, "last_used_at": r.last_used_at, "revoked_at": r.revoked_at
    })).collect::<Vec<_>>()})))
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
