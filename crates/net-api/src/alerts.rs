//! Fleet alerts: one node (Redis leader lock) re-evaluates the operator attention list every
//! minute and emits each item to the operator webhooks at most once per hour per subject.

use crate::state::Shared;
use std::time::Duration;

const LEADER_KEY: &str = "aurix:alerts:leader";
const LEADER_TTL_MS: u64 = 90_000;
const REPEAT_SECS: u64 = 3600;

pub async fn run(st: Shared) {
    let mut iv = tokio::time::interval(Duration::from_secs(60));
    loop {
        iv.tick().await;
        if let Err(e) = cycle(&st).await {
            tracing::warn!(error = %e, "alert cycle failed");
        }
    }
}

async fn is_leader(st: &Shared) -> anyhow::Result<bool> {
    let mut c = st.redis.raw();
    let node = st.node_id.to_string();
    let got: Option<String> = redis::cmd("SET")
        .arg(LEADER_KEY)
        .arg(&node)
        .arg("NX")
        .arg("PX")
        .arg(LEADER_TTL_MS)
        .query_async(&mut c)
        .await?;
    if got.is_some() {
        return Ok(true);
    }
    let leader: Option<String> = redis::cmd("GET")
        .arg(LEADER_KEY)
        .query_async(&mut c)
        .await?;
    if leader.as_deref() == Some(node.as_str()) {
        let _: () = redis::cmd("PEXPIRE")
            .arg(LEADER_KEY)
            .arg(LEADER_TTL_MS)
            .query_async(&mut c)
            .await?;
        return Ok(true);
    }
    Ok(false)
}

/// True the first time a subject is seen within `REPEAT_SECS`.
async fn first_time(st: &Shared, key: &str) -> anyhow::Result<bool> {
    let mut c = st.redis.raw();
    let got: Option<String> = redis::cmd("SET")
        .arg(format!("aurix:alert:{key}"))
        .arg("1")
        .arg("NX")
        .arg("EX")
        .arg(REPEAT_SECS)
        .query_async(&mut c)
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
    for n in arr("stale_nodes") {
        items.push(("node.stale", n["id"].to_string(), n));
    }
    for n in arr("draining_nodes") {
        items.push(("node.draining", n["id"].to_string(), n));
    }
    for p in arr("suspended_projects") {
        items.push(("project.suspended", p["id"].to_string(), p));
    }
    for w in arr("failing_webhooks") {
        items.push(("webhook.failing", w["webhook_id"].to_string(), w));
    }
    for u in arr("usage_caps") {
        let hard = u["used"].as_f64().unwrap_or(0.0) >= u["cap"].as_f64().unwrap_or(f64::MAX);
        let key = format!(
            "{}:{}:{}",
            u["project_id"],
            u["metric"],
            if hard { "hard" } else { "soft" }
        );
        let mut u = u;
        u["hard"] = serde_json::Value::Bool(hard);
        items.push(("project.usage_cap", key, u));
    }
    for r in arr("rejections") {
        items.push(("project.rejections", r["project_id"].to_string(), r));
    }
    for (event, subject, payload) in items {
        if first_time(st, &format!("{event}:{subject}")).await? {
            st.webhooks.emit_fleet(event, payload);
        }
    }
    Ok(())
}
