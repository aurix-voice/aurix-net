//! A `lockstep` room confirms the newest input of a frame that arrives behind its tick.

mod common;

use bytes::{BufMut, Bytes, BytesMut};
use common::Node;
use net_api::lockstep::ServerSim;
use net_api::sync::ARENA_SCHEMA;
use net_client::lockstep::{LockstepClient, LsEvent};
use net_client::{Client, Event, Options};
use net_common::{ActorId, Channel};
use net_game::GameEngine;
use net_rooms::protocol::{ClientMsg, ServerMsg};
use net_sim::{Frame, FrameConfig};
use net_wire::Schema;
use serde_json::json;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// One input frame (`0x10`) with a one-byte input for each tick from `first`.
fn input_frame(first: u32, inputs: &[u8]) -> Bytes {
    let mut frame = BytesMut::new();
    frame.put_u8(0x10);
    frame.put_u32(first);
    frame.put_u8(inputs.len() as u8);
    for input in inputs {
        frame.put_u8(1);
        frame.put_u8(*input);
    }
    frame.freeze()
}

/// The inputs of `actor` in the bundles `client` has received, in tick order.
fn confirmed_inputs(client: &mut Client, bundles: &mut LockstepClient, actor: u16) -> Vec<Bytes> {
    let mut confirmed = Vec::new();
    while let Some(event) = client.try_next_event() {
        let Event::Data { payload, .. } = event else {
            continue;
        };
        for event in bundles.receive(&payload).unwrap_or_default() {
            if let LsEvent::Tick { inputs, .. } = event {
                confirmed.extend(
                    inputs
                        .get(&actor)
                        .filter(|input| !input.is_empty())
                        .cloned(),
                );
            }
        }
    }
    confirmed
}

#[tokio::test]
async fn late_frame_has_its_newest_input_confirmed_once() {
    let node = Node::start().await;
    let project = node.project("late-frame").await;
    let opts = Options {
        tunnel: true,
        ..Default::default()
    };
    let mut ann = Client::connect(&node.http, &project.grant("ann").await, opts)
        .await
        .expect("ann connects");
    let create: ClientMsg = serde_json::from_value(
        json!({"t": "create", "name": "arena", "mode": "lockstep", "props": {"tick_hz": 30}}),
    )
    .expect("client message");
    let actor = ann.join(create).await.expect("ann creates").actor.0;
    tokio::time::sleep(Duration::from_millis(500)).await;

    ann.send_data(Channel::UnreliableSequenced, input_frame(1, &[1, 2, 3]))
        .expect("ann sends");
    ann.flush();

    let mut bundles = LockstepClient::new();
    let mut confirmed = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    while confirmed.is_empty() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
        confirmed.extend(confirmed_inputs(&mut ann, &mut bundles, actor));
    }
    // Whatever else the room confirms for ann arrives within the next few ticks.
    tokio::time::sleep(Duration::from_millis(300)).await;
    confirmed.extend(confirmed_inputs(&mut ann, &mut bundles, actor));
    assert_eq!(confirmed, [Bytes::from_static(&[3])]);
}

/// An ack frame (`0x29`): the tick the client needs next.
fn ack_frame(next: u32) -> Bytes {
    let mut frame = BytesMut::new();
    frame.put_u8(0x29);
    frame.put_u32(next);
    frame.freeze()
}

#[tokio::test]
async fn client_that_loses_bundles_in_a_row_keeps_getting_ticks() {
    let node = Node::start().await;
    let project = node.project("lost-bundles").await;
    let opts = Options {
        tunnel: true,
        ..Default::default()
    };
    let mut ann = Client::connect(&node.http, &project.grant("ann").await, opts)
        .await
        .expect("ann connects");
    let create: ClientMsg = serde_json::from_value(
        json!({"t": "create", "name": "arena", "mode": "lockstep", "props": {"tick_hz": 60}}),
    )
    .expect("client message");
    ann.join(create).await.expect("ann creates");

    // Every tenth datagram is lost, then five in a row, then more than one datagram can make up.
    let lost = |datagram: u32| {
        datagram % 10 == 9 || (100..105).contains(&datagram) || (200..400).contains(&datagram)
    };
    let mut bundles = LockstepClient::new();
    let mut datagrams = 0;
    let deadline = Instant::now() + Duration::from_secs(30);
    while datagrams < 500 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
        while let Some(event) = ann.try_next_event() {
            let Event::Data {
                channel, payload, ..
            } = event
            else {
                continue;
            };
            if channel == Channel::UnreliableSequenced && payload.first() == Some(&0x20) {
                datagrams += 1;
                if lost(datagrams) {
                    continue;
                }
                bundles.receive(&payload);
                ann.send_data(
                    Channel::UnreliableSequenced,
                    ack_frame(bundles.confirmed_tick()),
                )
                .expect("ann acks");
                ann.flush();
            } else {
                bundles.receive(&payload);
            }
        }
    }
    assert!(datagrams >= 500, "the room stopped sending");
    let behind = bundles.server_tick - bundles.confirmed_tick();
    assert!(
        behind <= 3,
        "ann is {behind} ticks behind the room at tick {}",
        bundles.server_tick
    );
}

fn hash_frame(tick: u32, hash: u64) -> Bytes {
    let mut frame = BytesMut::new();
    frame.put_u8(0x21);
    frame.put_u32(tick);
    frame.put_u64(hash);
    frame.freeze()
}

/// The control messages and reliable data frames `client` has received.
fn drain(client: &mut Client) -> (Vec<ServerMsg>, Vec<Bytes>) {
    let mut control = Vec::new();
    let mut reliable = Vec::new();
    while let Some(event) = client.try_next_event() {
        match event {
            Event::Control(message) => control.push(message),
            Event::Data {
                channel: Channel::ReliableOrdered,
                payload,
                ..
            } => reliable.push(payload),
            _ => {}
        }
    }
    (control, reliable)
}

#[tokio::test]
async fn desynced_client_is_sent_a_restore_frame_and_stays_trusted() {
    let node = Node::start().await;
    let project = node.project("restore").await;
    let message = |t: &str| -> ClientMsg {
        serde_json::from_value(
            json!({"t": t, "name": "arena", "mode": "lockstep", "props": {"tick_hz": 60}}),
        )
        .expect("client message")
    };
    let mut clients = Vec::new();
    for (player, t) in [("ann", "create"), ("bob", "join"), ("carol", "join")] {
        let opts = Options {
            tunnel: true,
            ..Default::default()
        };
        let mut client = Client::connect(&node.http, &project.grant(player).await, opts)
            .await
            .expect("connects");
        client.join(message(t)).await.expect("joins");
        clients.push(client);
    }
    tokio::time::sleep(Duration::from_secs(1)).await;
    for client in &mut clients {
        drain(client);
    }

    // Carol disagrees with the other two about tick 30.
    for (client, hash) in clients.iter().zip([7, 7, 8]) {
        client
            .send_data(Channel::UnreliableSequenced, hash_frame(30, hash))
            .expect("hash");
        client.flush();
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    let (control, reliable) = drain(&mut clients[2]);
    let strikes: Vec<(u8, bool)> = control
        .iter()
        .filter_map(|message| match message {
            ServerMsg::Desync {
                strikes, untrusted, ..
            } => Some((*strikes, *untrusted)),
            _ => None,
        })
        .collect();
    assert_eq!(strikes, [(1, false)]);
    let kinds: Vec<u8> = reliable.iter().map(|frame| frame[0]).collect();
    assert!(!kinds.contains(&0x24), "reliable frames: {kinds:?}");
    let at = kinds
        .iter()
        .position(|kind| *kind == 0x2A)
        .unwrap_or_else(|| panic!("no restore among the reliable frames: {kinds:?}"));
    // Restore: kind, u32 tick, u64 hash, u32 length, state; the bundles from that tick follow.
    let restore = &reliable[at];
    let tick = u32::from_be_bytes(restore[1..5].try_into().expect("tick"));
    let length = u32::from_be_bytes(restore[13..17].try_into().expect("length")) as usize;
    assert_eq!(restore.len(), 17 + length);
    let tail = &reliable[at + 1];
    assert_eq!((tail[0], tail[4]), (0x20, 0x20));
    assert_eq!(
        u32::from_be_bytes(tail[5..9].try_into().expect("tick")),
        tick
    );

    // Agreeing again, carol gets no further strike.
    for client in &clients {
        client
            .send_data(Channel::UnreliableSequenced, hash_frame(60, 9))
            .expect("hash");
        client.flush();
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    let (control, _) = drain(&mut clients[2]);
    assert!(
        !control
            .iter()
            .any(|message| matches!(message, ServerMsg::Desync { .. })),
        "{control:?}"
    );
}

const IDLE_MODULE: &str = "(module (memory (export \"memory\") 1) \
    (func (export \"aurix_init\") (param i64)) (func (export \"aurix_step\")))";

/// An honest client of a room with a referee: steps `sim` over the bundles it receives and
/// reports the hash of the state after every tenth tick under that tick, as `docs/sdk.md` says.
struct Mirror {
    sim: ServerSim,
    bundles: LockstepClient,
    reported: Vec<u32>,
    /// The state each tick starts from.
    states: BTreeMap<u32, Vec<u8>>,
    desyncs: usize,
    reliable: Vec<Bytes>,
}

impl Mirror {
    fn pump(&mut self, client: &mut Client) {
        while let Some(event) = client.try_next_event() {
            let payload = match event {
                Event::Data {
                    channel, payload, ..
                } => {
                    if channel == Channel::ReliableOrdered {
                        self.reliable.push(payload.clone());
                    }
                    payload
                }
                Event::Control(ServerMsg::Desync { .. }) => {
                    self.desyncs += 1;
                    continue;
                }
                _ => continue,
            };
            for event in self.bundles.receive(&payload).unwrap_or_default() {
                let LsEvent::Tick { tick, inputs } = event else {
                    continue;
                };
                let inputs = inputs
                    .into_iter()
                    .map(|(actor, input)| (ActorId(actor), input.to_vec()))
                    .collect();
                self.sim.step(&inputs);
                self.states.insert(tick + 1, self.sim.frame.snapshot());
                if tick.is_multiple_of(10) {
                    client
                        .send_data(
                            Channel::UnreliableSequenced,
                            hash_frame(tick, self.sim.hash()),
                        )
                        .expect("hash");
                    self.reported.push(tick);
                }
            }
            client.flush();
        }
    }
}

#[tokio::test]
async fn client_hashing_by_the_documented_rule_agrees_with_the_referee() {
    let node = Node::start().await;
    let project = node.project("referee").await;
    let upload = reqwest::Client::new()
        .put(format!("{}/v1/modules/idle", project.api.http))
        .bearer_auth(&project.api.token)
        .body(IDLE_MODULE)
        .send()
        .await
        .expect("upload");
    assert!(upload.status().is_success(), "upload: {}", upload.status());

    let opts = Options {
        tunnel: true,
        ..Default::default()
    };
    let mut ann = Client::connect(&node.http, &project.grant("ann").await, opts)
        .await
        .expect("connects");
    let props = json!({"tick_hz": 60, "logic": "idle"});
    let create = json!({"t": "create", "name": "arena", "mode": "lockstep", "props": props});
    ann.join(serde_json::from_value(create).expect("client message"))
        .await
        .expect("joins");
    // The seed is in `joined`, which `net-client` does not hand out.
    let mut bob = project.player("bob").await;
    bob.send(json!({"t": "join", "name": "arena"})).await;
    let seed = bob.until("joined").await["seed"].as_u64().expect("seed");

    let engine = GameEngine::new();
    let (_, module) = engine.load(IDLE_MODULE.as_bytes()).expect("module");
    let logic = engine.instantiate(&module, seed).expect("instance");
    let config = FrameConfig {
        tick_hz: 60,
        ..Default::default()
    };
    let schema = Schema::parse(ARENA_SCHEMA).expect("schema");
    let mut mirror = Mirror {
        sim: ServerSim::new(Frame::new(schema, config, seed), Box::new(logic)),
        bundles: LockstepClient::new(),
        reported: Vec::new(),
        states: BTreeMap::new(),
        desyncs: 0,
        reliable: Vec::new(),
    };
    // Past the first checkpoint of the referee, which it takes after 5 s.
    let deadline = Instant::now() + Duration::from_secs(15);
    while mirror.reported.last().is_none_or(|tick| *tick < 330) {
        assert!(Instant::now() < deadline, "reported: {:?}", mirror.reported);
        mirror.pump(&mut ann);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        mirror.desyncs, 0,
        "the node called an honest client desynced"
    );
    assert_eq!(mirror.reported[..3], [0, 10, 20]);

    // A wrong hash is a desync, and the state sent back is the one the checkpoint tick starts from.
    let last = *mirror.reported.last().expect("a reported tick");
    mirror.reliable.clear();
    ann.send_data(
        Channel::UnreliableSequenced,
        hash_frame(last, !mirror.sim.hash()),
    )
    .expect("hash");
    ann.flush();
    tokio::time::sleep(Duration::from_millis(500)).await;
    mirror.pump(&mut ann);
    assert_eq!(mirror.desyncs, 1);
    let restore = mirror
        .reliable
        .iter()
        .find(|frame| frame[0] == 0x2A)
        .expect("a restore frame");
    let tick = u32::from_be_bytes(restore[1..5].try_into().expect("tick"));
    assert_eq!(tick, 300);
    assert_eq!(restore[17..], mirror.states[&tick][..]);
}

#[tokio::test]
async fn room_seed_is_a_number_every_json_reader_keeps() {
    let node = Node::start().await;
    let project = node.project("seed").await;
    let mut seeds = Vec::new();
    for (player, t) in [("ann", "create"), ("bob", "join")] {
        let mut player = project.player(player).await;
        player
            .send(json!({"t": t, "name": "arena", "mode": "lockstep"}))
            .await;
        seeds.push(player.until("joined").await["seed"].clone());
    }
    let seed = seeds[0].as_u64().expect("seed");
    // What a reader that keeps numbers as doubles sees.
    assert_eq!(seeds[0].as_f64().expect("seed") as u64, seed);
    assert!(seed < 1 << 53, "{seed}");
    assert_eq!(seeds[1].as_u64(), Some(seed));
}

#[tokio::test]
async fn room_continued_without_a_checkpoint_gets_a_seed_of_its_own() {
    let fast = &[("NET__SERVER__HEARTBEAT_SECS", "1")];
    let node_a = Node::start_with(fast).await;
    let node_b = Node::start_with(fast).await;
    let pa = node_a.project("seed-moved").await;
    let pb = node_b.same_project(&pa);
    let mut creators = Vec::new();
    for (player, room) in [("ann", "arena"), ("cat", "duel")] {
        let mut player = pa.player(player).await;
        player
            .send(json!({"t": "create", "name": room, "mode": "lockstep"}))
            .await;
        player.until("joined").await;
        creators.push(player);
    }

    // A falls silent before the first checkpoint of either room, due five seconds in; B
    // continues them when a player asks for them.
    let frozen = node_a.freeze(Duration::from_secs(8));
    let moved = async {
        tokio::time::sleep(Duration::from_secs(4)).await;
        let mut seeds = Vec::new();
        for (player, room) in [("bob", "arena"), ("dan", "duel")] {
            let mut player = pb.player(player).await;
            player.send(json!({"t": "join", "name": room})).await;
            seeds.push(player.until("joined").await["seed"].as_u64().expect("seed"));
        }
        seeds
    };
    let (_, seeds) = tokio::join!(frozen, moved);
    assert_ne!(seeds[0], seeds[1]);
    assert!(seeds.iter().all(|seed| *seed < 1 << 53), "{seeds:?}");
}
