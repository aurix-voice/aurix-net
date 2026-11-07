//! Matchmaking tickets: clients (or the game backend) file a ticket with the room shape they
//! want; the matcher groups compatible tickets and assigns them a room name. Clients then
//! `join_or_create` that name with the same mode. Tickets live in Redis, so a ticket filed on
//! any node of a region matches tickets filed on every other node; one node at a time runs the
//! matching pass (short leader lock, re-elected every cycle). 5 minute TTL while searching.

use net_common::{NetError, ProjectId, Result, RoomMode};
use net_control::RedisStore;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TicketReq {
    pub player_id: String,
    #[serde(default)]
    pub mode: RoomMode,
    #[serde(default)]
    pub region: Option<String>,
    #[serde(default = "d_max")]
    pub max_players: u16,
    #[serde(default = "d_min")]
    pub min_players: u16,
    #[serde(default = "d_wait")]
    pub max_wait_secs: u64,
    #[serde(default)]
    pub props: serde_json::Map<String, serde_json::Value>,
}
fn d_max() -> u16 {
    8
}
fn d_min() -> u16 {
    2
}
fn d_wait() -> u64 {
    15
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum TicketState {
    Searching {
        waiting: usize,
    },
    Matched {
        room: String,
        mode: RoomMode,
        max_players: u16,
        players: Vec<String>,
    },
    Cancelled,
}

/// A group of tickets that became one room during a cycle (`match.found` webhook).
#[derive(Debug, Clone)]
pub struct MatchFound {
    pub project: ProjectId,
    pub room: String,
    pub mode: RoomMode,
    pub players: Vec<String>,
    pub tickets: Vec<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ticket {
    pub id: Uuid,
    pub project: ProjectId,
    pub req: TicketReq,
    /// Unix milliseconds.
    pub created_ms: u64,
    pub state: TicketState,
}

impl Ticket {
    fn bucket(&self) -> String {
        format!(
            "{:?}|{}|{}|{}",
            self.req.mode,
            self.req.region.as_deref().unwrap_or(""),
            self.req.max_players,
            serde_json::Value::Object(self.req.props.clone())
        )
    }

    pub fn age_secs(&self, now_ms: u64) -> u64 {
        now_ms.saturating_sub(self.created_ms) / 1000
    }

    fn searching(&self) -> bool {
        matches!(self.state, TicketState::Searching { .. })
    }
}

pub const SEARCH_TTL_SECS: u64 = 300;
pub const RESULT_TTL_SECS: u64 = 120;
const LEADER_TTL_MS: u64 = 1500;

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Pure matching pass over every searching ticket (all projects): groups per project+bucket,
/// starts full groups, or groups past the oldest ticket's wait with enough players. Mutates
/// states in place (waiting counts, matches) and returns the ids that changed plus the room
/// names started.
pub fn match_tickets(tickets: &mut [Ticket], now_ms: u64) -> (Vec<Uuid>, Vec<String>) {
    let mut groups: HashMap<(ProjectId, String), Vec<usize>> = HashMap::new();
    for (i, t) in tickets.iter().enumerate() {
        if t.searching() {
            groups.entry((t.project, t.bucket())).or_default().push(i);
        }
    }
    let mut changed = Vec::new();
    let mut rooms = Vec::new();
    for (_, mut idx) in groups {
        idx.sort_by_key(|i| (tickets[*i].created_ms, tickets[*i].id));
        let n = idx.len();
        for i in &idx {
            let new = TicketState::Searching { waiting: n };
            if tickets[*i].state != new {
                tickets[*i].state = new;
                changed.push(tickets[*i].id);
            }
        }
        let first = &tickets[idx[0]].req;
        let (max, min, wait) = (
            first.max_players as usize,
            first.min_players as usize,
            first.max_wait_secs,
        );
        let oldest_age = tickets[idx[0]].age_secs(now_ms);
        let mut i = 0;
        while i < n {
            let left = n - i;
            let take = if left >= max {
                max
            } else if left >= min.max(1) && oldest_age >= wait {
                left
            } else {
                break;
            };
            let room = format!("mm-{}", &Uuid::now_v7().simple().to_string()[..12]);
            let players: Vec<String> = idx[i..i + take]
                .iter()
                .map(|j| tickets[*j].req.player_id.clone())
                .collect();
            for j in &idx[i..i + take] {
                let t = &mut tickets[*j];
                t.state = TicketState::Matched {
                    room: room.clone(),
                    mode: t.req.mode,
                    max_players: t.req.max_players,
                    players: players.clone(),
                };
                if !changed.contains(&t.id) {
                    changed.push(t.id);
                }
            }
            rooms.push(room);
            i += take;
        }
    }
    (changed, rooms)
}

fn ticket_key(id: Uuid) -> String {
    format!("net:mm:t:{id}")
}
const SEARCHING_SET: &str = "net:mm:searching";
const LEADER_KEY: &str = "net:mm:leader";

fn stats_key(project: ProjectId, hour: u64) -> String {
    format!("net:mm:stats:{}:{hour}", project.0)
}

fn rerr(e: redis::RedisError) -> NetError {
    NetError::Redis(e.to_string())
}

/// Redis-backed ticket store shared by every node.
#[derive(Default, Clone)]
pub struct Matchmaker;

impl Matchmaker {
    async fn put(&self, redis: &RedisStore, t: &Ticket) -> Result<()> {
        let mut c = redis.raw();
        let body = serde_json::to_vec(t).map_err(|e| NetError::Internal(e.to_string()))?;
        let ttl = if t.searching() {
            SEARCH_TTL_SECS
        } else {
            RESULT_TTL_SECS
        };
        let mut p = redis::pipe();
        p.atomic().set_ex(ticket_key(t.id), body, ttl).ignore();
        if t.searching() {
            p.sadd(SEARCHING_SET, t.id.to_string()).ignore();
        } else {
            p.srem(SEARCHING_SET, t.id.to_string()).ignore();
        }
        p.query_async::<()>(&mut c).await.map_err(rerr)
    }

    async fn load_many(&self, redis: &RedisStore, ids: &[String]) -> Result<Vec<Ticket>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut c = redis.raw();
        let keys: Vec<String> = ids
            .iter()
            .filter_map(|s| s.parse::<Uuid>().ok())
            .map(ticket_key)
            .collect();
        let bodies: Vec<Option<Vec<u8>>> = c.mget(&keys).await.map_err(rerr)?;
        let mut out = Vec::with_capacity(bodies.len());
        let mut stale = Vec::new();
        for (id, b) in ids.iter().zip(bodies) {
            match b.and_then(|b| serde_json::from_slice::<Ticket>(&b).ok()) {
                Some(t) => out.push(t),
                None => stale.push(id.clone()),
            }
        }
        if !stale.is_empty() {
            let _: () = c.srem(SEARCHING_SET, stale).await.map_err(rerr)?;
        }
        Ok(out)
    }

    async fn searching_ids(&self, redis: &RedisStore) -> Result<Vec<String>> {
        let mut c = redis.raw();
        c.smembers(SEARCHING_SET).await.map_err(rerr)
    }

    /// Files a ticket; a player's earlier searching ticket in the same project is cancelled.
    pub async fn file(
        &self,
        redis: &RedisStore,
        project: ProjectId,
        req: TicketReq,
    ) -> Result<Ticket> {
        let ids = self.searching_ids(redis).await?;
        for mut t in self.load_many(redis, &ids).await? {
            if t.project == project && t.req.player_id == req.player_id && t.searching() {
                t.state = TicketState::Cancelled;
                self.put(redis, &t).await?;
            }
        }
        let ticket = Ticket {
            id: Uuid::now_v7(),
            project,
            req,
            created_ms: now_ms(),
            state: TicketState::Searching { waiting: 1 },
        };
        self.put(redis, &ticket).await?;
        Ok(ticket)
    }

    pub async fn get(
        &self,
        redis: &RedisStore,
        project: ProjectId,
        id: Uuid,
    ) -> Result<Option<Ticket>> {
        let mut c = redis.raw();
        let body: Option<Vec<u8>> = c.get(ticket_key(id)).await.map_err(rerr)?;
        Ok(body
            .and_then(|b| serde_json::from_slice::<Ticket>(&b).ok())
            .filter(|t| t.project == project))
    }

    pub async fn cancel(&self, redis: &RedisStore, project: ProjectId, id: Uuid) -> Result<bool> {
        match self.get(redis, project, id).await? {
            Some(mut t) if t.searching() => {
                t.state = TicketState::Cancelled;
                self.put(redis, &t).await?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    /// One matching cycle; only the node holding the leader lock does work. Returns the matches
    /// formed (empty when not leader or nothing matched) so the caller can notify webhooks.
    pub async fn run_cycle(&self, redis: &RedisStore, node: &str) -> Result<Vec<MatchFound>> {
        let mut c = redis.raw();
        let got: Option<String> = redis::cmd("SET")
            .arg(LEADER_KEY)
            .arg(node)
            .arg("NX")
            .arg("PX")
            .arg(LEADER_TTL_MS)
            .query_async(&mut c)
            .await
            .map_err(rerr)?;
        if got.is_none() {
            // Somebody else leads; a dead leader's lock expires within LEADER_TTL_MS.
            let leader: Option<String> = c.get(LEADER_KEY).await.map_err(rerr)?;
            if leader.as_deref() != Some(node) {
                return Ok(Vec::new());
            }
        }
        let ids = self.searching_ids(redis).await?;
        let mut tickets = self.load_many(redis, &ids).await?;
        let now = now_ms();
        tickets.retain(|t| t.searching() && t.age_secs(now) < SEARCH_TTL_SECS);
        let (changed, _rooms) = match_tickets(&mut tickets, now);
        let mut found: Vec<MatchFound> = Vec::new();
        for t in tickets.iter().filter(|t| changed.contains(&t.id)) {
            self.put(redis, t).await?;
            if let TicketState::Matched {
                room,
                mode,
                players,
                ..
            } = &t.state
            {
                match found
                    .iter_mut()
                    .find(|m| m.project == t.project && m.room == *room)
                {
                    Some(m) => m.tickets.push(t.id),
                    None => found.push(MatchFound {
                        project: t.project,
                        room: room.clone(),
                        mode: *mode,
                        players: players.clone(),
                        tickets: vec![t.id],
                    }),
                }
            }
        }
        let hour = now / 3_600_000;
        let mut per_project: std::collections::HashMap<ProjectId, i64> = Default::default();
        for m in &found {
            *per_project.entry(m.project).or_default() += 1;
        }
        for (project, n) in per_project {
            let key = stats_key(project, hour);
            let mut p = redis::pipe();
            p.atomic()
                .incr(&key, n)
                .ignore()
                .expire(&key, 26 * 3600)
                .ignore();
            p.query_async::<()>(&mut c).await.map_err(rerr)?;
        }
        Ok(found)
    }

    /// Tickets of one project still waiting, oldest first.
    pub async fn list_searching(
        &self,
        redis: &RedisStore,
        project: ProjectId,
    ) -> Result<Vec<Ticket>> {
        let ids = self.searching_ids(redis).await?;
        let now = now_ms();
        let mut out: Vec<Ticket> = self
            .load_many(redis, &ids)
            .await?
            .into_iter()
            .filter(|t| t.project == project && t.searching() && t.age_secs(now) < SEARCH_TTL_SECS)
            .collect();
        out.sort_by_key(|t| t.created_ms);
        Ok(out)
    }

    /// Matches formed for a project: (last hour, last 24 h).
    pub async fn matched_stats(
        &self,
        redis: &RedisStore,
        project: ProjectId,
    ) -> Result<(i64, i64)> {
        let hour = now_ms() / 3_600_000;
        let keys: Vec<String> = (0..24).map(|i| stats_key(project, hour - i)).collect();
        let mut c = redis.raw();
        let vals: Vec<Option<i64>> = c.mget(&keys).await.map_err(rerr)?;
        let last_hour = vals.first().copied().flatten().unwrap_or(0);
        let day: i64 = vals.iter().map(|v| v.unwrap_or(0)).sum();
        Ok((last_hour, day))
    }

    pub async fn searching(&self, redis: &RedisStore) -> Result<usize> {
        let mut c = redis.raw();
        c.scard(SEARCHING_SET).await.map_err(rerr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(player: &str, max: u16, min: u16, wait: u64, props: serde_json::Value) -> TicketReq {
        TicketReq {
            player_id: player.into(),
            mode: RoomMode::Relay,
            region: None,
            max_players: max,
            min_players: min,
            max_wait_secs: wait,
            props: props.as_object().cloned().unwrap_or_default(),
        }
    }

    fn ticket(project: ProjectId, r: TicketReq, created_ms: u64) -> Ticket {
        Ticket {
            id: Uuid::now_v7(),
            project,
            req: r,
            created_ms,
            state: TicketState::Searching { waiting: 1 },
        }
    }

    fn room_of(t: &Ticket) -> Option<&str> {
        match &t.state {
            TicketState::Matched { room, .. } => Some(room),
            _ => None,
        }
    }

    #[test]
    fn fills_groups_and_times_out() {
        let p = ProjectId::new();
        let q = ProjectId::new();
        let mut ts = vec![
            ticket(p, req("a", 2, 2, 15, serde_json::json!({})), 1000),
            ticket(p, req("b", 2, 2, 15, serde_json::json!({})), 1100),
            ticket(p, req("c", 2, 2, 15, serde_json::json!({})), 1200),
            // different props = different bucket
            ticket(p, req("d", 2, 2, 15, serde_json::json!({"map": "x"})), 1000),
            // other project never mixes
            ticket(q, req("e", 2, 2, 15, serde_json::json!({})), 1000),
            // solo with min 1 and zero wait starts immediately
            ticket(q, req("f", 4, 1, 0, serde_json::json!({"solo": 1})), 1000),
        ];
        let (changed, rooms) = match_tickets(&mut ts, 2000);
        assert_eq!(rooms.len(), 2);
        assert_eq!(room_of(&ts[0]), room_of(&ts[1]));
        assert!(room_of(&ts[0]).is_some());
        assert_eq!(ts[2].state, TicketState::Searching { waiting: 3 });
        assert_eq!(ts[3].state, TicketState::Searching { waiting: 1 });
        assert_eq!(ts[4].state, TicketState::Searching { waiting: 1 });
        assert!(room_of(&ts[5]).is_some());
        assert!(changed.contains(&ts[2].id)); // waiting count changed 1 -> 3

        // "c" alone: no partner, min 2 -> keeps waiting even past max_wait.
        let (_, rooms) = match_tickets(&mut ts, 1000 + 20_000);
        assert!(rooms.is_empty());
        // c + e are different projects; add g to p: min reached and wait passed -> start.
        ts.push(ticket(p, req("g", 2, 2, 15, serde_json::json!({})), 30_000));
        let (_, rooms) = match_tickets(&mut ts, 31_000);
        assert_eq!(rooms.len(), 1);
        assert_eq!(room_of(&ts[2]), room_of(&ts[6]));

        // Timeout with min < max: 3 of max 4 start after the oldest waited long enough.
        let r = ProjectId::new();
        let mut ts = vec![
            ticket(r, req("x", 4, 2, 5, serde_json::json!({})), 0),
            ticket(r, req("y", 4, 2, 5, serde_json::json!({})), 100),
            ticket(r, req("z", 4, 2, 5, serde_json::json!({})), 200),
        ];
        assert!(match_tickets(&mut ts, 4_000).1.is_empty());
        assert_eq!(match_tickets(&mut ts, 5_000).1.len(), 1);
        assert!(ts.iter().all(|t| room_of(t).is_some()));
    }
}
