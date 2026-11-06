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
    pub max_event_bytes: usize,
    pub max_props_bytes: usize,
    pub max_cached_events_per_room: usize,
}

pub const DEFAULTS: &str = r#"
[server]
environment = "development"
region = "local"
http_addr = "127.0.0.1:8090"
public_host = "127.0.0.1"
heartbeat_secs = 30

[transport]
quic_addr = "0.0.0.0:7000"
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
max_event_bytes = 65536
max_props_bytes = 16384
max_cached_events_per_room = 1000
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
        if self.server.environment != "development" {
            if self.auth.grant_secret.len() < 32 || self.auth.grant_secret.starts_with("dev-only") {
                return Err(NetError::Config(
                    "auth.grant_secret must be a random string of 32+ bytes outside development"
                        .into(),
                ));
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
