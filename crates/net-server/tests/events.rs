//! The per-player event limit: a host relaying an engine's packets to every client stays within
//! it, a player flooding one receiver does not and is told so without losing his connection.

mod common;

use bytes::Bytes;
use common::Node;
use net_client::{Client, Event, Options};
use net_rooms::event::EventTarget;
use net_rooms::protocol::{ClientMsg, ServerMsg};
use serde_json::json;
use std::time::Duration;

const CLIENTS: usize = 16;
const SECONDS: u32 = 4;
/// What the Unreal driver was measured to send one peer at a 30 Hz net tick, acks included.
const PER_PEER_HZ: u32 = 75;

fn msg(json: serde_json::Value) -> ClientMsg {
    serde_json::from_value(json).expect("client message")
}

#[derive(Default)]
struct Seen {
    events: usize,
    refused: usize,
}

fn drain(client: &mut Client, seen: &mut Seen) {
    while let Some(event) = client.try_next_event() {
        match event {
            Event::Data { .. } => seen.events += 1,
            Event::Control(ServerMsg::Error { code, .. }) if code == "rate_limited" => {
                seen.refused += 1
            }
            _ => {}
        }
    }
}

async fn connect(node: &Node, grant: &str) -> Client {
    let opts = Options {
        tunnel: true,
        ..Default::default()
    };
    Client::connect(&node.http, grant, opts)
        .await
        .expect("client connects")
}

#[tokio::test]
async fn host_and_sixteen_clients_exchange_engine_packets_without_loss() {
    let node = Node::start().await;
    let project = node.project("relay-host").await;
    let mut host = connect(&node, &project.grant("host").await).await;
    let host_actor = host
        .join(msg(
            json!({"t": "create", "name": "arena", "max_players": 32}),
        ))
        .await
        .expect("host creates")
        .actor;
    let mut clients = Vec::new();
    for index in 0..CLIENTS {
        let grant = project.grant(&format!("client{index}")).await;
        let mut client = connect(&node, &grant).await;
        let actor = client
            .join(msg(json!({"t": "join", "name": "arena"})))
            .await
            .expect("client joins")
            .actor;
        clients.push((client, actor, Seen::default()));
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut host_seen = Seen::default();
    drain(&mut host, &mut host_seen);
    host_seen.events = 0;
    for (client, _, seen) in &mut clients {
        drain(client, seen);
        seen.events = 0;
    }

    let packet = Bytes::from_static(&[0u8; 200]);
    let rounds = SECONDS * PER_PEER_HZ;
    let mut tick = tokio::time::interval(Duration::from_secs(1) / PER_PEER_HZ);
    for _ in 0..rounds {
        tick.tick().await;
        for (client, actor, seen) in &mut clients {
            host.send_event(1, EventTarget::Actors(vec![*actor]), packet.clone(), true)
                .expect("host sends");
            client
                .send_event(
                    1,
                    EventTarget::Actors(vec![host_actor]),
                    packet.clone(),
                    true,
                )
                .expect("client sends");
            client.flush();
            drain(client, seen);
        }
        host.flush();
        drain(&mut host, &mut host_seen);
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    drain(&mut host, &mut host_seen);
    let mut refused = host_seen.refused;
    let mut delivered = 0;
    for (client, _, seen) in &mut clients {
        drain(client, seen);
        refused += seen.refused;
        delivered += seen.events;
    }
    let sent = rounds as usize * CLIENTS;
    println!(
        "host: sent {sent}, refused {}, delivered to clients {delivered}; clients: sent {sent}, refused {}, delivered to host {}",
        host_seen.refused,
        refused - host_seen.refused,
        host_seen.events
    );
    assert_eq!(refused, 0, "events the node refused");
    assert_eq!(delivered, sent, "host packets that reached the clients");
    assert_eq!(
        host_seen.events, sent,
        "client packets that reached the host"
    );
}

#[tokio::test]
async fn player_flooding_one_receiver_is_cut_off() {
    let node = Node::start().await;
    let project = node.project("flood").await;
    let mut flooder = connect(&node, &project.grant("flooder").await).await;
    flooder
        .join(msg(json!({"t": "create", "name": "arena"})))
        .await
        .expect("flooder creates");
    let mut victim = connect(&node, &project.grant("victim").await).await;
    let victim_actor = victim
        .join(msg(json!({"t": "join", "name": "arena"})))
        .await
        .expect("victim joins")
        .actor;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (mut flooder_seen, mut victim_seen) = (Seen::default(), Seen::default());
    drain(&mut flooder, &mut flooder_seen);
    drain(&mut victim, &mut victim_seen);
    victim_seen.events = 0;

    const SENT: usize = 1000;
    let packet = Bytes::from_static(&[0u8; 200]);
    for _ in 0..SENT / 50 {
        for _ in 0..50 {
            flooder
                .send_event(
                    1,
                    EventTarget::Actors(vec![victim_actor]),
                    packet.clone(),
                    true,
                )
                .expect("flooder sends");
        }
        flooder.flush();
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    drain(&mut flooder, &mut flooder_seen);
    drain(&mut victim, &mut victim_seen);
    // The default is 120 a second with two seconds' worth in hand; the flood lasts under a second.
    assert!(
        (240..400).contains(&victim_seen.events),
        "{} of {SENT} events reached the victim",
        victim_seen.events
    );
    assert!(flooder_seen.refused > 0, "the flooder was told nothing");
}

#[tokio::test]
async fn event_over_the_limit_is_dropped_and_the_connection_stays() {
    let node = Node::start().await;
    let project = node.project("flood-json").await;
    let mut flooder = project.player("flooder").await;
    common::joined(&flooder.call(json!({"t": "create", "name": "arena"})).await);
    for _ in 0..300 {
        flooder
            .send(json!({"t": "event", "code": 1, "data": "AQI=", "target": {"kind": "others"}}))
            .await;
    }
    common::refused(&flooder.until("error").await, "rate_limited");
    flooder.send(json!({"t": "ping", "ts": 1})).await;
    flooder.until("pong").await;
}
