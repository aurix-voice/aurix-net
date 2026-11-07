//! Hourly Postgres janitor: deletes closed rooms, audit and usage rows past `[retention]`.
//! Every node runs it; the deletes are idempotent, so overlap between nodes is harmless.

use crate::state::Shared;
use net_db::queries;
use std::time::Duration;

const EVERY: Duration = Duration::from_secs(3600);

pub async fn run(st: Shared) {
    let mut iv = tokio::time::interval(EVERY);
    iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        iv.tick().await;
        let jitter = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| u64::from(d.subsec_nanos()) % 5_000)
            .unwrap_or(0);
        tokio::time::sleep(Duration::from_millis(jitter)).await;
        let r = &st.cfg.retention;
        for (what, res) in [
            (
                "rooms",
                queries::purge_closed_rooms(&st.pool, r.closed_rooms_days).await,
            ),
            ("audit", queries::purge_audit(&st.pool, r.audit_days).await),
            ("usage", queries::purge_usage(&st.pool, r.usage_days).await),
            ("bans", queries::purge_expired_bans(&st.pool).await),
            (
                "webhook_deliveries",
                queries::purge_webhook_deliveries(&st.pool, r.webhook_deliveries_days).await,
            ),
            (
                "operator_webhook_deliveries",
                queries::purge_operator_webhook_deliveries(&st.pool, r.webhook_deliveries_days)
                    .await,
            ),
        ] {
            match res {
                Ok(0) => {}
                Ok(n) => tracing::info!(table = what, deleted = n, "janitor"),
                Err(e) => tracing::warn!(table = what, error = %e, "janitor"),
            }
        }
    }
}
