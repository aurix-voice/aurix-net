//! Who may enter a room, and what happens to a control session when its player is kicked, the
//! room is closed or an offline slot expires. Each test runs against a real node.

mod common;

use common::{joined, refused, Node, Sink};
use serde_json::json;
use std::time::Duration;

const QUIET: Duration = Duration::from_millis(300);

#[tokio::test]
async fn banned_player_cannot_join_a_room() {
    let node = Node::start().await;
    let p = node.project("ban").await;
    let mut alice = p.player("alice").await;
    joined(&alice.call(json!({"t": "create", "name": "arena"})).await);

    // The grant is issued before the ban and stays valid: the join itself must refuse.
    let mut mallory = p.player("mallory").await;
    let (status, body) = p
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
    let p = node.project("suspended").await;
    let mut alice = p.player("alice").await;
    joined(&alice.call(json!({"t": "create", "name": "arena"})).await);
    let mut bob = p.player("bob").await;
    let (status, body) = p
        .operator
        .post(
            &format!("/v1/operator/projects/{}/suspend", p.id),
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
    let p = node.project("cap").await;
    let (status, body) = p
        .operator
        .post(
            &format!("/v1/operator/projects/{}/limits", p.id),
            json!({"max_concurrent_players": 1}),
        )
        .await;
    assert!(status < 300, "limits: {body}");
    let mut alice = p.player("alice").await;
    joined(&alice.call(json!({"t": "create", "name": "arena"})).await);

    let mut bob = p.player("bob").await;
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
    let p = node.project("return").await;
    let mut alice = p.player("alice").await;
    joined(&alice.call(json!({"t": "create", "name": "arena"})).await);
    let mut bob = p.player("bob").await;
    let actor = joined(&bob.call(json!({"t": "join", "name": "arena"})).await);
    let (status, body) = p
        .operator
        .post(
            &format!("/v1/operator/projects/{}/limits", p.id),
            json!({"max_concurrent_players": 2}),
        )
        .await;
    assert!(status < 300, "limits: {body}");

    drop(bob);
    let left = alice.until("player_left").await;
    assert_eq!(left["offline"], true, "{left}");

    // The project is full (alice and bob's held slot), a stranger is refused, bob comes back.
    let mut carol = p.player("carol").await;
    refused(
        &carol.call(json!({"t": "join", "name": "arena"})).await,
        "quota_exceeded",
    );
    let mut bob = p.player("bob").await;
    let back = bob
        .call(json!({"t": "join", "name": "arena", "rejoin_actor": actor}))
        .await;
    assert_eq!(joined(&back), actor);
}

#[tokio::test]
async fn room_grant_holds_create_and_join_random() {
    let node = Node::start().await;
    let p = node.project("scope").await;
    let mut alice = p.player("alice").await;
    joined(
        &alice
            .call(json!({"t": "create", "name": "open-room"}))
            .await,
    );

    let grant = p
        .grant_with(json!({"player_id": "bob", "name": "bob", "room": "lobby-7"}))
        .await;
    let mut bob = common::Ws::open(&p.api.http, &grant).await;
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
    assert_eq!(p.room_names().await, ["open-room"]);

    joined(&bob.call(json!({"t": "create", "name": "lobby-7"})).await);
}

#[tokio::test]
async fn kicked_player_can_create_a_room_at_once() {
    let node = Node::start().await;
    let p = node.project("kick").await;
    let mut alice = p.player("alice").await;
    joined(&alice.call(json!({"t": "create", "name": "arena"})).await);
    let mut bob = p.player("bob").await;
    let actor = joined(&bob.call(json!({"t": "join", "name": "arena"})).await);

    alice
        .send(json!({"t": "kick", "actor": actor, "reason": "afk"}))
        .await;
    let left = bob.until("left").await;
    assert_eq!(left["reason"], "afk");

    joined(&bob.call(json!({"t": "create", "name": "bobs-room"})).await);
}

#[tokio::test]
async fn closed_room_frees_its_players_and_stays_closed() {
    let node = Node::start().await;
    let p = node.project("close").await;
    let mut alice = p.player("alice").await;
    let created = alice.call(json!({"t": "create", "name": "arena"})).await;
    joined(&created);
    let mut bob = p.player("bob").await;
    joined(&bob.call(json!({"t": "join", "name": "arena"})).await);

    let room = created["room"].as_str().expect("room id");
    let (status, body) = p.api.delete(&format!("/v1/rooms/{room}")).await;
    assert_eq!(status, 204, "close room: {body}");
    assert_eq!(alice.until("left").await["reason"], "room closed");
    assert_eq!(bob.until("left").await["reason"], "room closed");

    joined(&alice.call(json!({"t": "create", "name": "next"})).await);

    // A player of the closed room going away must not put the room back into the listing.
    drop(bob);
    tokio::time::sleep(QUIET).await;
    assert_eq!(p.room_names().await, ["next"]);
}

#[tokio::test]
async fn spectator_cannot_become_master() {
    let node = Node::start().await;
    let p = node.project("master").await;
    let mut ann = p.player("ann").await;
    let created = ann.call(json!({"t": "create", "name": "arena"})).await;
    let master = joined(&created);
    let mut ben = p.player("ben").await;
    let spectator = joined(
        &ben.call(json!({"t": "join", "name": "arena", "spectator": true}))
            .await,
    );
    let mut cid = p.player("cid").await;
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
    let p = node.project("expire").await;
    let mut alice = p.player("alice").await;
    joined(
        &alice
            .call(json!({"t": "create", "name": "arena", "props": {"player_ttl_secs": 1}}))
            .await,
    );
    let mut bob = p.player("bob").await;
    joined(&bob.call(json!({"t": "join", "name": "arena"})).await);
    let mut carol = p.player("carol").await;
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
    let p = node.project("kick-hook").await;
    let mut sink = Sink::start().await;
    let (status, reply) = p
        .api
        .post(
            "/v1/webhooks",
            json!({"url": sink.url, "events": ["player.left"]}),
        )
        .await;
    assert_eq!(status, 200, "{reply}");
    let mut alice = p.player("alice").await;
    let master = joined(&alice.call(json!({"t": "create", "name": "arena"})).await);
    let mut bob = p.player("bob").await;
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
