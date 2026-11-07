//! Control-channel messages (JSON over WebSocket). Field names are short on purpose.

use crate::event::{CacheOp, EventTarget};
use net_common::{ActorId, Channel, RoomMode};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ClientMsg {
    /// First message: the join grant issued by the developer's backend.
    Auth {
        grant: String,
    },
    Create {
        name: Option<String>,
        #[serde(default)]
        mode: RoomMode,
        #[serde(default)]
        max_players: Option<u16>,
        #[serde(default = "default_true")]
        visible: bool,
        #[serde(default = "default_true")]
        open: bool,
        #[serde(default)]
        props: serde_json::Map<String, serde_json::Value>,
        /// Property keys visible in lobby listings.
        #[serde(default)]
        lobby_props: Vec<String>,
        #[serde(default)]
        region: Option<String>,
        #[serde(default)]
        player_props: serde_json::Map<String, serde_json::Value>,
    },
    Join {
        name: String,
        #[serde(default)]
        player_props: serde_json::Map<String, serde_json::Value>,
        /// Reconnect into the same actor slot.
        #[serde(default)]
        rejoin_actor: Option<ActorId>,
        /// Sync rooms: the client's compiled schema hash; mismatch is refused.
        #[serde(default)]
        schema_hash: Option<String>,
        /// Watch only: not counted against max_players, never master, cannot send events or
        /// inputs; sync sends the whole world, lockstep delays the input stream.
        #[serde(default)]
        spectator: bool,
    },
    /// Join by name, create with these settings if absent.
    JoinOrCreate {
        name: String,
        #[serde(default)]
        mode: RoomMode,
        #[serde(default)]
        max_players: Option<u16>,
        #[serde(default)]
        props: serde_json::Map<String, serde_json::Value>,
        #[serde(default)]
        lobby_props: Vec<String>,
        #[serde(default)]
        region: Option<String>,
        #[serde(default)]
        player_props: serde_json::Map<String, serde_json::Value>,
        /// Player ids whose slots are held for 60 s when this call creates the room (party join).
        #[serde(default)]
        reserved: Vec<String>,
    },
    /// Any open, visible room with a free slot, optionally filtered by lobby props.
    JoinRandom {
        #[serde(default)]
        filter: serde_json::Map<String, serde_json::Value>,
        #[serde(default)]
        player_props: serde_json::Map<String, serde_json::Value>,
        /// SQL-like condition on the room's lobby props (`map = 'dust' AND rank_min <= 1400`).
        #[serde(default)]
        expr: Option<String>,
        /// `fill` (default: most players first), `serial` (fewest players first), `random`.
        #[serde(default)]
        match_type: Option<String>,
    },
    Leave,
    SetRoomProps {
        props: serde_json::Map<String, serde_json::Value>,
        /// Only apply if the current values match (compare-and-swap).
        #[serde(default)]
        expected: Option<serde_json::Map<String, serde_json::Value>>,
        #[serde(default)]
        visible: Option<bool>,
        #[serde(default)]
        open: Option<bool>,
    },
    SetPlayerProps {
        props: serde_json::Map<String, serde_json::Value>,
        #[serde(default)]
        expected: Option<serde_json::Map<String, serde_json::Value>>,
    },
    /// Event via the control channel (also available as a binary frame on the data path).
    Event {
        code: u8,
        /// base64 payload
        data: String,
        #[serde(default = "default_target")]
        target: EventTarget,
        #[serde(default = "default_channel")]
        channel: Channel,
        #[serde(default)]
        cache: CacheOp,
        #[serde(default)]
        group: Option<u16>,
    },
    SetGroups {
        #[serde(default)]
        join: Vec<u16>,
        #[serde(default)]
        leave: Vec<u16>,
    },
    SetMaster {
        actor: ActorId,
    },
    Kick {
        actor: ActorId,
        #[serde(default)]
        reason: String,
    },
    Lobby {
        subscribe: bool,
    },
    Ping {
        #[serde(default)]
        ts: u64,
    },
}

fn default_true() -> bool {
    true
}
fn default_target() -> EventTarget {
    EventTarget::Others
}
fn default_channel() -> Channel {
    Channel::ReliableOrdered
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlayerInfo {
    pub actor: ActorId,
    pub player_id: String,
    pub name: String,
    pub props: serde_json::Map<String, serde_json::Value>,
    pub online: bool,
    #[serde(default)]
    pub spectator: bool,
}

/// Where to open the data path.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DataEndpoint {
    pub host: String,
    pub port: u16,
    pub cert_sha256: String,
    /// base64 data ticket
    pub ticket: String,
    pub ws_tunnel: bool,
    /// `https://host:port/` of the node's WebTransport endpoint (browsers), if enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webtransport_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoomSummary {
    pub id: String,
    pub name: String,
    pub mode: RoomMode,
    pub players: u16,
    pub max_players: u16,
    pub open: bool,
    pub props: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "t", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
pub enum ServerMsg {
    Hello {
        session: String,
        version: String,
        region: String,
    },
    Authed {
        player_id: String,
        name: String,
    },
    Joined {
        room: String,
        name: String,
        mode: RoomMode,
        actor: ActorId,
        master: ActorId,
        max_players: u16,
        props: serde_json::Map<String, serde_json::Value>,
        players: Vec<PlayerInfo>,
        data: DataEndpoint,
        /// Cached events replayed in order (code, from, base64 data).
        cached: Vec<(u8, ActorId, String)>,
        /// Sync rooms only.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        schema_hash: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tick_hz: Option<u16>,
        /// Lockstep rooms only: the server tick the joiner enters at, and the room seed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tick: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seed: Option<u64>,
    },
    Left {
        reason: String,
    },
    PlayerJoined {
        player: PlayerInfo,
    },
    PlayerLeft {
        actor: ActorId,
        #[serde(default)]
        offline: bool,
    },
    RoomProps {
        props: serde_json::Map<String, serde_json::Value>,
        by: Option<ActorId>,
        visible: bool,
        open: bool,
    },
    PlayerProps {
        actor: ActorId,
        props: serde_json::Map<String, serde_json::Value>,
    },
    Event {
        code: u8,
        from: ActorId,
        data: String,
    },
    MasterChanged {
        actor: ActorId,
    },
    /// System message from the operator or the game backend (maintenance, warnings).
    /// `level` is `info` or `warn`. Clients that predate it may ignore the tag.
    Notice {
        text: String,
        level: String,
    },
    LobbyRooms {
        rooms: Vec<RoomSummary>,
    },
    LobbyUpdate {
        room: RoomSummary,
        removed: bool,
    },
    Pong {
        ts: u64,
        server_ms: u64,
    },
    Error {
        code: String,
        message: String,
    },
    /// Lockstep: this actor's state hash for `tick` disagreed with the majority. The actor is
    /// sent a fresh checkpoint + journal tail to reload from; after `UNTRUSTED_AFTER` strikes
    /// its results should not be trusted.
    Desync {
        tick: u32,
        actor: ActorId,
        strikes: u8,
        untrusted: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_json_shapes() {
        let m: ClientMsg = serde_json::from_str(r#"{"t":"join","name":"lobby"}"#).unwrap();
        assert_eq!(
            m,
            ClientMsg::Join {
                spectator: false,
                name: "lobby".into(),
                player_props: Default::default(),
                rejoin_actor: None,
                schema_hash: None,
            }
        );
        let m: ClientMsg = serde_json::from_str(
            r#"{"t":"event","code":7,"data":"AQI=","target":{"kind":"actors","actors":[1,2]}}"#,
        )
        .unwrap();
        match m {
            ClientMsg::Event {
                target, channel, ..
            } => {
                assert_eq!(target, EventTarget::Actors(vec![ActorId(1), ActorId(2)]));
                assert_eq!(channel, Channel::ReliableOrdered);
            }
            _ => panic!(),
        }
    }
}
