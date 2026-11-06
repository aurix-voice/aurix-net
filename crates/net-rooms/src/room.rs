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
    pub max_events_per_sec: u32,
    pub max_event_bytes: usize,
    pub max_props_bytes: usize,
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
    pub groups: HashSet<u8>,
}

impl Player {
    fn info(&self) -> PlayerInfo {
        PlayerInfo {
            actor: self.actor,
            player_id: self.player_id.clone(),
            name: self.name.clone(),
            props: self.props.clone(),
            online: self.online,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
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
    #[error("player already in room")]
    Duplicate,
    #[error("not in room")]
    NotMember,
    #[error("only the master client may do this")]
    NotMaster,
    #[error("event too large")]
    EventTooLarge,
    #[error("too many events per second")]
    EventRate,
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
}

#[derive(Debug)]
pub struct Room {
    pub cfg: RoomConfig,
    players: BTreeMap<ActorId, Player>,
    master: Option<ActorId>,
    next_actor: u16,
    cache: VecDeque<CachedEvent>,
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
    pub fn online_count(&self) -> u16 {
        self.players.values().filter(|p| p.online).count() as u16
    }
    pub fn is_empty(&self) -> bool {
        self.players.is_empty()
    }

    pub fn summary(&self) -> RoomSummary {
        let props = self
            .cfg
            .props
            .iter()
            .filter(|(k, _)| self.cfg.lobby_props.iter().any(|l| l == *k))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        RoomSummary {
            id: self.cfg.id.to_string(),
            name: self.cfg.name.clone(),
            mode: self.cfg.mode,
            players: self.players.len() as u16,
            max_players: self.cfg.max_players,
            open: self.cfg.open && self.players.len() < self.cfg.max_players as usize,
            props,
        }
    }

    pub fn is_listed(&self) -> bool {
        self.cfg.visible && self.cfg.open && self.players.len() < self.cfg.max_players as usize
    }

    /// Admits a player. Returns the actor id, the player list to send them, cached events,
    /// and notifications for everyone else.
    pub fn join(
        &mut self,
        player_id: &str,
        name: &str,
        props: Map<String, Value>,
        rejoin: Option<ActorId>,
        _now: Instant,
    ) -> Result<(ActorId, Vec<Outgoing>), RoomError> {
        if props_len(&props) > self.cfg.max_props_bytes {
            return Err(RoomError::PropsTooLarge);
        }
        // Reconnect into the existing slot.
        if let Some(&a) = self.by_player.get(player_id) {
            if let Some(p) = self.players.get_mut(&a) {
                if p.online {
                    return Err(RoomError::Duplicate);
                }
                if rejoin.is_some_and(|r| r != a) {
                    return Err(RoomError::NoActor);
                }
                p.online = true;
                p.offline_since = None;
                p.name = name.to_string();
                if !props.is_empty() {
                    p.props = props;
                }
                let info = p.info();
                self.empty_since = None;
                let out = self.broadcast_except(a, ServerMsg::PlayerJoined { player: info });
                return Ok((a, out));
            }
        }
        if !self.cfg.open {
            return Err(RoomError::Closed);
        }
        if self.players.len() >= self.cfg.max_players as usize {
            return Err(RoomError::Full);
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
        };
        let info = p.info();
        self.players.insert(actor, p);
        self.by_player.insert(player_id.to_string(), actor);
        self.empty_since = None;
        let mut out = Vec::new();
        if self.master.is_none() {
            self.master = Some(actor);
        }
        out.extend(self.broadcast_except(actor, ServerMsg::PlayerJoined { player: info }));
        Ok((actor, out))
    }

    pub fn player_infos(&self) -> Vec<PlayerInfo> {
        self.players.values().map(|p| p.info()).collect()
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
            self.cache.retain(|c| c.from != actor);
            out.extend(self.broadcast_except(
                actor,
                ServerMsg::PlayerLeft {
                    actor,
                    offline: false,
                },
            ));
        }
        if self.master == Some(actor) {
            // Oldest online player becomes master.
            self.master = self
                .players
                .values()
                .filter(|p| p.online)
                .map(|p| p.actor)
                .min()
                .or_else(|| self.players.keys().next().copied());
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
        if props_len(&merged) > self.cfg.max_props_bytes {
            return Err(RoomError::PropsTooLarge);
        }
        p.props = merged;
        let props = p.props.clone();
        Ok(self.broadcast_all(ServerMsg::PlayerProps { actor, props }))
    }

    pub fn set_groups(
        &mut self,
        actor: ActorId,
        join: &[u8],
        leave: &[u8],
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
        if frame.payload.len() > self.cfg.max_event_bytes {
            return Err(RoomError::EventTooLarge);
        }
        self.refill(now);
        if self.tokens < 1.0 {
            return Err(RoomError::EventRate);
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
            max_events_per_sec: 4,
            max_event_bytes: 100,
            max_props_bytes: 200,
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
        let (a, out) = r.join("p1", "A", Map::new(), None, t).unwrap();
        assert!(out.is_empty());
        assert_eq!(r.master(), Some(a));
        let (b, out) = r.join("p2", "B", Map::new(), None, t).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(
            r.join("p3", "C", Map::new(), None, t).unwrap_err(),
            RoomError::Full
        );
        assert_eq!(
            r.join("p2", "B", Map::new(), None, t).unwrap_err(),
            RoomError::Duplicate
        );
        let out = r.leave(a, false, t);
        assert!(out.contains(&Outgoing::Control(b, ServerMsg::MasterChanged { actor: b })));
        assert_eq!(r.master(), Some(b));
    }

    #[test]
    fn offline_rejoin_keeps_actor_then_expires() {
        let t = Instant::now();
        let mut r = Room::new(cfg(), t);
        let (a, _) = r.join("p1", "A", Map::new(), None, t).unwrap();
        r.leave(a, true, t);
        assert_eq!(r.online_count(), 0);
        let (a2, _) = r
            .join("p1", "A", Map::new(), None, t + Duration::from_secs(1))
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
        let (a, _) = r.join("p1", "A", Map::new(), None, t).unwrap();
        let (b, _) = r.join("p2", "B", Map::new(), None, t).unwrap();
        let (cc, _) = r.join("p3", "C", Map::new(), None, t).unwrap();
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
        let (a, _) = r.join("p1", "A", Map::new(), None, t).unwrap();
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
}
