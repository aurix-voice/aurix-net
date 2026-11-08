//! One relay-mode room. Returns the messages to deliver instead of sending them.

use crate::event::{CacheOp, EventFrame, EventTarget};
use crate::protocol::{PlayerInfo, RoomSummary, ServerMsg};
use net_common::{ActorId, Channel, ProjectId, RoomId, RoomMode};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct RoomConfig {
    pub id: RoomId,
    pub project: ProjectId,
    pub name: String,
    pub mode: RoomMode,
    pub max_players: u16,
    pub visible: bool,
    pub open: bool,
    pub props: Map<String, Value>,
    pub lobby_props: Vec<String>,
    /// Player ids with a held slot (a party arriving together); others cannot take those slots
    /// until `reserved_until` passes.
    pub reserved: Vec<String>,
    pub reserved_until: Option<Instant>,
    /// Drop a player's cached events when they leave for good (room prop `cleanup_cache_on_leave`,
    /// default true); false keeps them for late joiners, the "keep the cache on leave" behaviour.
    pub cleanup_cache_on_leave: bool,
    pub max_events_per_sec: u32,
    /// Events one player may send per second (burst = 2 s worth).
    pub max_events_per_sec_per_player: u32,
    pub max_event_bytes: usize,
    pub max_props_bytes: usize,
    pub max_player_props_bytes: usize,
    pub max_cached_events: usize,
    /// How long an offline player keeps their slot before it is freed.
    pub offline_ttl: Duration,
    /// Close the room this long after the last player leaves.
    pub empty_ttl: Duration,
}

#[derive(Debug, Clone)]
pub struct Player {
    pub actor: ActorId,
    pub player_id: String,
    pub name: String,
    pub props: Map<String, Value>,
    pub online: bool,
    pub offline_since: Option<Instant>,
    pub groups: HashSet<u16>,
    pub spectator: bool,
    tokens: f64,
    last_refill: Instant,
    joined_at: Instant,
    events_in: u64,
    bytes_in: u64,
    last_event: Option<Instant>,
    /// One-second window for the live events/s figure shown in the console.
    win_start: Instant,
    win_count: u32,
    eps: f32,
}

/// Live per-player counters for the operator console (not persisted, reset on rehost).
#[derive(Debug, Clone, serde::Serialize)]
pub struct PlayerStats {
    pub actor: ActorId,
    pub events: u64,
    pub bytes_in: u64,
    pub eps: f32,
    pub idle_secs: Option<f32>,
    pub since_secs: f32,
}

impl Player {
    fn count_event(&mut self, bytes: usize, now: Instant) {
        self.events_in += 1;
        self.bytes_in += bytes as u64;
        self.last_event = Some(now);
        let w = now.duration_since(self.win_start).as_secs_f32();
        if w >= 1.0 {
            self.eps = self.win_count as f32 / w;
            self.win_start = now;
            self.win_count = 0;
        }
        self.win_count += 1;
    }

    fn stats(&self, now: Instant) -> PlayerStats {
        let w = now.duration_since(self.win_start).as_secs_f32();
        // A window older than 2 s without a refresh means the player went quiet.
        let eps = if w > 2.0 { 0.0 } else { self.eps };
        PlayerStats {
            actor: self.actor,
            events: self.events_in,
            bytes_in: self.bytes_in,
            eps,
            idle_secs: self.last_event.map(|t| now.duration_since(t).as_secs_f32()),
            since_secs: now.duration_since(self.joined_at).as_secs_f32(),
        }
    }

    fn info(&self) -> PlayerInfo {
        PlayerInfo {
            actor: self.actor,
            player_id: self.player_id.clone(),
            name: self.name.clone(),
            props: self.props.clone(),
            online: self.online,
            spectator: self.spectator,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum Outgoing {
    /// Control-channel message to one actor.
    Control(ActorId, ServerMsg),
    /// Data-path event frame to one actor on a channel.
    Data(ActorId, Channel, EventFrame),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RoomError {
    #[error("room is full")]
    Full,
    #[error("room is closed")]
    Closed,
    #[error("not in room")]
    NotMember,
    #[error("only the master client may do this")]
    NotMaster,
    #[error("event too large")]
    EventTooLarge,
    #[error("too many events per second")]
    EventRate,
    #[error("you send too many events per second")]
    PlayerEventRate,
    #[error("spectators cannot send")]
    Spectator,
    #[error("properties too large")]
    PropsTooLarge,
    #[error("expected values did not match")]
    CasFailed,
    #[error("no such actor")]
    NoActor,
}

#[derive(Debug, Clone)]
struct CachedEvent {
    code: u8,
    from: ActorId,
    payload: bytes::Bytes,
    slice: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheSliceOp {
    Increase,
    Set(u16),
    Purge(u16),
    PurgeUpTo(u16),
}

#[derive(Debug)]
pub struct Room {
    pub cfg: RoomConfig,
    players: BTreeMap<ActorId, Player>,
    master: Option<ActorId>,
    next_actor: u16,
    cache: VecDeque<CachedEvent>,
    /// Slice newly cached events are stamped with (see `ClientMsg::CacheSlice`).
    cache_slice: u16,
    /// Token bucket for events.
    tokens: f64,
    last_refill: Instant,
    pub created: Instant,
    empty_since: Option<Instant>,
    /// Map player_id -> actor for rejoin.
    by_player: HashMap<String, ActorId>,
}

impl Room {
    pub fn new(cfg: RoomConfig, now: Instant) -> Self {
        let tokens = cfg.max_events_per_sec as f64;
        Self {
            cfg,
            players: BTreeMap::new(),
            master: None,
            next_actor: 1,
            cache: VecDeque::new(),
            cache_slice: 0,
            tokens,
            last_refill: now,
            created: now,
            empty_since: Some(now),
            by_player: HashMap::new(),
        }
    }

    pub fn id(&self) -> RoomId {
        self.cfg.id
    }
    pub fn master(&self) -> Option<ActorId> {
        self.master
    }
    pub fn players(&self) -> impl Iterator<Item = &Player> {
        self.players.values()
    }
    pub fn player(&self, a: ActorId) -> Option<&Player> {
        self.players.get(&a)
    }
    /// Actor of `player_id` if that player is currently online in this room.
    pub fn online_actor(&self, player_id: &str) -> Option<ActorId> {
        self.by_player
            .get(player_id)
            .copied()
            .filter(|a| self.players.get(a).is_some_and(|p| p.online))
    }

    /// Players counted against `max_players` (spectators excluded).
    pub fn player_count(&self) -> usize {
        self.players.values().filter(|p| !p.spectator).count()
    }

    pub fn online_count(&self) -> u16 {
        self.players.values().filter(|p| p.online).count() as u16
    }
    pub fn is_empty(&self) -> bool {
        self.players.is_empty()
    }

    /// System notice to every online player; returns the frames and how many players get it.
    pub fn notice(&self, text: &str, level: &str) -> (Vec<Outgoing>, usize) {
        let out: Vec<Outgoing> = self
            .players
            .iter()
            .filter(|(_, p)| p.online)
            .map(|(a, _)| {
                Outgoing::Control(
                    *a,
                    ServerMsg::Notice {
                        text: text.to_string(),
                        level: level.to_string(),
                    },
                )
            })
            .collect();
        let n = out.len();
        (out, n)
    }

    pub fn summary(&self) -> RoomSummary {
        let props = self
            .cfg
            .props
            .iter()
            .filter(|(k, _)| k.as_str() == "lobby" || self.cfg.lobby_props.iter().any(|l| l == *k))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        RoomSummary {
            id: self.cfg.id.to_string(),
            name: self.cfg.name.clone(),
            mode: self.cfg.mode,
            players: self.players.len() as u16,
            max_players: self.cfg.max_players,
            open: self.cfg.open && self.player_count() < self.cfg.max_players as usize,
            props,
        }
    }

    /// Reserved slots whose player has not arrived yet (0 once the hold expired).
    pub fn pending_reserved(&self, now: Instant) -> usize {
        if !self.cfg.reserved_until.is_some_and(|u| now < u) {
            return 0;
        }
        self.cfg
            .reserved
            .iter()
            .filter(|r| !self.by_player.contains_key(r.as_str()))
            .count()
    }

    pub fn is_listed(&self) -> bool {
        self.cfg.visible && self.cfg.open && self.player_count() < self.cfg.max_players as usize
    }

    /// Admits a player. Returns the actor id, the player list to send them, cached events,
    /// and notifications for everyone else.
    pub fn join(
        &mut self,
        player_id: &str,
        name: &str,
        props: Map<String, Value>,
        rejoin: Option<ActorId>,
        spectator: bool,
        now: Instant,
    ) -> Result<(ActorId, Vec<Outgoing>), RoomError> {
        if props_len(&props) > self.cfg.max_player_props_bytes {
            return Err(RoomError::PropsTooLarge);
        }
        // Reconnect into the existing slot. If the player is still online from another
        // session, the new session takes the slot over (the caller tells the old one).
        if let Some(&a) = self.by_player.get(player_id) {
            if let Some(p) = self.players.get_mut(&a) {
                if rejoin.is_some_and(|r| r != a) {
                    return Err(RoomError::NoActor);
                }
                let was_online = p.online;
                let spec = p.spectator;
                p.online = true;
                p.offline_since = None;
                p.name = name.to_string();
                if !props.is_empty() {
                    p.props = props;
                }
                let info = p.info();
                self.empty_since = None;
                if self.master.is_none() && !spec {
                    self.master = Some(a);
                }
                let out = if was_online {
                    Vec::new()
                } else {
                    self.broadcast_except(a, ServerMsg::PlayerJoined { player: info })
                };
                return Ok((a, out));
            }
        }
        if !self.cfg.open {
            return Err(RoomError::Closed);
        }
        if !spectator {
            let held = if self.cfg.reserved.iter().any(|r| r == player_id) {
                0
            } else {
                self.pending_reserved(now)
            };
            if self.player_count() + held >= self.cfg.max_players as usize {
                return Err(RoomError::Full);
            }
        }
        let actor = ActorId(self.next_actor);
        self.next_actor = self.next_actor.wrapping_add(1).max(1);
        let p = Player {
            actor,
            player_id: player_id.to_string(),
            name: name.to_string(),
            props,
            online: true,
            offline_since: None,
            groups: HashSet::new(),
            spectator,
            tokens: 2.0 * self.cfg.max_events_per_sec_per_player as f64,
            last_refill: now,
            joined_at: now,
            events_in: 0,
            bytes_in: 0,
            last_event: None,
            win_start: now,
            win_count: 0,
            eps: 0.0,
        };
        let info = p.info();
        self.players.insert(actor, p);
        self.by_player.insert(player_id.to_string(), actor);
        self.empty_since = None;
        let mut out = Vec::new();
        if self.master.is_none() && !spectator {
            self.master = Some(actor);
        }
        out.extend(self.broadcast_except(actor, ServerMsg::PlayerJoined { player: info }));
        Ok((actor, out))
    }

    /// Re-host: recreates the players of a checkpointed room as offline slots so they get
    /// their old actor ids back when they reconnect (`offline_ttl` applies from `now`).
    pub fn restore_offline(
        &mut self,
        players: Vec<(ActorId, String, String)>,
        master: Option<ActorId>,
        now: Instant,
    ) {
        for (actor, player_id, name) in players {
            if self.players.contains_key(&actor) || self.by_player.contains_key(&player_id) {
                continue;
            }
            self.players.insert(
                actor,
                Player {
                    actor,
                    player_id: player_id.clone(),
                    name,
                    props: Map::new(),
                    online: false,
                    offline_since: Some(now),
                    groups: HashSet::new(),
                    spectator: false,
                    tokens: 2.0 * self.cfg.max_events_per_sec_per_player as f64,
                    last_refill: now,
                    joined_at: now,
                    events_in: 0,
                    bytes_in: 0,
                    last_event: None,
                    win_start: now,
                    win_count: 0,
                    eps: 0.0,
                },
            );
            self.by_player.insert(player_id, actor);
            self.next_actor = self.next_actor.max(actor.0.wrapping_add(1).max(1));
        }
        if let Some(m) = master.filter(|m| self.players.contains_key(m)) {
            self.master = Some(m);
        }
        self.empty_since = Some(now);
    }

    pub fn player_infos(&self) -> Vec<PlayerInfo> {
        self.players.values().map(|p| p.info()).collect()
    }

    /// Counts a simulation input (sync/lockstep) that bypasses `raise_event`, so the live
    /// per-player figures cover every mode.
    pub fn count_input(&mut self, actor: ActorId, bytes: usize, now: Instant) {
        if let Some(p) = self.players.get_mut(&actor) {
            p.count_event(bytes, now);
        }
    }

    pub fn player_stats(&self, now: Instant) -> Vec<PlayerStats> {
        self.players.values().map(|p| p.stats(now)).collect()
    }

    /// Current cache slice index.
    pub fn cache_slice(&self) -> u16 {
        self.cache_slice
    }

    /// `increase` / `set` move the slice new cached events go to; `purge` drops one slice,
    /// `purge_up_to` every slice up to and including `index`. Returns cached events left.
    pub fn cache_slice_op(&mut self, op: CacheSliceOp) -> usize {
        match op {
            CacheSliceOp::Increase => self.cache_slice = self.cache_slice.wrapping_add(1),
            CacheSliceOp::Set(i) => self.cache_slice = i,
            CacheSliceOp::Purge(i) => self.cache.retain(|c| c.slice != i),
            CacheSliceOp::PurgeUpTo(i) => self.cache.retain(|c| c.slice > i),
        }
        self.cache.len()
    }

    pub fn cached_for_join(&self) -> Vec<(u8, ActorId, String)> {
        use base64::Engine;
        self.cache
            .iter()
            .map(|c| {
                (
                    c.code,
                    c.from,
                    base64::engine::general_purpose::STANDARD.encode(&c.payload),
                )
            })
            .collect()
    }

    /// `offline = true` keeps the slot for `offline_ttl` so the player can rejoin.
    pub fn leave(&mut self, actor: ActorId, offline: bool, now: Instant) -> Vec<Outgoing> {
        let mut out = Vec::new();
        if offline {
            let Some(p) = self.players.get_mut(&actor) else {
                return out;
            };
            p.online = false;
            p.offline_since = Some(now);
            out.extend(self.broadcast_except(
                actor,
                ServerMsg::PlayerLeft {
                    actor,
                    offline: true,
                },
            ));
        } else {
            let Some(p) = self.players.remove(&actor) else {
                return out;
            };
            self.by_player.remove(&p.player_id);
            if self.cfg.cleanup_cache_on_leave {
                self.cache.retain(|c| c.from != actor);
            }
            out.extend(self.broadcast_except(
                actor,
                ServerMsg::PlayerLeft {
                    actor,
                    offline: false,
                },
            ));
        }
        if self.master == Some(actor) {
            // Oldest online (non-spectator) player becomes master.
            self.master = self
                .players
                .values()
                .filter(|p| p.online && !p.spectator)
                .map(|p| p.actor)
                .min()
                .or_else(|| {
                    self.players
                        .values()
                        .filter(|p| !p.spectator)
                        .map(|p| p.actor)
                        .min()
                });
            if let Some(m) = self.master {
                out.extend(self.broadcast_all(ServerMsg::MasterChanged { actor: m }));
            }
        }
        if self.players.is_empty() {
            self.empty_since = Some(now);
        }
        out
    }

    /// Frees slots of players offline longer than `offline_ttl`. Returns notifications.
    pub fn expire(&mut self, now: Instant) -> Vec<Outgoing> {
        let gone: Vec<ActorId> = self
            .players
            .values()
            .filter(|p| {
                !p.online
                    && p.offline_since
                        .is_some_and(|t| now.duration_since(t) >= self.cfg.offline_ttl)
            })
            .map(|p| p.actor)
            .collect();
        let mut out = Vec::new();
        for a in gone {
            out.extend(self.leave(a, false, now));
        }
        out
    }

    pub fn should_close(&self, now: Instant) -> bool {
        self.players.is_empty()
            && self
                .empty_since
                .is_some_and(|t| now.duration_since(t) >= self.cfg.empty_ttl)
    }

    pub fn set_room_props(
        &mut self,
        by: ActorId,
        props: Map<String, Value>,
        expected: Option<&Map<String, Value>>,
        visible: Option<bool>,
        open: Option<bool>,
    ) -> Result<Vec<Outgoing>, RoomError> {
        if !self.players.contains_key(&by) {
            return Err(RoomError::NotMember);
        }
        if let Some(exp) = expected {
            for (k, v) in exp {
                if self.cfg.props.get(k).unwrap_or(&Value::Null) != v {
                    return Err(RoomError::CasFailed);
                }
            }
        }
        let mut merged = self.cfg.props.clone();
        merge_props(&mut merged, props);
        if props_len(&merged) > self.cfg.max_props_bytes {
            return Err(RoomError::PropsTooLarge);
        }
        self.cfg.props = merged;
        if let Some(v) = visible {
            self.cfg.visible = v;
        }
        if let Some(o) = open {
            self.cfg.open = o;
        }
        Ok(self.broadcast_all(ServerMsg::RoomProps {
            props: self.cfg.props.clone(),
            by: Some(by),
            visible: self.cfg.visible,
            open: self.cfg.open,
        }))
    }

    pub fn set_player_props(
        &mut self,
        actor: ActorId,
        props: Map<String, Value>,
        expected: Option<&Map<String, Value>>,
    ) -> Result<Vec<Outgoing>, RoomError> {
        let p = self.players.get_mut(&actor).ok_or(RoomError::NotMember)?;
        if let Some(exp) = expected {
            for (k, v) in exp {
                if p.props.get(k).unwrap_or(&Value::Null) != v {
                    return Err(RoomError::CasFailed);
                }
            }
        }
        let mut merged = p.props.clone();
        merge_props(&mut merged, props);
        if props_len(&merged) > self.cfg.max_player_props_bytes {
            return Err(RoomError::PropsTooLarge);
        }
        p.props = merged;
        let props = p.props.clone();
        Ok(self.broadcast_all(ServerMsg::PlayerProps { actor, props }))
    }

    pub fn set_groups(
        &mut self,
        actor: ActorId,
        join: &[u16],
        leave: &[u16],
    ) -> Result<(), RoomError> {
        let p = self.players.get_mut(&actor).ok_or(RoomError::NotMember)?;
        for g in leave {
            p.groups.remove(g);
        }
        for g in join {
            p.groups.insert(*g);
        }
        Ok(())
    }

    pub fn set_master(&mut self, by: ActorId, target: ActorId) -> Result<Vec<Outgoing>, RoomError> {
        if self.master != Some(by) {
            return Err(RoomError::NotMaster);
        }
        if !self.players.contains_key(&target) {
            return Err(RoomError::NoActor);
        }
        self.master = Some(target);
        Ok(self.broadcast_all(ServerMsg::MasterChanged { actor: target }))
    }

    pub fn kick(
        &mut self,
        by: ActorId,
        target: ActorId,
        reason: &str,
        now: Instant,
    ) -> Result<Vec<Outgoing>, RoomError> {
        if self.master != Some(by) {
            return Err(RoomError::NotMaster);
        }
        self.admin_kick(target, reason, now)
    }

    /// Kick by the operator/project backend: no master check, same `left{reason}` to the target.
    pub fn admin_kick(
        &mut self,
        target: ActorId,
        reason: &str,
        now: Instant,
    ) -> Result<Vec<Outgoing>, RoomError> {
        if !self.players.contains_key(&target) {
            return Err(RoomError::NoActor);
        }
        let mut out = vec![Outgoing::Control(
            target,
            ServerMsg::Left {
                reason: if reason.is_empty() {
                    "kicked".into()
                } else {
                    reason.into()
                },
            },
        )];
        out.extend(self.leave(target, false, now));
        Ok(out)
    }

    fn refill(&mut self, now: Instant) {
        let dt = now.duration_since(self.last_refill).as_secs_f64();
        self.last_refill = now;
        let max = self.cfg.max_events_per_sec as f64;
        self.tokens = (self.tokens + dt * max).min(max);
    }

    /// Routes an event from `from`. `frame.from` is overwritten with the real sender.
    pub fn raise_event(
        &mut self,
        from: ActorId,
        mut frame: EventFrame,
        channel: Channel,
        now: Instant,
    ) -> Result<Vec<Outgoing>, RoomError> {
        if !self.players.get(&from).is_some_and(|p| p.online) {
            return Err(RoomError::NotMember);
        }
        if self.players[&from].spectator {
            return Err(RoomError::Spectator);
        }
        if frame.payload.len() > self.cfg.max_event_bytes {
            return Err(RoomError::EventTooLarge);
        }
        self.refill(now);
        if self.tokens < 1.0 {
            return Err(RoomError::EventRate);
        }
        {
            let rate = self.cfg.max_events_per_sec_per_player as f64;
            let p = self.players.get_mut(&from).unwrap();
            let dt = now.duration_since(p.last_refill).as_secs_f64();
            p.last_refill = now;
            p.tokens = (p.tokens + dt * rate).min(2.0 * rate);
            if p.tokens < 1.0 {
                return Err(RoomError::PlayerEventRate);
            }
            p.tokens -= 1.0;
            p.count_event(frame.payload.len(), now);
        }
        self.tokens -= 1.0;
        frame.from = from;

        match frame.cache {
            CacheOp::Add => {
                if self.cache.len() >= self.cfg.max_cached_events {
                    self.cache.pop_front();
                }
                self.cache.push_back(CachedEvent {
                    code: frame.code,
                    from,
                    payload: frame.payload.clone(),
                    slice: self.cache_slice,
                });
            }
            CacheOp::Remove => {
                self.cache
                    .retain(|c| !(c.from == from && c.code == frame.code));
            }
            CacheOp::None => {}
        }

        let targets: Vec<ActorId> = match &frame.target {
            EventTarget::All => self
                .players
                .values()
                .filter(|p| p.online)
                .map(|p| p.actor)
                .collect(),
            EventTarget::Others => self
                .players
                .values()
                .filter(|p| p.online && p.actor != from)
                .map(|p| p.actor)
                .collect(),
            EventTarget::Master => self.master.into_iter().collect(),
            EventTarget::Actors(list) => list
                .iter()
                .copied()
                .filter(|a| self.players.get(a).is_some_and(|p| p.online))
                .collect(),
        };
        let group = frame.group;
        let out = targets
            .into_iter()
            .filter(|a| match group {
                None => true,
                Some(g) => self.players.get(a).is_some_and(|p| p.groups.contains(&g)),
            })
            .map(|a| Outgoing::Data(a, channel, frame.clone()))
            .collect();
        Ok(out)
    }

    fn broadcast_all(&self, msg: ServerMsg) -> Vec<Outgoing> {
        self.players
            .values()
            .filter(|p| p.online)
            .map(|p| Outgoing::Control(p.actor, msg.clone()))
            .collect()
    }

    fn broadcast_except(&self, except: ActorId, msg: ServerMsg) -> Vec<Outgoing> {
        self.players
            .values()
            .filter(|p| p.online && p.actor != except)
            .map(|p| Outgoing::Control(p.actor, msg.clone()))
            .collect()
    }
}

/// `null` deletes a key, everything else overwrites.
pub fn merge_props(into: &mut Map<String, Value>, from: Map<String, Value>) {
    for (k, v) in from {
        if v.is_null() {
            into.remove(&k);
        } else {
            into.insert(k, v);
        }
    }
}

pub fn props_len(p: &Map<String, Value>) -> usize {
    serde_json::to_vec(p).map(|v| v.len()).unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    fn cfg() -> RoomConfig {
        RoomConfig {
            id: RoomId::new(),
            project: ProjectId::new(),
            name: "r".into(),
            mode: RoomMode::Relay,
            max_players: 2,
            visible: true,
            open: true,
            props: Map::new(),
            lobby_props: vec!["map".into()],
            reserved: Vec::new(),
            reserved_until: None,
            cleanup_cache_on_leave: true,
            max_events_per_sec: 4,
            max_events_per_sec_per_player: 3,
            max_event_bytes: 100,
            max_props_bytes: 200,
            max_player_props_bytes: 200,
            max_cached_events: 2,
            offline_ttl: Duration::from_secs(10),
            empty_ttl: Duration::from_secs(5),
        }
    }

    fn ev(code: u8, target: EventTarget, cache: CacheOp) -> EventFrame {
        EventFrame {
            code,
            target,
            cache,
            group: None,
            from: ActorId(0),
            payload: Bytes::from_static(b"x"),
        }
    }

    #[test]
    fn join_full_master_rotation() {
        let t = Instant::now();
        let mut r = Room::new(cfg(), t);
        let (a, out) = r.join("p1", "A", Map::new(), None, false, t).unwrap();
        assert!(out.is_empty());
        assert_eq!(r.master(), Some(a));
        let (b, out) = r.join("p2", "B", Map::new(), None, false, t).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(
            r.join("p3", "C", Map::new(), None, false, t).unwrap_err(),
            RoomError::Full
        );
        // same player again: takes over the slot silently
        let (b2, out) = r.join("p2", "B", Map::new(), None, false, t).unwrap();
        assert_eq!((b2, out.len()), (b, 0));
        let out = r.leave(a, false, t);
        assert!(out.contains(&Outgoing::Control(b, ServerMsg::MasterChanged { actor: b })));
        assert_eq!(r.master(), Some(b));
    }

    #[test]
    fn offline_rejoin_keeps_actor_then_expires() {
        let t = Instant::now();
        let mut r = Room::new(cfg(), t);
        let (a, _) = r.join("p1", "A", Map::new(), None, false, t).unwrap();
        r.leave(a, true, t);
        assert_eq!(r.online_count(), 0);
        let (a2, _) = r
            .join(
                "p1",
                "A",
                Map::new(),
                None,
                false,
                t + Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(a, a2);
        r.leave(a, true, t);
        assert!(r.expire(t + Duration::from_secs(5)).is_empty());
        r.expire(t + Duration::from_secs(11));
        assert!(r.is_empty());
        assert!(r.should_close(t + Duration::from_secs(17)));
    }

    #[test]
    fn events_targets_groups_cache_rate() {
        let t = Instant::now();
        let mut c = cfg();
        c.max_players = 3;
        let mut r = Room::new(c, t);
        let (a, _) = r.join("p1", "A", Map::new(), None, false, t).unwrap();
        let (b, _) = r.join("p2", "B", Map::new(), None, false, t).unwrap();
        let (cc, _) = r.join("p3", "C", Map::new(), None, false, t).unwrap();
        let out = r
            .raise_event(
                a,
                ev(1, EventTarget::Others, CacheOp::Add),
                Channel::ReliableOrdered,
                t,
            )
            .unwrap();
        assert_eq!(out.len(), 2);
        assert!(
            matches!(&out[0], Outgoing::Data(x, Channel::ReliableOrdered, f) if *x == b && f.from == a)
        );
        let out = r
            .raise_event(
                b,
                ev(2, EventTarget::Master, CacheOp::None),
                Channel::UnreliableSequenced,
                t,
            )
            .unwrap();
        assert_eq!(out.len(), 1);
        assert!(matches!(&out[0], Outgoing::Data(x, _, _) if *x == a));
        r.set_groups(cc, &[7], &[]).unwrap();
        let mut g = ev(3, EventTarget::All, CacheOp::None);
        g.group = Some(7);
        let out = r.raise_event(a, g, Channel::ReliableOrdered, t).unwrap();
        assert_eq!(out.len(), 1);
        assert!(matches!(&out[0], Outgoing::Data(x, _, _) if *x == cc));
        // cache: Add kept, Remove drops, bounded at 2
        r.raise_event(
            a,
            ev(4, EventTarget::All, CacheOp::Add),
            Channel::ReliableOrdered,
            t,
        )
        .unwrap();
        assert_eq!(r.cached_for_join().len(), 2);
        assert_eq!(
            r.raise_event(
                a,
                ev(5, EventTarget::All, CacheOp::None),
                Channel::ReliableOrdered,
                t
            )
            .unwrap_err(),
            RoomError::EventRate
        );
        let t2 = t + Duration::from_secs(1);
        r.raise_event(
            a,
            ev(1, EventTarget::All, CacheOp::Remove),
            Channel::ReliableOrdered,
            t2,
        )
        .unwrap();
        assert_eq!(r.cached_for_join().len(), 1);
        assert_eq!(r.cached_for_join()[0].0, 4);
        r.leave(a, false, t2);
        assert!(r.cached_for_join().is_empty());
    }

    #[test]
    fn props_cas_and_size() {
        let t = Instant::now();
        let mut r = Room::new(cfg(), t);
        let (a, _) = r.join("p1", "A", Map::new(), None, false, t).unwrap();
        let mut p = Map::new();
        p.insert("map".into(), Value::from("dust"));
        r.set_room_props(a, p.clone(), None, None, None).unwrap();
        assert_eq!(r.summary().props.get("map"), Some(&Value::from("dust")));
        let mut exp = Map::new();
        exp.insert("map".into(), Value::from("other"));
        assert_eq!(
            r.set_room_props(a, p.clone(), Some(&exp), None, None)
                .unwrap_err(),
            RoomError::CasFailed
        );
        let mut big = Map::new();
        big.insert("x".into(), Value::from("y".repeat(300)));
        assert_eq!(
            r.set_player_props(a, big, None).unwrap_err(),
            RoomError::PropsTooLarge
        );
        let mut del = Map::new();
        del.insert("map".into(), Value::Null);
        r.set_room_props(a, del, None, Some(false), None).unwrap();
        assert!(r.cfg.props.is_empty());
        assert!(!r.is_listed());
    }

    #[test]
    fn spectators_do_not_take_slots_or_master_and_cannot_send() {
        let t = Instant::now();
        let mut r = Room::new(cfg(), t);
        let (s, _) = r.join("watch", "W", Map::new(), None, true, t).unwrap();
        assert_eq!(r.master(), None);
        let (a, _) = r.join("p1", "A", Map::new(), None, false, t).unwrap();
        let (_b, _) = r.join("p2", "B", Map::new(), None, false, t).unwrap();
        assert_eq!(r.master(), Some(a));
        assert_eq!(
            r.join("p3", "C", Map::new(), None, false, t).unwrap_err(),
            RoomError::Full
        );
        assert!(r.join("watch2", "W2", Map::new(), None, true, t).is_ok());
        assert_eq!(r.player_count(), 2);
        assert_eq!(
            r.raise_event(
                s,
                ev(1, EventTarget::All, CacheOp::None),
                Channel::ReliableOrdered,
                t
            )
            .unwrap_err(),
            RoomError::Spectator
        );
        r.leave(a, false, t);
        assert_ne!(r.master(), Some(s));
    }

    #[test]
    fn reserved_slots_hold_seats_for_the_party() {
        let mut c = cfg();
        c.max_players = 2;
        c.reserved = vec!["p2".into()];
        let t0 = Instant::now();
        c.reserved_until = Some(t0 + Duration::from_secs(60));
        let mut r = Room::new(c, t0);
        r.join("p1", "p1", Map::new(), None, false, t0).unwrap();
        // the only free seat is held for p2
        assert!(matches!(
            r.join("stranger", "s", Map::new(), None, false, t0),
            Err(RoomError::Full)
        ));
        assert_eq!(r.pending_reserved(t0), 1);
        r.join("p2", "p2", Map::new(), None, false, t0).unwrap();
        assert_eq!(r.pending_reserved(t0), 0);
        // hold lapsed: a stranger may take an unclaimed reserved seat
        let mut c = cfg();
        c.max_players = 2;
        c.reserved = vec!["late".into()];
        c.reserved_until = Some(t0 + Duration::from_secs(60));
        let mut r = Room::new(c, t0);
        r.join("p1", "p1", Map::new(), None, false, t0).unwrap();
        let later = t0 + Duration::from_secs(61);
        r.join("stranger", "s", Map::new(), None, false, later)
            .unwrap();
    }

    #[test]
    fn cache_slices_group_and_purge_cached_events() {
        let mut c = cfg();
        c.max_cached_events = 8;
        let t = Instant::now();
        let mut r = Room::new(c, t);
        let (a, _) = r.join("a", "a", Map::new(), None, false, t).unwrap();
        let add = |r: &mut Room, code: u8| {
            r.raise_event(
                a,
                ev(code, EventTarget::All, CacheOp::Add),
                Channel::ReliableOrdered,
                t,
            )
            .unwrap();
        };
        add(&mut r, 1);
        assert_eq!(r.cache_slice_op(CacheSliceOp::Increase), 1);
        assert_eq!(r.cache_slice(), 1);
        add(&mut r, 2);
        add(&mut r, 3);
        r.cache_slice_op(CacheSliceOp::Set(5));
        add(&mut r, 4);
        let codes = |r: &Room| r.cached_for_join().iter().map(|c| c.0).collect::<Vec<_>>();
        assert_eq!(codes(&r), vec![1, 2, 3, 4]);
        assert_eq!(r.cache_slice_op(CacheSliceOp::Purge(1)), 2);
        assert_eq!(codes(&r), vec![1, 4]);
        assert_eq!(r.cache_slice_op(CacheSliceOp::PurgeUpTo(4)), 1);
        assert_eq!(codes(&r), vec![4]);
        assert_eq!(r.cache_slice_op(CacheSliceOp::Purge(9)), 1);
    }

    #[test]
    fn cached_events_survive_a_leave_when_cleanup_is_off() {
        for (cleanup, left) in [(true, 0usize), (false, 1usize)] {
            let mut c = cfg();
            c.cleanup_cache_on_leave = cleanup;
            let t = Instant::now();
            let mut r = Room::new(c, t);
            let (a, _) = r.join("a", "a", Map::new(), None, false, t).unwrap();
            r.join("b", "b", Map::new(), None, false, t).unwrap();
            r.raise_event(
                a,
                ev(4, EventTarget::All, CacheOp::Add),
                Channel::ReliableOrdered,
                t,
            )
            .unwrap();
            assert_eq!(r.cached_for_join().len(), 1);
            r.leave(a, false, t);
            assert_eq!(r.cached_for_join().len(), left, "cleanup={cleanup}");
        }
    }
}
