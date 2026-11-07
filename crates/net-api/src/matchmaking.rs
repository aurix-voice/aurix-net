//! Matchmaking tickets: clients (or the game backend) file a ticket with the room shape they
//! want; a worker groups compatible tickets and assigns them a room name. Clients then
//! `join_or_create` that name with the same mode. Tickets live on the node that received
//! them (the control plane routes a project to one region's nodes); 5 minute TTL.

use net_common::{ProjectId, RoomMode};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use uuid::Uuid;

#[derive(Debug, Clone, Deserialize)]
pub struct TicketReq {
    pub player_id: String,
    #[serde(default)]
    pub mode: RoomMode,
    #[serde(default)]
    pub region: Option<String>,
    /// Room size; a match starts when the group is full or `max_wait_secs` elapsed with at
    /// least `min_players`.
    #[serde(default = "d_max")]
    pub max_players: u16,
    #[serde(default = "d_min")]
    pub min_players: u16,
    #[serde(default = "d_wait")]
    pub max_wait_secs: u64,
    /// Only tickets with identical props are matched together (game mode, map, skill bucket).
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

#[derive(Debug, Clone, Serialize)]
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

#[derive(Debug, Clone)]
pub struct Ticket {
    pub id: Uuid,
    pub project: ProjectId,
    pub req: TicketReq,
    pub created: Instant,
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
}

#[derive(Default)]
pub struct Matchmaker {
    tickets: Mutex<HashMap<Uuid, Ticket>>,
}

impl Matchmaker {
    pub fn file(&self, project: ProjectId, req: TicketReq) -> Ticket {
        let mut t = self.tickets.lock();
        // one ticket per player per project
        t.retain(|_, x| {
            !(x.project == project
                && x.req.player_id == req.player_id
                && matches!(x.state, TicketState::Searching { .. }))
        });
        let ticket = Ticket {
            id: Uuid::now_v7(),
            project,
            req,
            created: Instant::now(),
            state: TicketState::Searching { waiting: 1 },
        };
        t.insert(ticket.id, ticket.clone());
        ticket
    }

    pub fn get(&self, project: ProjectId, id: Uuid) -> Option<Ticket> {
        self.tickets
            .lock()
            .get(&id)
            .filter(|t| t.project == project)
            .cloned()
    }

    pub fn cancel(&self, project: ProjectId, id: Uuid) -> bool {
        let mut t = self.tickets.lock();
        match t.get_mut(&id) {
            Some(x) if x.project == project && matches!(x.state, TicketState::Searching { .. }) => {
                x.state = TicketState::Cancelled;
                true
            }
            _ => false,
        }
    }

    /// One pass: group searching tickets per project+bucket; start full groups, or groups past
    /// the oldest ticket's wait with enough players. Returns matched room names.
    pub fn run_once(&self) -> Vec<String> {
        let mut t = self.tickets.lock();
        let now = Instant::now();
        t.retain(|_, x| now.duration_since(x.created) < Duration::from_secs(300));
        let mut groups: HashMap<(ProjectId, String), Vec<Uuid>> = HashMap::new();
        for x in t.values() {
            if matches!(x.state, TicketState::Searching { .. }) {
                groups
                    .entry((x.project, x.bucket()))
                    .or_default()
                    .push(x.id);
            }
        }
        let mut rooms = Vec::new();
        for (_, mut ids) in groups {
            ids.sort_by_key(|id| t[id].created);
            let n = ids.len();
            for id in &ids {
                if let Some(x) = t.get_mut(id) {
                    x.state = TicketState::Searching { waiting: n };
                }
            }
            let first = &t[&ids[0]].req;
            let (max, min, wait) = (
                first.max_players as usize,
                first.min_players as usize,
                first.max_wait_secs,
            );
            let oldest_age = now.duration_since(t[&ids[0]].created);
            let mut i = 0;
            while i < n {
                let left = n - i;
                let take = if left >= max {
                    max
                } else if left >= min.max(1) && oldest_age >= Duration::from_secs(wait) {
                    left
                } else {
                    break;
                };
                let room = format!("mm-{}", &Uuid::now_v7().simple().to_string()[..12]);
                let players: Vec<String> = ids[i..i + take]
                    .iter()
                    .map(|id| t[id].req.player_id.clone())
                    .collect();
                for id in &ids[i..i + take] {
                    if let Some(x) = t.get_mut(id) {
                        x.state = TicketState::Matched {
                            room: room.clone(),
                            mode: x.req.mode,
                            max_players: x.req.max_players,
                            players: players.clone(),
                        };
                    }
                }
                rooms.push(room);
                i += take;
            }
        }
        rooms
    }

    pub fn searching(&self) -> usize {
        self.tickets
            .lock()
            .values()
            .filter(|t| matches!(t.state, TicketState::Searching { .. }))
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(p: &str, max: u16, min: u16, wait: u64) -> TicketReq {
        TicketReq {
            player_id: p.into(),
            mode: RoomMode::Relay,
            region: None,
            max_players: max,
            min_players: min,
            max_wait_secs: wait,
            props: Default::default(),
        }
    }

    #[test]
    fn fills_groups_and_times_out() {
        let mm = Matchmaker::default();
        let pr = ProjectId::new();
        let a = mm.file(pr, req("a", 2, 2, 10));
        assert!(mm.run_once().is_empty());
        let b = mm.file(pr, req("b", 2, 2, 10));
        let rooms = mm.run_once();
        assert_eq!(rooms.len(), 1);
        let (ta, tb) = (mm.get(pr, a.id).unwrap(), mm.get(pr, b.id).unwrap());
        match (&ta.state, &tb.state) {
            (
                TicketState::Matched {
                    room: r1, players, ..
                },
                TicketState::Matched { room: r2, .. },
            ) => {
                assert_eq!(r1, r2);
                assert_eq!(players.len(), 2);
            }
            _ => panic!(),
        }
        // wait 0 with min 1: a lone ticket starts immediately
        let c = mm.file(pr, req("c", 4, 1, 0));
        assert_eq!(mm.run_once().len(), 1);
        assert!(matches!(
            mm.get(pr, c.id).unwrap().state,
            TicketState::Matched { .. }
        ));
        // different props never mix; cancel works; other project invisible
        let mut r = req("d", 2, 2, 10);
        r.props.insert("map".into(), "x".into());
        let d = mm.file(pr, r);
        mm.file(pr, req("e", 2, 2, 10));
        assert!(mm.run_once().is_empty());
        assert!(mm.cancel(pr, d.id));
        assert!(mm.get(ProjectId::new(), d.id).is_none());
        assert_eq!(mm.searching(), 1);
        // refiling replaces the player's old ticket
        mm.file(pr, req("e", 2, 2, 10));
        assert_eq!(mm.searching(), 1);
    }
}
