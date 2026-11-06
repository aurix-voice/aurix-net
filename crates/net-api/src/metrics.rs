use prometheus::{Encoder, IntCounter, IntCounterVec, IntGauge, Opts, Registry, TextEncoder};

pub struct Metrics {
    pub registry: Registry,
    pub rooms: IntGauge,
    pub players: IntGauge,
    pub control_sessions: IntGauge,
    pub datagrams_in: IntCounter,
    pub datagrams_out: IntCounter,
    pub bytes_in: IntCounter,
    pub bytes_out: IntCounter,
    pub events: IntCounterVec,
    pub errors: IntCounterVec,
}

impl Metrics {
    pub fn new() -> Self {
        let registry = Registry::new();
        let g = |n: &str, h: &str| {
            let g = IntGauge::new(n, h).unwrap();
            registry.register(Box::new(g.clone())).unwrap();
            g
        };
        let c = |n: &str, h: &str| {
            let c = IntCounter::new(n, h).unwrap();
            registry.register(Box::new(c.clone())).unwrap();
            c
        };
        let events = IntCounterVec::new(
            Opts::new("net_events_total", "relayed events by channel"),
            &["channel"],
        )
        .unwrap();
        registry.register(Box::new(events.clone())).unwrap();
        let errors = IntCounterVec::new(
            Opts::new("net_errors_total", "client-visible errors by code"),
            &["code"],
        )
        .unwrap();
        registry.register(Box::new(errors.clone())).unwrap();
        Self {
            rooms: g("net_rooms", "open rooms on this node"),
            players: g("net_players", "online players on this node"),
            control_sessions: g("net_control_sessions", "open control websockets"),
            datagrams_in: c("net_datagrams_in_total", "datagrams received"),
            datagrams_out: c("net_datagrams_out_total", "datagrams sent"),
            bytes_in: c("net_bytes_in_total", "data-path bytes received"),
            bytes_out: c("net_bytes_out_total", "data-path bytes sent"),
            events,
            errors,
            registry,
        }
    }

    pub fn render(&self) -> String {
        let mut buf = Vec::new();
        TextEncoder::new()
            .encode(&self.registry.gather(), &mut buf)
            .ok();
        String::from_utf8(buf).unwrap_or_default()
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}
