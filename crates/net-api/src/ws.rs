//! Control channel: one WebSocket per client. Text frames are JSON [`ClientMsg`]/[`ServerMsg`],
//! binary frames are tunnelled data-path datagrams (first one is the data ticket).

use crate::host::{dispatch, handle_data_frame, Attached, DataSession, DataSink, HostedRoom};
use crate::state::Shared;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::{IntoResponse, Response};
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
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

const IDLE_TIMEOUT: Duration = Duration::from_secs(90);
/// A connection that has not authenticated within this window is dropped.
const AUTH_TIMEOUT: Duration = Duration::from_secs(10);
/// Largest text frame accepted before `auth` succeeded (a grant is ~1 KB).
const PRE_AUTH_MAX_TEXT: usize = 8 * 1024;
const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(st): State<Shared>,
    ip: Option<axum::Extension<crate::ratelimit::ClientIp>>,
) -> Response {
    let ip = ip
        .map(|e| e.0 .0)
        .unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED));
    if st.is_draining() {
        return (axum::http::StatusCode::SERVICE_UNAVAILABLE, "node draining").into_response();
    }
    if !st.guards.ws_connects.allow(ip) || !st.guards.acquire_session(ip) {
        st.metrics.errors.with_label_values(&["rate_limited"]).inc();
        return (
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            "too many connections",
        )
            .into_response();
    }
    ws.max_message_size(net_common::limits::MAX_MESSAGE + 4096)
        .on_upgrade(move |socket| async move {
            run(socket, st.clone(), ip).await;
            st.guards.release_session(ip);
        })
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
    ip: std::net::IpAddr,
    /// Last `stats` / `find_friends` (fleet fan-out): at most one per second.
    last_lookup: Option<Instant>,
    /// `rpc` token bucket: `RPC_RATE` per second, burst `RPC_BURST`.
    rpc_tokens: f32,
    rpc_at: Instant,
}

const RPC_RATE: f32 = 5.0;
const RPC_BURST: f32 = 10.0;
const RPC_MAX_BODY: usize = 8 * 1024;

async fn run(socket: WebSocket, st: Shared, ip: std::net::IpAddr) {
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
        ip,
        last_lookup: None,
        rpc_tokens: RPC_BURST,
        rpc_at: Instant::now(),
    };
    let _ = sess.ctl.try_send(ServerMsg::Hello {
        session: sess.id.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        region: st.region.to_string(),
    });

    loop {
        let limit = if sess.grant.is_some() {
            IDLE_TIMEOUT
        } else {
            AUTH_TIMEOUT
        };
        let next = tokio::time::timeout(limit, stream.next()).await;
        let msg = match next {
            Err(_) => break,
            Ok(None) => break,
            Ok(Some(Err(_))) => break,
            Ok(Some(Ok(m))) => m,
        };
        match msg {
            Message::Text(t) if sess.grant.is_none() && t.len() > PRE_AUTH_MAX_TEXT => {
                sess.error(NetError::Invalid("message too large before auth".into()));
                break;
            }
            Message::Text(t) => match serde_json::from_str::<ClientMsg>(&t) {
                Ok(m) => {
                    if let Err(e) = sess.handle(m).await {
                        let fatal = matches!(e, NetError::RateLimited);
                        sess.error(e);
                        if fatal {
                            break;
                        }
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

    /// False once another session of the same player took over this room slot.
    fn owns_slot(&self, h: &HostedRoom, actor: ActorId) -> bool {
        h.sessions.get(&actor).is_some_and(|a| a.session == self.id)
    }

    fn replaced(&mut self) -> NetError {
        self.room = None;
        self.data = None;
        NetError::Conflict("session_replaced".into())
    }

    async fn handle(&mut self, msg: ClientMsg) -> Result<()> {
        if let Some(r) = &self.room {
            let mine = self.owns_slot(&r.hosted.lock(), r.actor);
            if !mine {
                return Err(self.replaced());
            }
        }
        match msg {
            ClientMsg::Auth { grant } => {
                let g = match self.st.grants.verify(&grant) {
                    Ok(g) => g,
                    Err(e) => {
                        return Err(if self.st.guards.auth_fail.allow(self.ip) {
                            e
                        } else {
                            NetError::RateLimited
                        });
                    }
                };
                let _ = self.ctl.try_send(ServerMsg::Authed {
                    player_id: g.player_id.clone(),
                    name: g.name.clone(),
                });
                if let Some(old) = &self.grant {
                    self.st.host.presence_leave(old.project, &old.player_id);
                }
                self.st.host.presence_enter(g.project, &g.player_id);
                self.grant = Some(g);
                Ok(())
            }
            ClientMsg::Rpc { id, name, body } => {
                let g = self.grant()?.clone();
                let name_ok = !name.is_empty()
                    && name.len() <= 64
                    && name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'));
                if !name_ok {
                    return Err(NetError::Invalid(
                        "rpc: name is 1..=64 chars of [A-Za-z0-9_.-]".into(),
                    ));
                }
                if serde_json::to_vec(&body).map_or(usize::MAX, |v| v.len()) > RPC_MAX_BODY {
                    return Err(NetError::Invalid(format!(
                        "rpc: body over {RPC_MAX_BODY} bytes"
                    )));
                }
                self.rpc_budget()?;
                let room = self.room.as_ref().map(|r| (r.id.to_string(), r.actor));
                let payload = serde_json::json!({
                    "name": name,
                    "player_id": g.player_id,
                    "actor": room.as_ref().map(|r| r.1),
                    "room": room.as_ref().map(|r| r.0.clone()),
                    "body": body,
                });
                let (hooks, ctl) = (self.st.webhooks.clone(), self.ctl.clone());
                tokio::spawn(async move {
                    let out = hooks.rpc(g.project, payload).await;
                    let _ = ctl.try_send(ServerMsg::RpcResult {
                        id,
                        ok: out.ok,
                        status: out.status,
                        body: out.body,
                        error: out.error,
                    });
                });
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
                        Vec::new(),
                    )
                    .await?;
                self.join_hosted(h, player_props, None, false).await
            }
            ClientMsg::Join {
                name,
                player_props,
                rejoin_actor,
                schema_hash,
                spectator,
            } => {
                let g = self.grant()?.clone();
                self.ensure_not_in_room()?;
                self.check_room_scope(&g, &name)?;
                let h = {
                    let mut attempt = 0;
                    loop {
                        match self.find_room(g.project, &name).await {
                            Err(NetError::Conflict(ref m))
                                if m == "room_pending" && attempt < 200 =>
                            {
                                attempt += 1;
                                tokio::time::sleep(Duration::from_millis(10)).await;
                            }
                            r => break r?,
                        }
                    }
                };
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
                self.join_hosted(h, player_props, rejoin_actor, spectator)
                    .await
            }
            ClientMsg::JoinOrCreate {
                name,
                mode,
                max_players,
                props,
                lobby_props,
                region,
                player_props,
                reserved,
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
                        Err(NetError::Conflict(ref m)) if m == "room_pending" && attempt < 200 => {
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
                            reserved.clone(),
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
                self.join_hosted(h, player_props, None, false).await
            }
            ClientMsg::JoinRandom {
                filter,
                expr,
                match_type,
                player_props,
            } => {
                let g = self.grant()?.clone();
                self.ensure_not_in_room()?;
                let expr = match expr.as_deref() {
                    Some(e) => Some(
                        crate::mmfilter::Filter::parse(e)
                            .map_err(|e| NetError::Invalid(format!("expr: {e}")))?,
                    ),
                    None => None,
                };
                let accepts = |props: &Map<String, Value>, lobby: &Map<String, Value>| {
                    filter.iter().all(|(k, v)| props.get(k) == Some(v))
                        && expr.as_ref().is_none_or(|f| f.matches(lobby))
                };
                // Local rooms first (no redirect), then the region-wide list from Redis.
                let mut local: Vec<net_control::LiveRoom> = Vec::new();
                for h in self.st.host.all() {
                    let r = h.lock();
                    if r.project != g.project || !r.room.is_listed() {
                        continue;
                    }
                    let lobby: Map<String, Value> = r.room.summary().props.into_iter().collect();
                    if accepts(&r.room.cfg.props, &lobby) {
                        local.push(net_control::LiveRoom {
                            room: r.room.id(),
                            node: self.st.node_id.to_string(),
                            name: r.room.cfg.name.clone(),
                            mode: format!("{:?}", r.room.cfg.mode).to_lowercase(),
                            players: r.room.players().count() as u16,
                            max_players: r.room.cfg.max_players,
                            open: true,
                            visible: true,
                            props: Value::Object(lobby),
                        });
                    }
                }
                let refs: Vec<&net_control::LiveRoom> = local.iter().collect();
                if let Some(pick) = crate::matchmaking::pick_room(&refs, match_type.as_deref()) {
                    let h = self
                        .st
                        .host
                        .by_name(g.project, &pick.name)
                        .ok_or_else(|| NetError::NotFound("no matching room".into()))?;
                    return self.join_hosted(h, player_props, None, false).await;
                }
                let me = self.st.node_id.to_string();
                let remote: Vec<net_control::LiveRoom> = self
                    .st
                    .redis
                    .list_rooms(g.project)
                    .await
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|r| {
                        r.node != me
                            && r.open
                            && r.visible
                            && r.players < r.max_players
                            && filter.is_empty()
                            && expr
                                .as_ref()
                                .is_none_or(|f| r.props.as_object().is_some_and(|p| f.matches(p)))
                    })
                    .collect();
                let refs: Vec<&net_control::LiveRoom> = remote.iter().collect();
                let pick = crate::matchmaking::pick_room(&refs, match_type.as_deref())
                    .ok_or_else(|| NetError::NotFound("no matching room".into()))?;
                let node = uuid::Uuid::parse_str(&pick.node)
                    .ok()
                    .and_then(|n| self.st.registry.get(net_common::NodeId::from_uuid(n)))
                    .ok_or_else(|| NetError::NotFound("no matching room".into()))?;
                Err(NetError::Conflict(format!(
                    "room_elsewhere:{}#{}",
                    node.http_url, pick.name
                )))
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
            ClientMsg::CacheSlice { op, index } => {
                let r = self.in_room()?;
                let need = |i: Option<u16>| {
                    i.ok_or_else(|| NetError::Invalid("cache_slice: index required".into()))
                };
                let op = match op.as_str() {
                    "increase" => net_rooms::room::CacheSliceOp::Increase,
                    "set" => net_rooms::room::CacheSliceOp::Set(need(index)?),
                    "purge" => net_rooms::room::CacheSliceOp::Purge(need(index)?),
                    "purge_up_to" => net_rooms::room::CacheSliceOp::PurgeUpTo(need(index)?),
                    _ => {
                        return Err(NetError::Invalid(
                            "cache_slice: op is increase|set|purge|purge_up_to".into(),
                        ))
                    }
                };
                r.hosted.lock().room.cache_slice_op(op);
                Ok(())
            }
            ClientMsg::SetView {
                viewpoints,
                interest,
            } => {
                let r = self.in_room()?;
                let mut h = r.hosted.lock();
                match h.sync.as_mut() {
                    Some(sy) => {
                        sy.set_view(r.actor, &viewpoints, &interest);
                        Ok(())
                    }
                    None => Err(NetError::Invalid("set_view: sync rooms only".into())),
                }
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
            ClientMsg::Stats => {
                let g = self.grant()?.clone();
                self.lookup_budget()?;
                let rooms: Vec<RoomSummary> = match self.st.redis.list_rooms(g.project).await {
                    Ok(live) => live.into_iter().map(live_to_summary).collect(),
                    Err(_) => self.st.host.listed(g.project),
                };
                let (rooms, players, lobbies) = crate::presence::lobby_stats(&rooms);
                let _ = self.ctl.try_send(ServerMsg::Stats {
                    rooms,
                    players,
                    lobbies,
                });
                Ok(())
            }
            ClientMsg::FindFriends { players } => {
                let g = self.grant()?.clone();
                if players.len() > crate::presence::MAX_FRIENDS {
                    return Err(NetError::Invalid(format!(
                        "find_friends: at most {} players",
                        crate::presence::MAX_FRIENDS
                    )));
                }
                if players.iter().any(|p| !crate::rest::valid_player_id(p)) {
                    return Err(NetError::Invalid("find_friends: invalid player id".into()));
                }
                self.lookup_budget()?;
                let friends = crate::presence::find_friends(&self.st, g.project, &players).await;
                let _ = self.ctl.try_send(ServerMsg::Friends { friends });
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
                crate::rest::reject(&self.st, g.project, "grant");
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
        if self.st.is_draining() {
            return Err(NetError::Conflict("node_draining".into()));
        }
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
                    // Adoption in progress on another session of this node.
                    _ if self.st.adopting.lock().contains(&row.id) => {
                        Err(NetError::Conflict("room_pending".into()))
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
        reserved: Vec<String>,
    ) -> Result<Arc<Mutex<HostedRoom>>> {
        if !g.can("create") {
            crate::rest::reject(&self.st, g.project, "grant");
            return Err(NetError::Forbidden("grant may not create rooms".into()));
        }
        if self.st.is_draining() {
            return Err(NetError::Conflict("node_draining".into()));
        }
        crate::rest::validate_room_create(&name, &lobby_props)?;
        let mut lockstep = if mode == RoomMode::Lockstep {
            let hz = props.get("tick_hz").and_then(|v| v.as_u64()).unwrap_or(60) as u16;
            let seed = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(1);
            Some(
                crate::lockstep::LockstepRoom::new(hz, seed).with_spectator_delay(prop_secs(
                    &props,
                    "spectator_delay_secs",
                    3,
                )
                    as u32),
            )
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
        if let Some(ls) = lockstep.as_mut() {
            attach_lockstep_sim(&self.st, g.project, &props, ls).await?;
        }
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
            crate::rest::reject(&self.st, g.project, "suspended");
            return Err(NetError::Forbidden("project disabled".into()));
        }
        if let Some(b) = queries::get_ban(&self.st.pool, g.project.0, &g.player_id).await? {
            crate::rest::reject(&self.st, g.project, "banned");
            return Err(NetError::Forbidden(crate::rest::ban_message(&b)));
        }
        let limits = ProjectLimits::from_json(&project.limits).effective(&self.st.cfg.limits);
        if let Some(allowed) = &limits.allowed_regions {
            if !allowed.iter().any(|r| r == self.st.region.as_str()) {
                crate::rest::reject(&self.st, g.project, "region");
                return Err(NetError::Forbidden("region not allowed for project".into()));
            }
        }
        let open_rooms = queries::count_open_rooms(&self.st.pool, g.project.0).await?;
        if open_rooms >= limits.max_rooms as i64 {
            crate::rest::reject(&self.st, g.project, "quota");
            return Err(NetError::QuotaExceeded(format!(
                "max {} open rooms",
                limits.max_rooms
            )));
        }
        crate::rest::check_usage_cap(&self.st, g.project.0, &limits).await?;
        if self.st.host.len() >= self.st.cfg.limits.max_rooms_per_node as usize {
            crate::rest::reject(&self.st, g.project, "capacity");
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
            reserved: reserved.iter().take(32).cloned().collect(),
            reserved_until: (!reserved.is_empty())
                .then(|| std::time::Instant::now() + Duration::from_secs(60)),
            cleanup_cache_on_leave: prop_bool(&props, "cleanup_cache_on_leave", true),
            max_events_per_sec: limits.max_events_per_sec_per_room,
            max_events_per_sec_per_player: limits.max_events_per_sec_per_player,
            max_event_bytes: limits.max_event_bytes,
            max_props_bytes: limits.max_props_bytes,
            max_player_props_bytes: limits.max_player_props_bytes,
            max_cached_events: limits.max_cached_events_per_room,
            offline_ttl: Duration::from_secs(prop_secs(&props, "player_ttl_secs", 30)),
            empty_ttl: Duration::from_secs(prop_secs(&props, "empty_ttl_secs", 5)),
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
        spectator: bool,
    ) -> Result<()> {
        let g = self.grant()?.clone();
        if rejoin.is_none() {
            let (room_id, room_name, mode, props) = {
                let hr = h.lock();
                if hr.project != g.project {
                    return Err(NetError::NotFound("room".into()));
                }
                (
                    hr.room.cfg.id,
                    hr.room.cfg.name.clone(),
                    format!("{:?}", hr.room.cfg.mode).to_lowercase(),
                    hr.room.cfg.props.clone(),
                )
            };
            let payload = serde_json::json!({
                "room": room_id.to_string(), "name": room_name, "mode": mode, "room_props": props,
                "player_id": g.player_id, "player_name": g.name, "player_props": player_props,
                "spectator": spectator,
            });
            match self.st.webhooks.validate_join(g.project, payload).await {
                crate::webhooks::JoinVerdict::Allow => {}
                crate::webhooks::JoinVerdict::Deny(reason) => {
                    crate::rest::reject(&self.st, g.project, "validate");
                    return Err(NetError::Forbidden(format!("join rejected: {reason}")));
                }
                crate::webhooks::JoinVerdict::Unavailable(e) => {
                    tracing::warn!(project = %g.project, error = %e, "join validator unavailable");
                    crate::rest::reject(&self.st, g.project, "validate_unavailable");
                    return Err(NetError::Forbidden("join validator unavailable".into()));
                }
            }
        }
        let joined = {
            let mut hr = h.lock();
            if hr.project != g.project {
                return Err(NetError::NotFound("room".into()));
            }
            // Same player from a second connection: the new session takes the slot, the old
            // one is told and ignored from here on (see `owns_slot`).
            if let Some(old) = hr.room.online_actor(&g.player_id) {
                if let Some(a) = hr.sessions.get(&old) {
                    let _ = a.control.try_send(ServerMsg::Left {
                        reason: "session_replaced".into(),
                    });
                }
            }
            let (actor, out) = hr
                .room
                .join(
                    &g.player_id,
                    &g.name,
                    player_props,
                    rejoin,
                    spectator,
                    Instant::now(),
                )
                .map_err(|e| {
                    if matches!(e, RoomError::Full) {
                        crate::rest::reject(&self.st, g.project, "room_full");
                    }
                    room_err(e)
                })?;
            hr.sessions.insert(
                actor,
                Attached {
                    control: self.ctl.clone(),
                    session: self.id,
                    data: None,
                },
            );
            self.st.webhooks.emit(
                hr.project,
                "player.joined",
                serde_json::json!({"room": hr.room.id().to_string(), "actor": actor.0}),
            );
            if let Some(sy) = hr.sync.as_mut() {
                sy.on_join_as(actor, spectator);
            }
            if let Some(ls) = hr.lockstep.as_mut() {
                ls.on_join_as(actor, spectator);
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
        if !self.owns_slot(&r.hosted.lock(), r.actor) {
            return;
        }
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
                let (hosted, actor) = (r.hosted.clone(), r.actor);
                for m in ds.receive(b) {
                    let mut h = hosted.lock();
                    if !self.owns_slot(&h, actor) {
                        drop(h);
                        return Err(self.replaced());
                    }
                    handle_data_frame(&mut h, actor, m.channel, m.payload, &self.st.metrics)?;
                }
                Ok(())
            }
        }
    }

    /// `stats` / `find_friends` fan out across the fleet: one per second per session.
    fn rpc_budget(&mut self) -> Result<()> {
        let now = Instant::now();
        let refill = now.duration_since(self.rpc_at).as_secs_f32() * RPC_RATE;
        self.rpc_tokens = (self.rpc_tokens + refill).min(RPC_BURST);
        self.rpc_at = now;
        if self.rpc_tokens < 1.0 {
            return Err(NetError::Conflict(
                "rpc_rate: at most 5 calls per second per session".into(),
            ));
        }
        self.rpc_tokens -= 1.0;
        Ok(())
    }

    fn lookup_budget(&mut self) -> Result<()> {
        let now = Instant::now();
        if self
            .last_lookup
            .is_some_and(|t| now.duration_since(t) < Duration::from_secs(1))
        {
            return Err(NetError::Conflict(
                "lookup_rate: one stats/find_friends per second".into(),
            ));
        }
        self.last_lookup = Some(now);
        Ok(())
    }

    async fn cleanup(&mut self) {
        if let Some(p) = self.lobby.take() {
            self.st.host.unsubscribe_lobby(p, &self.ctl);
        }
        if let Some(g) = &self.grant {
            self.st.host.presence_leave(g.project, &g.player_id);
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
    let spectator = g.room.player(actor).is_some_and(|p| p.spectator);
    if let Some(ls) = g.lockstep.as_mut() {
        for f in ls.catchup_for(spectator) {
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
    let checkpoint_every = (2_000_000 / period.as_micros().max(1)) as u64;
    let writer = RoomWriter::spawn(st.clone(), room);
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
                st.metrics.sync_lost_deltas.inc_by(sy.take_lost_deltas());
                let (limited, invalid) = sy.take_input_stats();
                st.metrics.sync_inputs_rate_limited.inc_by(limited);
                st.metrics.sync_invalid_inputs.inc_by(invalid);
                let (resyncs, starved) = sy.take_replication_stats();
                st.metrics.sync_resyncs.inc_by(resyncs);
                st.metrics.sync_aoi_starved.inc_by(starved);
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
        let took = t0.elapsed();
        if took > period {
            st.metrics.tick_overruns.inc();
        }
        st.metrics.sync_tick.observe(took.as_secs_f64());
        if let Some((project, body)) = checkpoint {
            writer.push(RedisWrite::Checkpoint(project, body));
        }
    }
}

/// One Redis write of a room's tick loop.
pub enum RedisWrite {
    Checkpoint(ProjectId, Vec<u8>),
    Journal(ProjectId, Vec<Vec<u8>>, isize),
}

/// Per-room single writer: checkpoints and journal batches reach Redis in tick order, each
/// write bounded by a timeout, with a bounded queue, so a Redis outage never stalls the tick
/// loop, piles up tasks or lets an older checkpoint land after a newer one.
pub struct RoomWriter {
    tx: mpsc::Sender<RedisWrite>,
    room: RoomId,
    /// Set when a journal batch was lost (failed, timed out or dropped): the journal in Redis
    /// has a gap, so the tick loop must write a fresh checkpoint before appending more.
    journal_broken: Arc<std::sync::atomic::AtomicBool>,
}

impl RoomWriter {
    const QUEUE: usize = 32;
    const TIMEOUT: Duration = Duration::from_secs(3);

    pub fn spawn(st: Shared, room: RoomId) -> Self {
        let (tx, mut rx) = mpsc::channel::<RedisWrite>(Self::QUEUE);
        let journal_broken = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let broken = journal_broken.clone();
        tokio::spawn(async move {
            while let Some(w) = rx.recv().await {
                let redis = st.redis.clone();
                let is_journal = matches!(w, RedisWrite::Journal(..));
                let (what, fut): (&str, std::pin::Pin<Box<dyn Future<Output = _> + Send>>) = match w
                {
                    RedisWrite::Checkpoint(project, body) => (
                        "checkpoint",
                        Box::pin(
                            async move { redis.put_checkpoint(project, room, &body, 600).await },
                        ),
                    ),
                    RedisWrite::Journal(project, entries, keep) => (
                        "journal",
                        Box::pin(async move {
                            redis
                                .append_journal(project, room, &entries, keep, 600)
                                .await
                        }),
                    ),
                };
                let ok = match tokio::time::timeout(Self::TIMEOUT, fut).await {
                    Ok(Ok(())) => true,
                    Ok(Err(e)) => {
                        tracing::warn!(%room, what, error = %e, "redis write failed");
                        false
                    }
                    Err(_) => {
                        tracing::warn!(%room, what, "redis write timed out");
                        false
                    }
                };
                if !ok {
                    st.metrics.errors.with_label_values(&["redis_write"]).inc();
                    if is_journal {
                        broken.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }
        });
        Self {
            tx,
            room,
            journal_broken,
        }
    }

    /// True once after journal batches were lost; the caller writes a checkpoint that tick.
    pub fn take_journal_broken(&self) -> bool {
        self.journal_broken
            .swap(false, std::sync::atomic::Ordering::Relaxed)
    }

    /// Queues a write; when Redis is far behind the write is dropped (bounded memory) and the
    /// next checkpoint supersedes it.
    pub fn push(&self, w: RedisWrite) {
        let is_journal = matches!(w, RedisWrite::Journal(..));
        if let Err(mpsc::error::TrySendError::Full(_)) = self.tx.try_send(w) {
            if is_journal {
                self.journal_broken
                    .store(true, std::sync::atomic::Ordering::Relaxed);
            }
            tracing::debug!(room = %self.room, "redis write queue full, dropping write");
        }
    }
}

/// Lockstep input-journal entries buffered before one Redis write (ticks).
const JOURNAL_FLUSH: usize = 6;

/// Lockstep clock: every tick bundle the inputs for that tick and fan them out.
pub async fn run_lockstep(st: Shared, room: RoomId, period: std::time::Duration) {
    let mut iv = tokio::time::interval(period);
    iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let checkpoint_every = (5_000_000 / period.as_micros().max(1)) as u64;
    // Input journal between checkpoints: every tick's bundle goes to Redis in batches of
    // JOURNAL_FLUSH ticks, so an adopting node loses at most that many ticks of inputs.
    let journal_keep = (10_000_000 / period.as_micros().max(1)) as isize;
    let mut journal_buf: Vec<Vec<u8>> = Vec::new();
    let writer = RoomWriter::spawn(st.clone(), room);
    let mut n: u64 = 0;
    loop {
        iv.tick().await;
        let Some(h) = st.host.get(room) else { return };
        let t0 = Instant::now();
        let mut checkpoint: Option<(ProjectId, Vec<u8>)> = None;
        let project;
        {
            let mut g = h.lock();
            project = g.project;
            n += 1;
            if n.is_multiple_of(checkpoint_every) || writer.take_journal_broken() {
                let snap = g.lockstep.as_ref().map(|l| l.snapshot());
                checkpoint = Some((g.project, RoomCheckpoint::of(&g, None, snap).encode()));
            }
            let master = g.room.master();
            let (bundle, desyncs, actors, ckpt_req, watch, reports, dump_reqs, dumps) = {
                let Some(ls) = g.lockstep.as_mut() else {
                    return;
                };
                ls.master = master;
                let bundle = ls.tick();
                if let Some(e) = ls.last_bundle() {
                    journal_buf.push(e.to_vec());
                }
                let ckpt_req = ls.checkpoint_request();
                let desyncs: Vec<(u32, ActorId, u8, Vec<Bytes>)> = ls
                    .take_desyncs()
                    .into_iter()
                    .map(|(tick, actor)| {
                        let strikes = ls.strike(actor);
                        // Catch-up (checkpoint + journal tail) only until the actor is untrusted:
                        // a client that keeps lying about its hash must not make the node
                        // re-send the whole state every few ticks.
                        let resync = if strikes <= crate::lockstep::UNTRUSTED_AFTER {
                            ls.on_late_join()
                        } else {
                            Vec::new()
                        };
                        (tick, actor, strikes, resync)
                    })
                    .collect();
                let actors: Vec<ActorId> = ls.actors.keys().copied().collect();
                let watch = ls.spectator_datagram().map(|d| (ls.spectators(), d));
                let reports = ls.input_reports();
                let mut dump_reqs: Vec<(ActorId, Bytes)> = Vec::new();
                for (tick, actor, _, _) in &desyncs {
                    if let Some(f) = ls.request_dump(*tick, *actor) {
                        dump_reqs.push((*actor, f));
                    }
                    // With a referee sim the node's own state is the reference (already in
                    // `dumps`); otherwise ask the master for its state at that tick.
                    if ls.has_sim() {
                        continue;
                    }
                    if let Some(m) = master.filter(|m| m != actor) {
                        if let Some(f) = ls.request_dump(*tick, m) {
                            dump_reqs.push((m, f));
                        }
                    }
                }
                let dumps = ls.take_dumps();
                (
                    bundle, desyncs, actors, ckpt_req, watch, reports, dump_reqs, dumps,
                )
            };
            for (a, f) in reports {
                if let Some(ds) = g.sessions.get(&a).and_then(|s| s.data.as_ref()) {
                    ds.send(net_common::Channel::UnreliableSequenced, f);
                }
            }
            for (a, f) in dump_reqs {
                if let Some(ds) = g.sessions.get(&a).and_then(|s| s.data.as_ref()) {
                    ds.send(net_common::Channel::ReliableOrdered, f);
                }
            }
            for d in dumps {
                let redis = st.redis.clone();
                tokio::spawn(async move {
                    if let Err(e) = redis
                        .put_desync(project, room, d.tick, d.actor.0, d.hash, &d.state)
                        .await
                    {
                        tracing::warn!(room = %room, error = %e, "desync dump not stored");
                    }
                });
            }
            if let Some((spectators, d)) = watch {
                for a in &spectators {
                    if let Some(ds) = g.sessions.get(a).and_then(|s| s.data.as_ref()) {
                        ds.send(net_common::Channel::UnreliableSequenced, d.clone());
                    }
                }
            }
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
            for (tick, actor, strikes, resync) in desyncs {
                let untrusted = strikes >= crate::lockstep::UNTRUSTED_AFTER;
                st.metrics.lockstep_desyncs.inc();
                // The desynced client reloads from the master's confirmed state + journal tail.
                if let Some(ds) = g.sessions.get(&actor).and_then(|s| s.data.as_ref()) {
                    for f in resync {
                        ds.send(net_common::Channel::ReliableOrdered, f);
                    }
                }
                st.webhooks.emit(
                    g.project,
                    "desync",
                    serde_json::json!({"room": room.to_string(), "tick": tick, "actor": actor.0, "strikes": strikes, "untrusted": untrusted}),
                );
                for a in &actors {
                    if let Some(s) = g.sessions.get(a) {
                        let _ = s.control.try_send(ServerMsg::Desync {
                            tick,
                            actor,
                            strikes,
                            untrusted,
                        });
                    }
                }
            }
        }
        if journal_buf.len() >= JOURNAL_FLUSH {
            let entries = std::mem::take(&mut journal_buf);
            writer.push(RedisWrite::Journal(project, entries, journal_keep));
        }
        let took = t0.elapsed();
        if took > period {
            st.metrics.tick_overruns.inc();
        }
        st.metrics.sync_tick.observe(took.as_secs_f64());
        if let Some((project, body)) = checkpoint {
            writer.push(RedisWrite::Checkpoint(project, body));
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
    let logic = load_logic(st, project, props, seed).await?;
    crate::sync::SyncRoom::with_logic(
        schema,
        hz,
        seed,
        crate::sync::SyncRoom::replication_from_props(props),
        logic,
    )
    .map_err(|e| NetError::Invalid(format!("schema: {e}")))
}

/// Project WASM module named by room prop `logic` (`PUT /v1/modules/:name`), if any.
pub async fn load_logic(
    st: &Shared,
    project: ProjectId,
    props: &Map<String, Value>,
    seed: u64,
) -> Result<Option<Box<dyn crate::sync::GameLogic>>> {
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
    Ok(logic)
}

/// Lockstep rooms with a `logic` module run it on the node too (server-verified lockstep).
pub async fn attach_lockstep_sim(
    st: &Shared,
    project: ProjectId,
    props: &Map<String, Value>,
    ls: &mut crate::lockstep::LockstepRoom,
) -> Result<()> {
    let Some(logic) = load_logic(st, project, props, ls.seed).await? else {
        return Ok(());
    };
    let schema = net_wire::Schema::parse(
        props
            .get("schema")
            .and_then(|v| v.as_str())
            .unwrap_or(crate::sync::ARENA_SCHEMA),
    )
    .map_err(|e| NetError::Invalid(format!("schema: {e}")))?;
    ls.attach_sim(schema, logic).map_err(NetError::Invalid)
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
                .filter(|p| !p.spectator)
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
    struct Adopting<'a>(&'a Shared, uuid::Uuid);
    impl Drop for Adopting<'_> {
        fn drop(&mut self) {
            self.0.adopting.lock().remove(&self.1);
        }
    }
    if !st.adopting.lock().insert(row.id) {
        return Err(NetError::Conflict("room_pending".into()));
    }
    let _adopting = Adopting(st, row.id);
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
        reserved: Vec::new(),
        reserved_until: None,
        cleanup_cache_on_leave: prop_bool(&props, "cleanup_cache_on_leave", true),
        max_events_per_sec: limits.max_events_per_sec_per_room,
        max_events_per_sec_per_player: limits.max_events_per_sec_per_player,
        max_event_bytes: limits.max_event_bytes,
        max_props_bytes: limits.max_props_bytes,
        max_player_props_bytes: limits.max_player_props_bytes,
        max_cached_events: limits.max_cached_events_per_room,
        offline_ttl: Duration::from_secs(prop_secs(&props, "player_ttl_secs", 30)),
        empty_ttl: Duration::from_secs(prop_secs(&props, "empty_ttl_secs", 5)),
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
            let mut ls = crate::lockstep::LockstepRoom::restore(snap)
                .with_spectator_delay(prop_secs(&props, "spectator_delay_secs", 3) as u32);
            match st.redis.get_journal(project, room_id).await {
                Ok(entries) => {
                    let applied = ls.apply_journal(&entries);
                    tracing::info!(room = %room_id, applied, tick = ls.tick.0, "journal tail applied");
                }
                Err(e) => tracing::warn!(room = %room_id, error = %e, "journal read failed"),
            }
            lockstep = Some(ls);
            restored = "lockstep";
        }
    }
    if mode == RoomMode::Sync && sync.is_none() {
        sync = Some(build_sync(st, project, &props, 0).await?);
    }
    if mode == RoomMode::Lockstep && lockstep.is_none() {
        lockstep = Some(
            crate::lockstep::LockstepRoom::new(hz, 1).with_spectator_delay(prop_secs(
                &props,
                "spectator_delay_secs",
                3,
            ) as u32),
        );
    }
    if let Some(ls) = lockstep.as_mut() {
        if let Err(e) = attach_lockstep_sim(st, project, &props, ls).await {
            tracing::warn!(room = %room_id, error = %e, "server sim not attached after adoption");
        }
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
    st.metrics.rooms_adopted.inc();
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

/// Shutdown: every hosted room gets a fresh checkpoint in Redis and becomes `orphaned` in the
/// DB, so the next node a player reaches adopts it (same path as a crashed node, minus the
/// heartbeat wait). Players get `left{reason:"rehost"}` and rejoin by room name.
/// Server-side kick (REST): removes the actor from a room hosted on this node, tells the client
/// `left{reason}`, updates sync/lockstep membership and republishes the room.
/// Removes `actor` from a room hosted here; returns the kicked player's id for the audit trail.
/// Pushes a system notice to every online player of a room hosted here; returns the player count.
pub fn notice_room(st: &Shared, room: RoomId, text: &str, level: &str) -> Result<usize> {
    let hr = st
        .host
        .get(room)
        .ok_or_else(|| NetError::NotFound("room not hosted here".into()))?;
    let h = hr.lock();
    let (out, n) = h.room.notice(text, level);
    dispatch(&h, out, &st.metrics);
    Ok(n)
}

/// Same for every room of a project hosted here: (rooms, players).
pub fn notice_project(st: &Shared, project: ProjectId, text: &str, level: &str) -> (usize, usize) {
    let (mut rooms, mut players) = (0usize, 0usize);
    for hr in st.host.all() {
        let h = hr.lock();
        if h.project != project {
            continue;
        }
        let (out, n) = h.room.notice(text, level);
        if n > 0 {
            rooms += 1;
            players += n;
            dispatch(&h, out, &st.metrics);
        }
    }
    (rooms, players)
}

pub async fn kick_player(
    st: &Shared,
    room: RoomId,
    actor: ActorId,
    reason: &str,
) -> Result<String> {
    let hr = st
        .host
        .get(room)
        .ok_or_else(|| NetError::NotFound("room not hosted here".into()))?;
    let player_id;
    {
        let mut h = hr.lock();
        player_id = h
            .room
            .player_infos()
            .into_iter()
            .find(|p| p.actor == actor)
            .map(|p| p.player_id)
            .ok_or_else(|| NetError::NotFound("actor not in room".into()))?;
        let out = h
            .room
            .admin_kick(actor, reason, Instant::now())
            .map_err(room_err)?;
        dispatch(&h, out, &st.metrics);
        h.sessions.remove(&actor);
        st.webhooks.emit(
            h.project,
            "player.left",
            serde_json::json!({"room": h.room.id().to_string(), "actor": actor.0, "kicked": true}),
        );
        if let Some(sy) = h.sync.as_mut() {
            sy.on_leave(actor);
        }
        if let Some(ls) = h.lockstep.as_mut() {
            ls.on_leave(actor);
        }
    }
    publish_room(st, &hr).await;
    Ok(player_id)
}

pub async fn handoff_all(st: &Shared) -> usize {
    let rooms = st.host.all();
    for h in &rooms {
        let (project, id, body) = {
            let mut g = h.lock();
            let sync = g.sync.as_mut().map(|s| s.checkpoint());
            let ls = g.lockstep.as_ref().map(|l| l.snapshot());
            (
                g.project,
                g.room.id(),
                RoomCheckpoint::of(&g, sync, ls).encode(),
            )
        };
        if let Err(e) = st.redis.put_checkpoint(project, id, &body, 600).await {
            tracing::warn!(room = %id, error = %e, "handoff checkpoint failed");
        }
    }
    match queries::orphan_rooms_of_node(&st.pool, st.node_id.0).await {
        Ok(n) => tracing::info!(rooms = n, "rooms handed off for adoption"),
        Err(e) => tracing::error!(error = %e, "handoff: orphan rooms"),
    }
    let n = rooms.len();
    for h in rooms {
        let (id, sessions) = {
            let g = h.lock();
            (
                g.room.id(),
                g.sessions
                    .values()
                    .map(|s| s.control.clone())
                    .collect::<Vec<_>>(),
            )
        };
        st.host.remove(id);
        for s in sessions {
            let _ = s.try_send(ServerMsg::Left {
                reason: "rehost".into(),
            });
        }
    }
    n
}

/// Upper bound for one stored replay; longer matches keep their most recent ticks.
pub const MAX_REPLAY_BYTES: usize = 16 << 20;

/// Serializes `r`, dropping the oldest ticks (and their hashes) until it fits `max_bytes`;
/// `from_tick` moves forward accordingly. None only if even an empty replay does not fit.
pub fn fit_replay(r: &mut net_rollback::Replay, max_bytes: usize) -> Option<Vec<u8>> {
    loop {
        let body = serde_json::to_vec(r).ok()?;
        if body.len() <= max_bytes {
            return Some(body);
        }
        if r.inputs.is_empty() {
            return None;
        }
        let drop = (r.inputs.len() / 2).max(1);
        r.inputs.drain(..drop);
        r.from_tick += drop as u32;
        let from = r.from_tick;
        r.hashes.retain(|(t, _)| *t >= from);
        tracing::info!(from_tick = from, "replay trimmed to fit the size cap");
    }
}

/// `room.closed` payload: name/mode/props always; with room prop `export_state_on_close` the
/// final checkpoint (players, master, sync state or lockstep snapshot) as base64 so a backend can
/// persist the match — the "persist room state via webhooks" pattern. Capped at 256 KiB.
fn closed_payload(g: &mut HostedRoom, id: RoomId) -> Value {
    const MAX_EXPORT: usize = 256 * 1024;
    let mut p = serde_json::json!({
        "room": id.to_string(),
        "name": g.room.cfg.name,
        "mode": format!("{:?}", g.room.cfg.mode).to_lowercase(),
        "props": g.room.cfg.props,
    });
    if prop_bool(&g.room.cfg.props, "export_state_on_close", false) {
        let sync = g.sync.as_mut().map(|s| s.checkpoint());
        let ls = g.lockstep.as_ref().map(|l| l.snapshot());
        let bytes = RoomCheckpoint::of(g, sync, ls).encode();
        if bytes.len() <= MAX_EXPORT {
            p["state"] = Value::String(base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                &bytes,
            ));
        } else {
            p["state_too_large"] = Value::from(bytes.len());
        }
    }
    p
}

pub async fn close_room(st: &Shared, project: ProjectId, id: RoomId) {
    let Some(h) = st.host.remove(id) else {
        st.webhooks.emit(
            project,
            "room.closed",
            serde_json::json!({"room": id.to_string()}),
        );
        return;
    };
    {
        let (sessions, replay) = {
            let mut g = h.lock();
            st.webhooks
                .emit(project, "room.closed", closed_payload(&mut g, id));
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
        if let Some(mut r) = replay {
            if let Some(body) = fit_replay(&mut r, MAX_REPLAY_BYTES) {
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
        RoomError::NotMember | RoomError::NoActor => NetError::NotFound(e.to_string()),
        RoomError::NotMaster | RoomError::Spectator => NetError::Forbidden(e.to_string()),
        RoomError::EventRate | RoomError::PlayerEventRate => NetError::RateLimited,
        RoomError::EventTooLarge | RoomError::PropsTooLarge | RoomError::CasFailed => {
            NetError::Invalid(e.to_string())
        }
    }
}

/// Room TTL props (`player_ttl_secs`, `empty_ttl_secs`), capped at 300 s.
pub fn prop_bool(props: &Map<String, Value>, key: &str, default: bool) -> bool {
    props.get(key).and_then(|v| v.as_bool()).unwrap_or(default)
}

pub fn prop_secs(props: &Map<String, Value>, key: &str, default: u64) -> u64 {
    props
        .get(key)
        .and_then(|v| v.as_u64())
        .unwrap_or(default)
        .min(300)
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

#[cfg(test)]
mod replay_fit_tests {
    use super::fit_replay;

    #[test]
    fn trims_oldest_ticks_until_it_fits() {
        let mut r = net_rollback::Replay {
            seed: 1,
            schema_hash: "h".into(),
            tick_hz: 60,
            actors: vec![net_common::ActorId(1)],
            from_tick: 0,
            inputs: (0..1000)
                .map(|_| {
                    [(net_common::ActorId(1), vec![7u8; 16])]
                        .into_iter()
                        .collect()
                })
                .collect(),
            hash_every: 100,
            hashes: (1..=10).map(|i| (i * 100, i as u64)).collect(),
        };
        let full = serde_json::to_vec(&r).unwrap().len();
        let body = fit_replay(&mut r, full / 4).unwrap();
        assert!(body.len() <= full / 4);
        assert!(r.from_tick >= 750 && r.inputs.len() + r.from_tick as usize == 1000);
        assert!(r.hashes.iter().all(|(t, _)| *t >= r.from_tick));
        let mut tiny = r.clone();
        assert!(fit_replay(&mut tiny, 10).is_none());
    }
}
