//! Load test (relay or sync mode): N rooms x M players, each player sends `rate` events/s on the data
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
    /// Room mode: relay (events) or sync (inputs in, state deltas out).
    #[arg(long, default_value = "relay")]
    mode: String,
    /// Run id used in room names (default: seconds since epoch), so runs never share rooms.
    #[arg(long, default_value_t = epoch_us() / 1_000_000 % 1_000_000)]
    run: u64,
    /// Sync mode: simulation rate requested for the room.
    #[arg(long, default_value_t = 60)]
    tick_hz: u16,
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
    if cli.mode == "sync" {
        if let Ok(txt) = reqwest::get(format!("{}/metrics", cli.http)).await {
            if let Ok(body) = txt.text().await {
                println!("{}", summarize_hist(&body, "net_sync_tick_seconds"));

                println!(
                    "sync: inputs sent={} deltas received={} (~{:.1} per player per second)",
                    stats.sent.load(Ordering::Relaxed),
                    stats.received.load(Ordering::Relaxed),
                    stats.received.load(Ordering::Relaxed) as f64
                        / (cli.rooms * cli.players).max(1) as f64
                        / (cli.duration_secs.saturating_sub(5)).max(1) as f64
                );
            }
        }
    }
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
    let room_name = format!("lt-{}-{room}", cli.run);
    tx.send(send(&ClientMsg::JoinOrCreate {
        name: room_name,
        mode: if cli.mode == "sync" {
            net_common::types::RoomMode::Sync
        } else {
            net_common::types::RoomMode::Relay
        },
        max_players: Some(cli.players as u16),
        props: if cli.mode == "sync" {
            let mut m = serde_json::Map::new();
            m.insert("tick_hz".into(), serde_json::json!(cli.tick_hz));
            m
        } else {
            Default::default()
        },
        lobby_props: vec![],
        region: None,
        player_props: Default::default(),
    }))
    .await?;

    // Wait for Joined.
    let (data, tick_hz) = loop {
        let Some(m) = rx.next().await else {
            anyhow::bail!("ws closed before join")
        };
        if let Message::Text(t) = m? {
            match serde_json::from_str::<ServerMsg>(&t)? {
                ServerMsg::Joined { data, tick_hz, .. } => break (data, tick_hz),
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

    let sync = cli.mode == "sync";
    let server_tick = Arc::new(std::sync::atomic::AtomicU32::new(u32::MAX));
    // Incoming datagrams -> link -> latency samples.
    let on_datagram = {
        let server_tick = server_tick.clone();
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
                if sync {
                    if m.payload[0] == net_api::sync::KIND_DELTA && m.payload.len() >= 5 {
                        stats.received.fetch_add(1, Ordering::Relaxed);
                        let t = u32::from_be_bytes(m.payload[1..5].try_into().unwrap());
                        server_tick.store(t, Ordering::Relaxed);
                    }
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
    let period = if sync {
        Duration::from_secs_f64(1.0 / tick_hz.unwrap_or(cli.tick_hz).max(1) as f64)
    } else {
        Duration::from_secs_f64(1.0 / cli.rate.max(0.01))
    };
    let mut iv = tokio::time::interval(period);
    let stop = deadline - Duration::from_secs(5);
    let mut payload = vec![0u8; cli.payload.max(8)];
    let mut tick: u32 = 0;
    let mut recent: std::collections::VecDeque<Vec<u8>> = std::collections::VecDeque::new();
    let mut dir: (i8, i8) = (127, 0);
    while Instant::now() < stop {
        iv.tick().await;
        if sync {
            // Follow the server clock: inputs are for a couple of ticks ahead of the last delta.
            let st = server_tick.load(Ordering::Relaxed);
            if st != u32::MAX && (tick < st || tick > st + 8) {
                tick = st + 2;
                recent.clear();
            }
            if tick.is_multiple_of(90) {
                dir = match (tick / 90) % 4 {
                    0 => (127, 0),
                    1 => (0, 127),
                    2 => (-127, 0),
                    _ => (0, -127),
                };
            }
            recent.push_back(vec![dir.0 as u8, dir.1 as u8, 0]);
            while recent.len() > net_api::sync::INPUT_REDUNDANCY {
                recent.pop_front();
            }
            let first = tick + 1 - recent.len() as u32;
            let inputs: Vec<Vec<u8>> = recent.iter().cloned().collect();
            let frame =
                net_api::sync::SyncRoom::encode_input(net_common::types::Tick(first), &inputs);
            {
                let mut l = link.lock();
                if l.send(Channel::UnreliableSequenced, frame, Instant::now())
                    .is_ok()
                {
                    stats.sent.fetch_add(1, Ordering::Relaxed);
                }
                l.flush_batch();
            }
            flush();
            tick += 1;
            continue;
        }
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

/// p50/p99 upper-bucket estimate from a Prometheus histogram in `/metrics` text.
fn summarize_hist(body: &str, name: &str) -> String {
    let mut buckets: Vec<(f64, u64)> = Vec::new();
    let mut count = 0u64;
    let mut sum = 0f64;
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix(&format!("{name}_bucket{{le=\"")) {
            if let Some((le, n)) = rest.split_once("\"} ") {
                if let (Ok(le), Ok(n)) = (le.parse::<f64>(), n.trim().parse::<u64>()) {
                    buckets.push((le, n));
                }
            }
        } else if let Some(n) = line.strip_prefix(&format!("{name}_count ")) {
            count = n.trim().parse().unwrap_or(0);
        } else if let Some(n) = line.strip_prefix(&format!("{name}_sum ")) {
            sum = n.trim().parse().unwrap_or(0.0);
        }
    }
    if count == 0 {
        return format!("{name}: no samples");
    }
    buckets.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    let q = |p: f64| {
        let target = (count as f64 * p).ceil() as u64;
        buckets
            .iter()
            .find(|(_, n)| *n >= target)
            .map(|(le, _)| format!("<={:.1}ms", le * 1000.0))
            .unwrap_or_else(|| ">33ms".into())
    };
    format!(
        "server tick: n={count} mean={:.2}ms p50{} p99{}",
        sum / count as f64 * 1000.0,
        q(0.5),
        q(0.99)
    )
}
