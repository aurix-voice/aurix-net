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
use net_rooms::protocol::{ClientMsg, DataEndpoint, ServerMsg, UNSEQUENCED};
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
    let response: serde_json::Value = reqwest::Client::new()
        .post(format!("{http_url}/v1/grants"))
        .bearer_auth(api_key)
        .json(&serde_json::json!({ "player_id": player_id, "name": name }))
        .send()
        .await
        .map_err(|error| ClientError::Http(error.to_string()))?
        .error_for_status()
        .map_err(|error| ClientError::Http(error.to_string()))?
        .json()
        .await
        .map_err(|error| ClientError::Http(error.to_string()))?;
    response["grant"]
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
            let mut link = self.link.lock();
            std::iter::from_fn(|| link.next_outgoing()).collect()
        };
        for datagram in out {
            match &self.sink {
                Sink::Quic(conn) => {
                    let _ = conn.send_datagram(datagram);
                }
                Sink::Ws(tx) => {
                    let _ = tx.try_send(Message::Binary(datagram));
                }
            }
        }
    }
}

const PING_EVERY: Duration = Duration::from_secs(1);
const TICKET_RESEND: Duration = Duration::from_millis(100);

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
    /// The node takes the unsequenced channel (`hello.supports`).
    unsequenced: bool,
    opts: Options,
    pub joined: Option<Joined>,
    /// Sync rooms: the entity the room made for this client as its avatar, once the room has
    /// announced it. Kept from the join on, whether or not the game reads the frame.
    pub avatar: Option<u64>,
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
    /// Lockstep rooms: the seed every client and the node start the game from.
    pub seed: Option<u64>,
    /// Sync rooms that answer tagged spawns: the first tag the room has not seen from this actor.
    pub next_spawn_tag: Option<u32>,
    pub data: DataEndpoint,
}

/// The avatar entity of a sync room's announcement: the answer (`0x14`) to spawn tag 0.
fn announced_avatar(frame: &[u8]) -> Option<u64> {
    let created = frame.len() == 18 && frame[0] == 0x14 && frame[1..6] == [0; 5];
    created.then(|| u64::from_be_bytes(frame[6..14].try_into().expect("8 bytes")))
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
        .map_err(|error| ClientError::Ws(error.to_string()))?;
        let (mut tx, mut rx) = ws.split();
        let (ctl, mut ctl_rx) = mpsc::channel::<Message>(256);
        let (events_tx, events) = mpsc::channel::<Event>(4096);
        let writer = tokio::spawn(async move {
            while let Some(message) = ctl_rx.recv().await {
                if tx.send(message).await.is_err() {
                    break;
                }
            }
        });
        let mut client = Client {
            ctl,
            events,
            replay: VecDeque::new(),
            lost: false,
            events_tx: events_tx.clone(),
            data: None,
            unsequenced: false,
            opts,
            joined: None,
            avatar: None,
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
            let (ctl, probe) = (client.ctl.clone(), probe.clone());
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
        client.tasks.push(pinger);
        let rtt_ms = client.rtt_ms.clone();
        // Reader: text -> Control events, binary -> tunnel datagrams (routed via data path if any).
        let tunnel_in: Arc<Mutex<Option<Arc<DataPath>>>> = Arc::new(Mutex::new(None));
        let tunnel_slot = tunnel_in.clone();
        let reader = tokio::spawn(async move {
            while let Some(message) = rx.next().await {
                match message {
                    Ok(Message::Text(text)) => match serde_json::from_str::<ServerMsg>(&text) {
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
                        Err(error) => tracing::warn!(%error, "bad control message"),
                    },
                    Ok(Message::Binary(datagram)) => {
                        let dp = tunnel_slot.lock().clone();
                        if let Some(dp) = dp {
                            deliver(&dp, datagram, &events_tx).await;
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
        client.tasks.push(reader);
        client.tunnel_slot = Some(tunnel_in);
        client
            .send(ClientMsg::Auth {
                grant: grant.to_string(),
                supports: vec![UNSEQUENCED.to_string()],
            })
            .await?;
        // wait for Authed (or Error)
        let deadline = tokio::time::Instant::now() + client.opts.connect_timeout;
        loop {
            match client.answer_by(deadline).await? {
                Event::Control(ServerMsg::Authed { .. }) => break,
                Event::Control(ServerMsg::Hello { supports, .. }) => {
                    client.unsequenced = supports.iter().any(|s| s == UNSEQUENCED);
                }
                Event::Control(ServerMsg::Error { code, message, .. }) => {
                    return Err(ClientError::Server { code, message })
                }
                _ => continue,
            }
        }
        Ok(client)
    }

    /// Sends a control message.
    pub async fn send(&self, msg: ClientMsg) -> Result<()> {
        let txt =
            serde_json::to_string(&msg).map_err(|error| ClientError::Ws(error.to_string()))?;
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
        let mut client = Client::connect(http_url, grant, opts.clone()).await?;
        match client.join(msg.clone()).await {
            Ok(joined) => Ok((client, joined)),
            Err(ClientError::Server { code, message })
                if code == "conflict" && message.contains("room_elsewhere:") =>
            {
                let url = message
                    .split("room_elsewhere:")
                    .nth(1)
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                drop(client);
                let mut client = Client::connect(&url, grant, opts).await?;
                let joined = client.join(msg).await?;
                Ok((client, joined))
            }
            Err(error) => Err(error),
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
            Some(msg) => msg,
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
                Ok(rejoined) => return Ok(rejoined),
                Err(error) => last = error,
            }
        }
        Err(last)
    }

    pub async fn join(&mut self, msg: ClientMsg) -> Result<Joined> {
        self.last_join = Some(msg.clone());
        self.replay.clear();
        self.lost = false;
        self.send(msg).await?;
        let deadline = tokio::time::Instant::now() + self.opts.connect_timeout;
        let (joined, cached) = loop {
            match self.answer_by(deadline).await? {
                Event::Control(ServerMsg::Joined {
                    room,
                    name,
                    mode,
                    actor,
                    master,
                    data,
                    tick_hz,
                    schema_hash,
                    seed,
                    next_spawn_tag,
                    cached,
                    ..
                }) => {
                    let joined = Joined {
                        room,
                        name,
                        mode,
                        actor,
                        master,
                        tick_hz,
                        schema_hash,
                        seed,
                        next_spawn_tag,
                        data,
                    };
                    break (joined, cached);
                }
                Event::Control(ServerMsg::Error { code, message, .. }) => {
                    return Err(ClientError::Server { code, message })
                }
                _ => continue,
            }
        };
        // the room announces the avatar as soon as the data path is up
        self.avatar = None;
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

    /// Stops the data path of the room the client is leaving or has lost. Left running, it would
    /// go on resending what the room has not acknowledged, to a node that no longer expects it.
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
            .map_err(|error| ClientError::Data(error.to_string()))?;
        let link = Mutex::new(Link::new(Instant::now()));
        let dp: Arc<DataPath> = if self.opts.tunnel {
            let (ws_tx, mut ws_rx) = mpsc::channel::<Message>(1024);
            let ctl = self.ctl.clone();
            self.data_tasks.push(tokio::spawn(async move {
                while let Some(message) = ws_rx.recv().await {
                    if ctl.send(message).await.is_err() {
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
                    .map_err(|error| ClientError::Data(error.to_string()))?;
            let addr = tokio::net::lookup_host((ep.host.as_str(), ep.port))
                .await
                .map_err(|error| ClientError::Data(error.to_string()))?
                .find(|address| address.is_ipv4())
                .ok_or_else(|| ClientError::Data("no address".into()))?;
            let conn = tokio::time::timeout(self.opts.connect_timeout, async {
                endpoint
                    .connect(addr, "aurix-net")
                    .map_err(|error| ClientError::Data(error.to_string()))?
                    .await
                    .map_err(|error| ClientError::Data(error.to_string()))
            })
            .await
            .map_err(|_| ClientError::Timeout)??;
            let ticket = Bytes::from(ticket);
            conn.send_datagram(ticket.clone())
                .map_err(|error| ClientError::Data(error.to_string()))?;
            let dp = Arc::new(DataPath {
                link,
                sink: Sink::Quic(conn.clone()),
            });
            let (dp2, ev) = (dp.clone(), self.events_tx.clone());
            self.data_tasks.push(tokio::spawn(async move {
                // the ticket is a datagram: it is repeated until the node's first one arrives
                let start = tokio::time::Instant::now() + TICKET_RESEND;
                let mut again = tokio::time::interval_at(start, TICKET_RESEND);
                let mut answered = false;
                loop {
                    tokio::select! {
                        datagram = conn.read_datagram() => {
                            let Ok(datagram) = datagram else { break };
                            answered = true;
                            deliver(&dp2, datagram, &ev).await;
                        }
                        _ = again.tick(), if !answered => {
                            let _ = conn.send_datagram(ticket.clone());
                        }
                    }
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
        let deadline = tokio::time::Instant::now() + self.opts.connect_timeout;
        loop {
            if let Event::DataReady = self.answer_by(deadline).await? {
                return Ok(());
            }
        }
    }

    /// The next event while a call waits for the node's answer: the wait ends with the
    /// connection and at `deadline`.
    async fn answer_by(&mut self, deadline: tokio::time::Instant) -> Result<Event> {
        match tokio::time::timeout_at(deadline, self.next_event()).await {
            Ok(Some(Event::Closed(_))) | Ok(None) => Err(ClientError::Disconnected),
            Ok(Some(event)) => Ok(event),
            Err(_) => Err(ClientError::Timeout),
        }
    }

    /// Queues a data-path message; unreliable messages leave on the next timer tick (or
    /// immediately with `flush`).
    /// A message for [`Channel::UnreliableUnsequenced`] goes on the sequenced channel to a node
    /// that does not take it. Fails with [`ClientError::Disconnected`] outside a room and once
    /// the connection is lost.
    pub fn send_data(&self, channel: Channel, payload: Bytes) -> Result<()> {
        let dp = self.data.as_ref().ok_or(ClientError::Disconnected)?;
        let channel = match channel {
            Channel::UnreliableUnsequenced if !self.unsequenced => Channel::UnreliableSequenced,
            channel => channel,
        };
        dp.link
            .lock()
            .send(channel, payload, Instant::now())
            .map_err(|error| ClientError::Data(format!("{error:?}")))?;
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
        let channel = if reliable {
            Channel::ReliableOrdered
        } else {
            Channel::UnreliableSequenced
        };
        self.send_event_on(channel, code, target, payload, group, cache)
    }
    /// [`Self::send_event_ex`] on a channel of the caller's choice: the sequenced one for state,
    /// where a newer event replaces an older one, the unsequenced one for events that are all
    /// wanted.
    pub fn send_event_on(
        &self,
        channel: Channel,
        code: u8,
        target: EventTarget,
        payload: Bytes,
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
        self.send_data(channel, frame.encode())
    }

    pub async fn next_event(&mut self) -> Option<Event> {
        loop {
            let event = match self.replay.pop_front() {
                Some(event) => event,
                None => {
                    let event = self.events.recv().await?;
                    self.note(&event);
                    event
                }
            };
            if self.reports(&event) {
                return Some(event);
            }
        }
    }

    /// Non-blocking poll for engines with their own loop.
    pub fn try_next_event(&mut self) -> Option<Event> {
        loop {
            let event = match self.replay.pop_front() {
                Some(event) => event,
                None => {
                    let event = self.events.try_recv().ok()?;
                    self.note(&event);
                    event
                }
            };
            if self.reports(&event) {
                return Some(event);
            }
        }
    }

    /// The node took the client out of its room (kick, room closed, rehost). `left` names the
    /// room: one about a room left earlier, or the answer to the client's own `leave`, changes
    /// nothing.
    fn forget_room_on_left(&mut self, reason: &str, room: Option<&String>) {
        let current = self.joined.as_ref().map(|joined| &joined.room);
        if reason != "left" && current.is_some() && current == room {
            self.joined = None;
            self.avatar = None;
        }
    }

    /// What the client keeps of an event whether or not the game reads it.
    fn note(&mut self, event: &Event) {
        match event {
            Event::Control(ServerMsg::Left { reason, room }) => {
                self.forget_room_on_left(reason, room.as_ref());
            }
            Event::Data { payload, .. } => {
                if let Some(entity) = announced_avatar(payload) {
                    self.avatar = Some(entity);
                }
            }
            Event::Closed(_) => self.close_data(),
            _ => {}
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
        self.data.as_ref().map(|path| path.link.lock().stats())
    }

    pub async fn leave(&mut self) -> Result<()> {
        self.close_data();
        self.joined = None;
        self.avatar = None;
        self.send(ClientMsg::Leave).await
    }

    /// Ask for project-wide room/player counts per lobby; the answer arrives as
    /// `Event::Control(ServerMsg::Stats { id, .. })`, a refusal as `ServerMsg::Error { id, .. }`,
    /// both with the `id` given here. At most one `stats`/`find_friends` per second.
    pub async fn stats(&self, id: u32) -> Result<()> {
        self.send(ClientMsg::Stats { id: Some(id) }).await
    }

    /// Ask where up to 64 players are (online, room) across the fleet; the answer arrives as
    /// `Event::Control(ServerMsg::Friends { id, .. })`, a refusal as `ServerMsg::Error { id, .. }`.
    pub async fn find_friends(&self, id: u32, players: Vec<String>) -> Result<()> {
        self.send(ClientMsg::FindFriends {
            players,
            id: Some(id),
        })
        .await
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
        for task in &self.tasks {
            task.abort();
        }
    }
}

async fn deliver(dp: &Arc<DataPath>, datagram: Bytes, ev: &mpsc::Sender<Event>) {
    let recv = dp.link.lock().receive(datagram, Instant::now());
    dp.flush();
    for message in recv {
        if message.payload.is_empty() {
            let _ = ev.send(Event::DataReady).await;
            continue;
        }
        if ev
            .send(Event::Data {
                channel: message.channel,
                tick: message.tick,
                payload: message.payload,
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
        .map_err(|error| ClientError::Http(error.to_string()))?;
    let base = catalog_http.trim_end_matches('/');
    let regions: Vec<serde_json::Value> = http
        .get(format!("{base}/v1/regions"))
        .send()
        .await
        .map_err(|error| ClientError::Http(error.to_string()))?
        .json()
        .await
        .map_err(|error| ClientError::Http(error.to_string()))?;
    let probes = probes.clamp(1, 10);
    let mut tasks = Vec::new();
    for entry in &regions {
        let region = entry["id"].as_str().unwrap_or_default().to_string();
        let display_name = entry["display_name"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        for ep in entry["endpoints"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|endpoint| endpoint.as_str())
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
                    let started = std::time::Instant::now();
                    if http.get(format!("{ep}/healthz")).send().await.is_ok() {
                        let ms = started.elapsed().as_millis() as i32;
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
        .filter(|candidate| candidate.rtt_ms >= 0)
        .min_by_key(|candidate| candidate.rtt_ms)
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
