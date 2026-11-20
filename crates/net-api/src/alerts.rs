//! Fleet alerts: one node (Redis leader lock) re-evaluates the operator attention list every
//! 30 s and emits each item to the operator webhooks at most once per hour per subject. The
//! period is short enough to see a dead node between its second missed heartbeat (stale) and the
//! moment its peers remove its row (after the third).

use crate::state::Shared;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const LEADER_KEY: &str = "aurix:alerts:leader";
const LEADER_TTL_MS: u64 = 90_000;
const REPEAT_SECS: u64 = 3600;

pub async fn run(st: Shared, stop: CancellationToken) {
    let mut iv = tokio::time::interval(Duration::from_secs(30));
    loop {
        tokio::select! {
            () = stop.cancelled() => break,
            _ = iv.tick() => {}
        }
        if let Err(e) = cycle(&st).await {
            tracing::warn!(error = %e, "alert cycle failed");
        }
    }
    // Left held, the lock would keep the other nodes from the alerts until it ran out.
    let node = st.node_id.to_string();
    if let Err(e) = st.redis.release_lock(LEADER_KEY, &node).await {
        tracing::warn!(error = %e, "alert leader lock not released");
    }
}

async fn is_leader(st: &Shared) -> anyhow::Result<bool> {
    let mut redis = st.redis.raw();
    let node = st.node_id.to_string();
    let got: Option<String> = redis::cmd("SET")
        .arg(LEADER_KEY)
        .arg(&node)
        .arg("NX")
        .arg("PX")
        .arg(LEADER_TTL_MS)
        .query_async(&mut redis)
        .await?;
    if got.is_some() {
        return Ok(true);
    }
    let leader: Option<String> = redis::cmd("GET")
        .arg(LEADER_KEY)
        .query_async(&mut redis)
        .await?;
    if leader.as_deref() == Some(node.as_str()) {
        let _: () = redis::cmd("PEXPIRE")
            .arg(LEADER_KEY)
            .arg(LEADER_TTL_MS)
            .query_async(&mut redis)
            .await?;
        return Ok(true);
    }
    Ok(false)
}

/// True the first time a subject is seen within `REPEAT_SECS`.
async fn first_time(st: &Shared, key: &str) -> anyhow::Result<bool> {
    let mut redis = st.redis.raw();
    let got: Option<String> = redis::cmd("SET")
        .arg(format!("aurix:alert:{key}"))
        .arg("1")
        .arg("NX")
        .arg("EX")
        .arg(REPEAT_SECS)
        .query_async(&mut redis)
        .await?;
    Ok(got.is_some())
}

async fn cycle(st: &Shared) -> anyhow::Result<()> {
    if !is_leader(st).await? {
        return Ok(());
    }
    let att = crate::rest::attention_value(st)
        .await
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let mut items: Vec<(&'static str, String, serde_json::Value)> = Vec::new();
    let arr = |k: &str| att[k].as_array().cloned().unwrap_or_default();
    for node in arr("stale_nodes") {
        items.push(("node.stale", node["id"].to_string(), node));
    }
    for node in arr("draining_nodes") {
        items.push(("node.draining", node["id"].to_string(), node));
    }
    for project in arr("suspended_projects") {
        items.push(("project.suspended", project["id"].to_string(), project));
    }
    for webhook in arr("failing_webhooks") {
        items.push((
            "webhook.failing",
            webhook["webhook_id"].to_string(),
            webhook,
        ));
    }
    for cap in arr("usage_caps") {
        let hard = cap["used"].as_f64().unwrap_or(0.0) >= cap["cap"].as_f64().unwrap_or(f64::MAX);
        let key = format!(
            "{}:{}:{}",
            cap["project_id"],
            cap["metric"],
            if hard { "hard" } else { "soft" }
        );
        let mut cap = cap;
        cap["hard"] = serde_json::Value::Bool(hard);
        items.push(("project.usage_cap", key, cap));
    }
    for rejection in arr("rejections") {
        items.push((
            "project.rejections",
            rejection["project_id"].to_string(),
            rejection,
        ));
    }
    for project in arr("invalid_limits") {
        items.push((
            "project.invalid_limits",
            project["project_id"].to_string(),
            project,
        ));
    }
    for (event, subject, payload) in items {
        if first_time(st, &format!("{event}:{subject}")).await? {
            st.webhooks.emit_fleet(event, payload);
        }
    }
    Ok(())
}
