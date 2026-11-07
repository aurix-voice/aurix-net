//! Rust client for Aurix Net: control channel over WebSocket, data path over QUIC (pinned
//! certificate) or the WebSocket tunnel, with the same `Link` channels the server uses.
//! Engine SDKs and the C ABI are thin layers over this crate.

use base64::Engine;
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use net_common::{Channel, RoomMode};
use net_rooms::protocol::{ClientMsg, DataEndpoint, ServerMsg};
use net_transport::{quic, Link};
use parking_lot::Mutex;
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
                    let _ = tx.try_send(Message::Binary(d.to_vec()));
                }
            }
        }
    }
}

pub struct Client {
    ctl: mpsc::Sender<Message>,
    events: mpsc::Receiver<Event>,
    events_tx: mpsc::Sender<Event>,
    data: Option<Arc<DataPath>>,
    opts: Options,
    pub joined: Option<Joined>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    tunnel_slot: Option<Arc<Mutex<Option<Arc<DataPath>>>>>,
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
            events_tx: events_tx.clone(),
            data: None,
            opts,
            joined: None,
            tasks: vec![writer],
            tunnel_slot: None,
        };
        // Reader: text -> Control events, binary -> tunnel datagrams (routed via data path if any).
        let tunnel_in: Arc<Mutex<Option<Arc<DataPath>>>> = Arc::new(Mutex::new(None));
        let tunnel_slot = tunnel_in.clone();
        let reader = tokio::spawn(async move {
            while let Some(m) = rx.next().await {
                match m {
                    Ok(Message::Text(t)) => match serde_json::from_str::<ServerMsg>(&t) {
                        Ok(msg) => {
                            if events_tx.send(Event::Control(msg)).await.is_err() {
                                break;
                            }
                        }
                        Err(e) => tracing::warn!(error = %e, "bad control message"),
                    },
                    Ok(Message::Binary(b)) => {
                        let dp = tunnel_slot.lock().clone();
                        if let Some(dp) = dp {
                            deliver(&dp, Bytes::from(b), &events_tx).await;
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
            .send(Message::Text(txt))
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
    pub async fn join(&mut self, msg: ClientMsg) -> Result<Joined> {
        self.send(msg).await?;
        let joined = loop {
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
                    ..
                })) => {
                    break Joined {
                        room,
                        name,
                        mode,
                        actor,
                        master,
                        tick_hz,
                        schema_hash,
                        data,
                    };
                }
                Some(Event::Control(ServerMsg::Error { code, message })) => {
                    return Err(ClientError::Server { code, message })
                }
                Some(Event::Closed(_)) | None => return Err(ClientError::Disconnected),
                _ => continue,
            }
        };
        self.open_data(&joined.data).await?;
        self.joined = Some(joined.clone());
        Ok(joined)
    }

    async fn open_data(&mut self, ep: &DataEndpoint) -> Result<()> {
        let ticket = base64::engine::general_purpose::STANDARD
            .decode(&ep.ticket)
            .map_err(|e| ClientError::Data(e.to_string()))?;
        let link = Mutex::new(Link::new(Instant::now()));
        let dp: Arc<DataPath> = if self.opts.tunnel {
            let (ws_tx, mut ws_rx) = mpsc::channel::<Message>(1024);
            let ctl = self.ctl.clone();
            self.tasks.push(tokio::spawn(async move {
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
                .send(Message::Binary(ticket))
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
            self.tasks.push(tokio::spawn(async move {
                while let Ok(d) = conn.read_datagram().await {
                    deliver(&dp2, d, &ev).await;
                }
                let _ = ev.send(Event::Closed("data path closed".into())).await;
            }));
            dp
        };
        // link timer
        let (dp2, tick) = (dp.clone(), self.opts.tick);
        self.tasks.push(tokio::spawn(async move {
            let mut iv = tokio::time::interval(tick);
            loop {
                iv.tick().await;
                if dp2.link.lock().poll(Instant::now()).is_err() {
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

    /// Relay helper: raise an event.
    pub fn send_event(
        &self,
        code: u8,
        target: EventTarget,
        payload: Bytes,
        reliable: bool,
    ) -> Result<()> {
        let frame = EventFrame {
            code,
            target,
            cache: CacheOp::None,
            group: None,
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
        self.events.recv().await
    }

    /// Non-blocking poll for engines with their own loop.
    pub fn try_next_event(&mut self) -> Option<Event> {
        self.events.try_recv().ok()
    }

    pub fn link_stats(&self) -> Option<net_transport::LinkStats> {
        self.data.as_ref().map(|d| d.link.lock().stats())
    }

    pub async fn leave(&mut self) -> Result<()> {
        self.send(ClientMsg::Leave).await
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
        if let Some(dp) = &self.data {
            if let Sink::Quic(c) = &dp.sink {
                c.close(0u32.into(), b"bye");
            }
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
