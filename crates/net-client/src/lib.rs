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
use net_rooms::protocol::{ClientMsg, DataEndpoint, ServerMsg, JOIN_ID, UNSEQUENCED};
use net_transport::{link::MAX_RTO, quic, Link};
use parking_lot::Mutex;
use rustls::pki_types::CertificateDer;
use std::collections::VecDeque;
use std::future::Future;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::Connector;
use tokio_util::sync::CancellationToken;

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
    /// The node closed the control channel: the code and the reason of its close.
    #[error("disconnected: {0}")]
    Closed(String),
    /// A control message of this many bytes, over the most the node reads:
    /// [`MAX_CONTROL_MESSAGE`], or [`MAX_CONTROL_MESSAGE_BEFORE_AUTH`] for the sign-in.
    #[error("control message of {0} bytes: at most {1}")]
    TooLarge(usize, usize),
    #[error("timeout")]
    Timeout,
    /// The node took the client out of the room before the join had entered it: the reason of
    /// its `left`.
    #[error("left: {0}")]
    Left(String),
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
    /// Certificate authorities trusted for an `https://` node beside the public ones: the one
    /// of a node behind a private CA.
    pub roots: Vec<CertificateDer<'static>>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            tunnel: false,
            connect_timeout: Duration::from_secs(10),
            tick: Duration::from_millis(5),
            roots: Vec::new(),
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
    /// The reliable events the node had not acknowledged when the room was lost, in the order
    /// they were sent. Comes before `Closed` and before the `Left` of a room the node took the
    /// client out of or [`Client::leave`] left without their acknowledgement. The room may
    /// have got any of them.
    Undelivered(Vec<EventFrame>),
    Closed(String),
}

/// The mark of an event among the messages of a link.
const EVENT: u32 = 1;

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

/// Idle timeout of the data connection, and of the link on it.
const IDLE_TIMEOUT: Duration = Duration::from_secs(15);
/// How long a leave waits for the node to acknowledge what the game sent it reliably: by then
/// the link has sent every message again at least once, and the answer has had time to come.
pub const LEAVE_WAIT: Duration = MAX_RTO.saturating_mul(2);
/// The code the node closes a data connection with when it gave the link up.
const LINK_DEAD_CLOSE: u32 = 4;
/// The largest control message the node reads, in bytes of its JSON, and the largest it reads
/// from a client that has not signed in, the `auth` with the grant: it closes the control
/// channel on a larger one.
pub use net_common::limits::{MAX_CONTROL_MESSAGE, MAX_CONTROL_MESSAGE_BEFORE_AUTH};
const PING_EVERY: Duration = Duration::from_secs(1);
const TICKET_RESEND: Duration = Duration::from_millis(100);
/// The ids the client gives its own joins and leaves start here; the ids a game gives its
/// requests (`stats`, `find_friends`, `rpc`) stay below.
pub const OWN_IDS: u32 = 1 << 31;

pub struct Client {
    ctl: mpsc::Sender<Message>,
    /// Set once the control channel is closed: the code and the reason of the node's close, when
    /// it gave them.
    closed_with: Arc<OnceLock<Option<String>>>,
    events: mpsc::Receiver<Event>,
    events_tx: mpsc::Sender<Event>,
    /// Events handed out before anything newer: what the node said while a call waited for its
    /// own answer, then the events the room kept for late joiners.
    replay: VecDeque<Event>,
    /// One lost connection is reported once, whichever of the link, the data path and the
    /// control channel notices it first.
    lost: bool,
    /// The loss that ended a call, until the game is told of it.
    unheard: Option<Event>,
    data: Option<Arc<DataPath>>,
    /// The node takes the unsequenced channel (`hello.supports`).
    unsequenced: bool,
    /// Who the grant says this client is, as the node answered the sign-in.
    player_id: String,
    player_name: String,
    opts: Options,
    pub joined: Option<Joined>,
    /// Sync rooms: the entity the room made for this client as its avatar, once the room has
    /// announced it. Kept from the join on, whether or not the game reads the frame.
    pub avatar: Option<u64>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    /// Tasks of the current data path; they end with it.
    data_tasks: Vec<tokio::task::JoinHandle<()>>,
    /// Cancelled when the client closes the current data path.
    data_stop: CancellationToken,
    /// The node repeats the `id` of a join and of a leave in its answer (`hello.supports`).
    join_ids: bool,
    /// The last id of [`OWN_IDS`] this client gave a request.
    asked: u32,
    /// Leaves a node without [`JOIN_ID`] has not answered yet. It answers in the order it was
    /// asked, so what comes before the answer to a leave was asked before that leave.
    leaves_unanswered: u32,
    /// The room of the `joined` a join has in hand, while it opens the data path.
    entering: Option<String>,
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
    pub max_players: u16,
    pub props: serde_json::Map<String, serde_json::Value>,
    /// Who was in the room at the join, this client among them.
    pub players: Vec<protocol::PlayerInfo>,
    pub tick_hz: Option<u16>,
    pub schema_hash: Option<String>,
    /// Lockstep rooms: the server tick the client enters at.
    pub tick: Option<u32>,
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

/// TLS of the control channel of an `https://` node: the public authorities `reqwest` trusts for
/// the HTTP calls, and those the game adds.
fn tls(roots: &[CertificateDer<'static>]) -> Result<Connector> {
    let mut trusted = rustls::RootCertStore::empty();
    trusted.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    for root in roots {
        trusted
            .add(root.clone())
            .map_err(|error| ClientError::Ws(error.to_string()))?;
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|error| ClientError::Ws(error.to_string()))?
        .with_root_certificates(trusted)
        .with_no_client_auth();
    Ok(Connector::Rustls(Arc::new(config)))
}

impl Client {
    /// Opens the control channel and authenticates with a grant.
    pub async fn connect(http_url: &str, grant: &str, opts: Options) -> Result<Client> {
        let ws_url = http_url.replacen("http", "ws", 1) + "/ws";
        let connector = tls(&opts.roots)?;
        let (ws, _) = tokio::time::timeout(
            opts.connect_timeout,
            tokio_tungstenite::connect_async_tls_with_config(&ws_url, None, false, Some(connector)),
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
        let closed_with = Arc::new(OnceLock::new());
        let told = closed_with.clone();
        let mut client = Client {
            ctl,
            closed_with,
            events,
            replay: VecDeque::new(),
            lost: false,
            unheard: None,
            events_tx: events_tx.clone(),
            data: None,
            unsequenced: false,
            player_id: String::new(),
            player_name: String::new(),
            opts,
            joined: None,
            avatar: None,
            tasks: vec![writer],
            data_tasks: Vec::new(),
            data_stop: CancellationToken::new(),
            join_ids: false,
            asked: 0,
            leaves_unanswered: 0,
            entering: None,
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
                            // the node gave the data path up and will take nothing more over it
                            let given_up = matches!(&msg, ServerMsg::Error { code, .. } if code == "transport");
                            if events_tx.send(Event::Control(msg)).await.is_err() {
                                break;
                            }
                            if given_up
                                && events_tx
                                    .send(Event::Closed("link dead".into()))
                                    .await
                                    .is_err()
                            {
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
                    Ok(Message::Close(Some(frame))) => {
                        let said = format!("{} {}", u16::from(frame.code), frame.reason);
                        let said = said.trim_end();
                        let _ = told.set(Some(said.to_string()));
                        let _ = events_tx
                            .send(Event::Closed(format!("control channel closed: {said}")))
                            .await;
                        break;
                    }
                    Ok(Message::Close(None)) | Err(_) => {
                        let _ = told.set(None);
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
        let mut passed = VecDeque::new();
        loop {
            match client.answer_by(deadline).await? {
                Event::Control(ServerMsg::Authed { player_id, name }) => {
                    client.player_id = player_id;
                    client.player_name = name;
                    break;
                }
                Event::Control(ServerMsg::Hello { supports, .. }) => {
                    client.unsequenced = supports.iter().any(|s| s == UNSEQUENCED);
                    client.join_ids = supports.iter().any(|s| s == JOIN_ID);
                }
                Event::Control(ServerMsg::Error { code, message, .. }) => {
                    return Err(ClientError::Server { code, message })
                }
                event => passed.push_back(event),
            }
        }
        client.replay = passed;
        Ok(client)
    }

    /// The player id of the grant this client signed in with: the id the node knows the player
    /// by in rooms, held seats and webhooks.
    pub fn player_id(&self) -> &str {
        &self.player_id
    }

    /// The display name of the grant this client signed in with.
    pub fn player_name(&self) -> &str {
        &self.player_name
    }

    /// Sends a control message.
    pub async fn send(&self, msg: ClientMsg) -> Result<()> {
        let txt =
            serde_json::to_string(&msg).map_err(|error| ClientError::Ws(error.to_string()))?;
        let most = match msg {
            ClientMsg::Auth { .. } => MAX_CONTROL_MESSAGE_BEFORE_AUTH,
            _ => MAX_CONTROL_MESSAGE,
        };
        if txt.len() > most {
            return Err(ClientError::TooLarge(txt.len(), most));
        }
        if self.closed_with.get().is_some() {
            return Err(self.gone());
        }
        self.ctl
            .send(Message::Text(txt.into()))
            .await
            .map_err(|_| self.gone())
    }

    /// Why a call cannot reach the node any more.
    fn gone(&self) -> ClientError {
        match self.closed_with.get() {
            Some(Some(said)) => ClientError::Closed(said.clone()),
            _ => ClientError::Disconnected,
        }
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
        let Some(msg) = self.last_join.clone() else {
            return Err(ClientError::Disconnected);
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
        let joined = self.enter(msg).await;
        let room = self.entering.take();
        match joined {
            Ok(_) | Err(ClientError::Server { .. }) => joined,
            // the node took the client out itself
            Err(ClientError::Left(_)) => {
                self.end_data().await;
                joined
            }
            // the node may have let the client in, or may yet: the client is in no room after this
            Err(error) => Err(self.give_up(error, room).await),
        }
    }

    /// Leaves after a join that failed with `error`. When the node had let the client into
    /// `room`, the game hears nothing of that room: not the answer to this leave, awaited no
    /// longer than [`LEAVE_WAIT`], and not a kick the node sent before it. The join then fails
    /// with the reason of the kick.
    async fn give_up(&mut self, mut error: ClientError, room: Option<String>) -> ClientError {
        let (Ok(Some(id)), Some(room)) = (self.ask_to_leave().await, room) else {
            return error;
        };
        let deadline = tokio::time::Instant::now() + LEAVE_WAIT;
        let mut passed = VecDeque::new();
        while let Ok(Some(event)) = tokio::time::timeout_at(deadline, self.next(false)).await {
            match event {
                Event::Control(ServerMsg::Left { id: answered, .. }) if answered == Some(id) => {
                    break
                }
                Event::Control(ServerMsg::Left {
                    reason,
                    room: Some(left),
                    ..
                }) if reason != "left" && left == room => error = ClientError::Left(reason),
                event => passed.push_back(event),
            }
        }
        self.keep(passed);
        error
    }

    async fn enter(&mut self, msg: ClientMsg) -> Result<Joined> {
        let asked = msg.clone();
        // what the room before this one sent is not the new room's
        self.replay
            .retain(|event| !matches!(event, Event::Data { .. }));
        self.lost = false;
        let id = self.own_id();
        self.send(with_id(msg, id)).await?;
        // the loss that ended a join before this one is of the stay the game gave up
        self.unheard = None;
        let deadline = tokio::time::Instant::now() + self.opts.connect_timeout;
        let mut passed = VecDeque::new();
        let answer = loop {
            match self.answer_by(deadline).await {
                // the answer to a join the client gave up: it left right after
                Ok(Event::Control(
                    ServerMsg::Joined { id: answered, .. } | ServerMsg::Error { id: answered, .. },
                )) if self.gave_up(id, answered) => {}
                Ok(Event::Control(ServerMsg::Joined {
                    room,
                    name,
                    mode,
                    actor,
                    master,
                    max_players,
                    props,
                    players,
                    data,
                    tick_hz,
                    schema_hash,
                    tick,
                    seed,
                    next_spawn_tag,
                    cached,
                    id: answered,
                })) if answered == id => {
                    let joined = Joined {
                        room,
                        name,
                        mode,
                        actor,
                        master,
                        max_players,
                        props,
                        players,
                        tick_hz,
                        schema_hash,
                        tick,
                        seed,
                        next_spawn_tag,
                        data,
                    };
                    break Ok((joined, cached));
                }
                Ok(Event::Control(ServerMsg::Error {
                    code,
                    message,
                    id: answered,
                })) if id.is_none() || answered.is_none() || answered == id => {
                    break Err(ClientError::Server { code, message })
                }
                Ok(Event::Data { .. } | Event::DataReady) => {}
                Ok(event) => passed.push_back(event),
                Err(error) => break Err(error),
            }
        };
        self.keep(passed);
        let (joined, cached) = answer?;
        self.entering = Some(joined.room.clone());
        self.last_join = Some(way_back(&asked, &joined.name));
        // the room announces the avatar as soon as the data path is up
        self.avatar = None;
        self.open_data(&joined.data, &joined.room).await?;
        let cached = cached.into_iter().filter_map(|(code, from, data)| {
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
        });
        self.replay.extend(cached);
        self.joined = Some(joined.clone());
        Ok(joined)
    }

    /// Stops the data path of the room the client is leaving or has lost. Left running, it would
    /// go on resending what the room has not acknowledged, to a node that no longer expects it.
    /// The tasks of the path are told to stop first, so its end is not reported as a loss.
    fn close_data(&mut self) {
        self.data_stop.cancel();
        self.data_tasks.clear();
        self.drop_data();
    }

    /// `close_data` where the client can wait: the tasks of the path have ended when this
    /// returns, and the connection is closed after them.
    async fn end_data(&mut self) {
        self.data_stop.cancel();
        for task in self.data_tasks.drain(..) {
            let _ = task.await;
        }
        self.drop_data();
    }

    fn drop_data(&mut self) {
        if let Some(slot) = &self.tunnel_slot {
            *slot.lock() = None;
        }
        if let Some(dp) = self.data.take() {
            if let Sink::Quic(conn) = &dp.sink {
                conn.close(0u32.into(), b"bye");
            }
        }
    }

    /// Opens the data path of `room`, the room a join is entering. A `left` by which the node
    /// takes the client out of that room meanwhile ends the join with its reason, and is not
    /// the game's: it was never in the room.
    async fn open_data(&mut self, ep: &DataEndpoint, room: &str) -> Result<()> {
        self.end_data().await;
        self.data_stop = CancellationToken::new();
        let ticket = base64::engine::general_purpose::STANDARD
            .decode(&ep.ticket)
            .map_err(|error| ClientError::Data(error.to_string()))?;
        let link = Mutex::new(Link::new(Instant::now(), IDLE_TIMEOUT));
        let dp: Arc<DataPath> = if self.opts.tunnel {
            let (ws_tx, mut ws_rx) = mpsc::channel::<Message>(1024);
            let (ctl, stop) = (self.ctl.clone(), self.data_stop.clone());
            self.data_tasks.push(tokio::spawn(async move {
                let forward = async {
                    while let Some(message) = ws_rx.recv().await {
                        if ctl.send(message).await.is_err() {
                            break;
                        }
                    }
                };
                stop.run_until_cancelled(forward).await;
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
            let transport = quic::transport_config(IDLE_TIMEOUT, Duration::from_secs(5));
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
            let reading = async move {
                // the ticket is a datagram: it is repeated until the node's first one arrives
                let start = tokio::time::Instant::now() + TICKET_RESEND;
                let mut again = tokio::time::interval_at(start, TICKET_RESEND);
                let mut answered = false;
                loop {
                    tokio::select! {
                        datagram = conn.read_datagram() => {
                            match datagram {
                                Ok(datagram) => {
                                    answered = true;
                                    deliver(&dp2, datagram, &ev).await;
                                }
                                Err(quinn::ConnectionError::ApplicationClosed(close))
                                    if close.error_code == LINK_DEAD_CLOSE.into() =>
                                {
                                    break "link dead";
                                }
                                Err(_) => break "data path closed",
                            }
                        }
                        _ = again.tick(), if !answered => {
                            let _ = conn.send_datagram(ticket.clone());
                        }
                    }
                }
            };
            self.data_task(reading);
            dp
        };
        let (dp2, tick) = (dp.clone(), self.opts.tick);
        self.data_task(async move {
            let mut iv = tokio::time::interval(tick);
            loop {
                iv.tick().await;
                if dp2.link.lock().poll(Instant::now()).is_err() {
                    break "link dead";
                }
                dp2.flush();
            }
        });
        self.data = Some(dp);
        // wait for the server's empty ack frame
        let deadline = tokio::time::Instant::now() + self.opts.connect_timeout;
        let mut passed = VecDeque::new();
        let ready = loop {
            match self.answer_by(deadline).await {
                Ok(Event::DataReady) => break Ok(()),
                Ok(Event::Control(ServerMsg::Left {
                    reason,
                    room: Some(left),
                    ..
                })) if reason != "left" && left == room => break Err(ClientError::Left(reason)),
                Ok(event) => passed.push_back(event),
                Err(error) => break Err(error),
            }
        };
        self.keep(passed);
        ready
    }

    /// Runs a task of the current data path until the client closes the path. `task` ends
    /// when the path closes by itself, with the reason the game is told.
    fn data_task(&mut self, task: impl Future<Output = &'static str> + Send + 'static) {
        let (stop, events) = (self.data_stop.clone(), self.events_tx.clone());
        self.data_tasks.push(tokio::spawn(async move {
            let report = async {
                let reason = task.await;
                // a path the client closes may end before its task sees the stop
                if !stop.is_cancelled() {
                    let _ = events.send(Event::Closed(reason.into())).await;
                }
            };
            stop.run_until_cancelled(report).await;
        }));
    }

    /// Hands the game what a call read while it waited for its own answer, in the order it came
    /// and before anything newer.
    fn keep(&mut self, mut passed: VecDeque<Event>) {
        passed.append(&mut self.replay);
        self.replay = passed;
    }

    /// The next event while a call waits for the node's answer: the wait ends with the
    /// connection and at `deadline`. The loss that ends it is the game's to hear as well.
    async fn answer_by(&mut self, deadline: tokio::time::Instant) -> Result<Event> {
        match tokio::time::timeout_at(deadline, self.next(false)).await {
            Ok(Some(Event::Closed(reason))) => {
                self.unheard = Some(Event::Closed(reason));
                Err(self.gone())
            }
            Ok(None) => Err(self.gone()),
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
        self.queue(channel, payload, 0)
    }

    fn queue(&self, channel: Channel, payload: Bytes, mark: u32) -> Result<()> {
        let dp = self.data.as_ref().ok_or(ClientError::Disconnected)?;
        let channel = match channel {
            Channel::UnreliableUnsequenced if !self.unsequenced => Channel::UnreliableSequenced,
            channel => channel,
        };
        dp.link
            .lock()
            .send_marked(channel, payload, Instant::now(), mark)
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
        self.queue(channel, frame.encode(), EVENT)
    }

    pub async fn next_event(&mut self) -> Option<Event> {
        self.next(true).await
    }

    /// The next event: for the game, or for a call of the client that waits for its answer.
    /// The loss that ended a call is the game's alone, after what the call read before it.
    async fn next(&mut self, for_game: bool) -> Option<Event> {
        loop {
            let event = match self.replay.pop_front() {
                Some(event) => event,
                None => {
                    if for_game && self.unheard.is_some() {
                        return self.heard();
                    }
                    let event = self.events.recv().await?;
                    self.noted(event)
                }
            };
            if self.reports(&event) {
                return Some(event);
            }
        }
    }

    fn heard(&mut self) -> Option<Event> {
        self.lost = true;
        self.unheard.take()
    }

    /// Non-blocking poll for engines with their own loop.
    pub fn try_next_event(&mut self) -> Option<Event> {
        loop {
            let event = match self.replay.pop_front() {
                Some(event) => event,
                None => {
                    if self.unheard.is_some() {
                        return self.heard();
                    }
                    let event = self.events.try_recv().ok()?;
                    self.noted(event)
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
    fn forget_room_on_left(&mut self, reason: &str, room: Option<&String>) -> Option<Event> {
        let current = self.joined.as_ref().map(|joined| &joined.room);
        if reason != "left" && current.is_some() && current == room {
            self.joined = None;
            self.avatar = None;
            return self.lose_data();
        }
        None
    }

    /// The id of the next join or leave, for a node that repeats it in its answer.
    fn own_id(&mut self) -> Option<u32> {
        self.join_ids.then(|| {
            self.asked = (self.asked + 1) % OWN_IDS;
            OWN_IDS | self.asked
        })
    }

    /// Whether `answered` is on the answer to a join the client gave up, and not to the one it
    /// asked with `asked`.
    fn gave_up(&self, asked: Option<u32>, answered: Option<u32>) -> bool {
        match asked {
            Some(asked) => answered.is_some_and(|id| id >= OWN_IDS && id != asked),
            None => answered.is_none() && self.leaves_unanswered > 0,
        }
    }

    /// Closes a data path the game did not close; the reliable events the node had not
    /// acknowledged on it are for the game.
    fn lose_data(&mut self) -> Option<Event> {
        let events = self.unacknowledged();
        self.close_data();
        events
    }

    /// The reliable events of the room the node has not acknowledged, as the game is handed them.
    fn unacknowledged(&self) -> Option<Event> {
        let events: Vec<_> = self
            .data
            .iter()
            .flat_map(|path| path.link.lock().unacknowledged_messages())
            .filter(|message| message.mark == EVENT)
            .filter_map(|message| EventFrame::decode(message.msg).ok())
            .collect();
        (!events.is_empty()).then_some(Event::Undelivered(events))
    }

    /// What the client keeps of an event whether or not the game reads it. Returns what the
    /// game is told before the event.
    fn note(&mut self, event: &Event) -> Option<Event> {
        match event {
            Event::Control(ServerMsg::Left { reason, room, .. }) => {
                if reason == "left" {
                    self.leaves_unanswered = self.leaves_unanswered.saturating_sub(1);
                }
                self.forget_room_on_left(reason, room.as_ref())
            }
            Event::Data { payload, .. } => {
                if let Some(entity) = announced_avatar(payload) {
                    self.avatar = Some(entity);
                }
                None
            }
            Event::Closed(_) => self.lose_data(),
            _ => None,
        }
    }

    /// An event as it came from the connection, after what [`Self::note`] puts before it.
    fn noted(&mut self, event: Event) -> Event {
        match self.note(&event) {
            Some(first) => {
                self.replay.push_front(event);
                first
            }
            None => event,
        }
    }

    /// A loss is told once, and what a room sent is not the game's once it is out of the room.
    fn reports(&mut self, event: &Event) -> bool {
        match event {
            Event::Closed(_) => !std::mem::replace(&mut self.lost, true),
            Event::Data { .. } | Event::DataReady => {
                self.joined.is_some() || self.entering.is_some()
            }
            _ => true,
        }
    }

    /// Smoothed round trip of the control channel in milliseconds; 0 until the first sample.
    pub fn rtt_ms(&self) -> u32 {
        self.rtt_ms.load(Ordering::Relaxed)
    }

    pub fn link_stats(&self) -> Option<net_transport::LinkStats> {
        self.data.as_ref().map(|path| path.link.lock().stats())
    }

    /// Waits until the node has acknowledged the reliable messages of the room, so that a
    /// leave does not cut off what the game sent before it, and no longer than [`LEAVE_WAIT`].
    /// What the room sends meanwhile is kept for the game; a path that is lost ends the wait.
    async fn acknowledged(&mut self) {
        let end = tokio::time::sleep(LEAVE_WAIT);
        tokio::pin!(end);
        let mut again = tokio::time::interval(self.opts.tick);
        while self
            .data
            .as_ref()
            .is_some_and(|path| !path.link.lock().unacknowledged_messages().is_empty())
        {
            tokio::select! {
                _ = &mut end => break,
                _ = again.tick() => {}
                event = self.events.recv() => {
                    let Some(event) = event else { break };
                    let first = self.note(&event);
                    self.replay.extend(first);
                    self.replay.push_back(event);
                }
            }
        }
    }

    /// Leaves the room once the node has acknowledged what the game sent it reliably, or after
    /// [`LEAVE_WAIT`]: the events it has not acknowledged by then come as [`Event::Undelivered`]
    /// before the `left`.
    pub async fn leave(&mut self) -> Result<()> {
        self.ask_to_leave().await.map(drop)
    }

    /// Returns the id of the `leave`, which a node that repeats ids puts on its answer.
    async fn ask_to_leave(&mut self) -> Result<Option<u32>> {
        self.acknowledged().await;
        let unacknowledged = self.unacknowledged();
        self.replay.extend(unacknowledged);
        self.end_data().await;
        self.joined = None;
        self.avatar = None;
        let id = self.own_id();
        self.send(ClientMsg::Leave { id }).await?;
        if id.is_none() {
            self.leaves_unanswered += 1;
        }
        Ok(id)
    }

    /// Ask for project-wide room/player counts per lobby; the answer arrives as
    /// `Event::Control(ServerMsg::Stats { id, .. })`, a refusal as `ServerMsg::Error { id, .. }`,
    /// both with the `id` given here, which stays below [`OWN_IDS`]. At most one
    /// `stats`/`find_friends` per second.
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

/// The join that brings a client back into the room `asked` led to: the room by its name, as
/// the player that entered it. A random join or a create asked again would lead elsewhere.
fn way_back(asked: &ClientMsg, name: &str) -> ClientMsg {
    let (player_props, schema_hash, spectator) = match asked {
        ClientMsg::Join {
            player_props,
            schema_hash,
            spectator,
            ..
        } => (player_props.clone(), schema_hash.clone(), *spectator),
        ClientMsg::Create { player_props, .. }
        | ClientMsg::JoinOrCreate { player_props, .. }
        | ClientMsg::JoinRandom { player_props, .. } => (player_props.clone(), None, false),
        _ => (Default::default(), None, false),
    };
    ClientMsg::Join {
        name: name.to_owned(),
        player_props,
        rejoin_actor: None,
        schema_hash,
        spectator,
        id: None,
    }
}

/// `msg` with `id` if it is a join: the client numbers its joins itself.
fn with_id(mut msg: ClientMsg, asked: Option<u32>) -> ClientMsg {
    if let ClientMsg::Create { id, .. }
    | ClientMsg::Join { id, .. }
    | ClientMsg::JoinOrCreate { id, .. }
    | ClientMsg::JoinRandom { id, .. } = &mut msg
    {
        *id = asked;
    }
    msg
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_way_back_into_a_room_is_a_join_by_its_name() {
        let props: serde_json::Map<String, serde_json::Value> =
            [("team".to_owned(), "red".into())].into_iter().collect();
        let random = ClientMsg::JoinRandom {
            filter: [("map".to_owned(), "dust".into())].into_iter().collect(),
            player_props: props.clone(),
            expr: Some("rank > 3".into()),
            match_type: Some("fill".into()),
            id: None,
        };
        let watched = ClientMsg::Join {
            name: "asked".into(),
            player_props: props.clone(),
            rejoin_actor: None,
            schema_hash: Some("abc".into()),
            spectator: true,
            id: None,
        };
        assert_eq!(
            serde_json::to_value(way_back(&random, "found")).unwrap(),
            serde_json::json!({"t": "join", "name": "found", "player_props": {"team": "red"},
                "rejoin_actor": null, "schema_hash": null, "spectator": false})
        );
        assert_eq!(
            serde_json::to_value(way_back(&watched, "asked")).unwrap(),
            serde_json::to_value(&watched).unwrap()
        );
    }
}
