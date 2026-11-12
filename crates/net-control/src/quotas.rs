//! Per-project limits, stored as JSON in `projects.limits`; missing keys fall back to the
//! node-wide `limits` config.

use net_common::config::LimitsConfig;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct ProjectLimits {
    #[serde(default)]
    pub max_rooms: Option<u32>,
    #[serde(default)]
    pub max_players_per_room: Option<u16>,
    #[serde(default)]
    pub max_events_per_sec_per_room: Option<u32>,
    #[serde(default)]
    pub max_events_per_sec_per_player: Option<u32>,
    #[serde(default)]
    pub max_event_bytes: Option<usize>,
    #[serde(default)]
    pub allowed_regions: Option<Vec<String>>,
    /// Calendar-month caps; once reached, new rooms are refused with `usage_cap` until the
    /// month rolls over (`None` = unmetered). Player-minutes is the billable unit.
    #[serde(default)]
    pub monthly_player_minutes: Option<u64>,
    #[serde(default)]
    pub monthly_bytes: Option<u64>,
    /// Fleet-wide cap on players online at once in this project (hosted plans); a join over
    /// the cap is refused with `capacity`. `None` = unlimited.
    #[serde(default)]
    pub max_concurrent_players: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct EffectiveLimits {
    pub max_rooms: u32,
    pub max_players_per_room: u16,
    pub max_events_per_sec_per_room: u32,
    pub max_events_per_sec_per_player: u32,
    pub max_event_bytes: usize,
    pub max_props_bytes: usize,
    pub max_player_props_bytes: usize,
    pub max_cached_events_per_room: usize,
    pub allowed_regions: Option<Vec<String>>,
    pub monthly_player_minutes: Option<u64>,
    pub monthly_bytes: Option<u64>,
    pub max_concurrent_players: Option<u32>,
}

impl ProjectLimits {
    pub fn from_json(v: &serde_json::Value) -> Self {
        serde_json::from_value(v.clone()).unwrap_or_default()
    }

    pub fn effective(&self, node: &LimitsConfig) -> EffectiveLimits {
        EffectiveLimits {
            max_rooms: self.max_rooms.unwrap_or(node.max_rooms_per_node),
            max_players_per_room: self
                .max_players_per_room
                .unwrap_or(node.max_players_per_room)
                .min(node.max_players_per_room),
            max_events_per_sec_per_room: self
                .max_events_per_sec_per_room
                .unwrap_or(node.max_events_per_sec_per_room),
            max_events_per_sec_per_player: self
                .max_events_per_sec_per_player
                .unwrap_or(node.max_events_per_sec_per_player),
            max_event_bytes: self
                .max_event_bytes
                .unwrap_or(node.max_event_bytes)
                .min(node.max_event_bytes),
            max_props_bytes: node.max_props_bytes,
            max_player_props_bytes: node.max_player_props_bytes,
            max_cached_events_per_room: node.max_cached_events_per_room,
            monthly_player_minutes: self.monthly_player_minutes,
            monthly_bytes: self.monthly_bytes,
            max_concurrent_players: self.max_concurrent_players,
            allowed_regions: self.allowed_regions.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_cannot_exceed_node() {
        let node = LimitsConfig {
            max_players_per_room: 128,
            max_rooms_per_node: 2000,
            max_events_per_sec_per_room: 500,
            max_events_per_sec_per_player: 60,
            max_event_bytes: 65536,
            max_props_bytes: 16384,
            max_player_props_bytes: 16384,
            max_cached_events_per_room: 1000,
            max_sessions_per_ip: 64,
            http_rps_per_ip: 50,
            ws_connects_per_min_per_ip: 60,
            auth_failures_per_min_per_ip: 20,
            writes_per_sec_per_key: 20,
            grants_per_sec_per_key: 500,
        };
        let p = ProjectLimits::from_json(
            &serde_json::json!({"max_players_per_room": 1000, "max_rooms": 5}),
        );
        let e = p.effective(&node);
        assert_eq!(e.max_players_per_room, 128);
        assert_eq!(e.max_rooms, 5);
        assert_eq!(e.max_events_per_sec_per_room, 500);
    }
}
