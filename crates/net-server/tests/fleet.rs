//! Two nodes of one fleet sharing Postgres and Redis.

mod common;

use common::{joined, refused, Node};
use serde_json::json;
use std::time::Duration;

/// One-second heartbeats: a node that is silent for three seconds is taken for dead.
const FAST: &[(&str, &str)] = &[("NET__SERVER__HEARTBEAT_SECS", "1")];

#[tokio::test]
async fn stalled_node_takes_its_rooms_back() {
    let node_a = Node::start_with(FAST).await;
    let node_b = Node::start_with(FAST).await;
    let pa = node_a.project("stall").await;
    let pb = node_b.same_project(&pa);
    let mut alice = pa.player("alice").await;
    joined(&alice.call(json!({"t": "create", "name": "arena"})).await);

    // B reaps A and offers its rooms for adoption; nobody takes them before A carries on.
    node_a.freeze(Duration::from_secs(6)).await;
    tokio::time::sleep(Duration::from_secs(3)).await;

    // The room has one home again: B points the player to A instead of starting a second copy.
    let mut bob = pb.player("bob").await;
    let reply = bob.call(json!({"t": "join", "name": "arena"})).await;
    refused(&reply, "conflict");
    let message = reply["message"].as_str().unwrap_or_default();
    assert!(
        message.contains(&format!("room_elsewhere:{}", node_a.http)),
        "{reply}"
    );
    let mut bob = pa.player("bob").await;
    joined(&bob.call(json!({"t": "join", "name": "arena"})).await);
    assert_eq!(
        alice.until("player_joined").await["player"]["player_id"],
        "bob"
    );
}

#[tokio::test]
async fn stalled_node_lets_go_of_a_room_adopted_elsewhere() {
    let node_a = Node::start_with(FAST).await;
    let node_b = Node::start_with(FAST).await;
    let pa = node_a.project("adopted").await;
    let pb = node_b.same_project(&pa);
    let mut alice = pa.player("alice").await;
    let created = alice.call(json!({"t": "create", "name": "arena"})).await;
    joined(&created);

    // While A is silent a player reaches the room through B, which adopts it.
    let frozen = node_a.freeze(Duration::from_secs(8));
    let adopt = async {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let mut bob = pb.player("bob").await;
        joined(&bob.call(json!({"t": "join", "name": "arena"})).await);
        bob
    };
    let (_, _bob) = tokio::join!(frozen, adopt);

    // Back on its feet, A drops its copy and sends its players to the node that has the room.
    let left = alice.until("left").await;
    assert_eq!(left["reason"], "rehost");
    assert_eq!(left["room"], created["room"]);
    let reply = alice.call(json!({"t": "join", "name": "arena"})).await;
    refused(&reply, "conflict");
    let message = reply["message"].as_str().unwrap_or_default();
    assert!(
        message.contains(&format!("room_elsewhere:{}", node_b.http)),
        "{reply}"
    );
}

#[tokio::test]
async fn room_on_a_node_that_just_started_is_found_from_the_others() {
    let node_a = Node::start().await;
    let node_b = Node::start().await;
    let pa = node_a.project("fresh").await;
    let pb = node_b.same_project(&pa);
    let mut bob = pb.player("bob").await;
    joined(&bob.call(json!({"t": "create", "name": "arena"})).await);

    // A has not refreshed its node list since B registered.
    let mut alice = pa.player("alice").await;
    let reply = alice.call(json!({"t": "join", "name": "arena"})).await;
    refused(&reply, "conflict");
    assert_eq!(pb.room_names().await, ["arena"]);
}

async fn room_id(project: &common::Project, name: &str) -> String {
    let (status, body) = project.api.get("/v1/rooms").await;
    assert_eq!(status, 200, "list rooms: {body}");
    let room = body["rooms"]
        .as_array()
        .and_then(|rooms| rooms.iter().find(|r| r["name"] == name))
        .unwrap_or_else(|| panic!("no room {name} in {body}"));
    room["room"].as_str().expect("room id").to_string()
}

#[tokio::test]
async fn project_key_closes_a_room_hosted_on_another_node() {
    let node_b = Node::start().await;
    let node_a = Node::start().await;
    let pb = node_b.project("close").await;
    let pa = node_a.same_project(&pb);
    let mut bob = pb.player("bob").await;
    joined(&bob.call(json!({"t": "create", "name": "arena"})).await);

    let id = room_id(&pa, "arena").await;
    let (status, reply) = pa.api.delete(&format!("/v1/rooms/{id}")).await;
    assert_eq!(status, 204, "{reply}");
    bob.until("left").await;
}

#[tokio::test]
async fn notice_reaches_a_room_on_a_node_that_just_started() {
    let node_a = Node::start().await;
    let node_b = Node::start().await;
    let pa = node_a.project("notice").await;
    let pb = node_b.same_project(&pa);
    let mut bob = pb.player("bob").await;
    joined(&bob.call(json!({"t": "create", "name": "arena"})).await);

    let id = room_id(&pa, "arena").await;
    let (status, reply) = pa
        .api
        .post(
            &format!("/v1/rooms/{id}/notice"),
            json!({"text": "restart at noon"}),
        )
        .await;
    assert_eq!(status, 200, "{reply}");
    assert_eq!(bob.until("notice").await["text"], "restart at noon");
}

#[tokio::test]
async fn kick_through_another_node_keeps_the_reason() {
    let node_b = Node::start().await;
    let node_a = Node::start().await;
    let pb = node_b.project("kick").await;
    let pa = node_a.same_project(&pb);
    let mut bob = pb.player("bob").await;
    let actor = joined(&bob.call(json!({"t": "create", "name": "arena"})).await);

    let id = room_id(&pa, "arena").await;
    let path = format!("/v1/rooms/{id}/players/{actor}?reason=afk%20too%20long");
    let (status, reply) = pa.api.delete(&path).await;
    assert_eq!(status, 204, "{reply}");
    assert_eq!(bob.until("left").await["reason"], "afk too long");
}
