//! A client that runs the room's module over the confirmed inputs and reports its state hash
//! the way `docs/sdk.md` tells it to agrees with the node, which runs the same module.

mod common;

use bytes::Bytes;
use common::Node;
use net_api::lockstep::ServerSim;
use net_api::sync::ARENA_SCHEMA;
use net_client::lockstep::{LockstepClient, LsEvent, Outgoing};
use net_client::{Client, Event, Options};
use net_common::types::ActorId;
use net_common::Channel;
use net_rooms::protocol::{ClientMsg, ServerMsg};
use net_sim::{Frame, FrameConfig};
use net_wire::Schema;
use serde_json::json;
use std::time::{Duration, Instant};

const ARENA: &str = include_str!("../../net-game/examples/arena.wat");
const TICK_HZ: u16 = 60;

fn send(client: &Client, frame: Outgoing) {
    let channel = if frame.reliable {
        Channel::ReliableOrdered
    } else {
        Channel::UnreliableSequenced
    };
    client.send_data(channel, frame.payload).expect("sends");
}

/// Steps `sim` over the ticks `client` has received and reports the hash after every tenth
/// one under that tick's number. Returns the tick numbers of the desyncs the node announced.
fn play(
    client: &mut Client,
    bundles: &mut LockstepClient,
    sim: &mut ServerSim,
    reported: &mut Vec<(u32, u64)>,
) -> Vec<u32> {
    let mut desyncs = Vec::new();
    while let Some(event) = client.try_next_event() {
        let payload = match event {
            Event::Control(ServerMsg::Desync { tick, .. }) => {
                desyncs.push(tick);
                continue;
            }
            Event::Data { payload, .. } => payload,
            _ => continue,
        };
        for event in bundles.receive(&payload).unwrap_or_default() {
            match event {
                LsEvent::Tick { tick, inputs } => {
                    let inputs = inputs
                        .iter()
                        .map(|(actor, input)| (ActorId(*actor), input.to_vec()))
                        .collect();
                    sim.step(&inputs);
                    if tick % 10 == 0 {
                        let hash = sim.hash();
                        send(client, LockstepClient::hash_frame(tick, hash));
                        reported.push((tick, hash));
                    }
                }
                LsEvent::Restore { state, .. } if !state.is_empty() => {
                    sim.frame.restore(&state).expect("restores");
                }
                _ => {}
            }
        }
        send(client, bundles.ack_frame());
    }
    desyncs
}

#[tokio::test]
async fn client_that_reports_hashes_as_documented_agrees_with_the_referee() {
    let node = Node::start().await;
    let project = node.project("referee").await;
    let upload = reqwest::Client::new()
        .put(format!("{}/v1/modules/arena", node.http))
        .bearer_auth(&project.api.token)
        .body(ARENA)
        .send()
        .await
        .expect("uploads the module");
    assert!(upload.status().is_success(), "{}", upload.status());

    let opts = Options {
        tunnel: true,
        ..Default::default()
    };
    let mut client = Client::connect(&node.http, &project.grant("ann").await, opts)
        .await
        .expect("connects");
    let create: ClientMsg = serde_json::from_value(json!({
        "t": "create", "name": "arena", "mode": "lockstep",
        "props": {
            "tick_hz": TICK_HZ,
            "logic": "arena",
            "schema": ARENA_SCHEMA,
            "position": "Transform.0",
        },
    }))
    .expect("client message");
    let seed = client
        .join(create)
        .await
        .expect("creates")
        .seed
        .expect("seed");

    let engine = net_game::GameEngine::new();
    let (_, module) = engine.load(ARENA.as_bytes()).expect("module");
    let logic = engine.instantiate(&module, seed).expect("logic");
    let config = FrameConfig {
        tick_hz: TICK_HZ,
        ..Default::default()
    };
    let schema = Schema::parse(ARENA_SCHEMA).expect("schema");
    let mut sim = ServerSim::new(Frame::new(schema, config, seed), Box::new(logic));
    let mut bundles = LockstepClient::new();
    let mut reported = Vec::new();

    let until = Instant::now() + Duration::from_secs(3);
    while Instant::now() < until {
        if let Some((_, frame)) = bundles.send_input(Bytes::from_static(&[127, 0, 0])) {
            send(&client, frame);
        }
        client.flush();
        tokio::time::sleep(Duration::from_millis(1000 / u64::from(TICK_HZ))).await;
        let desyncs = play(&mut client, &mut bundles, &mut sim, &mut reported);
        assert!(desyncs.is_empty(), "desync at ticks {desyncs:?}");
    }
    assert!(reported.len() >= 10, "{reported:?}");
    let states: std::collections::BTreeSet<u64> = reported.iter().map(|(_, hash)| *hash).collect();
    assert!(states.len() >= 10, "the avatar did not move: {reported:?}");

    // The node does compare: a wrong hash under the last reported tick is a desync.
    let (tick, hash) = *reported.last().expect("a hash");
    send(&client, LockstepClient::hash_frame(tick, hash ^ 1));
    client.flush();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let desyncs = play(&mut client, &mut bundles, &mut sim, &mut reported);
    assert_eq!(desyncs, [tick]);
}
