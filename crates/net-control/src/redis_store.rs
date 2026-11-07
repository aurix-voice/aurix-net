//! Live room directory in Redis: which node holds a room, member counts for lobby listings,
//! and a pub/sub channel per project for lobby updates across nodes.

use net_common::{NetError, ProjectId, Result, RoomId};
use redis::aio::ConnectionManager;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LiveRoom {
    pub room: RoomId,
    pub node: String,
    pub name: String,
    pub mode: String,
    pub players: u16,
    pub max_players: u16,
    pub open: bool,
    pub visible: bool,
    pub props: serde_json::Value,
}

#[derive(Clone)]
pub struct RedisStore {
    conn: ConnectionManager,
}

const ROOM_TTL_SECS: i64 = 120;

impl RedisStore {
    pub async fn connect(url: &str) -> Result<Self> {
        let client = redis::Client::open(url).map_err(|e| NetError::Redis(e.to_string()))?;
        let conn = ConnectionManager::new(client)
            .await
            .map_err(|e| NetError::Redis(e.to_string()))?;
        Ok(Self { conn })
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

    /// Writes/refreshes a room's live record (call on every change and every 30 s).
    pub async fn put_room(&self, project: ProjectId, r: &LiveRoom) -> Result<()> {
        let mut c = self.conn.clone();
        let body = serde_json::to_string(r).map_err(|e| NetError::Internal(e.to_string()))?;
        let _: () = redis::pipe()
            .set_ex(Self::room_key(project, r.room), &body, ROOM_TTL_SECS as u64)
            .sadd(Self::index_key(project), r.room.to_string())
            .publish(Self::lobby_channel(project), &body)
            .query_async(&mut c)
            .await
            .map_err(|e| NetError::Redis(e.to_string()))?;
        Ok(())
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
        let mut c = self.conn.clone();
        c.set_ex::<_, _, ()>(Self::checkpoint_key(project, room), body, ttl_secs)
            .await
            .map_err(|e| NetError::Redis(e.to_string()))
    }

    pub async fn get_checkpoint(
        &self,
        project: ProjectId,
        room: RoomId,
    ) -> Result<Option<Vec<u8>>> {
        let mut c = self.conn.clone();
        c.get::<_, Option<Vec<u8>>>(Self::checkpoint_key(project, room))
            .await
            .map_err(|e| NetError::Redis(e.to_string()))
    }

    fn journal_key(project: ProjectId, room: RoomId) -> String {
        format!("net:{project}:journal:{room}")
    }

    /// Appends lockstep input-journal entries (one per tick) and keeps the newest `keep`.
    pub async fn append_journal(
        &self,
        project: ProjectId,
        room: RoomId,
        entries: &[Vec<u8>],
        keep: isize,
        ttl_secs: i64,
    ) -> Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        let mut c = self.conn.clone();
        let key = Self::journal_key(project, room);
        let _: () = redis::pipe()
            .rpush(&key, entries)
            .ltrim(&key, -keep, -1)
            .expire(&key, ttl_secs)
            .query_async(&mut c)
            .await
            .map_err(|e| NetError::Redis(e.to_string()))?;
        Ok(())
    }

    pub async fn get_journal(&self, project: ProjectId, room: RoomId) -> Result<Vec<Vec<u8>>> {
        let mut c = self.conn.clone();
        c.lrange::<_, Vec<Vec<u8>>>(Self::journal_key(project, room), 0, -1)
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
        let mut c = self.conn.clone();
        c.set_ex::<_, _, ()>(Self::replay_key(project, room), body, ttl_secs)
            .await
            .map_err(|e| NetError::Redis(e.to_string()))
    }

    pub async fn get_replay(&self, project: ProjectId, room: RoomId) -> Result<Option<Vec<u8>>> {
        let mut c = self.conn.clone();
        c.get::<_, Option<Vec<u8>>>(Self::replay_key(project, room))
            .await
            .map_err(|e| NetError::Redis(e.to_string()))
    }

    pub async fn remove_room(&self, project: ProjectId, room: RoomId) -> Result<()> {
        let mut c = self.conn.clone();
        let gone = serde_json::json!({"room": room, "removed": true}).to_string();
        let _: () = redis::pipe()
            .del(Self::room_key(project, room))
            .del(Self::journal_key(project, room))
            .srem(Self::index_key(project), room.to_string())
            .publish(Self::lobby_channel(project), gone)
            .query_async(&mut c)
            .await
            .map_err(|e| NetError::Redis(e.to_string()))?;
        Ok(())
    }

    pub async fn get_room(&self, project: ProjectId, room: RoomId) -> Result<Option<LiveRoom>> {
        let mut c = self.conn.clone();
        let v: Option<String> = c
            .get(Self::room_key(project, room))
            .await
            .map_err(|e| NetError::Redis(e.to_string()))?;
        Ok(v.and_then(|s| serde_json::from_str(&s).ok()))
    }

    /// All live rooms of a project (expired records are dropped from the index lazily).
    pub async fn list_rooms(&self, project: ProjectId) -> Result<Vec<LiveRoom>> {
        let mut c = self.conn.clone();
        let ids: Vec<String> = c
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
        let vals: Vec<Option<String>> = c
            .mget(&keys)
            .await
            .map_err(|e| NetError::Redis(e.to_string()))?;
        let mut out = Vec::new();
        let mut stale = Vec::new();
        for (id, v) in ids.iter().zip(vals) {
            match v.and_then(|s| serde_json::from_str::<LiveRoom>(&s).ok()) {
                Some(r) => out.push(r),
                None => stale.push(id.clone()),
            }
        }
        if !stale.is_empty() {
            let _: () = c
                .srem(Self::index_key(project), stale)
                .await
                .map_err(|e| NetError::Redis(e.to_string()))?;
        }
        Ok(out)
    }

    /// Sliding-window rate limit: true when allowed.
    pub async fn allow(&self, key: &str, limit: u32, window_secs: u64) -> Result<bool> {
        let mut c = self.conn.clone();
        let k = format!("net:rl:{key}");
        let (n,): (i64,) = redis::pipe()
            .atomic()
            .incr(&k, 1)
            .expire(&k, window_secs as i64)
            .ignore()
            .query_async(&mut c)
            .await
            .map_err(|e| NetError::Redis(e.to_string()))?;
        Ok(n as u32 <= limit)
    }

    pub fn raw(&self) -> ConnectionManager {
        self.conn.clone()
    }
}
