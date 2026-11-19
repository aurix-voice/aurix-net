//! Inputs that reach a `sync` room behind its tick show up in the node's metrics and in the
//! report the room sends their sender, and still run.

mod common;

use base64::Engine;
use bytes::{BufMut, Bytes, BytesMut};
use common::{Node, Project};
use net_client::{Client, Event, Options};
use net_common::Channel;
use net_replication::Replicator;
use net_rooms::protocol::ClientMsg;
use net_sim::{Frame, FrameConfig};
use net_transport::{quic, Link};
use net_wire::Schema;
use serde_json::{json, Value};
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

/// Applies the deltas `client` receives to `world` until `done` holds.
async fn follow(client: &mut Client, world: &mut Frame, done: impl Fn(&Frame) -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !done(world) {
        let event = tokio::time::timeout_at(deadline, client.next_event())
            .await
            .expect("the room's deltas did not get there")
            .expect("the client is connected");
        if let Event::Data { payload, .. } = event {
            if payload.first() == Some(&0x11) {
                Replicator::apply(world, &payload[1..]).expect("delta");
            }
        }
    }
}

#[tokio::test]
async fn spawn_sent_for_a_tick_already_simulated_happens_once() {
    let node = Node::start().await;
    let project = node.project("late-spawn").await;
    let (mut ann, mut world) = enter(&node, &project, create_arena()).await;
    // The avatar, and a room old enough to be five ticks behind of.
    follow(&mut ann, &mut world, |world| {
        world.entities().count() == 1 && world.tick.0 > 5
    })
    .await;

    // Five ticks behind the room, as after a stall of the client; sent twice, as a client
    // repeats its last inputs.
    let behind = world.tick.0 - 5;
    for _ in 0..2 {
        ann.send_data(Channel::UnreliableSequenced, spawn_frame(behind))
            .expect("ann sends");
        ann.flush();
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    follow(&mut ann, &mut world, |world| world.entities().count() == 2).await;
    // Half a second of ticks later the repeat has not made a third.
    let seen = world.tick.0;
    follow(&mut ann, &mut world, |world| world.tick.0 >= seen + 15).await;
    assert_eq!(world.entities().count(), 2);
}

/// One input frame (`0x10`) for `tick` whose input is a single tagged spawn op with `writes`
/// (`[0]`: none).
fn tagged_spawn_frame(tick: u32, tag: u32, writes: &[u8]) -> Bytes {
    let mut frame = BytesMut::new();
    frame.put_u8(0x10);
    frame.put_u32(tick);
    frame.put_u8(1);
    frame.put_u8(7 + writes.len() as u8);
    frame.put_slice(&[0xFF, 1, 7]);
    frame.put_u32(tag);
    frame.put_slice(writes);
    frame.freeze()
}

/// Ann's client in the `sync` room "arena", entered with `message`, and an empty mirror of the
/// room's world.
async fn enter(node: &Node, project: &Project, message: Value) -> (Client, Frame) {
    let opts = Options {
        tunnel: true,
        ..Default::default()
    };
    let mut ann = Client::connect(&node.http, &project.grant("ann").await, opts)
        .await
        .expect("ann connects");
    ann.join(serde_json::from_value(message).expect("client message"))
        .await
        .expect("ann enters");
    let config = FrameConfig {
        tick_hz: 30,
        ..Default::default()
    };
    let world = Frame::new(Schema::parse(POSE_SCHEMA).expect("schema"), config, 0);
    (ann, world)
}

fn create_arena() -> Value {
    let props = json!({"tick_hz": 30, "schema": POSE_SCHEMA});
    json!({"t": "create", "name": "arena", "mode": "sync", "props": props})
}

fn ids(world: &Frame) -> Vec<u64> {
    let mut ids: Vec<u64> = world.entities().map(|e| e.id.to_u64()).collect();
    ids.sort_unstable();
    ids
}

/// Follows the room until it has answered `tag` (`0x14`) and `world` is past the tick of the
/// answer, so that what the op did is in it. Returns (status, entity).
async fn answer(ann: &mut Client, world: &mut Frame, tag: u32) -> (u8, u64) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut answered = None;
    loop {
        if let Some((status, entity, tick)) = answered {
            if world.tick.0 > tick {
                return (status, entity);
            }
        }
        let event = tokio::time::timeout_at(deadline, ann.next_event())
            .await
            .unwrap_or_else(|_| panic!("no answer to tag {tag}, or no delta after it"))
            .expect("ann is connected");
        let Event::Data { payload, .. } = event else {
            continue;
        };
        match payload.first() {
            Some(0x11) => {
                Replicator::apply(world, &payload[1..]).expect("delta");
            }
            Some(0x14) => {
                assert_eq!(payload.len(), 18);
                let field = |at: std::ops::Range<usize>| payload[at].try_into().expect("field");
                assert_eq!(
                    u32::from_be_bytes(field(1..5)),
                    tag,
                    "an answer not asked for"
                );
                let entity = u64::from_be_bytes(payload[6..14].try_into().expect("entity"));
                answered = Some((payload[5], entity, u32::from_be_bytes(field(14..18))));
            }
            _ => {}
        }
    }
}

/// Sends a spawn under `tag` a few ticks ahead and returns the room's answer: (status, entity).
async fn spawn_tagged(ann: &mut Client, world: &mut Frame, tag: u32, writes: &[u8]) -> (u8, u64) {
    let ahead = world.tick.0 + 6;
    let frame = tagged_spawn_frame(ahead, tag, writes);
    ann.send_data(Channel::UnreliableSequenced, frame)
        .expect("ann sends");
    ann.flush();
    answer(ann, world, tag).await
}

#[tokio::test]
async fn tagged_spawn_is_answered_with_its_entity_and_repeats_create_nothing() {
    let node = Node::start().await;
    let project = node.project("tagged-spawn").await;
    let (mut ann, mut world) = enter(&node, &project, create_arena()).await;
    // The avatar is announced under tag 0.
    let (_, avatar) = answer(&mut ann, &mut world, 0).await;
    assert_eq!(ids(&world), [avatar]);

    // The same op in three inputs for three ticks: what a client does when no answer came.
    let (status, spawned) = spawn_tagged(&mut ann, &mut world, 5, &[0]).await;
    assert_eq!(status, 0);
    assert_ne!(spawned, avatar);
    for _ in 0..2 {
        let again = spawn_tagged(&mut ann, &mut world, 5, &[0]).await;
        assert_eq!(again, (0, spawned));
    }
    // One write announced and none sent: the op is invalid, and the client is told.
    let cut_short = spawn_tagged(&mut ann, &mut world, 9, &[1]).await;
    assert_eq!(cut_short, (2, 0));
    let mut both = [avatar, spawned];
    both.sort_unstable();
    assert_eq!(ids(&world), both);

    // A second connection of Ann is told where her tags go on; a newcomer starts from 1.
    let join = json!({"t": "join", "name": "arena"});
    let mut second = project.player("ann").await;
    assert_eq!(second.call(join.clone()).await["next_spawn_tag"], 6);
    let mut bob = project.player("bob").await;
    assert_eq!(bob.call(join).await["next_spawn_tag"], 1);
    // A room without a schema runs the built-in logic, which takes no ops.
    let plain = json!({"t": "create", "name": "plain", "mode": "sync"});
    let reply = project.player("cid").await.call(plain).await;
    assert_eq!(
        (&reply["t"], &reply["next_spawn_tag"]),
        (&json!("joined"), &Value::Null)
    );
}

/// One-second heartbeats: a node that is silent for three seconds is taken for dead.
const FAST: &[(&str, &str)] = &[("NET__SERVER__HEARTBEAT_SECS", "1")];

#[tokio::test]
async fn tagged_spawn_repeated_after_the_room_moved_creates_nothing() {
    let node_a = Node::start_with(FAST).await;
    let node_b = Node::start_with(FAST).await;
    let pa = node_a.project("tag-moved").await;
    let pb = node_b.same_project(&pa);
    let (mut ann, mut world) = enter(&node_a, &pa, create_arena()).await;
    let (_, avatar) = answer(&mut ann, &mut world, 0).await;
    let spawned = spawn_tagged(&mut ann, &mut world, 5, &[0]).await;
    assert_eq!(spawned.0, 0);
    // A sync room is checkpointed every two seconds.
    tokio::time::sleep(Duration::from_secs(3)).await;

    // A falls silent; Ann comes back through B, which continues the room from the checkpoint.
    let frozen = node_a.freeze(Duration::from_secs(8));
    let back = async {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let join = json!({"t": "join", "name": "arena"});
        let (mut ann, mut world) = enter(&node_b, &pb, join).await;
        assert_eq!(answer(&mut ann, &mut world, 0).await, (0, avatar));
        let again = spawn_tagged(&mut ann, &mut world, 5, &[0]).await;
        assert_eq!(again, spawned);
        let mut both = [avatar, spawned.1];
        both.sort_unstable();
        assert_eq!(ids(&world), both);
    };
    tokio::join!(frozen, back);
}

#[tokio::test]
async fn client_whose_data_path_opens_late_gets_the_whole_world() {
    let node = Node::start().await;
    let project = node.project("late-path").await;
    let mut ann = project.player("ann").await;
    let reply = ann.call(create_arena()).await;
    assert_eq!(reply["t"], "joined", "{reply}");
    // The room ticks for a while before the client has a data path for its deltas.
    tokio::time::sleep(Duration::from_millis(300)).await;

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
    conn.send_datagram(Bytes::from(ticket)).expect("ticket");

    // A client that never acks has only what the deltas bring.
    let config = FrameConfig {
        tick_hz: 30,
        ..Default::default()
    };
    let mut world = Frame::new(Schema::parse(POSE_SCHEMA).expect("schema"), config, 0);
    let mut link = Link::new(Instant::now());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while world.entities().count() == 0 {
        let datagram = tokio::time::timeout_at(deadline, conn.read_datagram())
            .await
            .expect("the avatar is in no delta")
            .expect("the path closed");
        for message in link.receive(datagram, Instant::now()) {
            if message.payload.first() == Some(&0x11) {
                Replicator::apply(&mut world, &message.payload[1..]).expect("delta");
            }
        }
    }
}
