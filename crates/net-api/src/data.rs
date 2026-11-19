//! QUIC data plane: the first datagram of a connection is the data ticket; everything after
//! runs through the session's [`net_transport::Link`].

use crate::host::{handle_data_frame, DataSession, DataSink, HostedRoom};
use crate::state::Shared;
use crate::ws::{attach_data, spawn_ticker};
use net_common::{ActorId, ProjectId};
use net_transport::DataTicket;
use std::sync::Arc;
use std::time::Duration;

/// How long a fresh connection may take to present its ticket.
const TICKET_WAIT: Duration = Duration::from_secs(5);

/// The project of the room when the ticket's session still holds its slot: a newer session of
/// the same player takes the slot over, and the old ticket must not follow it there.
fn slot_project(room: &HostedRoom, ticket: &DataTicket) -> Option<ProjectId> {
    room.sessions
        .get(&ticket.actor)
        .is_some_and(|attached| attached.session == ticket.session)
        .then_some(room.project)
}

/// Clears the slot's data path when the connection ends, unless a newer one took it; events
/// then reach the player over the control channel.
fn detach(room: &mut HostedRoom, actor: ActorId, ds: &Arc<DataSession>) {
    if let Some(session) = room.sessions.get_mut(&actor) {
        if session.data.as_ref().is_some_and(|d| Arc::ptr_eq(d, ds)) {
            session.data = None;
        }
    }
}

pub async fn run_quic(st: Shared, endpoint: quinn::Endpoint) {
    while let Some(incoming) = endpoint.accept().await {
        // Address validation before any handshake work: no amplification from spoofed sources.
        if !incoming.remote_address_validated() {
            let _ = incoming.retry();
            continue;
        }
        let ip = incoming.remote_address().ip();
        let Some(slot) = st.guards.acquire_session(ip) else {
            st.metrics.errors.with_label_values(&["rate_limited"]).inc();
            incoming.refuse();
            continue;
        };
        let st = st.clone();
        tokio::spawn(async move {
            let _slot = slot;
            if let Err(e) = handle(st, incoming).await {
                tracing::debug!(error = %e, "quic connection ended");
            }
        });
    }
}

async fn handle(st: Shared, incoming: quinn::Incoming) -> anyhow::Result<()> {
    let conn = incoming.await?;
    let deadline = tokio::time::Instant::now() + TICKET_WAIT;
    // Datagrams that overtook the ticket are dropped: only the ticket proves whose they are.
    let (first, ticket) = loop {
        let Ok(datagram) = tokio::time::timeout_at(deadline, conn.read_datagram()).await else {
            conn.close(1u32.into(), b"bad ticket");
            anyhow::bail!("no valid ticket");
        };
        let datagram = datagram?;
        if let Some(ticket) = st.tickets.verify(&datagram) {
            break (datagram, ticket);
        }
    };
    let Some(hosted) = st.host.get(ticket.room) else {
        conn.close(2u32.into(), b"no room");
        anyhow::bail!("no such room");
    };
    let Some(project) = slot_project(&hosted.lock(), &ticket) else {
        conn.close(3u32.into(), b"not joined");
        anyhow::bail!("actor not joined");
    };
    let ds = st.host.data_session(
        project,
        DataSink::Quic(conn.clone()),
        st.metrics.clone(),
        Duration::from_millis(st.cfg.transport.idle_timeout_ms),
    );
    let ds = attach_data(&hosted, ticket.actor, ds);
    spawn_ticker(&st, ds.clone());
    loop {
        let datagram = match conn.read_datagram().await {
            Ok(received) => received,
            Err(_) => break,
        };
        // A client repeats the ticket until it hears from the node.
        if datagram == first {
            continue;
        }
        for message in ds.receive(datagram) {
            if message.payload.is_empty() {
                continue;
            }
            let mut room = hosted.lock();
            // A newer connection (reconnect or another session of the player) owns the slot.
            if !room
                .sessions
                .get(&ticket.actor)
                .and_then(|a| a.data.as_ref())
                .is_some_and(|d| Arc::ptr_eq(d, &ds))
            {
                return Ok(());
            }
            if let Err(e) = handle_data_frame(
                &mut room,
                ticket.actor,
                message.channel,
                message.payload,
                &st.metrics,
            ) {
                st.metrics.errors.with_label_values(&[e.code()]).inc();
            }
        }
    }
    detach(&mut hosted.lock(), ticket.actor, &ds);
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
        let Some(slot) = st.guards.acquire_session(ip) else {
            st.metrics.errors.with_label_values(&["rate_limited"]).inc();
            continue;
        };
        let st = st.clone();
        tokio::spawn(async move {
            let _slot = slot;
            if let Err(e) = handle_wt(st, incoming).await {
                tracing::debug!(error = %e, "webtransport session ended");
            }
        });
    }
}

async fn handle_wt(
    st: Shared,
    incoming: wtransport::endpoint::IncomingSession,
) -> anyhow::Result<()> {
    let request = incoming.await?;
    let conn = request.accept().await?;
    let deadline = tokio::time::Instant::now() + TICKET_WAIT;
    let (first, ticket) = loop {
        let Ok(datagram) = tokio::time::timeout_at(deadline, conn.receive_datagram()).await else {
            conn.close(1u32.into(), b"bad ticket");
            anyhow::bail!("no valid ticket");
        };
        let datagram = datagram?.payload();
        if let Some(ticket) = st.tickets.verify(&datagram) {
            break (datagram, ticket);
        }
    };
    let Some(hosted) = st.host.get(ticket.room) else {
        conn.close(2u32.into(), b"no room");
        anyhow::bail!("no such room");
    };
    let Some(project) = slot_project(&hosted.lock(), &ticket) else {
        conn.close(3u32.into(), b"not joined");
        anyhow::bail!("actor not joined");
    };
    let ds = st.host.data_session(
        project,
        DataSink::WebTransport(conn.clone()),
        st.metrics.clone(),
        Duration::from_millis(st.cfg.transport.idle_timeout_ms),
    );
    let ds = attach_data(&hosted, ticket.actor, ds);
    spawn_ticker(&st, ds.clone());
    loop {
        let datagram = match conn.receive_datagram().await {
            Ok(received) => received.payload(),
            Err(_) => break,
        };
        if datagram == first {
            continue;
        }
        for message in ds.receive(datagram) {
            if message.payload.is_empty() {
                continue;
            }
            let mut room = hosted.lock();
            // A newer connection (reconnect or another session of the player) owns the slot.
            if !room
                .sessions
                .get(&ticket.actor)
                .and_then(|a| a.data.as_ref())
                .is_some_and(|d| Arc::ptr_eq(d, &ds))
            {
                return Ok(());
            }
            if let Err(e) = handle_data_frame(
                &mut room,
                ticket.actor,
                message.channel,
                message.payload,
                &st.metrics,
            ) {
                st.metrics.errors.with_label_values(&[e.code()]).inc();
            }
        }
    }
    detach(&mut hosted.lock(), ticket.actor, &ds);
    Ok(())
}
