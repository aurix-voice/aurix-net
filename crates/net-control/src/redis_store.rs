//! Live room directory in Redis: which node holds a room, member counts for lobby listings,
//! and a pub/sub channel per project for lobby updates across nodes.

use net_common::{NetError, ProjectId, Result, RoomId};
use redis::aio::ConnectionManager;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

/// Deletes the key if it still holds the value given.
static RELEASE_LOCK: LazyLock<redis::Script> = LazyLock::new(|| {
    redis::Script::new(
        "if redis.call('GET', KEYS[1]) == ARGV[1] then return redis.call('DEL', KEYS[1]) end
         return 0",
    )
});

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LiveRoom {
    pub room: RoomId,
    pub node: String,
    pub name: String,
    pub mode: String,
    pub players: u16,
    /// Players a seat is held for who have not arrived yet.
    #[serde(default)]
    pub held_for: Vec<String>,
    pub max_players: u16,
    pub open: bool,
    pub visible: bool,
    pub props: serde_json::Value,
}

impl LiveRoom {
    /// Seats `player` cannot have: the taken ones and, unless one is held for him, the held.
    pub fn seats_taken_for(&self, player: &str) -> usize {
        let held = if self.held_for.iter().any(|p| p == player) {
            0
        } else {
            self.held_for.len()
        };
        usize::from(self.players) + held
    }

    /// Whether the room list and a random join offer the room to `player`.
    pub fn is_listed_for(&self, player: &str) -> bool {
        self.visible && self.open && self.seats_taken_for(player) < usize::from(self.max_players)
    }
}

#[derive(Clone)]
pub struct RedisStore {
    conn: ConnectionManager,
    /// How long a room's live record stays without its node renewing it.
    room_ttl_secs: u64,
}

/// Redis client for `url`; `ca_cert` (PEM path) pins a private CA for `rediss://` servers.
pub fn redis_client(url: &str, ca_cert: &str) -> Result<redis::Client> {
    if ca_cert.is_empty() {
        return redis::Client::open(url).map_err(|e| NetError::Redis(e.to_string()));
    }
    let pem = std::fs::read(ca_cert)
        .map_err(|e| NetError::Redis(format!("redis.ca_cert {ca_cert}: {e}")))?;
    redis::Client::build_with_tls(
        url,
        redis::TlsCertificates {
            client_tls: None,
            root_cert: Some(pem),
        },
    )
    .map_err(|e| NetError::Redis(e.to_string()))
}

/// `redis://user:pw@host/` -> `redis://user:***@host/` for logs and errors.
pub fn redact_url(url: &str) -> String {
    let Some(mut url) = redis::parse_redis_url(url) else {
        return "an address that is not a Redis URL".into();
    };
    if url.password().is_some() {
        let _ = url.set_password(Some("***"));
    }
    url.into()
}

impl RedisStore {
    pub async fn connect(url: &str, ca_cert: &str, room_ttl_secs: u64) -> Result<Self> {
        let client = redis_client(url, ca_cert)?;
        // ConnectionManager retries forever; probe once so a bad URL, password or TLS
        // certificate fails the startup fast with the real error instead of hanging.
        let probe = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            client.get_multiplexed_async_connection(),
        )
        .await;
        match probe {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                return Err(NetError::Redis(format!(
                    "cannot connect to {}: {e}",
                    redact_url(url)
                )))
            }
            Err(_) => {
                return Err(NetError::Redis(format!(
                    "cannot connect to {}: timeout",
                    redact_url(url)
                )))
            }
        }
        // Bounded reconnect backoff: a Redis outage costs a tick loop at most a few seconds
        // per write instead of the default unbounded exponential delays.
        let cfg = redis::aio::ConnectionManagerConfig::new()
            .set_exponent_base(2.0)
            .set_min_delay(std::time::Duration::from_millis(100))
            .set_max_delay(std::time::Duration::from_millis(1_000))
            .set_number_of_retries(5)
            .set_connection_timeout(Some(std::time::Duration::from_secs(2)))
            .set_response_timeout(Some(std::time::Duration::from_secs(3)));
        let conn = ConnectionManager::new_with_config(client, cfg)
            .await
            .map_err(|e| NetError::Redis(e.to_string()))?;
        Ok(Self {
            conn,
            room_ttl_secs,
        })
    }

    fn room_key(project: ProjectId, room: RoomId) -> String {
        format!("net:p:{project}:room:{room}")
    }
    fn index_key(project: ProjectId) -> String {
        format!("net:p:{project}:rooms")
    }
    pub fn lobby_channel(project: ProjectId) -> String {
        format!("net:p:{project}:lobby")
    }

    pub fn webhooks_channel(project: ProjectId) -> String {
        format!("net:p:{project}:webhooks")
    }

    /// Tells every node that the project's webhooks changed.
    pub async fn webhooks_changed(&self, project: ProjectId) -> Result<()> {
        let mut conn = self.conn.clone();
        let _: () = conn
            .publish(Self::webhooks_channel(project), "")
            .await
            .map_err(|e| NetError::Redis(e.to_string()))?;
        Ok(())
    }

    /// Writes a room's live record and tells the lobby subscribers (call on every change).
    pub async fn put_room(&self, project: ProjectId, room: &LiveRoom) -> Result<()> {
        let mut conn = self.conn.clone();
        let body = serde_json::to_string(room).map_err(|e| NetError::Internal(e.to_string()))?;
        let _: () = redis::pipe()
            .set_ex(
                Self::room_key(project, room.room),
                &body,
                self.room_ttl_secs,
            )
            .sadd(Self::index_key(project), room.room.to_string())
            .publish(Self::lobby_channel(project), &body)
            .query_async(&mut conn)
            .await
            .map_err(|e| NetError::Redis(e.to_string()))?;
        Ok(())
    }

    /// Renews the live records of rooms that did not change. False for a record that is gone.
    pub async fn touch_rooms(&self, rooms: &[(ProjectId, RoomId)]) -> Result<Vec<bool>> {
        if rooms.is_empty() {
            return Ok(Vec::new());
        }
        let mut conn = self.conn.clone();
        let mut pipe = redis::pipe();
        for (project, room) in rooms {
            pipe.expire(Self::room_key(*project, *room), self.room_ttl_secs as i64);
        }
        pipe.query_async(&mut conn)
            .await
            .map_err(|e| NetError::Redis(e.to_string()))
    }

    fn checkpoint_key(project: ProjectId, room: RoomId) -> String {
        format!("net:{project}:ckpt:{room}")
    }

    /// Stores a sync-room checkpoint (frame snapshot) for re-hosting after a node failure.
    pub async fn put_checkpoint(
        &self,
        project: ProjectId,
        room: RoomId,
        body: &[u8],
        ttl_secs: u64,
    ) -> Result<()> {
        let mut conn = self.conn.clone();
        conn.set_ex::<_, _, ()>(Self::checkpoint_key(project, room), body, ttl_secs)
            .await
            .map_err(|e| NetError::Redis(e.to_string()))
    }

    pub async fn get_checkpoint(
        &self,
        project: ProjectId,
        room: RoomId,
    ) -> Result<Option<Vec<u8>>> {
        let mut conn = self.conn.clone();
        conn.get::<_, Option<Vec<u8>>>(Self::checkpoint_key(project, room))
            .await
            .map_err(|e| NetError::Redis(e.to_string()))
    }

    fn journal_key(project: ProjectId, room: RoomId) -> String {
        format!("net:{project}:journal:{room}")
    }

    fn hashes_key(project: ProjectId, room: RoomId) -> String {
        format!("net:{project}:hashes:{room}")
    }

    /// Appends lockstep input-journal entries (one per tick) and the state hashes of those
    /// ticks, and keeps the newest `keep` of each.
    pub async fn append_journal(
        &self,
        project: ProjectId,
        room: RoomId,
        entries: &[Vec<u8>],
        hashes: &[Vec<u8>],
        keep: isize,
        ttl_secs: i64,
    ) -> Result<()> {
        let mut conn = self.conn.clone();
        let mut pipe = redis::pipe();
        let lists = [
            (Self::journal_key(project, room), entries),
            (Self::hashes_key(project, room), hashes),
        ];
        for (key, added) in &lists {
            if !added.is_empty() {
                pipe.rpush(key, *added).ltrim(key, -keep, -1);
            }
            pipe.expire(key, ttl_secs);
        }
        let _: () = pipe
            .query_async(&mut conn)
            .await
            .map_err(|e| NetError::Redis(e.to_string()))?;
        Ok(())
    }

    /// The state hashes stored with the journal of a room, oldest first.
    pub async fn get_hashes(&self, project: ProjectId, room: RoomId) -> Result<Vec<Vec<u8>>> {
        let mut conn = self.conn.clone();
        conn.lrange::<_, Vec<Vec<u8>>>(Self::hashes_key(project, room), 0, -1)
            .await
            .map_err(|e| NetError::Redis(e.to_string()))
    }

    pub async fn get_journal(&self, project: ProjectId, room: RoomId) -> Result<Vec<Vec<u8>>> {
        let mut conn = self.conn.clone();
        conn.lrange::<_, Vec<Vec<u8>>>(Self::journal_key(project, room), 0, -1)
            .await
            .map_err(|e| NetError::Redis(e.to_string()))
    }

    fn replay_key(project: ProjectId, room: RoomId) -> String {
        format!("net:{project}:replay:{room}")
    }

    /// Stores a lockstep replay (JSON) for a closed room.
    pub async fn put_replay(
        &self,
        project: ProjectId,
        room: RoomId,
        body: &[u8],
        ttl_secs: u64,
    ) -> Result<()> {
        let mut conn = self.conn.clone();
        conn.set_ex::<_, _, ()>(Self::replay_key(project, room), body, ttl_secs)
            .await
            .map_err(|e| NetError::Redis(e.to_string()))
    }

    pub async fn get_replay(&self, project: ProjectId, room: RoomId) -> Result<Option<Vec<u8>>> {
        let mut conn = self.conn.clone();
        conn.get::<_, Option<Vec<u8>>>(Self::replay_key(project, room))
            .await
            .map_err(|e| NetError::Redis(e.to_string()))
    }

    fn desync_index_key(project: ProjectId, room: RoomId) -> String {
        format!("net:{project}:desyncs:{room}")
    }
    fn desync_blob_key(project: ProjectId, room: RoomId, tick: u32, actor: u16) -> String {
        format!("net:{project}:desync:{room}:{tick}:{actor}")
    }

    /// Stores a client's state dump taken after a desync (24 h), listed under the room.
    pub async fn put_desync(
        &self,
        project: ProjectId,
        room: RoomId,
        tick: u32,
        actor: u16,
        hash: u64,
        state: &[u8],
    ) -> Result<()> {
        const TTL: i64 = 86_400;
        let mut conn = self.conn.clone();
        let meta = serde_json::json!({
            "tick": tick, "actor": actor, "hash": format!("{hash:016x}"), "bytes": state.len(),
            "at": net_db::Timestamp::now().to_rfc3339(),
        })
        .to_string();
        let _: () = redis::pipe()
            .set_ex(
                Self::desync_blob_key(project, room, tick, actor),
                state,
                TTL as u64,
            )
            .hset(
                Self::desync_index_key(project, room),
                format!("{tick}:{actor}"),
                meta,
            )
            .expire(Self::desync_index_key(project, room), TTL)
            .query_async(&mut conn)
            .await
            .map_err(|e| NetError::Redis(e.to_string()))?;
        Ok(())
    }

    pub async fn list_desyncs(
        &self,
        project: ProjectId,
        room: RoomId,
    ) -> Result<Vec<serde_json::Value>> {
        let mut conn = self.conn.clone();
        let all: Vec<(String, String)> = conn
            .hgetall(Self::desync_index_key(project, room))
            .await
            .map_err(|e| NetError::Redis(e.to_string()))?;
        let mut out: Vec<serde_json::Value> = all
            .into_iter()
            .filter_map(|(_, v)| serde_json::from_str(&v).ok())
            .collect();
        out.sort_by_key(|v| {
            (
                v["tick"].as_u64().unwrap_or(0),
                v["actor"].as_u64().unwrap_or(0),
            )
        });
        Ok(out)
    }

    pub async fn get_desync(
        &self,
        project: ProjectId,
        room: RoomId,
        tick: u32,
        actor: u16,
    ) -> Result<Option<Vec<u8>>> {
        let mut conn = self.conn.clone();
        conn.get::<_, Option<Vec<u8>>>(Self::desync_blob_key(project, room, tick, actor))
            .await
            .map_err(|e| NetError::Redis(e.to_string()))
    }

    pub async fn remove_room(&self, project: ProjectId, room: RoomId) -> Result<()> {
        let mut conn = self.conn.clone();
        let gone = serde_json::json!({"room": room, "removed": true}).to_string();
        let _: () = redis::pipe()
            .del(Self::room_key(project, room))
            .del(Self::checkpoint_key(project, room))
            .del(Self::journal_key(project, room))
            .del(Self::hashes_key(project, room))
            .srem(Self::index_key(project), room.to_string())
            .publish(Self::lobby_channel(project), gone)
            .query_async(&mut conn)
            .await
            .map_err(|e| NetError::Redis(e.to_string()))?;
        Ok(())
    }

    pub async fn get_room(&self, project: ProjectId, room: RoomId) -> Result<Option<LiveRoom>> {
        let mut conn = self.conn.clone();
        let stored: Option<String> = conn
            .get(Self::room_key(project, room))
            .await
            .map_err(|e| NetError::Redis(e.to_string()))?;
        Ok(stored.and_then(|s| serde_json::from_str(&s).ok()))
    }

    /// All live rooms of a project (expired records are dropped from the index lazily).
    pub async fn list_rooms(&self, project: ProjectId) -> Result<Vec<LiveRoom>> {
        let mut conn = self.conn.clone();
        let ids: Vec<String> = conn
            .smembers(Self::index_key(project))
            .await
            .map_err(|e| NetError::Redis(e.to_string()))?;
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let keys: Vec<String> = ids
            .iter()
            .filter_map(|s| s.parse::<RoomId>().ok())
            .map(|r| Self::room_key(project, r))
            .collect();
        let vals: Vec<Option<String>> = conn
            .mget(&keys)
            .await
            .map_err(|e| NetError::Redis(e.to_string()))?;
        let mut out = Vec::new();
        let mut stale = Vec::new();
        for (id, raw) in ids.iter().zip(vals) {
            match raw.and_then(|s| serde_json::from_str::<LiveRoom>(&s).ok()) {
                Some(room) => out.push(room),
                None => stale.push(id.clone()),
            }
        }
        if !stale.is_empty() {
            let _: () = conn
                .srem(Self::index_key(project), stale)
                .await
                .map_err(|e| NetError::Redis(e.to_string()))?;
        }
        Ok(out)
    }

    /// Sliding-window rate limit: true when allowed.
    pub async fn allow(&self, key: &str, limit: u32, window_secs: u64) -> Result<bool> {
        let mut conn = self.conn.clone();
        let counter = format!("net:rl:{key}");
        let (count,): (i64,) = redis::pipe()
            .atomic()
            .incr(&counter, 1)
            .expire(&counter, window_secs as i64)
            .ignore()
            .query_async(&mut conn)
            .await
            .map_err(|e| NetError::Redis(e.to_string()))?;
        Ok(count as u32 <= limit)
    }

    /// Per-project rejection counters in hourly buckets (`net:rej:{project}:{hour}` hashes,
    /// 26 h TTL). Fire-and-forget from the hot path; one HINCRBY + EXPIRE.
    pub async fn incr_rejection(&self, project: ProjectId, reason: &str) -> Result<()> {
        let mut conn = self.conn.clone();
        let hour = net_db::Timestamp::now().unix_timestamp() / 3600;
        let bucket = format!("net:rej:{project}:{hour}");
        let rejected = Self::rejected_key(hour);
        redis::pipe()
            .atomic()
            .hincr(&bucket, reason, 1)
            .ignore()
            .expire(&bucket, 26 * 3600)
            .ignore()
            .sadd(&rejected, project.to_string())
            .ignore()
            .expire(&rejected, 26 * 3600)
            .ignore()
            .query_async::<()>(&mut conn)
            .await
            .map_err(|e| NetError::Redis(e.to_string()))
    }

    /// Hourly rejection buckets for the last `hours` (oldest first); each entry is
    /// `(hour_start_unix, reason -> count)`. Missing hours are returned as empty maps.
    /// The projects that had a rejection in the hour, kept as long as their counters.
    fn rejected_key(hour: i64) -> String {
        format!("net:rej:projects:{hour}")
    }

    /// Every project with a rejection in the current hour, with its counts by reason.
    pub async fn rejected_this_hour(
        &self,
    ) -> Result<Vec<(ProjectId, std::collections::HashMap<String, i64>)>> {
        let mut conn = self.conn.clone();
        let hour = net_db::Timestamp::now().unix_timestamp() / 3600;
        let projects: Vec<String> = conn
            .smembers(Self::rejected_key(hour))
            .await
            .map_err(|e| NetError::Redis(e.to_string()))?;
        let projects: Vec<ProjectId> = projects.iter().filter_map(|p| p.parse().ok()).collect();
        if projects.is_empty() {
            return Ok(Vec::new());
        }
        let mut read = redis::pipe();
        for project in &projects {
            read.hgetall(format!("net:rej:{project}:{hour}"));
        }
        let counts: Vec<std::collections::HashMap<String, i64>> = read
            .query_async(&mut conn)
            .await
            .map_err(|e| NetError::Redis(e.to_string()))?;
        Ok(projects.into_iter().zip(counts).collect())
    }

    pub async fn get_rejections(
        &self,
        project: ProjectId,
        hours: u32,
    ) -> Result<Vec<(i64, std::collections::HashMap<String, i64>)>> {
        let mut conn = self.conn.clone();
        let now = net_db::Timestamp::now().unix_timestamp() / 3600;
        let hours = hours.clamp(1, 26) as i64;
        let mut out = Vec::with_capacity(hours as usize);
        for hour in (now - hours + 1)..=now {
            let bucket = format!("net:rej:{project}:{hour}");
            let counts: std::collections::HashMap<String, i64> = redis::cmd("HGETALL")
                .arg(&bucket)
                .query_async(&mut conn)
                .await
                .map_err(|e| NetError::Redis(e.to_string()))?;
            out.push((hour * 3600, counts));
        }
        Ok(out)
    }

    /// Gives up a lock that one node holds for the whole fleet, if `holder` still has it.
    pub async fn release_lock(&self, key: &str, holder: &str) -> Result<()> {
        let mut conn = self.conn.clone();
        RELEASE_LOCK
            .key(key)
            .arg(holder)
            .invoke_async::<i64>(&mut conn)
            .await
            .map(drop)
            .map_err(|e| NetError::Redis(e.to_string()))
    }

    pub fn raw(&self) -> ConnectionManager {
        self.conn.clone()
    }
}

#[cfg(test)]
mod redact_tests {
    #[test]
    fn redacts_userinfo() {
        assert_eq!(
            super::redact_url("rediss://:secret@h:6390/"),
            "rediss://:***@h:6390/"
        );
        assert_eq!(
            super::redact_url("redis://app:p%40ss@h/"),
            "redis://app:***@h/"
        );
        assert_eq!(super::redact_url("redis://h:6379/"), "redis://h:6379/");
        assert!(!super::redact_url("h:6379 secret").contains("secret"));
    }
}

#[cfg(test)]
mod live_room_tests {
    use super::LiveRoom;

    #[test]
    fn record_without_held_seats_reads_as_none_held() {
        let record = r#"{"room":"01a1f3bd-3dd9-709b-9189-6479ce3b7b20","node":"n",
            "name":"arena","mode":"relay","players":1,"max_players":2,"open":true,
            "visible":true,"props":{}}"#;
        let room: LiveRoom = serde_json::from_str(record).expect("record");
        assert!(room.held_for.is_empty());
        assert!(room.is_listed_for("anyone"));
    }

    #[test]
    fn record_of_a_newer_node_is_read_past_its_new_fields() {
        let record = r#"{"room":"01a1f3bd-3dd9-709b-9189-6479ce3b7b20","node":"n",
            "name":"arena","mode":"relay","players":1,"max_players":2,"open":true,
            "visible":true,"props":{},"added_later":1}"#;
        assert!(serde_json::from_str::<LiveRoom>(record).is_ok());
    }
}
