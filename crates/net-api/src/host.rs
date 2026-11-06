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
    /// Binary WebSocket frames (tunnel mode).
    Ws(mpsc::Sender<Bytes>),
}

pub struct DataSession {
    pub link: Mutex<Link>,
    pub sink: DataSink,
    pub metrics: Arc<Metrics>,
}

impl DataSession {
    pub fn new(sink: DataSink, metrics: Arc<Metrics>) -> Arc<Self> {
        Arc::new(Self {
            link: Mutex::new(Link::new(Instant::now())),
            sink,
            metrics,
        })
    }

    /// Queues a message and flushes everything the link produced.
    pub fn send(&self, channel: Channel, payload: Bytes) {
        let out: Vec<Bytes> = {
            let mut l = self.link.lock();
            if l.send(channel, payload, Instant::now()).is_err() {
                return;
            }
            // Unreliable messages are coalesced and leave on the next flusher pass.
            if channel == Channel::UnreliableSequenced {
                return;
            }
            std::iter::from_fn(|| l.next_outgoing()).collect()
        };
        self.flush(out);
    }

    pub fn flush(&self, out: Vec<Bytes>) {
        for d in out {
            self.metrics.datagrams_out.inc();
            self.metrics.bytes_out.inc_by(d.len() as u64);
            let ok = match &self.sink {
                DataSink::Quic(c) => c.send_datagram(d).is_ok(),
                DataSink::Ws(tx) => tx.try_send(d).is_ok(),
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
    pub fn receive(&self, d: Bytes) -> Vec<net_transport::Received> {
        self.metrics.datagrams_in.inc();
        self.metrics.bytes_in.inc_by(d.len() as u64);
        let (recv, out) = {
            let mut l = self.link.lock();
            let r = l.receive(d, Instant::now());
            let _ = l.poll(Instant::now());
            (
                r,
                std::iter::from_fn(|| l.next_outgoing()).collect::<Vec<_>>(),
            )
        };
        self.flush(out);
        recv
    }

    /// Timer-driven retransmits/acks. Returns false when the link is dead.
    pub fn tick(&self) -> bool {
        let (alive, out) = {
            let mut l = self.link.lock();
            let alive = l.poll(Instant::now()).is_ok();
            (
                alive,
                std::iter::from_fn(|| l.next_outgoing()).collect::<Vec<_>>(),
            )
        };
        self.flush(out);
        alive
    }
}

pub struct Attached {
    pub control: mpsc::Sender<ServerMsg>,
    pub data: Option<Arc<DataSession>>,
}

pub struct HostedRoom {
    pub room: Room,
    pub sessions: HashMap<ActorId, Attached>,
    pub project: ProjectId,
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
}

impl RoomHost {
    pub fn register_data(&self, ds: &Arc<DataSession>) {
        self.data_sessions.lock().push(Arc::downgrade(ds));
    }

    /// One pass: poll every live link and push its datagrams. Returns dead links.
    pub fn flush_all(&self) -> usize {
        let sessions: Vec<Arc<DataSession>> = {
            let mut v = self.data_sessions.lock();
            v.retain(|w| w.strong_count() > 1);
            v.iter().filter_map(|w| w.upgrade()).collect()
        };
        let mut dead = 0;
        for s in sessions {
            if !s.tick() {
                dead += 1;
                if let DataSink::Quic(c) = &s.sink {
                    c.close(4u32.into(), b"link dead");
                }
            }
        }
        dead
    }

    pub fn insert(&self, project: ProjectId, room: Room) -> Arc<Mutex<HostedRoom>> {
        let id = room.id();
        let name = room.cfg.name.clone();
        let h = Arc::new(Mutex::new(HostedRoom {
            room,
            sessions: HashMap::new(),
            project,
        }));
        self.rooms.insert(id, h.clone());
        self.by_name.insert((project, name), id);
        h
    }

    pub fn get(&self, id: RoomId) -> Option<Arc<Mutex<HostedRoom>>> {
        self.rooms.get(&id).map(|r| r.clone())
    }

    pub fn by_name(&self, project: ProjectId, name: &str) -> Option<Arc<Mutex<HostedRoom>>> {
        let id = *self.by_name.get(&(project, name.to_string()))?;
        self.get(id)
    }

    pub fn remove(&self, id: RoomId) -> Option<Arc<Mutex<HostedRoom>>> {
        let (_, h) = self.rooms.remove(&id)?;
        let (project, name) = {
            let g = h.lock();
            (g.project, g.room.cfg.name.clone())
        };
        self.by_name.remove(&(project, name));
        Some(h)
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
                let g = r.lock();
                (g.project == project && g.room.is_listed()).then(|| g.room.summary())
            })
            .collect()
    }

    pub fn subscribe_lobby(&self, project: ProjectId, tx: mpsc::Sender<ServerMsg>) {
        self.lobby.entry(project).or_default().push(tx);
    }

    pub fn unsubscribe_lobby(&self, project: ProjectId, tx: &mpsc::Sender<ServerMsg>) {
        if let Some(mut v) = self.lobby.get_mut(&project) {
            v.retain(|t| !t.same_channel(tx));
        }
    }

    pub fn notify_lobby(&self, project: ProjectId, msg: ServerMsg) {
        if let Some(mut v) = self.lobby.get_mut(&project) {
            v.retain(|t| !t.is_closed());
            for t in v.iter() {
                let _ = t.try_send(msg.clone());
            }
        }
    }
}

/// Delivers what the room logic produced.
pub fn dispatch(h: &HostedRoom, out: Vec<Outgoing>, metrics: &Metrics) {
    for o in out {
        match o {
            Outgoing::Control(actor, msg) => {
                if let Some(s) = h.sessions.get(&actor) {
                    let _ = s.control.try_send(msg);
                }
            }
            Outgoing::Data(actor, channel, frame) => {
                let Some(s) = h.sessions.get(&actor) else {
                    continue;
                };
                metrics
                    .events
                    .with_label_values(&[channel_name(channel)])
                    .inc();
                match &s.data {
                    Some(d) => d.send(channel, frame.encode()),
                    None => {
                        // No data path yet: fall back to the control channel.
                        use base64::Engine;
                        let _ = s.control.try_send(ServerMsg::Event {
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

pub fn channel_name(c: Channel) -> &'static str {
    match c {
        Channel::UnreliableSequenced => "unreliable_sequenced",
        Channel::ReliableOrdered => "reliable_ordered",
        Channel::ReliableUnordered => "reliable_unordered",
    }
}

/// Routes one data-path frame from `actor` through the room.
pub fn handle_data_frame(
    h: &mut HostedRoom,
    actor: ActorId,
    channel: Channel,
    payload: Bytes,
    metrics: &Metrics,
) -> Result<()> {
    let frame = EventFrame::decode(payload).map_err(|e| NetError::Invalid(e.to_string()))?;
    let out = h
        .room
        .raise_event(actor, frame, channel, Instant::now())
        .map_err(|e| NetError::Invalid(e.to_string()))?;
    dispatch(h, out, metrics);
    Ok(())
}
