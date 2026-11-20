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
async fn lobby_watcher_is_told_once_and_by_id_alone_that_a_room_left_his_list() {
    let node = Node::start().await;
    let project = node.project("lobby-unlisted").await;
    let mut ann = project.player("ann").await;
    ann.send(json!({"t": "lobby", "subscribe": true})).await;
    ann.until("lobby_rooms").await;
    let updates = |heard: Vec<Value>| -> Vec<Value> {
        heard
            .into_iter()
            .filter(|msg| msg["t"] == "lobby_update")
            .collect()
    };

    // Rooms the list never offered her.
    let mut hosts = Vec::new();
    for create in [
        json!({"t": "create", "name": "hidden", "visible": false}),
        json!({"t": "create", "name": "closed", "open": false}),
        json!({"t": "create", "name": "held", "max_players": 2, "reserved": ["friend"]}),
    ] {
        let name = create["name"].clone();
        let mut host = project.player(name.as_str().expect("name")).await;
        joined(&host.call(create).await);
        assert_eq!(
            updates(ann.drain(Duration::from_secs(1)).await),
            [] as [Value; 0]
        );
        hosts.push(host);
    }

    let mut bob = project.player("bob").await;
    let create = json!({"t": "create", "name": "shown", "props": {"map": "dust"},
        "lobby_props": ["map"]});
    let id = bob.call(create).await["room"].clone();
    let shown = updates(ann.drain(Duration::from_secs(1)).await);
    assert!(!shown.is_empty());
    assert!(shown
        .iter()
        .all(|msg| msg["room"]["name"] == "shown" && msg["removed"] == false));

    bob.send(json!({"t": "set_room_props", "props": {}, "visible": false}))
        .await;
    let gone = json!({"t": "lobby_update", "removed": true, "room": {"id": id, "name": "",
        "mode": "relay", "players": 0, "max_players": 0, "open": false, "props": {}}});
    assert_eq!(updates(ann.drain(Duration::from_secs(1)).await), [gone]);

    bob.send(json!({"t": "leave"})).await;
    bob.until("left").await;
    assert_eq!(
        updates(ann.drain(Duration::from_secs(1)).await),
        [] as [Value; 0]
    );
}

#[tokio::test]
async fn lobby_watcher_who_subscribes_again_is_still_told_once_that_a_room_left() {
    let node = Node::start().await;
    let project = node.project("lobby-again").await;
    let mut ann = project.player("ann").await;
    ann.send(json!({"t": "lobby", "subscribe": true})).await;
    ann.until("lobby_rooms").await;
    let mut bob = project.player("bob").await;
    let id = bob.call(json!({"t": "create", "name": "shown"})).await["room"].clone();
    ann.until("lobby_update").await;
    ann.drain(Duration::from_secs(1)).await;

    ann.send(json!({"t": "lobby", "subscribe": true})).await;
    assert_eq!(ann.until("lobby_rooms").await["rooms"][0]["id"], id);
    let updates = |heard: Vec<Value>| -> Vec<Value> {
        heard
            .into_iter()
            .filter(|msg| msg["t"] == "lobby_update")
            .collect()
    };

    bob.send(json!({"t": "set_room_props", "props": {}, "visible": false}))
        .await;
    let left = updates(ann.drain(Duration::from_secs(1)).await);
    assert_eq!(left.len(), 1, "{left:?}");
    assert_eq!(left[0]["removed"], true);
    assert_eq!(left[0]["room"]["id"], id);

    bob.send(json!({"t": "leave"})).await;
    bob.until("left").await;
    assert_eq!(
        updates(ann.drain(Duration::from_secs(1)).await),
        [] as [Value; 0]
    );
}

#[tokio::test]
async fn room_is_not_made_with_more_held_seats_than_it_has() {
    let node = Node::start().await;
    let project = node.project("lobby-overheld").await;

    let room = json!({"name": "rest", "max_players": 2, "reserved": ["a", "b", "c"]});
    let (status, reply) = project.api.post("/v1/rooms", room).await;
    assert_eq!((status, &reply["error"]), (400, &json!("invalid_request")));
    assert!(project.room_names().await.is_empty());

    let mut host = project.player("host").await;
    let create = json!({"t": "create", "name": "ws", "max_players": 2, "reserved": ["a", "b"]});
    refused(&host.call(create).await, "invalid_request");
    let create = json!({"t": "create", "name": "ws", "max_players": 2, "reserved": ["a"]});
    joined(&host.call(create).await);
}

#[tokio::test]
async fn room_is_not_made_with_a_list_of_held_seats_the_node_would_cut() {
    let node = Node::start().await;
    let project = node.project("lobby-longlist").await;
    let names = |n: usize| (0..n).map(|i| format!("p{i}")).collect::<Vec<_>>();

    let room = json!({"name": "rest", "max_players": 64, "reserved": names(33)});
    let (status, reply) = project.api.post("/v1/rooms", room).await;
    assert_eq!((status, &reply["error"]), (400, &json!("invalid_request")));
    let room = json!({"name": "rest", "max_players": 64, "reserved": names(32)});
    let (status, reply) = project.api.post("/v1/rooms", room).await;
    assert_eq!(status, 200, "{reply}");

    let mut host = project.player("host").await;
    let create = json!({"t": "create", "name": "ws", "max_players": 64, "reserved": names(33)});
    refused(&host.call(create).await, "invalid_request");
    let create = json!({"t": "create", "name": "ws", "max_players": 64, "reserved": names(32)});
    joined(&host.call(create).await);
}

#[tokio::test]
async fn random_join_does_not_guess_what_an_unknown_match_type_meant() {
    let node = Node::start().await;
    let project = node.project("lobby-match-type").await;
    let mut host = project.player("host").await;
    joined(&host.call(json!({"t": "create", "name": "arena"})).await);

    let mut ann = project.player("ann").await;
    let fullest = json!({"t": "join_random", "match_type": "fullest"});
    refused(&ann.call(fullest).await, "invalid_request");
    let serial = json!({"t": "join_random", "match_type": "serial"});
    joined(&ann.call(serial).await);
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

#[tokio::test]
async fn find_friends_names_the_room_a_player_is_in() {
    const FAST: &[(&str, &str)] = &[("NET__SERVER__HEARTBEAT_SECS", "1")];
    let node = Node::start_with(FAST).await;
    let other = Node::start_with(FAST).await;
    let project = node.project("friends").await;
    let elsewhere = other.same_project(&project);
    let mut ann = project.player("ann").await;
    let find = async |ann: &mut common::Ws| {
        tokio::time::sleep(Duration::from_millis(1100)).await;
        let reply = ann
            .call(json!({"t": "find_friends", "players": ["bob"]}))
            .await;
        let bob = &reply["friends"][0];
        (bob["online"] == true, bob["room"]["name"].clone())
    };

    // Bob loses the connection to a room, and the room keeps his seat.
    let mut bob = project.player("bob").await;
    joined(&bob.call(json!({"t": "create", "name": "left"})).await);
    let mut stays = project.player("cat").await;
    joined(&stays.call(json!({"t": "join", "name": "left"})).await);
    drop(bob);
    assert_eq!(stays.until("player_left").await["offline"], true);
    assert_eq!(find(&mut ann).await, (false, json!("left")));

    // He connects to another node: online, and in no room.
    let mut bob = elsewhere.player("bob").await;
    assert_eq!(find(&mut ann).await, (true, Value::Null));

    joined(&bob.call(json!({"t": "create", "name": "now"})).await);
    assert_eq!(find(&mut ann).await, (true, json!("now")));
}
