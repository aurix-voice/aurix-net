//! Webhook delivery: events are queued from the hot path and POSTed by one worker per node.
//! Body is JSON; `X-Aurix-Event` names the event, `X-Aurix-Signature: sha256=<hex>` is
//! HMAC-SHA256 of the raw body with the webhook secret. Three attempts with backoff.

use hmac::{Hmac, Mac};
use net_common::ipnet::is_public_ip;
use net_common::NetError;
use net_common::ProjectId;
use net_db::{queries, DbPool};
use sha2::Sha256;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct WebhookEvent {
    pub project: ProjectId,
    pub event: &'static str,
    pub payload: serde_json::Value,
    /// Set for test sends: deliver to this webhook only, ignoring its event filter.
    pub only: Option<Uuid>,
}

#[derive(Clone)]
pub struct Dispatcher {
    tx: mpsc::Sender<WebhookEvent>,
}

impl Dispatcher {
    /// Starts the worker; returns the handle used to enqueue events.
    /// `allow_private`: development only — lets webhooks target loopback/private addresses.
    pub fn start(pool: DbPool, allow_private: bool) -> Self {
        let (tx, rx) = mpsc::channel(4096);
        tokio::spawn(worker(pool, rx, allow_private));
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
            only: None,
        });
    }

    /// Sends a `test` event to one webhook only (ignores its event filter); the outcome lands in
    /// the delivery log like any other emission.
    pub fn test(&self, project: ProjectId, webhook: Uuid) {
        let _ = self.tx.try_send(WebhookEvent {
            project,
            event: "test",
            payload: serde_json::json!({"webhook": webhook}),
            only: Some(webhook),
        });
    }
}

#[derive(Clone)]
struct Hook {
    id: Uuid,
    url: String,
    secret: String,
    events: Vec<String>,
}

struct Cached {
    at: Instant,
    hooks: Vec<Hook>,
}

/// Rejects URLs a tenant must not make the node call: non-http(s), credentials in the URL,
/// loopback/private/link-local hosts (literal or well-known names). Outside development
/// plain `http://` is refused too.
pub fn validate_webhook_url(url: &str, allow_private: bool) -> net_common::Result<()> {
    let u = reqwest::Url::parse(url).map_err(|e| NetError::Invalid(format!("url: {e}")))?;
    match u.scheme() {
        "https" => {}
        "http" if allow_private => {}
        "http" => return Err(NetError::Invalid("url must be https".into())),
        _ => return Err(NetError::Invalid("url must be http(s)".into())),
    }
    if !u.username().is_empty() || u.password().is_some() {
        return Err(NetError::Invalid("url must not carry credentials".into()));
    }
    let Some(host) = u.host_str() else {
        return Err(NetError::Invalid("url has no host".into()));
    };
    if allow_private {
        return Ok(());
    }
    let literal = host.trim_start_matches('[').trim_end_matches(']');
    let blocked = match literal.parse::<std::net::IpAddr>() {
        Ok(ip) => !is_public_ip(ip),
        Err(_) => {
            let d = host.trim_end_matches('.').to_ascii_lowercase();
            d == "localhost"
                || d.ends_with(".localhost")
                || d.ends_with(".local")
                || d.ends_with(".internal")
                || d.ends_with(".arpa")
                || !d.contains('.')
        }
    };
    if blocked {
        return Err(NetError::Invalid("url must point at a public host".into()));
    }
    Ok(())
}

/// Resolves the host right before sending and refuses private answers (DNS rebinding).
async fn resolves_public(url: &str) -> bool {
    let Ok(u) = reqwest::Url::parse(url) else {
        return false;
    };
    let Some(host) = u
        .host_str()
        .map(|h| h.trim_start_matches('[').trim_end_matches(']').to_string())
    else {
        return false;
    };
    let port = u.port_or_known_default().unwrap_or(443);
    let target = format!("{host}:{port}");
    match tokio::net::lookup_host(target).await {
        Ok(addrs) => {
            let v: Vec<_> = addrs.collect();
            !v.is_empty() && v.iter().all(|a| is_public_ip(a.ip()))
        }
        Err(_) => false,
    }
}

async fn worker(pool: DbPool, mut rx: mpsc::Receiver<WebhookEvent>, allow_private: bool) {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("reqwest");
    let mut cache: HashMap<ProjectId, Cached> = HashMap::new();
    while let Some(ev) = rx.recv().await {
        let hooks = match cache.get(&ev.project) {
            Some(c) if c.at.elapsed() < Duration::from_secs(30) && ev.only.is_none() => {
                c.hooks.clone()
            }
            _ => {
                let rows = queries::list_webhooks(&pool, ev.project.0)
                    .await
                    .unwrap_or_default();
                let hooks: Vec<Hook> = rows
                    .into_iter()
                    .map(|r| Hook {
                        id: r.id,
                        url: r.url,
                        secret: r.secret,
                        events: r.events,
                    })
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
        for Hook {
            id,
            url,
            secret,
            events,
        } in hooks
        {
            match ev.only {
                Some(only) if only != id => continue,
                Some(_) => {}
                None if !events.is_empty() && !events.iter().any(|e| e == ev.event || e == "*") => {
                    continue
                }
                None => {}
            }
            let http = http.clone();
            let pool = pool.clone();
            let body = body.clone();
            let sig = sign(&secret, body.as_bytes());
            let event = ev.event;
            let project = ev.project.0;
            tokio::spawn(async move {
                let started = Instant::now();
                let outcome = deliver(&http, &url, event, &sig, &body, allow_private).await;
                let latency_ms = started.elapsed().as_millis().min(i32::MAX as u128) as i32;
                let d = queries::NewDelivery {
                    webhook_id: id,
                    project_id: project,
                    event,
                    ok: outcome.ok,
                    status: outcome.status,
                    attempts: outcome.attempts,
                    latency_ms,
                    error: outcome.error.as_deref(),
                };
                if let Err(e) = queries::insert_webhook_delivery(&pool, &d).await {
                    tracing::warn!(error = %e, "webhook delivery log");
                }
            });
        }
    }
}

struct Outcome {
    ok: bool,
    status: Option<i16>,
    attempts: i16,
    error: Option<String>,
}

/// Up to three attempts with backoff; 4xx stops early (the receiver rejected it on purpose).
async fn deliver(
    http: &reqwest::Client,
    url: &str,
    event: &str,
    sig: &str,
    body: &str,
    allow_private: bool,
) -> Outcome {
    if !allow_private && (validate_webhook_url(url, false).is_err() || !resolves_public(url).await)
    {
        tracing::warn!(%url, "webhook target is not a public host; skipped");
        return Outcome {
            ok: false,
            status: None,
            attempts: 0,
            error: Some("target is not a public host".into()),
        };
    }
    let mut last_err = None;
    let mut status = None;
    for attempt in 0..3u32 {
        let r = http
            .post(url)
            .header("content-type", "application/json")
            .header("x-aurix-event", event)
            .header("x-aurix-signature", sig)
            .body(body.to_string())
            .send()
            .await;
        match r {
            Ok(resp) if resp.status().is_success() => {
                return Outcome {
                    ok: true,
                    status: Some(resp.status().as_u16() as i16),
                    attempts: attempt as i16 + 1,
                    error: None,
                };
            }
            Ok(resp) if resp.status().is_client_error() => {
                tracing::debug!(%url, status = %resp.status(), "webhook rejected");
                return Outcome {
                    ok: false,
                    status: Some(resp.status().as_u16() as i16),
                    attempts: attempt as i16 + 1,
                    error: Some(format!("receiver answered {}", resp.status())),
                };
            }
            Ok(resp) => {
                status = Some(resp.status().as_u16() as i16);
                last_err = Some(format!("receiver answered {}", resp.status()));
            }
            Err(e) => {
                status = None;
                last_err = Some(describe_err(&e));
            }
        }
        if attempt < 2 {
            tokio::time::sleep(Duration::from_millis(500 * (1 << attempt))).await;
        }
    }
    tracing::debug!(%url, event, "webhook gave up");
    Outcome {
        ok: false,
        status,
        attempts: 3,
        error: last_err,
    }
}

/// reqwest's top-level message is just "error sending request"; the useful part (connection
/// refused, timeout, TLS) is in the source chain. URLs are dropped so query strings never land
/// in the log.
fn describe_err(e: &reqwest::Error) -> String {
    let mut parts = Vec::new();
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(e);
    while let Some(err) = cur {
        let msg = err.to_string();
        let msg = msg
            .split(" for url ")
            .next()
            .unwrap_or(&msg)
            .trim()
            .to_string();
        if !msg.is_empty() && !parts.contains(&msg) {
            parts.push(msg);
        }
        cur = err.source();
    }
    let joined = if e.is_timeout() {
        "timeout".to_string()
    } else {
        parts.join(": ")
    };
    joined.chars().take(200).collect()
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

#[cfg(test)]
mod ssrf_tests {
    use super::validate_webhook_url;
    #[test]
    fn ssrf_guard() {
        for bad in [
            "http://example.com/hook",
            "https://127.0.0.1/hook",
            "https://10.1.2.3/x",
            "https://169.254.169.254/latest/meta-data",
            "https://[::1]/x",
            "https://[fd00::1]/x",
            "https://localhost/x",
            "https://db.internal/x",
            "https://router/x",
            "https://user:pw@example.com/x",
            "ftp://example.com/x",
        ] {
            assert!(validate_webhook_url(bad, false).is_err(), "{bad}");
        }
        assert!(validate_webhook_url("https://hooks.example.com/aurix", false).is_ok());
        assert!(validate_webhook_url("http://127.0.0.1:9000/dev", true).is_ok());
    }
}
