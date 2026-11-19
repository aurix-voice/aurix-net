//! Opening the QUIC data path of a running node when datagrams are lost or repeated.

mod common;

use base64::Engine;
use bytes::Bytes;
use common::{joined, Node};
use net_common::Channel;
use net_transport::{quic, Link};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

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
    let mut link = Link::new(Instant::now());

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
    let mut link = Link::new(Instant::now());

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
        !stale.opened(&mut Link::new(Instant::now())).await,
        "the replaced session got the data path"
    );

    let path = connect(&new_reply).await;
    path.conn.send_datagram(path.ticket.clone()).expect("send");
    assert!(
        path.opened(&mut Link::new(Instant::now())).await,
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
    let mut link = Link::new(Instant::now());
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
    let mut link = Link::new(Instant::now());
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
