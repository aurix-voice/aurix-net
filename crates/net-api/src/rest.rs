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
use std::net::IpAddr;
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
        .route("/v1/rooms/:id/desyncs", get(list_desyncs))
        .route("/v1/rooms/:id/desyncs/:tick/:actor", get(get_desync))
        .route("/v1/usage", get(usage))
        .route("/v1/keys", get(list_keys).post(create_key))
        .route("/v1/keys/:id", delete(revoke_key))
        .route("/v1/webhooks", get(list_webhooks).post(create_webhook))
        .route("/v1/matchmaking", get(mm_stats))
        .route("/v1/players/:player_id", get(player_lookup))
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
        .route("/v1/operator/projects/:id/grants", post(op_create_grant))
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
        .route("/v1/operator/attention", get(op_attention))
        .route(
            "/v1/operator/tokens",
            get(op_list_tokens).post(op_create_token),
        )
        .route("/v1/operator/tokens/:id", delete(op_revoke_token))
        .route(
            "/v1/operator/webhooks",
            get(op_list_alert_webhooks).post(op_create_alert_webhook),
        )
        .route("/v1/operator/webhooks/:id", delete(op_delete_alert_webhook))
        .route(
            "/v1/operator/webhooks/:id/deliveries",
            get(op_alert_webhook_deliveries),
        )
        .route(
            "/v1/operator/webhooks/:id/test",
            post(op_alert_webhook_test),
        )
        .route("/v1/operator/projects/:id/matchmaking", get(op_mm_stats))
        .route(
            "/v1/operator/projects/:id/players/:player_id",
            get(op_player_lookup),
        )
        .route("/v1/internal/players/:player_id", get(internal_find_player))
        .route("/v1/internal/presence", get(internal_presence))
        .route("/v1/internal/stats", get(internal_stats))
        .route("/v1/operator/nodes/:id/stats", get(op_node_stats))
        .route(
            "/v1/operator/projects/:id/webhooks/:wid",
            delete(op_delete_webhook),
        )
        .route(
            "/v1/operator/projects/:id/modules/:name",
            delete(op_delete_module),
        )
        .route("/v1/internal/rooms/:id/players", get(internal_players))
        .route("/v1/internal/rooms/:id/replay", get(internal_replay))
        .route("/v1/internal/drain", post(internal_drain))
        .route("/v1/webhooks/:id/deliveries", get(webhook_deliveries))
        .route("/v1/rejections", get(rejections))
        .route("/v1/operator/projects/:id/rejections", get(op_rejections))
        .route("/v1/webhooks/:id/test", post(webhook_test))
        .route("/v1/webhooks/:id/rotate", post(webhook_rotate))
        .route(
            "/v1/operator/projects/:id/webhooks/:wid/deliveries",
            get(op_webhook_deliveries),
        )
        .route(
            "/v1/operator/projects/:id/webhooks/:wid/rotate",
            post(op_webhook_rotate),
        )
        .route(
            "/v1/operator/projects/:id/webhooks/:wid/test",
            post(op_webhook_test),
        )
        .route("/v1/internal/rooms/:id", delete(internal_close_room))
        .route(
            "/v1/internal/rooms/:id/players/:actor",
            delete(internal_kick),
        )
        .route("/v1/rooms/:id/players/:actor", delete(kick_player))
        .route("/v1/rooms/:id/notice", post(room_notice))
        .route("/v1/notice", post(project_notice))
        .route("/v1/operator/rooms/:id/notice", post(op_room_notice))
        .route("/v1/operator/projects/:id/notice", post(op_project_notice))
        .route("/v1/internal/rooms/:id/notice", post(internal_room_notice))
        .route("/v1/internal/notice", post(internal_project_notice))
        .route("/v1/bans", get(list_bans).post(create_ban))
        .route("/v1/bans/:player_id", delete(delete_ban))
        .route(
            "/v1/operator/projects/:id/bans",
            get(op_list_bans).post(op_create_ban),
        )
        .route(
            "/v1/operator/projects/:id/bans/:player_id",
            delete(op_delete_ban),
        )
        .route(
            "/v1/operator/rooms/:id/players/:actor",
            delete(op_kick_player),
        )
        .route("/v1/operator/nodes/:id/drain", post(op_drain_node))
        .route("/v1/operator/rooms/:id/replay", get(op_replay))
        .route("/v1/operator/rooms/:id/desyncs", get(op_list_desyncs))
        .route(
            "/v1/operator/rooms/:id/desyncs/:tick/:actor",
            get(op_get_desync),
        )
        .route(
            "/v1/operator/projects/:id",
            get(op_project).delete(op_delete_project),
        )
        .route("/v1/operator/projects/:id/usage", get(op_usage))
        .route("/v1/operator/billing", get(op_billing))
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
    if !row.allowed_cidrs.is_empty() {
        let ip = h
            .get(crate::ratelimit::CLIENT_IP_HEADER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<IpAddr>().ok());
        if !ip.is_some_and(|ip| cidr_allows(&row.allowed_cidrs, ip)) {
            reject(st, ProjectId::from_uuid(row.project_id), "ip_denied");
            return Err(NetError::Forbidden(
                "api key is not allowed from this address".into(),
            ));
        }
    }
    let _ = queries::touch_api_key(&st.pool, row.id).await;
    Ok(row)
}

/// Who the operator is: the bootstrap config token acts as `operator`, named tokens as `op:<name>`.
pub(crate) struct Operator {
    pub actor: String,
}

async fn operator(st: &Shared, h: &HeaderMap) -> Result<Operator> {
    let t = bearer(h).unwrap_or("");
    if net_auth::admin::admin_token_ok(&st.cfg.auth.admin_token, t) {
        return Ok(Operator {
            actor: "operator".into(),
        });
    }
    if looks_like_key(t) {
        let ip = h
            .get(crate::ratelimit::CLIENT_IP_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        if let Some(row) =
            queries::find_operator_token(&st.pool, &hash_key(t), ip.as_deref()).await?
        {
            if !row.allowed_cidrs.is_empty()
                && !ip
                    .as_deref()
                    .and_then(|v| v.parse::<IpAddr>().ok())
                    .is_some_and(|ip| cidr_allows(&row.allowed_cidrs, ip))
            {
                st.metrics.errors.with_label_values(&["ip_denied"]).inc();
                return Err(NetError::Forbidden(
                    "operator token is not allowed from this address".into(),
                ));
            }
            return Ok(Operator {
                actor: format!("op:{}", row.name),
            });
        }
    }
    Err(NetError::Unauthorized("operator token required".into()))
}

#[derive(Deserialize)]
struct OperatorTokenReq {
    #[serde(default)]
    name: String,
    #[serde(default)]
    expires_in_secs: Option<u64>,
    /// Source networks allowed to use the token (CIDR or bare IP); empty = anywhere.
    #[serde(default)]
    allowed_cidrs: Vec<String>,
}

fn operator_token_json(r: &net_db::models::OperatorTokenRow) -> Value {
    json!({"id": r.id, "name": r.name, "prefix": r.prefix, "created_by": r.created_by, "created_at": r.created_at,
           "expires_at": r.expires_at, "revoked_at": r.revoked_at, "last_used_at": r.last_used_at,
           "allowed_cidrs": r.allowed_cidrs, "last_used_ip": r.last_used_ip})
}

async fn op_list_tokens(State(st): State<Shared>, h: HeaderMap) -> ApiResult<Json<Value>> {
    operator(&st, &h).await?;
    let rows = queries::list_operator_tokens(&st.pool).await?;
    Ok(Json(
        json!({"tokens": rows.iter().map(operator_token_json).collect::<Vec<_>>()}),
    ))
}

/// Issue a named operator token; the plaintext is returned once.
#[derive(Deserialize)]
struct AlertWebhookReq {
    url: String,
    #[serde(default)]
    events: Vec<String>,
}

async fn op_list_alert_webhooks(State(st): State<Shared>, h: HeaderMap) -> ApiResult<Json<Value>> {
    operator(&st, &h).await?;
    let rows = queries::list_operator_webhooks(&st.pool).await?;
    Ok(Json(json!({"webhooks": rows})))
}

async fn op_create_alert_webhook(
    State(st): State<Shared>,
    h: HeaderMap,
    Json(req): Json<AlertWebhookReq>,
) -> ApiResult<Json<Value>> {
    let op = operator(&st, &h).await?;
    crate::webhooks::validate_webhook_url(&req.url, st.cfg.server.environment == "development")?;
    crate::webhooks::validate_operator_events(&req.events)?;
    let secret = ApiKeyMaterial::generate().plaintext;
    let row = queries::insert_operator_webhook(&st.pool, &req.url, &secret, &req.events, &op.actor)
        .await?;
    audit_log(
        &st,
        None,
        &op.actor,
        "operator_webhook.create",
        &row.id.to_string(),
        json!({"url": row.url, "events": row.events}),
    )
    .await;
    let mut out = serde_json::to_value(&row).unwrap_or_default();
    out["secret"] = Value::String(secret);
    Ok(Json(out))
}

async fn op_delete_alert_webhook(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let op = operator(&st, &h).await?;
    if !queries::delete_operator_webhook(&st.pool, id).await? {
        return Err(NetError::NotFound("webhook not found".into()).into());
    }
    st.webhooks.invalidate(ProjectId::from_uuid(Uuid::nil()));
    audit_log(
        &st,
        None,
        &op.actor,
        "operator_webhook.delete",
        &id.to_string(),
        json!({}),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

async fn op_alert_webhook_deliveries(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<DeliveriesQuery>,
) -> ApiResult<Json<Value>> {
    operator(&st, &h).await?;
    queries::get_operator_webhook(&st.pool, id)
        .await?
        .ok_or_else(|| NetError::NotFound("webhook not found".into()))?;
    let rows =
        queries::list_operator_webhook_deliveries(&st.pool, id, q.limit.unwrap_or(50)).await?;
    Ok(Json(json!({"deliveries": rows})))
}

async fn op_alert_webhook_test(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let op = operator(&st, &h).await?;
    queries::get_operator_webhook(&st.pool, id)
        .await?
        .ok_or_else(|| NetError::NotFound("webhook not found".into()))?;
    st.webhooks.test_fleet(id);
    audit_log(
        &st,
        None,
        &op.actor,
        "operator_webhook.test",
        &id.to_string(),
        json!({}),
    )
    .await;
    Ok(Json(json!({"queued": true})))
}

#[derive(Deserialize, Default)]
struct BillingQuery {
    /// `YYYY-MM` (UTC); defaults to the current month.
    #[serde(default)]
    month: Option<String>,
    #[serde(default)]
    format: Option<String>,
}

fn month_bounds(spec: Option<&str>) -> Result<(chrono::DateTime<Utc>, chrono::DateTime<Utc>)> {
    use chrono::{Datelike, TimeZone};
    let (y, m) = match spec {
        None => {
            let n = Utc::now();
            (n.year(), n.month())
        }
        Some(s) => {
            let (y, m) = s
                .split_once('-')
                .ok_or_else(|| NetError::Invalid("month: YYYY-MM".into()))?;
            let y: i32 = y
                .parse()
                .map_err(|_| NetError::Invalid("month: YYYY-MM".into()))?;
            let m: u32 = m
                .parse()
                .map_err(|_| NetError::Invalid("month: YYYY-MM".into()))?;
            if !(1..=12).contains(&m) || !(2020..=2100).contains(&y) {
                return Err(NetError::Invalid("month: YYYY-MM".into()));
            }
            (y, m)
        }
    };
    let from = Utc
        .with_ymd_and_hms(y, m, 1, 0, 0, 0)
        .single()
        .ok_or_else(|| NetError::Invalid("month: YYYY-MM".into()))?;
    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    let to = Utc
        .with_ymd_and_hms(ny, nm, 1, 0, 0, 0)
        .single()
        .ok_or_else(|| NetError::Invalid("month: YYYY-MM".into()))?;
    Ok((from, to))
}

/// Month report per project for invoicing: the metered units, rooms created and the caps that
/// applied. Soft-deleted projects are included (they used the service that month).
async fn op_billing(
    State(st): State<Shared>,
    h: HeaderMap,
    Query(q): Query<BillingQuery>,
) -> ApiResult<axum::response::Response> {
    use axum::response::IntoResponse;
    operator(&st, &h).await?;
    let (from, to) = month_bounds(q.month.as_deref())?;
    let projects = queries::list_projects_all(&st.pool).await?;
    let usage = queries::usage_by_project_between(&st.pool, from, to).await?;
    let created: std::collections::HashMap<Uuid, i64> =
        queries::rooms_created_between(&st.pool, from, to)
            .await?
            .into_iter()
            .collect();
    let mut by: std::collections::HashMap<Uuid, std::collections::BTreeMap<String, f64>> =
        Default::default();
    for r in usage {
        *by.entry(r.project_id)
            .or_default()
            .entry(r.metric)
            .or_default() += r.quantity;
    }
    let get =
        |m: &std::collections::BTreeMap<String, f64>, k: &str| m.get(k).copied().unwrap_or(0.0);
    let mut rows: Vec<Value> = Vec::new();
    let mut total = std::collections::BTreeMap::<&str, f64>::new();
    for p in &projects {
        let m = by.remove(&p.id).unwrap_or_default();
        let rooms = created.get(&p.id).copied().unwrap_or(0);
        if m.is_empty() && rooms == 0 && p.deleted_at.is_some() {
            continue;
        }
        let l = net_control::ProjectLimits::from_json(&p.limits);
        for k in ["player_minutes", "room_minutes", "bytes_in", "bytes_out"] {
            *total.entry(k).or_default() += get(&m, k);
        }
        *total.entry("rooms_created").or_default() += rooms as f64;
        rows.push(json!({
            "project_id": p.id, "name": p.name, "plan": p.plan,
            "player_minutes": get(&m, "player_minutes"), "room_minutes": get(&m, "room_minutes"),
            "bytes_in": get(&m, "bytes_in"), "bytes_out": get(&m, "bytes_out"),
            "rooms_created": rooms,
            "cap_player_minutes": l.monthly_player_minutes, "cap_bytes": l.monthly_bytes,
            "deleted_at": p.deleted_at, "disabled_at": p.disabled_at,
        }));
    }
    rows.sort_by(|a, b| {
        b["player_minutes"]
            .as_f64()
            .partial_cmp(&a["player_minutes"].as_f64())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let month = from.format("%Y-%m").to_string();
    match q.format.as_deref() {
        None | Some("json") => Ok(Json(
            json!({"month": month, "from": from, "to": to, "projects": rows, "totals": total}),
        )
        .into_response()),
        Some("csv") => {
            let mut csv = String::from(
                "project_id,name,plan,player_minutes,room_minutes,bytes_in,bytes_out,rooms_created,cap_player_minutes,cap_bytes,deleted_at\n",
            );
            for r in &rows {
                let cell = |k: &str| match &r[k] {
                    Value::Null => String::new(),
                    Value::String(s) => csv_cell(s),
                    v => v.to_string(),
                };
                csv.push_str(&format!(
                    "{},{},{},{},{},{},{},{},{},{},{}\n",
                    cell("project_id"),
                    cell("name"),
                    cell("plan"),
                    cell("player_minutes"),
                    cell("room_minutes"),
                    cell("bytes_in"),
                    cell("bytes_out"),
                    cell("rooms_created"),
                    cell("cap_player_minutes"),
                    cell("cap_bytes"),
                    cell("deleted_at"),
                ));
            }
            Ok((
                [
                    (
                        axum::http::header::CONTENT_TYPE,
                        "text/csv; charset=utf-8".to_string(),
                    ),
                    (
                        axum::http::header::CONTENT_DISPOSITION,
                        format!("attachment; filename=\"aurix-net-billing-{month}.csv\""),
                    ),
                ],
                csv,
            )
                .into_response())
        }
        _ => Err(NetError::Invalid("format: json or csv".into()).into()),
    }
}

async fn op_create_token(
    State(st): State<Shared>,
    h: HeaderMap,
    Json(req): Json<OperatorTokenReq>,
) -> ApiResult<Json<Value>> {
    let op = operator(&st, &h).await?;
    let name = req.name.trim();
    if name.is_empty() || name.len() > 64 || !name.chars().all(|c| c.is_ascii_graphic() || c == ' ')
    {
        return Err(NetError::Invalid("name: 1-64 printable ASCII characters".into()).into());
    }
    let cidrs = parse_cidrs(&req.allowed_cidrs)?;
    let m = ApiKeyMaterial::generate();
    let row = queries::insert_operator_token(
        &st.pool,
        name,
        &m.hash,
        &m.prefix,
        &op.actor,
        key_expiry(req.expires_in_secs),
        &cidrs,
    )
    .await?;
    audit_log(
        &st,
        None,
        &op.actor,
        "operator_token.create",
        &row.id.to_string(),
        json!({"name": row.name, "expires_at": row.expires_at, "allowed_cidrs": row.allowed_cidrs}),
    )
    .await;
    let mut out = operator_token_json(&row);
    out["token"] = Value::String(m.plaintext);
    Ok(Json(out))
}

async fn op_revoke_token(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let op = operator(&st, &h).await?;
    let row = queries::revoke_operator_token(&st.pool, id)
        .await?
        .ok_or_else(|| NetError::NotFound("token not found or already revoked".into()))?;
    audit_log(
        &st,
        None,
        &op.actor,
        "operator_token.revoke",
        &id.to_string(),
        json!({"name": row.name}),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
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
/// First instant of the current UTC calendar month.
pub(crate) fn month_start() -> chrono::DateTime<chrono::Utc> {
    use chrono::{Datelike, TimeZone};
    let now = chrono::Utc::now();
    chrono::Utc
        .with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0)
        .single()
        .unwrap_or(now)
}

/// Month-to-date (player_minutes, bytes) of a project, cached 30 s per node so the join path
/// does not add a SUM to every room creation.
pub(crate) async fn usage_month(st: &Shared, project: Uuid) -> Result<(f64, f64)> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    use std::time::Instant;
    type UsageCache = Mutex<HashMap<Uuid, (Instant, (f64, f64))>>;
    static CACHE: OnceLock<UsageCache> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some((at, v)) = cache.lock().unwrap().get(&project) {
        if at.elapsed() < std::time::Duration::from_secs(30) {
            return Ok(*v);
        }
    }
    let v = queries::usage_totals_since(&st.pool, project, month_start()).await?;
    let mut c = cache.lock().unwrap();
    if c.len() > 10_000 {
        c.clear();
    }
    c.insert(project, (Instant::now(), v));
    Ok(v)
}

/// Refuses a new room once a monthly cap is reached (counted as rejection `usage_cap`).
pub(crate) async fn check_usage_cap(
    st: &Shared,
    project: Uuid,
    limits: &net_control::quotas::EffectiveLimits,
) -> Result<()> {
    if limits.monthly_player_minutes.is_none() && limits.monthly_bytes.is_none() {
        return Ok(());
    }
    let (minutes, bytes) = usage_month(st, project).await?;
    let hit = match (limits.monthly_player_minutes, limits.monthly_bytes) {
        (Some(cap), _) if minutes >= cap as f64 => {
            Some(format!("monthly cap of {cap} player-minutes reached"))
        }
        (_, Some(cap)) if bytes >= cap as f64 => {
            Some(format!("monthly cap of {cap} bytes reached"))
        }
        _ => None,
    };
    if let Some(msg) = hit {
        reject(st, ProjectId::from_uuid(project), "usage_cap");
        return Err(NetError::QuotaExceeded(msg));
    }
    Ok(())
}

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
    operator(&st, &h).await?;
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
    if !valid_player_id(&req.player_id) {
        return Err(NetError::Invalid("player_id: 1..=128 chars".into()).into());
    }
    if let Some(b) = queries::get_ban(&st.pool, key.project_id, &req.player_id).await? {
        reject(&st, ProjectId::from_uuid(key.project_id), "banned");
        return Err(NetError::Forbidden(ban_message(&b)).into());
    }
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

/// Operator issues a short-lived test grant for a project (debugging a client without holding
/// the project's API key). Same checks as the key path (suspended, banned); the token is never
/// written to the audit log, only who/what/for how long.
async fn op_create_grant(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(req): Json<GrantReq>,
) -> ApiResult<Json<Value>> {
    let op = operator(&st, &h).await?;
    let prow = queries::get_project(&st.pool, id).await?;
    if prow.disabled_at.is_some() {
        return Err(NetError::Forbidden("project is suspended".into()).into());
    }
    if !valid_player_id(&req.player_id) {
        return Err(NetError::Invalid("player_id: 1..=128 chars".into()).into());
    }
    if let Some(b) = queries::get_ban(&st.pool, id, &req.player_id).await? {
        return Err(NetError::Forbidden(ban_message(&b)).into());
    }
    let scopes: Vec<&str> = req
        .scopes
        .as_ref()
        .map(|v| v.iter().map(String::as_str).collect())
        .unwrap_or_else(|| vec!["join", "create"]);
    let ttl = req.ttl_secs.unwrap_or(300).min(3600);
    let (token, claims) = st.grants.issue(
        ProjectId::from_uuid(id),
        &req.player_id,
        &req.name,
        req.room.as_deref(),
        &scopes,
        req.meta,
        Some(ttl),
    )?;
    audit_log(
        &st,
        Some(id),
        &op.actor,
        "grant.issue",
        &req.player_id,
        json!({"scopes": scopes, "room": req.room, "ttl_secs": ttl}),
    )
    .await;
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
    /// REST creates relay rooms only; sync/lockstep need the control-plane `create`.
    #[serde(default)]
    mode: Option<String>,
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
    /// Player ids whose slots are held for 60 s (party join); max 32.
    #[serde(default)]
    reserved: Vec<String>,
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
    if req.mode.as_deref().is_some_and(|m| m != "relay") {
        return Err(NetError::Invalid(
            "REST creates relay rooms only; create sync/lockstep rooms through the control plane"
                .into(),
        )
        .into());
    }
    validate_room_create(&req.name, &req.lobby_props)?;
    let project = ProjectId::from_uuid(key.project_id);
    if st.host.by_name(project, &req.name).is_some() {
        return Err(NetError::Conflict("room name already in use".into()).into());
    }
    let prow = queries::get_project(&st.pool, key.project_id).await?;
    let limits = net_control::ProjectLimits::from_json(&prow.limits).effective(&st.cfg.limits);
    let open_rooms = queries::count_open_rooms(&st.pool, key.project_id).await?;
    if open_rooms >= limits.max_rooms as i64 {
        reject(&st, project, "quota");
        return Err(NetError::QuotaExceeded(format!("max {} open rooms", limits.max_rooms)).into());
    }
    check_usage_cap(&st, key.project_id, &limits).await?;
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
        reserved: req.reserved.iter().take(32).cloned().collect(),
        reserved_until: (!req.reserved.is_empty())
            .then(|| std::time::Instant::now() + std::time::Duration::from_secs(60)),
        cleanup_cache_on_leave: crate::ws::prop_bool(&req.props, "cleanup_cache_on_leave", true),
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
        let g = hr.lock();
        let now = std::time::Instant::now();
        let stats: std::collections::HashMap<_, _> = g
            .room
            .player_stats(now)
            .into_iter()
            .map(|s| (s.actor, s))
            .collect();
        g.room
            .player_infos()
            .into_iter()
            .map(|p| {
                let s = stats.get(&p.actor);
                let rtt_ms = g
                    .sessions
                    .get(&p.actor)
                    .and_then(|s| s.data.as_ref())
                    .map(|d| d.link.lock().stats().rtt.as_secs_f64() * 1000.0)
                    .filter(|r| *r > 0.0);
                json!({
                    "actor": p.actor, "player_id": p.player_id, "name": p.name, "online": p.online,
                    "events": s.map(|s| s.events), "bytes_in": s.map(|s| s.bytes_in),
                    "eps": s.map(|s| s.eps), "idle_secs": s.and_then(|s| s.idle_secs),
                    "since_secs": s.map(|s| s.since_secs), "rtt_ms": rtt_ms,
                })
            })
            .collect()
    })
}

/// Live statistics of this process for the operator console: what the node is doing right now,
/// not what Postgres remembers about it.
fn local_stats(st: &Shared) -> Value {
    let mut by_mode: std::collections::BTreeMap<String, u32> = Default::default();
    let mut players = 0usize;
    let mut spectators = 0usize;
    for r in st.host.all() {
        let g = r.lock();
        *by_mode
            .entry(format!("{:?}", g.room.cfg.mode).to_lowercase())
            .or_default() += 1;
        players += g.room.online_count() as usize;
        spectators += g.room.player_infos().iter().filter(|p| p.spectator).count();
    }
    let m = &st.metrics;
    let tick = m.sync_tick.get_sample_count();
    let tick_avg_ms = if tick > 0 {
        m.sync_tick.get_sample_sum() / tick as f64 * 1000.0
    } else {
        0.0
    };
    let tick_p99_ms = {
        use prometheus::core::Metric as _;
        let proto = m.sync_tick.metric();
        let h = proto.get_histogram();
        let total = h.get_sample_count();
        let mut p99 = 0.0;
        if total > 0 {
            let target = (total as f64 * 0.99).ceil() as u64;
            for b in h.get_bucket() {
                if b.get_cumulative_count() >= target {
                    p99 = b.get_upper_bound() * 1000.0;
                    break;
                }
            }
        }
        p99
    };
    let rss_mb = std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|s| {
            s.split_whitespace()
                .nth(1)
                .and_then(|p| p.parse::<u64>().ok())
        })
        .map(|pages| pages * 4096 / 1_048_576);
    json!({
        "node_id": st.node_id,
        "version": env!("CARGO_PKG_VERSION"),
        "region": st.region,
        "uptime_secs": st.started.elapsed().as_secs(),
        "draining": st.is_draining(),
        "rooms": st.host.len(),
        "rooms_by_mode": by_mode,
        "players": players,
        "spectators": spectators,
        "control_sessions": m.control_sessions.get(),
        "datagrams_in": m.datagrams_in.get(),
        "datagrams_out": m.datagrams_out.get(),
        "bytes_in": m.bytes_in.get(),
        "bytes_out": m.bytes_out.get(),
        "sync_ticks": tick,
        "sync_tick_avg_ms": tick_avg_ms,
        "sync_tick_p99_ms": tick_p99_ms,
        "tick_overruns": m.tick_overruns.get(),
        "sync_lost_deltas": m.sync_lost_deltas.get(),
        "sync_resyncs": m.sync_resyncs.get(),
        "lockstep_desyncs": m.lockstep_desyncs.get(),
        "rooms_adopted": m.rooms_adopted.get(),
        "rss_mb": rss_mb,
    })
}

async fn internal_stats(State(st): State<Shared>, h: HeaderMap) -> ApiResult<Json<Value>> {
    operator(&st, &h).await?;
    Ok(Json(local_stats(&st)))
}

/// Live stats of any node: answered locally for this node, fetched from the node otherwise.
async fn op_node_stats(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    operator(&st, &h).await?;
    if id == st.node_id.0 {
        return Ok(Json(local_stats(&st)));
    }
    let n = st
        .registry
        .get(net_common::NodeId::from_uuid(id))
        .ok_or_else(|| NetError::NotFound("node is not live".into()))?;
    Ok(Json(
        internal_get(&st, &n.http_url, "/v1/internal/stats").await?,
    ))
}

fn local_find_player(st: &Shared, project: ProjectId, player_id: &str) -> Vec<Value> {
    st.host
        .find_player(project, player_id)
        .into_iter()
        .map(|(room, name, mode, actor, pname, online)| {
            json!({"room_id": room, "name": name, "mode": mode, "node_id": st.node_id, "actor": actor, "player_name": pname, "online": online})
        })
        .collect()
}

#[derive(Deserialize)]
struct ProjectQuery {
    project: Uuid,
}

/// Node-to-node: rooms hosted here where a player sits. Fleet-wide lookups fan out to every live node.
async fn internal_find_player(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(player_id): Path<String>,
    Query(q): Query<ProjectQuery>,
) -> ApiResult<Json<Value>> {
    operator(&st, &h).await?;
    Ok(Json(
        json!({"rooms": local_find_player(&st, ProjectId::from_uuid(q.project), &player_id)}),
    ))
}

#[derive(Deserialize)]
struct PresenceQuery {
    project: Uuid,
    /// Player ids separated by `\n` (never part of a valid id).
    #[serde(default)]
    ids: String,
}

/// Node-to-node: presence and room slots of players on this node (`find_friends` fans out to every node).
async fn internal_presence(
    State(st): State<Shared>,
    h: HeaderMap,
    Query(q): Query<PresenceQuery>,
) -> ApiResult<Json<Value>> {
    operator(&st, &h).await?;
    let ids: Vec<String> = q
        .ids
        .split('\n')
        .filter(|s| !s.is_empty())
        .take(crate::presence::MAX_FRIENDS)
        .map(str::to_string)
        .collect();
    Ok(Json(json!({
        "players": crate::presence::local_friends(&st, ProjectId::from_uuid(q.project), &ids)
    })))
}

pub(crate) fn pct(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[derive(Deserialize, Serialize, Clone)]
struct NoticeReq {
    #[serde(default)]
    text: String,
    #[serde(default)]
    level: Option<String>,
}

fn notice_level(req: &NoticeReq) -> Result<(String, &str)> {
    let text = req.text.trim();
    if text.is_empty() || text.chars().count() > 512 || text.chars().any(|c| c.is_control()) {
        return Err(NetError::Invalid(
            "text: 1-512 characters, no control characters".into(),
        ));
    }
    let level = match req.level.as_deref() {
        None | Some("info") => "info",
        Some("warn") => "warn",
        _ => return Err(NetError::Invalid("level: info or warn".into())),
    };
    Ok((text.to_string(), level))
}

/// Notice to one room wherever it is hosted; returns players reached.
async fn notice_anywhere(
    st: &Shared,
    project: ProjectId,
    id: Uuid,
    req: &NoticeReq,
) -> Result<usize> {
    let (text, level) = notice_level(req)?;
    let room = RoomId::from_uuid(id);
    if st.host.get(room).is_some() {
        return crate::ws::notice_room(st, room, &text, level);
    }
    let live = st
        .redis
        .get_room(project, room)
        .await?
        .ok_or_else(|| NetError::NotFound("room is not live".into()))?;
    let node = Uuid::parse_str(&live.node)
        .ok()
        .and_then(|n| st.registry.get(net_common::NodeId::from_uuid(n)))
        .ok_or_else(|| NetError::NotFound("hosting node unknown".into()))?;
    let v = internal_post(
        st,
        &node.http_url,
        &format!("/v1/internal/rooms/{id}/notice"),
        json!({"text": text, "level": level}),
    )
    .await?;
    Ok(v["players"].as_u64().unwrap_or(0) as usize)
}

/// Notice to every live room of a project on every node; returns (rooms, players).
async fn notice_project_anywhere(
    st: &Shared,
    project: Uuid,
    req: &NoticeReq,
) -> Result<(usize, usize)> {
    let (text, level) = notice_level(req)?;
    let (mut rooms, mut players) =
        crate::ws::notice_project(st, ProjectId::from_uuid(project), &text, level);
    let body = json!({"text": text, "level": level});
    let path = format!("/v1/internal/notice?project={project}");
    let (path, body) = (path.as_str(), &body);
    let remote = st
        .registry
        .snapshot()
        .into_iter()
        .filter(|n| n.id != st.node_id);
    let results = futures::future::join_all(remote.map(|n| async move {
        let r = internal_post(st, &n.http_url, path, body.clone()).await;
        (n, r)
    }))
    .await;
    for (n, r) in results {
        match r {
            Ok(v) => {
                rooms += v["rooms"].as_u64().unwrap_or(0) as usize;
                players += v["players"].as_u64().unwrap_or(0) as usize;
            }
            Err(e) => tracing::warn!(node = %n.http_url, error = %e, "notice: node unreachable"),
        }
    }
    Ok((rooms, players))
}

async fn room_notice(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(req): Json<NoticeReq>,
) -> ApiResult<Json<Value>> {
    let key = api_key(&st, &h, Scope::Rooms).await?;
    key_write(&st, &key)?;
    let row = queries::get_room(&st.pool, id).await?;
    if row.project_id != key.project_id {
        return Err(NetError::NotFound("room not found".into()).into());
    }
    let players = notice_anywhere(&st, ProjectId::from_uuid(row.project_id), id, &req).await?;
    audit_log(
        &st,
        Some(row.project_id),
        &format!("key:{}", key.id),
        "room.notice",
        &id.to_string(),
        json!({"text": req.text, "players": players}),
    )
    .await;
    Ok(Json(json!({"rooms": 1, "players": players})))
}

async fn project_notice(
    State(st): State<Shared>,
    h: HeaderMap,
    Json(req): Json<NoticeReq>,
) -> ApiResult<Json<Value>> {
    let key = api_key(&st, &h, Scope::Rooms).await?;
    key_write(&st, &key)?;
    let (rooms, players) = notice_project_anywhere(&st, key.project_id, &req).await?;
    audit_log(
        &st,
        Some(key.project_id),
        &format!("key:{}", key.id),
        "project.notice",
        &key.project_id.to_string(),
        json!({"text": req.text, "rooms": rooms, "players": players}),
    )
    .await;
    Ok(Json(json!({"rooms": rooms, "players": players})))
}

async fn op_room_notice(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(req): Json<NoticeReq>,
) -> ApiResult<Json<Value>> {
    let op = operator(&st, &h).await?;
    let row = queries::get_room(&st.pool, id).await?;
    let players = notice_anywhere(&st, ProjectId::from_uuid(row.project_id), id, &req).await?;
    audit_log(
        &st,
        Some(row.project_id),
        &op.actor,
        "room.notice",
        &id.to_string(),
        json!({"text": req.text, "players": players}),
    )
    .await;
    Ok(Json(json!({"rooms": 1, "players": players})))
}

async fn op_project_notice(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(req): Json<NoticeReq>,
) -> ApiResult<Json<Value>> {
    let op = operator(&st, &h).await?;
    let (rooms, players) = notice_project_anywhere(&st, id, &req).await?;
    audit_log(
        &st,
        Some(id),
        &op.actor,
        "project.notice",
        &id.to_string(),
        json!({"text": req.text, "rooms": rooms, "players": players}),
    )
    .await;
    Ok(Json(json!({"rooms": rooms, "players": players})))
}

async fn internal_room_notice(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(req): Json<NoticeReq>,
) -> ApiResult<Json<Value>> {
    operator(&st, &h).await?;
    let (text, level) = notice_level(&req)?;
    let players = crate::ws::notice_room(&st, RoomId::from_uuid(id), &text, level)?;
    Ok(Json(json!({"players": players})))
}

async fn internal_project_notice(
    State(st): State<Shared>,
    h: HeaderMap,
    Query(q): Query<ProjectQuery>,
    Json(req): Json<NoticeReq>,
) -> ApiResult<Json<Value>> {
    operator(&st, &h).await?;
    let (text, level) = notice_level(&req)?;
    let (rooms, players) =
        crate::ws::notice_project(&st, ProjectId::from_uuid(q.project), &text, level);
    Ok(Json(json!({"rooms": rooms, "players": players})))
}

pub(crate) async fn internal_get(st: &Shared, http_url: &str, path: &str) -> Result<Value> {
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
    let resp = http
        .get(format!("{http_url}{path}"))
        .bearer_auth(&st.cfg.auth.admin_token)
        .send()
        .await
        .map_err(|e| NetError::Internal(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(NetError::Internal(format!(
            "node returned {}",
            resp.status()
        )));
    }
    resp.json::<Value>()
        .await
        .map_err(|e| NetError::Internal(e.to_string()))
}

/// Where a player is right now (every live node), whether they are banned, and what was done to them.
async fn player_lookup_json(st: &Shared, project: Uuid, player_id: &str) -> Result<Value> {
    if !valid_player_id(player_id) {
        return Err(NetError::Invalid("invalid player_id".into()));
    }
    let pid = ProjectId::from_uuid(project);
    let mut rooms = local_find_player(st, pid, player_id);
    let mut unreachable = Vec::new();
    let path = format!("/v1/internal/players/{}?project={project}", pct(player_id));
    let path = path.as_str();
    let remote = st
        .registry
        .snapshot()
        .into_iter()
        .filter(|n| n.id != st.node_id);
    let results = futures::future::join_all(remote.map(|n| async move {
        let r = internal_get(st, &n.http_url, path).await;
        (n, r)
    }))
    .await;
    for (n, r) in results {
        match r {
            Ok(v) => rooms.extend(v["rooms"].as_array().cloned().unwrap_or_default()),
            Err(e) => {
                tracing::warn!(node = %n.http_url, error = %e, "player lookup: node unreachable");
                unreachable.push(json!({"node_id": n.id, "http_url": n.http_url}));
            }
        }
    }
    let ban = queries::get_ban(&st.pool, project, player_id).await?;
    let audit = queries::list_audit_for_target(&st.pool, project, player_id, 20).await?;
    Ok(
        json!({"player_id": player_id, "rooms": rooms, "ban": ban, "audit": audit, "unreachable_nodes": unreachable}),
    )
}

async fn player_lookup(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(player_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let key = api_key(&st, &h, Scope::Read).await?;
    Ok(Json(
        player_lookup_json(&st, key.project_id, &player_id).await?,
    ))
}

async fn op_player_lookup(
    State(st): State<Shared>,
    h: HeaderMap,
    Path((id, player_id)): Path<(Uuid, String)>,
) -> ApiResult<Json<Value>> {
    operator(&st, &h).await?;
    Ok(Json(player_lookup_json(&st, id, &player_id).await?))
}

/// Node-to-node: player list of a room hosted here, for consoles pointed at a different node.
/// Guarded by the fleet-wide admin token; never proxied further (a room is hosted by one node).
async fn internal_players(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    operator(&st, &h).await?;
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
    let referee = st
        .host
        .get(RoomId::from_uuid(id))
        .map(|hr| hr.lock().lockstep.as_ref().is_some_and(|ls| ls.has_sim()));
    Ok(
        json!({"room": row, "live": live, "players": players, "players_source": source, "referee": referee}),
    )
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
    operator(&st, &h).await?;
    let row = queries::get_room(&st.pool, id).await?;
    Ok(Json(room_detail(&st, row).await?))
}

/// Operator force-close of any room (abuse response); audited.
async fn op_delete_room(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let op = operator(&st, &h).await?;
    let row = queries::get_room(&st.pool, id).await?;
    close_room_anywhere(&st, ProjectId::from_uuid(row.project_id), id).await;
    audit_log(
        &st,
        Some(row.project_id),
        &op.actor,
        "room.close",
        &id.to_string(),
        json!({}),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

/// Lockstep replay (input journal) of a closed room, kept for 24 h.
#[derive(Deserialize)]
struct ReplayQuery {
    /// First tick of the window (instant replay of a live room); default: everything kept.
    #[serde(default)]
    from: Option<u32>,
    /// End tick (exclusive).
    #[serde(default)]
    to: Option<u32>,
}

/// Replay of a lockstep room: the stored journal for a closed room (24 h), or — while the room is
/// live — the confirmed inputs the hosting node still holds, optionally windowed with
/// `?from=&to=` (kill cams, spectator rewinds, bug reports).
async fn get_replay(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<ReplayQuery>,
) -> ApiResult<Json<Value>> {
    let key = api_key(&st, &h, Scope::Read).await?;
    let row = queries::get_room(&st.pool, id).await?;
    if row.project_id != key.project_id {
        return Err(NetError::NotFound("room".into()).into());
    }
    Ok(Json(
        replay_anywhere(&st, ProjectId::from_uuid(row.project_id), id, &q).await?,
    ))
}

fn local_replay(st: &Shared, id: Uuid, q: &ReplayQuery) -> Option<Result<Value>> {
    let h = st.host.get(RoomId::from_uuid(id))?;
    let g = h.lock();
    Some(match g.lockstep.as_ref() {
        Some(l) => serde_json::to_value(l.replay_window(q.from.unwrap_or(0), q.to))
            .map_err(|e| NetError::Internal(e.to_string())),
        None => Err(NetError::Invalid(
            "replays exist for lockstep rooms only".into(),
        )),
    })
}

async fn replay_anywhere(
    st: &Shared,
    project: ProjectId,
    id: Uuid,
    q: &ReplayQuery,
) -> Result<Value> {
    if let Some(v) = local_replay(st, id, q) {
        return v;
    }
    let room = RoomId::from_uuid(id);
    if let Some(live) = st.redis.get_room(project, room).await? {
        if let Some(node) = Uuid::parse_str(&live.node)
            .ok()
            .and_then(|n| st.registry.get(net_common::NodeId::from_uuid(n)))
        {
            let mut path = format!(
                "/v1/internal/rooms/{id}/replay?from={}",
                q.from.unwrap_or(0)
            );
            if let Some(to) = q.to {
                path.push_str(&format!("&to={to}"));
            }
            return internal_get(st, &node.http_url, &path).await;
        }
    }
    let body = st
        .redis
        .get_replay(project, room)
        .await?
        .ok_or_else(|| NetError::NotFound("replay".into()))?;
    let mut r: net_rollback::Replay =
        serde_json::from_slice(&body).map_err(|e| NetError::Internal(e.to_string()))?;
    if q.from.is_some() || q.to.is_some() {
        crate::lockstep::window_replay(&mut r, q.from.unwrap_or(0), q.to);
    }
    serde_json::to_value(r).map_err(|e| NetError::Internal(e.to_string()))
}

async fn internal_replay(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<ReplayQuery>,
) -> ApiResult<Json<Value>> {
    operator(&st, &h).await?;
    let v = local_replay(&st, id, &q)
        .ok_or_else(|| NetError::NotFound("room not hosted here".into()))??;
    Ok(Json(v))
}

/// State dumps collected after lockstep desyncs (the desynced client's and the master's state
/// at the flagged tick), kept 24 h: compare them offline to find the non-deterministic code.
async fn list_desyncs(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let key = api_key(&st, &h, Scope::Read).await?;
    let row = queries::get_room(&st.pool, id).await?;
    if row.project_id != key.project_id {
        return Err(NetError::NotFound("room".into()).into());
    }
    let list = st
        .redis
        .list_desyncs(ProjectId::from_uuid(key.project_id), RoomId::from_uuid(id))
        .await?;
    Ok(Json(json!({"room": id, "desyncs": list})))
}

type Blob = ([(axum::http::header::HeaderName, &'static str); 1], Vec<u8>);

async fn desync_blob(
    st: &Shared,
    project: Uuid,
    room: Uuid,
    tick: u32,
    actor: u16,
) -> ApiResult<Blob> {
    let body = st
        .redis
        .get_desync(
            ProjectId::from_uuid(project),
            RoomId::from_uuid(room),
            tick,
            actor,
        )
        .await?
        .ok_or_else(|| NetError::NotFound("dump".into()))?;
    Ok((
        [(axum::http::header::CONTENT_TYPE, "application/octet-stream")],
        body,
    ))
}

async fn get_desync(
    State(st): State<Shared>,
    h: HeaderMap,
    Path((id, tick, actor)): Path<(Uuid, u32, u16)>,
) -> ApiResult<Blob> {
    let key = api_key(&st, &h, Scope::Read).await?;
    let row = queries::get_room(&st.pool, id).await?;
    if row.project_id != key.project_id {
        return Err(NetError::NotFound("room".into()).into());
    }
    desync_blob(&st, key.project_id, id, tick, actor).await
}

async fn op_list_desyncs(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    operator(&st, &h).await?;
    let row = queries::get_room(&st.pool, id).await?;
    let list = st
        .redis
        .list_desyncs(ProjectId::from_uuid(row.project_id), RoomId::from_uuid(id))
        .await?;
    Ok(Json(json!({"room": id, "desyncs": list})))
}

async fn op_get_desync(
    State(st): State<Shared>,
    h: HeaderMap,
    Path((id, tick, actor)): Path<(Uuid, u32, u16)>,
) -> ApiResult<Blob> {
    operator(&st, &h).await?;
    let row = queries::get_room(&st.pool, id).await?;
    desync_blob(&st, row.project_id, id, tick, actor).await
}

async fn mm_stats_json(st: &Shared, project: ProjectId) -> Result<Value> {
    let now = crate::matchmaking::now_ms();
    let searching: Vec<Value> = st
        .matchmaker
        .list_searching(&st.redis, project)
        .await?
        .iter()
        .map(|t| json!({"id": t.id, "player_id": t.req.player_id, "mode": t.req.mode, "region": t.req.region, "max_players": t.req.max_players, "age_secs": t.age_secs(now),
            "attrs": t.req.attrs, "filter": t.req.filter, "fill_existing": t.req.fill_existing, "match_type": t.req.match_type}))
        .collect();
    let (last_hour, last_24h) = st.matchmaker.matched_stats(&st.redis, project).await?;
    Ok(json!({"searching": searching, "matched": {"last_hour": last_hour, "last_24h": last_24h}}))
}

/// Live matchmaking queue of the caller's project.
async fn mm_stats(State(st): State<Shared>, h: HeaderMap) -> ApiResult<Json<Value>> {
    let key = api_key(&st, &h, Scope::Read).await?;
    Ok(Json(
        mm_stats_json(&st, ProjectId::from_uuid(key.project_id)).await?,
    ))
}

async fn op_mm_stats(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    operator(&st, &h).await?;
    Ok(Json(mm_stats_json(&st, ProjectId::from_uuid(id)).await?))
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
    /// `json` (default) or `csv` (hour,metric,quantity; attachment for billing exports).
    format: Option<String>,
}

fn usage_out(
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    rows: Vec<net_db::queries::UsageRow>,
    q: &UsageQuery,
    name: &str,
) -> Result<axum::response::Response> {
    use axum::response::IntoResponse;
    match q.format.as_deref() {
        None | Some("json") => {
            Ok(Json(json!({"from": from, "to": to, "usage": rows})).into_response())
        }
        Some("csv") => {
            let mut csv = String::with_capacity(rows.len() * 48 + 32);
            csv.push_str("hour,metric,quantity\n");
            for r in &rows {
                csv.push_str(&format!(
                    "{},{},{}\n",
                    r.hour.to_rfc3339(),
                    r.metric,
                    r.quantity
                ));
            }
            let file = format!(
                "aurix-net-usage-{name}-{}-{}.csv",
                from.format("%Y%m%d"),
                to.format("%Y%m%d")
            );
            Ok((
                [
                    (
                        axum::http::header::CONTENT_TYPE,
                        "text/csv; charset=utf-8".to_string(),
                    ),
                    (
                        axum::http::header::CONTENT_DISPOSITION,
                        format!("attachment; filename=\"{file}\""),
                    ),
                ],
                csv,
            )
                .into_response())
        }
        Some(other) => Err(NetError::Invalid(format!("unknown format {other:?}"))),
    }
}

async fn usage(
    State(st): State<Shared>,
    h: HeaderMap,
    Query(q): Query<UsageQuery>,
) -> ApiResult<axum::response::Response> {
    let key = api_key(&st, &h, Scope::Read).await?;
    let to = q.to.unwrap_or_else(Utc::now);
    let from = q.from.unwrap_or(to - chrono::Duration::days(30));
    let rows = queries::usage_between(&st.pool, key.project_id, from, to).await?;
    Ok(usage_out(from, to, rows, &q, &key.project_id.to_string())?)
}

#[derive(Deserialize)]
struct KeyReq {
    #[serde(default)]
    name: String,
    #[serde(default)]
    scopes: Vec<String>,
    /// Seconds until the key stops authenticating; omit for no expiry.
    #[serde(default)]
    expires_in_secs: Option<u64>,
    /// Source networks allowed to use the key (CIDR or bare IP); empty = anywhere.
    #[serde(default)]
    allowed_cidrs: Vec<String>,
}

/// Normalises `allowed_cidrs`: bare IPs become /32 or /128, at most 32 entries.
fn parse_cidrs(v: &[String]) -> Result<Vec<String>> {
    if v.len() > 32 {
        return Err(NetError::Invalid(
            "allowed_cidrs: at most 32 entries".into(),
        ));
    }
    v.iter()
        .map(|c| {
            let c = c.trim();
            if let Ok(ip) = c.parse::<IpAddr>() {
                let bits = if ip.is_ipv4() { 32 } else { 128 };
                return Ok(format!("{ip}/{bits}"));
            }
            net_common::ipnet::IpNet::parse(c)
                .map(|_| c.to_string())
                .ok_or_else(|| {
                    NetError::Invalid(format!("allowed_cidrs: {c:?} is not an IP or CIDR"))
                })
        })
        .collect()
}

fn cidr_allows(cidrs: &[String], ip: IpAddr) -> bool {
    cidrs
        .iter()
        .any(|c| net_common::ipnet::IpNet::parse(c).is_some_and(|n| n.contains(ip)))
}

fn key_expiry(secs: Option<u64>) -> Option<chrono::DateTime<chrono::Utc>> {
    secs.filter(|s| *s > 0)
        .map(|s| chrono::Utc::now() + chrono::Duration::seconds(s.min(10 * 365 * 86_400) as i64))
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

async fn issue_key(
    st: &Shared,
    project: Uuid,
    name: &str,
    scopes: &[String],
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
    allowed_cidrs: &[String],
) -> Result<Value> {
    let scopes = validate_scopes(scopes)?;
    let m = ApiKeyMaterial::generate();
    let row = queries::insert_api_key(
        &st.pool,
        project,
        &m.hash,
        &m.prefix,
        name,
        &scopes,
        expires_at,
        allowed_cidrs,
    )
    .await?;
    Ok(
        json!({"id": row.id, "key": m.plaintext, "prefix": row.prefix, "scopes": row.scopes, "name": row.name, "expires_at": row.expires_at, "allowed_cidrs": row.allowed_cidrs}),
    )
}

async fn create_key(
    State(st): State<Shared>,
    h: HeaderMap,
    Json(req): Json<KeyReq>,
) -> ApiResult<Json<Value>> {
    let key = api_key(&st, &h, Scope::Admin).await?;
    key_write(&st, &key)?;
    let cidrs = parse_cidrs(&req.allowed_cidrs)?;
    let out = issue_key(
        &st,
        key.project_id,
        &req.name,
        &req.scopes,
        key_expiry(req.expires_in_secs),
        &cidrs,
    )
    .await?;
    audit_log(
        &st,
        Some(key.project_id),
        &format!("key:{}", key.id),
        "key.create",
        out["id"].as_str().unwrap_or_default(),
        json!({"name": req.name, "scopes": req.scopes, "expires_at": out["expires_at"], "allowed_cidrs": cidrs}),
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
    crate::webhooks::validate_events(&req.events)?;
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
        json!({"webhooks": rows.into_iter().map(|r| json!({"id": r.id, "url": r.url, "events": r.events, "created_at": r.created_at, "previous_secret_expires_at": r.previous_secret_expires_at})).collect::<Vec<_>>()}),
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
    let op = operator(&st, &h).await?;
    let p = queries::create_project(&st.pool, &req.name, &req.plan).await?;
    let key = issue_key(
        &st,
        p.id,
        "default",
        &["rooms".into(), "read".into(), "admin".into()],
        None,
        &[],
    )
    .await?;
    queries::audit(
        &st.pool,
        Some(p.id),
        &op.actor,
        "project.create",
        &p.id.to_string(),
        json!({}),
    )
    .await?;
    Ok(Json(json!({"project": p, "key": key})))
}

async fn op_list_projects(State(st): State<Shared>, h: HeaderMap) -> ApiResult<Json<Value>> {
    operator(&st, &h).await?;
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
    let op = operator(&st, &h).await?;
    queries::get_project(&st.pool, id).await?;
    let cidrs = parse_cidrs(&req.allowed_cidrs)?;
    let out = issue_key(
        &st,
        id,
        &req.name,
        &req.scopes,
        key_expiry(req.expires_in_secs),
        &cidrs,
    )
    .await?;
    audit_log(
        &st,
        Some(id),
        &op.actor,
        "key.create",
        out["id"].as_str().unwrap_or_default(),
        json!({"name": req.name, "scopes": req.scopes, "expires_at": out["expires_at"], "allowed_cidrs": cidrs}),
    )
    .await;
    Ok(Json(out))
}

async fn op_revoke_key(
    State(st): State<Shared>,
    h: HeaderMap,
    Path((id, kid)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    let op = operator(&st, &h).await?;
    if !queries::revoke_api_key(&st.pool, id, kid).await? {
        return Err(NetError::NotFound("key".into()).into());
    }
    audit_log(
        &st,
        Some(id),
        &op.actor,
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
    let op = operator(&st, &h).await?;
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
        &op.actor,
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
    let op = operator(&st, &h).await?;
    let _ = net_control::ProjectLimits::from_json(&limits);
    queries::set_project_limits(&st.pool, id, &limits).await?;
    audit_log(
        &st,
        Some(id),
        &op.actor,
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

#[derive(Deserialize, Default)]
struct RotateReq {
    /// How long the previous secret keeps signing (second header). Default 24 h, max 7 days.
    #[serde(default)]
    grace_secs: Option<u64>,
}

/// New signing secret, shown once; the old one stays valid for `grace_secs`.
async fn rotate_webhook(
    st: &Shared,
    project: Uuid,
    wid: Uuid,
    req: &RotateReq,
    by: &str,
) -> Result<Value> {
    let grace = chrono::Duration::seconds(req.grace_secs.unwrap_or(86_400).min(7 * 86_400) as i64);
    let secret = ApiKeyMaterial::generate().plaintext;
    let row = queries::rotate_webhook_secret(&st.pool, project, wid, &secret, grace)
        .await?
        .ok_or_else(|| NetError::NotFound("webhook not found".into()))?;
    st.webhooks.invalidate(ProjectId::from_uuid(project));
    audit_log(
        st,
        Some(project),
        by,
        "webhook.rotate",
        &wid.to_string(),
        json!({"url": row.url, "previous_secret_expires_at": row.previous_secret_expires_at}),
    )
    .await;
    Ok(
        json!({"id": row.id, "secret": row.secret, "previous_secret_expires_at": row.previous_secret_expires_at}),
    )
}

async fn webhook_rotate(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    body: Option<Json<RotateReq>>,
) -> ApiResult<Json<Value>> {
    let key = api_key(&st, &h, Scope::Admin).await?;
    key_write(&st, &key)?;
    let req = body.map(|Json(r)| r).unwrap_or_default();
    Ok(Json(
        rotate_webhook(&st, key.project_id, id, &req, &format!("key:{}", key.id)).await?,
    ))
}

async fn op_webhook_rotate(
    State(st): State<Shared>,
    h: HeaderMap,
    Path((id, wid)): Path<(Uuid, Uuid)>,
    body: Option<Json<RotateReq>>,
) -> ApiResult<Json<Value>> {
    operator(&st, &h).await?;
    let req = body.map(|Json(r)| r).unwrap_or_default();
    Ok(Json(rotate_webhook(&st, id, wid, &req, "operator").await?))
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
    operator(&st, &h).await?;
    Ok(Json(deliveries_json(&st, id, wid, q.limit).await?))
}

async fn op_webhook_test(
    State(st): State<Shared>,
    h: HeaderMap,
    Path((id, wid)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    let op = operator(&st, &h).await?;
    let out = test_webhook(&st, id, wid).await?;
    audit_log(
        &st,
        Some(id),
        &op.actor,
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
            Ok(_) => return,
            Err(e) => {
                tracing::warn!(room = %id, node = %n.http_url, error = %e, "remote close failed; closing locally")
            }
        }
    }
    close_room(st, project, room).await;
}

/// Room name and `lobby_props` keys as accepted by both the control plane and REST.
pub(crate) fn validate_room_create(
    name: &str,
    lobby_props: &[String],
) -> std::result::Result<(), NetError> {
    let n = name.chars().count();
    if !(1..=64).contains(&n) || name.chars().any(char::is_control) {
        return Err(NetError::Invalid(
            "room name must be 1..=64 chars without control characters".into(),
        ));
    }
    if lobby_props.len() > 32 {
        return Err(NetError::Invalid("at most 32 lobby_props".into()));
    }
    if lobby_props
        .iter()
        .any(|k| k.is_empty() || k.chars().count() > 64 || k.chars().any(char::is_control))
    {
        return Err(NetError::Invalid(
            "lobby_props keys must be 1..=64 chars without control characters".into(),
        ));
    }
    Ok(())
}

pub(crate) fn valid_player_id(p: &str) -> bool {
    let n = p.chars().count();
    (1..=128).contains(&n) && !p.chars().any(char::is_control)
}

pub(crate) fn ban_message(b: &net_db::models::BanRow) -> String {
    match (&b.reason, b.expires_at) {
        (r, Some(e)) if !r.is_empty() => format!("banned until {}: {r}", e.to_rfc3339()),
        (_, Some(e)) => format!("banned until {}", e.to_rfc3339()),
        (r, None) if !r.is_empty() => format!("banned: {r}"),
        _ => "banned".into(),
    }
}

#[derive(Deserialize)]
struct BanReq {
    player_id: String,
    #[serde(default)]
    reason: String,
    /// Seconds from now; omit for a permanent ban.
    #[serde(default)]
    duration_secs: Option<u64>,
}

async fn add_ban(st: &Shared, project: Uuid, req: &BanReq, by: &str) -> Result<Value> {
    if !valid_player_id(&req.player_id) {
        return Err(NetError::Invalid("player_id: 1..=128 chars".into()));
    }
    if req.reason.chars().count() > 256 {
        return Err(NetError::Invalid("reason: at most 256 chars".into()));
    }
    let expires = req
        .duration_secs
        .filter(|d| *d > 0)
        .map(|d| chrono::Utc::now() + chrono::Duration::seconds(d.min(10 * 365 * 86_400) as i64));
    let row =
        queries::upsert_ban(&st.pool, project, &req.player_id, &req.reason, by, expires).await?;
    audit_log(
        st,
        Some(project),
        by,
        "player.ban",
        &req.player_id,
        json!({"reason": req.reason, "expires_at": row.expires_at}),
    )
    .await;
    Ok(json!({"ban": row}))
}

async fn remove_ban(st: &Shared, project: Uuid, player_id: &str, by: &str) -> Result<()> {
    if !queries::delete_ban(&st.pool, project, player_id).await? {
        return Err(NetError::NotFound("ban not found".into()));
    }
    audit_log(st, Some(project), by, "player.unban", player_id, json!({})).await;
    Ok(())
}

async fn list_bans(State(st): State<Shared>, h: HeaderMap) -> ApiResult<Json<Value>> {
    let key = api_key(&st, &h, Scope::Read).await?;
    Ok(Json(
        json!({"bans": queries::list_bans(&st.pool, key.project_id).await?}),
    ))
}

async fn create_ban(
    State(st): State<Shared>,
    h: HeaderMap,
    Json(req): Json<BanReq>,
) -> ApiResult<Json<Value>> {
    let key = api_key(&st, &h, Scope::Rooms).await?;
    key_write(&st, &key)?;
    Ok(Json(
        add_ban(&st, key.project_id, &req, &format!("key:{}", key.id)).await?,
    ))
}

async fn delete_ban(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(player_id): Path<String>,
) -> ApiResult<StatusCode> {
    let key = api_key(&st, &h, Scope::Rooms).await?;
    remove_ban(&st, key.project_id, &player_id, &format!("key:{}", key.id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn op_list_bans(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    operator(&st, &h).await?;
    Ok(Json(
        json!({"bans": queries::list_bans(&st.pool, id).await?}),
    ))
}

async fn op_create_ban(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(req): Json<BanReq>,
) -> ApiResult<Json<Value>> {
    operator(&st, &h).await?;
    queries::get_project(&st.pool, id).await?;
    Ok(Json(add_ban(&st, id, &req, "operator").await?))
}

async fn op_delete_ban(
    State(st): State<Shared>,
    h: HeaderMap,
    Path((id, player_id)): Path<(Uuid, String)>,
) -> ApiResult<StatusCode> {
    operator(&st, &h).await?;
    remove_ban(&st, id, &player_id, "operator").await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct KickQuery {
    #[serde(default)]
    reason: Option<String>,
}

/// Reason shown to the kicked player: short, printable ASCII only, so it can travel in a URL
/// between nodes and in a control frame unchanged.
fn kick_reason(q: &KickQuery) -> String {
    let r: String = q
        .reason
        .as_deref()
        .unwrap_or("")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || " _.,:-".contains(*c))
        .take(64)
        .collect();
    let r = r.trim().to_string();
    if r.is_empty() {
        "kicked".into()
    } else {
        r
    }
}

/// Kicks wherever the room is hosted: locally, or on the hosting node via the internal route.
async fn kick_anywhere(
    st: &Shared,
    project: ProjectId,
    id: Uuid,
    actor: u16,
    reason: &str,
) -> Result<String> {
    let room = RoomId::from_uuid(id);
    let actor_id = net_common::ActorId(actor);
    if st.host.get(room).is_some() {
        return crate::ws::kick_player(st, room, actor_id, reason).await;
    }
    let live = st
        .redis
        .get_room(project, room)
        .await?
        .ok_or_else(|| NetError::NotFound("room is not live".into()))?;
    let node = Uuid::parse_str(&live.node)
        .ok()
        .and_then(|n| st.registry.get(net_common::NodeId::from_uuid(n)))
        .ok_or_else(|| NetError::NotFound("hosting node unknown".into()))?;
    let path = format!(
        "/v1/internal/rooms/{id}/players/{actor}?reason={}",
        reason.replace(' ', "%20")
    );
    let v = internal_delete(st, &node.http_url, &path).await?;
    Ok(v["player_id"].as_str().unwrap_or_default().to_string())
}

async fn internal_kick(
    State(st): State<Shared>,
    h: HeaderMap,
    Path((id, actor)): Path<(Uuid, u16)>,
    Query(q): Query<KickQuery>,
) -> ApiResult<Json<Value>> {
    operator(&st, &h).await?;
    let player_id = crate::ws::kick_player(
        &st,
        RoomId::from_uuid(id),
        net_common::ActorId(actor),
        &kick_reason(&q),
    )
    .await?;
    Ok(Json(json!({"player_id": player_id})))
}

/// Project backend kicks a player (`rooms` scope); the room must belong to the key's project.
async fn kick_player(
    State(st): State<Shared>,
    h: HeaderMap,
    Path((id, actor)): Path<(Uuid, u16)>,
    Query(q): Query<KickQuery>,
) -> ApiResult<StatusCode> {
    let key = api_key(&st, &h, Scope::Rooms).await?;
    let row = queries::get_room(&st.pool, id).await?;
    if row.project_id != key.project_id {
        return Err(NetError::NotFound("room not found".into()).into());
    }
    let reason = kick_reason(&q);
    let player_id = kick_anywhere(
        &st,
        ProjectId::from_uuid(row.project_id),
        id,
        actor,
        &reason,
    )
    .await?;
    audit_log(
        &st,
        Some(row.project_id),
        &format!("key:{}", key.id),
        "player.kick",
        &player_id,
        json!({"room": id, "actor": actor, "reason": reason}),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

async fn op_kick_player(
    State(st): State<Shared>,
    h: HeaderMap,
    Path((id, actor)): Path<(Uuid, u16)>,
    Query(q): Query<KickQuery>,
) -> ApiResult<StatusCode> {
    let op = operator(&st, &h).await?;
    let row = queries::get_room(&st.pool, id).await?;
    let reason = kick_reason(&q);
    let player_id = kick_anywhere(
        &st,
        ProjectId::from_uuid(row.project_id),
        id,
        actor,
        &reason,
    )
    .await?;
    audit_log(
        &st,
        Some(row.project_id),
        &op.actor,
        "player.kick",
        &player_id,
        json!({"room": id, "actor": actor, "reason": reason}),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

async fn internal_close_room(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    operator(&st, &h).await?;
    let room = RoomId::from_uuid(id);
    let project = st
        .host
        .get(room)
        .map(|h| h.lock().project)
        .ok_or_else(|| NetError::NotFound("room not hosted here".into()))?;
    close_room(&st, project, room).await;
    Ok(StatusCode::NO_CONTENT)
}

/// DELETE on another node with the fleet token; returns the JSON body (Null for 204).
async fn internal_delete(st: &Shared, http_url: &str, path: &str) -> Result<Value> {
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
    if res.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(NetError::NotFound("not found on hosting node".into()));
    }
    if !res.status().is_success() {
        return Err(NetError::Internal(format!(
            "node answered {}",
            res.status()
        )));
    }
    if res.status() == reqwest::StatusCode::NO_CONTENT {
        return Ok(Value::Null);
    }
    Ok(res.json::<Value>().await.unwrap_or(Value::Null))
}

/// Delete a project: every live room is closed fleet-wide, all keys revoked, webhooks and
/// modules removed. The project row stays (soft delete) so usage and audit history survive.
async fn op_delete_project(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let op = operator(&st, &h).await?;
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
        &op.actor,
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
    operator(&st, &h).await?;
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
    let op = operator(&st, &h).await?;
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
        &op.actor,
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

/// Everything an operator should look at right now: nodes without a heartbeat or draining,
/// suspended projects, webhooks failing in the last hour, projects with rejections this hour.
async fn op_attention(State(st): State<Shared>, h: HeaderMap) -> ApiResult<Json<Value>> {
    operator(&st, &h).await?;
    Ok(Json(attention_value(&st).await?))
}

/// Everything that deserves an operator's eye right now; shared by `/v1/operator/attention`
/// and the alert loop.
pub(crate) async fn attention_value(st: &Shared) -> Result<Value> {
    let now = chrono::Utc::now();
    let nodes = queries::list_all_nodes(&st.pool).await?;
    let stale: Vec<Value> = nodes
        .iter()
        .filter(|n| (now - n.last_heartbeat).num_seconds() > 30)
        .map(|n| json!({"id": n.id, "public_host": n.public_host, "quic_port": n.quic_port, "region": n.region, "last_heartbeat": n.last_heartbeat}))
        .collect();
    let draining: Vec<Value> = nodes
        .iter()
        .filter(|n| n.draining && (now - n.last_heartbeat).num_seconds() <= 30)
        .map(|n| json!({"id": n.id, "public_host": n.public_host, "quic_port": n.quic_port, "region": n.region, "rooms": n.rooms}))
        .collect();
    let projects = queries::list_projects(&st.pool).await?;
    let suspended: Vec<Value> = projects
        .iter()
        .filter(|p| p.disabled_at.is_some())
        .map(|p| json!({"id": p.id, "name": p.name, "since": p.disabled_at}))
        .collect();
    let failing = queries::failing_webhooks_last_hour(&st.pool).await?;
    let names: std::collections::HashMap<Uuid, &str> =
        projects.iter().map(|p| (p.id, p.name.as_str())).collect();
    let failing_webhooks: Vec<Value> = failing
        .iter()
        .map(|f| json!({"project_id": f.project_id, "project_name": names.get(&f.project_id).copied().unwrap_or(""), "webhook_id": f.webhook_id, "url": f.url, "failed": f.failed, "total": f.total}))
        .collect();
    let mut rejections: Vec<Value> = Vec::new();
    for p in projects.iter().take(200) {
        let buckets = st
            .redis
            .get_rejections(ProjectId::from_uuid(p.id), 1)
            .await
            .unwrap_or_default();
        let mut total = 0i64;
        let mut by: std::collections::BTreeMap<String, i64> = Default::default();
        for (_, counts) in buckets {
            for (k, v) in counts {
                total += v;
                *by.entry(k).or_default() += v;
            }
        }
        if total > 0 {
            rejections.push(
                json!({"project_id": p.id, "project_name": p.name, "total": total, "by": by}),
            );
        }
    }
    rejections.sort_by_key(|r| -r["total"].as_i64().unwrap_or(0));
    rejections.truncate(20);
    let mut usage_caps: Vec<Value> = Vec::new();
    for p in projects.iter().filter(|p| p.deleted_at.is_none()) {
        let l = net_control::ProjectLimits::from_json(&p.limits);
        if l.monthly_player_minutes.is_none() && l.monthly_bytes.is_none() {
            continue;
        }
        let (minutes, bytes) = usage_month(st, p.id).await?;
        for (metric, used, cap) in [
            ("player_minutes", minutes, l.monthly_player_minutes),
            ("bytes", bytes, l.monthly_bytes),
        ] {
            if let Some(cap) = cap.filter(|c| *c > 0) {
                if used >= cap as f64 * 0.8 {
                    usage_caps.push(json!({"project_id": p.id, "project_name": p.name, "metric": metric, "used": used, "cap": cap}));
                }
            }
        }
    }
    Ok(json!({
        "stale_nodes": stale,
        "draining_nodes": draining,
        "suspended_projects": suspended,
        "failing_webhooks": failing_webhooks,
        "rejections": rejections,
        "usage_caps": usage_caps,
    }))
}

async fn op_nodes(State(st): State<Shared>, h: HeaderMap) -> ApiResult<Json<Value>> {
    operator(&st, &h).await?;
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
        "created_at": r.created_at, "last_used_at": r.last_used_at, "revoked_at": r.revoked_at,
        "expires_at": r.expires_at, "allowed_cidrs": r.allowed_cidrs
    })
}

/// Who the bearer is: the operator, or an API key with its project. The console signs in with it.
async fn me(State(st): State<Shared>, h: HeaderMap) -> ApiResult<Json<Value>> {
    if let Ok(op) = operator(&st, &h).await {
        return Ok(Json(
            json!({"kind": "operator", "actor": op.actor, "region": st.region, "node_id": st.node_id}),
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
    /// `json` (default) or `csv` (attachment: id,created_at,project_id,actor,action,target,details).
    #[serde(default)]
    format: Option<String>,
}

fn csv_cell(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn audit_out(
    rows: Vec<net_db::models::AuditRow>,
    q: &AuditQuery,
) -> Result<axum::response::Response> {
    use axum::response::IntoResponse;
    match q.format.as_deref() {
        None | Some("json") => Ok(Json(json!({"audit": rows})).into_response()),
        Some("csv") => {
            let mut csv = String::from("id,created_at,project_id,actor,action,target,details\n");
            for r in &rows {
                csv.push_str(&format!(
                    "{},{},{},{},{},{},{}\n",
                    r.id,
                    r.created_at.to_rfc3339(),
                    r.project_id.map(|p| p.to_string()).unwrap_or_default(),
                    csv_cell(&r.actor),
                    csv_cell(&r.action),
                    csv_cell(&r.target),
                    csv_cell(&r.details.to_string()),
                ));
            }
            let file = format!(
                "aurix-net-audit-{}.csv",
                chrono::Utc::now().format("%Y%m%dT%H%M%SZ")
            );
            Ok((
                [
                    (
                        axum::http::header::CONTENT_TYPE,
                        "text/csv; charset=utf-8".to_string(),
                    ),
                    (
                        axum::http::header::CONTENT_DISPOSITION,
                        format!("attachment; filename=\"{file}\""),
                    ),
                ],
                csv,
            )
                .into_response())
        }
        Some(other) => Err(NetError::Invalid(format!("unknown format {other:?}"))),
    }
}

async fn list_audit(
    State(st): State<Shared>,
    h: HeaderMap,
    Query(q): Query<AuditQuery>,
) -> ApiResult<axum::response::Response> {
    let key = api_key(&st, &h, Scope::Read).await?;
    let rows = queries::list_audit(
        &st.pool,
        Some(key.project_id),
        q.action.as_deref(),
        q.actor.as_deref(),
        q.limit.unwrap_or(100),
    )
    .await?;
    Ok(audit_out(rows, &q)?)
}

async fn op_audit(
    State(st): State<Shared>,
    h: HeaderMap,
    Query(q): Query<AuditQuery>,
) -> ApiResult<axum::response::Response> {
    operator(&st, &h).await?;
    let rows = queries::list_audit(
        &st.pool,
        q.project,
        q.action.as_deref(),
        q.actor.as_deref(),
        q.limit.unwrap_or(100),
    )
    .await?;
    Ok(audit_out(rows, &q)?)
}

/// Operator removes a project's webhook (e.g. a leaked secret or an abusive endpoint); audited.
async fn op_delete_webhook(
    State(st): State<Shared>,
    h: HeaderMap,
    Path((id, wid)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    let op = operator(&st, &h).await?;
    if queries::delete_webhook(&st.pool, id, wid).await? {
        audit_log(
            &st,
            Some(id),
            &op.actor,
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
    let op = operator(&st, &h).await?;
    if queries::delete_game_module(&st.pool, id, &name).await? {
        audit_log(&st, Some(id), &op.actor, "module.delete", &name, json!({})).await;
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
    operator(&st, &h).await?;
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
    let (wh_failed, wh_total) = queries::webhook_delivery_stats_last_hour(&st.pool, id).await?;
    let (mtd_minutes, mtd_bytes) = usage_month(&st, id).await?;
    Ok(Json(json!({
        "project": p,
        "effective_limits": net_control::ProjectLimits::from_json(&p.limits).effective(&st.cfg.limits),
        "keys": keys,
        "webhooks": webhooks,
        "modules": modules,
        "rooms": rooms,
        "webhooks_1h": {"failed": wh_failed, "total": wh_total},
        "usage_month": {"player_minutes": mtd_minutes, "bytes": mtd_bytes, "since": month_start()},
    })))
}

async fn op_fleet_usage(
    State(st): State<Shared>,
    h: HeaderMap,
    Query(q): Query<UsageQuery>,
) -> ApiResult<axum::response::Response> {
    operator(&st, &h).await?;
    let to = q.to.unwrap_or_else(Utc::now);
    let from = q.from.unwrap_or(to - chrono::Duration::days(30));
    let rows = queries::usage_fleet_between(&st.pool, from, to).await?;
    Ok(usage_out(from, to, rows, &q, "fleet")?)
}

async fn op_replay(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<ReplayQuery>,
) -> ApiResult<Json<Value>> {
    operator(&st, &h).await?;
    let row = queries::get_room(&st.pool, id).await?;
    Ok(Json(
        replay_anywhere(&st, ProjectId::from_uuid(row.project_id), id, &q).await?,
    ))
}

async fn op_usage(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<UsageQuery>,
) -> ApiResult<axum::response::Response> {
    operator(&st, &h).await?;
    let to = q.to.unwrap_or_else(Utc::now);
    let from = q.from.unwrap_or(to - chrono::Duration::days(30));
    let rows = queries::usage_between(&st.pool, id, from, to).await?;
    Ok(usage_out(from, to, rows, &q, &id.to_string())?)
}

async fn op_project_rooms(
    State(st): State<Shared>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    operator(&st, &h).await?;
    let rooms = st
        .redis
        .list_rooms(ProjectId::from_uuid(id))
        .await
        .unwrap_or_default();
    Ok(Json(json!({"rooms": rooms})))
}

/// Every live room in the region, across projects (operator view).
async fn op_rooms(State(st): State<Shared>, h: HeaderMap) -> ApiResult<Json<Value>> {
    operator(&st, &h).await?;
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
