use clap::Parser;
use net_api::rest;
use net_api::state::AppState;
use net_auth::GrantSigner;
use net_common::config::RedisConfig;
use net_common::{NetConfig, NodeId, RegionId};
use net_control::{NodeRegistry, RedisStore};
use net_db::queries::{self, NodeHeartbeat};
use net_rooms::protocol::ServerMsg;
use net_transport::quic;
use net_transport::TicketSigner;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

/// glibc malloc keeps per-thread arenas at their high-water mark (RSS kept climbing by tens of MB
/// per batch of referee desyncs with nothing leaked); mimalloc returns freed pages to the OS.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(Parser)]
#[command(name = "aurix-net-server", version, about = "Aurix Net node")]
struct Cli {
    /// TOML config layered over built-in defaults; env NET__SECTION__KEY overrides both.
    #[arg(short, long)]
    config: Option<String>,
    #[arg(long)]
    migrate_only: bool,
    /// Probe this node's own `/healthz` and exit 0 on 200: the container HEALTHCHECK for images
    /// without a shell or curl (distroless).
    #[arg(long)]
    healthcheck: bool,
}

fn healthcheck(cfg: &NetConfig) -> bool {
    use std::io::{Read, Write};
    let mut addr = cfg.server.http_addr;
    if addr.ip().is_unspecified() {
        addr.set_ip(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
    }
    let timeout = std::time::Duration::from_secs(2);
    let Ok(mut stream) = std::net::TcpStream::connect_timeout(&addr, timeout) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));
    if stream
        .write_all(b"GET /healthz HTTP/1.0\r\nHost: localhost\r\n\r\n")
        .is_err()
    {
        return false;
    }
    let mut buf = [0u8; 64];
    let len = stream.read(&mut buf).unwrap_or(0);
    buf[..len].starts_with(b"HTTP/1.1 200") || buf[..len].starts_with(b"HTTP/1.0 200")
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let cfg = NetConfig::load(cli.config.as_deref())?;
    if cli.healthcheck {
        std::process::exit(if healthcheck(&cfg) { 0 } else { 1 });
    }
    let startup_warnings = cfg.production_warnings();
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info,quinn=warn,sqlx=warn".into());
    if cfg.server.log_format == "json" {
        tracing_subscriber::fmt()
            .json()
            .flatten_event(true)
            .with_env_filter(filter)
            .init();
    } else {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }
    let _ = rustls::crypto::ring::default_provider().install_default();
    for warning in &startup_warnings {
        warn!("production check: {warning}");
    }
    for key in NetConfig::unknown_env_keys() {
        warn!(var = %key, "unknown NET__ section in environment, ignored (sections: server, transport, database, redis, auth, limits, retention)");
    }
    info!(version = env!("CARGO_PKG_VERSION"), region = %cfg.server.region, "starting aurix-net node");

    let pool = net_db::create_pool(&cfg.database).await?;
    if cli.migrate_only {
        info!("migrations applied");
        return Ok(());
    }
    // The node renews its rooms' records with every heartbeat; one outlives the node by four.
    let room_ttl_secs = 4 * cfg.server.heartbeat_secs.max(1);
    let redis = RedisStore::connect(&cfg.redis.url, &cfg.redis.ca_cert, room_ttl_secs).await?;

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
    let wt_endpoint = match cfg.transport.webtransport_addr {
        Some(addr) => Some(net_transport::webtransport::server_endpoint(
            addr,
            &identity,
            std::time::Duration::from_millis(cfg.transport.idle_timeout_ms),
            std::time::Duration::from_millis(cfg.transport.keep_alive_ms),
        )?),
        None => None,
    };
    let webtransport_port = wt_endpoint
        .as_ref()
        .and_then(|e| e.local_addr().ok())
        .map(|a| a.port());
    info!(addr = %cfg.transport.quic_addr, fingerprint = %identity.fingerprint_sha256, "quic data plane listening");

    let listener = tokio::net::TcpListener::bind(cfg.server.http_addr).await?;
    let http_addr = listener.local_addr()?;
    let registry = NodeRegistry::new(pool.clone());
    let http_url = cfg
        .server
        .public_url
        .as_deref()
        .map(|u| u.trim_end_matches('/').to_string())
        .unwrap_or_else(|| format!("http://{}:{}", cfg.server.public_host, http_addr.port()));
    let st = Arc::new(AppState {
        grants: GrantSigner::with_previous(
            &cfg.auth.grant_secret,
            cfg.auth
                .grant_secret_previous
                .as_deref()
                .filter(|p| !p.is_empty()),
            cfg.auth.grant_ttl_secs,
        ),
        tickets: TicketSigner::new(cfg.auth.grant_secret.as_bytes()),
        host: Default::default(),
        node_id,
        region: region.clone(),
        cert_sha256: identity.fingerprint_sha256.clone(),
        quic_port,
        webtransport_port,
        metrics: Arc::new(net_api::metrics::Metrics::new()),
        webhooks: net_api::webhooks::Dispatcher::start(
            pool.clone(),
            cfg.server.environment == "development",
        ),
        matchmaker: Default::default(),
        games: Arc::new(net_game::GameEngine::new()),
        started: std::time::Instant::now(),
        guards: net_api::ratelimit::Guards::new(&cfg.limits, &cfg.server.trusted_proxies),
        draining: std::sync::atomic::AtomicBool::new(false),
        adopting: Default::default(),
        pool: pool.clone(),
        redis: redis.clone(),
        registry: registry.clone(),
        cfg: cfg.clone(),
    });

    // First heartbeat before serving so the node is discoverable immediately.
    heartbeat(&st, &http_url, st.is_draining()).await?;
    registry.refresh().await?;
    registry.spawn_refresh(Duration::from_secs(cfg.server.heartbeat_secs));

    tokio::spawn(net_api::data::run_quic(st.clone(), endpoint));
    if let Some(wt) = wt_endpoint {
        info!(addr = ?cfg.transport.webtransport_addr, "webtransport data plane listening");
        tokio::spawn(net_api::data::run_webtransport(st.clone(), wt));
    }
    tokio::spawn(net_api::ws::reaper(st.clone()));
    tokio::spawn(net_api::ws::flusher(st.clone()));
    tokio::spawn(net_api::metering::meter(st.clone()));
    tokio::spawn(net_api::janitor::run(st.clone()));
    let stop_matchmaking = CancellationToken::new();
    let matchmaking = tokio::spawn({
        let st = st.clone();
        let stop = stop_matchmaking.clone();
        async move {
            let mut iv = tokio::time::interval(std::time::Duration::from_millis(500));
            loop {
                tokio::select! {
                    () = stop.cancelled() => break,
                    _ = iv.tick() => {}
                }
                match st
                    .matchmaker
                    .run_cycle(&st.redis, &st.node_id.to_string())
                    .await
                {
                    Ok(found) => {
                        for started in found {
                            tracing::debug!(room = %started.room, players = started.players.len(), "match started");
                            st.webhooks.emit(
                                started.project,
                                "match.found",
                                serde_json::json!({
                                    "room": started.room,
                                    "mode": started.mode.as_str(),
                                    "players": started.players,
                                    "tickets": started.tickets,
                                    "bots": started.bots,
                                }),
                            );
                        }
                    }
                    Err(e) => tracing::warn!(error = %e, "matchmaking cycle failed"),
                }
            }
        }
    });
    tokio::spawn(fleet_listener(st.clone(), cfg.redis.clone()));
    tokio::spawn(net_api::alerts::run(st.clone()));
    {
        let st = st.clone();
        let http_url = http_url.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_secs(st.cfg.server.heartbeat_secs));
            iv.tick().await;
            loop {
                iv.tick().await;
                if let Err(e) = heartbeat(&st, &http_url, st.is_draining()).await {
                    warn!(error = %e, "heartbeat failed");
                }
            }
        });
    }

    tokio::spawn({
        let st = st.clone();
        async move {
            let mut iv = tokio::time::interval(Duration::from_secs(60));
            loop {
                iv.tick().await;
                st.guards.sweep();
            }
        }
    });
    let app = axum::Router::new()
        .route("/ws", axum::routing::get(net_api::ws::ws_handler))
        .with_state(st.clone())
        .merge(rest::router(st.clone()))
        .layer(axum::extract::DefaultBodyLimit::max(64 * 1024))
        .layer(axum::middleware::from_fn_with_state(
            st.clone(),
            net_api::ratelimit::http_guard,
        ))
        .layer(
            tower_http::trace::TraceLayer::new_for_http().make_span_with(
                |request: &axum::extract::Request| {
                    tracing::info_span!(
                        "request",
                        method = %request.method(),
                        path = request.uri().path(),
                        project = tracing::field::Empty,
                    )
                },
            ),
        )
        .layer(cors_layer(
            &cfg.server.allowed_origins,
            &cfg.server.environment,
        ));
    info!(addr = %http_addr, node = %node_id, "control plane listening");
    // What a stopping node is busy with, for the log when its time runs out.
    let (doing, unfinished) = tokio::sync::watch::channel("");
    let shutdown = {
        let st = st.clone();
        let http_url = http_url.clone();
        let doing = doing.clone();
        async move {
            wait_for_signal().await;
            info!("shutdown: draining");
            doing.send_replace("rooms still not handed off");
            st.draining
                .store(true, std::sync::atomic::Ordering::Relaxed);
            if let Err(e) = heartbeat(&st, &http_url, true).await {
                warn!(error = %e, "drain heartbeat failed");
            }
            // A cycle under way is finished, so no match is made without its webhook.
            stop_matchmaking.cancel();
            let _ = matchmaking.await;
            let moved = net_api::ws::handoff_all(&st).await;
            info!(rooms = moved, "shutdown: rooms handed off");
            if let Err(e) = queries::delete_node(&st.pool, st.node_id.0).await {
                error!(error = %e, "delete node row");
            }
            doing.send_replace("requests still under way");
        }
    };
    let stop = async {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .with_graceful_shutdown(shutdown)
        .await?;
        doing.send_replace("webhooks still undelivered");
        st.webhooks.flush().await;
        std::io::Result::Ok(())
    };
    let out_of_time = {
        let mut asked = unfinished.clone();
        let within = Duration::from_secs(cfg.server.shutdown_secs);
        async move {
            let _ = asked.wait_for(|doing| !doing.is_empty()).await;
            tokio::time::sleep(within).await;
        }
    };
    tokio::select! {
        stopped = stop => stopped?,
        () = out_of_time => warn!("shutdown: {}", *unfinished.borrow()),
    }
    Ok(())
}

/// CORS for browser clients: an explicit origin list, or any origin when none is configured
/// (logged as a warning outside development).
fn cors_layer(origins: &[String], environment: &str) -> tower_http::cors::CorsLayer {
    use tower_http::cors::{AllowOrigin, CorsLayer};
    if origins.is_empty() {
        let _ = environment;
        return CorsLayer::permissive();
    }
    let list: Vec<axum::http::HeaderValue> =
        origins.iter().filter_map(|o| o.parse().ok()).collect();
    CorsLayer::new()
        .allow_origin(AllowOrigin::list(list))
        .allow_methods(tower_http::cors::Any)
        .allow_headers(tower_http::cors::Any)
}

/// Ctrl-C or SIGTERM (what Docker, systemd and Kubernetes send).
async fn wait_for_signal() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

async fn heartbeat(st: &Arc<AppState>, http_url: &str, draining: bool) -> anyhow::Result<()> {
    net_api::ws::renew_rooms(st).await;
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
            heartbeat_secs: st.cfg.server.heartbeat_secs.clamp(1, i32::MAX as u64) as i32,
        },
    )
    .await?;
    net_api::ws::reconcile_rooms(st).await?;
    Ok(())
}

/// Applies what the nodes publish in Redis: lobby changes go to the local lobby subscribers,
/// and a change of a project's webhooks drops this node's copy of them.
async fn fleet_listener(st: Arc<AppState>, redis: RedisConfig) {
    loop {
        if let Err(e) = fleet_listen_once(&st, &redis).await {
            warn!(error = %e, "fleet listener stopped; retrying");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

async fn fleet_listen_once(st: &Arc<AppState>, redis: &RedisConfig) -> anyhow::Result<()> {
    use futures::StreamExt;
    let client = net_control::redis_store::redis_client(&redis.url, &redis.ca_cert)?;
    let mut ps = client.get_async_pubsub().await?;
    ps.psubscribe("net:p:*:lobby").await?;
    ps.psubscribe("net:p:*:webhooks").await?;
    net_api::ws::resync_lobby(st).await;
    let mut stream = ps.on_message();
    while let Some(msg) = stream.next().await {
        let Some((project, topic)) = msg
            .get_channel_name()
            .strip_prefix("net:p:")
            .and_then(|s| s.split_once(':'))
        else {
            continue;
        };
        let Ok(pid) = project.parse::<net_common::ProjectId>() else {
            continue;
        };
        if topic == "webhooks" {
            st.webhooks.invalidate(pid).await;
            continue;
        }
        let payload: String = match msg.get_payload() {
            Ok(text) => text,
            Err(_) => continue,
        };
        let change: serde_json::Value = match serde_json::from_str(&payload) {
            Ok(parsed) => parsed,
            Err(_) => continue,
        };
        if change.get("removed").and_then(|r| r.as_bool()) == Some(true) {
            let id = change
                .get("room")
                .and_then(|r| r.as_str())
                .unwrap_or("")
                .to_string();
            let gone = ServerMsg::LobbyUpdate {
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
            };
            st.host.notify_lobby(pid, |_| gone.clone());
        } else if let Ok(live) =
            serde_json::from_value::<net_control::redis_store::LiveRoom>(change)
        {
            st.host.notify_lobby(pid, |player| ServerMsg::LobbyUpdate {
                removed: !live.is_listed_for(player),
                room: net_api::ws::live_to_summary(live.clone()),
            });
        }
    }
    anyhow::bail!("the connection closed")
}
