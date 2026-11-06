use thiserror::Error;

/// Error codes are stable strings; clients and docs refer to them.
#[derive(Debug, Error)]
pub enum NetError {
    #[error("unauthorized: {0}")]
    Unauthorized(String),
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("invalid request: {0}")]
    Invalid(String),
    #[error("room is full ({max} players)")]
    RoomFull { max: u16 },
    #[error("room is closed")]
    RoomClosed,
    #[error("schema mismatch: client {client} server {server}")]
    SchemaMismatch { client: String, server: String },
    #[error("quota exceeded: {0}")]
    QuotaExceeded(String),
    #[error("rate limited")]
    RateLimited,
    #[error("transport: {0}")]
    Transport(String),
    #[error("database: {0}")]
    Database(String),
    #[error("redis: {0}")]
    Redis(String),
    #[error("configuration: {0}")]
    Config(String),
    #[error("internal: {0}")]
    Internal(String),
}

impl NetError {
    /// Stable machine-readable code.
    pub fn code(&self) -> &'static str {
        match self {
            NetError::Unauthorized(_) => "unauthorized",
            NetError::Forbidden(_) => "forbidden",
            NetError::NotFound(_) => "not_found",
            NetError::Conflict(_) => "conflict",
            NetError::Invalid(_) => "invalid_request",
            NetError::RoomFull { .. } => "room_full",
            NetError::RoomClosed => "room_closed",
            NetError::SchemaMismatch { .. } => "schema_mismatch",
            NetError::QuotaExceeded(_) => "quota_exceeded",
            NetError::RateLimited => "rate_limited",
            NetError::Transport(_) => "transport",
            NetError::Database(_) => "database",
            NetError::Redis(_) => "redis",
            NetError::Config(_) => "config",
            NetError::Internal(_) => "internal",
        }
    }

    pub fn http_status(&self) -> u16 {
        match self {
            NetError::Unauthorized(_) => 401,
            NetError::Forbidden(_) | NetError::QuotaExceeded(_) => 403,
            NetError::NotFound(_) => 404,
            NetError::Conflict(_) | NetError::RoomFull { .. } | NetError::RoomClosed => 409,
            NetError::Invalid(_) | NetError::SchemaMismatch { .. } => 400,
            NetError::RateLimited => 429,
            _ => 500,
        }
    }
}

pub type Result<T> = std::result::Result<T, NetError>;
