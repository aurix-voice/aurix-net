//! Webhook delivery: events are queued from the hot path and POSTed by one worker per node.
//! Body is JSON; `X-Aurix-Event` names the event, `X-Aurix-Signature: sha256=<hex>` is
//! HMAC-SHA256 of the raw body with the webhook secret. Three attempts with backoff.

use hmac::{Hmac, Mac};
use net_common::ProjectId;
use net_db::{queries, DbPool};
use sha2::Sha256;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
pub struct WebhookEvent {
    pub project: ProjectId,
    pub event: &'static str,
    pub payload: serde_json::Value,
}

#[derive(Clone)]
pub struct Dispatcher {
    tx: mpsc::Sender<WebhookEvent>,
}

impl Dispatcher {
    /// Starts the worker; returns the handle used to enqueue events.
    pub fn start(pool: DbPool) -> Self {
        let (tx, rx) = mpsc::channel(4096);
        tokio::spawn(worker(pool, rx));
        Self { tx }
    }

    /// For tests and tools without a database: events are dropped.
    pub fn noop() -> Self {
        let (tx, _rx) = mpsc::channel(1);
        Self { tx }
    }

    pub fn emit(&self, project: ProjectId, event: &'static str, payload: serde_json::Value) {
        let _ = self.tx.try_send(WebhookEvent {
            project,
            event,
            payload,
        });
    }
}

struct Cached {
    at: Instant,
    hooks: Vec<(String, String, Vec<String>)>,
}

async fn worker(pool: DbPool, mut rx: mpsc::Receiver<WebhookEvent>) {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("reqwest");
    let mut cache: HashMap<ProjectId, Cached> = HashMap::new();
    while let Some(ev) = rx.recv().await {
        let hooks = match cache.get(&ev.project) {
            Some(c) if c.at.elapsed() < Duration::from_secs(30) => c.hooks.clone(),
            _ => {
                let rows = queries::list_webhooks(&pool, ev.project.0)
                    .await
                    .unwrap_or_default();
                let hooks: Vec<(String, String, Vec<String>)> = rows
                    .into_iter()
                    .map(|r| (r.url, r.secret, r.events))
                    .collect();
                cache.insert(
                    ev.project,
                    Cached {
                        at: Instant::now(),
                        hooks: hooks.clone(),
                    },
                );
                hooks
            }
        };
        if hooks.is_empty() {
            continue;
        }
        let body = serde_json::json!({
            "event": ev.event,
            "project": ev.project.to_string(),
            "at": chrono::Utc::now().to_rfc3339(),
            "data": ev.payload,
        })
        .to_string();
        for (url, secret, events) in hooks {
            if !events.is_empty() && !events.iter().any(|e| e == ev.event || e == "*") {
                continue;
            }
            let http = http.clone();
            let body = body.clone();
            let sig = sign(&secret, body.as_bytes());
            let event = ev.event;
            tokio::spawn(async move {
                for attempt in 0..3u32 {
                    let r = http
                        .post(&url)
                        .header("content-type", "application/json")
                        .header("x-aurix-event", event)
                        .header("x-aurix-signature", &sig)
                        .body(body.clone())
                        .send()
                        .await;
                    match r {
                        Ok(resp) if resp.status().is_success() => return,
                        Ok(resp) if resp.status().is_client_error() => {
                            tracing::debug!(%url, status = %resp.status(), "webhook rejected");
                            return;
                        }
                        _ => tokio::time::sleep(Duration::from_millis(500 * (1 << attempt))).await,
                    }
                }
                tracing::debug!(%url, event, "webhook gave up");
            });
        }
    }
}

pub fn sign(secret: &str, body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("hmac key");
    mac.update(body);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

#[cfg(test)]
mod tests {
    #[test]
    fn signature_is_stable() {
        let s = super::sign("secret", b"{}");
        assert!(s.starts_with("sha256="));
        assert_eq!(s.len(), 7 + 64);
        assert_eq!(s, super::sign("secret", b"{}"));
        assert_ne!(s, super::sign("other", b"{}"));
    }
}
