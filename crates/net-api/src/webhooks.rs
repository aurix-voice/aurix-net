//! Webhook delivery: events are queued from the hot path and POSTed by one worker per node.
//! Body is JSON; `X-Aurix-Event` names the event, `X-Aurix-Signature: sha256=<hex>` is
//! HMAC-SHA256 of the raw body with the webhook secret. Three attempts with backoff.
//! After a secret rotation `X-Aurix-Signature-Previous` carries the old secret's signature
//! until the grace period ends, so receivers can switch without dropping events.

use hmac::{Hmac, KeyInit, Mac};
use net_common::ipnet::is_public_ip;
use net_common::NetError;
use net_common::ProjectId;
use net_db::{queries, DbPool};
use sha2::Sha256;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use uuid::Uuid;

/// Every event a webhook can subscribe to (`events: []` means all). Kept in sync with docs/webhooks.md.
pub const EVENTS: &[&str] = &[
    "join.validate",
    "room.created",
    "room.closed",
    "room.rehosted",
    "player.joined",
    "player.left",
    "match.found",
    "rpc",
    "test",
];

/// Fleet-level alerts delivered to operator webhooks (`/v1/operator/webhooks`). Each fires at
/// most once per hour per subject while the condition persists.
pub const OPERATOR_EVENTS: &[&str] = &[
    "node.stale",
    "node.draining",
    "project.suspended",
    "project.usage_cap",
    "project.rejections",
    "webhook.failing",
    "test",
];

pub fn validate_operator_events(events: &[String]) -> Result<(), NetError> {
    for e in events {
        if !OPERATOR_EVENTS.contains(&e.as_str()) {
            return Err(NetError::Invalid(format!(
                "events: unknown event {e:?}; known: {}",
                OPERATOR_EVENTS.join(", ")
            )));
        }
    }
    Ok(())
}

/// Registration-time check so a typo (`room.create`) does not silently subscribe to nothing.
pub fn validate_events(events: &[String]) -> Result<(), NetError> {
    if events.len() > EVENTS.len() {
        return Err(NetError::Invalid("events: too many entries".into()));
    }
    for e in events {
        if !EVENTS.contains(&e.as_str()) {
            return Err(NetError::Invalid(format!(
                "events: unknown event {e:?}; known: {}",
                EVENTS.join(", ")
            )));
        }
    }
    Ok(())
}

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
    /// Projects whose hook list changed (e.g. secret rotation): the worker drops its cache for them.
    dirty: Arc<std::sync::Mutex<HashSet<ProjectId>>>,
    /// Hooks subscribed to `join.validate`, per project (30 s cache, dropped on `invalidate`).
    validators: Arc<std::sync::Mutex<HashMap<ProjectId, Cached>>>,
    /// Hooks subscribed to `rpc`, per project (same caching).
    rpc_hooks: Arc<std::sync::Mutex<HashMap<ProjectId, Cached>>>,
    pool: Option<DbPool>,
    http: reqwest::Client,
    allow_private: bool,
}

/// Answer of a synchronous `join.validate` webhook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinVerdict {
    /// No validator configured, or the receiver allowed the join.
    Allow,
    /// The receiver answered `{"allow": false}`; `reason` goes back to the client.
    Deny(String),
    /// The receiver was unreachable or answered badly: the join is refused (fail closed).
    Unavailable(String),
}

const VALIDATE_TIMEOUT: Duration = Duration::from_secs(2);
/// `rpc` calls: the endpoint has 5 s and may answer at most 16 KiB of JSON.
pub const RPC_TIMEOUT: Duration = Duration::from_secs(5);
pub const RPC_MAX_RESPONSE: usize = 16 * 1024;

/// Answer of a client-initiated `rpc` call.
#[derive(Debug, Clone, PartialEq)]
pub struct RpcOutcome {
    pub ok: bool,
    pub status: Option<u16>,
    pub body: serde_json::Value,
    pub error: Option<String>,
}
impl RpcOutcome {
    fn failed(status: Option<u16>, error: impl Into<String>) -> Self {
        Self {
            ok: false,
            status,
            body: serde_json::Value::Null,
            error: Some(error.into()),
        }
    }
}

impl Dispatcher {
    /// Starts the worker; returns the handle used to enqueue events.
    /// `allow_private`: development only — lets webhooks target loopback/private addresses.
    pub fn start(pool: DbPool, allow_private: bool) -> Self {
        let (tx, rx) = mpsc::channel(4096);
        let dirty = Arc::new(std::sync::Mutex::new(HashSet::new()));
        tokio::spawn(worker(pool.clone(), rx, allow_private, dirty.clone()));
        Self {
            tx,
            dirty,
            validators: Arc::new(std::sync::Mutex::new(HashMap::new())),
            rpc_hooks: Arc::new(std::sync::Mutex::new(HashMap::new())),
            pool: Some(pool),
            http: validate_client(),
            allow_private,
        }
    }

    /// For tests and tools without a database: events are dropped.
    pub fn noop() -> Self {
        let (tx, _rx) = mpsc::channel(1);
        Self {
            tx,
            dirty: Arc::new(std::sync::Mutex::new(HashSet::new())),
            validators: Arc::new(std::sync::Mutex::new(HashMap::new())),
            rpc_hooks: Arc::new(std::sync::Mutex::new(HashMap::new())),
            pool: None,
            http: validate_client(),
            allow_private: true,
        }
    }

    /// Call after a project's webhooks change so the next event reloads them from the database.
    pub fn invalidate(&self, project: ProjectId) {
        if let Ok(mut d) = self.dirty.lock() {
            d.insert(project);
        }
        if let Ok(mut v) = self.validators.lock() {
            v.remove(&project);
        }
        if let Ok(mut v) = self.rpc_hooks.lock() {
            v.remove(&project);
        }
    }

    /// Synchronous pre-join check (fine-grained validation): every webhook of the
    /// project subscribed to `join.validate` is POSTed `payload` and must answer 2xx with
    /// `{"allow": true}` (or an empty body) within 2 s; `{"allow": false, "reason": "..."}` refuses
    /// the join with that reason. Receiver down or non-2xx = refused (fail closed), so only
    /// subscribe a validator you can keep up. Each call lands in the delivery log.
    pub async fn validate_join(
        &self,
        project: ProjectId,
        payload: serde_json::Value,
    ) -> JoinVerdict {
        let Some(pool) = &self.pool else {
            return JoinVerdict::Allow;
        };
        let hooks = match self.validator_hooks(pool, project).await {
            Ok(h) => h,
            Err(e) => return JoinVerdict::Unavailable(format!("validator lookup failed: {e}")),
        };
        if hooks.is_empty() {
            return JoinVerdict::Allow;
        }
        let body = serde_json::json!({
            "event": "join.validate",
            "project": project.to_string(),
            "at": chrono::Utc::now().to_rfc3339(),
            "data": payload,
        })
        .to_string();
        for h in hooks {
            let started = Instant::now();
            let (verdict, outcome) = self.ask(&h, &body).await;
            let latency_ms = started.elapsed().as_millis().min(i32::MAX as u128) as i32;
            let d = queries::NewDelivery {
                webhook_id: h.id,
                project_id: project.0,
                event: "join.validate",
                ok: outcome.ok,
                status: outcome.status,
                attempts: outcome.attempts,
                latency_ms,
                error: outcome.error.as_deref(),
            };
            if let Err(e) = queries::insert_webhook_delivery(pool, &d).await {
                tracing::warn!(error = %e, "webhook delivery log");
            }
            if verdict != JoinVerdict::Allow {
                return verdict;
            }
        }
        JoinVerdict::Allow
    }

    async fn validator_hooks(
        &self,
        pool: &DbPool,
        project: ProjectId,
    ) -> net_common::Result<Vec<Hook>> {
        self.hooks_for(pool, project, "join.validate", &self.validators)
            .await
    }
    /// Enabled hooks of `project` subscribed to `event`, cached 30 s (dropped on `invalidate`).
    async fn hooks_for(
        &self,
        pool: &DbPool,
        project: ProjectId,
        event: &str,
        cache: &std::sync::Mutex<HashMap<ProjectId, Cached>>,
    ) -> net_common::Result<Vec<Hook>> {
        if let Ok(v) = cache.lock() {
            if let Some(c) = v.get(&project) {
                if c.at.elapsed() < Duration::from_secs(30) {
                    return Ok(c.hooks.clone());
                }
            }
        }
        let hooks: Vec<Hook> = queries::list_webhooks(pool, project.0)
            .await?
            .into_iter()
            .filter(|r| r.enabled && r.events.iter().any(|e| e == event))
            .map(|r| Hook {
                id: r.id,
                url: r.url,
                secret: r.secret,
                previous: r.previous_secret.filter(|_| {
                    r.previous_secret_expires_at
                        .is_some_and(|t| t > chrono::Utc::now())
                }),
                events: r.events,
            })
            .collect();
        if let Ok(mut v) = cache.lock() {
            v.insert(
                project,
                Cached {
                    at: Instant::now(),
                    hooks: hooks.clone(),
                },
            );
        }
        Ok(hooks)
    }
    /// Client-initiated call into the game's backend (WebRPC): the first webhook of the
    /// project subscribed to `rpc` is POSTed `payload`; its JSON answer (≤ 16 KiB, within 5 s)
    /// goes back to the caller. One attempt — RPCs are not assumed idempotent. Lands in the
    /// delivery log like every other webhook call.
    pub async fn rpc(&self, project: ProjectId, payload: serde_json::Value) -> RpcOutcome {
        let Some(pool) = &self.pool else {
            return RpcOutcome::failed(None, "rpc: no database");
        };
        let hooks = match self.hooks_for(pool, project, "rpc", &self.rpc_hooks).await {
            Ok(h) => h,
            Err(e) => return RpcOutcome::failed(None, format!("rpc endpoint lookup failed: {e}")),
        };
        let Some(h) = hooks.first() else {
            return RpcOutcome::failed(None, "no webhook subscribed to `rpc`");
        };
        let body = serde_json::json!({
            "event": "rpc",
            "project": project.to_string(),
            "at": chrono::Utc::now().to_rfc3339(),
            "data": payload,
        })
        .to_string();
        let started = Instant::now();
        let out = self.call_rpc(h, &body).await;
        let d = queries::NewDelivery {
            webhook_id: h.id,
            project_id: project.0,
            event: "rpc",
            ok: out.ok,
            status: out.status.map(|s| s as i16),
            attempts: 1,
            latency_ms: started.elapsed().as_millis().min(i32::MAX as u128) as i32,
            error: out.error.as_deref(),
        };
        if let Err(e) = queries::insert_webhook_delivery(pool, &d).await {
            tracing::warn!(error = %e, "webhook delivery log");
        }
        out
    }
    async fn call_rpc(&self, h: &Hook, body: &str) -> RpcOutcome {
        if !self.allow_private
            && (validate_webhook_url(&h.url, false).is_err() || !resolves_public(&h.url).await)
        {
            return RpcOutcome::failed(None, "target is not a public host");
        }
        let mut req = self
            .http
            .post(&h.url)
            .timeout(RPC_TIMEOUT)
            .header("content-type", "application/json")
            .header("x-aurix-event", "rpc")
            .header("x-aurix-signature", sign(&h.secret, body.as_bytes()));
        if let Some(p) = &h.previous {
            req = req.header("x-aurix-signature-previous", sign(p, body.as_bytes()));
        }
        let mut resp = match req.body(body.to_string()).send().await {
            Ok(r) => r,
            Err(e) => return RpcOutcome::failed(None, format!("rpc endpoint: {e}")),
        };
        let status = resp.status().as_u16();
        let mut buf = Vec::new();
        loop {
            match resp.chunk().await {
                Ok(Some(c)) => {
                    buf.extend_from_slice(&c);
                    if buf.len() > RPC_MAX_RESPONSE {
                        return RpcOutcome::failed(Some(status), "rpc answer too large");
                    }
                }
                Ok(None) => break,
                Err(e) => return RpcOutcome::failed(Some(status), format!("rpc endpoint: {e}")),
            }
        }
        let body = if buf.iter().all(u8::is_ascii_whitespace) {
            serde_json::Value::Null
        } else {
            match serde_json::from_slice(&buf) {
                Ok(v) => v,
                Err(_) => {
                    return RpcOutcome::failed(Some(status), "rpc endpoint answered non-JSON")
                }
            }
        };
        RpcOutcome {
            ok: (200..300).contains(&status),
            status: Some(status),
            body,
            error: None,
        }
    }
    async fn ask(&self, h: &Hook, body: &str) -> (JoinVerdict, Outcome) {
        if !self.allow_private
            && (validate_webhook_url(&h.url, false).is_err() || !resolves_public(&h.url).await)
        {
            let msg = "target is not a public host".to_string();
            return (
                JoinVerdict::Unavailable(msg.clone()),
                Outcome {
                    ok: false,
                    status: None,
                    attempts: 0,
                    error: Some(msg),
                },
            );
        }
        let mut req = self
            .http
            .post(&h.url)
            .header("content-type", "application/json")
            .header("x-aurix-event", "join.validate")
            .header("x-aurix-signature", sign(&h.secret, body.as_bytes()));
        if let Some(p) = &h.previous {
            req = req.header("x-aurix-signature-previous", sign(p, body.as_bytes()));
        }
        match req.body(body.to_string()).send().await {
            Ok(resp) => {
                let status = resp.status();
                let code = Some(status.as_u16() as i16);
                if !status.is_success() {
                    let msg = format!("validator answered {status}");
                    return (
                        JoinVerdict::Unavailable(msg.clone()),
                        Outcome {
                            ok: false,
                            status: code,
                            attempts: 1,
                            error: Some(msg),
                        },
                    );
                }
                let text = resp.text().await.unwrap_or_default();
                let v: serde_json::Value = if text.trim().is_empty() {
                    serde_json::json!({"allow": true})
                } else {
                    match serde_json::from_str(&text) {
                        Ok(v) => v,
                        Err(_) => {
                            let msg = "validator answered non-JSON".to_string();
                            return (
                                JoinVerdict::Unavailable(msg.clone()),
                                Outcome {
                                    ok: false,
                                    status: code,
                                    attempts: 1,
                                    error: Some(msg),
                                },
                            );
                        }
                    }
                };
                let allow = v.get("allow").and_then(|a| a.as_bool()).unwrap_or(true);
                let ok = Outcome {
                    ok: true,
                    status: code,
                    attempts: 1,
                    error: None,
                };
                if allow {
                    (JoinVerdict::Allow, ok)
                } else {
                    let reason = v
                        .get("reason")
                        .and_then(|r| r.as_str())
                        .unwrap_or("rejected by validator")
                        .chars()
                        .take(200)
                        .collect::<String>();
                    (JoinVerdict::Deny(reason), ok)
                }
            }
            Err(e) => {
                let msg = describe_err(&e);
                (
                    JoinVerdict::Unavailable(msg.clone()),
                    Outcome {
                        ok: false,
                        status: None,
                        attempts: 1,
                        error: Some(msg),
                    },
                )
            }
        }
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
    /// Fleet-level alert: delivered to operator webhooks, `project` is null in the body.
    pub fn emit_fleet(&self, event: &'static str, payload: serde_json::Value) {
        self.emit(ProjectId::from_uuid(Uuid::nil()), event, payload);
    }

    pub fn test_fleet(&self, webhook: Uuid) {
        self.test(ProjectId::from_uuid(Uuid::nil()), webhook);
    }

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
    /// Previous secret while its grace period lasts: deliveries carry a second signature.
    previous: Option<String>,
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

async fn worker(
    pool: DbPool,
    mut rx: mpsc::Receiver<WebhookEvent>,
    allow_private: bool,
    dirty: Arc<std::sync::Mutex<HashSet<ProjectId>>>,
) {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("reqwest");
    let mut cache: HashMap<ProjectId, Cached> = HashMap::new();
    while let Some(ev) = rx.recv().await {
        if dirty
            .lock()
            .map(|mut d| d.remove(&ev.project))
            .unwrap_or(false)
        {
            cache.remove(&ev.project);
        }
        let hooks = match cache.get(&ev.project) {
            Some(c) if c.at.elapsed() < Duration::from_secs(30) && ev.only.is_none() => {
                c.hooks.clone()
            }
            _ => {
                let hooks: Vec<Hook> = if ev.project.0.is_nil() {
                    queries::list_operator_webhooks(&pool)
                        .await
                        .unwrap_or_default()
                        .into_iter()
                        .map(|r| Hook {
                            id: r.id,
                            url: r.url,
                            secret: r.secret,
                            previous: None,
                            events: r.events,
                        })
                        .collect()
                } else {
                    queries::list_webhooks(&pool, ev.project.0)
                        .await
                        .unwrap_or_default()
                        .into_iter()
                        .map(|r| Hook {
                            id: r.id,
                            url: r.url,
                            secret: r.secret,
                            previous: r.previous_secret.filter(|_| {
                                r.previous_secret_expires_at
                                    .is_some_and(|t| t > chrono::Utc::now())
                            }),
                            events: r.events,
                        })
                        .collect()
                };
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
        let project_field = if ev.project.0.is_nil() {
            serde_json::Value::Null
        } else {
            serde_json::Value::String(ev.project.to_string())
        };
        let body = serde_json::json!({
            "event": ev.event,
            "project": project_field,
            "at": chrono::Utc::now().to_rfc3339(),
            "data": ev.payload,
        })
        .to_string();
        for Hook {
            id,
            url,
            secret,
            previous,
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
            let sig_prev = previous.map(|p| sign(&p, body.as_bytes()));
            let event = ev.event;
            let project = ev.project.0;
            tokio::spawn(async move {
                let started = Instant::now();
                let outcome = deliver(
                    &http,
                    &url,
                    event,
                    &sig,
                    sig_prev.as_deref(),
                    &body,
                    allow_private,
                )
                .await;
                let latency_ms = started.elapsed().as_millis().min(i32::MAX as u128) as i32;
                if project.is_nil() {
                    let d = queries::NewOperatorDelivery {
                        webhook_id: id,
                        event,
                        ok: outcome.ok,
                        status: outcome.status,
                        attempts: outcome.attempts,
                        latency_ms,
                        error: outcome.error.as_deref(),
                    };
                    if let Err(e) = queries::insert_operator_webhook_delivery(&pool, &d).await {
                        tracing::warn!(error = %e, "operator webhook delivery log");
                    }
                    return;
                }
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

fn validate_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(VALIDATE_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("reqwest")
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
    sig_prev: Option<&str>,
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
        let mut req = http
            .post(url)
            .header("content-type", "application/json")
            .header("x-aurix-event", event)
            .header("x-aurix-signature", sig);
        if let Some(p) = sig_prev {
            req = req.header("x-aurix-signature-previous", p);
        }
        let r = req.body(body.to_string()).send().await;
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
    use super::*;

    /// One-shot HTTP server on loopback answering `status` + `body`; returns its URL and the
    /// request it received.
    async fn one_shot(
        status: u16,
        body: Vec<u8>,
    ) -> (String, tokio::sync::oneshot::Receiver<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/rpc", l.local_addr().unwrap());
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let mut buf = vec![0u8; 64 * 1024];
            let mut got = Vec::new();
            loop {
                let n = s.read(&mut buf).await.unwrap();
                got.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&got).to_string();
                if let Some(h) = text.find("\r\n\r\n") {
                    let len = text[..h]
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length: "))
                        .and_then(|v| v.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if got.len() >= h + 4 + len || n == 0 {
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            let _ = tx.send(String::from_utf8_lossy(&got).to_string());
            let head = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            let _ = s.write_all(head.as_bytes()).await;
            let _ = s.write_all(&body).await;
            let _ = s.shutdown().await;
        });
        (url, rx)
    }

    fn hook(url: String) -> Hook {
        Hook {
            id: Uuid::nil(),
            url,
            secret: "s3".into(),
            previous: None,
            events: vec!["rpc".into()],
        }
    }

    #[tokio::test]
    async fn rpc_call_is_signed_and_answer_comes_back() {
        let d = Dispatcher::noop();
        let (url, rx) = one_shot(200, br#"{"echo": 42}"#.to_vec()).await;
        let body = r#"{"event":"rpc","data":{"name":"echo"}}"#;
        let out = d.call_rpc(&hook(url), body).await;
        assert_eq!(out.status, Some(200));
        assert!(out.ok, "{out:?}");
        assert_eq!(out.body["echo"], 42);
        let req = rx.await.unwrap();
        assert!(req.starts_with("POST /rpc HTTP/1.1"));
        assert!(req.contains("x-aurix-event: rpc"));
        assert!(req.contains(&format!(
            "x-aurix-signature: {}",
            sign("s3", body.as_bytes())
        )));
        assert!(req.ends_with(body));
    }

    #[tokio::test]
    async fn rpc_non_2xx_non_json_and_oversize_are_reported() {
        let d = Dispatcher::noop();
        let (url, _rx) = one_shot(404, br#"{"error":"nope"}"#.to_vec()).await;
        let out = d.call_rpc(&hook(url), "{}").await;
        assert!(!out.ok && out.status == Some(404) && out.body["error"] == "nope");
        let (url, _rx) = one_shot(200, b"<html>".to_vec()).await;
        let out = d.call_rpc(&hook(url), "{}").await;
        assert!(!out.ok && out.error.as_deref() == Some("rpc endpoint answered non-JSON"));
        let (url, _rx) = one_shot(200, vec![b' '; RPC_MAX_RESPONSE + 1]).await;
        let out = d.call_rpc(&hook(url), "{}").await;
        assert!(!out.ok && out.error.as_deref() == Some("rpc answer too large"));
        let out = d
            .call_rpc(&hook("http://127.0.0.1:9/rpc".into()), "{}")
            .await;
        assert!(!out.ok && out.status.is_none());
        assert!(d
            .rpc(ProjectId(Uuid::nil()), serde_json::json!({}))
            .await
            .error
            .is_some());
    }

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
