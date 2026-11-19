//! The room list a player subscribed to in the lobby of a running node.

mod common;

use common::{joined, refused, Node};
use net_client::{Client, Event, Options};
use net_rooms::protocol::ServerMsg;
use serde_json::{json, Value};
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

#[tokio::test]
async fn room_nobody_touches_stays_in_the_list() {
    // One-second heartbeats: a room's record lives four seconds unless its node renews it.
    let node = Node::start_with(&[("NET__SERVER__HEARTBEAT_SECS", "1")]).await;
    let project = node.project("lobby-idle").await;
    let mut ann = project.player("ann").await;
    joined(&ann.call(json!({"t": "create", "name": "arena"})).await);
    tokio::time::sleep(Duration::from_secs(6)).await;

    let (status, list) = project.api.get("/v1/rooms").await;
    assert_eq!(status, 200, "{list}");
    assert_eq!(list["rooms"][0]["name"], "arena", "{list}");
}

#[tokio::test]
async fn room_created_closed_takes_its_creator_and_nobody_else() {
    let node = Node::start().await;
    let project = node.project("lobby-closed").await;
    let mut host = project.player("host").await;
    let closed = json!({"t": "create", "name": "closed", "open": false});
    let actor = joined(&host.call(closed).await);

    let mut ann = project.player("ann").await;
    refused(
        &ann.call(json!({"t": "join", "name": "closed"})).await,
        "room_closed",
    );

    let rooms: serde_json::Value = reqwest::Client::new()
        .get(format!("{}/v1/rooms", node.http))
        .bearer_auth(&project.api.token)
        .send()
        .await
        .expect("asks for the rooms")
        .json()
        .await
        .expect("rooms");
    let rooms = rooms["rooms"].as_array().expect("rooms");
    assert_eq!(rooms.len(), 1, "{rooms:?}");
    assert_eq!(rooms[0]["players"], 1, "{rooms:?}");
    assert_eq!(hosted_rooms(&node).await, 1);

    // the creator who lost the connection comes back to the seat that is kept for him
    drop(host);
    let mut back = project.player("host").await;
    let reply = back.call(json!({"t": "join", "name": "closed"})).await;
    assert_eq!(joined(&reply), actor);
}

#[tokio::test]
async fn room_made_by_join_or_create_is_hidden_or_closed_as_asked() {
    let node = Node::start().await;
    let project = node.project("lobby-made").await;
    let mut host = project.player("host").await;
    let hidden = json!({"t": "join_or_create", "name": "hidden", "visible": false});
    joined(&host.call(hidden).await);

    let mut ann = project.player("ann").await;
    let rooms = ann.call(json!({"t": "get_rooms", "id": 1})).await;
    assert_eq!(rooms["rooms"], json!([]), "{rooms}");
    refused(&ann.call(json!({"t": "join_random"})).await, "not_found");

    // joining a room that exists changes nothing about it
    let shown = json!({"t": "join_or_create", "name": "hidden", "visible": true});
    joined(&ann.call(shown).await);
    let mut bob = project.player("bob").await;
    let rooms = bob.call(json!({"t": "get_rooms", "id": 2})).await;
    assert_eq!(rooms["rooms"], json!([]), "{rooms}");

    let closed = json!({"t": "join_or_create", "name": "closed", "open": false});
    joined(&bob.call(closed).await);
    let mut cat = project.player("cat").await;
    let rooms = cat.call(json!({"t": "get_rooms", "id": 3})).await;
    assert_eq!(rooms["rooms"], json!([]), "{rooms}");
    refused(&cat.call(json!({"t": "join_random"})).await, "not_found");
    refused(
        &cat.call(json!({"t": "join", "name": "closed"})).await,
        "room_closed",
    );
    refused(
        &cat.call(json!({"t": "join_or_create", "name": "closed"}))
            .await,
        "room_closed",
    );
}

/// The `net_rooms` count of rooms the node hosts.
async fn hosted_rooms(node: &Node) -> u64 {
    let metrics = reqwest::get(format!("{}/metrics", node.http))
        .await
        .expect("metrics")
        .text()
        .await
        .expect("metrics text");
    metrics
        .lines()
        .find_map(|l| l.strip_prefix("net_rooms "))
        .map_or(0, |count| count.parse().expect("count"))
}

#[tokio::test]
async fn room_whose_creator_is_refused_is_not_left_behind() {
    let node = Node::start().await;
    let project = node.project("lobby-refused").await;
    let mut host = project.player("host").await;
    let heavy = json!({"bio": "x".repeat(5000)});

    for call in ["create", "join_or_create"] {
        let reply = host
            .call(json!({"t": call, "name": call, "player_props": heavy}))
            .await;
        refused(&reply, "invalid_request");
        assert_eq!(hosted_rooms(&node).await, 0, "after {call}");
    }

    joined(&host.call(json!({"t": "create", "name": "create"})).await);
    assert_eq!(hosted_rooms(&node).await, 1);
}

#[tokio::test]
async fn room_whose_last_seat_is_held_is_offered_to_its_player_alone() {
    let node = Node::start().await;
    let project = node.project("lobby-held").await;
    let mut host = project.player("host").await;
    let create = json!({"t": "create", "name": "party", "max_players": 2, "reserved": ["friend"]});
    joined(&host.call(create).await);

    let mut ann = project.player("ann").await;
    let rooms = ann.call(json!({"t": "get_rooms", "id": 1})).await;
    assert_eq!(rooms["rooms"], json!([]), "{rooms}");
    refused(&ann.call(json!({"t": "join_random"})).await, "not_found");

    let mut friend = project.player("friend").await;
    let rooms = friend.call(json!({"t": "get_rooms", "id": 2})).await;
    assert_eq!(rooms["rooms"][0]["name"], "party", "{rooms}");
    joined(&friend.call(json!({"t": "join_random"})).await);
}

#[tokio::test]
async fn lobby_watcher_hears_of_a_room_whose_last_seat_is_held_for_him_alone() {
    let node = Node::start().await;
    let project = node.project("lobby-feed").await;
    let mut ann = project.player("ann").await;
    let mut friend = project.player("friend").await;
    for watcher in [&mut ann, &mut friend] {
        watcher.send(json!({"t": "lobby", "subscribe": true})).await;
        watcher.until("lobby_rooms").await;
    }

    let mut host = project.player("host").await;
    let create = json!({"t": "create", "name": "party", "max_players": 2, "reserved": ["friend"]});
    joined(&host.call(create).await);

    let offered = |updates: Vec<Value>| {
        updates.into_iter().any(|msg| {
            msg["t"] == "lobby_update" && msg["room"]["name"] == "party" && msg["removed"] == false
        })
    };
    assert!(offered(friend.drain(Duration::from_secs(1)).await));
    assert!(!offered(ann.drain(Duration::from_secs(1)).await));
}

#[tokio::test]
async fn room_is_offered_again_once_the_hold_on_its_seat_lapses() {
    let node = Node::start().await;
    let project = node.project("lobby-lapse").await;
    let mut host = project.player("host").await;
    let create = json!({"t": "create", "name": "party", "max_players": 2, "reserved": ["friend"]});
    joined(&host.call(create).await);

    let mut ann = project.player("ann").await;
    let rooms = ann.call(json!({"t": "get_rooms", "id": 1})).await;
    assert_eq!(rooms["rooms"], json!([]), "{rooms}");

    tokio::time::sleep(Duration::from_secs(63)).await;
    let rooms = ann.call(json!({"t": "get_rooms", "id": 2})).await;
    assert_eq!(rooms["rooms"][0]["name"], "party", "{rooms}");
    joined(&ann.call(json!({"t": "join_random"})).await);
}
