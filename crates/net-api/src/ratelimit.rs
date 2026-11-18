//! Per-IP and per-key guards: token buckets for HTTP, WebSocket connects, auth failures and
//! API-key writes, plus concurrent session counts per IP. `X-Forwarded-For` is honoured only
//! from `server.trusted_proxies`.
use axum::body::Body;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use dashmap::DashMap;
use governor::{DefaultKeyedRateLimiter, Quota, RateLimiter};
use net_common::config::LimitsConfig;
use net_common::ipnet::{net_contains, parse_net, IpNet};
use std::net::{IpAddr, SocketAddr};
use std::num::NonZeroU32;

pub type Limiter<K> = DefaultKeyedRateLimiter<K>;

fn at_least_one(n: u32) -> NonZeroU32 {
    NonZeroU32::new(n).unwrap_or(NonZeroU32::MIN)
}

/// `n` a second, in bursts of up to twice that.
fn per_second(n: u32) -> Quota {
    Quota::per_second(at_least_one(n)).allow_burst(at_least_one(n.saturating_mul(2)))
}

fn per_minute(n: u32) -> Quota {
    Quota::per_minute(at_least_one(n))
}

pub struct Guards {
    pub http: Limiter<IpAddr>,
    pub ws_connects: Limiter<IpAddr>,
    pub auth_fail: Limiter<IpAddr>,
    pub key_writes: Limiter<uuid::Uuid>,
    pub key_grants: Limiter<uuid::Uuid>,
    sessions: DashMap<IpAddr, u32>,
    max_sessions_per_ip: u32,
    pub trusted_proxies: Vec<IpNet>,
}

impl Guards {
    pub fn new(l: &LimitsConfig, trusted: &[String]) -> Self {
        Self {
            http: RateLimiter::keyed(per_second(l.http_rps_per_ip)),
            ws_connects: RateLimiter::keyed(per_minute(l.ws_connects_per_min_per_ip)),
            auth_fail: RateLimiter::keyed(per_minute(l.auth_failures_per_min_per_ip)),
            key_writes: RateLimiter::keyed(per_second(l.writes_per_sec_per_key)),
            key_grants: RateLimiter::keyed(per_second(l.grants_per_sec_per_key)),
            sessions: DashMap::new(),
            max_sessions_per_ip: l.max_sessions_per_ip.max(1),
            trusted_proxies: trusted.iter().filter_map(|p| parse_net(p)).collect(),
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
        self.http.retain_recent();
        self.ws_connects.retain_recent();
        self.auth_fail.retain_recent();
        self.key_writes.retain_recent();
        self.key_grants.retain_recent();
    }

    /// Peer address, or the first `X-Forwarded-For` hop when the peer is a trusted proxy.
    pub fn client_ip(&self, peer: IpAddr, headers: &HeaderMap) -> IpAddr {
        if self.trusted_proxies.iter().any(|n| net_contains(n, peer)) {
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

/// Resolved client address, written by the middleware (any client-supplied value is replaced)
/// so authentication helpers that only see headers can enforce per-key allowlists.
pub const CLIENT_IP_HEADER: &str = "x-aurix-client-ip";

/// Axum middleware: per-IP request budget, the `ClientIp` extension for handlers, and the
/// per-IP budget for failed authentications (401 → 429 once spent).
pub async fn http_guard(
    State(st): State<crate::state::Shared>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    mut req: Request<Body>,
    next: Next,
) -> Response {
    let ip = st.guards.client_ip(peer.ip(), req.headers());
    if st.guards.http.check_key(&ip).is_err() {
        st.metrics.errors.with_label_values(&["rate_limited"]).inc();
        return (StatusCode::TOO_MANY_REQUESTS, "rate limited").into_response();
    }
    req.extensions_mut().insert(ClientIp(ip));
    if let Ok(v) = ip.to_string().parse() {
        req.headers_mut().insert(CLIENT_IP_HEADER, v);
    }
    let resp = next.run(req).await;
    // Failed REST auth (bad API key / operator token) shares the per-IP auth-failure budget
    // with WebSocket grant failures; once it is spent the caller sees 429 instead of 401,
    // so credential guessing cannot run at the general HTTP rate. Valid requests are untouched.
    if resp.status() == StatusCode::UNAUTHORIZED && st.guards.auth_fail.check_key(&ip).is_err() {
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
    fn burst_is_twice_the_rate_per_second() {
        let b: Limiter<u8> = RateLimiter::keyed(per_second(500));
        assert!((0..1000).all(|_| b.check_key(&1).is_ok()));
        assert!(b.check_key(&1).is_err());
        assert!(b.check_key(&2).is_ok());
        std::thread::sleep(std::time::Duration::from_millis(5));
        assert!(b.check_key(&1).is_ok());
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
