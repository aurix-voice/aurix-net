//! The room list a player subscribed to in the lobby of a running node.

mod common;

use common::{joined, Node};
use net_client::{Client, Event, Options};
use net_rooms::protocol::ServerMsg;
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

#[tokio::test]
async fn lookups_sent_at_once_are_told_apart_by_their_ids() {
    let node = Node::start().await;
    let project = node.project("lobby-ids").await;
    let mut host = project.player("host").await;
    let create =
        json!({"t": "create", "name": "dust-1", "props": {"map": "dust"}, "lobby_props": ["map"]});
    joined(&host.call(create).await);

    let mut ann = project.player("ann").await;
    // One lookup a second: the second of two sent at once is refused.
    ann.send(json!({"t": "get_rooms", "expr": "map = 'dust'", "id": 7}))
        .await;
    ann.send(json!({"t": "get_rooms", "expr": "map = 'nuke'", "id": 8}))
        .await;
    let rooms = ann.until("rooms").await;
    assert_eq!(rooms["id"], 7, "{rooms}");
    assert_eq!(rooms["rooms"][0]["name"], "dust-1", "{rooms}");
    let refused = ann.until("error").await;
    assert_eq!(
        (&refused["code"], &refused["id"]),
        (&json!("conflict"), &json!(8)),
        "{refused}"
    );

    tokio::time::sleep(Duration::from_millis(1100)).await;
    let stats = ann.call(json!({"t": "stats", "id": 9})).await;
    assert_eq!((&stats["t"], &stats["id"]), (&json!("stats"), &json!(9)));
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let friends = ann
        .call(json!({"t": "find_friends", "players": ["host"], "id": 10}))
        .await;
    assert_eq!(
        (&friends["t"], &friends["id"]),
        (&json!("friends"), &json!(10))
    );
    let refused = ann
        .call(json!({"t": "rpc", "id": 11, "name": "no spaces", "body": {}}))
        .await;
    assert_eq!(
        (&refused["t"], &refused["id"]),
        (&json!("error"), &json!(11)),
        "{refused}"
    );

    // A request without an id is answered without one, as before.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let stats = ann.call(json!({"t": "stats"})).await;
    assert_eq!(stats["t"], "stats");
    assert!(stats.get("id").is_none(), "{stats}");
}

#[tokio::test]
async fn the_rust_client_sends_a_lookup_under_the_id_it_is_given() {
    let node = Node::start().await;
    let project = node.project("lobby-client-ids").await;
    let grant = project.grant("ann").await;
    let mut ann = Client::connect(&node.http, &grant, Options::default())
        .await
        .expect("client connects");

    // One lookup a second: the second of two sent at once is refused.
    ann.stats(7).await.expect("stats sent");
    ann.find_friends(8, vec!["host".into()])
        .await
        .expect("find_friends sent");
    let (mut answered, mut refused) = (None, None);
    while answered.is_none() || refused.is_none() {
        let event = tokio::time::timeout(Duration::from_secs(5), ann.next_event())
            .await
            .expect("both lookups are answered");
        match event.expect("the connection stays") {
            Event::Control(ServerMsg::Stats { id, .. }) => answered = id,
            Event::Control(ServerMsg::Error { id, .. }) => refused = id,
            _ => {}
        }
    }
    assert_eq!((answered, refused), (Some(7), Some(8)));
}
