//! Inputs that reach a `sync` room behind its tick show up in the node's metrics and in the
//! report the room sends their sender, and still run.

mod common;

use bytes::{BufMut, Bytes, BytesMut};
use common::Node;
use net_client::{Client, Event, Options};
use net_common::Channel;
use net_replication::Replicator;
use net_rooms::protocol::ClientMsg;
use net_sim::{Frame, FrameConfig};
use net_wire::Schema;
use serde_json::json;
use std::time::{Duration, Instant};

/// One input frame (`0x10`) holding a single input for `tick`.
fn input_frame(tick: u32) -> Bytes {
    let mut frame = BytesMut::new();
    frame.put_u8(0x10);
    frame.put_u32(tick);
    frame.put_u8(1);
    frame.put_u8(3);
    frame.put_slice(&[127, 0, 0]);
    frame.freeze()
}

async fn late_inputs(node: &Node) -> u64 {
    let metrics = reqwest::get(format!("{}/metrics", node.http))
        .await
        .expect("metrics")
        .text()
        .await
        .expect("metrics text");
    metrics
        .lines()
        .find_map(|line| line.strip_prefix("net_sync_late_inputs_total "))
        .map_or(0, |count| count.trim().parse().expect("a counter"))
}

#[tokio::test]
async fn inputs_behind_the_room_tick_are_counted() {
    let node = Node::start().await;
    let project = node.project("late-inputs").await;
    let opts = Options {
        tunnel: true,
        ..Default::default()
    };
    let mut client = Client::connect(&node.http, &project.grant("ann").await, opts)
        .await
        .expect("ann connects");
    let create: ClientMsg = serde_json::from_value(
        json!({"t": "create", "name": "arena", "mode": "sync", "props": {"tick_hz": 30}}),
    )
    .expect("client message");
    client.join(create).await.expect("ann creates");
    tokio::time::sleep(Duration::from_millis(500)).await;

    for _ in 0..5 {
        client
            .send_data(Channel::UnreliableSequenced, input_frame(1))
            .expect("ann sends");
        client.flush();
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while late_inputs(&node).await < 5 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(late_inputs(&node).await, 5);
}

/// The input reports (`0x25`: tick, least margin, late frames) `client` has received.
fn input_reports(client: &mut Client) -> Vec<(u32, i8, u8)> {
    let mut reports = Vec::new();
    while let Some(event) = client.try_next_event() {
        if let Event::Data { payload, .. } = event {
            if payload.len() == 7 && payload[0] == 0x25 {
                let tick = u32::from_be_bytes(payload[1..5].try_into().expect("four bytes"));
                reports.push((tick, payload[5] as i8, payload[6]));
            }
        }
    }
    reports
}

#[tokio::test]
async fn sender_of_late_inputs_is_told_how_late_they_are() {
    let node = Node::start().await;
    let project = node.project("input-reports").await;
    let join: ClientMsg = serde_json::from_value(
        json!({"t": "join_or_create", "name": "arena", "mode": "sync", "props": {"tick_hz": 30}}),
    )
    .expect("client message");
    let mut clients = Vec::new();
    for name in ["ann", "bob"] {
        let opts = Options {
            tunnel: true,
            ..Default::default()
        };
        let mut client = Client::connect(&node.http, &project.grant(name).await, opts)
            .await
            .expect("connects");
        client.join(join.clone()).await.expect("joins");
        clients.push(client);
    }
    let [ann, bob] = &mut clients[..] else {
        unreachable!()
    };
    tokio::time::sleep(Duration::from_millis(500)).await;

    for _ in 0..60 {
        ann.send_data(Channel::UnreliableSequenced, input_frame(1))
            .expect("ann sends");
        ann.flush();
        tokio::time::sleep(Duration::from_millis(33)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;

    let reports = input_reports(ann);
    assert!(!reports.is_empty(), "ann got no report");
    for (tick, margin, late) in reports {
        assert_eq!(tick % 30, 0, "a report every 30 ticks");
        assert!(margin < 0 && late > 0, "margin {margin}, late {late}");
    }
    assert_eq!(input_reports(bob), [], "bob sent no input");
}

const POSE_SCHEMA: &str = "component Pose {\n    pos: vec3 @range(-512, 512, 0.01)\n}\n";

/// One input frame (`0x10`) for `tick` whose input is a single spawn op.
fn spawn_frame(tick: u32) -> Bytes {
    let mut frame = BytesMut::new();
    frame.put_u8(0x10);
    frame.put_u32(tick);
    frame.put_u8(1);
    frame.put_u8(4);
    frame.put_slice(&[0xFF, 1, 1, 0]);
    frame.freeze()
}

/// Applies the deltas `client` has received to `world` and returns how many entities it holds.
fn entities(client: &mut Client, world: &mut Frame) -> usize {
    while let Some(event) = client.try_next_event() {
        if let Event::Data { payload, .. } = event {
            if payload.first() == Some(&0x11) {
                Replicator::apply(world, &payload[1..]).expect("delta");
            }
        }
    }
    world.entities().count()
}

#[tokio::test]
async fn spawn_sent_for_a_tick_already_simulated_happens_once() {
    let node = Node::start().await;
    let project = node.project("late-spawn").await;
    let opts = Options {
        tunnel: true,
        ..Default::default()
    };
    let mut ann = Client::connect(&node.http, &project.grant("ann").await, opts)
        .await
        .expect("ann connects");
    let props = json!({"tick_hz": 30, "schema": POSE_SCHEMA});
    let create = json!({"t": "create", "name": "arena", "mode": "sync", "props": props});
    ann.join(serde_json::from_value(create).expect("client message"))
        .await
        .expect("ann creates");
    let config = FrameConfig {
        tick_hz: 30,
        ..Default::default()
    };
    let mut world = Frame::new(Schema::parse(POSE_SCHEMA).expect("schema"), config, 0);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(entities(&mut ann, &mut world), 1, "the avatar");

    // Five ticks behind the room, as after a stall of the client; sent twice, as a client
    // repeats its last inputs.
    let behind = world.tick.0 - 5;
    for _ in 0..2 {
        ann.send_data(Channel::UnreliableSequenced, spawn_frame(behind))
            .expect("ann sends");
        ann.flush();
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(entities(&mut ann, &mut world), 2);
}
