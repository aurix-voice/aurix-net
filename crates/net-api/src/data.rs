//! QUIC data plane: the first datagram of a connection is the data ticket; everything after
//! runs through the session's [`Link`].

use crate::host::{handle_data_frame, DataSession, DataSink};
use crate::state::Shared;
use crate::ws::{attach_data, spawn_ticker};
use std::sync::Arc;
use std::time::Duration;

pub async fn run_quic(st: Shared, endpoint: quinn::Endpoint) {
    while let Some(incoming) = endpoint.accept().await {
        // Address validation before any handshake work: no amplification from spoofed sources.
        if !incoming.remote_address_validated() {
            let _ = incoming.retry();
            continue;
        }
        let ip = incoming.remote_address().ip();
        if !st.guards.acquire_session(ip) {
            st.metrics.errors.with_label_values(&["rate_limited"]).inc();
            incoming.refuse();
            continue;
        }
        let st = st.clone();
        tokio::spawn(async move {
            if let Err(e) = handle(st.clone(), incoming).await {
                tracing::debug!(error = %e, "quic connection ended");
            }
            st.guards.release_session(ip);
        });
    }
}

async fn handle(st: Shared, incoming: quinn::Incoming) -> anyhow::Result<()> {
    let conn = incoming.await?;
    let first = tokio::time::timeout(Duration::from_secs(5), conn.read_datagram()).await??;
    let Some(t) = st.tickets.verify(&first) else {
        conn.close(1u32.into(), b"bad ticket");
        anyhow::bail!("bad ticket");
    };
    let Some(h) = st.host.get(t.room) else {
        conn.close(2u32.into(), b"no room");
        anyhow::bail!("no such room");
    };
    {
        let g = h.lock();
        if !g.sessions.contains_key(&t.actor) {
            drop(g);
            conn.close(3u32.into(), b"not joined");
            anyhow::bail!("actor not joined");
        }
    }
    let ds = DataSession::new(DataSink::Quic(conn.clone()), st.metrics.clone());
    let ds = attach_data(&h, t.actor, ds);
    spawn_ticker(&st, ds.clone());
    loop {
        let d = match conn.read_datagram().await {
            Ok(d) => d,
            Err(_) => break,
        };
        for m in ds.receive(d) {
            if m.payload.is_empty() {
                continue;
            }
            let mut g = h.lock();
            // A newer connection (reconnect or another session of the player) owns the slot.
            if !g
                .sessions
                .get(&t.actor)
                .and_then(|a| a.data.as_ref())
                .is_some_and(|d| Arc::ptr_eq(d, &ds))
            {
                return Ok(());
            }
            if let Err(e) = handle_data_frame(&mut g, t.actor, m.channel, m.payload, &st.metrics) {
                st.metrics.errors.with_label_values(&[e.code()]).inc();
            }
        }
    }
    // Detach only if we are still the active data session.
    let mut g = h.lock();
    if let Some(a) = g.sessions.get_mut(&t.actor) {
        if a.data.as_ref().is_some_and(|d| Arc::ptr_eq(d, &ds)) {
            a.data = None;
        }
    }
    Ok(())
}

/// WebTransport data plane: identical protocol to [`run_quic`] (ticket first, then datagrams
/// through the session's link), reached by browsers via `joined.data.webtransport_url`.
pub async fn run_webtransport(
    st: Shared,
    endpoint: wtransport::Endpoint<wtransport::endpoint::endpoint_side::Server>,
) {
    loop {
        let incoming = endpoint.accept().await;
        let ip = incoming.remote_address().ip();
        if !st.guards.acquire_session(ip) {
            st.metrics.errors.with_label_values(&["rate_limited"]).inc();
            continue;
        }
        let st = st.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_wt(st.clone(), incoming).await {
                tracing::debug!(error = %e, "webtransport session ended");
            }
            st.guards.release_session(ip);
        });
    }
}

async fn handle_wt(
    st: Shared,
    incoming: wtransport::endpoint::IncomingSession,
) -> anyhow::Result<()> {
    let request = incoming.await?;
    let conn = request.accept().await?;
    let first = tokio::time::timeout(Duration::from_secs(5), conn.receive_datagram()).await??;
    let Some(t) = st.tickets.verify(&first.payload()) else {
        conn.close(1u32.into(), b"bad ticket");
        anyhow::bail!("bad ticket");
    };
    let Some(h) = st.host.get(t.room) else {
        conn.close(2u32.into(), b"no room");
        anyhow::bail!("no such room");
    };
    if !h.lock().sessions.contains_key(&t.actor) {
        conn.close(3u32.into(), b"not joined");
        anyhow::bail!("actor not joined");
    }
    let ds = DataSession::new(DataSink::WebTransport(conn.clone()), st.metrics.clone());
    let ds = attach_data(&h, t.actor, ds);
    spawn_ticker(&st, ds.clone());
    loop {
        let d = match conn.receive_datagram().await {
            Ok(d) => d.payload(),
            Err(_) => break,
        };
        for m in ds.receive(d) {
            if m.payload.is_empty() {
                continue;
            }
            let mut g = h.lock();
            // A newer connection (reconnect or another session of the player) owns the slot.
            if !g
                .sessions
                .get(&t.actor)
                .and_then(|a| a.data.as_ref())
                .is_some_and(|d| Arc::ptr_eq(d, &ds))
            {
                return Ok(());
            }
            if let Err(e) = handle_data_frame(&mut g, t.actor, m.channel, m.payload, &st.metrics) {
                tracing::debug!(error = %e, "bad data frame (webtransport)");
            }
        }
    }
    Ok(())
}
