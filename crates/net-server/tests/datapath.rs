//! Opening the QUIC data path of a running node when datagrams are lost or repeated.

mod common;

use base64::Engine;
use bytes::Bytes;
use common::{joined, refused, Node};
use futures::FutureExt;
use net_common::Channel;
use net_transport::{quic, Link};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

const IDLE: Duration = Duration::from_secs(15);

struct Path {
    conn: quinn::Connection,
    ticket: Bytes,
}

async fn connect(reply: &Value) -> Path {
    let data = &reply["data"];
    let ticket = base64::engine::general_purpose::STANDARD
        .decode(data["ticket"].as_str().expect("ticket"))
        .expect("base64");
    let transport = quic::transport_config(Duration::from_secs(15), Duration::from_secs(5));
    let fingerprint = data["cert_sha256"].as_str().expect("cert_sha256");
    let endpoint = quic::client_endpoint("0.0.0.0:0".parse().unwrap(), fingerprint, transport)
        .expect("endpoint");
    let port = data["port"].as_u64().expect("port") as u16;
    let conn = endpoint
        .connect(([127, 0, 0, 1], port).into(), "aurix-net")
        .expect("connect")
        .await
        .expect("handshake");
    Path {
        conn,
        ticket: Bytes::from(ticket),
    }
}

impl Path {
    /// Feeds the node's datagrams to the link until it reports the path open.
    async fn opened(&self, link: &mut Link) -> bool {
        let wait = async {
            loop {
                let Ok(datagram) = self.conn.read_datagram().await else {
                    return false;
                };
                let opened = link
                    .receive(datagram, Instant::now())
                    .iter()
                    .any(|m| m.channel == Channel::ReliableOrdered && m.payload.is_empty());
                if opened {
                    return true;
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(3), wait)
            .await
            .unwrap_or(false)
    }
}

fn packet(link: &mut Link, payload: &'static [u8]) -> Bytes {
    link.send(
        Channel::ReliableOrdered,
        Bytes::from_static(payload),
        Instant::now(),
    )
    .expect("send");
    link.next_outgoing().expect("datagram")
}

#[tokio::test]
async fn packet_that_overtakes_the_ticket_does_not_close_the_path() {
    let node = Node::start().await;
    let project = node.project("datapath").await;
    let mut ann = project.player("ann").await;
    let reply = ann.call(json!({"t": "create", "name": "room"})).await;
    joined(&reply);
    let path = connect(&reply).await;
    let mut link = Link::new(Instant::now(), IDLE);

    path.conn
        .send_datagram(packet(&mut link, b"early"))
        .expect("send");
    tokio::time::sleep(Duration::from_millis(200)).await;
    let _ = path.conn.send_datagram(path.ticket.clone());

    assert!(path.opened(&mut link).await, "the node closed the path");
}

#[tokio::test]
async fn ticket_repeated_after_the_path_is_open_is_ignored() {
    let node = Node::start().await;
    let project = node.project("datapath").await;
    let mut ann = project.player("ann").await;
    let reply = ann.call(json!({"t": "create", "name": "room"})).await;
    joined(&reply);
    let path = connect(&reply).await;
    let mut link = Link::new(Instant::now(), IDLE);

    path.conn.send_datagram(path.ticket.clone()).expect("send");
    assert!(path.opened(&mut link).await, "the path did not open");
    for _ in 0..3 {
        path.conn.send_datagram(path.ticket.clone()).expect("send");
    }

    path.conn
        .send_datagram(packet(&mut link, b""))
        .expect("send");
    let acked = async {
        loop {
            let datagram = path.conn.read_datagram().await.expect("the path closed");
            link.receive(datagram, Instant::now());
            if link.stats().rtt > Duration::ZERO {
                return;
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(3), acked)
        .await
        .expect("the node did not acknowledge a packet sent after the repeats");
}

#[tokio::test]
async fn ticket_of_a_replaced_session_does_not_open_a_path() {
    let node = Node::start().await;
    let project = node.project("datapath").await;
    let mut old = project.player("ann").await;
    let old_reply = old.call(json!({"t": "create", "name": "room"})).await;
    let actor = joined(&old_reply);
    let mut new = project.player("ann").await;
    let new_reply = new.call(json!({"t": "join", "name": "room"})).await;
    assert_eq!(joined(&new_reply), actor);

    let stale = connect(&old_reply).await;
    stale
        .conn
        .send_datagram(stale.ticket.clone())
        .expect("send");
    assert!(
        !stale.opened(&mut Link::new(Instant::now(), IDLE)).await,
        "the replaced session got the data path"
    );

    let path = connect(&new_reply).await;
    path.conn.send_datagram(path.ticket.clone()).expect("send");
    assert!(
        path.opened(&mut Link::new(Instant::now(), IDLE)).await,
        "the path did not open"
    );
}

#[tokio::test]
async fn events_fall_back_to_the_control_channel_when_webtransport_ends() {
    let node = Node::start().await;
    let project = node.project("datapath").await;
    let mut ann = project.player("ann").await;
    let reply = ann.call(json!({"t": "create", "name": "room"})).await;
    joined(&reply);
    let data = &reply["data"];
    let ticket = base64::engine::general_purpose::STANDARD
        .decode(data["ticket"].as_str().expect("ticket"))
        .expect("base64");
    let endpoint = net_transport::webtransport::client_endpoint(
        data["cert_sha256"].as_str().expect("cert_sha256"),
    )
    .expect("endpoint");
    let conn = endpoint
        .connect(data["webtransport_url"].as_str().expect("webtransport_url"))
        .await
        .expect("webtransport session");
    conn.send_datagram(ticket).expect("send");
    let mut link = Link::new(Instant::now(), IDLE);
    let opened = async {
        loop {
            let datagram = conn.receive_datagram().await.expect("the path closed");
            let messages = link.receive(datagram.payload(), Instant::now());
            if messages.iter().any(|m| m.payload.is_empty()) {
                return;
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(3), opened)
        .await
        .expect("the path did not open");

    conn.close(0u32.into(), b"gone");
    drop(conn);
    tokio::time::sleep(Duration::from_millis(500)).await;
    let mut bob = project.player("bob").await;
    joined(&bob.call(json!({"t": "join", "name": "room"})).await);
    bob.send(json!({"t": "event", "code": 1, "data": "AQI=", "target": {"kind": "others"}}))
        .await;

    let deadline = Instant::now() + Duration::from_secs(3);
    let mut event = None;
    while event.is_none() && Instant::now() < deadline {
        event = ann
            .poll(Duration::from_millis(200))
            .await
            .filter(|message| message["t"] == "event");
    }
    assert!(event.is_some(), "ann got no event after her path closed");
}

#[tokio::test]
async fn events_fall_back_to_the_control_channel_when_a_tunnel_stops_acknowledging() {
    let node = Node::start_with(&[("NET__TRANSPORT__IDLE_TIMEOUT_MS", "1000")]).await;
    let project = node.project("datapath").await;
    let mut ann = project.player("ann").await;
    let reply = ann.call(json!({"t": "create", "name": "room"})).await;
    joined(&reply);
    let ticket = base64::engine::general_purpose::STANDARD
        .decode(reply["data"]["ticket"].as_str().expect("ticket"))
        .expect("base64");
    // ann opens the tunnel and acknowledges nothing the node sends through it
    ann.send_binary(ticket).await;
    let mut bob = project.player("bob").await;
    joined(&bob.call(json!({"t": "join", "name": "room"})).await);
    let event = json!({"t": "event", "code": 1, "data": "AQI=", "target": {"kind": "others"}});

    bob.send(event.clone()).await;
    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        let message = ann.poll(Duration::from_millis(100)).await;
        assert!(
            message.is_none_or(|message| message["t"] != "event"),
            "the event left the tunnel while the tunnel was alive"
        );
    }

    tokio::time::sleep(Duration::from_secs(1)).await;
    bob.send(event).await;
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut event = None;
    while event.is_none() && Instant::now() < deadline {
        event = ann
            .poll(Duration::from_millis(200))
            .await
            .filter(|message| message["t"] == "event");
    }
    assert!(
        event.is_some(),
        "ann got no event after her tunnel was given up"
    );
}

#[tokio::test]
async fn unsequenced_event_goes_sequenced_to_a_path_whose_client_did_not_announce_it() {
    use net_client::{Client, Options};
    use net_rooms::event::{CacheOp, EventTarget};

    let node = Node::start().await;
    let project = node.project("datapath").await;
    let mut ann = Client::connect(&node.http, &project.grant("ann").await, Options::default())
        .await
        .expect("ann connects");
    let create = serde_json::from_value(json!({"t": "create", "name": "room"})).expect("create");
    ann.join(create).await.expect("ann creates");
    // A client from before the channel: its `auth` announces nothing.
    let mut old = project.player("old").await;
    let reply = old.call(json!({"t": "join", "name": "room"})).await;
    joined(&reply);
    let path = connect(&reply).await;
    let mut link = Link::new(Instant::now(), IDLE);
    path.conn.send_datagram(path.ticket.clone()).expect("send");
    assert!(path.opened(&mut link).await, "the path did not open");

    for code in 1..=5 {
        ann.send_event_on(
            Channel::UnreliableUnsequenced,
            code,
            EventTarget::Others,
            Bytes::from_static(b"shot"),
            None,
            CacheOp::None,
        )
        .expect("ann sends");
        ann.flush();
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let mut got = Vec::new();
    let all = async {
        while got.len() < 5 {
            let datagram = path.conn.read_datagram().await.expect("the path closed");
            for message in link.receive(datagram, Instant::now()) {
                if !message.payload.is_empty() {
                    got.push(message.channel);
                }
            }
        }
    };
    let _ = tokio::time::timeout(Duration::from_secs(5), all).await;
    assert_eq!(got, [Channel::UnreliableSequenced; 5]);
}

fn ticket(reply: &Value) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(reply["data"]["ticket"].as_str().expect("ticket"))
        .expect("base64")
}

/// A datagram with a frame no room takes.
fn refused_frame(link: &mut Link) -> Bytes {
    packet(link, &[0xff])
}

fn errors(heard: &[Value]) -> Vec<(&str, &str)> {
    heard
        .iter()
        .filter(|message| message["t"] == "error")
        .map(|message| {
            (
                message["code"].as_str().expect("code"),
                message["message"].as_str().expect("message"),
            )
        })
        .collect()
}

const REFUSED: (&str, &str) = ("invalid_request", "invalid request: frame too short");

#[tokio::test]
async fn refused_data_frame_is_answered_over_quic() {
    let node = Node::start().await;
    let project = node.project("datapath").await;
    let mut ann = project.player("ann").await;
    let reply = ann.call(json!({"t": "create", "name": "room"})).await;
    joined(&reply);
    let path = connect(&reply).await;
    path.conn.send_datagram(path.ticket.clone()).expect("send");
    let mut link = Link::new(Instant::now(), IDLE);
    assert!(path.opened(&mut link).await, "the path did not open");

    path.conn
        .send_datagram(refused_frame(&mut link))
        .expect("send");
    let heard = ann.drain(Duration::from_millis(500)).await;
    assert_eq!(errors(&heard), [REFUSED]);
}

#[tokio::test]
async fn refused_data_frame_is_answered_over_webtransport() {
    let node = Node::start().await;
    let project = node.project("datapath").await;
    let mut ann = project.player("ann").await;
    let reply = ann.call(json!({"t": "create", "name": "room"})).await;
    joined(&reply);
    let data = &reply["data"];
    let endpoint = net_transport::webtransport::client_endpoint(
        data["cert_sha256"].as_str().expect("cert_sha256"),
    )
    .expect("endpoint");
    let conn = endpoint
        .connect(data["webtransport_url"].as_str().expect("webtransport_url"))
        .await
        .expect("webtransport session");
    conn.send_datagram(ticket(&reply)).expect("send");
    let mut link = Link::new(Instant::now(), IDLE);
    let opened = async {
        loop {
            let datagram = conn.receive_datagram().await.expect("the path closed");
            let messages = link.receive(datagram.payload(), Instant::now());
            if messages.iter().any(|m| m.payload.is_empty()) {
                return;
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(3), opened)
        .await
        .expect("the path did not open");

    conn.send_datagram(refused_frame(&mut link)).expect("send");
    let heard = ann.drain(Duration::from_millis(500)).await;
    assert_eq!(errors(&heard), [REFUSED]);
}

#[tokio::test]
async fn refused_data_frame_is_answered_over_the_tunnel() {
    let node = Node::start().await;
    let project = node.project("datapath").await;
    let mut ann = project.player("ann").await;
    let reply = ann.call(json!({"t": "create", "name": "room"})).await;
    joined(&reply);
    ann.send_binary(ticket(&reply)).await;
    let mut link = Link::new(Instant::now(), IDLE);

    ann.send_binary(refused_frame(&mut link).to_vec()).await;
    let heard = ann.drain(Duration::from_millis(500)).await;
    assert_eq!(errors(&heard), [REFUSED]);
}

#[tokio::test]
async fn frames_on_their_way_when_a_player_is_kicked_are_not_answered() {
    let node = Node::start().await;
    let project = node.project("datapath").await;
    let mut ann = project.player("ann").await;
    joined(&ann.call(json!({"t": "create", "name": "room"})).await);
    let mut bob = project.player("bob").await;
    let reply = bob.call(json!({"t": "join", "name": "room"})).await;
    let actor = joined(&reply);
    bob.send_binary(ticket(&reply)).await;
    let mut link = Link::new(Instant::now(), IDLE);

    ann.send(json!({"t": "kick", "actor": actor})).await;
    bob.until("left").await;
    // what bob sent before it read `left`: frames of its tunnel and the ticket once more
    for _ in 0..8 {
        bob.send_binary(refused_frame(&mut link).to_vec()).await;
    }
    bob.send_binary(ticket(&reply)).await;
    let heard = bob.drain(Duration::from_millis(500)).await;
    assert_eq!(errors(&heard), []);

    // the next room and its tunnel are not affected
    let reply = bob.call(json!({"t": "create", "name": "other"})).await;
    joined(&reply);
    bob.send_binary(ticket(&reply)).await;
    let mut link = Link::new(Instant::now(), IDLE);
    bob.send_binary(refused_frame(&mut link).to_vec()).await;
    let heard = bob.drain(Duration::from_millis(500)).await;
    assert_eq!(errors(&heard), [REFUSED]);
}

#[tokio::test]
async fn tunnel_that_was_given_up_is_answered_once() {
    let node = Node::start_with(&[("NET__TRANSPORT__IDLE_TIMEOUT_MS", "1000")]).await;
    let project = node.project("datapath").await;
    let mut ann = project.player("ann").await;
    let reply = ann.call(json!({"t": "create", "name": "room"})).await;
    joined(&reply);
    ann.send_binary(ticket(&reply)).await;
    let mut link = Link::new(Instant::now(), IDLE);
    let mut bob = project.player("bob").await;
    joined(&bob.call(json!({"t": "join", "name": "room"})).await);
    // ann acknowledges nothing, so the node gives her tunnel up: bob's events then reach her
    // over the control channel
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(Instant::now() < deadline, "the tunnel was not given up");
        bob.send(json!({"t": "event", "code": 1, "data": "AQI=", "target": {"kind": "others"}}))
            .await;
        let heard = ann.drain(Duration::from_millis(200)).await;
        if heard.iter().any(|message| message["t"] == "event") {
            break;
        }
    }

    let frame = refused_frame(&mut link).to_vec();
    for _ in 0..50 {
        ann.send_binary(frame.clone()).await;
    }
    let heard = ann.drain(Duration::from_millis(500)).await;
    assert_eq!(
        errors(&heard),
        [("transport", "transport: data path closed: link dead")]
    );
}

#[tokio::test]
async fn foreign_ticket_and_a_second_session_are_still_refused() {
    let node = Node::start().await;
    let project = node.project("datapath").await;
    let mut ann = project.player("ann").await;
    let reply = ann.call(json!({"t": "create", "name": "room"})).await;
    joined(&reply);
    let mut bob = project.player("bob").await;
    joined(&bob.call(json!({"t": "join", "name": "room"})).await);

    let mismatch = (
        "unauthorized",
        "unauthorized: ticket does not match session",
    );
    bob.send_binary(ticket(&reply)).await;
    bob.send_binary(ticket(&reply)).await;
    let heard = bob.drain(Duration::from_millis(500)).await;
    assert_eq!(errors(&heard), [mismatch, mismatch]);

    // what is no ticket at all is answered once
    bob.send_binary(vec![7; 40]).await;
    bob.send_binary(vec![7; 40]).await;
    let heard = bob.drain(Duration::from_millis(500)).await;
    assert_eq!(
        errors(&heard),
        [("unauthorized", "unauthorized: bad data ticket")]
    );

    ann.send_binary(ticket(&reply)).await;
    let mut link = Link::new(Instant::now(), IDLE);
    let mut second = project.player("ann").await;
    joined(&second.call(json!({"t": "join", "name": "room"})).await);
    ann.until("left").await;
    ann.send_binary(refused_frame(&mut link).to_vec()).await;
    let heard = ann.drain(Duration::from_millis(500)).await;
    assert_eq!(errors(&heard), [("conflict", "conflict: session_replaced")]);
}

#[tokio::test]
async fn frames_of_a_kicked_player_are_not_answered_over_quic() {
    let node = Node::start().await;
    let project = node.project("datapath").await;
    let mut ann = project.player("ann").await;
    joined(&ann.call(json!({"t": "create", "name": "room"})).await);
    let mut bob = project.player("bob").await;
    let reply = bob.call(json!({"t": "join", "name": "room"})).await;
    let actor = joined(&reply);
    let path = connect(&reply).await;
    path.conn.send_datagram(path.ticket.clone()).expect("send");
    let mut link = Link::new(Instant::now(), IDLE);
    assert!(path.opened(&mut link).await, "the path did not open");

    ann.send(json!({"t": "kick", "actor": actor})).await;
    bob.until("left").await;
    for _ in 0..8 {
        // the node closes the connection when it reads the first of them
        let _ = path.conn.send_datagram(refused_frame(&mut link));
    }
    let heard = bob.drain(Duration::from_millis(500)).await;
    assert_eq!(errors(&heard), []);
}

#[tokio::test]
async fn frames_of_a_kicked_player_are_not_answered_over_webtransport() {
    let node = Node::start().await;
    let project = node.project("datapath").await;
    let mut ann = project.player("ann").await;
    joined(&ann.call(json!({"t": "create", "name": "room"})).await);
    let mut bob = project.player("bob").await;
    let reply = bob.call(json!({"t": "join", "name": "room"})).await;
    let actor = joined(&reply);
    let data = &reply["data"];
    let endpoint = net_transport::webtransport::client_endpoint(
        data["cert_sha256"].as_str().expect("cert_sha256"),
    )
    .expect("endpoint");
    let conn = endpoint
        .connect(data["webtransport_url"].as_str().expect("webtransport_url"))
        .await
        .expect("webtransport session");
    conn.send_datagram(ticket(&reply)).expect("send");
    let mut link = Link::new(Instant::now(), IDLE);
    let opened = async {
        loop {
            let datagram = conn.receive_datagram().await.expect("the path closed");
            let messages = link.receive(datagram.payload(), Instant::now());
            if messages.iter().any(|m| m.payload.is_empty()) {
                return;
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(3), opened)
        .await
        .expect("the path did not open");

    ann.send(json!({"t": "kick", "actor": actor})).await;
    bob.until("left").await;
    for _ in 0..8 {
        let _ = conn.send_datagram(refused_frame(&mut link));
    }
    let heard = bob.drain(Duration::from_millis(500)).await;
    assert_eq!(errors(&heard), []);
}

#[tokio::test]
async fn frames_the_node_cannot_read_are_answered_at_a_bounded_rate() {
    let node = Node::start_with(&[("NET__LIMITS__FRAME_ERRORS_PER_SEC_PER_PLAYER", "2")]).await;
    let project = node.project("datapath").await;
    let mut ann = project.player("ann").await;
    let reply = ann.call(json!({"t": "create", "name": "room"})).await;
    joined(&reply);
    ann.send_binary(ticket(&reply)).await;
    let mut link = Link::new(Instant::now(), IDLE);

    // as many as the link sends before it waits for an acknowledgement
    for _ in 0..30 {
        ann.send_binary(refused_frame(&mut link).to_vec()).await;
    }
    let heard = ann.drain(Duration::from_millis(500)).await;
    // a burst of twice the limit and what half a second adds to it
    let answered = errors(&heard).len();
    assert!((4..=6).contains(&answered), "{answered} answers");
}

/// The node closes the path of a player it took out of the room; an acknowledgement that
/// leaves before the close tells him the packet was taken. WebTransport lets it out in most
/// rounds, hence several.
#[tokio::test]
async fn packet_of_a_kicked_player_is_not_acknowledged_over_webtransport() {
    let node = Node::start().await;
    let project = node.project("datapath").await;
    let mut ann = project.player("ann").await;
    joined(&ann.call(json!({"t": "create", "name": "room"})).await);
    for round in 0..8 {
        let mut bob = project.player(&format!("bob-{round}")).await;
        let reply = bob.call(json!({"t": "join", "name": "room"})).await;
        let actor = joined(&reply);
        let data = &reply["data"];
        let endpoint = net_transport::webtransport::client_endpoint(
            data["cert_sha256"].as_str().expect("cert_sha256"),
        )
        .expect("endpoint");
        let conn = endpoint
            .connect(data["webtransport_url"].as_str().expect("webtransport_url"))
            .await
            .expect("webtransport session");
        conn.send_datagram(ticket(&reply)).expect("send");
        let mut link = Link::new(Instant::now(), IDLE);
        let opened = async {
            loop {
                let datagram = conn.receive_datagram().await.expect("the path closed");
                let messages = link.receive(datagram.payload(), Instant::now());
                if messages.iter().any(|m| m.payload.is_empty()) {
                    return;
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(3), opened)
            .await
            .expect("the path did not open");

        ann.send(json!({"t": "kick", "actor": actor})).await;
        bob.until("left").await;
        let _ = conn.send_datagram(packet(&mut link, b"late"));
        let wait = Duration::from_millis(500);
        while let Ok(Ok(datagram)) = tokio::time::timeout(wait, conn.receive_datagram()).await {
            link.receive(datagram.payload(), Instant::now());
        }

        // The link repeats what is not acknowledged.
        link.poll(Instant::now() + 2 * link.rto()).expect("poll");
        assert_eq!(link.stats().retransmits, 1, "round {round}");
    }
}

/// Takes what the node sent into the link and sends what the link has to send.
fn pump(path: &Path, link: &mut Link) {
    while let Some(Ok(datagram)) = path.conn.read_datagram().now_or_never() {
        link.receive(datagram, Instant::now());
    }
    let _ = link.poll(Instant::now());
    while let Some(datagram) = link.next_outgoing() {
        let _ = path.conn.send_datagram(datagram);
    }
}

/// The relay events `client` has got since the last call.
fn events(client: &mut net_client::Client) -> u64 {
    std::iter::from_fn(|| client.try_next_event())
        .filter(|event| matches!(event, net_client::Event::Data { .. }))
        .count() as u64
}

/// The reliable events the node has relayed.
async fn relayed(node: &Node) -> u64 {
    let metrics = reqwest::get(format!("{}/metrics", node.http))
        .await
        .expect("metrics")
        .text()
        .await
        .expect("metrics text");
    metrics
        .lines()
        .find_map(|l| l.strip_prefix("net_events_total{channel=\"reliable_ordered\"} "))
        .map_or(0, |count| count.parse().expect("count"))
}

const NO_EVENT_LIMIT: &[(&str, &str)] = &[
    ("NET__LIMITS__MAX_EVENTS_PER_SEC_PER_PLAYER", "1000000"),
    ("NET__LIMITS__MAX_EVENTS_PER_SEC_PER_ROOM", "1000000"),
];

/// ann sends reliable events faster than they get through, the node hands its rooms off
/// after 2000 of them, and she goes on until she is told to leave. Returns how many events
/// she holds acknowledged and how many bob got.
async fn flood_through_a_handoff(node: &Node, stop: bool) -> (u64, u64) {
    use net_client::{Client, Options};
    use net_rooms::event::{CacheOp, EventFrame, EventTarget};
    let project = node.project("datapath").await;
    let mut ann = project.player("ann").await;
    let reply = ann.call(json!({"t": "create", "name": "room"})).await;
    let actor = net_common::ActorId(joined(&reply) as u16);
    let path = connect(&reply).await;
    let mut link = Link::new(Instant::now(), IDLE);
    path.conn.send_datagram(path.ticket.clone()).expect("send");
    assert!(path.opened(&mut link).await, "the path did not open");
    let mut bob = Client::connect(&node.http, &project.grant("bob").await, Options::default())
        .await
        .expect("bob connects");
    let join = serde_json::from_value(json!({"t": "join", "name": "room"})).expect("join");
    bob.join(join).await.expect("bob joins");

    let drain = project
        .operator
        .post("/v1/internal/drain", json!({"draining": true}));
    tokio::pin!(drain);
    let (mut sent, mut got) = (0u32, 0);
    let (mut asked, mut drained, mut left) = (false, false, false);
    while !left {
        for _ in 0..4 {
            let event = EventFrame {
                code: 1,
                target: EventTarget::Others,
                cache: CacheOp::None,
                group: None,
                from: actor,
                payload: Bytes::copy_from_slice(&sent.to_be_bytes()),
            };
            link.send(Channel::ReliableOrdered, event.encode(), Instant::now())
                .expect("send");
            sent += 1;
        }
        pump(&path, &mut link);
        got += events(&mut bob);
        if sent >= 2000 && stop && !asked {
            node.stop();
        }
        asked = sent >= 2000;
        tokio::select! {
            biased;
            _ = &mut drain, if asked && !stop && !drained => drained = true,
            message = ann.poll(Duration::from_micros(200)) => {
                left = message.is_some_and(|message| message["t"] == "left");
            }
        }
    }
    let settled = Instant::now() + Duration::from_millis(500);
    while Instant::now() < settled {
        pump(&path, &mut link);
        got += events(&mut bob);
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    if !stop && !drained {
        drain.await;
    }
    (u64::from(sent) - link.unacknowledged() as u64, got)
}

#[tokio::test]
async fn events_the_node_acknowledged_have_reached_the_room_when_it_is_handed_off() {
    let node = Node::start_with(NO_EVENT_LIMIT).await;
    let (acknowledged, got) = flood_through_a_handoff(&node, false).await;
    let took = relayed(&node).await;
    assert_eq!((acknowledged, got), (took, took));
}

#[tokio::test]
async fn events_the_node_acknowledged_have_reached_the_room_when_the_node_stops() {
    let node = Node::start_with(NO_EVENT_LIMIT).await;
    let (acknowledged, got) = flood_through_a_handoff(&node, true).await;
    assert_eq!(acknowledged, got);
}

#[tokio::test]
async fn player_who_acknowledges_nothing_holds_a_stopping_node_for_half_its_time() {
    let node = Node::start_with(&[("NET__SERVER__SHUTDOWN_SECS", "4")]).await;
    let project = node.project("datapath").await;
    let mut ann = project.player("ann").await;
    joined(&ann.call(json!({"t": "create", "name": "room"})).await);
    let mut bob = project.player("bob").await;
    let reply = bob.call(json!({"t": "join", "name": "room"})).await;
    joined(&reply);
    let path = connect(&reply).await;
    let mut link = Link::new(Instant::now(), IDLE);
    path.conn.send_datagram(path.ticket.clone()).expect("send");
    assert!(path.opened(&mut link).await, "the path did not open");
    // bob's link sends nothing from here on: what the node sends him stays unacknowledged
    ann.send(json!({"t": "event", "code": 1, "data": "AQI=", "target": {"kind": "others"}}))
        .await;

    let asked = Instant::now();
    node.stop();
    assert_eq!(ann.until("left").await["reason"], "rehost");
    let waited = asked.elapsed();
    assert!(
        (Duration::from_millis(1900)..Duration::from_secs(4)).contains(&waited),
        "{waited:?}"
    );
    let logged = node.logged();
    assert!(
        logged
            .iter()
            .any(|line| line["message"] == "handoff: events not acknowledged in time"),
        "{}",
        node.log_tail()
    );
}

#[tokio::test]
async fn event_on_the_control_channel_is_refused_once_the_room_takes_no_more() {
    let node = Node::start_with(&[("NET__SERVER__SHUTDOWN_SECS", "4")]).await;
    let project = node.project("datapath").await;
    let mut ann = project.player("ann").await;
    joined(&ann.call(json!({"t": "create", "name": "room"})).await);
    let mut bob = project.player("bob").await;
    let reply = bob.call(json!({"t": "join", "name": "room"})).await;
    joined(&reply);
    let path = connect(&reply).await;
    let mut link = Link::new(Instant::now(), IDLE);
    path.conn.send_datagram(path.ticket.clone()).expect("send");
    assert!(path.opened(&mut link).await, "the path did not open");
    // bob acknowledges nothing: the node waits for him for two seconds before it lets go
    let event = json!({"t": "event", "code": 1, "data": "AQI=", "target": {"kind": "others"}});
    ann.send(event.clone()).await;

    node.stop();
    ann.drain(Duration::from_millis(500)).await;
    let reply = ann.call(event).await;
    refused(&reply, "conflict");
    assert_eq!(reply["message"], "conflict: node_draining");
}
