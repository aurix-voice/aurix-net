use clap::Parser;
use net_api::rest;
use net_api::state::AppState;
use net_auth::GrantSigner;
use net_common::{NetConfig, NodeId, RegionId};
use net_control::{NodeRegistry, RedisStore};
use net_db::queries::{self, NodeHeartbeat};
use net_rooms::protocol::ServerMsg;
use net_transport::quic;
use net_transport::TicketSigner;
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, warn};

#[derive(Parser)]
#[command(name = "aurix-net-server", version, about = "Aurix Net node")]
struct Cli {
    /// TOML config layered over built-in defaults; env NET__SECTION__KEY overrides both.
    #[arg(short, long)]
    config: Option<String>,
    #[arg(long)]
    migrate_only: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let cfg = NetConfig::load(cli.config.as_deref())?;
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,quinn=warn,sqlx=warn".into()),
        )
        .init();
    let _ = rustls::crypto::ring::default_provider().install_default();
    info!(version = env!("CARGO_PKG_VERSION"), region = %cfg.server.region, "starting aurix-net node");

    let pool = net_db::create_pool(&cfg.database).await?;
    if cli.migrate_only {
        info!("migrations applied");
        return Ok(());
    }
    let redis = RedisStore::connect(&cfg.redis.url).await?;

    let node_id = cfg
        .server
        .node_id
        .as_deref()
        .and_then(|s| s.parse::<NodeId>().ok())
        .unwrap_or_else(NodeId::new);
    let region = RegionId::new(cfg.server.region.clone());
    let regions = queries::list_regions(&pool, false).await?;
    if !regions.iter().any(|r| r.id == region.as_str()) {
        queries::upsert_region(
            &pool,
            &net_db::models::RegionRow {
                id: region.to_string(),
                display_name: region.to_string(),
                dns_label: region.to_string().replace('_', "-"),
                enabled: true,
            },
        )
        .await?;
    }

    let identity = quic::server_identity(
        cfg.transport.cert_pem.as_deref(),
        cfg.transport.key_pem.as_deref(),
        &[cfg.server.public_host.clone(), "localhost".into()],
    )?;
    let transport = quic::transport_config(
        Duration::from_millis(cfg.transport.idle_timeout_ms),
        Duration::from_millis(cfg.transport.keep_alive_ms),
    );
    let endpoint = quic::server_endpoint(cfg.transport.quic_addr, &identity, transport)?;
    let quic_port = endpoint.local_addr()?.port();
    info!(addr = %cfg.transport.quic_addr, fingerprint = %identity.fingerprint_sha256, "quic data plane listening");

    let registry = NodeRegistry::new(pool.clone(), cfg.server.heartbeat_secs);
    let http_url = format!(
        "http://{}:{}",
        cfg.server.public_host,
        cfg.server.http_addr.port()
    );
    let st = Arc::new(AppState {
        grants: GrantSigner::new(&cfg.auth.grant_secret, cfg.auth.grant_ttl_secs),
        tickets: TicketSigner::new(cfg.auth.grant_secret.as_bytes()),
        host: Default::default(),
        node_id,
        region: region.clone(),
        cert_sha256: identity.fingerprint_sha256.clone(),
        quic_port,
        metrics: net_api::metrics::Metrics::new(),
        started: std::time::Instant::now(),
        pool: pool.clone(),
        redis: redis.clone(),
        registry: registry.clone(),
        cfg: cfg.clone(),
    });

    // First heartbeat before serving so the node is discoverable immediately.
    heartbeat(&st, &http_url, false).await?;
    registry.refresh().await?;
    registry.spawn_refresh(Duration::from_secs(cfg.server.heartbeat_secs));

    tokio::spawn(net_api::data::run_quic(st.clone(), endpoint));
    tokio::spawn(net_api::ws::reaper(st.clone()));
    tokio::spawn(lobby_forwarder(st.clone(), cfg.redis.url.clone()));
    {
        let st = st.clone();
        let http_url = http_url.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_secs(st.cfg.server.heartbeat_secs));
            iv.tick().await;
            loop {
                iv.tick().await;
                if let Err(e) = heartbeat(&st, &http_url, false).await {
                    warn!(error = %e, "heartbeat failed");
                }
            }
        });
    }

    let app = axum::Router::new()
        .route("/ws", axum::routing::get(net_api::ws::ws_handler))
        .with_state(st.clone())
        .merge(rest::router(st.clone()))
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .layer(tower_http::cors::CorsLayer::permissive());
    let listener = tokio::net::TcpListener::bind(cfg.server.http_addr).await?;
    info!(addr = %cfg.server.http_addr, node = %node_id, "control plane listening");
    let shutdown = {
        let st = st.clone();
        let http_url = http_url.clone();
        async move {
            let _ = tokio::signal::ctrl_c().await;
            info!("shutdown: draining");
            if let Err(e) = heartbeat(&st, &http_url, true).await {
                warn!(error = %e, "drain heartbeat failed");
            }
            for h in st.host.all() {
                let (project, id) = {
                    let g = h.lock();
                    (g.project, g.room.id())
                };
                net_api::ws::close_room(&st, project, id).await;
            }
            if let Err(e) = queries::delete_node(&st.pool, st.node_id.0).await {
                error!(error = %e, "delete node row");
            }
        }
    };
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await?;
    Ok(())
}

async fn heartbeat(st: &Arc<AppState>, http_url: &str, draining: bool) -> anyhow::Result<()> {
    let rooms = st.host.len() as i32;
    let players = st.host.players() as i32;
    let cap = st.cfg.limits.max_rooms_per_node.max(1) as f32;
    let load = (rooms as f32 / cap)
        .max(players as f32 / (cap * 8.0))
        .min(1.0);
    queries::upsert_node(
        &st.pool,
        &NodeHeartbeat {
            id: st.node_id.0,
            region: st.region.to_string(),
            public_host: st.cfg.server.public_host.clone(),
            quic_port: st.quic_port as i32,
            http_url: http_url.to_string(),
            cert_sha256: st.cert_sha256.clone(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            rooms,
            players,
            load_factor: load,
            draining,
        },
    )
    .await?;
    Ok(())
}

/// Forwards Redis lobby publications (from every node) to local lobby subscribers.
async fn lobby_forwarder(st: Arc<AppState>, url: String) {
    loop {
        if let Err(e) = lobby_forward_once(&st, &url).await {
            warn!(error = %e, "lobby forwarder stopped; retrying");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

async fn lobby_forward_once(st: &Arc<AppState>, url: &str) -> anyhow::Result<()> {
    use futures::StreamExt;
    let client = redis::Client::open(url)?;
    let mut ps = client.get_async_pubsub().await?;
    ps.psubscribe("net:p:*:lobby").await?;
    let mut stream = ps.on_message();
    while let Some(msg) = stream.next().await {
        let channel: String = msg.get_channel_name().to_string();
        let Some(pid) = channel
            .strip_prefix("net:p:")
            .and_then(|s| s.strip_suffix(":lobby"))
            .and_then(|s| s.parse::<net_common::ProjectId>().ok())
        else {
            continue;
        };
        let payload: String = match msg.get_payload() {
            Ok(p) => p,
            Err(_) => continue,
        };
        let v: serde_json::Value = match serde_json::from_str(&payload) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if v.get("removed").and_then(|r| r.as_bool()) == Some(true) {
            let id = v
                .get("room")
                .and_then(|r| r.as_str())
                .unwrap_or("")
                .to_string();
            st.host.notify_lobby(
                pid,
                ServerMsg::LobbyUpdate {
                    room: net_rooms::protocol::RoomSummary {
                        id,
                        name: String::new(),
                        mode: net_common::RoomMode::Relay,
                        players: 0,
                        max_players: 0,
                        open: false,
                        props: Default::default(),
                    },
                    removed: true,
                },
            );
        } else if let Ok(live) = serde_json::from_value::<net_control::redis_store::LiveRoom>(v) {
            let listed = live.visible && live.open && live.players < live.max_players;
            st.host.notify_lobby(
                pid,
                ServerMsg::LobbyUpdate {
                    room: net_api::ws::live_to_summary(live),
                    removed: !listed,
                },
            );
        }
    }
    Ok(())
}
