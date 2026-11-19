//! Matchmaking tickets: clients (or the game backend) file a ticket with the room shape they
//! want; the matcher groups compatible tickets and assigns them a room name. Clients then
//! `join_or_create` that name with the same mode. Tickets live in Redis, so a ticket filed on
//! any node of a region matches tickets filed on every other node; one node at a time runs the
//! matching pass (short leader lock, re-elected every cycle). 5 minute TTL while searching.

use net_common::{NetError, ProjectId, Result, RoomMode};
use net_control::{LiveRoom, RedisStore};
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::LazyLock;
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
    /// Player attributes other tickets may filter on (rank, language, ...); not part of the bucket.
    #[serde(default)]
    pub attrs: serde_json::Map<String, serde_json::Value>,
    /// SQL-like condition the *other* players' `attrs` must satisfy (`rank >= 10 AND lang = 'en'`).
    /// Two tickets only share a room when each one's filter accepts the other's attrs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    /// Prefer a seat in an already running open room (same mode, free slot, lobby props satisfy
    /// `filter`) over waiting for a fresh group; `match_type` picks among candidates:
    /// `fill` (fullest first, default), `serial` (emptiest first), `random`.
    #[serde(default)]
    pub fill_existing: bool,
    /// When `max_wait_secs` runs out with fewer than `max_players`, hand the group a room anyway
    /// and report the empty seats as `bots` so the game (master or WASM logic) fills them with AI
    /// (bot backfill); without it the group starts short-handed.
    #[serde(default)]
    pub bots: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_type: Option<String>,
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
        /// Seats the game should fill with bots (0 unless the ticket asked for `bots`).
        #[serde(default)]
        bots: u16,
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
    pub bots: u16,
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

/// Picks one room out of `rooms` (already filtered to acceptable ones) by `match_type`.
pub fn pick_room<'a>(rooms: &[&'a LiveRoom], match_type: Option<&str>) -> Option<&'a LiveRoom> {
    match match_type.unwrap_or("fill") {
        "serial" => rooms
            .iter()
            .min_by_key(|r| (r.players, r.name.clone()))
            .copied(),
        "random" => {
            let room_count = rooms.len();
            (room_count > 0).then(|| rooms[(now_ms() as usize) % room_count])
        }
        _ => rooms
            .iter()
            .max_by_key(|r| (r.players, std::cmp::Reverse(r.name.clone())))
            .copied(),
    }
}

/// Seats `fill_existing` tickets into live rooms before grouping: open, visible, same mode, a free
/// slot, lobby props accepted by the ticket's filter. Seats taken in this pass are counted so one
/// free slot is not handed to two tickets.
pub fn fill_existing(
    tickets: &mut [Ticket],
    rooms: &HashMap<ProjectId, Vec<LiveRoom>>,
) -> Vec<Uuid> {
    let mut changed = Vec::new();
    let mut taken: HashMap<(ProjectId, String), u16> = HashMap::new();
    let mut order: Vec<usize> = (0..tickets.len()).collect();
    order.sort_by_key(|i| (tickets[*i].created_ms, tickets[*i].id));
    for i in order {
        let ticket = &tickets[i];
        if !ticket.searching() || !ticket.req.fill_existing {
            continue;
        }
        let Some(live) = rooms.get(&ticket.project) else {
            continue;
        };
        let filter = ticket
            .req
            .filter
            .as_deref()
            .and_then(|f| crate::mmfilter::Filter::parse(f).ok());
        let mode = ticket.req.mode.as_str();
        let candidates: Vec<&LiveRoom> = live
            .iter()
            .filter(|r| {
                let extra = taken
                    .get(&(ticket.project, r.name.clone()))
                    .copied()
                    .unwrap_or(0);
                r.open
                    && r.visible
                    && r.mode == mode
                    && r.players + extra < r.max_players
                    && filter
                        .as_ref()
                        .is_none_or(|f| r.props.as_object().is_some_and(|p| f.matches(p)))
            })
            .collect();
        let Some(room) = pick_room(&candidates, ticket.req.match_type.as_deref()) else {
            continue;
        };
        let (name, max_players) = (room.name.clone(), room.max_players);
        *taken.entry((ticket.project, name.clone())).or_default() += 1;
        let ticket = &mut tickets[i];
        ticket.state = TicketState::Matched {
            room: name,
            mode: ticket.req.mode,
            max_players,
            players: vec![ticket.req.player_id.clone()],
            bots: 0,
        };
        changed.push(ticket.id);
    }
    changed
}

/// Pure matching pass over every searching ticket (all projects): groups per project+bucket,
/// starts full groups, or groups past the oldest ticket's wait with enough players. Mutates
/// states in place (waiting counts, matches) and returns the ids that changed plus the room
/// names started.
pub fn match_tickets(tickets: &mut [Ticket], now_ms: u64) -> (Vec<Uuid>, Vec<String>) {
    let mut groups: HashMap<(ProjectId, String), Vec<usize>> = HashMap::new();
    for (i, ticket) in tickets.iter().enumerate() {
        if ticket.searching() {
            groups
                .entry((ticket.project, ticket.bucket()))
                .or_default()
                .push(i);
        }
    }
    let mut changed = Vec::new();
    let mut rooms = Vec::new();
    for (_, mut idx) in groups {
        idx.sort_by_key(|i| (tickets[*i].created_ms, tickets[*i].id));
        let waiting = idx.len();
        for i in &idx {
            let new = TicketState::Searching { waiting };
            if tickets[*i].state != new {
                tickets[*i].state = new;
                changed.push(tickets[*i].id);
            }
        }
        // `max_players` is part of the bucket; `min_players` and `max_wait_secs` are each
        // ticket's own.
        let max = tickets[idx[0]].req.max_players as usize;
        let mins: Vec<usize> = idx
            .iter()
            .map(|i| tickets[*i].req.min_players.max(1) as usize)
            .collect();
        let filters: Vec<Option<crate::mmfilter::Filter>> = idx
            .iter()
            .map(|i| {
                tickets[*i]
                    .req
                    .filter
                    .as_deref()
                    .and_then(|f| crate::mmfilter::Filter::parse(f).ok())
            })
            .collect();
        let attrs: Vec<serde_json::Map<String, serde_json::Value>> =
            idx.iter().map(|i| tickets[*i].req.attrs.clone()).collect();
        let compatible = |a: usize, b: usize| -> bool {
            let ok = |f: &Option<crate::mmfilter::Filter>, other: usize| {
                f.as_ref().is_none_or(|f| f.matches(&attrs[other]))
            };
            ok(&filters[a], b) && ok(&filters[b], a)
        };
        // Greedy: the oldest unassigned ticket seeds a group, later tickets join when they are
        // mutually compatible with every member. A group fills a room when it reaches `max`, or
        // when its seed has waited its `max_wait_secs` and the group is large enough for every
        // member's `min_players`; members who want more players than there are stay in the queue.
        let mut taken = vec![false; waiting];
        for seed in 0..waiting {
            if taken[seed] {
                continue;
            }
            let mut group = vec![seed];
            for cand in (seed + 1..waiting).filter(|c| !taken[*c]) {
                if group.len() >= max {
                    break;
                }
                if group.iter().all(|m| compatible(*m, cand)) {
                    group.push(cand);
                }
            }
            if group.len() < max {
                let seed_ticket = &tickets[idx[seed]];
                if seed_ticket.age_secs(now_ms) < seed_ticket.req.max_wait_secs {
                    continue;
                }
                loop {
                    let size = group.len();
                    group.retain(|g| mins[*g] <= size);
                    if group.len() == size {
                        break;
                    }
                }
                if group.first() != Some(&seed) {
                    continue;
                }
            }
            let room = format!("mm-{}", &Uuid::new_v4().simple().to_string()[..12]);
            let players: Vec<String> = group
                .iter()
                .map(|g| tickets[idx[*g]].req.player_id.clone())
                .collect();
            // short-handed start: bot seats when every member agreed to play with bots
            let bots = if group.len() < max && group.iter().all(|g| tickets[idx[*g]].req.bots) {
                (max - group.len()) as u16
            } else {
                0
            };
            for member in &group {
                taken[*member] = true;
                let ticket = &mut tickets[idx[*member]];
                ticket.state = TicketState::Matched {
                    room: room.clone(),
                    mode: ticket.req.mode,
                    max_players: ticket.req.max_players,
                    players: players.clone(),
                    bots,
                };
                if !changed.contains(&ticket.id) {
                    changed.push(ticket.id);
                }
            }
            rooms.push(room);
        }
    }
    (changed, rooms)
}

// The keys of one project carry its id as a Redis Cluster hash tag, so they share a slot and a
// script may touch several of them.
fn ticket_key(project: ProjectId, id: impl std::fmt::Display) -> String {
    format!("net:mm:{{{}}}:t:{id}", project.0)
}
/// Ids of the tickets one project has waiting.
fn searching_key(project: ProjectId) -> String {
    format!("net:mm:{{{}}}:searching", project.0)
}
/// The id of the ticket a player filed last.
fn player_key(project: ProjectId, player: &str) -> String {
    format!("net:mm:{{{}}}:player:{player}", project.0)
}
/// Projects that have tickets waiting.
const PROJECTS_SET: &str = "net:mm:projects";
const LEADER_KEY: &str = "net:mm:leader";

/// `KEYS`: the searching set of the project, then one ticket key each. `ARGV` per ticket: id, body, TTL in
/// seconds, 1 if it stays searching.
static REPLACE_SEARCHING: LazyLock<redis::Script> = LazyLock::new(|| {
    redis::Script::new(
        r"
local n = #KEYS - 1
for j = 1, n do
  if redis.call('SISMEMBER', KEYS[1], ARGV[4 * j - 3]) == 0 or redis.call('EXISTS', KEYS[j + 1]) == 0 then
    return 0
  end
end
for j = 1, n do
  redis.call('SET', KEYS[j + 1], ARGV[4 * j - 2], 'EX', ARGV[4 * j - 1])
  if ARGV[4 * j] == '0' then
    redis.call('SREM', KEYS[1], ARGV[4 * j - 3])
  end
end
return 1
",
    )
});

/// Files a ticket in place of the one its player filed before, unless another took that place
/// meanwhile. `KEYS`: the player key, the searching set of the project, the key of the new
/// ticket, the key of the one before. `ARGV`: the id of the one before (empty if none), the new
/// id, its body and TTL, the one before as cancelled (empty if it is not waiting) and its TTL.
static FILE_TICKET: LazyLock<redis::Script> = LazyLock::new(|| {
    redis::Script::new(
        r"
if (redis.call('GET', KEYS[1]) or '') ~= ARGV[1] then
  return 0
end
if ARGV[5] ~= '' and redis.call('SISMEMBER', KEYS[2], ARGV[1]) == 1 and redis.call('EXISTS', KEYS[4]) == 1 then
  redis.call('SET', KEYS[4], ARGV[5], 'EX', ARGV[6])
  redis.call('SREM', KEYS[2], ARGV[1])
end
redis.call('SET', KEYS[3], ARGV[3], 'EX', ARGV[4])
redis.call('SADD', KEYS[2], ARGV[2])
redis.call('SET', KEYS[1], ARGV[2], 'EX', ARGV[4])
return 1
",
    )
});

fn stats_key(project: ProjectId, hour: u64) -> String {
    format!("net:mm:{{{}}}:stats:{hour}", project.0)
}

fn rerr(e: redis::RedisError) -> NetError {
    NetError::Redis(e.to_string())
}

/// Redis-backed ticket store shared by every node.
#[derive(Default, Clone)]
pub struct Matchmaker;

impl Matchmaker {
    /// Writes `tickets`, all of one project, over their stored copies if every one of them is
    /// still searching there, and none of them otherwise. A cancel and the matching cycle both read a ticket before
    /// they change it; this is what keeps the later of the two from undoing the earlier.
    async fn replace_searching(&self, redis: &RedisStore, tickets: &[&Ticket]) -> Result<bool> {
        let Some(first) = tickets.first() else {
            return Ok(true);
        };
        let mut call = REPLACE_SEARCHING.key(searching_key(first.project));
        for ticket in tickets {
            let body = serde_json::to_vec(ticket).map_err(|e| NetError::Internal(e.to_string()))?;
            let ttl = if ticket.searching() {
                SEARCH_TTL_SECS
            } else {
                RESULT_TTL_SECS
            };
            call.key(ticket_key(ticket.project, ticket.id))
                .arg(ticket.id.to_string())
                .arg(body)
                .arg(ttl)
                .arg(u8::from(ticket.searching()));
        }
        let mut conn = redis.raw();
        call.invoke_async(&mut conn).await.map_err(rerr)
    }

    /// The tickets waiting in `projects`.
    async fn waiting(&self, redis: &RedisStore, projects: &[ProjectId]) -> Result<Vec<Ticket>> {
        if projects.is_empty() {
            return Ok(Vec::new());
        }
        let mut conn = redis.raw();
        let mut sets = redis::pipe();
        for project in projects {
            sets.smembers(searching_key(*project));
        }
        let sets: Vec<Vec<String>> = sets.query_async(&mut conn).await.map_err(rerr)?;
        let sets: Vec<(ProjectId, Vec<String>)> = projects
            .iter()
            .copied()
            .zip(sets)
            .filter(|(_, ids)| !ids.is_empty())
            .collect();
        if sets.is_empty() {
            return Ok(Vec::new());
        }
        let mut bodies = redis::pipe();
        for (project, ids) in &sets {
            let keys: Vec<String> = ids.iter().map(|id| ticket_key(*project, id)).collect();
            bodies.cmd("MGET").arg(keys);
        }
        let bodies: Vec<Vec<Option<Vec<u8>>>> =
            bodies.query_async(&mut conn).await.map_err(rerr)?;
        let mut out = Vec::new();
        let mut stale = redis::pipe();
        for ((project, ids), bodies) in sets.into_iter().zip(bodies) {
            for (id, body) in ids.into_iter().zip(bodies) {
                match body.and_then(|body| serde_json::from_slice::<Ticket>(&body).ok()) {
                    Some(ticket) => out.push(ticket),
                    None => {
                        stale.srem(searching_key(project), id).ignore();
                    }
                }
            }
        }
        if !stale.is_empty() {
            stale.query_async::<()>(&mut conn).await.map_err(rerr)?;
        }
        Ok(out)
    }

    /// Files a ticket; a player's earlier searching ticket in the same project is cancelled.
    pub async fn file(
        &self,
        redis: &RedisStore,
        project: ProjectId,
        req: TicketReq,
    ) -> Result<Ticket> {
        if let Some(filter) = req.filter.as_deref() {
            crate::mmfilter::Filter::parse(filter)
                .map_err(|e| NetError::Invalid(format!("filter: {e}")))?;
        }
        if serde_json::to_vec(&req.attrs)
            .map(|v| v.len())
            .unwrap_or(usize::MAX)
            > 1024
        {
            return Err(NetError::Invalid("attrs larger than 1 KiB".into()));
        }
        let ticket = Ticket {
            id: Uuid::now_v7(),
            project,
            req,
            created_ms: now_ms(),
            state: TicketState::Searching { waiting: 1 },
        };
        let body = serde_json::to_vec(&ticket).map_err(|e| NetError::Internal(e.to_string()))?;
        let player = player_key(project, &ticket.req.player_id);
        let mut conn = redis.raw();
        loop {
            let earlier: Option<String> = conn.get(&player).await.map_err(rerr)?;
            let earlier = earlier.unwrap_or_default();
            let mut cancelled = Vec::new();
            if let Ok(id) = earlier.parse::<Uuid>() {
                if let Some(mut earlier) = self.get(redis, project, id).await? {
                    if earlier.searching() {
                        earlier.state = TicketState::Cancelled;
                        cancelled = serde_json::to_vec(&earlier)
                            .map_err(|e| NetError::Internal(e.to_string()))?;
                    }
                }
            }
            let filed: bool = FILE_TICKET
                .key(&player)
                .key(searching_key(project))
                .key(ticket_key(project, ticket.id))
                .key(ticket_key(project, &earlier))
                .arg(&earlier)
                .arg(ticket.id.to_string())
                .arg(&body)
                .arg(SEARCH_TTL_SECS)
                .arg(cancelled)
                .arg(RESULT_TTL_SECS)
                .invoke_async(&mut conn)
                .await
                .map_err(rerr)?;
            if filed {
                break;
            }
        }
        conn.sadd::<_, _, ()>(PROJECTS_SET, project.0.to_string())
            .await
            .map_err(rerr)?;
        Ok(ticket)
    }

    pub async fn get(
        &self,
        redis: &RedisStore,
        project: ProjectId,
        id: Uuid,
    ) -> Result<Option<Ticket>> {
        let mut conn = redis.raw();
        let body: Option<Vec<u8>> = conn.get(ticket_key(project, id)).await.map_err(rerr)?;
        Ok(body
            .and_then(|b| serde_json::from_slice::<Ticket>(&b).ok())
            .filter(|t| t.project == project))
    }

    pub async fn cancel(&self, redis: &RedisStore, project: ProjectId, id: Uuid) -> Result<bool> {
        match self.get(redis, project, id).await? {
            Some(mut ticket) if ticket.searching() => {
                ticket.state = TicketState::Cancelled;
                self.replace_searching(redis, &[&ticket]).await
            }
            _ => Ok(false),
        }
    }

    /// One matching cycle; only the node holding the leader lock does work. Returns the matches
    /// formed (empty when not leader or nothing matched) so the caller can notify webhooks.
    pub async fn run_cycle(&self, redis: &RedisStore, node: &str) -> Result<Vec<MatchFound>> {
        let mut conn = redis.raw();
        let got: Option<String> = redis::cmd("SET")
            .arg(LEADER_KEY)
            .arg(node)
            .arg("NX")
            .arg("PX")
            .arg(LEADER_TTL_MS)
            .query_async(&mut conn)
            .await
            .map_err(rerr)?;
        if got.is_none() {
            // Somebody else leads; a dead leader's lock expires within LEADER_TTL_MS.
            let leader: Option<String> = conn.get(LEADER_KEY).await.map_err(rerr)?;
            if leader.as_deref() != Some(node) {
                return Ok(Vec::new());
            }
        }
        let projects: Vec<String> = conn.smembers(PROJECTS_SET).await.map_err(rerr)?;
        let projects: Vec<ProjectId> = projects
            .iter()
            .filter_map(|id| id.parse().ok().map(ProjectId))
            .collect();
        let mut tickets = self.waiting(redis, &projects).await?;
        let busy: std::collections::HashSet<ProjectId> =
            tickets.iter().map(|ticket| ticket.project).collect();
        for project in projects.iter().filter(|project| !busy.contains(project)) {
            // A filing adds its project after its ticket, so a ticket filed meanwhile either
            // shows in the count or puts the project back itself.
            let name = project.0.to_string();
            conn.srem::<_, _, ()>(PROJECTS_SET, &name)
                .await
                .map_err(rerr)?;
            let left: u64 = conn.scard(searching_key(*project)).await.map_err(rerr)?;
            if left > 0 {
                conn.sadd::<_, _, ()>(PROJECTS_SET, &name)
                    .await
                    .map_err(rerr)?;
            }
        }
        let now = now_ms();
        tickets.retain(|t| t.searching() && t.age_secs(now) < SEARCH_TTL_SECS);
        let mut live: HashMap<ProjectId, Vec<LiveRoom>> = HashMap::new();
        for ticket in tickets
            .iter()
            .filter(|t| t.req.fill_existing)
            .map(|t| t.project)
            .collect::<std::collections::HashSet<_>>()
        {
            live.insert(ticket, redis.list_rooms(ticket).await.unwrap_or_default());
        }
        let seated = fill_existing(&mut tickets, &live);
        let (grouped, _rooms) = match_tickets(&mut tickets, now);
        // The tickets of a new room are written together, so it starts with all of them or not
        // at all; a ticket seated into a running room or still waiting is written on its own.
        let mut writes: Vec<Vec<&Ticket>> = Vec::new();
        let mut rooms: HashMap<(ProjectId, &str), usize> = HashMap::new();
        for ticket in &tickets {
            match &ticket.state {
                TicketState::Matched { room, .. } if grouped.contains(&ticket.id) => {
                    let slot = *rooms.entry((ticket.project, room)).or_insert_with(|| {
                        writes.push(Vec::new());
                        writes.len() - 1
                    });
                    writes[slot].push(ticket);
                }
                _ if seated.contains(&ticket.id) || grouped.contains(&ticket.id) => {
                    writes.push(vec![ticket])
                }
                _ => {}
            }
        }
        let mut found: Vec<MatchFound> = Vec::new();
        for group in writes {
            if !self.replace_searching(redis, &group).await? {
                continue;
            }
            for ticket in group {
                let TicketState::Matched {
                    room,
                    mode,
                    players,
                    bots,
                    ..
                } = &ticket.state
                else {
                    continue;
                };
                match found
                    .iter_mut()
                    .find(|m| m.project == ticket.project && m.room == *room)
                {
                    Some(started) => {
                        started.tickets.push(ticket.id);
                        // Tickets seated into a running room each list only their own player.
                        for player in players {
                            if !started.players.contains(player) {
                                started.players.push(player.clone());
                            }
                        }
                    }
                    None => found.push(MatchFound {
                        project: ticket.project,
                        room: room.clone(),
                        mode: *mode,
                        players: players.clone(),
                        tickets: vec![ticket.id],
                        bots: *bots,
                    }),
                }
            }
        }
        let hour = now / 3_600_000;
        let mut per_project: std::collections::HashMap<ProjectId, i64> = Default::default();
        for started in &found {
            *per_project.entry(started.project).or_default() += 1;
        }
        for (project, count) in per_project {
            let key = stats_key(project, hour);
            let mut pipe = redis::pipe();
            pipe.atomic()
                .incr(&key, count)
                .ignore()
                .expire(&key, 26 * 3600)
                .ignore();
            pipe.query_async::<()>(&mut conn).await.map_err(rerr)?;
        }
        Ok(found)
    }

    /// Tickets of one project still waiting, oldest first.
    pub async fn list_searching(
        &self,
        redis: &RedisStore,
        project: ProjectId,
    ) -> Result<Vec<Ticket>> {
        let now = now_ms();
        let mut out: Vec<Ticket> = self
            .waiting(redis, &[project])
            .await?
            .into_iter()
            .filter(|t| t.searching() && t.age_secs(now) < SEARCH_TTL_SECS)
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
        let mut conn = redis.raw();
        let vals: Vec<Option<i64>> = conn.mget(&keys).await.map_err(rerr)?;
        let last_hour = vals.first().copied().flatten().unwrap_or(0);
        let day: i64 = vals.iter().map(|v| v.unwrap_or(0)).sum();
        Ok((last_hour, day))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bot_backfill_reports_empty_seats_only_when_everyone_agreed() {
        let project = ProjectId::new();
        let with_bots = |id: &str| {
            let mut request = req(id, 4, 2, 15, serde_json::json!({}));
            request.bots = true;
            request
        };
        let mut ts = vec![
            ticket(project, with_bots("a"), 1000),
            ticket(project, with_bots("b"), 1100),
        ];
        // before max_wait nothing starts; after it the pair starts with 2 bot seats
        assert!(match_tickets(&mut ts, 5_000).1.is_empty());
        let (_, rooms) = match_tickets(&mut ts, 20_000);
        assert_eq!(rooms.len(), 1);
        for ticket in &ts {
            assert!(
                matches!(&ticket.state, TicketState::Matched { bots: 2, players, .. } if players.len() == 2)
            );
        }
        // one member without `bots`: the group starts short-handed, no bot seats
        let mut ts = vec![
            ticket(project, with_bots("c"), 1000),
            ticket(project, req("d", 4, 2, 15, serde_json::json!({})), 1100),
        ];
        match_tickets(&mut ts, 20_000);
        assert!(matches!(&ts[0].state, TicketState::Matched { bots: 0, .. }));
        // a full group never gets bots
        let mut ts: Vec<Ticket> = ["e", "f", "g", "h"]
            .iter()
            .enumerate()
            .map(|(i, id)| ticket(project, with_bots(id), 1000 + i as u64))
            .collect();
        match_tickets(&mut ts, 1_500);
        assert!(matches!(&ts[3].state, TicketState::Matched { bots: 0, .. }));
    }

    fn req(player: &str, max: u16, min: u16, wait: u64, props: serde_json::Value) -> TicketReq {
        TicketReq {
            player_id: player.into(),
            mode: RoomMode::Relay,
            region: None,
            max_players: max,
            min_players: min,
            max_wait_secs: wait,
            props: props.as_object().cloned().unwrap_or_default(),
            attrs: Default::default(),
            filter: None,
            fill_existing: false,
            bots: false,
            match_type: None,
        }
    }

    fn ticket(project: ProjectId, request: TicketReq, created_ms: u64) -> Ticket {
        Ticket {
            id: Uuid::now_v7(),
            project,
            req: request,
            created_ms,
            state: TicketState::Searching { waiting: 1 },
        }
    }

    fn room_of(ticket: &Ticket) -> Option<&str> {
        match &ticket.state {
            TicketState::Matched { room, .. } => Some(room),
            _ => None,
        }
    }

    #[test]
    fn fills_existing_rooms_by_match_type() {
        let room = |name: &str, players: u16, map: &str| LiveRoom {
            room: net_common::RoomId(Uuid::new_v4()),
            node: "n".into(),
            name: name.into(),
            mode: "relay".into(),
            players,
            max_players: 4,
            open: true,
            visible: true,
            props: serde_json::json!({"map": map}),
        };
        let project = ProjectId::from_uuid(Uuid::nil());
        let mut rooms = HashMap::new();
        rooms.insert(
            project,
            vec![
                room("a", 3, "dust"),
                room("b", 1, "dust"),
                room("c", 1, "nuke"),
                room("full", 4, "dust"),
            ],
        );
        let make = |p: &str, mt: Option<&str>| {
            let mut request = req(p, 4, 2, 15, serde_json::json!({}));
            request.fill_existing = true;
            request.filter = Some("map = 'dust'".into());
            request.match_type = mt.map(str::to_string);
            Ticket {
                id: Uuid::now_v7(),
                project,
                req: request,
                created_ms: 0,
                state: TicketState::Searching { waiting: 0 },
            }
        };
        let mut tickets = vec![
            make("p1", None),
            make("p2", None),
            make("p3", Some("serial")),
            make("p4", Some("serial")),
        ];
        let changed = fill_existing(&mut tickets, &rooms);
        assert_eq!(changed.len(), 4);
        let room_of = |i: usize| match &tickets[i].state {
            TicketState::Matched { room, .. } => room.clone(),
            s => panic!("{s:?}"),
        };
        assert_eq!(room_of(0), "a"); // fullest with a seat
        assert_eq!(room_of(1), "b"); // "a" is now full after p1's seat
        assert_eq!(room_of(2), "b"); // emptiest dust room
        assert_eq!(room_of(3), "b");
        let mut none = vec![make("p5", None)];
        none[0].req.filter = Some("map = 'office'".into());
        assert!(fill_existing(&mut none, &rooms).is_empty());
        assert!(none[0].searching());
    }

    #[test]
    fn filters_split_a_bucket_by_attrs() {
        let make = |p: &str, rank: i64, filter: &str| {
            let mut request = req(p, 2, 2, 15, serde_json::json!({}));
            request.attrs = serde_json::json!({"rank": rank})
                .as_object()
                .cloned()
                .unwrap();
            request.filter = Some(filter.to_string());
            Ticket {
                id: Uuid::now_v7(),
                project: ProjectId::from_uuid(Uuid::nil()),
                req: request,
                created_ms: 0,
                state: TicketState::Searching { waiting: 0 },
            }
        };
        let mut tickets = vec![
            make("low", 5, "rank < 10"),
            make("high", 50, "rank >= 40"),
            make("low2", 7, "rank < 10"),
            make("high2", 45, "rank >= 40"),
        ];
        let (_, rooms) = match_tickets(&mut tickets, 1000);
        assert_eq!(rooms.len(), 2);
        let room_of = |p: &str| match &tickets.iter().find(|t| t.req.player_id == p).unwrap().state
        {
            TicketState::Matched { room, .. } => room.clone(),
            s => panic!("{p}: {s:?}"),
        };
        assert_eq!(room_of("low"), room_of("low2"));
        assert_eq!(room_of("high"), room_of("high2"));
        assert_ne!(room_of("low"), room_of("high"));
        // one-sided rejection keeps them apart even though the other side would accept
        let mut odd = vec![
            make("picky", 30, "rank > 100"),
            make("open", 30, "rank > 0"),
        ];
        let (_, rooms) = match_tickets(&mut odd, 1000);
        assert!(rooms.is_empty());
    }

    #[test]
    fn short_group_respects_each_ticket_minimum_and_wait() {
        let project = ProjectId::new();
        let none = || serde_json::json!({});
        // "a" accepts two players after 1 s, "b" wants four: nobody starts with two.
        let mut ts = vec![
            ticket(project, req("a", 4, 2, 1, none()), 0),
            ticket(project, req("b", 4, 4, 60, none()), 100),
        ];
        assert!(match_tickets(&mut ts, 3_000).1.is_empty());
        // "c" also accepts two: "a" and "c" start, "b" keeps waiting.
        ts.push(ticket(project, req("c", 4, 2, 60, none()), 200));
        assert_eq!(match_tickets(&mut ts, 3_000).1.len(), 1);
        assert_eq!(room_of(&ts[0]), room_of(&ts[2]));
        assert!(room_of(&ts[0]).is_some());
        assert!(room_of(&ts[1]).is_none());

        // The wait is the seed's own: "y" and "z" start after their 1 s, although the older
        // "x", whose filter keeps it apart, asked to wait a minute.
        let mut lone = req("x", 4, 2, 60, none());
        lone.filter = Some("rank >= 50".into());
        let mut ts = vec![
            ticket(project, lone, 0),
            ticket(project, req("y", 4, 2, 1, none()), 100),
            ticket(project, req("z", 4, 2, 1, none()), 200),
        ];
        assert_eq!(match_tickets(&mut ts, 3_000).1.len(), 1);
        assert_eq!(room_of(&ts[1]), room_of(&ts[2]));
        assert!(room_of(&ts[0]).is_none());
    }

    #[test]
    fn fills_groups_and_times_out() {
        let project = ProjectId::new();
        let other = ProjectId::new();
        let mut ts = vec![
            ticket(project, req("a", 2, 2, 15, serde_json::json!({})), 1000),
            ticket(project, req("b", 2, 2, 15, serde_json::json!({})), 1100),
            ticket(project, req("c", 2, 2, 15, serde_json::json!({})), 1200),
            // different props = different bucket
            ticket(
                project,
                req("d", 2, 2, 15, serde_json::json!({"map": "x"})),
                1000,
            ),
            // other project never mixes
            ticket(other, req("e", 2, 2, 15, serde_json::json!({})), 1000),
            // solo with min 1 and zero wait starts immediately
            ticket(
                other,
                req("f", 4, 1, 0, serde_json::json!({"solo": 1})),
                1000,
            ),
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
        ts.push(ticket(
            project,
            req("g", 2, 2, 15, serde_json::json!({})),
            30_000,
        ));
        let (_, rooms) = match_tickets(&mut ts, 31_000);
        assert_eq!(rooms.len(), 1);
        assert_eq!(room_of(&ts[2]), room_of(&ts[6]));

        // Timeout with min < max: 3 of max 4 start after the oldest waited long enough.
        let project = ProjectId::new();
        let mut ts = vec![
            ticket(project, req("x", 4, 2, 5, serde_json::json!({})), 0),
            ticket(project, req("y", 4, 2, 5, serde_json::json!({})), 100),
            ticket(project, req("z", 4, 2, 5, serde_json::json!({})), 200),
        ];
        assert!(match_tickets(&mut ts, 4_000).1.is_empty());
        assert_eq!(match_tickets(&mut ts, 5_000).1.len(), 1);
        assert!(ts.iter().all(|t| room_of(t).is_some()));
    }
}
