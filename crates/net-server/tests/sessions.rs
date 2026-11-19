//! Who may enter a room, and what happens to a control session when its player is kicked, the
//! room is closed, an offline slot expires or its handshake is cut short. Each test runs against
//! a real node.

mod common;

use common::{joined, refused, Node, Sink};
use serde_json::{json, Value};
use std::time::Duration;

const QUIET: Duration = Duration::from_millis(300);

#[tokio::test]
async fn banned_player_cannot_join_a_room() {
    let node = Node::start().await;
    let project = node.project("ban").await;
    let mut alice = project.player("alice").await;
    joined(&alice.call(json!({"t": "create", "name": "arena"})).await);

    // The grant is issued before the ban and stays valid: the join itself must refuse.
    let mut mallory = project.player("mallory").await;
    let (status, body) = project
        .api
        .post(
            "/v1/bans",
            json!({"player_id": "mallory", "reason": "cheat"}),
        )
        .await;
    assert!(status < 300, "ban: {body}");

    refused(
        &mallory.call(json!({"t": "join", "name": "arena"})).await,
        "forbidden",
    );
    refused(
        &mallory
            .call(json!({"t": "join_or_create", "name": "arena"}))
            .await,
        "forbidden",
    );
    refused(
        &mallory.call(json!({"t": "join_random"})).await,
        "forbidden",
    );
    refused(
        &mallory.call(json!({"t": "create", "name": "own"})).await,
        "forbidden",
    );
}

#[tokio::test]
async fn player_of_a_suspended_project_cannot_join_a_room() {
    let node = Node::start().await;
    let project = node.project("suspended").await;
    let mut alice = project.player("alice").await;
    joined(&alice.call(json!({"t": "create", "name": "arena"})).await);
    let mut bob = project.player("bob").await;
    let (status, body) = project
        .operator
        .post(
            &format!("/v1/operator/projects/{}/suspend", project.id),
            json!({"suspended": true}),
        )
        .await;
    assert!(status < 300, "suspend: {body}");

    refused(
        &bob.call(json!({"t": "join", "name": "arena"})).await,
        "forbidden",
    );
}

#[tokio::test]
async fn rejoin_actor_does_not_lift_the_player_cap() {
    let node = Node::start().await;
    let project = node.project("cap").await;
    let (status, body) = project
        .operator
        .post(
            &format!("/v1/operator/projects/{}/limits", project.id),
            json!({"max_concurrent_players": 1}),
        )
        .await;
    assert!(status < 300, "limits: {body}");
    let mut alice = project.player("alice").await;
    joined(&alice.call(json!({"t": "create", "name": "arena"})).await);

    let mut bob = project.player("bob").await;
    refused(
        &bob.call(json!({"t": "join", "name": "arena"})).await,
        "quota_exceeded",
    );
    refused(
        &bob.call(json!({"t": "join", "name": "arena", "rejoin_actor": 77}))
            .await,
        "quota_exceeded",
    );
}

#[tokio::test]
async fn offline_player_returns_to_the_same_slot_at_the_cap() {
    let node = Node::start().await;
    let project = node.project("return").await;
    let mut alice = project.player("alice").await;
    joined(&alice.call(json!({"t": "create", "name": "arena"})).await);
    let mut bob = project.player("bob").await;
    let actor = joined(&bob.call(json!({"t": "join", "name": "arena"})).await);
    let (status, body) = project
        .operator
        .post(
            &format!("/v1/operator/projects/{}/limits", project.id),
            json!({"max_concurrent_players": 2}),
        )
        .await;
    assert!(status < 300, "limits: {body}");

    drop(bob);
    let left = alice.until("player_left").await;
    assert_eq!(left["offline"], true, "{left}");

    // The project is full (alice and bob's held slot), a stranger is refused, bob comes back.
    let mut carol = project.player("carol").await;
    refused(
        &carol.call(json!({"t": "join", "name": "arena"})).await,
        "quota_exceeded",
    );
    let mut bob = project.player("bob").await;
    let back = bob
        .call(json!({"t": "join", "name": "arena", "rejoin_actor": actor}))
        .await;
    assert_eq!(joined(&back), actor);
}

#[tokio::test]
async fn room_grant_holds_create_and_join_random() {
    let node = Node::start().await;
    let project = node.project("scope").await;
    let mut alice = project.player("alice").await;
    joined(
        &alice
            .call(json!({"t": "create", "name": "open-room"}))
            .await,
    );

    let grant = project
        .grant_with(json!({"player_id": "bob", "name": "bob", "room": "lobby-7"}))
        .await;
    let mut bob = common::Ws::open(&project.api.http, &grant).await;
    refused(
        &bob.call(json!({"t": "join", "name": "open-room"})).await,
        "forbidden",
    );
    refused(
        &bob.call(json!({"t": "create", "name": "secret-room"}))
            .await,
        "forbidden",
    );
    let random = bob.call(json!({"t": "join_random"})).await;
    assert_eq!(
        random["t"], "error",
        "join_random left the grant's room: {random}"
    );
    assert_eq!(project.room_names().await, ["open-room"]);

    joined(&bob.call(json!({"t": "create", "name": "lobby-7"})).await);
}

#[tokio::test]
async fn kicked_player_can_create_a_room_at_once() {
    let node = Node::start().await;
    let project = node.project("kick").await;
    let mut alice = project.player("alice").await;
    joined(&alice.call(json!({"t": "create", "name": "arena"})).await);
    let mut bob = project.player("bob").await;
    let actor = joined(&bob.call(json!({"t": "join", "name": "arena"})).await);

    alice
        .send(json!({"t": "kick", "actor": actor, "reason": "afk"}))
        .await;
    let left = bob.until("left").await;
    assert_eq!(left["reason"], "afk");

    joined(&bob.call(json!({"t": "create", "name": "bobs-room"})).await);
}

#[tokio::test]
async fn left_names_the_room_it_is_about() {
    let node = Node::start().await;
    let project = node.project("left-room").await;
    let mut alice = project.player("alice").await;
    let created = alice.call(json!({"t": "create", "name": "arena"})).await;
    joined(&created);
    let room = created["room"].as_str().expect("room id");
    let join = json!({"t": "join", "name": "arena"});
    let told = |left: Value| (left["reason"].clone(), left["room"].clone());

    let mut bob = project.player("bob").await;
    joined(&bob.call(join.clone()).await);
    bob.send(json!({"t": "leave"})).await;
    assert_eq!(told(bob.until("left").await), (json!("left"), json!(room)));
    // Outside a room there is nothing to name.
    bob.send(json!({"t": "leave"})).await;
    assert_eq!(told(bob.until("left").await), (json!("left"), Value::Null));

    let actor = joined(&bob.call(join.clone()).await);
    alice.send(json!({"t": "kick", "actor": actor})).await;
    assert_eq!(
        told(bob.until("left").await),
        (json!("kicked"), json!(room))
    );

    joined(&bob.call(join.clone()).await);
    let mut second = project.player("bob").await;
    joined(&second.call(join).await);
    assert_eq!(
        told(bob.until("left").await),
        (json!("session_replaced"), json!(room))
    );

    let (status, body) = project.api.delete(&format!("/v1/rooms/{room}")).await;
    assert_eq!(status, 204, "close room: {body}");
    assert_eq!(
        told(second.until("left").await),
        (json!("room closed"), json!(room))
    );
}

#[tokio::test]
async fn closed_room_frees_its_players_and_stays_closed() {
    let node = Node::start().await;
    let project = node.project("close").await;
    let mut alice = project.player("alice").await;
    let created = alice.call(json!({"t": "create", "name": "arena"})).await;
    joined(&created);
    let mut bob = project.player("bob").await;
    joined(&bob.call(json!({"t": "join", "name": "arena"})).await);

    let room = created["room"].as_str().expect("room id");
    let (status, body) = project.api.delete(&format!("/v1/rooms/{room}")).await;
    assert_eq!(status, 204, "close room: {body}");
    assert_eq!(alice.until("left").await["reason"], "room closed");
    assert_eq!(bob.until("left").await["reason"], "room closed");

    joined(&alice.call(json!({"t": "create", "name": "next"})).await);

    // A player of the closed room going away must not put the room back into the listing.
    drop(bob);
    tokio::time::sleep(QUIET).await;
    assert_eq!(project.room_names().await, ["next"]);
}

#[tokio::test]
async fn spectator_cannot_become_master() {
    let node = Node::start().await;
    let project = node.project("master").await;
    let mut ann = project.player("ann").await;
    let created = ann.call(json!({"t": "create", "name": "arena"})).await;
    let master = joined(&created);
    let mut ben = project.player("ben").await;
    let spectator = joined(
        &ben.call(json!({"t": "join", "name": "arena", "spectator": true}))
            .await,
    );
    let mut cid = project.player("cid").await;
    let player = joined(&cid.call(json!({"t": "join", "name": "arena"})).await);
    for who in [&mut ann, &mut ben, &mut cid] {
        who.drain(QUIET).await;
    }

    refused(
        &ann.call(json!({"t": "set_master", "actor": spectator}))
            .await,
        "forbidden",
    );
    refused(
        &ben.call(json!({"t": "kick", "actor": player, "reason": "x"}))
            .await,
        "forbidden",
    );

    // A player still can.
    ann.send(json!({"t": "set_master", "actor": player})).await;
    assert_eq!(cid.until("master_changed").await["actor"], player);
    assert_ne!(master, player);
}

#[tokio::test]
async fn expired_offline_slot_leaves_the_others_connected() {
    let node = Node::start().await;
    let project = node.project("expire").await;
    let mut alice = project.player("alice").await;
    joined(
        &alice
            .call(json!({"t": "create", "name": "arena", "props": {"player_ttl_secs": 1}}))
            .await,
    );
    let mut bob = project.player("bob").await;
    joined(&bob.call(json!({"t": "join", "name": "arena"})).await);
    let mut carol = project.player("carol").await;
    let gone = joined(&carol.call(json!({"t": "join", "name": "arena"})).await);

    drop(carol);
    for who in [&mut alice, &mut bob] {
        loop {
            let left = who.until("player_left").await;
            assert_eq!(left["actor"], gone, "{left}");
            if left["offline"] == false {
                break;
            }
        }
    }

    // The players who stayed keep their sessions: a room command still goes through.
    alice
        .send(json!({"t": "set_room_props", "props": {"round": 2}}))
        .await;
    assert_eq!(bob.until("room_props").await["props"]["round"], 2);
    alice.drain(QUIET).await;
    bob.send(json!({"t": "set_room_props", "props": {"round": 3}}))
        .await;
    assert_eq!(alice.until("room_props").await["props"]["round"], 3);
}

#[tokio::test]
async fn player_left_webhook_tells_a_kick_from_a_leave() {
    let node = Node::start().await;
    let project = node.project("kick-hook").await;
    let mut sink = Sink::start().await;
    let (status, reply) = project
        .api
        .post(
            "/v1/webhooks",
            json!({"url": sink.url, "events": ["player.left"]}),
        )
        .await;
    assert_eq!(status, 200, "{reply}");
    let mut alice = project.player("alice").await;
    let master = joined(&alice.call(json!({"t": "create", "name": "arena"})).await);
    let mut bob = project.player("bob").await;
    let kicked = joined(&bob.call(json!({"t": "join", "name": "arena"})).await);
    alice.until("player_joined").await;

    alice.send(json!({"t": "kick", "actor": kicked})).await;
    let data = sink.next("player.left").await["data"].clone();
    assert_eq!(data["actor"], kicked);
    assert_eq!(data["kicked"], true, "{data}");

    alice.send(json!({"t": "leave"})).await;
    let data = sink.next("player.left").await["data"].clone();
    assert_eq!(data["actor"], master);
    assert!(data.get("kicked").is_none(), "{data}");
}

#[tokio::test]
async fn refused_ban_request_bans_nobody() {
    let node = Node::start().await;
    let project = node.project("ban-refused").await;
    for (player, scope) in [("eve", "planet"), ("fred", "player_ip")] {
        let ban = json!({"player_id": player, "scope": scope});
        let (status, reply) = project.api.post("/v1/bans", ban).await;
        assert_eq!(status, 400, "{reply}");
    }
    let (_, listed) = project.api.get("/v1/bans").await;
    assert_eq!(listed["bans"], json!([]), "{listed}");
    project.grant("eve").await;
    project.grant("fred").await;
}

#[tokio::test]
async fn ban_for_no_time_is_refused_and_not_made_permanent() {
    let node = Node::start().await;
    let project = node.project("ban-zero").await;
    let ban = json!({"player_id": "eve", "duration_secs": 0});
    let (status, reply) = project.api.post("/v1/bans", ban).await;
    assert_eq!((status, &reply["error"]), (400, &json!("invalid_request")));
    let (_, listed) = project.api.get("/v1/bans").await;
    assert_eq!(listed["bans"], json!([]), "{listed}");
}

#[tokio::test]
async fn address_ban_stops_the_next_player_from_that_address() {
    let node = Node::start().await;
    let project = node.project("ban-address").await;
    let mut alice = project.player("alice").await;
    joined(&alice.call(json!({"t": "create", "name": "arena"})).await);
    let ban = json!({"player_id": "alice", "scope": "player_ip"});
    let (status, reply) = project.api.post("/v1/bans", ban).await;
    assert_eq!(status, 200, "{reply}");

    let mut zed = project.player("zed").await;
    let reply = zed.call(json!({"t": "create", "name": "other"})).await;
    refused(&reply, "forbidden");
}

#[tokio::test]
async fn full_room_refusal_names_the_room_size() {
    let node = Node::start().await;
    let project = node.project("full").await;
    let mut alice = project.player("alice").await;
    joined(
        &alice
            .call(json!({"t": "create", "name": "duel", "max_players": 1}))
            .await,
    );
    let mut bob = project.player("bob").await;
    let reply = bob.call(json!({"t": "join", "name": "duel"})).await;
    refused(&reply, "room_full");
    assert_eq!(reply["message"], "room is full (max_players is 1)");
}

#[tokio::test]
async fn handshake_cut_short_gives_its_connection_back_to_the_address() {
    use tokio::io::AsyncWriteExt;

    let node = Node::start_with(&[("NET__LIMITS__MAX_SESSIONS_PER_IP", "2")]).await;
    let address = node.http.trim_start_matches("http://");
    let request = format!(
        "GET /ws HTTP/1.1\r\nHost: {address}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\
         Sec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n"
    );
    for _ in 0..4 {
        let mut tcp = tokio::net::TcpStream::connect(address)
            .await
            .expect("connect");
        tcp.write_all(request.as_bytes()).await.expect("request");
        // Closed with a reset, so the node has nowhere to write its answer.
        socket2::SockRef::from(&tcp)
            .set_linger(Some(Duration::ZERO))
            .expect("linger");
        drop(tcp);
    }
    tokio::time::sleep(QUIET).await;

    let url = node.http.replacen("http", "ws", 1) + "/ws";
    let connected = tokio_tungstenite::connect_async(&url).await;
    assert!(connected.is_ok(), "{:?}", connected.err());
}

#[tokio::test]
async fn control_message_with_an_unknown_field_is_refused_by_its_name() {
    let node = Node::start().await;
    let project = node.project("unknown-field").await;
    let mut ann = project.player("ann").await;

    let reply = ann
        .call(json!({"t": "create", "name": "arena", "colour": "red"}))
        .await;
    refused(&reply, "invalid_request");
    let message = reply["message"].as_str().expect("message");
    assert!(message.contains("unknown field `colour`"), "{reply}");

    // Props are the game's own and take any key.
    let create = json!({"t": "create", "name": "arena", "props": {"colour": "red"}});
    joined(&ann.call(create).await);

    let target = json!({"kind": "actors", "actors": [1], "colour": "red"});
    let reply = ann
        .call(json!({"t": "event", "code": 1, "data": "", "target": target}))
        .await;
    refused(&reply, "invalid_request");
    let message = reply["message"].as_str().expect("message");
    assert!(message.contains("colour"), "{reply}");
}
