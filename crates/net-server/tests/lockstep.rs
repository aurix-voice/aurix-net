//! A `lockstep` room confirms the newest input of a frame that arrives behind its tick.

mod common;

use bytes::{BufMut, Bytes, BytesMut};
use common::Node;
use net_client::lockstep::{LockstepClient, LsEvent};
use net_client::{Client, Event, Options};
use net_common::Channel;
use net_rooms::protocol::{ClientMsg, ServerMsg};
use serde_json::json;
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
