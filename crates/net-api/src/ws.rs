//! Control channel: one WebSocket per client. Text frames are JSON [`ClientMsg`]/[`ServerMsg`],
//! binary frames are tunnelled data-path datagrams (first one is the data ticket).

use crate::host::{dispatch, handle_data_frame, Attached, DataSession, DataSink, HostedRoom};
use crate::state::Shared;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use base64::Engine;
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use net_auth::Grant;
use net_common::{ActorId, NetError, ProjectId, Result, RoomId, RoomMode, SessionId};
use net_control::redis_store::LiveRoom;
use net_control::ProjectLimits;
use net_db::queries;
use net_rooms::protocol::{ClientMsg, DataEndpoint, RoomSummary, ServerMsg};
use net_rooms::{EventFrame, Room, RoomConfig, RoomError};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

const IDLE_TIMEOUT: Duration = Duration::from_secs(90);
const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

pub async fn ws_handler(ws: WebSocketUpgrade, State(st): State<Shared>) -> Response {
    ws.max_message_size(net_common::limits::MAX_MESSAGE + 4096)
        .on_upgrade(move |socket| run(socket, st))
}

struct InRoom {
    id: RoomId,
    actor: ActorId,
    hosted: Arc<Mutex<HostedRoom>>,
}

struct Session {
    st: Shared,
    id: SessionId,
    ctl: mpsc::Sender<ServerMsg>,
    bin: mpsc::Sender<Bytes>,
    grant: Option<Grant>,
    room: Option<InRoom>,
    data: Option<Arc<DataSession>>,
    lobby: Option<ProjectId>,
}

async fn run(socket: WebSocket, st: Shared) {
    st.metrics.control_sessions.inc();
    let (mut sink, mut stream) = socket.split();
    let (ctl_tx, mut ctl_rx) = mpsc::channel::<ServerMsg>(512);
    let (bin_tx, mut bin_rx) = mpsc::channel::<Bytes>(2048);
    let writer = tokio::spawn(async move {
        loop {
            tokio::select! {
                m = ctl_rx.recv() => match m {
                    Some(m) => {
                        let txt = serde_json::to_string(&m).unwrap_or_default();
                        if sink.send(Message::Text(txt)).await.is_err() { break; }
                    }
                    None => break,
                },
                b = bin_rx.recv() => match b {
                    Some(b) => if sink.send(Message::Binary(b.to_vec())).await.is_err() { break; },
                    None => break,
                },
            }
        }
        let _ = sink.close().await;
    });

    let mut sess = Session {
        st: st.clone(),
        id: SessionId::new(),
        ctl: ctl_tx,
        bin: bin_tx,
        grant: None,
        room: None,
        data: None,
        lobby: None,
    };
    let _ = sess.ctl.try_send(ServerMsg::Hello {
        session: sess.id.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        region: st.region.to_string(),
    });

    loop {
        let next = tokio::time::timeout(IDLE_TIMEOUT, stream.next()).await;
        let msg = match next {
            Err(_) => break,
            Ok(None) => break,
            Ok(Some(Err(_))) => break,
            Ok(Some(Ok(m))) => m,
        };
        match msg {
            Message::Text(t) => match serde_json::from_str::<ClientMsg>(&t) {
                Ok(m) => {
                    if let Err(e) = sess.handle(m).await {
                        sess.error(e);
                    }
                }
                Err(e) => sess.error(NetError::Invalid(format!("bad message: {e}"))),
            },
            Message::Binary(b) => {
                if let Err(e) = sess.on_binary(Bytes::from(b)) {
                    sess.error(e);
                }
            }
            Message::Close(_) => break,
            Message::Ping(_) | Message::Pong(_) => {}
        }
    }
    sess.cleanup().await;
    writer.abort();
    st.metrics.control_sessions.dec();
}

impl Session {
    fn error(&self, e: NetError) {
        self.st.metrics.errors.with_label_values(&[e.code()]).inc();
        let _ = self.ctl.try_send(ServerMsg::Error {
            code: e.code().to_string(),
            message: e.to_string(),
        });
    }

    fn grant(&self) -> Result<&Grant> {
        self.grant
            .as_ref()
            .ok_or_else(|| NetError::Unauthorized("send auth first".into()))
    }

    fn in_room(&self) -> Result<&InRoom> {
        self.room
            .as_ref()
            .ok_or_else(|| NetError::Invalid("not in a room".into()))
    }

    async fn handle(&mut self, msg: ClientMsg) -> Result<()> {
        match msg {
            ClientMsg::Auth { grant } => {
                let g = self.st.grants.verify(&grant)?;
                let _ = self.ctl.try_send(ServerMsg::Authed {
                    player_id: g.player_id.clone(),
                    name: g.name.clone(),
                });
                self.grant = Some(g);
                Ok(())
            }
            ClientMsg::Ping { ts } => {
                let _ = self.ctl.try_send(ServerMsg::Pong {
                    ts,
                    server_ms: self.st.started.elapsed().as_millis() as u64,
                });
                Ok(())
            }
            ClientMsg::Create {
                name,
                mode,
                max_players,
                visible,
                open,
                props,
                lobby_props,
                region,
                player_props,
            } => {
                let g = self.grant()?.clone();
                self.ensure_not_in_room()?;
                let name = name.unwrap_or_else(short_id);
                let h = self
                    .create_room(
                        &g,
                        name,
                        mode,
                        max_players,
                        visible,
                        open,
                        props,
                        lobby_props,
                        region,
                    )
                    .await?;
                self.join_hosted(h, player_props, None).await
            }
            ClientMsg::Join {
                name,
                player_props,
                rejoin_actor,
                schema_hash,
            } => {
                let g = self.grant()?.clone();
                self.ensure_not_in_room()?;
                self.check_room_scope(&g, &name)?;
                let h = self.find_room(g.project, &name).await?;
                if let Some(c) = schema_hash.as_deref() {
                    let hr = h.lock();
                    if let Some(sy) = hr.sync.as_ref() {
                        if sy.schema_hash != c {
                            return Err(NetError::SchemaMismatch {
                                client: c.to_string(),
                                server: sy.schema_hash.clone(),
                            });
                        }
                    }
                }
                self.join_hosted(h, player_props, rejoin_actor).await
            }
            ClientMsg::JoinOrCreate {
                name,
                mode,
                max_players,
                props,
                lobby_props,
                region,
                player_props,
            } => {
                let g = self.grant()?.clone();
                self.ensure_not_in_room()?;
                self.check_room_scope(&g, &name)?;
                // Several players may race to create the same room; whoever loses the DB
                // uniqueness race joins the winner's room.
                let mut attempt = 0;
                let h = loop {
                    match self.find_room(g.project, &name).await {
                        Ok(h) => {
                            let existing = h.lock().room.cfg.mode;
                            if existing != mode {
                                return Err(NetError::Conflict(format!(
                                    "room exists in {existing:?} mode, requested {mode:?}"
                                )));
                            }
                            break h;
                        }
                        Err(NetError::NotFound(_)) => {}
                        Err(NetError::Conflict(ref m)) if m == "room_pending" && attempt < 30 => {
                            attempt += 1;
                            tokio::time::sleep(Duration::from_millis(10)).await;
                            continue;
                        }
                        Err(e) => return Err(e),
                    }
                    match self
                        .create_room(
                            &g,
                            name.clone(),
                            mode,
                            max_players,
                            true,
                            true,
                            props.clone(),
                            lobby_props.clone(),
                            region.clone(),
                        )
                        .await
                    {
                        Ok(h) => break h,
                        Err(NetError::Conflict(_)) if attempt < 30 => {
                            attempt += 1;
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                        Err(e) => return Err(e),
                    }
                };
                self.join_hosted(h, player_props, None).await
            }
            ClientMsg::JoinRandom {
                filter,
                player_props,
            } => {
                let g = self.grant()?.clone();
                self.ensure_not_in_room()?;
                let candidates = self.st.host.all();
                let mut pick = None;
                for h in candidates {
                    let ok = {
                        let r = h.lock();
                        r.project == g.project
                            && r.room.is_listed()
                            && filter
                                .iter()
                                .all(|(k, v)| r.room.cfg.props.get(k) == Some(v))
                    };
                    if ok {
                        pick = Some(h);
                        break;
                    }
                }
                let h = pick.ok_or_else(|| NetError::NotFound("no matching room".into()))?;
                self.join_hosted(h, player_props, None).await
            }
            ClientMsg::Leave => {
                self.leave(false).await;
                let _ = self.ctl.try_send(ServerMsg::Left {
                    reason: "left".into(),
                });
                Ok(())
            }
            ClientMsg::SetRoomProps {
                props,
                expected,
                visible,
                open,
            } => {
                let r = self.in_room()?;
                {
                    let mut h = r.hosted.lock();
                    let out = h
                        .room
                        .set_room_props(r.actor, props, expected.as_ref(), visible, open)
                        .map_err(room_err)?;
                    dispatch(&h, out, &self.st.metrics);
                }
                self.publish_current().await;
                Ok(())
            }
            ClientMsg::SetPlayerProps { props, expected } => {
                let r = self.in_room()?;
                let mut h = r.hosted.lock();
                let out = h
                    .room
                    .set_player_props(r.actor, props, expected.as_ref())
                    .map_err(room_err)?;
                dispatch(&h, out, &self.st.metrics);
                Ok(())
            }
            ClientMsg::Event {
                code,
                data,
                target,
                channel,
                cache,
                group,
            } => {
                let r = self.in_room()?;
                let payload = B64
                    .decode(data)
                    .map_err(|_| NetError::Invalid("event data is not base64".into()))?;
                let frame = EventFrame {
                    code,
                    target,
                    cache,
                    group,
                    from: r.actor,
                    payload: Bytes::from(payload),
                };
                let mut h = r.hosted.lock();
                let out = h
                    .room
                    .raise_event(r.actor, frame, channel, Instant::now())
                    .map_err(room_err)?;
                dispatch(&h, out, &self.st.metrics);
                Ok(())
            }
            ClientMsg::SetGroups { join, leave } => {
                let r = self.in_room()?;
                r.hosted
                    .lock()
                    .room
                    .set_groups(r.actor, &join, &leave)
                    .map_err(room_err)
            }
            ClientMsg::SetMaster { actor } => {
                let r = self.in_room()?;
                let mut h = r.hosted.lock();
                let out = h.room.set_master(r.actor, actor).map_err(room_err)?;
                dispatch(&h, out, &self.st.metrics);
                Ok(())
            }
            ClientMsg::Kick { actor, reason } => {
                let r = self.in_room()?;
                {
                    let mut h = r.hosted.lock();
                    let out = h
                        .room
                        .kick(r.actor, actor, &reason, Instant::now())
                        .map_err(room_err)?;
                    dispatch(&h, out, &self.st.metrics);
                    h.sessions.remove(&actor);
                    self.st.webhooks.emit(
                        h.project,
                        "player.left",
                        serde_json::json!({"room": h.room.id().to_string(), "actor": actor.0}),
                    );
                    if let Some(sy) = h.sync.as_mut() {
                        sy.on_leave(actor);
                    }
                    if let Some(ls) = h.lockstep.as_mut() {
                        ls.on_leave(actor);
                    }
                }
                self.publish_current().await;
                Ok(())
            }
            ClientMsg::Lobby { subscribe } => {
                let g = self.grant()?.clone();
                if subscribe {
                    self.st.host.subscribe_lobby(g.project, self.ctl.clone());
                    self.lobby = Some(g.project);
                    let rooms = self.lobby_rooms(g.project).await;
                    let _ = self.ctl.try_send(ServerMsg::LobbyRooms { rooms });
                } else if let Some(p) = self.lobby.take() {
                    self.st.host.unsubscribe_lobby(p, &self.ctl);
                }
                Ok(())
            }
        }
    }

    fn ensure_not_in_room(&self) -> Result<()> {
        if self.room.is_some() {
            return Err(NetError::Conflict("already in a room; leave first".into()));
        }
        Ok(())
    }

    fn check_room_scope(&self, g: &Grant, name: &str) -> Result<()> {
        if let Some(r) = &g.room {
            if r != name {
                return Err(NetError::Forbidden(format!("grant is limited to room {r}")));
            }
        }
        Ok(())
    }

    async fn lobby_rooms(&self, project: ProjectId) -> Vec<RoomSummary> {
        match self.st.redis.list_rooms(project).await {
            Ok(live) => live
                .into_iter()
                .filter(|r| r.visible && r.open && r.players < r.max_players)
                .map(live_to_summary)
                .collect(),
            Err(_) => self.st.host.listed(project),
        }
    }

    async fn find_room(&self, project: ProjectId, name: &str) -> Result<Arc<Mutex<HostedRoom>>> {
        if let Some(h) = self.st.host.by_name(project, name) {
            return Ok(h);
        }
        match queries::find_open_room(&self.st.pool, project.0, name).await? {
            None => Err(NetError::NotFound(format!("room {name}"))),
            Some(row) if row.state == "orphaned" => adopt_room(&self.st, row).await,
            Some(row) => {
                let node = row
                    .node_id
                    .and_then(|n| self.st.registry.get(net_common::NodeId::from_uuid(n)));
                match node {
                    Some(n) if n.id != self.st.node_id => {
                        Err(NetError::Conflict(format!("room_elsewhere:{}", n.http_url)))
                    }
                    // Row exists for this node but the room is not hosted yet: creation in
                    // progress on another session, or a row left over from a crash.
                    _ if row.node_id == Some(self.st.node_id.0)
                        && (chrono::Utc::now() - row.created_at).num_seconds() < 10 =>
                    {
                        Err(NetError::Conflict("room_pending".into()))
                    }
                    _ => {
                        let _ = queries::close_room(&self.st.pool, row.id).await;
                        Err(NetError::NotFound(format!("room {name}")))
                    }
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn create_room(
        &self,
        g: &Grant,
        name: String,
        mode: RoomMode,
        max_players: Option<u16>,
        visible: bool,
        open: bool,
        props: Map<String, Value>,
        lobby_props: Vec<String>,
        region: Option<String>,
    ) -> Result<Arc<Mutex<HostedRoom>>> {
        if !g.can("create") && !g.can("join") {
            return Err(NetError::Forbidden("grant may not create rooms".into()));
        }
        if name.is_empty() || name.len() > 64 {
            return Err(NetError::Invalid("room name must be 1..=64 chars".into()));
        }
        let lockstep = if mode == RoomMode::Lockstep {
            let hz = props.get("tick_hz").and_then(|v| v.as_u64()).unwrap_or(60) as u16;
            let seed = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(1);
            Some(crate::lockstep::LockstepRoom::new(hz, seed))
        } else {
            None
        };
        let sync = if mode == RoomMode::Sync {
            let seed = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(1);
            Some(build_sync(&self.st, g.project, &props, seed).await?)
        } else {
            None
        };
        if let Some(r) = region.as_deref() {
            if r != self.st.region.as_str() {
                return Err(NetError::Conflict(format!(
                    "wrong_region:{}:{}",
                    self.st.region, r
                )));
            }
        }
        let project = queries::get_project(&self.st.pool, g.project.0).await?;
        if project.disabled_at.is_some() {
            return Err(NetError::Forbidden("project disabled".into()));
        }
        let limits = ProjectLimits::from_json(&project.limits).effective(&self.st.cfg.limits);
        if let Some(allowed) = &limits.allowed_regions {
            if !allowed.iter().any(|r| r == self.st.region.as_str()) {
                return Err(NetError::Forbidden("region not allowed for project".into()));
            }
        }
        let open_rooms = queries::count_open_rooms(&self.st.pool, g.project.0).await?;
        if open_rooms >= limits.max_rooms as i64 {
            return Err(NetError::QuotaExceeded(format!(
                "max {} open rooms",
                limits.max_rooms
            )));
        }
        if self.st.host.len() >= self.st.cfg.limits.max_rooms_per_node as usize {
            return Err(NetError::QuotaExceeded("node is full".into()));
        }
        let max_players = max_players
            .unwrap_or(limits.max_players_per_room.min(16))
            .clamp(1, limits.max_players_per_room);
        if net_rooms::room::props_len(&props) > limits.max_props_bytes {
            return Err(NetError::Invalid("props too large".into()));
        }
        let room_id = RoomId::new();
        let cfg = RoomConfig {
            id: room_id,
            project: g.project,
            name: name.clone(),
            mode,
            max_players,
            visible,
            open,
            props: props.clone(),
            lobby_props: lobby_props.clone(),
            max_events_per_sec: limits.max_events_per_sec_per_room,
            max_event_bytes: limits.max_event_bytes,
            max_props_bytes: limits.max_props_bytes,
            max_cached_events: limits.max_cached_events_per_room,
            offline_ttl: Duration::from_secs(30),
            empty_ttl: Duration::from_secs(5),
        };
        queries::insert_room(
            &self.st.pool,
            &queries::NewRoom {
                id: cfg.id.0,
                project_id: g.project.0,
                node_id: self.st.node_id.0,
                region: self.st.region.to_string(),
                name,
                mode: format!("{mode:?}").to_lowercase(),
                max_players: max_players as i32,
                visible,
                open,
                schema_hash: sync.as_ref().map(|s| s.schema_hash.clone()),
                props: Value::Object(props),
                lobby_props,
            },
        )
        .await
        .map_err(|e| match e {
            NetError::Conflict(_) => NetError::Conflict("room name already in use".into()),
            e => e,
        })?;
        let h = self
            .st
            .host
            .insert(g.project, Room::new(cfg, Instant::now()));
        self.st.webhooks.emit(
            g.project,
            "room.created",
            serde_json::json!({"room": room_id.to_string(), "name": h.lock().room.cfg.name, "mode": format!("{mode:?}").to_lowercase(), "region": self.st.region.to_string()}),
        );
        if let Some(sy) = sync {
            let period = sy.tick_period();
            h.lock().sync = Some(sy);
            tokio::spawn(run_sync(self.st.clone(), room_id, period));
        }
        if let Some(ls) = lockstep {
            let period = ls.tick_period();
            h.lock().lockstep = Some(ls);
            tokio::spawn(run_lockstep(self.st.clone(), room_id, period));
        }
        self.st.metrics.rooms.set(self.st.host.len() as i64);
        Ok(h)
    }

    async fn join_hosted(
        &mut self,
        h: Arc<Mutex<HostedRoom>>,
        player_props: Map<String, Value>,
        rejoin: Option<ActorId>,
    ) -> Result<()> {
        let g = self.grant()?.clone();
        let joined = {
            let mut hr = h.lock();
            if hr.project != g.project {
                return Err(NetError::NotFound("room".into()));
            }
            let (actor, out) = hr
                .room
                .join(&g.player_id, &g.name, player_props, rejoin, Instant::now())
                .map_err(room_err)?;
            hr.sessions.insert(
                actor,
                Attached {
                    control: self.ctl.clone(),
                    data: None,
                },
            );
            self.st.webhooks.emit(
                hr.project,
                "player.joined",
                serde_json::json!({"room": hr.room.id().to_string(), "actor": actor.0}),
            );
            if let Some(sy) = hr.sync.as_mut() {
                sy.on_join(actor);
            }
            if let Some(ls) = hr.lockstep.as_mut() {
                ls.on_join(actor);
            }
            dispatch(&hr, out, &self.st.metrics);
            let ticket = self.st.tickets.issue(self.id, hr.room.id(), actor);
            ServerMsg::Joined {
                room: hr.room.id().to_string(),
                name: hr.room.cfg.name.clone(),
                mode: hr.room.cfg.mode,
                actor,
                master: hr.room.master().unwrap_or(actor),
                max_players: hr.room.cfg.max_players,
                props: hr.room.cfg.props.clone(),
                players: hr.room.player_infos(),
                data: DataEndpoint {
                    host: self.st.cfg.server.public_host.clone(),
                    port: self.st.quic_port,
                    cert_sha256: self.st.cert_sha256.clone(),
                    ticket: B64.encode(ticket),
                    ws_tunnel: self.st.cfg.transport.ws_tunnel,
                    webtransport_url: self
                        .st
                        .webtransport_port
                        .map(|p| format!("https://{}:{p}/", self.st.cfg.server.public_host)),
                },
                cached: hr.room.cached_for_join(),
                schema_hash: hr.sync.as_ref().map(|s| s.schema_hash.clone()),
                tick_hz: hr
                    .sync
                    .as_ref()
                    .map(|s| s.hz)
                    .or(hr.lockstep.as_ref().map(|l| l.hz)),
                tick: hr.lockstep.as_ref().map(|l| l.tick.0),
                seed: hr.lockstep.as_ref().map(|l| l.seed),
            }
        };
        let (id, actor) = match &joined {
            ServerMsg::Joined { room, actor, .. } => (room.parse::<RoomId>().unwrap(), *actor),
            _ => unreachable!(),
        };
        self.room = Some(InRoom {
            id,
            actor,
            hosted: h,
        });
        let _ = self.ctl.try_send(joined);
        self.st.metrics.players.set(self.st.host.players() as i64);
        self.publish_current().await;
        Ok(())
    }

    /// Pushes the room's live record to Redis (which fans out lobby updates to all nodes).
    async fn publish_current(&self) {
        if let Some(r) = &self.room {
            publish_room(&self.st, &r.hosted).await;
        }
    }

    async fn leave(&mut self, offline: bool) {
        if let Some(d) = self.data.take() {
            match &d.sink {
                DataSink::Quic(c) => c.close(0u32.into(), b"left"),
                DataSink::WebTransport(c) => c.close(0u32.into(), b"left"),
                DataSink::Ws(_) => {}
            }
        }
        let Some(r) = self.room.take() else { return };
        let (should_close, project, id) = {
            let mut h = r.hosted.lock();
            let out = h.room.leave(r.actor, offline, Instant::now());
            dispatch(&h, out, &self.st.metrics);
            h.sessions.remove(&r.actor);
            self.st.webhooks.emit(
                h.project,
                "player.left",
                serde_json::json!({"room": h.room.id().to_string(), "actor": r.actor.0}),
            );
            if let Some(sy) = h.sync.as_mut() {
                sy.on_leave(r.actor);
            }
            if let Some(ls) = h.lockstep.as_mut() {
                ls.on_leave(r.actor);
            }
            (h.room.is_empty() && !offline, h.project, h.room.id())
        };
        self.st.metrics.players.set(self.st.host.players() as i64);
        if should_close {
            close_room(&self.st, project, id).await;
        } else {
            publish_room(&self.st, &r.hosted).await;
        }
    }

    fn on_binary(&mut self, b: Bytes) -> Result<()> {
        match &self.data {
            None => {
                let t = self
                    .st
                    .tickets
                    .verify(&b)
                    .ok_or_else(|| NetError::Unauthorized("bad data ticket".into()))?;
                let r = self.in_room()?;
                if t.room != r.id || t.actor != r.actor || t.session != self.id {
                    return Err(NetError::Unauthorized(
                        "ticket does not match session".into(),
                    ));
                }
                let ds = DataSession::new(DataSink::Ws(self.bin.clone()), self.st.metrics.clone());
                let ds = attach_data(&r.hosted, r.actor, ds);
                spawn_ticker(&self.st, ds.clone());
                self.data = Some(ds);
                Ok(())
            }
            Some(ds) => {
                let r = self.in_room()?;
                for m in ds.receive(b) {
                    let mut h = r.hosted.lock();
                    handle_data_frame(&mut h, r.actor, m.channel, m.payload, &self.st.metrics)?;
                }
                Ok(())
            }
        }
    }

    async fn cleanup(&mut self) {
        if let Some(p) = self.lobby.take() {
            self.st.host.unsubscribe_lobby(p, &self.ctl);
        }
        self.leave(true).await;
    }
}

pub fn attach_data(
    h: &Arc<Mutex<HostedRoom>>,
    actor: ActorId,
    ds: Arc<DataSession>,
) -> Arc<DataSession> {
    let mut g = h.lock();
    if let Some(a) = g.sessions.get_mut(&actor) {
        a.data = Some(ds.clone());
    }
    // An empty reliable frame tells the client the path is open; a lockstep room past
    // tick 0 then streams the catch-up (checkpoint + journal tail) on the same channel.
    ds.send(net_common::Channel::ReliableOrdered, Bytes::new());
    if let Some(ls) = g.lockstep.as_mut() {
        for f in ls.on_late_join() {
            ds.send(net_common::Channel::ReliableOrdered, f);
        }
    }
    ds
}

/// Registers a data session with the node-wide flusher (see [`flusher`]).
pub fn spawn_ticker(st: &Shared, ds: Arc<DataSession>) {
    st.host.register_data(&ds);
}

/// Fixed-rate simulation loop for one sync room; stops when the room is gone.
pub async fn run_sync(st: Shared, room: RoomId, period: std::time::Duration) {
    use net_rooms::event::{CacheOp, EventFrame, EventTarget};
    use net_rooms::room::Outgoing;
    let mut iv = tokio::time::interval(period);
    iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let checkpoint_every = (5_000_000 / period.as_micros().max(1)) as u64;
    let mut n: u64 = 0;
    loop {
        iv.tick().await;
        let Some(h) = st.host.get(room) else { return };
        let t0 = Instant::now();
        let mut checkpoint: Option<(ProjectId, Vec<u8>)> = None;
        {
            let mut g = h.lock();
            let project = g.project;
            let (out, events, actors) = {
                let Some(sy) = g.sync.as_mut() else { return };
                let out = sy.tick();
                let events = sy.take_events();
                let actors: Vec<ActorId> = sy.avatars.keys().copied().collect();
                n += 1;
                if n.is_multiple_of(checkpoint_every) {
                    checkpoint = Some((project, sy.checkpoint()));
                }
                (out, events, actors)
            };
            if let Some((_, body)) = checkpoint.take() {
                checkpoint = Some((project, RoomCheckpoint::of(&g, Some(body), None).encode()));
            }
            for (actor, d) in out {
                if let Some(ds) = g.sessions.get(&actor).and_then(|s| s.data.as_ref()) {
                    ds.send(net_common::Channel::UnreliableSequenced, d);
                    st.metrics.events.with_label_values(&["sync_delta"]).inc();
                }
            }
            if !events.is_empty() {
                let mut outgoing = Vec::new();
                for (target, code, payload) in events {
                    let frame = EventFrame {
                        code,
                        target: EventTarget::All,
                        cache: CacheOp::None,
                        group: None,
                        from: ActorId(0),
                        payload,
                    };
                    match target {
                        Some(a) => outgoing.push(Outgoing::Data(
                            a,
                            net_common::Channel::ReliableOrdered,
                            frame,
                        )),
                        None => {
                            for a in &actors {
                                outgoing.push(Outgoing::Data(
                                    *a,
                                    net_common::Channel::ReliableOrdered,
                                    frame.clone(),
                                ));
                            }
                        }
                    }
                }
                dispatch(&g, outgoing, &st.metrics);
            }
        }
        st.metrics.sync_tick.observe(t0.elapsed().as_secs_f64());
        if let Some((project, body)) = checkpoint {
            let redis = st.redis.clone();
            tokio::spawn(async move {
                if let Err(e) = redis.put_checkpoint(project, room, &body, 600).await {
                    tracing::warn!(%room, error = %e, "checkpoint write failed");
                }
            });
        }
    }
}

/// Lockstep clock: every tick bundle the inputs for that tick and fan them out.
pub async fn run_lockstep(st: Shared, room: RoomId, period: std::time::Duration) {
    let mut iv = tokio::time::interval(period);
    iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let checkpoint_every = (5_000_000 / period.as_micros().max(1)) as u64;
    let mut n: u64 = 0;
    loop {
        iv.tick().await;
        let Some(h) = st.host.get(room) else { return };
        let t0 = Instant::now();
        let mut checkpoint: Option<(ProjectId, Vec<u8>)> = None;
        {
            let mut g = h.lock();
            n += 1;
            if n.is_multiple_of(checkpoint_every) {
                let snap = g.lockstep.as_ref().map(|l| l.snapshot());
                checkpoint = Some((g.project, RoomCheckpoint::of(&g, None, snap).encode()));
            }
            let master = g.room.master();
            let (bundle, desyncs, actors, ckpt_req) = {
                let Some(ls) = g.lockstep.as_mut() else {
                    return;
                };
                ls.master = master;
                let bundle = ls.tick();
                let ckpt_req = ls.checkpoint_request();
                let desyncs = ls.take_desyncs();
                let actors: Vec<ActorId> = ls.actors.keys().copied().collect();
                (bundle, desyncs, actors, ckpt_req)
            };
            if let Some((req, m)) = ckpt_req.zip(master) {
                if let Some(ds) = g.sessions.get(&m).and_then(|s| s.data.as_ref()) {
                    ds.send(net_common::Channel::ReliableOrdered, req);
                }
            }
            for a in &actors {
                if let Some(ds) = g.sessions.get(a).and_then(|s| s.data.as_ref()) {
                    ds.send(net_common::Channel::UnreliableSequenced, bundle.clone());
                    st.metrics
                        .events
                        .with_label_values(&["lockstep_bundle"])
                        .inc();
                }
            }
            for (tick, actor) in desyncs {
                st.metrics.errors.with_label_values(&["desync"]).inc();
                st.webhooks.emit(
                    g.project,
                    "desync",
                    serde_json::json!({"room": room.to_string(), "tick": tick, "actor": actor.0}),
                );
                for a in &actors {
                    if let Some(s) = g.sessions.get(a) {
                        let _ = s.control.try_send(ServerMsg::Desync { tick, actor });
                    }
                }
            }
        }
        st.metrics.sync_tick.observe(t0.elapsed().as_secs_f64());
        if let Some((project, body)) = checkpoint {
            let redis = st.redis.clone();
            tokio::spawn(async move {
                if let Err(e) = redis.put_checkpoint(project, room, &body, 600).await {
                    tracing::warn!(%room, error = %e, "checkpoint write failed");
                }
            });
        }
    }
}

/// Sync room from room props: `tick_hz`, `schema` (IDL text), replication settings and
/// `logic` (name of a project WASM module uploaded via `PUT /v1/modules/:name`).
pub async fn build_sync(
    st: &Shared,
    project: ProjectId,
    props: &Map<String, Value>,
    seed: u64,
) -> Result<crate::sync::SyncRoom> {
    let schema = props.get("schema").and_then(|v| v.as_str());
    let hz = props.get("tick_hz").and_then(|v| v.as_u64()).unwrap_or(60) as u16;
    let logic: Option<Box<dyn crate::sync::GameLogic>> =
        match props.get("logic").and_then(|v| v.as_str()) {
            None => None,
            Some(name) => {
                let row = queries::get_game_module(&st.pool, project.0, name)
                    .await?
                    .ok_or_else(|| NetError::NotFound(format!("game module {name}")))?;
                let (_, module) = st
                    .games
                    .load(&row.wasm)
                    .map_err(|e| NetError::Invalid(format!("game module {name}: {e}")))?;
                let logic = st
                    .games
                    .instantiate(&module, seed)
                    .map_err(|e| NetError::Invalid(format!("game module {name}: {e}")))?;
                Some(Box::new(logic))
            }
        };
    crate::sync::SyncRoom::with_logic(
        schema,
        hz,
        seed,
        crate::sync::SyncRoom::replication_from_props(props),
        logic,
    )
    .map_err(|e| NetError::Invalid(format!("schema: {e}")))
}

/// What a node writes to Redis every few seconds per sync/lockstep room so that any other
/// node can continue the room after this one dies (`adopt_room`).
#[derive(Serialize, Deserialize)]
pub struct RoomCheckpoint {
    /// (actor, player_id, name) of every player, so reconnects get their actor ids back.
    pub players: Vec<(ActorId, String, String)>,
    pub master: Option<ActorId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync: Option<Vec<u8>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lockstep: Option<crate::lockstep::LockstepSnapshot>,
}

impl RoomCheckpoint {
    pub fn of(
        g: &HostedRoom,
        sync: Option<Vec<u8>>,
        lockstep: Option<crate::lockstep::LockstepSnapshot>,
    ) -> Self {
        Self {
            players: g
                .room
                .players()
                .map(|p| (p.actor, p.player_id.clone(), p.name.clone()))
                .collect(),
            master: g.room.master(),
            sync,
            lockstep,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }
}

/// Takes over an orphaned room (its node was reaped): rebuilds the room from the DB row,
/// restores simulation state from the Redis checkpoint, recreates the players as offline
/// slots and restarts the tick loop. Relay rooms come back empty but keep name/props/slots.
pub async fn adopt_room(
    st: &Shared,
    row: net_db::models::RoomRow,
) -> Result<Arc<Mutex<HostedRoom>>> {
    let project = ProjectId(row.project_id);
    let room_id = RoomId(row.id);
    if st.host.len() >= st.cfg.limits.max_rooms_per_node as usize {
        return Err(NetError::QuotaExceeded("node is full".into()));
    }
    if !queries::adopt_room(&st.pool, row.id, st.node_id.0, st.region.as_str()).await? {
        return Err(NetError::Conflict("room_pending".into()));
    }
    let ckpt = match st.redis.get_checkpoint(project, room_id).await {
        Ok(Some(b)) => serde_json::from_slice::<RoomCheckpoint>(&b).ok(),
        Ok(None) => None,
        Err(e) => {
            tracing::warn!(room = %room_id, error = %e, "checkpoint read failed");
            None
        }
    };
    let proj = queries::get_project(&st.pool, row.project_id).await?;
    let limits = ProjectLimits::from_json(&proj.limits).effective(&st.cfg.limits);
    let mode = match row.mode.as_str() {
        "sync" => RoomMode::Sync,
        "lockstep" => RoomMode::Lockstep,
        _ => RoomMode::Relay,
    };
    let props = row.props.as_object().cloned().unwrap_or_default();
    let cfg = RoomConfig {
        id: room_id,
        project,
        name: row.name.clone(),
        mode,
        max_players: row.max_players.clamp(1, i32::from(u16::MAX)) as u16,
        visible: row.visible,
        open: row.open,
        props: props.clone(),
        lobby_props: row.lobby_props.clone(),
        max_events_per_sec: limits.max_events_per_sec_per_room,
        max_event_bytes: limits.max_event_bytes,
        max_props_bytes: limits.max_props_bytes,
        max_cached_events: limits.max_cached_events_per_room,
        offline_ttl: Duration::from_secs(30),
        empty_ttl: Duration::from_secs(5),
    };
    let hz = props.get("tick_hz").and_then(|v| v.as_u64()).unwrap_or(60) as u16;
    let mut room = Room::new(cfg, Instant::now());
    let mut sync = None;
    let mut lockstep = None;
    let mut restored = "none";
    if let Some(c) = ckpt {
        room.restore_offline(c.players, c.master, Instant::now());
        if mode == RoomMode::Sync {
            let mut sy = build_sync(st, project, &props, 0).await?;
            if let Some(body) = c.sync {
                sy.restore(&body).map_err(NetError::Invalid)?;
                restored = "sync";
            }
            sync = Some(sy);
        }
        if let (RoomMode::Lockstep, Some(snap)) = (mode, c.lockstep) {
            lockstep = Some(crate::lockstep::LockstepRoom::restore(snap));
            restored = "lockstep";
        }
    }
    if mode == RoomMode::Sync && sync.is_none() {
        sync = Some(build_sync(st, project, &props, 0).await?);
    }
    if mode == RoomMode::Lockstep && lockstep.is_none() {
        lockstep = Some(crate::lockstep::LockstepRoom::new(hz, 1));
    }
    let h = st.host.insert(project, room);
    if let Some(sy) = sync {
        let period = sy.tick_period();
        h.lock().sync = Some(sy);
        tokio::spawn(run_sync(st.clone(), room_id, period));
    }
    if let Some(ls) = lockstep {
        let period = ls.tick_period();
        h.lock().lockstep = Some(ls);
        tokio::spawn(run_lockstep(st.clone(), room_id, period));
    }
    st.metrics.rooms.set(st.host.len() as i64);
    st.metrics.errors.with_label_values(&["room_adopted"]).inc();
    tracing::info!(room = %room_id, name = %row.name, mode = %row.mode, restored, "adopted orphaned room");
    st.webhooks.emit(
        project,
        "room.rehosted",
        serde_json::json!({"room": room_id.to_string(), "name": row.name, "mode": row.mode, "node": st.node_id.to_string(), "restored": restored}),
    );
    publish_room(st, &h).await;
    Ok(h)
}

/// Drives every data link every FLUSH_EVERY: coalesced batches, acks, retransmits.
pub async fn flusher(st: Shared) {
    let mut iv = tokio::time::interval(crate::host::FLUSH_EVERY);
    iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        iv.tick().await;
        st.host.flush_all();
    }
}

pub async fn publish_room(st: &Shared, h: &Arc<Mutex<HostedRoom>>) {
    let (project, live) = {
        let g = h.lock();
        (
            g.project,
            LiveRoom {
                room: g.room.id(),
                node: st.node_id.to_string(),
                name: g.room.cfg.name.clone(),
                mode: format!("{:?}", g.room.cfg.mode).to_lowercase(),
                players: g.room.players().count() as u16,
                max_players: g.room.cfg.max_players,
                open: g.room.cfg.open,
                visible: g.room.cfg.visible,
                props: Value::Object(
                    g.room
                        .summary()
                        .props
                        .into_iter()
                        .collect::<Map<String, Value>>(),
                ),
            },
        )
    };
    if let Err(e) = st.redis.put_room(project, &live).await {
        tracing::warn!(error = %e, "redis put_room failed");
        st.host.notify_lobby(
            project,
            ServerMsg::LobbyUpdate {
                room: live_to_summary(live),
                removed: false,
            },
        );
    }
}

pub async fn close_room(st: &Shared, project: ProjectId, id: RoomId) {
    st.webhooks.emit(
        project,
        "room.closed",
        serde_json::json!({"room": id.to_string()}),
    );
    if let Some(h) = st.host.remove(id) {
        let (sessions, replay) = {
            let g = h.lock();
            let replay = g
                .lockstep
                .as_ref()
                .filter(|l| !l.journal.is_empty())
                .map(|l| l.replay(""));
            (
                g.sessions
                    .values()
                    .map(|s| s.control.clone())
                    .collect::<Vec<mpsc::Sender<ServerMsg>>>(),
                replay,
            )
        };
        if let Some(r) = replay {
            if let Ok(body) = serde_json::to_vec(&r) {
                if let Err(e) = st.redis.put_replay(project, id, &body, 86_400).await {
                    tracing::warn!(error = %e, room = %id, "replay store");
                }
            }
        }
        for s in sessions {
            let _ = s.try_send(ServerMsg::Left {
                reason: "room closed".into(),
            });
        }
    }
    st.metrics.rooms.set(st.host.len() as i64);
    if let Err(e) = queries::close_room(&st.pool, id.0).await {
        tracing::warn!(error = %e, room = %id, "close_room db");
    }
    if let Err(e) = st.redis.remove_room(project, id).await {
        tracing::warn!(error = %e, room = %id, "close_room redis");
        st.host.notify_lobby(
            project,
            ServerMsg::LobbyUpdate {
                room: RoomSummary {
                    id: id.to_string(),
                    name: String::new(),
                    mode: RoomMode::Relay,
                    players: 0,
                    max_players: 0,
                    open: false,
                    props: Map::new(),
                },
                removed: true,
            },
        );
    }
}

pub fn live_to_summary(r: LiveRoom) -> RoomSummary {
    RoomSummary {
        id: r.room.to_string(),
        name: r.name,
        mode: match r.mode.as_str() {
            "sync" => RoomMode::Sync,
            "lockstep" => RoomMode::Lockstep,
            _ => RoomMode::Relay,
        },
        players: r.players,
        max_players: r.max_players,
        open: r.open && r.players < r.max_players,
        props: match r.props {
            Value::Object(m) => m,
            _ => Map::new(),
        },
    }
}

fn room_err(e: RoomError) -> NetError {
    match e {
        RoomError::Full => NetError::RoomFull { max: 0 },
        RoomError::Closed => NetError::RoomClosed,
        RoomError::Duplicate => NetError::Conflict("player already in room".into()),
        RoomError::NotMember | RoomError::NoActor => NetError::NotFound(e.to_string()),
        RoomError::NotMaster => NetError::Forbidden(e.to_string()),
        RoomError::EventRate => NetError::RateLimited,
        RoomError::EventTooLarge | RoomError::PropsTooLarge | RoomError::CasFailed => {
            NetError::Invalid(e.to_string())
        }
    }
}

fn short_id() -> String {
    let u = uuid::Uuid::new_v4();
    let b = u.as_bytes();
    B64.encode(&b[..6])
        .replace(['+', '/'], "x")
        .trim_end_matches('=')
        .to_lowercase()
}

/// Periodic housekeeping: expire offline players, close empty rooms.
pub async fn reaper(st: Shared) {
    let mut iv = tokio::time::interval(Duration::from_secs(2));
    loop {
        iv.tick().await;
        for h in st.host.all() {
            let (close, project, id, changed) = {
                let mut g = h.lock();
                let out = g.room.expire(Instant::now());
                let changed = !out.is_empty();
                for o in out.iter() {
                    if let net_rooms::Outgoing::Control(
                        a,
                        ServerMsg::PlayerLeft { offline: false, .. },
                    ) = o
                    {
                        g.sessions.remove(a);
                        st.webhooks.emit(
                            g.project,
                            "player.left",
                            serde_json::json!({"room": g.room.id().to_string(), "actor": a.0}),
                        );
                        if let Some(sy) = g.sync.as_mut() {
                            sy.on_leave(*a);
                        }
                        if let Some(ls) = g.lockstep.as_mut() {
                            ls.on_leave(*a);
                        }
                    }
                }
                dispatch(&g, out, &st.metrics);
                (
                    g.room.should_close(Instant::now()),
                    g.project,
                    g.room.id(),
                    changed,
                )
            };
            if close {
                close_room(&st, project, id).await;
            } else if changed {
                publish_room(&st, &h).await;
            }
        }
        st.metrics.players.set(st.host.players() as i64);
    }
}
