//! Rust client for Aurix Net: control channel over WebSocket, data path over QUIC (pinned
//! certificate) or the WebSocket tunnel, with the same `Link` channels the server uses.
//! Engine SDKs and the C ABI are thin layers over this crate.

pub mod lockstep;
pub mod margin;
pub mod rollback;

use base64::Engine;
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use net_common::{Channel, RoomMode};
use net_rooms::protocol::{ClientMsg, DataEndpoint, ServerMsg};
use net_transport::{quic, Link};
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

pub use net_common;
pub use net_rooms::event::{CacheOp, EventFrame, EventTarget};
pub use net_rooms::protocol;

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("http: {0}")]
    Http(String),
    #[error("websocket: {0}")]
    Ws(String),
    #[error("server: {code}: {message}")]
    Server { code: String, message: String },
    #[error("data path: {0}")]
    Data(String),
    #[error("disconnected")]
    Disconnected,
    #[error("timeout")]
    Timeout,
}

pub type Result<T> = std::result::Result<T, ClientError>;

/// Issues a join grant through the developer's API key. Game servers / backends only: the key
/// must never ship inside a game client.
pub async fn issue_grant(
    http_url: &str,
    api_key: &str,
    player_id: &str,
    name: &str,
) -> Result<String> {
    let v: serde_json::Value = reqwest::Client::new()
        .post(format!("{http_url}/v1/grants"))
        .bearer_auth(api_key)
        .json(&serde_json::json!({ "player_id": player_id, "name": name }))
        .send()
        .await
        .map_err(|e| ClientError::Http(e.to_string()))?
        .error_for_status()
        .map_err(|e| ClientError::Http(e.to_string()))?
        .json()
        .await
        .map_err(|e| ClientError::Http(e.to_string()))?;
    v["grant"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| ClientError::Http("no grant in response".into()))
}

#[derive(Debug, Clone)]
pub struct Options {
    /// Force the WebSocket tunnel instead of QUIC.
    pub tunnel: bool,
    pub connect_timeout: Duration,
    /// How often the link timer runs (acks, retransmits, batches).
    pub tick: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            tunnel: false,
            connect_timeout: Duration::from_secs(10),
            tick: Duration::from_millis(5),
        }
    }
}

/// Everything the game sees from the server.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum Event {
    /// Any control message (Joined, PlayerJoined, Event via control fallback, Desync, ...).
    Control(ServerMsg),
    /// A data-path message on `channel` (relay event frame, sync delta, lockstep bundle).
    Data {
        channel: Channel,
        tick: u16,
        payload: Bytes,
    },
    /// Data path became ready (first ack from the server).
    DataReady,
    Closed(String),
}

enum Sink {
    Quic(quinn::Connection),
    Ws(mpsc::Sender<Message>),
}

struct DataPath {
    link: Mutex<Link>,
    sink: Sink,
}

impl DataPath {
    fn flush(&self) {
        let out: Vec<Bytes> = {
            let mut l = self.link.lock();
            std::iter::from_fn(|| l.next_outgoing()).collect()
        };
        for d in out {
            match &self.sink {
                Sink::Quic(c) => {
                    let _ = c.send_datagram(d);
                }
                Sink::Ws(tx) => {
                    let _ = tx.try_send(Message::Binary(d));
                }
            }
        }
    }
}

const PING_EVERY: Duration = Duration::from_secs(1);

pub struct Client {
    ctl: mpsc::Sender<Message>,
    events: mpsc::Receiver<Event>,
    events_tx: mpsc::Sender<Event>,
    /// Events the room kept for late joiners, handed out before anything from the data path.
    replay: VecDeque<Event>,
    /// One lost connection is reported once, whichever of the link, the data path and the
    /// control channel notices it first.
    lost: bool,
    data: Option<Arc<DataPath>>,
    opts: Options,
    pub joined: Option<Joined>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    /// Tasks of the current data path; they end with it.
    data_tasks: Vec<tokio::task::JoinHandle<()>>,
    tunnel_slot: Option<Arc<Mutex<Option<Arc<DataPath>>>>>,
    /// What `rejoin` needs after a rehost: the URL given to `connect`, the grant, the last join.
    entry_url: String,
    grant: String,
    last_join: Option<ClientMsg>,
    rtt_ms: Arc<AtomicU32>,
}

/// Room facts from the `joined` message.
#[derive(Debug, Clone)]
pub struct Joined {
    pub room: String,
    pub name: String,
    pub mode: RoomMode,
    pub actor: net_common::ActorId,
    pub master: net_common::ActorId,
    pub tick_hz: Option<u16>,
    pub schema_hash: Option<String>,
    pub data: DataEndpoint,
}

impl Client {
    /// Opens the control channel and authenticates with a grant.
    pub async fn connect(http_url: &str, grant: &str, opts: Options) -> Result<Client> {
        let ws_url = http_url.replacen("http", "ws", 1) + "/ws";
        let (ws, _) = tokio::time::timeout(
            opts.connect_timeout,
            tokio_tungstenite::connect_async(&ws_url),
        )
        .await
        .map_err(|_| ClientError::Timeout)?
        .map_err(|e| ClientError::Ws(e.to_string()))?;
        let (mut tx, mut rx) = ws.split();
        let (ctl, mut ctl_rx) = mpsc::channel::<Message>(256);
        let (events_tx, events) = mpsc::channel::<Event>(4096);
        let writer = tokio::spawn(async move {
            while let Some(m) = ctl_rx.recv().await {
                if tx.send(m).await.is_err() {
                    break;
                }
            }
        });
        let mut c = Client {
            ctl,
            events,
            replay: VecDeque::new(),
            lost: false,
            events_tx: events_tx.clone(),
            data: None,
            opts,
            joined: None,
            tasks: vec![writer],
            data_tasks: Vec::new(),
            tunnel_slot: None,
            entry_url: http_url.to_string(),
            grant: grant.to_string(),
            last_join: None,
            rtt_ms: Arc::new(AtomicU32::new(0)),
        };
        // The control round trip, measured once a second: sync inputs are stamped ahead by it.
        let started = Instant::now();
        let probe = Arc::new(AtomicU64::new(u64::MAX));
        let pinger = {
            let (ctl, probe) = (c.ctl.clone(), probe.clone());
            tokio::spawn(async move {
                let mut every = tokio::time::interval(PING_EVERY);
                loop {
                    every.tick().await;
                    let ts = started.elapsed().as_millis() as u64;
                    probe.store(ts, Ordering::Relaxed);
                    let ping = ClientMsg::Ping { ts };
                    let Ok(text) = serde_json::to_string(&ping) else {
                        break;
                    };
                    if ctl.send(Message::Text(text.into())).await.is_err() {
                        break;
                    }
                }
            })
        };
        c.tasks.push(pinger);
        let rtt_ms = c.rtt_ms.clone();
        // Reader: text -> Control events, binary -> tunnel datagrams (routed via data path if any).
        let tunnel_in: Arc<Mutex<Option<Arc<DataPath>>>> = Arc::new(Mutex::new(None));
        let tunnel_slot = tunnel_in.clone();
        let reader = tokio::spawn(async move {
            while let Some(m) = rx.next().await {
                match m {
                    Ok(Message::Text(t)) => match serde_json::from_str::<ServerMsg>(&t) {
                        Ok(msg) => {
                            // The answer to the client's own probe is not the game's business.
                            let sent = probe.load(Ordering::Relaxed);
                            if matches!(msg, ServerMsg::Pong { ts, .. } if ts == sent) {
                                let sample = (started.elapsed().as_millis() as u64)
                                    .saturating_sub(sent)
                                    .min(u64::from(u32::MAX))
                                    as u32;
                                let smoothed = match rtt_ms.load(Ordering::Relaxed) {
                                    0 => sample,
                                    rtt => (rtt * 7 + sample) / 8,
                                };
                                rtt_ms.store(smoothed, Ordering::Relaxed);
                                continue;
                            }
                            if events_tx.send(Event::Control(msg)).await.is_err() {
                                break;
                            }
                        }
                        Err(e) => tracing::warn!(error = %e, "bad control message"),
                    },
                    Ok(Message::Binary(b)) => {
                        let dp = tunnel_slot.lock().clone();
                        if let Some(dp) = dp {
                            deliver(&dp, b, &events_tx).await;
                        }
                    }
                    Ok(Message::Close(_)) | Err(_) => {
                        let _ = events_tx
                            .send(Event::Closed("control channel closed".into()))
                            .await;
                        break;
                    }
                    _ => {}
                }
            }
        });
        c.tasks.push(reader);
        c.tunnel_slot = Some(tunnel_in);
        c.send(ClientMsg::Auth {
            grant: grant.to_string(),
        })
        .await?;
        // wait for Authed (or Error)
        loop {
            match c.next_event().await {
                Some(Event::Control(ServerMsg::Authed { .. })) => break,
                Some(Event::Control(ServerMsg::Hello { .. })) => continue,
                Some(Event::Control(ServerMsg::Error { code, message })) => {
                    return Err(ClientError::Server { code, message })
                }
                Some(Event::Closed(_)) | None => return Err(ClientError::Disconnected),
                _ => continue,
            }
        }
        Ok(c)
    }

    /// Sends a control message.
    pub async fn send(&self, msg: ClientMsg) -> Result<()> {
        let txt = serde_json::to_string(&msg).map_err(|e| ClientError::Ws(e.to_string()))?;
        self.ctl
            .send(Message::Text(txt.into()))
            .await
            .map_err(|_| ClientError::Disconnected)
    }

    /// `connect` + `join`, following one `room_elsewhere:<url>` redirect: rooms are pinned to
    /// the node that hosts them, so a join sent to another node of the region answers with
    /// the hosting node's URL and the client reconnects there with the same grant.
    pub async fn connect_and_join(
        http_url: &str,
        grant: &str,
        opts: Options,
        msg: ClientMsg,
    ) -> Result<(Client, Joined)> {
        let mut c = Client::connect(http_url, grant, opts.clone()).await?;
        match c.join(msg.clone()).await {
            Ok(j) => Ok((c, j)),
            Err(ClientError::Server { code, message })
                if code == "conflict" && message.contains("room_elsewhere:") =>
            {
                let url = message
                    .split("room_elsewhere:")
                    .nth(1)
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                drop(c);
                let mut c = Client::connect(&url, grant, opts).await?;
                let j = c.join(msg).await?;
                Ok((c, j))
            }
            Err(e) => Err(e),
        }
    }

    /// Joins (or creates) a room and opens the data path. Returns when the data path is ready.
    /// After `left{reason:"rehost"}` (the node shut down and handed the room to another node):
    /// a fresh client in the same room with the same grant, trying the entry URL then
    /// `fallback_urls` (other nodes / the region URL).
    pub async fn rejoin(self, fallback_urls: &[&str]) -> Result<(Client, Joined)> {
        let msg = match self.last_join.clone() {
            Some(ClientMsg::JoinOrCreate {
                name, player_props, ..
            }) => ClientMsg::Join {
                name,
                player_props,
                rejoin_actor: None,
                schema_hash: None,
                spectator: false,
            },
            Some(m) => m,
            None => return Err(ClientError::Disconnected),
        };
        let (entry, grant, opts) = (
            self.entry_url.clone(),
            self.grant.clone(),
            self.opts.clone(),
        );
        drop(self);
        let mut last = ClientError::Disconnected;
        for url in std::iter::once(entry.as_str()).chain(fallback_urls.iter().copied()) {
            match Client::connect_and_join(url, &grant, opts.clone(), msg.clone()).await {
                Ok(r) => return Ok(r),
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    pub async fn join(&mut self, msg: ClientMsg) -> Result<Joined> {
        self.last_join = Some(msg.clone());
        self.replay.clear();
        self.lost = false;
        self.send(msg).await?;
        let (joined, cached) = loop {
            match self.next_event().await {
                Some(Event::Control(ServerMsg::Joined {
                    room,
                    name,
                    mode,
                    actor,
                    master,
                    data,
                    tick_hz,
                    schema_hash,
                    cached,
                    ..
                })) => {
                    let joined = Joined {
                        room,
                        name,
                        mode,
                        actor,
                        master,
                        tick_hz,
                        schema_hash,
                        data,
                    };
                    break (joined, cached);
                }
                Some(Event::Control(ServerMsg::Error { code, message })) => {
                    return Err(ClientError::Server { code, message })
                }
                Some(Event::Closed(_)) | None => return Err(ClientError::Disconnected),
                _ => continue,
            }
        };
        self.open_data(&joined.data).await?;
        self.replay = cached
            .into_iter()
            .filter_map(|(code, from, data)| {
                let payload = base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .ok()?;
                let frame = EventFrame {
                    code,
                    target: EventTarget::Others,
                    cache: CacheOp::Add,
                    group: None,
                    from,
                    payload: payload.into(),
                };
                Some(Event::Data {
                    channel: Channel::ReliableOrdered,
                    tick: 0,
                    payload: frame.encode(),
                })
            })
            .collect();
        self.joined = Some(joined.clone());
        Ok(joined)
    }

    /// Stops the data path of the room the client is leaving. Left running, it would go on
    /// resending what the room has not acknowledged, to a node that no longer expects it.
    fn close_data(&mut self) {
        for task in self.data_tasks.drain(..) {
            task.abort();
        }
        if let Some(slot) = &self.tunnel_slot {
            *slot.lock() = None;
        }
        if let Some(dp) = self.data.take() {
            if let Sink::Quic(conn) = &dp.sink {
                conn.close(0u32.into(), b"bye");
            }
        }
    }

    async fn open_data(&mut self, ep: &DataEndpoint) -> Result<()> {
        self.close_data();
        let ticket = base64::engine::general_purpose::STANDARD
            .decode(&ep.ticket)
            .map_err(|e| ClientError::Data(e.to_string()))?;
        let link = Mutex::new(Link::new(Instant::now()));
        let dp: Arc<DataPath> = if self.opts.tunnel {
            let (ws_tx, mut ws_rx) = mpsc::channel::<Message>(1024);
            let ctl = self.ctl.clone();
            self.data_tasks.push(tokio::spawn(async move {
                while let Some(m) = ws_rx.recv().await {
                    if ctl.send(m).await.is_err() {
                        break;
                    }
                }
            }));
            let dp = Arc::new(DataPath {
                link,
                sink: Sink::Ws(ws_tx),
            });
            // Register before sending the ticket: the server's ack may arrive immediately.
            if let Some(slot) = &self.tunnel_slot {
                *slot.lock() = Some(dp.clone());
            }
            self.ctl
                .send(Message::Binary(ticket.into()))
                .await
                .map_err(|_| ClientError::Disconnected)?;
            dp
        } else {
            let transport = quic::transport_config(Duration::from_secs(15), Duration::from_secs(5));
            let endpoint =
                quic::client_endpoint("0.0.0.0:0".parse().unwrap(), &ep.cert_sha256, transport)
                    .map_err(|e| ClientError::Data(e.to_string()))?;
            let addr = tokio::net::lookup_host((ep.host.as_str(), ep.port))
                .await
                .map_err(|e| ClientError::Data(e.to_string()))?
                .find(|a| a.is_ipv4())
                .ok_or_else(|| ClientError::Data("no address".into()))?;
            let conn = tokio::time::timeout(self.opts.connect_timeout, async {
                endpoint
                    .connect(addr, "aurix-net")
                    .map_err(|e| ClientError::Data(e.to_string()))?
                    .await
                    .map_err(|e| ClientError::Data(e.to_string()))
            })
            .await
            .map_err(|_| ClientError::Timeout)??;
            conn.send_datagram(Bytes::from(ticket))
                .map_err(|e| ClientError::Data(e.to_string()))?;
            let dp = Arc::new(DataPath {
                link,
                sink: Sink::Quic(conn.clone()),
            });
            let (dp2, ev) = (dp.clone(), self.events_tx.clone());
            self.data_tasks.push(tokio::spawn(async move {
                while let Ok(d) = conn.read_datagram().await {
                    deliver(&dp2, d, &ev).await;
                }
                let _ = ev.send(Event::Closed("data path closed".into())).await;
            }));
            dp
        };
        // link timer
        let (dp2, tick, ev) = (dp.clone(), self.opts.tick, self.events_tx.clone());
        self.data_tasks.push(tokio::spawn(async move {
            let mut iv = tokio::time::interval(tick);
            loop {
                iv.tick().await;
                if dp2.link.lock().poll(Instant::now()).is_err() {
                    let _ = ev.send(Event::Closed("link dead".into())).await;
                    break;
                }
                dp2.flush();
            }
        }));
        self.data = Some(dp);
        // wait for the server's empty ack frame
        let deadline = Instant::now() + self.opts.connect_timeout;
        loop {
            match tokio::time::timeout_at(deadline.into(), self.next_event()).await {
                Ok(Some(Event::DataReady)) => return Ok(()),
                Ok(Some(Event::Closed(_))) | Ok(None) => return Err(ClientError::Disconnected),
                Ok(Some(_)) => continue,
                Err(_) => return Err(ClientError::Timeout),
            }
        }
    }

    /// Queues a data-path message; unreliable messages leave on the next timer tick (or
    /// immediately with `flush`).
    pub fn send_data(&self, channel: Channel, payload: Bytes) -> Result<()> {
        let dp = self.data.as_ref().ok_or(ClientError::Disconnected)?;
        dp.link
            .lock()
            .send(channel, payload, Instant::now())
            .map_err(|e| ClientError::Data(format!("{e:?}")))?;
        Ok(())
    }

    /// Pushes everything queued onto the wire now (call once per game frame).
    pub fn flush(&self) {
        if let Some(dp) = &self.data {
            dp.link.lock().flush_batch();
            dp.flush();
        }
    }

    /// Event with the room's defaults: no interest group, not cached. See [`Self::send_event_ex`].
    pub fn send_event(
        &self,
        code: u8,
        target: EventTarget,
        payload: Bytes,
        reliable: bool,
    ) -> Result<()> {
        self.send_event_ex(code, target, payload, reliable, None, CacheOp::None)
    }
    /// Event to `target` on the reliable or unreliable channel; `group` limits delivery to members of
    /// that interest group, `cache` keeps/removes it in the room cache for late joiners.
    pub fn send_event_ex(
        &self,
        code: u8,
        target: EventTarget,
        payload: Bytes,
        reliable: bool,
        group: Option<u16>,
        cache: CacheOp,
    ) -> Result<()> {
        let frame = EventFrame {
            code,
            target,
            cache,
            group,
            from: net_common::ActorId(0),
            payload,
        };
        self.send_data(
            if reliable {
                Channel::ReliableOrdered
            } else {
                Channel::UnreliableSequenced
            },
            frame.encode(),
        )
    }

    pub async fn next_event(&mut self) -> Option<Event> {
        loop {
            let event = match self.replay.pop_front() {
                Some(event) => event,
                None => self.events.recv().await?,
            };
            if self.reports(&event) {
                return Some(event);
            }
        }
    }

    /// Non-blocking poll for engines with their own loop.
    pub fn try_next_event(&mut self) -> Option<Event> {
        loop {
            let event = self
                .replay
                .pop_front()
                .or_else(|| self.events.try_recv().ok())?;
            if self.reports(&event) {
                return Some(event);
            }
        }
    }

    fn reports(&mut self, event: &Event) -> bool {
        !matches!(event, Event::Closed(_)) || !std::mem::replace(&mut self.lost, true)
    }

    /// Smoothed round trip of the control channel in milliseconds; 0 until the first sample.
    pub fn rtt_ms(&self) -> u32 {
        self.rtt_ms.load(Ordering::Relaxed)
    }

    pub fn link_stats(&self) -> Option<net_transport::LinkStats> {
        self.data.as_ref().map(|d| d.link.lock().stats())
    }

    pub async fn leave(&mut self) -> Result<()> {
        self.close_data();
        self.send(ClientMsg::Leave).await
    }

    /// Ask for project-wide room/player counts per lobby; the answer arrives as
    /// `Event::Control(ServerMsg::Stats { .. })`. At most one `stats`/`find_friends` per second.
    pub async fn stats(&self) -> Result<()> {
        self.send(ClientMsg::Stats).await
    }

    /// Ask where up to 64 players are (online, room) across the fleet; the answer arrives as
    /// `Event::Control(ServerMsg::Friends { .. })`.
    pub async fn find_friends(&self, players: Vec<String>) -> Result<()> {
        self.send(ClientMsg::FindFriends { players }).await
    }
    /// Call the game's backend through the node (the project's `rpc` webhook); the answer
    /// arrives as `Event::Control(ServerMsg::RpcResult { id, .. })`. Body ≤ 8 KiB, 5 per second.
    pub async fn rpc(
        &self,
        id: u32,
        name: impl Into<String>,
        body: serde_json::Value,
    ) -> Result<()> {
        self.send(ClientMsg::Rpc {
            id,
            name: name.into(),
            body,
        })
        .await
    }

    /// Sync rooms: steer area-of-interest: extra viewpoints (camera, scope target) and entities
    /// forced into view (team mates, objectives). Entity ids as in deltas.
    pub async fn set_view(&mut self, viewpoints: Vec<u64>, interest: Vec<u64>) -> Result<()> {
        self.send(ClientMsg::SetView {
            viewpoints,
            interest,
        })
        .await
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.close_data();
        for t in &self.tasks {
            t.abort();
        }
    }
}

async fn deliver(dp: &Arc<DataPath>, d: Bytes, ev: &mpsc::Sender<Event>) {
    let recv = dp.link.lock().receive(d, Instant::now());
    dp.flush();
    for m in recv {
        if m.payload.is_empty() {
            let _ = ev.send(Event::DataReady).await;
            continue;
        }
        if ev
            .send(Event::Data {
                channel: m.channel,
                tick: m.tick,
                payload: m.payload,
            })
            .await
            .is_err()
        {
            return;
        }
    }
}

/// One region candidate from [`best_region`].
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RegionCandidate {
    pub region: String,
    pub display_name: String,
    pub endpoint: String,
    /// Best of `probes` round trips to `/healthz`, or -1 when unreachable.
    pub rtt_ms: i32,
}

/// Result of [`best_region`]: the lowest-RTT live node plus every candidate measured.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RegionPick {
    pub region: String,
    pub endpoint: String,
    pub rtt_ms: i32,
    pub candidates: Vec<RegionCandidate>,
}

/// Region list and best-region ping in one call: reads `GET {catalog}/v1/regions` (any node or the
/// catalog answers), probes every live endpoint `probes` times over HTTP(S) `/healthz` and returns the
/// fastest. Use the chosen `endpoint` as the cell URL for `connect`. Nodes that do not answer are kept
/// in `candidates` with `rtt_ms = -1`.
pub async fn best_region(catalog_http: &str, probes: u8) -> Result<RegionPick> {
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .map_err(|e| ClientError::Http(e.to_string()))?;
    let base = catalog_http.trim_end_matches('/');
    let regions: Vec<serde_json::Value> = http
        .get(format!("{base}/v1/regions"))
        .send()
        .await
        .map_err(|e| ClientError::Http(e.to_string()))?
        .json()
        .await
        .map_err(|e| ClientError::Http(e.to_string()))?;
    let probes = probes.clamp(1, 10);
    let mut tasks = Vec::new();
    for r in &regions {
        let region = r["id"].as_str().unwrap_or_default().to_string();
        let display_name = r["display_name"].as_str().unwrap_or_default().to_string();
        for ep in r["endpoints"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|e| e.as_str())
        {
            let (http, region, display_name, ep) = (
                http.clone(),
                region.clone(),
                display_name.clone(),
                ep.trim_end_matches('/').to_string(),
            );
            tasks.push(async move {
                let mut best: i32 = -1;
                for _ in 0..probes {
                    let t = std::time::Instant::now();
                    if http.get(format!("{ep}/healthz")).send().await.is_ok() {
                        let ms = t.elapsed().as_millis() as i32;
                        best = if best < 0 { ms } else { best.min(ms) };
                    }
                }
                RegionCandidate {
                    region,
                    display_name,
                    endpoint: ep,
                    rtt_ms: best,
                }
            });
        }
    }
    let candidates: Vec<RegionCandidate> = futures::future::join_all(tasks).await;
    let Some(win) = candidates
        .iter()
        .filter(|c| c.rtt_ms >= 0)
        .min_by_key(|c| c.rtt_ms)
    else {
        return Err(ClientError::Http("no reachable region".into()));
    };
    Ok(RegionPick {
        region: win.region.clone(),
        endpoint: win.endpoint.clone(),
        rtt_ms: win.rtt_ms,
        candidates,
    })
}
