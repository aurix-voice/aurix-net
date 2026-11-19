//! Matchmaking tickets through the REST API of a running node.

mod common;

use common::{joined, Node, Project, Sink};
use futures::future::join_all;
use serde_json::{json, Value};
use std::time::{Duration, Instant};

async fn file(project: &Project, body: Value) -> String {
    let (status, reply) = project.api.post("/v1/matchmaking/tickets", body).await;
    assert_eq!(status, 200, "{reply}");
    reply["ticket"].as_str().expect("ticket id").to_string()
}

async fn state(project: &Project, ticket: &str) -> Value {
    let (status, reply) = project
        .api
        .get(&format!("/v1/matchmaking/tickets/{ticket}"))
        .await;
    assert_eq!(status, 200, "{reply}");
    reply["state"].clone()
}

/// Polls the ticket until it is matched; returns its room.
async fn matched(project: &Project, ticket: &str) -> String {
    for _ in 0..50 {
        let current = state(project, ticket).await;
        if current["state"] == "matched" {
            return current["room"].as_str().expect("room").to_string();
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("ticket {ticket} was not matched");
}

#[tokio::test]
async fn short_group_starts_without_the_player_who_wants_more() {
    let node = Node::start().await;
    let project = node.project("mm-min").await;
    // Ann accepts two players after a second; Ben wants a full table of four.
    let ann = file(
        &project,
        json!({"player_id": "ann", "max_players": 4, "min_players": 2, "max_wait_secs": 1}),
    )
    .await;
    let ben = file(
        &project,
        json!({"player_id": "ben", "max_players": 4, "min_players": 4, "max_wait_secs": 60}),
    )
    .await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(state(&project, &ann).await["state"], "searching");
    assert_eq!(state(&project, &ben).await["state"], "searching");

    // Cid accepts two as well: Ann and Cid get a room, Ben keeps waiting for his four.
    let cid = file(
        &project,
        json!({"player_id": "cid", "max_players": 4, "min_players": 2, "max_wait_secs": 60}),
    )
    .await;
    let room = matched(&project, &ann).await;
    assert_eq!(matched(&project, &cid).await, room);
    let ann_state = state(&project, &ann).await;
    assert_eq!(ann_state["players"], json!(["ann", "cid"]));
    assert_eq!(state(&project, &ben).await["state"], "searching");
}

#[tokio::test]
async fn match_found_names_every_player_seated_in_a_running_room() {
    let node = Node::start().await;
    let project = node.project("mm-fill").await;
    let mut sink = Sink::start().await;
    let (status, reply) = project
        .api
        .post(
            "/v1/webhooks",
            json!({"url": sink.url, "events": ["match.found"]}),
        )
        .await;
    assert_eq!(status, 200, "{reply}");
    let mut host = project.player("host").await;
    joined(
        &host
            .call(json!({"t": "create", "name": "open", "max_players": 4}))
            .await,
    );

    // Two players ask for a seat in a running room at the same moment.
    let seat = |player: &'static str| {
        file(
            &project,
            json!({"player_id": player, "max_players": 4, "fill_existing": true}),
        )
    };
    let (ann, ben) = tokio::join!(seat("ann"), seat("ben"));
    assert_eq!(matched(&project, &ann).await, "open");
    assert_eq!(matched(&project, &ben).await, "open");

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
    let project = &node.project("mm-cancel").await;
    let tickets = join_all((0..300).map(|i| {
        file(
            project,
            json!({"player_id": format!("p{i}"), "max_players": 2, "min_players": 2}),
        )
    }))
    .await;
    // Spread over one pass of the matchmaker, so some cancels land while it is writing.
    let cancelled = join_all(tickets.iter().enumerate().map(|(i, ticket)| async move {
        tokio::time::sleep(Duration::from_micros(1500 * i as u64)).await;
        let path = format!("/v1/matchmaking/tickets/{ticket}");
        project.api.delete(&path).await.0 == 204
    }))
    .await;
    tokio::time::sleep(Duration::from_secs(1)).await;

    let states = join_all(tickets.iter().map(|ticket| state(project, ticket))).await;
    for (i, reply) in states.iter().enumerate() {
        if cancelled[i] {
            assert_eq!(
                reply["state"], "cancelled",
                "p{i} was told the ticket is cancelled"
            );
        }
    }
    for reply in states.iter().filter(|s| s["state"] == "matched") {
        for player in reply["players"].as_array().expect("players") {
            let index: usize = player.as_str().expect("player id")[1..]
                .parse()
                .expect("player number");
            assert_eq!(
                states[index]["room"], reply["room"],
                "{player} is not in the room of {reply}"
            );
        }
    }
}

#[tokio::test]
async fn key_writes_are_cut_off_past_the_burst_and_come_back() {
    let node = Node::start_with(&[("NET__LIMITS__WRITES_PER_SEC_PER_KEY", "2")]).await;
    let project = node.project("mm-limit").await;
    let ticket = |i: usize| {
        project.api.post(
            "/v1/matchmaking/tickets",
            json!({"player_id": format!("p{i}"), "max_players": 8, "min_players": 8}),
        )
    };
    // Two a second with a burst of twice that, however slowly this machine sends.
    let started = Instant::now();
    let mut accepted = 0;
    for i in 0..40 {
        match ticket(i).await.0 {
            200 => accepted += 1,
            429 => {}
            status => panic!("write {i}: {status}"),
        }
    }
    let allowed = 4 + (2.0 * started.elapsed().as_secs_f64()).ceil() as usize;
    assert!(
        (4..=allowed).contains(&accepted),
        "{accepted} of 40 writes accepted, {allowed} allowed"
    );
    assert!(accepted < 40, "no write was refused");

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut status = 429;
    let mut i = 40;
    while status == 429 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
        status = ticket(i).await.0;
        i += 1;
    }
    assert_eq!(status, 200, "writes did not come back");
}
