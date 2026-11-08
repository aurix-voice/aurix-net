use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct ProjectRow {
    pub id: Uuid,
    pub name: String,
    pub plan: String,
    pub limits: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub disabled_at: Option<DateTime<Utc>>,
    pub deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow)]
pub struct ApiKeyRow {
    pub id: Uuid,
    pub project_id: Uuid,
    pub key_hash: String,
    pub prefix: String,
    pub name: String,
    pub scopes: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    /// Empty = any source address.
    pub allowed_cidrs: Vec<String>,
}

#[derive(Debug, Clone, FromRow)]
pub struct ApiKeyAuth {
    #[sqlx(flatten)]
    pub key: ApiKeyRow,
    pub project_disabled_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct OperatorTokenRow {
    pub id: Uuid,
    pub name: String,
    #[serde(skip_serializing)]
    pub token_hash: String,
    pub prefix: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub last_used_at: Option<DateTime<Utc>>,
    /// Empty = any source address.
    pub allowed_cidrs: Vec<String>,
    pub last_used_ip: Option<String>,
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct FailingWebhookRow {
    pub project_id: Uuid,
    pub webhook_id: Uuid,
    pub url: String,
    pub failed: i64,
    pub total: i64,
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct BanRow {
    pub project_id: Uuid,
    pub player_id: String,
    pub reason: String,
    pub created_by: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct WebhookDeliveryRow {
    pub id: Uuid,
    pub webhook_id: Uuid,
    pub project_id: Uuid,
    pub event: String,
    pub ok: bool,
    pub status: Option<i16>,
    pub attempts: i16,
    pub latency_ms: i32,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct RegionRow {
    pub id: String,
    pub display_name: String,
    pub dns_label: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct NodeRow {
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
    pub started_at: DateTime<Utc>,
    pub last_heartbeat: DateTime<Utc>,
    pub heartbeat_secs: i32,
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct RoomRow {
    pub id: Uuid,
    pub project_id: Uuid,
    pub node_id: Option<Uuid>,
    pub region: String,
    pub name: String,
    pub mode: String,
    pub state: String,
    pub max_players: i32,
    pub visible: bool,
    pub open: bool,
    pub schema_hash: Option<String>,
    pub props: serde_json::Value,
    pub lobby_props: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct WebhookRow {
    pub id: Uuid,
    pub project_id: Uuid,
    pub url: String,
    pub secret: String,
    pub events: Vec<String>,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub previous_secret: Option<String>,
    pub previous_secret_expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct GameModuleRow {
    pub project_id: Uuid,
    pub name: String,
    pub hash: String,
    #[serde(skip)]
    pub wasm: Vec<u8>,
    pub size: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct AuditRow {
    pub id: i64,
    pub project_id: Option<Uuid>,
    pub actor: String,
    pub action: String,
    pub target: String,
    pub details: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct OperatorWebhookRow {
    pub id: Uuid,
    pub url: String,
    #[serde(skip_serializing)]
    pub secret: String,
    pub events: Vec<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct OperatorWebhookDeliveryRow {
    pub id: Uuid,
    pub webhook_id: Uuid,
    pub event: String,
    pub ok: bool,
    pub status: Option<i16>,
    pub attempts: i16,
    pub latency_ms: i32,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
}
