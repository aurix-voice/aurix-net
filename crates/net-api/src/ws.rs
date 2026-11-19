//! Control channel: one WebSocket per client. Text frames are JSON [`ClientMsg`]/[`ServerMsg`],
//! binary frames are tunnelled data-path datagrams (first one is the data ticket).

use crate::host::{
    dispatch, handle_data_frame, Attached, Control, DataSession, DataSink, HostedRoom,
};
use crate::state::Shared;
use axum::extract::ws::{close_code, CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use base64::Engine;
use bytes::Bytes;
use futures::{stream, SinkExt, StreamExt};
use net_auth::Grant;
use net_common::{ActorId, NetError, ProjectId, Result, RoomId, RoomMode, SessionId};
use net_control::redis_store::LiveRoom;
use net_db::queries;
use net_rooms::protocol::{ClientMsg, DataEndpoint, RoomSummary, ServerMsg, JOIN_ID, UNSEQUENCED};
use net_rooms::{EventFrame, Room, RoomConfig, RoomError};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

const IDLE_TIMEOUT: Duration = Duration::from_secs(90);
/// How far a lockstep catch-up may fill a player's link; the rest stays for live frames.
const CATCHUP_BACKLOG_BYTES: usize = net_common::limits::RELIABLE_BACKLOG_BYTES / 2;
/// A connection that has not authenticated within this window is dropped.
const AUTH_TIMEOUT: Duration = Duration::from_secs(10);
/// Control messages a client must not miss that the node holds for it.
const CONTROL_QUEUE: usize = 512;
/// Unreliable events the node holds for a client without a data path; more are dropped.
const LOSSY_QUEUE: usize = 512;
/// Why a session is closed whose client did not read a message it must not miss.
const OVERRUN: &str = "control queue overflow";
/// How long the close of such a session waits for a client that may not be reading.
const OVERRUN_CLOSE_WAIT: Duration = Duration::from_secs(1);
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
    let slot = match st.guards.ws_connects.check_key(&ip) {
        Ok(()) => st.guards.acquire_session(ip),
        Err(_) => None,
    };
    let Some(slot) = slot else {
        st.metrics.errors.with_label_values(&["rate_limited"]).inc();
        return (
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            "too many connections",
        )
            .into_response();
    };
    // A handshake that fails drops this closure, and the slot with it.
    ws.max_message_size(net_common::limits::MAX_MESSAGE + 4096)
        .on_upgrade(move |socket| async move {
            let _slot = slot;
            run(socket, st, ip).await;
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
    ctl: Control,
    bin: mpsc::Sender<Bytes>,
    grant: Option<Grant>,
    room: Option<InRoom>,
    data: Option<Arc<DataSession>>,
    /// The session's room has answered a frame that opens no tunnel.
    tunnel_refused: bool,
    lobby: Option<ProjectId>,
    /// From `auth.supports`.
    unsequenced: bool,
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
    let (ctl_tx, mut ctl_rx) = mpsc::channel::<ServerMsg>(CONTROL_QUEUE);
    let (lossy_tx, mut lossy_rx) = mpsc::channel::<ServerMsg>(LOSSY_QUEUE);
    let (bin_tx, mut bin_rx) = mpsc::channel::<Bytes>(2048);
    let ctl = Control::new(ctl_tx, lossy_tx);
    let overrun = ctl.clone();
    let writer = tokio::spawn(async move {
        // Each queue that has something gets its turn, so no one of them holds up the rest.
        let text =
            |msg: ServerMsg| Message::Text(serde_json::to_string(&msg).unwrap_or_default().into());
        let mut queues = stream::select_all([
            stream::poll_fn(move |cx| ctl_rx.poll_recv(cx))
                .map(text)
                .boxed(),
            stream::poll_fn(move |cx| bin_rx.poll_recv(cx))
                .map(Message::Binary)
                .boxed(),
            stream::poll_fn(move |cx| lossy_rx.poll_recv(cx))
                .map(text)
                .boxed(),
        ]);
        let write = async {
            while let Some(message) = queues.next().await {
                if sink.send(message).await.is_err() {
                    break;
                }
            }
        };
        tokio::select! {
            () = write => {
                let _ = sink.close().await;
            }
            () = overrun.overrun() => {
                let close = Message::Close(Some(CloseFrame {
                    code: close_code::POLICY,
                    reason: OVERRUN.into(),
                }));
                let _ = tokio::time::timeout(OVERRUN_CLOSE_WAIT, sink.send(close)).await;
            }
        }
    });

    let mut sess = Session {
        st: st.clone(),
        id: SessionId::new(),
        ctl,
        bin: bin_tx,
        grant: None,
        room: None,
        data: None,
        tunnel_refused: false,
        lobby: None,
        unsequenced: false,
        ip,
        last_lookup: None,
        rpc_tokens: RPC_BURST,
        rpc_at: Instant::now(),
    };
    sess.ctl.send(ServerMsg::Hello {
        session: sess.id.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        region: st.region.to_string(),
        supports: vec![UNSEQUENCED.to_string(), JOIN_ID.to_string()],
    });

    let mut overrun = false;
    loop {
        let limit = if sess.grant.is_some() {
            IDLE_TIMEOUT
        } else {
            AUTH_TIMEOUT
        };
        let next = tokio::select! {
            next = tokio::time::timeout(limit, stream.next()) => next,
            () = sess.ctl.overrun() => {
                sess.log_overrun();
                overrun = true;
                break;
            }
        };
        let msg = match next {
            Err(_) => break,
            Ok(None) => break,
            Ok(Some(Err(_))) => break,
            Ok(Some(Ok(m))) => m,
        };
        match msg {
            Message::Text(text) if sess.grant.is_none() && text.len() > PRE_AUTH_MAX_TEXT => {
                sess.error(NetError::Invalid("message too large before auth".into()));
                break;
            }
            Message::Text(text) => match serde_json::from_str::<ClientMsg>(&text) {
                Ok(msg) => {
                    let auth = matches!(msg, ClientMsg::Auth { .. });
                    let id = msg.request_id();
                    if let Err(e) = sess.handle(msg).await {
                        if e.is_fault() {
                            sess.log_fault(&text, &e);
                        }
                        let fatal = auth && matches!(e, NetError::RateLimited);
                        sess.refuse(id, e);
                        if fatal {
                            break;
                        }
                    }
                }
                Err(e) => sess.error(NetError::Invalid(format!("bad message: {e}"))),
            },
            Message::Binary(frame) => {
                if let Err(e) = sess.on_binary(frame) {
                    sess.error(e);
                }
            }
            Message::Close(_) => break,
            Message::Ping(_) | Message::Pong(_) => {}
        }
    }
    sess.cleanup().await;
    if !overrun {
        writer.abort();
    }
    st.metrics.control_sessions.dec();
}

impl Session {
    fn log_overrun(&self) {
        self.st.metrics.control_overflows.inc();
        let player = self.grant.as_ref().map(|grant| grant.player_id.as_str());
        tracing::warn!(session = %self.id, player, "{OVERRUN}: session closed");
    }

    fn error(&self, e: NetError) {
        self.refuse(None, e);
    }

    /// `id`: the one the refused request carried.
    fn refuse(&self, id: Option<u32>, e: NetError) {
        self.st.metrics.errors.with_label_values(&[e.code()]).inc();
        self.ctl.send(ServerMsg::Error {
            code: e.code().to_string(),
            message: e.public_message(),
            id,
        });
    }

    /// The player is not told what failed inside the node; `frame` is the message it failed on.
    fn log_fault(&self, frame: &str, e: &NetError) {
        #[derive(serde::Deserialize)]
        struct Typed<'a> {
            t: &'a str,
        }
        let request = serde_json::from_str::<Typed>(frame).map_or("", |typed| typed.t);
        match &self.grant {
            Some(grant) => tracing::error!(
                error = %e, request, project = %grant.project, player = %grant.player_id,
                "control message failed"
            ),
            None => tracing::error!(error = %e, request, "control message failed"),
        }
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
    fn owns_slot(&self, hosted: &HostedRoom, actor: ActorId) -> bool {
        hosted
            .sessions
            .get(&actor)
            .is_some_and(|a| a.session == self.id)
    }

    /// Forgets a room the session is no longer in (kicked, the room closed): `left` has told
    /// the player. Returns whether another session of the player took the slot over instead.
    fn slot_taken_over(&mut self) -> bool {
        let Some(joined) = &self.room else {
            return false;
        };
        let holder = joined
            .hosted
            .lock()
            .sessions
            .get(&joined.actor)
            .map(|attached| attached.session);
        if holder.is_none() {
            self.room = None;
            self.data = None;
        }
        holder.is_some_and(|session| session != self.id)
    }

    fn replaced(&mut self) -> NetError {
        self.room = None;
        self.data = None;
        NetError::Conflict("session_replaced".into())
    }

    async fn handle(&mut self, msg: ClientMsg) -> Result<()> {
        if self.slot_taken_over() {
            // Only a message meant for that room is refused; the session is free to create
            // or join another one.
            let error = self.replaced();
            if matches!(
                msg,
                ClientMsg::SetRoomProps { .. }
                    | ClientMsg::SetPlayerProps { .. }
                    | ClientMsg::Event { .. }
                    | ClientMsg::SetGroups { .. }
                    | ClientMsg::CacheSlice { .. }
                    | ClientMsg::SetView { .. }
                    | ClientMsg::SetMaster { .. }
                    | ClientMsg::Kick { .. }
            ) {
                return Err(error);
            }
        }
        match msg {
            ClientMsg::Auth { grant, supports } => {
                self.unsequenced = supports.iter().any(|s| s == UNSEQUENCED);
                let checked = match self.st.grants.verify(&grant) {
                    Ok(verified) => verified,
                    Err(e) => {
                        return Err(if self.st.guards.auth_fail.check_key(&self.ip).is_ok() {
                            e
                        } else {
                            NetError::RateLimited
                        });
                    }
                };
                self.ctl.send(ServerMsg::Authed {
                    player_id: checked.player_id.clone(),
                    name: checked.name.clone(),
                });
                if let Some(old) = &self.grant {
                    self.st.host.presence_leave(old.project, &old.player_id);
                }
                self.st
                    .host
                    .presence_enter(checked.project, &checked.player_id);
                self.grant = Some(checked);
                Ok(())
            }
            ClientMsg::GetRooms { expr, limit, id } => {
                let grant = self.grant()?.clone();
                let filter = match expr.as_deref().map(str::trim) {
                    None | Some("") => None,
                    Some(expected) => Some(
                        crate::mmfilter::Filter::parse(expected)
                            .map_err(|e| NetError::Invalid(format!("expr: {e}")))?,
                    ),
                };
                self.lookup_budget()?;
                let limit = usize::from(limit.unwrap_or(100).clamp(1, 500));
                let rooms = self
                    .lobby_rooms(grant.project, &grant.player_id)
                    .await
                    .into_iter()
                    .filter(|r| filter.as_ref().is_none_or(|f| f.matches(&r.props)))
                    .take(limit)
                    .collect();
                self.ctl.send(ServerMsg::Rooms { rooms, id });
                Ok(())
            }
            ClientMsg::Rpc { id, name, body } => {
                let grant = self.grant()?.clone();
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
                    "player_id": grant.player_id,
                    "actor": room.as_ref().map(|r| r.1),
                    "room": room.as_ref().map(|r| r.0.clone()),
                    "body": body,
                });
                let (hooks, ctl) = (self.st.webhooks.clone(), self.ctl.clone());
                tokio::spawn(async move {
                    let out = hooks.rpc(grant.project, payload).await;
                    ctl.send(ServerMsg::RpcResult {
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
                self.ctl.send(ServerMsg::Pong {
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
                reserved,
                id,
            } => {
                let grant = self.grant()?.clone();
                self.ensure_not_in_room()?;
                let name = name.unwrap_or_else(short_id);
                self.check_room_scope(&grant, &name)?;
                let handle = self
                    .create_room(
                        &grant,
                        name,
                        mode,
                        max_players,
                        visible,
                        open,
                        props,
                        lobby_props,
                        region,
                        reserved,
                    )
                    .await?;
                self.join_created(handle, player_props, id).await
            }
            ClientMsg::Join {
                name,
                player_props,
                rejoin_actor,
                schema_hash,
                spectator,
                id,
            } => {
                let grant = self.grant()?.clone();
                self.ensure_not_in_room()?;
                self.check_room_scope(&grant, &name)?;
                let handle = {
                    let mut attempt = 0;
                    loop {
                        match self.find_room(grant.project, &name).await {
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
                if let Some(hash) = schema_hash.as_deref() {
                    let hr = handle.lock();
                    if let Some(sy) = hr.sync.as_ref() {
                        if sy.schema_hash != hash {
                            return Err(NetError::SchemaMismatch {
                                client: hash.to_string(),
                                server: sy.schema_hash.clone(),
                            });
                        }
                    }
                }
                self.join_hosted(handle, player_props, rejoin_actor, spectator, false, id)
                    .await
            }
            ClientMsg::JoinOrCreate {
                name,
                mode,
                max_players,
                visible,
                open,
                props,
                lobby_props,
                region,
                player_props,
                reserved,
                id,
            } => {
                let grant = self.grant()?.clone();
                self.ensure_not_in_room()?;
                self.check_room_scope(&grant, &name)?;
                // Several players may race to create the same room; whoever loses the DB
                // uniqueness race joins the winner's room.
                let mut attempt = 0;
                let (handle, created) = loop {
                    match self.find_room(grant.project, &name).await {
                        Ok(found) => {
                            let existing = found.lock().room.cfg.mode;
                            if existing != mode {
                                return Err(NetError::Conflict(format!(
                                    "room exists in {existing:?} mode, requested {mode:?}"
                                )));
                            }
                            break (found, false);
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
                            &grant,
                            name.clone(),
                            mode,
                            max_players,
                            visible,
                            open,
                            props.clone(),
                            lobby_props.clone(),
                            region.clone(),
                            reserved.clone(),
                        )
                        .await
                    {
                        Ok(created) => break (created, true),
                        Err(NetError::Conflict(_)) if attempt < 30 => {
                            attempt += 1;
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                        Err(e) => return Err(e),
                    }
                };
                if created {
                    self.join_created(handle, player_props, id).await
                } else {
                    self.join_hosted(handle, player_props, None, false, false, id)
                        .await
                }
            }
            ClientMsg::JoinRandom {
                filter,
                expr,
                match_type,
                player_props,
                id,
            } => {
                let grant = self.grant()?.clone();
                self.ensure_not_in_room()?;
                if let Some(match_type) = match_type.as_deref() {
                    if !crate::matchmaking::MATCH_TYPES.contains(&match_type) {
                        return Err(NetError::Invalid(
                            "match_type: fill, serial or random".into(),
                        ));
                    }
                }
                let expr = match expr.as_deref() {
                    Some(expected) => Some(
                        crate::mmfilter::Filter::parse(expected)
                            .map_err(|e| NetError::Invalid(format!("expr: {e}")))?,
                    ),
                    None => None,
                };
                let accepts = |props: &Map<String, Value>, lobby: &Map<String, Value>| {
                    filter.iter().all(|(k, v)| props.get(k) == Some(v))
                        && expr.as_ref().is_none_or(|f| f.matches(lobby))
                };
                // A grant limited to one room may only land in that room.
                let in_scope = |name: &str| grant.room.as_deref().is_none_or(|r| r == name);
                // Local rooms first (no redirect), then the region-wide list from Redis.
                let mut local: Vec<net_control::LiveRoom> = Vec::new();
                for handle in self.st.host.all() {
                    let hosted = handle.lock();
                    if hosted.project != grant.project
                        || !hosted.room.is_listed_for(&grant.player_id, Instant::now())
                        || !in_scope(&hosted.room.cfg.name)
                    {
                        continue;
                    }
                    let lobby: Map<String, Value> =
                        hosted.room.summary().props.into_iter().collect();
                    if accepts(&hosted.room.cfg.props, &lobby) {
                        local.push(net_control::LiveRoom {
                            room: hosted.room.id(),
                            node: self.st.node_id.to_string(),
                            name: hosted.room.cfg.name.clone(),
                            mode: hosted.room.cfg.mode.to_string(),
                            players: hosted.room.players().count() as u16,
                            held_for: Vec::new(),
                            max_players: hosted.room.cfg.max_players,
                            open: true,
                            visible: true,
                            props: Value::Object(lobby),
                        });
                    }
                }
                let refs: Vec<&net_control::LiveRoom> = local.iter().collect();
                if let Some(pick) = crate::matchmaking::pick_room(&refs, match_type.as_deref()) {
                    let handle = self
                        .st
                        .host
                        .by_name(grant.project, &pick.name)
                        .ok_or_else(|| NetError::NotFound("no matching room".into()))?;
                    return self
                        .join_hosted(handle, player_props, None, false, false, id)
                        .await;
                }
                let me = self.st.node_id.to_string();
                let remote: Vec<net_control::LiveRoom> = self
                    .st
                    .redis
                    .list_rooms(grant.project)
                    .await
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|r| {
                        r.node != me
                            && r.is_listed_for(&grant.player_id)
                            && in_scope(&r.name)
                            && filter.is_empty()
                            && expr
                                .as_ref()
                                .is_none_or(|f| r.props.as_object().is_some_and(|p| f.matches(p)))
                    })
                    .collect();
                let refs: Vec<&net_control::LiveRoom> = remote.iter().collect();
                let pick = crate::matchmaking::pick_room(&refs, match_type.as_deref())
                    .ok_or_else(|| NetError::NotFound("no matching room".into()))?;
                let node = room_host(&self.st, pick)
                    .await?
                    .ok_or_else(|| NetError::NotFound("no matching room".into()))?;
                Err(NetError::Conflict(format!(
                    "room_elsewhere:{}#{}",
                    node.http_url, pick.name
                )))
            }
            ClientMsg::Leave { id } => {
                let room = self
                    .room
                    .as_ref()
                    .map(|joined| joined.hosted.lock().room.id().to_string());
                self.leave(false).await;
                self.ctl.send(ServerMsg::Left {
                    reason: "left".into(),
                    room,
                    id,
                });
                Ok(())
            }
            ClientMsg::SetRoomProps {
                props,
                expected,
                visible,
                open,
            } => {
                let joined = self.in_room()?;
                {
                    let mut hosted = joined.hosted.lock();
                    let out = hosted
                        .room
                        .set_room_props(joined.actor, props, expected.as_ref(), visible, open)
                        .map_err(room_err)?;
                    dispatch(&hosted, out, &self.st.metrics);
                }
                self.publish_current().await;
                Ok(())
            }
            ClientMsg::SetPlayerProps { props, expected } => {
                let joined = self.in_room()?;
                let mut hosted = joined.hosted.lock();
                let out = hosted
                    .room
                    .set_player_props(joined.actor, props, expected.as_ref())
                    .map_err(room_err)?;
                dispatch(&hosted, out, &self.st.metrics);
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
                let joined = self.in_room()?;
                let payload = B64
                    .decode(data)
                    .map_err(|_| NetError::Invalid("event data is not base64".into()))?;
                let frame = EventFrame {
                    code,
                    target,
                    cache,
                    group,
                    from: joined.actor,
                    payload: Bytes::from(payload),
                };
                let mut hosted = joined.hosted.lock();
                if hosted.leaving {
                    return Err(NetError::Conflict("node_draining".into()));
                }
                let out = hosted
                    .room
                    .raise_event(joined.actor, frame, channel, Instant::now())
                    .map_err(room_err)?;
                dispatch(&hosted, out, &self.st.metrics);
                Ok(())
            }
            ClientMsg::SetGroups { join, leave } => {
                let joined = self.in_room()?;
                joined
                    .hosted
                    .lock()
                    .room
                    .set_groups(joined.actor, &join, &leave)
                    .map_err(room_err)
            }
            ClientMsg::CacheSlice { op, index } => {
                let joined = self.in_room()?;
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
                joined.hosted.lock().room.cache_slice_op(op);
                Ok(())
            }
            ClientMsg::SetView {
                viewpoints,
                interest,
            } => {
                let joined = self.in_room()?;
                let mut hosted = joined.hosted.lock();
                match hosted.sync.as_mut() {
                    Some(sy) => {
                        sy.set_view(joined.actor, &viewpoints, &interest);
                        Ok(())
                    }
                    None => Err(NetError::Invalid("set_view: sync rooms only".into())),
                }
            }
            ClientMsg::SetMaster { actor } => {
                let joined = self.in_room()?;
                let mut hosted = joined.hosted.lock();
                let out = hosted
                    .room
                    .set_master(joined.actor, actor)
                    .map_err(room_err)?;
                dispatch(&hosted, out, &self.st.metrics);
                Ok(())
            }
            ClientMsg::Kick { actor, reason } => {
                let joined = self.in_room()?;
                {
                    let mut hosted = joined.hosted.lock();
                    let out = hosted
                        .room
                        .kick(joined.actor, actor, &reason, Instant::now())
                        .map_err(room_err)?;
                    dispatch(&hosted, out, &self.st.metrics);
                    hosted.sessions.remove(&actor);
                    self.st.webhooks.emit(
                        hosted.project,
                        "player.left",
                        serde_json::json!({"room": hosted.room.id().to_string(), "actor": actor.0, "kicked": true}),
                    );
                    if let Some(sy) = hosted.sync.as_mut() {
                        sy.on_leave(actor);
                    }
                    if let Some(ls) = hosted.lockstep.as_mut() {
                        ls.on_leave(actor);
                    }
                }
                self.publish_current().await;
                Ok(())
            }
            ClientMsg::Stats { id } => {
                let grant = self.grant()?.clone();
                self.lookup_budget()?;
                let rooms: Vec<RoomSummary> = match self.st.redis.list_rooms(grant.project).await {
                    Ok(live) => live.into_iter().map(live_to_summary).collect(),
                    Err(_) => self.st.host.listed(grant.project, &grant.player_id),
                };
                let (rooms, players, lobbies) = crate::presence::lobby_stats(&rooms);
                self.ctl.send(ServerMsg::Stats {
                    rooms,
                    players,
                    lobbies,
                    id,
                });
                Ok(())
            }
            ClientMsg::FindFriends { players, id } => {
                let grant = self.grant()?.clone();
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
                let friends =
                    crate::presence::find_friends(&self.st, grant.project, &players).await;
                self.ctl.send(ServerMsg::Friends { friends, id });
                Ok(())
            }
            ClientMsg::Lobby { subscribe } => {
                let grant = self.grant()?.clone();
                if subscribe {
                    self.st
                        .host
                        .subscribe_lobby(grant.project, &grant.player_id, self.ctl.clone());
                    self.lobby = Some(grant.project);
                    let rooms = self.lobby_rooms(grant.project, &grant.player_id).await;
                    self.st.host.tell_lobby_watcher(
                        grant.project,
                        &self.ctl,
                        ServerMsg::LobbyRooms { rooms },
                    );
                } else if let Some(lobby) = self.lobby.take() {
                    self.st.host.unsubscribe_lobby(lobby, &self.ctl);
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

    fn check_room_scope(&self, grant: &Grant, name: &str) -> Result<()> {
        if let Some(allowed) = &grant.room {
            if allowed != name {
                crate::rest::reject(&self.st, grant.project, "grant");
                return Err(NetError::Forbidden(format!(
                    "grant is limited to room {allowed}"
                )));
            }
        }
        Ok(())
    }

    /// Refuses suspended projects and banned players (by id, IP or device) and records where the
    /// player was last seen. Every way into a room goes through here.
    async fn admit(&self, grant: &Grant) -> Result<net_db::models::ProjectRow> {
        let project = queries::get_project(&self.st.pool, grant.project.0).await?;
        if project.disabled_at.is_some() {
            crate::rest::reject(&self.st, grant.project, "suspended");
            return Err(NetError::Forbidden("project disabled".into()));
        }
        if let Some(ban) =
            queries::get_ban(&self.st.pool, grant.project.0, &grant.player_id).await?
        {
            crate::rest::reject(&self.st, grant.project, "banned");
            return Err(NetError::Forbidden(crate::rest::ban_message(&ban)));
        }
        let device: String = grant
            .meta
            .as_ref()
            .and_then(|m| m.get("device"))
            .and_then(|d| d.as_str())
            .unwrap_or("")
            .chars()
            .take(128)
            .collect();
        let ip = if self.ip.is_unspecified() {
            String::new()
        } else {
            self.ip.to_string()
        };
        for (kind, value) in [("ip", ip.as_str()), ("device", device.as_str())] {
            if value.is_empty() {
                continue;
            }
            if let Some(mark) =
                queries::active_mark(&self.st.pool, grant.project.0, kind, value).await?
            {
                crate::rest::reject(&self.st, grant.project, "banned");
                return Err(NetError::Forbidden(crate::rest::mark_message(&mark)));
            }
        }
        if let Err(e) = queries::touch_player_seen(
            &self.st.pool,
            grant.project.0,
            &grant.player_id,
            &ip,
            &device,
            &grant.name,
        )
        .await
        {
            tracing::warn!(error = %e, "player_seen update failed");
        }
        Ok(project)
    }

    async fn lobby_rooms(&self, project: ProjectId, player: &str) -> Vec<RoomSummary> {
        let live = self.st.redis.list_rooms(project).await;
        rooms_for(&self.st, project, &live, player)
    }

    async fn find_room(&self, project: ProjectId, name: &str) -> Result<Arc<Mutex<HostedRoom>>> {
        if self.st.is_draining() {
            return Err(NetError::Conflict("node_draining".into()));
        }
        if let Some(handle) = self.st.host.by_name(project, name) {
            return Ok(handle);
        }
        match queries::find_open_room(&self.st.pool, project.0, name).await? {
            None => Err(NetError::NotFound(format!("room {name}"))),
            Some(row) if row.state == "orphaned" => adopt_room(&self.st, row).await,
            Some(row) => {
                let host = row.node_id.map(net_common::NodeId::from_uuid);
                let node = match host {
                    Some(node) => self.st.registry.resolve(node).await?,
                    None => None,
                };
                match node {
                    Some(node) if node.id != self.st.node_id => Err(NetError::Conflict(format!(
                        "room_elsewhere:{}",
                        node.http_url
                    ))),
                    // Adoption in progress on another session of this node.
                    _ if self.st.adopting.lock().contains(&row.id) => {
                        Err(NetError::Conflict("room_pending".into()))
                    }
                    _ => match self.st.host.get(RoomId(row.id)) {
                        // Another session of this node brought the room up while the row
                        // was read.
                        Some(handle) => Ok(handle),
                        // Row exists for this node but the room is not hosted yet: creation
                        // in progress on another session, or a row left over from a crash.
                        None if row.node_id == Some(self.st.node_id.0)
                            && (net_db::Timestamp::now() - row.created_at).whole_seconds() < 10 =>
                        {
                            Err(NetError::Conflict("room_pending".into()))
                        }
                        // The host is gone and its peers have not reaped it yet; once they
                        // do the room is adopted by the next join.
                        None if host.is_some_and(|n| n != self.st.node_id) => {
                            Err(NetError::Conflict("room_pending".into()))
                        }
                        None => {
                            let _ = queries::close_room(&self.st.pool, row.id).await;
                            Err(NetError::NotFound(format!("room {name}")))
                        }
                    },
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn create_room(
        &self,
        grant: &Grant,
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
        if !grant.can("create") {
            crate::rest::reject(&self.st, grant.project, "grant");
            return Err(NetError::Forbidden("grant may not create rooms".into()));
        }
        if self.st.is_draining() {
            return Err(NetError::Conflict("node_draining".into()));
        }
        crate::rest::validate_room_create(&name, &lobby_props)?;
        let mut lockstep = if mode == RoomMode::Lockstep {
            let hz = props.get("tick_hz").and_then(|v| v.as_u64()).unwrap_or(60) as u16;
            let seed = room_seed();
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
            let seed = room_seed();
            Some(build_sync(&self.st, grant.project, &props, seed).await?)
        } else {
            None
        };
        if let Some(ls) = lockstep.as_mut() {
            attach_lockstep_sim(&self.st, grant.project, &props, ls).await?;
        }
        if let Some(wanted) = region.as_deref() {
            if wanted != self.st.region.as_str() {
                return Err(NetError::Conflict(format!(
                    "wrong_region:{}:{}",
                    self.st.region, wanted
                )));
            }
        }
        let project = self.admit(grant).await?;
        let limits = crate::rest::project_limits(&self.st, &project)?;
        if let Some(allowed) = &limits.allowed_regions {
            if !allowed.iter().any(|r| r == self.st.region.as_str()) {
                crate::rest::reject(&self.st, grant.project, "region");
                return Err(NetError::Forbidden("region not allowed for project".into()));
            }
        }
        let open_rooms = queries::count_open_rooms(&self.st.pool, grant.project.0).await?;
        if open_rooms >= limits.max_rooms as i64 {
            crate::rest::reject(&self.st, grant.project, "quota");
            return Err(NetError::QuotaExceeded(format!(
                "max {} open rooms",
                limits.max_rooms
            )));
        }
        crate::rest::check_usage_cap(&self.st, grant.project.0, &limits).await?;
        if self.st.host.len() >= self.st.cfg.limits.max_rooms_per_node as usize {
            crate::rest::reject(&self.st, grant.project, "capacity");
            return Err(NetError::QuotaExceeded("node is full".into()));
        }
        let max_players = max_players
            .unwrap_or(limits.max_players_per_room.min(16))
            .clamp(1, limits.max_players_per_room);
        if net_rooms::room::props_len(&props) > limits.max_props_bytes {
            return Err(NetError::Invalid("props too large".into()));
        }
        if reserved.len() > crate::rest::MAX_RESERVED {
            return Err(NetError::Invalid(format!(
                "reserved: at most {} players",
                crate::rest::MAX_RESERVED
            )));
        }
        // The creator takes a seat himself.
        let held = reserved.iter().filter(|r| **r != grant.player_id).count();
        if held >= usize::from(max_players) {
            return Err(NetError::Invalid(crate::rest::RESERVED_OVER_SEATS.into()));
        }
        let room_id = RoomId::new();
        let cfg = RoomConfig {
            id: room_id,
            project: grant.project,
            name: name.clone(),
            mode,
            max_players,
            visible,
            open,
            props: props.clone(),
            lobby_props: lobby_props.clone(),
            reserved: reserved.clone(),
            reserved_until: (!reserved.is_empty())
                .then(|| std::time::Instant::now() + Duration::from_secs(60)),
            cleanup_cache_on_leave: prop_bool(&props, "cleanup_cache_on_leave", true),
            publish_user_id: prop_bool(&props, "publish_user_id", true),
            suppress_room_events: prop_bool(&props, "suppress_room_events", false),
            suppress_player_info: prop_bool(&props, "suppress_player_info", false),
            broadcast_props_to_all: prop_bool(&props, "broadcast_props_to_all", true),
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
                project_id: grant.project.0,
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
        let handle = self
            .st
            .host
            .insert(grant.project, Room::new(cfg, Instant::now()));
        self.st.webhooks.emit(
            grant.project,
            "room.created",
            serde_json::json!({"room": room_id.to_string(), "name": handle.lock().room.cfg.name, "mode": format!("{mode:?}").to_lowercase(), "region": self.st.region.to_string()}),
        );
        if let Some(sy) = sync {
            let period = sy.tick_period();
            handle.lock().sync = Some(sy);
            tokio::spawn(run_sync(self.st.clone(), room_id, period));
        }
        if let Some(ls) = lockstep {
            let period = ls.tick_period();
            handle.lock().lockstep = Some(ls);
            tokio::spawn(run_lockstep(self.st.clone(), room_id, period));
        }
        self.st.metrics.rooms.set(self.st.host.len() as i64);
        Ok(handle)
    }

    /// Joins the room this call has just created. A room its creator did not get into is
    /// taken down.
    async fn join_created(
        &mut self,
        handle: Arc<Mutex<HostedRoom>>,
        player_props: Map<String, Value>,
        asked: Option<u32>,
    ) -> Result<()> {
        let joined = self
            .join_hosted(handle.clone(), player_props, None, false, true, asked)
            .await;
        if joined.is_err() {
            let (empty, project, id) = {
                let hosted = handle.lock();
                (hosted.room.is_empty(), hosted.project, hosted.room.id())
            };
            if empty {
                close_room(&self.st, project, id).await;
            }
        }
        joined
    }

    async fn join_hosted(
        &mut self,
        handle: Arc<Mutex<HostedRoom>>,
        player_props: Map<String, Value>,
        rejoin: Option<ActorId>,
        spectator: bool,
        created: bool,
        asked: Option<u32>,
    ) -> Result<()> {
        let grant = self.grant()?.clone();
        let project = self.admit(&grant).await?;
        // A player coming back to a slot they still hold was validated and counted when they
        // first joined; anyone else is a new arrival, whatever `rejoin_actor` says.
        let returning = handle.lock().room.actor_of(&grant.player_id).is_some();
        if !returning {
            let (room_id, room_name, mode, props) = {
                let hr = handle.lock();
                if hr.project != grant.project {
                    return Err(NetError::NotFound("room".into()));
                }
                (
                    hr.room.cfg.id,
                    hr.room.cfg.name.clone(),
                    hr.room.cfg.mode.as_str(),
                    hr.room.cfg.props.clone(),
                )
            };
            let payload = serde_json::json!({
                "room": room_id.to_string(), "name": room_name, "mode": mode, "room_props": props,
                "player_id": grant.player_id, "player_name": grant.name, "player_props": player_props,
                "spectator": spectator,
            });
            match self.st.webhooks.validate_join(grant.project, payload).await {
                crate::webhooks::JoinVerdict::Allow => {}
                crate::webhooks::JoinVerdict::Deny(reason) => {
                    crate::rest::reject(&self.st, grant.project, "validate");
                    return Err(NetError::Forbidden(format!("join rejected: {reason}")));
                }
                crate::webhooks::JoinVerdict::Unavailable(reason) => {
                    tracing::warn!(project = %grant.project, error = %reason, "join validator unavailable");
                    crate::rest::reject(&self.st, grant.project, "validate_unavailable");
                    return Err(NetError::Forbidden("join validator unavailable".into()));
                }
            }
        }
        if !returning && !spectator {
            let limits = crate::rest::project_limits(&self.st, &project)?;
            if let Some(cap) = limits.max_concurrent_players {
                let online: u64 = self
                    .st
                    .redis
                    .list_rooms(grant.project)
                    .await?
                    .iter()
                    .map(|r| r.players as u64)
                    .sum();
                if online >= cap as u64 {
                    crate::rest::reject(&self.st, grant.project, "capacity");
                    return Err(NetError::QuotaExceeded(format!(
                        "project is at its concurrent player cap ({cap})"
                    )));
                }
            }
        }
        let joined = {
            let mut hr = handle.lock();
            if hr.project != grant.project {
                return Err(NetError::NotFound("room".into()));
            }
            if hr
                .lockstep
                .as_ref()
                .is_some_and(|ls| !ls.takes_late_joiners())
            {
                return Err(NetError::Conflict(
                    "no_checkpoint: the journal no longer holds the ticks after the last saved state of the match; the game has to send checkpoints to take players this late".into(),
                ));
            }
            // Same player from a second connection: the new session takes the slot, the old
            // one is told and ignored from here on (see `owns_slot`).
            if let Some(old) = hr.room.online_actor(&grant.player_id) {
                if let Some(session) = hr.sessions.get(&old) {
                    session.control.send(ServerMsg::Left {
                        reason: "session_replaced".into(),
                        room: Some(hr.room.id().to_string()),
                        id: None,
                    });
                }
            }
            let now = Instant::now();
            let admitted = if created {
                hr.room
                    .join_created(&grant.player_id, &grant.name, player_props, now)
            } else {
                hr.room.join(
                    &grant.player_id,
                    &grant.name,
                    player_props,
                    rejoin,
                    spectator,
                    now,
                )
            };
            let (actor, out) = admitted.map_err(|e| {
                if matches!(e, RoomError::Full { .. }) {
                    crate::rest::reject(&self.st, grant.project, "room_full");
                }
                room_err(e)
            })?;
            hr.sessions.insert(
                actor,
                Attached {
                    control: self.ctl.clone(),
                    session: self.id,
                    data: None,
                    unsequenced: self.unsequenced,
                    frame_errors: governor::RateLimiter::direct(crate::ratelimit::per_second(
                        self.st.cfg.limits.frame_errors_per_sec_per_player,
                    )),
                },
            );
            self.st.webhooks.emit(
                hr.project,
                "player.joined",
                serde_json::json!({"room": hr.room.id().to_string(), "actor": actor.0}),
            );
            // The room decides: someone back in the slot they hold keeps the role they first
            // joined with, whatever this request asked for.
            let spectator = hr.room.player(actor).is_some_and(|p| p.spectator);
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
                players: hr.room.roster_for(actor),
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
                next_spawn_tag: hr.sync.as_ref().and_then(|s| s.next_spawn_tag(actor)),
                id: asked,
            }
        };
        let (id, actor) = match &joined {
            ServerMsg::Joined { room, actor, .. } => (room.parse::<RoomId>().unwrap(), *actor),
            _ => unreachable!(),
        };
        self.room = Some(InRoom {
            id,
            actor,
            hosted: handle,
        });
        self.tunnel_refused = false;
        self.ctl.send(joined);
        self.st.metrics.players.set(self.st.host.players() as i64);
        self.publish_current().await;
        Ok(())
    }

    /// Pushes the room's live record to Redis (which fans out lobby updates to all nodes).
    async fn publish_current(&self) {
        if let Some(joined) = &self.room {
            publish_room(&self.st, &joined.hosted).await;
        }
    }

    async fn leave(&mut self, offline: bool) {
        if let Some(data) = self.data.take() {
            match &data.sink {
                DataSink::Quic(conn) => conn.close(0u32.into(), b"left"),
                DataSink::WebTransport(conn) => conn.close(0u32.into(), b"left"),
                DataSink::Ws(_) => {}
            }
        }
        let Some(joined) = self.room.take() else {
            return;
        };
        if !self.owns_slot(&joined.hosted.lock(), joined.actor) {
            return;
        }
        let (should_close, project, id) = {
            let mut hosted = joined.hosted.lock();
            let out = hosted.room.leave(joined.actor, offline, Instant::now());
            dispatch(&hosted, out, &self.st.metrics);
            hosted.sessions.remove(&joined.actor);
            self.st.webhooks.emit(
                hosted.project,
                "player.left",
                serde_json::json!({"room": hosted.room.id().to_string(), "actor": joined.actor.0}),
            );
            if let Some(sy) = hosted.sync.as_mut() {
                sy.on_leave(joined.actor);
            }
            if let Some(ls) = hosted.lockstep.as_mut() {
                ls.on_leave(joined.actor);
            }
            (
                hosted.room.is_empty() && !offline,
                hosted.project,
                hosted.room.id(),
            )
        };
        self.st.metrics.players.set(self.st.host.players() as i64);
        if should_close {
            close_room(&self.st, project, id).await;
        } else {
            publish_room(&self.st, &joined.hosted).await;
        }
    }

    fn on_binary(&mut self, frame: Bytes) -> Result<()> {
        if self.slot_taken_over() {
            return Err(self.replaced());
        }
        let ticket = self.st.tickets.verify(&frame);
        let Some(joined) = &self.room else {
            // Frames that were on their way when the player left its room, its own ticket
            // among them, are not answered.
            return match ticket {
                Some(ticket) if ticket.session != self.id => Err(mismatched_ticket()),
                _ => Ok(()),
            };
        };
        let (hosted, actor) = (joined.hosted.clone(), joined.actor);
        let lost = self.data.as_ref().is_some_and(|ds| ds.lost());
        if let Some(ds) = self.data.as_ref().filter(|_| !lost) {
            let mut room = hosted.lock();
            for message in ds.receive(frame) {
                if !self.owns_slot(&room, actor) {
                    return Ok(());
                }
                handle_data_frame(
                    &mut room,
                    actor,
                    message.channel,
                    message.payload,
                    &self.st.metrics,
                );
            }
            return Ok(());
        }
        let Some(ticket) = ticket else {
            // A tunnel is refused once; what its client still sends after that is dropped.
            if std::mem::replace(&mut self.tunnel_refused, true) {
                return Ok(());
            }
            return Err(if lost {
                NetError::Transport("data path closed: link dead".into())
            } else {
                NetError::Unauthorized("bad data ticket".into())
            });
        };
        if ticket.room != joined.id || ticket.actor != actor || ticket.session != self.id {
            return Err(mismatched_ticket());
        }
        let project = hosted.lock().project;
        let ds = self.st.host.data_session(
            project,
            DataSink::Ws(self.bin.clone()),
            self.st.metrics.clone(),
            Duration::from_millis(self.st.cfg.transport.idle_timeout_ms),
        );
        let ds = attach_data(&hosted, actor, ds);
        spawn_ticker(&self.st, ds.clone());
        self.data = Some(ds);
        self.tunnel_refused = false;
        Ok(())
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
                "lookup_rate: one stats/find_friends/get_rooms per second".into(),
            ));
        }
        self.last_lookup = Some(now);
        Ok(())
    }

    async fn cleanup(&mut self) {
        if let Some(lobby) = self.lobby.take() {
            self.st.host.unsubscribe_lobby(lobby, &self.ctl);
        }
        if let Some(grant) = &self.grant {
            self.st.host.presence_leave(grant.project, &grant.player_id);
        }
        self.leave(true).await;
    }
}

pub fn attach_data(
    handle: &Arc<Mutex<HostedRoom>>,
    actor: ActorId,
    ds: Arc<DataSession>,
) -> Arc<DataSession> {
    let mut hosted = handle.lock();
    if let Some(session) = hosted.sessions.get_mut(&actor) {
        session.data = Some(ds.clone());
    }
    // An empty reliable frame tells the client the path is open; a lockstep room past
    // tick 0 then streams the catch-up (checkpoint + journal tail) on the same channel.
    ds.send(net_common::Channel::ReliableOrdered, Bytes::new());
    let spectator = hosted.room.player(actor).is_some_and(|p| p.spectator);
    if let Some(ls) = hosted.lockstep.as_mut() {
        if let Some(frame) = ls.catchup_for(actor, spectator) {
            ds.send(net_common::Channel::ReliableOrdered, frame);
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
    let mut ticks: u64 = 0;
    loop {
        iv.tick().await;
        let Some(handle) = st.host.get(room) else {
            return;
        };
        let t0 = Instant::now();
        let mut checkpoint: Option<(ProjectId, Vec<u8>)> = None;
        {
            let mut hosted = handle.lock();
            let project = hosted.project;
            let (out, reports, spawned, events, actors) = {
                let room = &mut *hosted;
                let Some(sy) = room.sync.as_mut() else {
                    return;
                };
                let sessions = &room.sessions;
                let wired = |actor| sessions.get(&actor).is_some_and(|s| s.data.is_some());
                let out = sy.tick();
                for (actor, _) in out.iter().filter(|(actor, _)| !wired(*actor)) {
                    sy.undelivered(*actor);
                }
                let reports = sy.input_reports();
                let spawned = sy.take_spawned(wired);
                let events = sy.take_events();
                let actors: Vec<ActorId> = sy.avatars.keys().copied().collect();
                st.metrics.sync_lost_deltas.inc_by(sy.take_lost_deltas());
                let (limited, invalid, late) = sy.take_input_stats();
                st.metrics.sync_inputs_rate_limited.inc_by(limited);
                st.metrics.sync_invalid_inputs.inc_by(invalid);
                st.metrics.sync_late_inputs.inc_by(late);
                let (resyncs, starved) = sy.take_replication_stats();
                st.metrics.sync_resyncs.inc_by(resyncs);
                st.metrics.sync_aoi_starved.inc_by(starved);
                ticks += 1;
                if ticks.is_multiple_of(checkpoint_every) {
                    checkpoint = Some((project, sy.checkpoint()));
                }
                (out, reports, spawned, events, actors)
            };
            if let Some((_, body)) = checkpoint.take() {
                checkpoint = Some((
                    project,
                    RoomCheckpoint::of(&hosted, Some(body), None).encode(),
                ));
            }
            for (actor, frame) in spawned {
                if let Some(ds) = hosted.sessions.get(&actor).and_then(|s| s.data.as_ref()) {
                    ds.send(net_common::Channel::ReliableOrdered, frame);
                }
            }
            for (actor, delta) in out {
                if let Some(ds) = hosted.sessions.get(&actor).and_then(|s| s.data.as_ref()) {
                    ds.send(net_common::Channel::UnreliableSequenced, delta);
                    st.metrics.events.with_label_values(&["sync_delta"]).inc();
                }
            }
            for (actor, frame) in reports {
                if let Some(ds) = hosted.sessions.get(&actor).and_then(|s| s.data.as_ref()) {
                    ds.send(net_common::Channel::UnreliableSequenced, frame);
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
                        Some(actor) => outgoing.push(Outgoing::Data(
                            actor,
                            net_common::Channel::ReliableOrdered,
                            frame,
                        )),
                        None => {
                            for actor in &actors {
                                outgoing.push(Outgoing::Data(
                                    *actor,
                                    net_common::Channel::ReliableOrdered,
                                    frame.clone(),
                                ));
                            }
                        }
                    }
                }
                dispatch(&hosted, outgoing, &st.metrics);
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
            while let Some(write) = rx.recv().await {
                let redis = st.redis.clone();
                let is_journal = matches!(write, RedisWrite::Journal(..));
                let (what, fut): (&str, std::pin::Pin<Box<dyn Future<Output = _> + Send>>) =
                    match write {
                        RedisWrite::Checkpoint(project, body) => (
                            "checkpoint",
                            Box::pin(async move {
                                redis.put_checkpoint(project, room, &body, 600).await
                            }),
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
    pub fn push(&self, write: RedisWrite) {
        let is_journal = matches!(write, RedisWrite::Journal(..));
        if let Err(mpsc::error::TrySendError::Full(_)) = self.tx.try_send(write) {
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
    let mut ticks: u64 = 0;
    loop {
        iv.tick().await;
        let Some(handle) = st.host.get(room) else {
            return;
        };
        let t0 = Instant::now();
        let mut checkpoint: Option<(ProjectId, Vec<u8>)> = None;
        let project;
        {
            let mut hosted = handle.lock();
            project = hosted.project;
            ticks += 1;
            if ticks.is_multiple_of(checkpoint_every) || writer.take_journal_broken() {
                let snap = hosted.lockstep.as_ref().map(|l| l.snapshot());
                checkpoint = Some((
                    hosted.project,
                    RoomCheckpoint::of(&hosted, None, snap).encode(),
                ));
            }
            let master = hosted.room.master();
            let backlogs: std::collections::HashMap<ActorId, usize> = hosted
                .sessions
                .iter()
                .filter_map(|(actor, session)| {
                    let data = session.data.as_ref().filter(|data| !data.lost())?;
                    Some((*actor, data.backlog()))
                })
                .collect();
            let (bundles, desyncs, actors, ckpt_req, reports, dump_reqs, dumps) = {
                let Some(ls) = hosted.lockstep.as_mut() else {
                    return;
                };
                ls.master = master;
                ls.tick();
                if let Some(bundle) = ls.last_bundle() {
                    journal_buf.push(bundle.to_vec());
                }
                let ckpt_req = ls.checkpoint_request();
                let desyncs: Vec<(u32, ActorId, u8, Option<Bytes>)> = ls
                    .take_desyncs()
                    .into_iter()
                    .map(|(tick, actor)| {
                        let strikes = ls.strike(actor);
                        // Restore (checkpoint + journal tail) only until the actor is untrusted:
                        // a client that keeps lying about its hash must not make the node
                        // re-send the whole state every few ticks.
                        let resync = if strikes <= crate::lockstep::UNTRUSTED_AFTER {
                            ls.on_desync(actor)
                        } else {
                            None
                        };
                        (tick, actor, strikes, resync)
                    })
                    .collect();
                let actors: Vec<ActorId> = ls.actors.keys().copied().collect();
                let bundles: Vec<(ActorId, Option<Bytes>, Option<Vec<Bytes>>)> = actors
                    .iter()
                    .copied()
                    .chain(ls.spectators())
                    .map(|actor| {
                        let room = backlogs
                            .get(&actor)
                            .map_or(0, |held| CATCHUP_BACKLOG_BYTES.saturating_sub(*held));
                        let (datagram, resent) = ls.bundles_for(actor, room);
                        (actor, datagram, resent)
                    })
                    .collect();
                let reports = ls.input_reports();
                let mut dump_reqs: Vec<(ActorId, Bytes)> = Vec::new();
                for (tick, actor, _, _) in &desyncs {
                    if let Some(frame) = ls.request_dump(*tick, *actor) {
                        dump_reqs.push((*actor, frame));
                    }
                    // With a referee sim the node's own state is the reference (already in
                    // `dumps`); otherwise ask the master for its state at that tick.
                    if ls.has_sim() {
                        continue;
                    }
                    if let Some(holder) = master.filter(|m| m != actor) {
                        if let Some(frame) = ls.request_dump(*tick, holder) {
                            dump_reqs.push((holder, frame));
                        }
                    }
                }
                let dumps = ls.take_dumps();
                (
                    bundles, desyncs, actors, ckpt_req, reports, dump_reqs, dumps,
                )
            };
            for (actor, frame) in reports {
                if let Some(ds) = hosted.sessions.get(&actor).and_then(|s| s.data.as_ref()) {
                    ds.send(net_common::Channel::UnreliableSequenced, frame);
                }
            }
            for (actor, frame) in dump_reqs {
                if let Some(ds) = hosted.sessions.get(&actor).and_then(|s| s.data.as_ref()) {
                    ds.send(net_common::Channel::ReliableOrdered, frame);
                }
            }
            for dump in dumps {
                let redis = st.redis.clone();
                tokio::spawn(async move {
                    if let Err(e) = redis
                        .put_desync(
                            project,
                            room,
                            dump.tick,
                            dump.actor.0,
                            dump.hash,
                            &dump.state,
                        )
                        .await
                    {
                        tracing::warn!(room = %room, error = %e, "desync dump not stored");
                    }
                });
            }
            if let Some((req, m)) = ckpt_req.zip(master) {
                if let Some(ds) = hosted.sessions.get(&m).and_then(|s| s.data.as_ref()) {
                    ds.send(net_common::Channel::ReliableOrdered, req);
                }
            }
            // The desynced client reloads from the master's confirmed state; the journal tail
            // follows with the bundles.
            for (_, actor, _, resync) in &desyncs {
                let data = hosted.sessions.get(actor).and_then(|s| s.data.as_ref());
                if let Some((ds, frame)) = data.zip(resync.clone()) {
                    ds.send(net_common::Channel::ReliableOrdered, frame);
                }
            }
            for (actor, datagram, resent) in bundles {
                let Some(ds) = hosted.sessions.get(&actor).and_then(|s| s.data.as_ref()) else {
                    continue;
                };
                let Some(resent) = resent else {
                    // Too slow for the journal to wait: the player comes back by rejoining.
                    ds.close();
                    continue;
                };
                for frame in resent {
                    ds.send(net_common::Channel::ReliableOrdered, frame);
                }
                if let Some(datagram) = datagram {
                    ds.send(net_common::Channel::UnreliableSequenced, datagram);
                    if actors.contains(&actor) {
                        st.metrics
                            .events
                            .with_label_values(&["lockstep_bundle"])
                            .inc();
                    }
                }
            }
            for (tick, actor, strikes, _) in desyncs {
                let untrusted = strikes >= crate::lockstep::UNTRUSTED_AFTER;
                st.metrics.lockstep_desyncs.inc();
                st.webhooks.emit(
                    hosted.project,
                    "desync",
                    serde_json::json!({"room": room.to_string(), "tick": tick, "actor": actor.0, "strikes": strikes, "untrusted": untrusted}),
                );
                for member in &actors {
                    if let Some(session) = hosted.sessions.get(member) {
                        session.control.send(ServerMsg::Desync {
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

fn mismatched_ticket() -> NetError {
    NetError::Unauthorized("ticket does not match session".into())
}

/// A new room's seed. It reaches clients as a JSON number, and those are only portable up to
/// 2^53 (RFC 8259, section 6).
fn room_seed() -> u64 {
    rand::random::<u64>() >> 11
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
    .map(|mut r| {
        let u32p = |k: &str| props.get(k).and_then(|v| v.as_u64()).map(|v| v as u32);
        r.set_replay(u32p("replay_every"), u32p("replay_secs"));
        r
    })
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
                // A module this node has not compiled yet takes seconds.
                let games = st.games.clone();
                let logic = tokio::task::spawn_blocking(move || {
                    let (_, module) = games.load(&row.wasm)?;
                    games.instantiate(&module, seed)
                })
                .await
                .map_err(|e| NetError::Internal(e.to_string()))?
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
    /// Sync rooms: the tags each actor's spawns were made under (actor, tag, entity), so a spawn
    /// repeated after the room moved creates nothing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spawn_tags: Vec<(ActorId, u32, u64)>,
    /// Sync rooms: what becomes of an entity when its owner leaves, where that is not the
    /// default (entity, policy).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub policies: Vec<(u64, u8)>,
    /// Sync rooms: which entity is whose avatar (actor, entity).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub avatars: Vec<(ActorId, u64)>,
    /// Sync rooms: the seed the room was created with, for its logic to start from again. A
    /// lockstep room has its own in `lockstep`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
}

impl RoomCheckpoint {
    pub fn of(
        hosted: &HostedRoom,
        sync: Option<Vec<u8>>,
        lockstep: Option<crate::lockstep::LockstepSnapshot>,
    ) -> Self {
        Self {
            players: hosted
                .room
                .players()
                .filter(|p| !p.spectator)
                .map(|p| (p.actor, p.player_id.clone(), p.name.clone()))
                .collect(),
            master: hosted.room.master(),
            sync,
            lockstep,
            spawn_tags: hosted
                .sync
                .as_ref()
                .map(crate::sync::SyncRoom::spawn_tags)
                .unwrap_or_default(),
            policies: hosted
                .sync
                .as_ref()
                .map(crate::sync::SyncRoom::policies)
                .unwrap_or_default(),
            avatars: hosted
                .sync
                .as_ref()
                .map(crate::sync::SyncRoom::avatar_ids)
                .unwrap_or_default(),
            seed: hosted.sync.as_ref().map(crate::sync::SyncRoom::seed),
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
    // Read before the room is taken: taken without its checkpoint, it would start over.
    let ckpt = st
        .redis
        .get_checkpoint(project, room_id)
        .await?
        .and_then(|body| serde_json::from_slice::<RoomCheckpoint>(&body).ok());
    if !queries::adopt_room(&st.pool, row.id, st.node_id.0, st.region.as_str()).await? {
        return Err(NetError::Conflict("room_pending".into()));
    }
    let proj = queries::get_project(&st.pool, row.project_id).await?;
    let limits = crate::rest::project_limits(st, &proj)?;
    let mode = row.mode.parse().unwrap_or_default();
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
        publish_user_id: prop_bool(&props, "publish_user_id", true),
        suppress_room_events: prop_bool(&props, "suppress_room_events", false),
        suppress_player_info: prop_bool(&props, "suppress_player_info", false),
        broadcast_props_to_all: prop_bool(&props, "broadcast_props_to_all", true),
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
    if let Some(checkpoint) = ckpt {
        room.restore_offline(checkpoint.players, checkpoint.master, Instant::now());
        if mode == RoomMode::Sync {
            let seed = checkpoint.seed.unwrap_or_else(room_seed);
            let mut sy = build_sync(st, project, &props, seed).await?;
            if let Some(body) = checkpoint.sync {
                sy.restore(&body).map_err(NetError::Invalid)?;
                sy.restore_spawn_tags(checkpoint.spawn_tags);
                sy.restore_policies(checkpoint.policies);
                sy.restore_avatars(checkpoint.avatars);
                restored = "sync";
            }
            sync = Some(sy);
        }
        if let (RoomMode::Lockstep, Some(snap)) = (mode, checkpoint.lockstep) {
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
        sync = Some(build_sync(st, project, &props, room_seed()).await?);
    }
    if mode == RoomMode::Lockstep && lockstep.is_none() {
        lockstep =
            Some(
                crate::lockstep::LockstepRoom::new(hz, room_seed())
                    .with_spectator_delay(prop_secs(&props, "spectator_delay_secs", 3) as u32),
            );
    }
    if let Some(ls) = lockstep.as_mut() {
        if let Err(e) = attach_lockstep_sim(st, project, &props, ls).await {
            tracing::warn!(room = %room_id, error = %e, "server sim not attached after adoption");
        }
    }
    let handle = st.host.insert(project, room);
    if let Some(sy) = sync {
        let period = sy.tick_period();
        handle.lock().sync = Some(sy);
        tokio::spawn(run_sync(st.clone(), room_id, period));
    }
    if let Some(ls) = lockstep {
        let period = ls.tick_period();
        handle.lock().lockstep = Some(ls);
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
    publish_room(st, &handle).await;
    Ok(handle)
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

/// The rooms the list offers `player`, out of the project's records in Redis.
fn rooms_for(
    st: &Shared,
    project: ProjectId,
    live: &Result<Vec<LiveRoom>>,
    player: &str,
) -> Vec<RoomSummary> {
    match live {
        Ok(live) => live
            .iter()
            .filter(|room| room.is_listed_for(player))
            .cloned()
            .map(live_to_summary)
            .collect(),
        Err(_) => st.host.listed(project, player),
    }
}

/// Brings Redis and the lobby subscribers of this node up to date after a break: Redis took
/// no writes while it was away and does not repeat what it published meanwhile.
pub async fn resync_lobby(st: &Shared) {
    for (project, room) in st.host.take_unlisted() {
        if st.redis.remove_room(project, room).await.is_err() {
            st.host.unlisted(project, room);
        }
    }
    for handle in st.host.all() {
        publish_room(st, &handle).await;
    }
    for project in st.host.lobby_projects() {
        let live = st.redis.list_rooms(project).await;
        st.host
            .notify_lobby(project, |player| ServerMsg::LobbyRooms {
                rooms: rooms_for(st, project, &live, player),
            });
    }
}

/// Runs with every heartbeat. A room's record in Redis is written when the room changes and
/// expires on its own, so the rooms of a dead node leave the lists; a room that only sits
/// there has its record renewed here, and written again if it has expired.
pub async fn renew_rooms(st: &Shared) {
    let rooms: Vec<(ProjectId, RoomId)> = st
        .host
        .all()
        .iter()
        .map(|handle| {
            let hosted = handle.lock();
            (hosted.project, hosted.room.id())
        })
        .collect();
    let renewed = match st.redis.touch_rooms(&rooms).await {
        Ok(renewed) => renewed,
        Err(e) => {
            tracing::warn!(error = %e, "redis touch_rooms failed");
            return;
        }
    };
    for ((_, room), renewed) in rooms.into_iter().zip(renewed) {
        if renewed {
            continue;
        }
        if let Some(handle) = st.host.get(room) {
            publish_room(st, &handle).await;
        }
    }
}

pub async fn publish_room(st: &Shared, handle: &Arc<Mutex<HostedRoom>>) {
    let (project, live) = {
        let hosted = handle.lock();
        (
            hosted.project,
            LiveRoom {
                room: hosted.room.id(),
                node: st.node_id.to_string(),
                name: hosted.room.cfg.name.clone(),
                mode: hosted.room.cfg.mode.to_string(),
                players: hosted.room.players().count() as u16,
                held_for: hosted
                    .room
                    .held_for(Instant::now())
                    .into_iter()
                    .map(String::from)
                    .collect(),
                max_players: hosted.room.cfg.max_players,
                open: hosted.room.cfg.open,
                visible: hosted.room.cfg.visible,
                props: Value::Object(
                    hosted
                        .room
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
        st.host
            .notify_lobby(project, |player| lobby_change(&live, player));
    }
}

/// Pushes a system notice to every online player of a room hosted here; returns the player count.
pub fn notice_room(st: &Shared, room: RoomId, text: &str, level: &str) -> Result<usize> {
    let hr = st
        .host
        .get(room)
        .ok_or_else(|| NetError::NotFound("room not hosted here".into()))?;
    let hosted = hr.lock();
    let (out, reached) = hosted.room.notice(text, level);
    dispatch(&hosted, out, &st.metrics);
    Ok(reached)
}

/// Same for every room of a project hosted here: (rooms, players).
pub fn notice_project(st: &Shared, project: ProjectId, text: &str, level: &str) -> (usize, usize) {
    let (mut rooms, mut players) = (0usize, 0usize);
    for hr in st.host.all() {
        let hosted = hr.lock();
        if hosted.project != project {
            continue;
        }
        let (out, reached) = hosted.room.notice(text, level);
        if reached > 0 {
            rooms += 1;
            players += reached;
            dispatch(&hosted, out, &st.metrics);
        }
    }
    (rooms, players)
}

/// Server-side kick (REST): removes the actor from a room hosted on this node, tells the client
/// `left{reason}`, updates sync/lockstep membership and republishes the room. Returns the kicked
/// player's id for the audit trail.
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
        let mut hosted = hr.lock();
        player_id = hosted
            .room
            .player_infos()
            .into_iter()
            .find(|p| p.actor == actor)
            .map(|p| p.player_id)
            .ok_or_else(|| NetError::NotFound("actor not in room".into()))?;
        let out = hosted
            .room
            .admin_kick(actor, reason, Instant::now())
            .map_err(room_err)?;
        dispatch(&hosted, out, &st.metrics);
        hosted.sessions.remove(&actor);
        st.webhooks.emit(
            hosted.project,
            "player.left",
            serde_json::json!({"room": hosted.room.id().to_string(), "actor": actor.0, "kicked": true}),
        );
        if let Some(sy) = hosted.sync.as_mut() {
            sy.on_leave(actor);
        }
        if let Some(ls) = hosted.lockstep.as_mut() {
            ls.on_leave(actor);
        }
    }
    publish_room(st, &hr).await;
    Ok(player_id)
}

/// The rooms and the links of their players stop taking events, and the node waits until
/// `until` for the players to acknowledge what it holds for them: an event the node
/// acknowledged has reached the room by the time its players are told to leave.
async fn settle_links(rooms: &[Arc<Mutex<HostedRoom>>], until: tokio::time::Instant) {
    let mut links = Vec::new();
    for handle in rooms {
        let mut hosted = handle.lock();
        hosted.leaving = true;
        for (actor, session) in &hosted.sessions {
            if let Some(ds) = &session.data {
                ds.stop_receiving();
                links.push((hosted.room.id(), *actor, ds.clone()));
            }
        }
    }
    let mut every = tokio::time::interval(crate::host::FLUSH_EVERY);
    while links.iter().any(|(_, _, ds)| !ds.settled()) {
        if tokio::time::timeout_at(until, every.tick()).await.is_err() {
            break;
        }
    }
    for (room, actor, _) in links.iter().filter(|(_, _, ds)| !ds.settled()) {
        tracing::warn!(%room, actor = actor.0, "handoff: events not acknowledged in time");
    }
}

/// Shutdown: every hosted room gets a fresh checkpoint in Redis and becomes `orphaned` in the
/// DB, so the next node a player reaches adopts it (same path as a crashed node, minus the
/// heartbeat wait). Players get `left{reason:"rehost"}` and rejoin by room name.
pub async fn handoff_all(st: &Shared, settle_until: tokio::time::Instant) -> usize {
    let rooms = st.host.all();
    settle_links(&rooms, settle_until).await;
    for handle in &rooms {
        let (project, id, body) = {
            let mut hosted = handle.lock();
            let sync = hosted.sync.as_mut().map(|s| s.checkpoint());
            let ls = hosted.lockstep.as_ref().map(|l| l.snapshot());
            (
                hosted.project,
                hosted.room.id(),
                RoomCheckpoint::of(&hosted, sync, ls).encode(),
            )
        };
        if let Err(e) = st.redis.put_checkpoint(project, id, &body, 600).await {
            tracing::warn!(room = %id, error = %e, "handoff checkpoint failed");
        }
    }
    match queries::orphan_rooms_of_node(&st.pool, st.node_id.0).await {
        Ok(count) => tracing::info!(rooms = count, "rooms handed off for adoption"),
        Err(e) => tracing::error!(error = %e, "handoff: orphan rooms"),
    }
    let count = rooms.len();
    for handle in rooms {
        // Taking the sessions out detaches every player: a later leave or disconnect must not
        // touch (or republish) a room this node no longer hosts.
        let (id, sessions) = {
            let mut hosted = handle.lock();
            (
                hosted.room.id(),
                std::mem::take(&mut hosted.sessions)
                    .into_values()
                    .map(|s| s.control)
                    .collect::<Vec<_>>(),
            )
        };
        st.host.remove(id);
        for session in sessions {
            session.send(ServerMsg::Left {
                reason: "rehost".into(),
                room: Some(id.to_string()),
                id: None,
            });
        }
    }
    count
}

/// Runs with every heartbeat. A node that missed three heartbeats is reaped by its peers and
/// its rooms become adoptable, but the node may only have been stalled and still host them. It
/// takes back the rooms nobody adopted and lets go of the ones another node now runs, so a room
/// never lives on two nodes. Returns (reclaimed, dropped).
///
/// It also closes the rooms of deleted projects: the node asked to delete a project may not
/// have reached this one, or the room was created while the project was being deleted.
pub async fn reconcile_rooms(st: &Shared) -> Result<(u64, usize)> {
    let rooms: Vec<(ProjectId, RoomId)> = st
        .host
        .all()
        .iter()
        .map(|h| {
            let hosted = h.lock();
            (hosted.project, hosted.room.id())
        })
        .collect();
    if rooms.is_empty() {
        return Ok((0, 0));
    }
    let mut projects: Vec<uuid::Uuid> = rooms.iter().map(|(project, _)| project.0).collect();
    projects.sort_unstable();
    projects.dedup();
    let deleted = queries::deleted_projects(&st.pool, &projects).await?;
    let mut hosted = Vec::new();
    for (project, room) in rooms {
        if deleted.contains(&project.0) {
            close_room(st, project, room).await;
        } else {
            hosted.push(room.0);
        }
    }
    if st.is_draining() || hosted.is_empty() {
        return Ok((0, 0));
    }
    let reclaimed = queries::reclaim_rooms(&st.pool, st.node_id.0, &hosted).await?;
    let lost = queries::rooms_lost(&st.pool, st.node_id.0, &hosted).await?;
    let mut dropped = 0;
    for id in lost {
        let Some(handle) = st.host.remove(net_common::RoomId(id)) else {
            continue;
        };
        dropped += 1;
        let sessions: Vec<_> = std::mem::take(&mut handle.lock().sessions)
            .into_values()
            .map(|s| s.control)
            .collect();
        for session in sessions {
            session.send(ServerMsg::Left {
                reason: "rehost".into(),
                room: Some(id.to_string()),
                id: None,
            });
        }
    }
    if reclaimed > 0 || dropped > 0 {
        tracing::warn!(
            reclaimed,
            dropped,
            "rooms reconciled after this node was reaped"
        );
    }
    Ok((reclaimed, dropped))
}

/// Upper bound for one stored replay; longer matches keep their most recent ticks.
pub const MAX_REPLAY_BYTES: usize = 16 << 20;

/// Serializes `replay`, dropping the oldest ticks (and their hashes) until it fits `max_bytes`;
/// `from_tick` moves forward accordingly. None only if even an empty replay does not fit.
pub fn fit_replay(replay: &mut net_rollback::Replay, max_bytes: usize) -> Option<Vec<u8>> {
    loop {
        let body = serde_json::to_vec(replay).ok()?;
        if body.len() <= max_bytes {
            return Some(body);
        }
        if replay.inputs.is_empty() {
            return None;
        }
        let drop = (replay.inputs.len() / 2).max(1);
        replay.inputs.drain(..drop);
        replay.from_tick += drop as u32;
        let from = replay.from_tick;
        replay.hashes.retain(|(t, _)| *t >= from);
        tracing::info!(from_tick = from, "replay trimmed to fit the size cap");
    }
}

/// `room.closed` payload: name/mode/props always; with room prop `export_state_on_close` the
/// final checkpoint (players, master, sync state or lockstep snapshot) as base64 so a backend can
/// persist the match (the "persist room state via webhooks" pattern). Capped at 256 KiB.
fn closed_payload(hosted: &mut HostedRoom, id: RoomId) -> Value {
    const MAX_EXPORT: usize = 256 * 1024;
    let mut payload = serde_json::json!({
        "room": id.to_string(),
        "name": hosted.room.cfg.name,
        "mode": hosted.room.cfg.mode.as_str(),
        "props": hosted.room.cfg.props,
    });
    if prop_bool(&hosted.room.cfg.props, "export_state_on_close", false) {
        let sync = hosted.sync.as_mut().map(|s| s.checkpoint());
        let ls = hosted.lockstep.as_ref().map(|l| l.snapshot());
        let bytes = RoomCheckpoint::of(hosted, sync, ls).encode();
        if bytes.len() <= MAX_EXPORT {
            payload["state"] = Value::String(base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                &bytes,
            ));
        } else {
            payload["state_too_large"] = Value::from(bytes.len());
        }
    }
    payload
}

/// The live node hosting `room`, if there is one.
pub(crate) async fn room_host(
    st: &Shared,
    room: &net_control::LiveRoom,
) -> Result<Option<net_control::NodeInfo>> {
    match uuid::Uuid::parse_str(&room.node) {
        Ok(id) => st.registry.resolve(net_common::NodeId::from_uuid(id)).await,
        Err(_) => Ok(None),
    }
}

pub async fn close_room(st: &Shared, project: ProjectId, id: RoomId) {
    let Some(handle) = st.host.remove(id) else {
        st.webhooks.emit(
            project,
            "room.closed",
            serde_json::json!({"room": id.to_string()}),
        );
        return;
    };
    {
        let (sessions, replay) = {
            let mut hosted = handle.lock();
            st.webhooks
                .emit(project, "room.closed", closed_payload(&mut hosted, id));
            let replay = hosted
                .lockstep
                .as_ref()
                .filter(|l| !l.journal.is_empty())
                .map(|l| l.replay(""));
            // Detach every player (see `handoff_all`): the room is gone for them too.
            (
                std::mem::take(&mut hosted.sessions)
                    .into_values()
                    .map(|s| s.control)
                    .collect::<Vec<Control>>(),
                replay,
            )
        };
        if let Some(mut kept) = replay {
            if let Some(body) = fit_replay(&mut kept, MAX_REPLAY_BYTES) {
                if let Err(e) = st.redis.put_replay(project, id, &body, 86_400).await {
                    tracing::warn!(error = %e, room = %id, "replay store");
                }
            }
        }
        for session in sessions {
            session.send(ServerMsg::Left {
                reason: "room closed".into(),
                room: Some(id.to_string()),
                id: None,
            });
        }
    }
    st.metrics.rooms.set(st.host.len() as i64);
    if let Err(e) = queries::close_room(&st.pool, id.0).await {
        tracing::warn!(error = %e, room = %id, "close_room db");
    }
    if let Err(e) = st.redis.remove_room(project, id).await {
        tracing::warn!(error = %e, room = %id, "close_room redis");
        st.host.unlisted(project, id);
        let gone = lobby_removal(id.to_string());
        st.host.notify_lobby(project, |_| gone.clone());
    }
}

/// Tells a lobby watcher that a room left his list; of the room it carries the id alone.
pub fn lobby_removal(id: String) -> ServerMsg {
    ServerMsg::LobbyUpdate {
        room: RoomSummary {
            id,
            name: String::new(),
            mode: RoomMode::Relay,
            players: 0,
            max_players: 0,
            open: false,
            props: Map::new(),
        },
        removed: true,
    }
}

/// What a lobby watcher hears of a room that changed: the room while the list offers it to
/// him, its removal otherwise.
pub fn lobby_change(live: &LiveRoom, player: &str) -> ServerMsg {
    if live.is_listed_for(player) {
        ServerMsg::LobbyUpdate {
            room: live_to_summary(live.clone()),
            removed: false,
        }
    } else {
        lobby_removal(live.room.to_string())
    }
}

pub fn live_to_summary(live: LiveRoom) -> RoomSummary {
    RoomSummary {
        id: live.room.to_string(),
        name: live.name,
        mode: live.mode.parse().unwrap_or_default(),
        players: live.players,
        max_players: live.max_players,
        open: live.open && live.players < live.max_players,
        props: match live.props {
            Value::Object(props) => props,
            _ => Map::new(),
        },
    }
}

pub(crate) fn room_err(e: RoomError) -> NetError {
    match e {
        RoomError::Full { max } => NetError::RoomFull { max },
        RoomError::Closed => NetError::RoomClosed,
        RoomError::NotMember | RoomError::NoActor => NetError::NotFound(e.to_string()),
        RoomError::NotMaster | RoomError::Spectator | RoomError::SpectatorMaster => {
            NetError::Forbidden(e.to_string())
        }
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
    let uuid = uuid::Uuid::new_v4();
    let bytes = uuid.as_bytes();
    B64.encode(&bytes[..6])
        .replace(['+', '/'], "x")
        .trim_end_matches('=')
        .to_lowercase()
}

/// Periodic housekeeping: expire offline players, close empty rooms.
pub async fn reaper(st: Shared) {
    let mut iv = tokio::time::interval(Duration::from_secs(2));
    loop {
        iv.tick().await;
        for handle in st.host.all() {
            let (close, project, id, changed) = {
                let mut hosted = handle.lock();
                let now = Instant::now();
                let out = hosted.room.expire(now);
                let changed = hosted.room.release_reserved(now) || !out.is_empty();
                // `PlayerLeft` goes to every player who stays, so the first field of `Control`
                // is a recipient. The player whose slot expired is the `actor` of the message.
                let mut expired = Vec::new();
                for outgoing in out.iter() {
                    if let net_rooms::Outgoing::Control(
                        _,
                        ServerMsg::PlayerLeft {
                            actor,
                            offline: false,
                        },
                    ) = outgoing
                    {
                        if !expired.contains(actor) {
                            expired.push(*actor);
                        }
                    }
                }
                for actor in expired.iter() {
                    hosted.sessions.remove(actor);
                    st.webhooks.emit(
                        hosted.project,
                        "player.left",
                        serde_json::json!({"room": hosted.room.id().to_string(), "actor": actor.0}),
                    );
                    if let Some(sy) = hosted.sync.as_mut() {
                        sy.on_leave(*actor);
                    }
                    if let Some(ls) = hosted.lockstep.as_mut() {
                        ls.on_leave(*actor);
                    }
                }
                dispatch(&hosted, out, &st.metrics);
                (
                    hosted.room.should_close(Instant::now()),
                    hosted.project,
                    hosted.room.id(),
                    changed,
                )
            };
            if close {
                close_room(&st, project, id).await;
            } else if changed {
                publish_room(&st, &handle).await;
            }
        }
        st.metrics.players.set(st.host.players() as i64);
    }
}

#[cfg(test)]
mod checkpoint_tests {
    use super::RoomCheckpoint;

    #[test]
    fn checkpoint_of_a_newer_node_is_read_past_its_new_fields() {
        let body = br#"{"players":[],"master":null,"added_later":1}"#;
        assert!(serde_json::from_slice::<RoomCheckpoint>(body).is_ok());
    }
}

#[cfg(test)]
mod replay_fit_tests {
    use super::fit_replay;

    #[test]
    fn trims_oldest_ticks_until_it_fits() {
        let mut replay = net_rollback::Replay {
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
        let full = serde_json::to_vec(&replay).unwrap().len();
        let body = fit_replay(&mut replay, full / 4).unwrap();
        assert!(body.len() <= full / 4);
        assert!(replay.from_tick >= 750 && replay.inputs.len() + replay.from_tick as usize == 1000);
        assert!(replay.hashes.iter().all(|(t, _)| *t >= replay.from_tick));
        let mut tiny = replay.clone();
        assert!(fit_replay(&mut tiny, 10).is_none());
    }
}
