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

/// Idle timeout of the data connection, and of the link on it.
const IDLE_TIMEOUT: Duration = Duration::from_secs(15);
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
    /// Use the WebTransport endpoint (what browsers use) instead of raw QUIC.
    #[arg(long)]
    webtransport: bool,
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
    /// Extra room props as JSON (e.g. '{"schema": "...", "logic": "arena"}').
    #[arg(long, default_value = "{}")]
    props_json: String,
    /// Lockstep: one extra player per room joins this many seconds in and must catch up
    /// (checkpoint + journal tail, then a gapless live stream). 0 = off.
    #[arg(long, default_value_t = 0)]
    late_join_secs: u64,
    /// Drop this fraction of incoming datagrams on the client (simulated downstream loss).
    #[arg(long, default_value_t = 0.0)]
    drop_in: f64,
    /// Sync with the `movers` example logic: ask each room for this many moving objects and
    /// report bandwidth per client over the run minus --warmup-secs. Needs its schema in
    /// --props-json. 0 = off.
    #[arg(long, default_value_t = 0)]
    objects: u16,
    #[arg(long, default_value_t = 10)]
    warmup_secs: u64,
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
    /// Lockstep: catch-up frames received / joiners that reached the live stream gaplessly /
    /// bundles replayed from the journal / checkpoints uploaded by masters.
    catchups: AtomicU64,
    caught_up: AtomicU64,
    catchup_bundles: AtomicU64,
    checkpoints: AtomicU64,
    catchup_from_tick: AtomicU64,
    /// Sync: datagrams dropped by --drop-in / delta gaps the client noticed / acks sent.
    dropped: AtomicU64,
    gaps: AtomicU64,
    acks: AtomicU64,
    /// Sync: entity records in received deltas / entities in the frame of the first player
    /// of room 0, which applies every delta (--objects).
    entities: AtomicU64,
    probe_entities: AtomicU64,
}

impl Stats {
    /// (bytes in, bytes out, deltas, entity records)
    fn traffic(&self) -> [u64; 4] {
        [
            self.bytes_in.load(Ordering::Relaxed),
            self.bytes_out.load(Ordering::Relaxed),
            self.received.load(Ordering::Relaxed),
            self.entities.load(Ordering::Relaxed),
        ]
    }
}

/// Entity records in a delta payload (after the kind byte): the count follows the despawn list.
fn delta_entities(payload: &[u8]) -> u64 {
    let Some(count) = payload.get(8..10) else {
        return 0;
    };
    let at = 10 + 6 * u16::from_be_bytes([count[0], count[1]]) as usize;
    payload
        .get(at..at + 2)
        .map_or(0, |n| u16::from_be_bytes([n[0], n[1]]) as u64)
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
            let created: serde_json::Value = http
                .post(format!("{}/v1/operator/projects", cli.http))
                .bearer_auth(t)
                .json(&serde_json::json!({"name": "loadtest"}))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            created["key"]["key"].as_str().unwrap().to_string()
        }
        _ => anyhow::bail!("need --api-key or --operator-token"),
    };

    let stats = Arc::new(Stats::default());
    let ws_url = cli.http.replacen("http", "ws", 1) + "/ws";
    let start = Instant::now();
    let deadline = start + Duration::from_secs(cli.duration_secs) + Duration::from_secs(5);
    let sem = Arc::new(tokio::sync::Semaphore::new(cli.join_concurrency));
    let mut tasks = Vec::new();
    for room_index in 0..cli.rooms {
        for player_index in 0..cli.players {
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
                    room_index,
                    player_index,
                    stats.clone(),
                    deadline,
                    permit,
                )
                .await;
                if let Err(e) = run {
                    stats.failed.fetch_add(1, Ordering::Relaxed);
                    tracing::warn!(room = room_index, player = player_index, error = %e, "player failed");
                }
            }));
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        if cli.late_join_secs > 0 && cli.mode == "lockstep" {
            let cli = cli.clone();
            let http = http.clone();
            let api_key = api_key.clone();
            let ws_url = ws_url.clone();
            let stats = stats.clone();
            let sem = sem.clone();
            tasks.push(tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(cli.late_join_secs)).await;
                let permit = sem.acquire().await.unwrap();
                let players = cli.players;
                let run = player(
                    &cli,
                    &http,
                    &api_key,
                    &ws_url,
                    room_index,
                    players,
                    stats.clone(),
                    deadline,
                    permit,
                )
                .await;
                if let Err(e) = run {
                    stats.failed.fetch_add(1, Ordering::Relaxed);
                    tracing::warn!(room = room_index, player = players, error = %e, "late player failed");
                }
            }));
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
    let window = {
        let stats = stats.clone();
        let warmup = cli.warmup_secs.min(cli.duration_secs / 2);
        let secs = cli.duration_secs - warmup;
        tokio::spawn(async move {
            tokio::time::sleep_until((start + Duration::from_secs(warmup)).into()).await;
            let before = stats.traffic();
            tokio::time::sleep(Duration::from_secs(secs)).await;
            let after = stats.traffic();
            (
                secs as f64,
                [
                    after[0] - before[0],
                    after[1] - before[1],
                    after[2] - before[2],
                    after[3] - before[3],
                ],
            )
        })
    };
    for task in tasks {
        let _ = task.await;
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
        if cli.tunnel {
            "ws-tunnel"
        } else if cli.webtransport {
            "webtransport"
        } else {
            "quic"
        }
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
    if cli.mode == "lockstep" && cli.late_join_secs > 0 {
        println!(
            "late join: catch-ups={} caught-up={}/{} journal bundles replayed={} (from tick {} avg) checkpoints uploaded={}",
            stats.catchups.load(Ordering::Relaxed),
            stats.caught_up.load(Ordering::Relaxed),
            cli.rooms,
            stats.catchup_bundles.load(Ordering::Relaxed),
            stats.catchup_from_tick.load(Ordering::Relaxed) / stats.catchups.load(Ordering::Relaxed).max(1),
            stats.checkpoints.load(Ordering::Relaxed),
        );
    }
    if cli.mode == "sync" || cli.mode == "lockstep" {
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
                if cli.mode == "sync" {
                    println!(
                        "sync acks: sent={} client gaps noticed={} datagrams dropped (--drop-in)={} {}",
                        stats.acks.load(Ordering::Relaxed),
                        stats.gaps.load(Ordering::Relaxed),
                        stats.dropped.load(Ordering::Relaxed),
                        summarize_counter(&body, "net_sync_lost_deltas_total")
                    );
                }
                if cli.objects > 0 {
                    let (secs, [bin, bout, deltas, entities]) = window.await?;
                    let per = secs * stats.joined.load(Ordering::Relaxed).max(1) as f64;
                    println!(
                        "objects: asked={} on client={} window={secs}s per client: down={:.2} kB/s up={:.2} kB/s deltas={:.1}/s objects per delta={:.1}",
                        cli.objects,
                        stats.probe_entities.load(Ordering::Relaxed),
                        bin as f64 / per / 1000.0,
                        bout as f64 / per / 1000.0,
                        deltas as f64 / per,
                        entities as f64 / deltas.max(1) as f64,
                    );
                }
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
    let send = |m: &ClientMsg| Message::Text(serde_json::to_string(m).unwrap().into());
    tx.send(send(&ClientMsg::Auth {
        grant,
        supports: Vec::new(),
    }))
    .await?;
    let room_name = format!("lt-{}-{room}", cli.run);
    tx.send(send(&ClientMsg::JoinOrCreate {
        name: room_name,
        mode: cli.mode.parse().unwrap_or_default(),
        max_players: Some(cli.players as u16 + u16::from(cli.late_join_secs > 0)),
        props: {
            let mut props: serde_json::Map<String, serde_json::Value> =
                serde_json::from_str(&cli.props_json).unwrap_or_default();
            if cli.mode == "sync" || cli.mode == "lockstep" {
                props.insert("tick_hz".into(), serde_json::json!(cli.tick_hz));
            }
            props
        },
        lobby_props: vec![],
        region: None,
        player_props: Default::default(),
        reserved: Vec::new(),
    }))
    .await?;

    let (data, tick_hz) = loop {
        let Some(message) = rx.next().await else {
            anyhow::bail!("ws closed before join")
        };
        if let Message::Text(text) = message? {
            match serde_json::from_str::<ServerMsg>(&text)? {
                ServerMsg::Joined { data, tick_hz, .. } => break (data, tick_hz),
                ServerMsg::Error { code, message, .. } => anyhow::bail!("{code}: {message}"),
                _ => {}
            }
        }
    };
    stats.joined.fetch_add(1, Ordering::Relaxed);
    drop(permit);

    let ticket = base64::engine::general_purpose::STANDARD.decode(&data.ticket)?;
    let link = Arc::new(Mutex::new(Link::new(Instant::now(), IDLE_TIMEOUT)));

    enum Sink {
        Quic(quinn::Connection),
        Wt(wtransport::Connection),
        Ws(tokio::sync::mpsc::Sender<Message>),
    }
    let (ws_tx, mut ws_rx) = tokio::sync::mpsc::channel::<Message>(1024);
    let sink: Arc<Sink> = if cli.tunnel {
        tx.send(Message::Binary(ticket.into())).await?;
        Arc::new(Sink::Ws(ws_tx.clone()))
    } else if cli.webtransport {
        let url = data
            .webtransport_url
            .clone()
            .ok_or_else(|| anyhow::anyhow!("server has no webtransport endpoint"))?;
        let ep = net_transport::webtransport::client_endpoint(&data.cert_sha256)?;
        let conn = ep.connect(url).await?;
        conn.send_datagram(ticket)?;
        Arc::new(Sink::Wt(conn))
    } else {
        let transport = quic::transport_config(IDLE_TIMEOUT, Duration::from_secs(5));
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
                let mut link = link.lock();
                std::iter::from_fn(|| link.next_outgoing()).collect()
            };
            for datagram in out {
                stats
                    .bytes_out
                    .fetch_add(datagram.len() as u64, Ordering::Relaxed);
                match &*sink {
                    Sink::Quic(conn) => {
                        let _ = conn.send_datagram(datagram);
                    }
                    Sink::Wt(conn) => {
                        let _ = conn.send_datagram(datagram);
                    }
                    Sink::Ws(sender) => {
                        let _ = sender.try_send(Message::Binary(datagram.to_vec().into()));
                    }
                }
            }
        }
    };

    // WS writer (tunnel frames + nothing else after join).
    let writer = tokio::spawn(async move {
        while let Some(message) = ws_rx.recv().await {
            if tx.send(message).await.is_err() {
                break;
            }
        }
    });

    let sync = cli.mode == "sync" || cli.mode == "lockstep";
    let server_tick = Arc::new(std::sync::atomic::AtomicU32::new(u32::MAX));
    let started = Instant::now();
    let round_trip_ms = Arc::new(AtomicU64::new(0));
    let catchup = Arc::new(Mutex::new(Catchup::default()));
    let acks = Arc::new(Mutex::new(net_replication::AckTracker::default()));
    let drop_in = cli.drop_in;
    let run_tag = cli
        .run
        .wrapping_mul(1_000_003)
        .wrapping_add((room * 1000 + idx) as u64);
    let probe = (cli.objects > 0 && room == 0 && idx == 0)
        .then(|| {
            let props: serde_json::Value = serde_json::from_str(&cli.props_json).ok()?;
            let schema = net_wire::Schema::parse(props["schema"].as_str()?).ok()?;
            Some(Arc::new(Mutex::new(net_sim::Frame::new(
                schema,
                Default::default(),
                0,
            ))))
        })
        .flatten();
    // Incoming datagrams -> link -> latency samples.
    let on_datagram = {
        let server_tick = server_tick.clone();
        let link = link.clone();
        let stats = stats.clone();
        let flush = flush.clone();
        let catchup = catchup.clone();
        let acks = acks.clone();
        let rng = Arc::new(AtomicU64::new(0x9E37_79B9_7F4A_7C15 ^ run_tag));
        let probe = probe.clone();
        move |d: Bytes| {
            stats.bytes_in.fetch_add(d.len() as u64, Ordering::Relaxed);
            if drop_in > 0.0 {
                let mut state = rng.load(Ordering::Relaxed);
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                rng.store(state, Ordering::Relaxed);
                if ((state >> 11) as f64) / ((1u64 << 53) as f64) < drop_in {
                    stats.dropped.fetch_add(1, Ordering::Relaxed);
                    return;
                }
            }
            let recv = link.lock().receive(d, Instant::now());
            flush();
            for message in recv {
                if message.payload.is_empty() {
                    continue;
                }
                if sync {
                    if message.payload[0] == net_api::sync::KIND_DELTA && message.payload.len() >= 9
                    {
                        stats.received.fetch_add(1, Ordering::Relaxed);
                        stats
                            .entities
                            .fetch_add(delta_entities(&message.payload[1..]), Ordering::Relaxed);
                        if let Some(probe) = &probe {
                            let mut probe = probe.lock();
                            if net_replication::Replicator::apply(&mut probe, &message.payload[1..])
                                .is_ok()
                            {
                                stats
                                    .probe_entities
                                    .store(probe.entities().count() as u64, Ordering::Relaxed);
                            }
                        }
                        let (tick, base) =
                            net_replication::delta_header(&message.payload[1..]).unwrap();
                        server_tick.store(tick, Ordering::Relaxed);
                        let mut acks = acks.lock();
                        if acks.on_delta(tick, base) {
                            stats.gaps.fetch_add(1, Ordering::Relaxed);
                        }
                        if let Some((last, bits)) = acks.ack() {
                            drop(acks);
                            let frame = net_api::sync::SyncRoom::encode_ack(last, bits);
                            let mut link = link.lock();
                            if link
                                .send(Channel::UnreliableSequenced, frame, Instant::now())
                                .is_ok()
                            {
                                stats.acks.fetch_add(1, Ordering::Relaxed);
                            }
                            link.flush_batch();
                            drop(link);
                            flush();
                        }
                        continue;
                    }
                    if message.payload[0] == net_api::lockstep::KIND_CATCHUP {
                        if let Some(ck) =
                            net_api::lockstep::LockstepRoom::decode_catchup(message.payload)
                        {
                            stats.catchups.fetch_add(1, Ordering::Relaxed);
                            stats
                                .catchup_from_tick
                                .fetch_add(ck.tick as u64, Ordering::Relaxed);
                            catchup.lock().next = Some(ck.tick);
                        }
                        continue;
                    }
                    if message.payload[0] == net_api::lockstep::KIND_CHECKPOINT_REQ {
                        // We are the master: upload a (fake, fixed-size) confirmed state.
                        if let Some(requested) =
                            net_api::lockstep::LockstepRoom::decode_checkpoint_request(
                                message.payload,
                            )
                        {
                            let st = server_tick.load(Ordering::Relaxed);
                            let tick = if st == u32::MAX {
                                requested
                            } else {
                                st.min(requested)
                            };
                            let frame = net_api::lockstep::LockstepRoom::encode_checkpoint(
                                net_common::types::Tick(tick),
                                0xA0_u64 | tick as u64,
                                &[7u8; 256],
                            );
                            let _ =
                                link.lock()
                                    .send(Channel::ReliableOrdered, frame, Instant::now());
                            flush();
                            stats.checkpoints.fetch_add(1, Ordering::Relaxed);
                        }
                        continue;
                    }
                    if message.payload[0] == net_api::lockstep::KIND_BUNDLE
                        && message.channel == Channel::ReliableOrdered
                    {
                        // Journal tail: must be contiguous from the checkpoint tick.
                        let mut cu = catchup.lock();
                        for (tick, _) in
                            net_api::lockstep::LockstepRoom::decode_bundles(message.payload)
                        {
                            if cu.next == Some(tick.0) {
                                cu.next = Some(tick.0 + 1);
                                stats.catchup_bundles.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                        cu.check(&stats);
                        continue;
                    }
                    if message.payload[0] == net_api::lockstep::KIND_BUNDLE {
                        let mut cu = catchup.lock();
                        if cu.next.is_some() {
                            // Live bundles may overtake the reliable tail: remember the oldest
                            // tick the live stream has covered so far.
                            if let Some((t, _)) = net_api::lockstep::LockstepRoom::decode_bundles(
                                message.payload.clone(),
                            )
                            .first()
                            {
                                cu.live_oldest = Some(cu.live_oldest.map_or(t.0, |o| o.min(t.0)));
                            }
                            cu.check(&stats);
                        }
                        drop(cu);
                        // newest bundle = server clock; count bundles (not redundant copies)
                        if let Some((t, _)) =
                            net_api::lockstep::LockstepRoom::decode_bundles(message.payload).last()
                        {
                            stats.received.fetch_add(1, Ordering::Relaxed);
                            server_tick.store(t.0 + 1, Ordering::Relaxed);
                        }
                    }
                    continue;
                }
                if let Ok(event) = EventFrame::decode(message.payload) {
                    if event.payload.len() >= 8 {
                        let ts = u64::from_le_bytes(event.payload[..8].try_into().unwrap());
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
            Some(tokio::spawn(async move {
                while let Ok(datagram) = conn.read_datagram().await {
                    on(datagram);
                }
            }))
        }
        Sink::Wt(conn) => {
            let conn = conn.clone();
            let on = on_datagram.clone();
            Some(tokio::spawn(async move {
                while let Ok(datagram) = conn.receive_datagram().await {
                    on(datagram.payload());
                }
            }))
        }
        Sink::Ws(_) => None,
    };
    // The control socket: tunnelled datagrams, and the pongs the input lead is measured from.
    let control = {
        let on = on_datagram.clone();
        let round_trip_ms = round_trip_ms.clone();
        tokio::spawn(async move {
            while let Some(Ok(m)) = rx.next().await {
                match m {
                    Message::Binary(bytes) => on(bytes),
                    Message::Text(text) => {
                        if let Ok(ServerMsg::Pong { ts, .. }) = serde_json::from_str(text.as_str())
                        {
                            let now = started.elapsed().as_millis() as u64;
                            round_trip_ms.store(now.saturating_sub(ts), Ordering::Relaxed);
                        }
                    }
                    _ => {}
                }
            }
        })
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
            // Follow the server clock. The last delta is half a round trip old and an input takes
            // the other half to arrive, so inputs are stamped a round trip and two ticks ahead.
            let st = server_tick.load(Ordering::Relaxed);
            if tick.is_multiple_of(tick_hz.unwrap_or(cli.tick_hz).max(1) as u32) {
                let ts = started.elapsed().as_millis() as u64;
                let _ = ws_tx.try_send(send(&ClientMsg::Ping { ts }));
            }
            let round_trip = round_trip_ms.load(Ordering::Relaxed) as f64 / 1000.0;
            let lead = 2 + (round_trip / period.as_secs_f64()).ceil() as u32;
            if st != u32::MAX && (tick + 2 < st + lead || tick > st + lead + 6) {
                tick = st + lead;
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
            recent.push_back(if cli.objects > 0 {
                cli.objects.to_le_bytes().to_vec()
            } else {
                vec![dir.0 as u8, dir.1 as u8, 0]
            });
            while recent.len() > net_api::sync::INPUT_REDUNDANCY {
                recent.pop_front();
            }
            let first = tick + 1 - recent.len() as u32;
            let inputs: Vec<Vec<u8>> = recent.iter().cloned().collect();
            let frame =
                net_api::sync::SyncRoom::encode_input(net_common::types::Tick(first), &inputs);
            {
                let mut link = link.lock();
                if link
                    .send(Channel::UnreliableSequenced, frame, Instant::now())
                    .is_ok()
                {
                    stats.sent.fetch_add(1, Ordering::Relaxed);
                }
                link.flush_batch();
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
            let mut link = link.lock();
            if link
                .send(Channel::UnreliableSequenced, frame.encode(), Instant::now())
                .is_ok()
            {
                stats.sent.fetch_add(1, Ordering::Relaxed);
            }
            link.flush_batch();
        }
        flush();
    }
    // Drain window for late deliveries.
    tokio::time::sleep(Duration::from_secs(2)).await;
    {
        let cu = catchup.lock();
        if cu.next.is_some() {
            tracing::warn!(room, idx, next = ?cu.next, live_oldest = ?cu.live_oldest, server_tick = server_tick.load(Ordering::Relaxed), "still catching up at exit");
        }
    }
    if let Some(reader) = reader {
        reader.abort();
    }
    control.abort();
    ticker.abort();
    let _ = ws_tx.send(Message::Close(None)).await;
    writer.abort();
    match &*sink {
        Sink::Quic(conn) => conn.close(0u32.into(), b"done"),
        Sink::Wt(conn) => conn.close(0u32.into(), b"done"),
        Sink::Ws(_) => {}
    }
    Ok(())
}

/// Late-join bookkeeping: `next` is the journal tick we expect next over the reliable
/// channel (None = not catching up); `live_oldest` the oldest tick the live stream covered.
/// Caught up = the reliable tail reached a tick the live stream also covers (no gap).
#[derive(Default)]
struct Catchup {
    next: Option<u32>,
    live_oldest: Option<u32>,
}

impl Catchup {
    fn check(&mut self, stats: &Stats) {
        if let (Some(n), Some(o)) = (self.next, self.live_oldest) {
            if n >= o {
                stats.caught_up.fetch_add(1, Ordering::Relaxed);
                self.next = None;
                self.live_oldest = None;
            }
        }
    }
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
        } else if let Some(rest) = line.strip_prefix(&format!("{name}_count ")) {
            count = rest.trim().parse().unwrap_or(0);
        } else if let Some(rest) = line.strip_prefix(&format!("{name}_sum ")) {
            sum = rest.trim().parse().unwrap_or(0.0);
        }
    }
    if count == 0 {
        return format!("{name}: no samples");
    }
    buckets.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    let quantile = |p: f64| {
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
        quantile(0.5),
        quantile(0.99)
    )
}

fn summarize_counter(body: &str, name: &str) -> String {
    let value = body
        .lines()
        .find(|l| l.starts_with(name))
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or("?");
    format!("server {name}={value}")
}
