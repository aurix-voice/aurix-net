//! Per-IP and per-key guards: token buckets for HTTP, WebSocket connects, auth failures and
//! API-key writes, plus concurrent session counts per IP. `X-Forwarded-For` is honoured only
//! from `server.trusted_proxies`.
use axum::body::Body;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use dashmap::DashMap;
use net_common::config::LimitsConfig;
use net_common::ipnet::IpNet;
use std::hash::Hash;
use std::net::{IpAddr, SocketAddr};
use std::time::Instant;

pub struct Buckets<K: Eq + Hash> {
    map: DashMap<K, (f64, Instant)>,
    per_sec: f64,
    burst: f64,
}

impl<K: Eq + Hash + Clone> Buckets<K> {
    pub fn new(per_sec: f64, burst: f64) -> Self {
        Self {
            map: DashMap::new(),
            per_sec,
            burst,
        }
    }

    /// Takes one token for `k`; false when the bucket is empty.
    pub fn allow(&self, k: K) -> bool {
        let now = Instant::now();
        let mut e = self.map.entry(k).or_insert((self.burst, now));
        let (tokens, last) = *e;
        let t = (tokens + last.elapsed().as_secs_f64() * self.per_sec).min(self.burst);
        if t >= 1.0 {
            *e = (t - 1.0, now);
            true
        } else {
            *e = (t, now);
            false
        }
    }

    /// Drops buckets idle long enough to be full again.
    pub fn sweep(&self) {
        let full_after = if self.per_sec > 0.0 {
            self.burst / self.per_sec
        } else {
            600.0
        };
        self.map
            .retain(|_, (_, last)| last.elapsed().as_secs_f64() < full_after + 1.0);
    }
}

pub struct Guards {
    pub http: Buckets<IpAddr>,
    pub ws_connects: Buckets<IpAddr>,
    pub auth_fail: Buckets<IpAddr>,
    pub key_writes: Buckets<uuid::Uuid>,
    pub key_grants: Buckets<uuid::Uuid>,
    sessions: DashMap<IpAddr, u32>,
    max_sessions_per_ip: u32,
    pub trusted_proxies: Vec<IpNet>,
}

impl Guards {
    pub fn new(l: &LimitsConfig, trusted: &[String]) -> Self {
        Self {
            http: Buckets::new(l.http_rps_per_ip as f64, 2.0 * l.http_rps_per_ip as f64),
            ws_connects: Buckets::new(
                l.ws_connects_per_min_per_ip as f64 / 60.0,
                l.ws_connects_per_min_per_ip as f64,
            ),
            auth_fail: Buckets::new(
                l.auth_failures_per_min_per_ip as f64 / 60.0,
                l.auth_failures_per_min_per_ip as f64,
            ),
            key_writes: Buckets::new(
                l.writes_per_sec_per_key as f64,
                2.0 * l.writes_per_sec_per_key as f64,
            ),
            key_grants: Buckets::new(
                l.grants_per_sec_per_key as f64,
                2.0 * l.grants_per_sec_per_key as f64,
            ),
            sessions: DashMap::new(),
            max_sessions_per_ip: l.max_sessions_per_ip.max(1),
            trusted_proxies: trusted.iter().filter_map(|p| IpNet::parse(p)).collect(),
        }
    }

    /// Counts a control session or data connection from `ip`; false when the IP is at cap.
    pub fn acquire_session(&self, ip: IpAddr) -> bool {
        let mut e = self.sessions.entry(ip).or_insert(0);
        if *e >= self.max_sessions_per_ip {
            return false;
        }
        *e += 1;
        true
    }

    pub fn release_session(&self, ip: IpAddr) {
        if let Some(mut e) = self.sessions.get_mut(&ip) {
            *e = e.saturating_sub(1);
            if *e == 0 {
                drop(e);
                self.sessions.remove_if(&ip, |_, n| *n == 0);
            }
        }
    }

    pub fn sweep(&self) {
        self.http.sweep();
        self.ws_connects.sweep();
        self.auth_fail.sweep();
        self.key_writes.sweep();
    }

    /// Peer address, or the first `X-Forwarded-For` hop when the peer is a trusted proxy.
    pub fn client_ip(&self, peer: IpAddr, headers: &HeaderMap) -> IpAddr {
        if self.trusted_proxies.iter().any(|n| n.contains(peer)) {
            if let Some(xff) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
                if let Some(ip) = xff.split(',').next().and_then(|s| s.trim().parse().ok()) {
                    return ip;
                }
            }
        }
        peer
    }
}

/// Request extension set by [`http_guard`].
#[derive(Clone, Copy, Debug)]
pub struct ClientIp(pub IpAddr);

/// Axum middleware: per-IP request budget, the `ClientIp` extension for handlers, and the
/// per-IP budget for failed authentications (401 → 429 once spent).
pub async fn http_guard(
    State(st): State<crate::state::Shared>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    mut req: Request<Body>,
    next: Next,
) -> Response {
    let ip = st.guards.client_ip(peer.ip(), req.headers());
    if !st.guards.http.allow(ip) {
        st.metrics.errors.with_label_values(&["rate_limited"]).inc();
        return (StatusCode::TOO_MANY_REQUESTS, "rate limited").into_response();
    }
    req.extensions_mut().insert(ClientIp(ip));
    let resp = next.run(req).await;
    // Failed REST auth (bad API key / operator token) shares the per-IP auth-failure budget
    // with WebSocket grant failures; once it is spent the caller sees 429 instead of 401,
    // so credential guessing cannot run at the general HTTP rate. Valid requests are untouched.
    if resp.status() == StatusCode::UNAUTHORIZED && !st.guards.auth_fail.allow(ip) {
        st.metrics
            .errors
            .with_label_values(&["auth_rate_limited"])
            .inc();
        return (
            StatusCode::TOO_MANY_REQUESTS,
            "too many failed authentications",
        )
            .into_response();
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bucket_refills() {
        let b = Buckets::new(1000.0, 2.0);
        assert!(b.allow(1u8));
        assert!(b.allow(1u8));
        assert!(!b.allow(1u8));
        std::thread::sleep(std::time::Duration::from_millis(5));
        assert!(b.allow(1u8));
    }
    #[test]
    fn xff_only_from_trusted_proxy() {
        let l = LimitsConfig {
            max_players_per_room: 8,
            max_rooms_per_node: 1,
            max_events_per_sec_per_room: 1,
            max_events_per_sec_per_player: 1,
            max_event_bytes: 1,
            max_props_bytes: 1,
            max_player_props_bytes: 1,
            max_cached_events_per_room: 1,
            max_sessions_per_ip: 2,
            http_rps_per_ip: 1,
            ws_connects_per_min_per_ip: 1,
            auth_failures_per_min_per_ip: 1,
            writes_per_sec_per_key: 1,
            grants_per_sec_per_key: 1,
        };
        let g = Guards::new(&l, &["10.0.0.0/8".into()]);
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", "203.0.113.9, 10.0.0.1".parse().unwrap());
        assert_eq!(
            g.client_ip("10.0.0.2".parse().unwrap(), &h),
            "203.0.113.9".parse::<IpAddr>().unwrap()
        );
        assert_eq!(
            g.client_ip("8.8.8.8".parse().unwrap(), &h),
            "8.8.8.8".parse::<IpAddr>().unwrap()
        );
        let ip: IpAddr = "1.1.1.1".parse().unwrap();
        assert!(g.acquire_session(ip) && g.acquire_session(ip) && !g.acquire_session(ip));
        g.release_session(ip);
        assert!(g.acquire_session(ip));
    }
}
