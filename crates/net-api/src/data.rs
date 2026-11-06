//! QUIC data plane: the first datagram of a connection is the data ticket; everything after
//! runs through the session's [`Link`].

use crate::host::{handle_data_frame, DataSession, DataSink};
use crate::state::Shared;
use crate::ws::{attach_data, spawn_ticker};
use std::sync::Arc;
use std::time::Duration;

pub async fn run_quic(st: Shared, endpoint: quinn::Endpoint) {
    while let Some(incoming) = endpoint.accept().await {
        let st = st.clone();
        tokio::spawn(async move {
            if let Err(e) = handle(st, incoming).await {
                tracing::debug!(error = %e, "quic connection ended");
            }
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
    let ds = DataSession::new(
        DataSink::Quic(conn.clone()),
        Arc::new(crate::metrics::Metrics::new()),
    );
    let ds = attach_data(&h, t.actor, ds);
    spawn_ticker(ds.clone());
    // Echo an empty ack so the client learns the path is open.
    ds.send(net_common::Channel::ReliableOrdered, bytes::Bytes::new());
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
