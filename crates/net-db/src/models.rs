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
}

#[derive(Debug, Clone, FromRow)]
pub struct ApiKeyAuth {
    #[sqlx(flatten)]
    pub key: ApiKeyRow,
    pub project_disabled_at: Option<DateTime<Utc>>,
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
