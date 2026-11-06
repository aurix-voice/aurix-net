//! Relay-mode load test: N rooms x M players, each player sends `rate` events/s on the data
//! path; every other player in the room should receive them. Reports loss and latency.

use base64::Engine;
use bytes::Bytes;
use clap::Parser;
use futures::{SinkExt, StreamExt};
use net_common::Channel;
use net_rooms::event::CacheOp;
use net_rooms::protocol::{ClientMsg, ServerMsg};
use net_rooms::{EventFrame, EventTarget};
use net_transport::quic;
use net_transport::Link;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::Message;

#[derive(Parser, Clone)]
struct Cli {
    #[arg(long, default_value = "http://127.0.0.1:8090")]
    http: String,
    /// API key with `rooms` scope; omit to create a throwaway project with --operator-token.
    #[arg(long, env = "NET_API_KEY")]
    api_key: Option<String>,
    #[arg(long, env = "NET_OPERATOR_TOKEN")]
    operator_token: Option<String>,
    #[arg(long, default_value_t = 10)]
    rooms: u32,
    #[arg(long, default_value_t = 8)]
    players: u32,
    /// Events per second per player.
    #[arg(long, default_value_t = 20.0)]
    rate: f64,
    #[arg(long, default_value_t = 64)]
    payload: usize,
    #[arg(long, default_value_t = 20)]
    duration_secs: u64,
    /// Use the WebSocket tunnel instead of QUIC for the data path.
    #[arg(long)]
    tunnel: bool,
    #[arg(long, default_value_t = 50)]
    join_concurrency: usize,
}

#[derive(Default)]
struct Stats {
    sent: AtomicU64,
    received: AtomicU64,
    joined: AtomicU64,
    failed: AtomicU64,
    bytes_out: AtomicU64,
    bytes_in: AtomicU64,
    latencies_us: Mutex<Vec<u32>>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .init();
    let _ = rustls::crypto::ring::default_provider().install_default();
    let http = reqwest::Client::new();

    let api_key = match (&cli.api_key, &cli.operator_token) {
        (Some(k), _) => k.clone(),
        (None, Some(t)) => {
            let v: serde_json::Value = http
                .post(format!("{}/v1/operator/projects", cli.http))
                .bearer_auth(t)
                .json(&serde_json::json!({"name": "loadtest"}))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            v["key"]["key"].as_str().unwrap().to_string()
        }
        _ => anyhow::bail!("need --api-key or --operator-token"),
    };

    let stats = Arc::new(Stats::default());
    let ws_url = cli.http.replacen("http", "ws", 1) + "/ws";
    let start = Instant::now();
    let deadline = start + Duration::from_secs(cli.duration_secs) + Duration::from_secs(5);
    let sem = Arc::new(tokio::sync::Semaphore::new(cli.join_concurrency));
    let mut tasks = Vec::new();
    for r in 0..cli.rooms {
        for p in 0..cli.players {
            let cli = cli.clone();
            let http = http.clone();
            let api_key = api_key.clone();
            let ws_url = ws_url.clone();
            let stats = stats.clone();
            let sem = sem.clone();
            tasks.push(tokio::spawn(async move {
                let permit = sem.acquire().await.unwrap();
                let run = player(
                    &cli,
                    &http,
                    &api_key,
                    &ws_url,
                    r,
                    p,
                    stats.clone(),
                    deadline,
                    permit,
                )
                .await;
                if let Err(e) = run {
                    stats.failed.fetch_add(1, Ordering::Relaxed);
                    tracing::warn!(room = r, player = p, error = %e, "player failed");
                }
            }));
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }
    let reporter = {
        let stats = stats.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_secs(5));
            loop {
                iv.tick().await;
                eprintln!(
                    "[{:>4}s] joined={} failed={} sent={} recv={}",
                    start.elapsed().as_secs(),
                    stats.joined.load(Ordering::Relaxed),
                    stats.failed.load(Ordering::Relaxed),
                    stats.sent.load(Ordering::Relaxed),
                    stats.received.load(Ordering::Relaxed),
                );
            }
        })
    };
    for t in tasks {
        let _ = t.await;
    }
    reporter.abort();

    let sent = stats.sent.load(Ordering::Relaxed);
    let recv = stats.received.load(Ordering::Relaxed);
    let expected = sent * (cli.players as u64 - 1);
    let mut lat = stats.latencies_us.lock().clone();
    lat.sort_unstable();
    let pct = |p: f64| -> u32 {
        if lat.is_empty() {
            0
        } else {
            lat[((lat.len() - 1) as f64 * p) as usize]
        }
    };
    println!(
        "rooms={} players/room={} rate={}/s payload={}B path={}",
        cli.rooms,
        cli.players,
        cli.rate,
        cli.payload,
        if cli.tunnel { "ws-tunnel" } else { "quic" }
    );
    println!(
        "joined={} failed={}",
        stats.joined.load(Ordering::Relaxed),
        stats.failed.load(Ordering::Relaxed)
    );
    println!(
        "events sent={} delivered={} expected={} loss={:.3}%",
        sent,
        recv,
        expected,
        if expected > 0 {
            100.0 * (1.0 - recv as f64 / expected as f64)
        } else {
            0.0
        }
    );
    println!(
        "latency us: p50={} p95={} p99={} max={} (n={})",
        pct(0.5),
        pct(0.95),
        pct(0.99),
        lat.last().copied().unwrap_or(0),
        lat.len()
    );
    println!(
        "bytes out={} in={}",
        stats.bytes_out.load(Ordering::Relaxed),
        stats.bytes_in.load(Ordering::Relaxed)
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn player(
    cli: &Cli,
    http: &reqwest::Client,
    api_key: &str,
    ws_url: &str,
    room: u32,
    idx: u32,
    stats: Arc<Stats>,
    deadline: Instant,
    permit: tokio::sync::SemaphorePermit<'_>,
) -> anyhow::Result<()> {
    let grant: serde_json::Value = http
        .post(format!("{}/v1/grants", cli.http))
        .bearer_auth(api_key)
        .json(&serde_json::json!({"player_id": format!("lt-{room}-{idx}"), "name": format!("P{idx}")}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let grant = grant["grant"].as_str().unwrap().to_string();

    let (ws, _) = tokio_tungstenite::connect_async(ws_url).await?;
    let (mut tx, mut rx) = ws.split();
    let send = |m: &ClientMsg| Message::Text(serde_json::to_string(m).unwrap());
    tx.send(send(&ClientMsg::Auth { grant })).await?;
    let room_name = format!("lt-{room}");
    tx.send(send(&ClientMsg::JoinOrCreate {
        name: room_name,
        mode: Default::default(),
        max_players: Some(cli.players as u16),
        props: Default::default(),
        lobby_props: vec![],
        region: None,
        player_props: Default::default(),
    }))
    .await?;

    // Wait for Joined.
    let data = loop {
        let Some(m) = rx.next().await else {
            anyhow::bail!("ws closed before join")
        };
        if let Message::Text(t) = m? {
            match serde_json::from_str::<ServerMsg>(&t)? {
                ServerMsg::Joined { data, .. } => break data,
                ServerMsg::Error { code, message } => anyhow::bail!("{code}: {message}"),
                _ => {}
            }
        }
    };
    stats.joined.fetch_add(1, Ordering::Relaxed);
    drop(permit);

    let ticket = base64::engine::general_purpose::STANDARD.decode(&data.ticket)?;
    let link = Arc::new(Mutex::new(Link::new(Instant::now())));

    enum Sink {
        Quic(quinn::Connection),
        Ws(tokio::sync::mpsc::Sender<Message>),
    }
    let (ws_tx, mut ws_rx) = tokio::sync::mpsc::channel::<Message>(1024);
    let sink: Arc<Sink> = if cli.tunnel {
        tx.send(Message::Binary(ticket)).await?;
        Arc::new(Sink::Ws(ws_tx.clone()))
    } else {
        let transport = quic::transport_config(Duration::from_secs(15), Duration::from_secs(5));
        let ep = quic::client_endpoint("0.0.0.0:0".parse()?, &data.cert_sha256, transport)?;
        let addr = tokio::net::lookup_host((data.host.as_str(), data.port))
            .await?
            .next()
            .ok_or_else(|| anyhow::anyhow!("resolve {}", data.host))?;
        let conn = ep.connect(addr, &data.host)?.await?;
        conn.send_datagram(Bytes::from(ticket))?;
        Arc::new(Sink::Quic(conn))
    };

    let flush = {
        let link = link.clone();
        let sink = sink.clone();
        let stats = stats.clone();
        move || {
            let out: Vec<Bytes> = {
                let mut l = link.lock();
                std::iter::from_fn(|| l.next_outgoing()).collect()
            };
            for d in out {
                stats.bytes_out.fetch_add(d.len() as u64, Ordering::Relaxed);
                match &*sink {
                    Sink::Quic(c) => {
                        let _ = c.send_datagram(d);
                    }
                    Sink::Ws(t) => {
                        let _ = t.try_send(Message::Binary(d.to_vec()));
                    }
                }
            }
        }
    };

    // WS writer (tunnel frames + nothing else after join).
    let writer = tokio::spawn(async move {
        while let Some(m) = ws_rx.recv().await {
            if tx.send(m).await.is_err() {
                break;
            }
        }
    });

    // Incoming datagrams -> link -> latency samples.
    let on_datagram = {
        let link = link.clone();
        let stats = stats.clone();
        let flush = flush.clone();
        move |d: Bytes| {
            stats.bytes_in.fetch_add(d.len() as u64, Ordering::Relaxed);
            let recv = link.lock().receive(d, Instant::now());
            flush();
            for m in recv {
                if m.payload.is_empty() {
                    continue;
                }
                if let Ok(f) = EventFrame::decode(m.payload) {
                    if f.payload.len() >= 8 {
                        let ts = u64::from_le_bytes(f.payload[..8].try_into().unwrap());
                        let now = epoch_us();
                        if now >= ts {
                            stats
                                .latencies_us
                                .lock()
                                .push((now - ts).min(u32::MAX as u64) as u32);
                        }
                    }
                    stats.received.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    };
    let reader = match &*sink {
        Sink::Quic(conn) => {
            let conn = conn.clone();
            let on = on_datagram.clone();
            tokio::spawn(async move {
                while let Ok(d) = conn.read_datagram().await {
                    on(d);
                }
            })
        }
        Sink::Ws(_) => {
            let on = on_datagram.clone();
            tokio::spawn(async move {
                while let Some(Ok(m)) = rx.next().await {
                    if let Message::Binary(b) = m {
                        on(Bytes::from(b));
                    }
                }
            })
        }
    };
    let ticker = {
        let link = link.clone();
        let flush = flush.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_millis(10));
            loop {
                iv.tick().await;
                if link.lock().poll(Instant::now()).is_err() {
                    break;
                }
                flush();
            }
        })
    };

    // Send loop.
    let period = Duration::from_secs_f64(1.0 / cli.rate.max(0.01));
    let mut iv = tokio::time::interval(period);
    let stop = deadline - Duration::from_secs(5);
    let mut payload = vec![0u8; cli.payload.max(8)];
    while Instant::now() < stop {
        iv.tick().await;
        payload[..8].copy_from_slice(&epoch_us().to_le_bytes());
        let frame = EventFrame {
            code: 1,
            target: EventTarget::Others,
            cache: CacheOp::None,
            group: None,
            from: net_common::ActorId(0),
            payload: Bytes::from(payload.clone()),
        };
        {
            let mut l = link.lock();
            if l.send(Channel::UnreliableSequenced, frame.encode(), Instant::now())
                .is_ok()
            {
                stats.sent.fetch_add(1, Ordering::Relaxed);
            }
            l.flush_batch();
        }
        flush();
    }
    // Drain window for late deliveries.
    tokio::time::sleep(Duration::from_secs(2)).await;
    reader.abort();
    ticker.abort();
    let _ = ws_tx.send(Message::Close(None)).await;
    writer.abort();
    if let Sink::Quic(c) = &*sink {
        c.close(0u32.into(), b"done");
    }
    Ok(())
}

fn epoch_us() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0)
}
