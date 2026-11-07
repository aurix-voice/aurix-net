//! Usage metering, the basis of node-hour billing (never CCU): every minute each node adds,
//! per project, `room_minutes`, `player_minutes`, `bytes_in` and `bytes_out` to `usage_hours`
//! (`GET /v1/usage`). Bytes are deltas of the per-session link counters, so a session that
//! leaves between two passes loses at most one minute of traffic accounting.

use crate::state::Shared;
use net_db::queries;
use std::time::Duration;

pub const METER_EVERY: Duration = Duration::from_secs(60);

pub async fn meter(st: Shared) {
    let mut iv = tokio::time::interval(METER_EVERY);
    iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    iv.tick().await;
    loop {
        iv.tick().await;
        let hour = chrono::Utc::now()
            .date_naive()
            .and_hms_opt(
                chrono::Utc::now()
                    .format("%H")
                    .to_string()
                    .parse()
                    .unwrap_or(0),
                0,
                0,
            )
            .map(|t| t.and_utc())
            .unwrap_or_else(chrono::Utc::now);
        for (project, (rooms, players, bytes_in, bytes_out)) in st.host.usage() {
            let minutes = METER_EVERY.as_secs_f64() / 60.0;
            for (metric, qty) in [
                ("room_minutes", rooms as f64 * minutes),
                ("player_minutes", players as f64 * minutes),
                ("bytes_in", bytes_in as f64),
                ("bytes_out", bytes_out as f64),
            ] {
                if qty > 0.0 {
                    if let Err(e) = queries::add_usage(&st.pool, project.0, hour, metric, qty).await
                    {
                        tracing::warn!(error = %e, metric, "usage write failed");
                    }
                }
            }
        }
    }
}
