//! Lobby statistics and friend presence (lobby statistics / find friends):
//! rooms grouped by the room prop `lobby`, and "is this player online / in which room" across
//! every node of the fleet.
use crate::state::Shared;
use net_common::ProjectId;
use net_rooms::protocol::{FriendInfo, FriendRoom, LobbyStats, RoomSummary};
use std::collections::BTreeMap;

/// Players per `find_friends` request.
pub const MAX_FRIENDS: usize = 64;
/// Room prop naming the lobby a room belongs to; rooms without it are in the default lobby `""`.
pub const LOBBY_PROP: &str = "lobby";

pub fn lobby_name(props: &serde_json::Map<String, serde_json::Value>) -> String {
    props
        .get(LOBBY_PROP)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// (rooms, players, per-lobby counts sorted by lobby name) over the given live rooms.
pub fn lobby_stats(rooms: &[RoomSummary]) -> (u32, u32, Vec<LobbyStats>) {
    let mut by: BTreeMap<String, (u32, u32)> = BTreeMap::new();
    for room in rooms {
        let lobby = by.entry(lobby_name(&room.props)).or_default();
        lobby.0 += 1;
        lobby.1 += u32::from(room.players);
    }
    let players = rooms.iter().map(|r| u32::from(r.players)).sum();
    let lobbies = by
        .into_iter()
        .map(|(lobby, (rooms, players))| LobbyStats {
            lobby,
            rooms,
            players,
        })
        .collect();
    (rooms.len() as u32, players, lobbies)
}

/// What this node knows about `players`: a control session here, or a slot in a room hosted here.
/// The room of an online player is one they are in; the room of an offline one holds their seat.
pub fn local_friends(st: &Shared, project: ProjectId, players: &[String]) -> Vec<FriendInfo> {
    players
        .iter()
        .map(|p| {
            let slots = st.host.find_player(project, p);
            let online = st.host.is_online(project, p) || slots.iter().any(|s| s.5);
            let room =
                slots
                    .into_iter()
                    .find(|slot| slot.5 == online)
                    .map(|(id, name, mode, _, _, _)| FriendRoom {
                        id: id.to_string(),
                        name,
                        mode,
                        node: st.node_id.to_string(),
                    });
            FriendInfo {
                player: p.clone(),
                online,
                room,
            }
        })
        .collect()
}

/// One answer per requested player (duplicates collapsed, request order kept): online if any node
/// says so, and then in the first room a node that has them online reports. A seat held for a
/// player is their room only while no node has them online.
pub fn merge_friends(
    order: &[String],
    parts: impl IntoIterator<Item = FriendInfo>,
) -> Vec<FriendInfo> {
    let mut map: BTreeMap<String, FriendInfo> = order
        .iter()
        .map(|p| {
            (
                p.clone(),
                FriendInfo {
                    player: p.clone(),
                    online: false,
                    room: None,
                },
            )
        })
        .collect();
    for part in parts {
        if let Some(friend) = map.get_mut(&part.player) {
            if part.online && !friend.online {
                friend.room = None;
            }
            if friend.room.is_none() && (part.online || !friend.online) {
                friend.room = part.room;
            }
            friend.online |= part.online;
        }
    }
    order.iter().filter_map(|p| map.remove(p)).collect()
}

/// Fleet-wide presence: this node plus `GET /v1/internal/presence` on every other live node
/// (3 s budget each; an unreachable node is logged and skipped).
pub async fn find_friends(st: &Shared, project: ProjectId, players: &[String]) -> Vec<FriendInfo> {
    let mut parts = local_friends(st, project, players);
    let ids = players
        .iter()
        .map(|p| crate::rest::pct(p))
        .collect::<Vec<_>>()
        .join("%0A");
    let path = format!("/v1/internal/presence?project={}&ids={ids}", project.0);
    let remote: Vec<_> = st
        .registry
        .snapshot()
        .into_iter()
        .filter(|n| n.id != st.node_id)
        .collect();
    let results = futures::future::join_all(
        remote
            .iter()
            .map(|n| crate::rest::internal_get(st, &n.http_url, &path)),
    )
    .await;
    for (peer, reply) in remote.iter().zip(results) {
        match reply {
            Ok(body) => match serde_json::from_value::<Vec<FriendInfo>>(body["players"].clone()) {
                Ok(fs) => parts.extend(fs),
                Err(e) => {
                    tracing::warn!(node = %peer.http_url, error = %e, "find_friends: bad answer")
                }
            },
            Err(e) => {
                tracing::warn!(node = %peer.http_url, error = %e, "find_friends: node unreachable")
            }
        }
    }
    merge_friends(players, parts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use net_common::RoomMode;
    use serde_json::json;

    fn room(name: &str, players: u16, lobby: Option<&str>) -> RoomSummary {
        let mut props = serde_json::Map::new();
        if let Some(name) = lobby {
            props.insert("lobby".into(), json!(name));
        }
        RoomSummary {
            id: name.into(),
            name: name.into(),
            mode: RoomMode::Relay,
            players,
            max_players: 8,
            open: true,
            props,
        }
    }

    #[test]
    fn stats_group_by_lobby_prop() {
        let rooms = [
            room("a", 3, Some("ranked")),
            room("b", 1, None),
            room("c", 5, Some("ranked")),
            room("d", 0, Some("casual")),
        ];
        let (room_count, player_count, lobbies) = lobby_stats(&rooms);
        assert_eq!((room_count, player_count), (4, 9));
        assert_eq!(
            lobbies,
            vec![
                LobbyStats {
                    lobby: "".into(),
                    rooms: 1,
                    players: 1
                },
                LobbyStats {
                    lobby: "casual".into(),
                    rooms: 1,
                    players: 0
                },
                LobbyStats {
                    lobby: "ranked".into(),
                    rooms: 2,
                    players: 8
                },
            ]
        );
    }

    fn friend(player: &str, online: bool, node: Option<&str>) -> FriendInfo {
        FriendInfo {
            player: player.into(),
            online,
            room: node.map(|node| FriendRoom {
                id: "r".into(),
                name: "r".into(),
                mode: RoomMode::Sync,
                node: node.into(),
            }),
        }
    }

    #[test]
    fn merge_keeps_order_and_dedupes() {
        let order: Vec<String> = ["bob", "amy", "bob", "zed"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let parts = vec![
            friend("amy", true, Some("n1")),
            friend("bob", true, None),
            friend("nobody", true, None),
        ];
        let out = merge_friends(&order, parts);
        let names: Vec<&str> = out.iter().map(|f| f.player.as_str()).collect();
        assert_eq!(names, ["bob", "amy", "zed"]);
        assert!(out[0].online && out[0].room.is_none());
        assert!(out[1].online && out[1].room.is_some());
        assert!(!out[2].online && out[2].room.is_none());
    }

    #[test]
    fn merge_names_the_room_a_player_is_in_over_a_seat_held_for_them() {
        let merged = |parts: Vec<FriendInfo>| {
            let out = merge_friends(&["amy".to_string()], parts).remove(0);
            (out.online, out.room.map(|room| room.node))
        };
        let (held, played) = (
            friend("amy", false, Some("held")),
            friend("amy", true, Some("played")),
        );
        let (other, idle) = (
            friend("amy", false, Some("other")),
            friend("amy", true, None),
        );
        let at = |node: &str| Some(node.to_string());
        assert_eq!(
            merged(vec![held.clone(), played.clone()]),
            (true, at("played"))
        );
        assert_eq!(
            merged(vec![played.clone(), held.clone()]),
            (true, at("played"))
        );
        assert_eq!(merged(vec![held.clone(), idle.clone()]), (true, None));
        assert_eq!(merged(vec![idle, held.clone()]), (true, None));
        assert_eq!(merged(vec![held, other]), (false, at("held")));
    }
}
