//! A player's role in a lockstep room survives a second join of the same player.

mod common;

use bytes::Bytes;
use common::Node;
use net_client::lockstep::{LockstepClient, LsEvent};
use net_client::{Client, Event, Options};
use net_common::Channel;
use net_rooms::protocol::ClientMsg;
use serde_json::json;
use std::time::Duration;

/// `sender` submits `input` every frame for a second; returns how many ticks carrying that input
/// for `actor` reached `watcher`.
async fn ticks_seen(
    sender: &mut Client,
    watcher: &mut Client,
    watcher_ls: &mut LockstepClient,
    actor: u16,
    input: &'static [u8],
) -> usize {
    let mut ls = LockstepClient::new();
    let mut seen = 0;
    let end = tokio::time::Instant::now() + Duration::from_secs(1);
    while tokio::time::Instant::now() < end {
        while let Some(ev) = sender.try_next_event() {
            if let Event::Data { payload, .. } = ev {
                ls.receive(&payload);
            }
        }
        if let Some((_, out)) = ls.send_input(Bytes::from_static(input)) {
            let _ = sender.send_data(Channel::UnreliableSequenced, out.payload);
        }
        sender.flush();
        while let Some(ev) = watcher.try_next_event() {
            if let Event::Data { payload, .. } = ev {
                for e in watcher_ls.receive(&payload).unwrap_or_default() {
                    if let LsEvent::Tick { inputs, .. } = e {
                        if inputs.get(&actor).is_some_and(|b| b.as_ref() == input) {
                            seen += 1;
                        }
                    }
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(16)).await;
    }
    seen
}

fn msg(v: serde_json::Value) -> ClientMsg {
    serde_json::from_value(v).expect("client message")
}

#[tokio::test]
async fn player_joining_again_as_spectator_keeps_sending_inputs() {
    let node = Node::start().await;
    let p = node.project("roles").await;
    let opts = Options {
        tunnel: true,
        ..Default::default()
    };
    let ann_grant = p.grant("ann").await;
    let mut ann = Client::connect(&node.http, &ann_grant, opts.clone())
        .await
        .expect("ann connects");
    let ann_actor = ann
        .join(msg(
            json!({"t": "create", "name": "duel", "mode": "lockstep", "max_players": 4}),
        ))
        .await
        .expect("ann creates")
        .actor;
    let mut ben = Client::connect(&node.http, &p.grant("ben").await, opts.clone())
        .await
        .expect("ben connects");
    ben.join(msg(json!({"t": "join", "name": "duel"})))
        .await
        .expect("ben joins");
    let mut ben_ls = LockstepClient::new();
    let before = ticks_seen(&mut ann, &mut ben, &mut ben_ls, ann_actor.0, b"A1").await;
    assert!(before > 10, "ben saw {before} ticks with ann's input");

    // Ann's game restarts and joins with the spectator flag set: she has a player slot, so she
    // stays a player and her inputs must keep reaching the room.
    let mut ann = Client::connect(&node.http, &ann_grant, opts)
        .await
        .expect("ann reconnects");
    let again = ann
        .join(msg(json!({"t": "join", "name": "duel", "spectator": true})))
        .await
        .expect("ann joins again");
    assert_eq!(again.actor, ann_actor);
    let after = ticks_seen(&mut ann, &mut ben, &mut ben_ls, ann_actor.0, b"A2").await;
    assert!(
        after > 10,
        "ben saw {after} ticks with ann's input after her second join"
    );
}
