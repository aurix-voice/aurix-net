//! Matchmaking tickets through the REST API of a running node.

mod common;

use common::{joined, Node, Project, Sink};
use futures::future::join_all;
use serde_json::{json, Value};
use std::time::Duration;

async fn file(p: &Project, body: Value) -> String {
    let (status, reply) = p.api.post("/v1/matchmaking/tickets", body).await;
    assert_eq!(status, 200, "{reply}");
    reply["ticket"].as_str().expect("ticket id").to_string()
}

async fn state(p: &Project, ticket: &str) -> Value {
    let (status, reply) = p
        .api
        .get(&format!("/v1/matchmaking/tickets/{ticket}"))
        .await;
    assert_eq!(status, 200, "{reply}");
    reply["state"].clone()
}

/// Polls the ticket until it is matched; returns its room.
async fn matched(p: &Project, ticket: &str) -> String {
    for _ in 0..50 {
        let s = state(p, ticket).await;
        if s["state"] == "matched" {
            return s["room"].as_str().expect("room").to_string();
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("ticket {ticket} was not matched");
}

#[tokio::test]
async fn short_group_starts_without_the_player_who_wants_more() {
    let node = Node::start().await;
    let p = node.project("mm-min").await;
    // Ann accepts two players after a second; Ben wants a full table of four.
    let ann = file(
        &p,
        json!({"player_id": "ann", "max_players": 4, "min_players": 2, "max_wait_secs": 1}),
    )
    .await;
    let ben = file(
        &p,
        json!({"player_id": "ben", "max_players": 4, "min_players": 4, "max_wait_secs": 60}),
    )
    .await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(state(&p, &ann).await["state"], "searching");
    assert_eq!(state(&p, &ben).await["state"], "searching");

    // Cid accepts two as well: Ann and Cid get a room, Ben keeps waiting for his four.
    let cid = file(
        &p,
        json!({"player_id": "cid", "max_players": 4, "min_players": 2, "max_wait_secs": 60}),
    )
    .await;
    let room = matched(&p, &ann).await;
    assert_eq!(matched(&p, &cid).await, room);
    let ann_state = state(&p, &ann).await;
    assert_eq!(ann_state["players"], json!(["ann", "cid"]));
    assert_eq!(state(&p, &ben).await["state"], "searching");
}

#[tokio::test]
async fn match_found_names_every_player_seated_in_a_running_room() {
    let node = Node::start().await;
    let p = node.project("mm-fill").await;
    let mut sink = Sink::start().await;
    let (status, reply) = p
        .api
        .post(
            "/v1/webhooks",
            json!({"url": sink.url, "events": ["match.found"]}),
        )
        .await;
    assert_eq!(status, 200, "{reply}");
    let mut host = p.player("host").await;
    joined(
        &host
            .call(json!({"t": "create", "name": "open", "max_players": 4}))
            .await,
    );

    // Two players ask for a seat in a running room at the same moment.
    let seat = |player: &'static str| {
        file(
            &p,
            json!({"player_id": player, "max_players": 4, "fill_existing": true}),
        )
    };
    let (ann, ben) = tokio::join!(seat("ann"), seat("ben"));
    assert_eq!(matched(&p, &ann).await, "open");
    assert_eq!(matched(&p, &ben).await, "open");

    // One webhook per matching pass; each names as many players as tickets.
    let mut players = Vec::new();
    while players.len() < 2 {
        let data = sink.next("match.found").await["data"].clone();
        assert_eq!(data["room"], "open");
        let tickets = data["tickets"].as_array().expect("tickets").len();
        let named = data["players"].as_array().expect("players").clone();
        assert_eq!(named.len(), tickets, "{data}");
        players.extend(named);
    }
    players.sort_by_key(|v| v.to_string());
    assert_eq!(players, [json!("ann"), json!("ben")]);
}

#[tokio::test]
async fn cancel_holds_against_a_matching_pass_already_running() {
    let node = Node::start_with(&[
        ("NET__LIMITS__HTTP_RPS_PER_IP", "100000"),
        ("NET__LIMITS__WRITES_PER_SEC_PER_KEY", "100000"),
    ])
    .await;
    let p = &node.project("mm-cancel").await;
    let tickets = join_all((0..300).map(|i| {
        file(
            p,
            json!({"player_id": format!("p{i}"), "max_players": 2, "min_players": 2}),
        )
    }))
    .await;
    // Spread over one pass of the matchmaker, so some cancels land while it is writing.
    let cancelled = join_all(tickets.iter().enumerate().map(|(i, ticket)| async move {
        tokio::time::sleep(Duration::from_micros(1500 * i as u64)).await;
        let path = format!("/v1/matchmaking/tickets/{ticket}");
        p.api.delete(&path).await.0 == 204
    }))
    .await;
    tokio::time::sleep(Duration::from_secs(1)).await;

    let states = join_all(tickets.iter().map(|ticket| state(p, ticket))).await;
    for (i, s) in states.iter().enumerate() {
        if cancelled[i] {
            assert_eq!(
                s["state"], "cancelled",
                "p{i} was told the ticket is cancelled"
            );
        }
    }
    for s in states.iter().filter(|s| s["state"] == "matched") {
        for player in s["players"].as_array().expect("players") {
            let i: usize = player.as_str().expect("player id")[1..]
                .parse()
                .expect("player number");
            assert_eq!(
                states[i]["room"], s["room"],
                "{player} is not in the room of {s}"
            );
        }
    }
}
