use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

use crate::error::{NetError, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetConfig {
    pub server: ServerConfig,
    pub transport: TransportConfig,
    pub database: DatabaseConfig,
    pub redis: RedisConfig,
    pub auth: AuthConfig,
    pub limits: LimitsConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub environment: String,
    pub region: String,
    pub node_id: Option<String>,
    pub http_addr: SocketAddr,
    /// Hostname clients use to reach this node's data port.
    pub public_host: String,
    pub heartbeat_secs: u64,
    /// Reverse proxies whose `X-Forwarded-For` is trusted (IPs or CIDRs). Empty = use the
    /// TCP peer address for every per-IP limit.
    #[serde(default)]
    pub trusted_proxies: Vec<String>,
    /// Bearer token for `GET /metrics`; empty leaves it open (bind it to a private interface).
    #[serde(default)]
    pub metrics_token: String,
    /// Browser origins allowed on the HTTP API (`https://game.example.com`). Empty = any
    /// origin (fine for development; production should list the game's origins).
    #[serde(default)]
    pub allowed_origins: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransportConfig {
    pub quic_addr: SocketAddr,
    /// PEM certificate chain; when empty a self-signed certificate is generated at start.
    pub cert_pem: Option<String>,
    pub key_pem: Option<String>,
    pub idle_timeout_ms: u64,
    pub keep_alive_ms: u64,
    pub ws_tunnel: bool,
    /// WebTransport (HTTP/3) endpoint for browsers; None disables it.
    #[serde(default)]
    pub webtransport_addr: Option<SocketAddr>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseConfig {
    pub url: String,
    pub max_connections: u32,
    pub run_migrations: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedisConfig {
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    /// HS256 secret for join grants and data tickets. Required outside `development`.
    pub grant_secret: String,
    pub grant_ttl_secs: u64,
    /// Static admin token for operator endpoints; empty disables them.
    pub admin_token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LimitsConfig {
    pub max_players_per_room: u16,
    pub max_rooms_per_node: u32,
    pub max_events_per_sec_per_room: u32,
    pub max_events_per_sec_per_player: u32,
    pub max_event_bytes: usize,
    pub max_props_bytes: usize,
    pub max_player_props_bytes: usize,
    pub max_cached_events_per_room: usize,
    /// Concurrent control sessions and data connections per client IP.
    #[serde(default = "d_sessions_per_ip")]
    pub max_sessions_per_ip: u32,
    /// HTTP requests per second per IP (burst 2x).
    #[serde(default = "d_http_rps")]
    pub http_rps_per_ip: u32,
    /// New WebSocket connections per minute per IP.
    #[serde(default = "d_ws_connects")]
    pub ws_connects_per_min_per_ip: u32,
    /// Failed `auth` messages per minute per IP before the IP is refused.
    #[serde(default = "d_auth_failures")]
    pub auth_failures_per_min_per_ip: u32,
    /// Write calls (grants, rooms, tickets, keys, webhooks, modules) per second per API key.
    #[serde(default = "d_key_writes")]
    pub writes_per_sec_per_key: u32,
}

fn d_sessions_per_ip() -> u32 {
    64
}
fn d_http_rps() -> u32 {
    50
}
fn d_ws_connects() -> u32 {
    60
}
fn d_auth_failures() -> u32 {
    20
}
fn d_key_writes() -> u32 {
    20
}

pub const DEFAULTS: &str = r#"
[server]
environment = "development"
region = "local"
http_addr = "127.0.0.1:8090"
public_host = "127.0.0.1"
heartbeat_secs = 30
trusted_proxies = []
metrics_token = ""
allowed_origins = []

[transport]
quic_addr = "0.0.0.0:7000"
webtransport_addr = "0.0.0.0:7443"
idle_timeout_ms = 15000
keep_alive_ms = 5000
ws_tunnel = true

[database]
url = "postgres://aurix:aurix@127.0.0.1:5432/aurix_net"
max_connections = 16
run_migrations = true

[redis]
url = "redis://127.0.0.1:6379/"

[auth]
grant_secret = "dev-only-change-me-dev-only-change-me"
grant_ttl_secs = 3600
admin_token = ""

[limits]
max_players_per_room = 128
max_rooms_per_node = 2000
max_events_per_sec_per_room = 500
max_events_per_sec_per_player = 60
max_event_bytes = 65536
max_props_bytes = 16384
max_player_props_bytes = 4096
max_cached_events_per_room = 1000
max_sessions_per_ip = 64
http_rps_per_ip = 50
ws_connects_per_min_per_ip = 60
auth_failures_per_min_per_ip = 20
writes_per_sec_per_key = 20
"#;

impl NetConfig {
    /// Defaults, then optional TOML file, then `NET__SECTION__KEY` env vars.
    pub fn load(path: Option<&str>) -> Result<Self> {
        let mut b = config::Config::builder()
            .add_source(config::File::from_str(DEFAULTS, config::FileFormat::Toml));
        if let Some(p) = path {
            b = b.add_source(config::File::with_name(p));
        }
        let cfg: NetConfig = b
            .add_source(config::Environment::with_prefix("NET").separator("__"))
            .build()
            .map_err(|e| NetError::Config(e.to_string()))?
            .try_deserialize()
            .map_err(|e| NetError::Config(e.to_string()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<()> {
        if self.server.environment != "development"
            && (self.auth.grant_secret.len() < 32 || self.auth.grant_secret.starts_with("dev-only"))
        {
            return Err(NetError::Config(
                "auth.grant_secret must be a random string of 32+ bytes outside development".into(),
            ));
        }
        for p in &self.server.trusted_proxies {
            if crate::ipnet::IpNet::parse(p).is_none() {
                return Err(NetError::Config(format!(
                    "server.trusted_proxies: bad entry {p}"
                )));
            }
        }
        if self.limits.max_players_per_room == 0 || self.limits.max_players_per_room > 1024 {
            return Err(NetError::Config(
                "limits.max_players_per_room must be 1..=1024".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_load() {
        let c = NetConfig::load(None).unwrap();
        assert_eq!(c.server.environment, "development");
        assert_eq!(c.limits.max_players_per_room, 128);
    }
}
