//! Rooms hosted on this node and the sessions attached to them.

use crate::metrics::Metrics;
use bytes::Bytes;
use dashmap::DashMap;
use net_common::{ActorId, Channel, NetError, ProjectId, Result, RoomId};
use net_rooms::protocol::ServerMsg;
use net_rooms::{EventFrame, Outgoing, Room};
use net_transport::Link;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;

/// Where a session's datagrams go.
#[derive(Clone)]
pub enum DataSink {
    Quic(quinn::Connection),
    /// WebTransport session (browsers): HTTP/3 datagrams over QUIC.
    WebTransport(wtransport::Connection),
    /// Binary WebSocket frames (tunnel mode).
    Ws(mpsc::Sender<Bytes>),
}

/// Per project: bytes (in, out) of closed data sessions the metering task has not billed yet.
pub type Unmetered = Arc<DashMap<ProjectId, (u64, u64)>>;

pub struct DataSession {
    pub link: Mutex<Link>,
    pub sink: DataSink,
    pub metrics: Arc<Metrics>,
    /// Byte counters already billed by the metering task (see `metering.rs`).
    pub metered_in: std::sync::atomic::AtomicU64,
    pub metered_out: std::sync::atomic::AtomicU64,
    project: ProjectId,
    unmetered: Unmetered,
}

impl Drop for DataSession {
    fn drop(&mut self) {
        let stats = self.link.get_mut().stats();
        let bytes_in = stats.recv_bytes.saturating_sub(*self.metered_in.get_mut());
        let bytes_out = stats.sent_bytes.saturating_sub(*self.metered_out.get_mut());
        if bytes_in > 0 || bytes_out > 0 {
            let mut left = self.unmetered.entry(self.project).or_default();
            left.0 += bytes_in;
            left.1 += bytes_out;
        }
    }
}

impl DataSession {
    fn new(
        project: ProjectId,
        sink: DataSink,
        metrics: Arc<Metrics>,
        unmetered: Unmetered,
    ) -> Arc<Self> {
        Arc::new(Self {
            link: Mutex::new(Link::new(Instant::now())),
            sink,
            metrics,
            metered_in: Default::default(),
            metered_out: Default::default(),
            project,
            unmetered,
        })
    }

    /// Queues a message and flushes everything the link produced.
    pub fn send(&self, channel: Channel, payload: Bytes) {
        let out: Vec<Bytes> = {
            let mut link = self.link.lock();
            if link.send(channel, payload, Instant::now()).is_err() {
                return;
            }
            // Unreliable messages are coalesced and leave on the next flusher pass.
            if channel == Channel::UnreliableSequenced {
                return;
            }
            std::iter::from_fn(|| link.next_outgoing()).collect()
        };
        self.flush(out);
    }

    pub fn flush(&self, out: Vec<Bytes>) {
        for datagram in out {
            self.metrics.datagrams_out.inc();
            self.metrics.bytes_out.inc_by(datagram.len() as u64);
            let ok = match &self.sink {
                DataSink::Quic(conn) => match conn.send_datagram(datagram) {
                    Ok(()) => true,
                    Err(quinn::SendDatagramError::TooLarge) => {
                        tracing::warn!(max = ?conn.max_datagram_size(), "datagram too large for path");
                        false
                    }
                    Err(_) => false,
                },
                DataSink::WebTransport(conn) => conn.send_datagram(datagram).is_ok(),
                DataSink::Ws(tx) => tx.try_send(datagram).is_ok(),
            };
            if !ok {
                self.metrics
                    .errors
                    .with_label_values(&["datagram_send"])
                    .inc();
            }
        }
    }

    /// Feeds a datagram; returns decoded application messages.
    pub fn receive(&self, datagram: Bytes) -> Vec<net_transport::Received> {
        self.metrics.datagrams_in.inc();
        self.metrics.bytes_in.inc_by(datagram.len() as u64);
        let (recv, out) = {
            let mut link = self.link.lock();
            let received = link.receive(datagram, Instant::now());
            let _ = link.poll(Instant::now());
            (
                received,
                std::iter::from_fn(|| link.next_outgoing()).collect::<Vec<_>>(),
            )
        };
        self.flush(out);
        recv
    }

    /// Timer-driven retransmits/acks. Returns false when the link is dead.
    pub fn tick(&self) -> bool {
        let (alive, out) = {
            let mut link = self.link.lock();
            let alive = link.poll(Instant::now()).is_ok();
            (
                alive,
                std::iter::from_fn(|| link.next_outgoing()).collect::<Vec<_>>(),
            )
        };
        self.flush(out);
        alive
    }
}

pub struct Attached {
    pub control: mpsc::Sender<ServerMsg>,
    /// Control session owning this slot; a newer session of the same player replaces it.
    pub session: net_common::SessionId,
    pub data: Option<Arc<DataSession>>,
}

pub struct HostedRoom {
    pub room: Room,
    pub sessions: HashMap<ActorId, Attached>,
    pub project: ProjectId,
    /// Present for rooms in sync mode.
    pub sync: Option<crate::sync::SyncRoom>,
    /// Present for rooms in lockstep mode.
    pub lockstep: Option<crate::lockstep::LockstepRoom>,
}

pub const FLUSH_EVERY: std::time::Duration = std::time::Duration::from_millis(2);

#[derive(Default)]
pub struct RoomHost {
    /// Every live data session; the flusher drives retransmits, acks and batches.
    data_sessions: Mutex<Vec<std::sync::Weak<DataSession>>>,
    rooms: DashMap<RoomId, Arc<Mutex<HostedRoom>>>,
    by_name: DashMap<(ProjectId, String), RoomId>,
    /// Lobby subscribers per project on this node.
    lobby: DashMap<ProjectId, Vec<mpsc::Sender<ServerMsg>>>,
    /// Authenticated control sessions per (project, player): presence for `find_friends`.
    presence: DashMap<(ProjectId, String), u32>,
    unmetered: Unmetered,
}

impl RoomHost {
    /// A data session whose traffic is billed to `project`.
    pub fn data_session(
        &self,
        project: ProjectId,
        sink: DataSink,
        metrics: Arc<Metrics>,
    ) -> Arc<DataSession> {
        DataSession::new(project, sink, metrics, self.unmetered.clone())
    }

    pub fn register_data(&self, ds: &Arc<DataSession>) {
        self.data_sessions.lock().push(Arc::downgrade(ds));
    }

    /// One pass: poll every live link and push its datagrams. Returns dead links.
    pub fn flush_all(&self) -> usize {
        let sessions: Vec<Arc<DataSession>> = {
            let mut sessions = self.data_sessions.lock();
            sessions.retain(|w| w.strong_count() > 1);
            sessions.iter().filter_map(|w| w.upgrade()).collect()
        };
        let mut dead = 0;
        for session in sessions {
            if !session.tick() {
                dead += 1;
                match &session.sink {
                    DataSink::Quic(conn) => conn.close(4u32.into(), b"link dead"),
                    DataSink::WebTransport(conn) => conn.close(4u32.into(), b"link dead"),
                    DataSink::Ws(_) => {}
                }
            }
        }
        dead
    }

    pub fn insert(&self, project: ProjectId, room: Room) -> Arc<Mutex<HostedRoom>> {
        let id = room.id();
        let name = room.cfg.name.clone();
        let handle = Arc::new(Mutex::new(HostedRoom {
            room,
            sessions: HashMap::new(),
            project,
            sync: None,
            lockstep: None,
        }));
        self.rooms.insert(id, handle.clone());
        self.by_name.insert((project, name), id);
        handle
    }

    pub fn get(&self, id: RoomId) -> Option<Arc<Mutex<HostedRoom>>> {
        self.rooms.get(&id).map(|r| r.clone())
    }

    pub fn by_name(&self, project: ProjectId, name: &str) -> Option<Arc<Mutex<HostedRoom>>> {
        let id = *self.by_name.get(&(project, name.to_string()))?;
        self.get(id)
    }

    pub fn remove(&self, id: RoomId) -> Option<Arc<Mutex<HostedRoom>>> {
        let (_, handle) = self.rooms.remove(&id)?;
        let (project, name) = {
            let hosted = handle.lock();
            (hosted.project, hosted.room.cfg.name.clone())
        };
        self.by_name.remove(&(project, name));
        Some(handle)
    }

    /// Per project: (rooms, online players, new bytes in, new bytes out) since the last call.
    pub fn usage(&self) -> HashMap<ProjectId, (u64, u64, u64, u64)> {
        use std::sync::atomic::Ordering;
        let mut out: HashMap<ProjectId, (u64, u64, u64, u64)> = HashMap::new();
        for entry in self.rooms.iter() {
            let hosted = entry.value().lock();
            let totals = out.entry(hosted.project).or_default();
            totals.0 += 1;
            totals.1 += hosted.room.online_count() as u64;
            for session in hosted.sessions.values() {
                if let Some(data) = &session.data {
                    let st = data.link.lock().stats();
                    let din = st
                        .recv_bytes
                        .saturating_sub(data.metered_in.swap(st.recv_bytes, Ordering::Relaxed));
                    let dout = st
                        .sent_bytes
                        .saturating_sub(data.metered_out.swap(st.sent_bytes, Ordering::Relaxed));
                    totals.2 += din;
                    totals.3 += dout;
                }
            }
        }
        let closed: Vec<ProjectId> = self.unmetered.iter().map(|left| *left.key()).collect();
        for project in closed {
            if let Some((_, (bytes_in, bytes_out))) = self.unmetered.remove(&project) {
                let totals = out.entry(project).or_default();
                totals.2 += bytes_in;
                totals.3 += bytes_out;
            }
        }
        out
    }

    /// Rooms hosted here where `player_id` has a slot: (room id, room name, mode, actor, display name, online).
    pub fn find_player(
        &self,
        project: ProjectId,
        player_id: &str,
    ) -> Vec<(RoomId, String, net_common::RoomMode, u16, String, bool)> {
        let mut out = Vec::new();
        for entry in self.rooms.iter() {
            let hosted = entry.value().lock();
            if hosted.project != project {
                continue;
            }
            for player in hosted.room.player_infos() {
                if player.player_id == player_id {
                    out.push((
                        *entry.key(),
                        hosted.room.cfg.name.clone(),
                        hosted.room.cfg.mode,
                        player.actor.0,
                        player.name.clone(),
                        player.online,
                    ));
                }
            }
        }
        out
    }

    pub fn len(&self) -> usize {
        self.rooms.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rooms.is_empty()
    }

    pub fn players(&self) -> usize {
        self.rooms
            .iter()
            .map(|r| r.lock().room.online_count() as usize)
            .sum()
    }

    pub fn all(&self) -> Vec<Arc<Mutex<HostedRoom>>> {
        self.rooms.iter().map(|r| r.clone()).collect()
    }

    pub fn listed(&self, project: ProjectId) -> Vec<net_rooms::protocol::RoomSummary> {
        self.rooms
            .iter()
            .filter_map(|r| {
                let hosted = r.lock();
                (hosted.project == project && hosted.room.is_listed())
                    .then(|| hosted.room.summary())
            })
            .collect()
    }

    pub fn presence_enter(&self, project: ProjectId, player: &str) {
        *self
            .presence
            .entry((project, player.to_string()))
            .or_default() += 1;
    }

    pub fn presence_leave(&self, project: ProjectId, player: &str) {
        let key = (project, player.to_string());
        let gone = match self.presence.get_mut(&key) {
            Some(mut count) => {
                *count = count.saturating_sub(1);
                *count == 0
            }
            None => false,
        };
        if gone {
            self.presence.remove_if(&key, |_, n| *n == 0);
        }
    }

    /// Whether `player` has an authenticated control session on this node right now.
    pub fn is_online(&self, project: ProjectId, player: &str) -> bool {
        self.presence
            .get(&(project, player.to_string()))
            .is_some_and(|n| *n > 0)
    }

    pub fn subscribe_lobby(&self, project: ProjectId, tx: mpsc::Sender<ServerMsg>) {
        let mut watchers = self.lobby.entry(project).or_default();
        if !watchers.iter().any(|watcher| watcher.same_channel(&tx)) {
            watchers.push(tx);
        }
    }

    pub fn unsubscribe_lobby(&self, project: ProjectId, tx: &mpsc::Sender<ServerMsg>) {
        if let Some(mut watchers) = self.lobby.get_mut(&project) {
            watchers.retain(|t| !t.same_channel(tx));
        }
    }

    pub fn notify_lobby(&self, project: ProjectId, msg: ServerMsg) {
        if let Some(mut watchers) = self.lobby.get_mut(&project) {
            watchers.retain(|t| !t.is_closed());
            for watcher in watchers.iter() {
                let _ = watcher.try_send(msg.clone());
            }
        }
    }
}

/// Delivers what the room logic produced.
pub fn dispatch(hosted: &HostedRoom, out: Vec<Outgoing>, metrics: &Metrics) {
    for outgoing in out {
        match outgoing {
            Outgoing::Control(actor, msg) => {
                if let Some(session) = hosted.sessions.get(&actor) {
                    let _ = session.control.try_send(msg);
                }
            }
            Outgoing::Data(actor, channel, frame) => {
                let Some(session) = hosted.sessions.get(&actor) else {
                    continue;
                };
                metrics
                    .events
                    .with_label_values(&[channel_name(channel)])
                    .inc();
                match &session.data {
                    Some(data) => data.send(channel, frame.encode()),
                    None => {
                        // No data path yet: fall back to the control channel.
                        use base64::Engine;
                        let _ = session.control.try_send(ServerMsg::Event {
                            code: frame.code,
                            from: frame.from,
                            data: base64::engine::general_purpose::STANDARD.encode(&frame.payload),
                        });
                    }
                }
            }
        }
    }
}

pub fn channel_name(channel: Channel) -> &'static str {
    match channel {
        Channel::UnreliableSequenced => "unreliable_sequenced",
        Channel::ReliableOrdered => "reliable_ordered",
        Channel::ReliableUnordered => "reliable_unordered",
    }
}

/// Routes one data-path frame from `actor` through the room.
pub fn handle_data_frame(
    hosted: &mut HostedRoom,
    actor: ActorId,
    channel: Channel,
    payload: Bytes,
    metrics: &Metrics,
) -> Result<()> {
    // Simulation frames travel on any channel (inputs unreliable, checkpoint uploads
    // reliable); unknown kinds fall through to relay events.
    if let Some(sy) = hosted.sync.as_mut() {
        if sy.handle(actor, payload.clone()) {
            hosted
                .room
                .count_input(actor, payload.len(), Instant::now());
            return Ok(());
        }
    }
    if let Some(ls) = hosted.lockstep.as_mut() {
        if ls.handle(actor, payload.clone()) {
            hosted
                .room
                .count_input(actor, payload.len(), Instant::now());
            return Ok(());
        }
    }
    let frame = EventFrame::decode(payload).map_err(|e| NetError::Invalid(e.to_string()))?;
    let out = hosted
        .room
        .raise_event(actor, frame, channel, Instant::now())
        .map_err(crate::ws::room_err)?;
    dispatch(hosted, out, metrics);
    Ok(())
}
