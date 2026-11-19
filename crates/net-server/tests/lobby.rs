//! The room list a player subscribed to in the lobby of a running node.

mod common;

use common::{joined, Node};
use serde_json::json;
use std::time::Duration;

#[tokio::test]
async fn subscribing_twice_delivers_each_update_once() {
    let node = Node::start().await;
    let project = node.project("lobby").await;
    let mut ann = project.player("ann").await;
    for _ in 0..2 {
        ann.send(json!({"t": "lobby", "subscribe": true})).await;
        ann.until("lobby_rooms").await;
    }

    let mut bob = project.player("bob").await;
    joined(&bob.call(json!({"t": "create", "name": "open"})).await);

    let updates = ann
        .drain(Duration::from_secs(1))
        .await
        .into_iter()
        .filter(|msg| msg["t"] == "lobby_update" && msg["room"]["name"] == "open")
        .count();
    assert_eq!(updates, 1);
}
