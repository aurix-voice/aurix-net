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

fn ticket(player: &str) -> Value {
    json!({"player_id": player, "mode": "relay", "max_players": 2, "min_players": 2,
           "max_wait_secs": 30, "props": {}})
}

#[tokio::test]
async fn ticket_is_not_taken_for_a_player_no_grant_would_name() {
    let node = Node::start().await;
    let project = node.project("mm-player-id").await;
    for player in ["", "a\nb", &"x".repeat(129)] {
        let (status, reply) = project
            .api
            .post("/v1/matchmaking/tickets", ticket(player))
            .await;
        assert_eq!((status, &reply["error"]), (400, &json!("invalid_request")));
    }
    let (_, queue) = project.api.get("/v1/matchmaking").await;
    assert_eq!(queue["searching"], json!([]), "{queue}");
    file(&project, ticket(&"x".repeat(128))).await;
}

#[tokio::test]
async fn ticket_is_not_taken_for_a_banned_player() {
    let node = Node::start().await;
    let project = node.project("mm-banned").await;
    let ban = json!({"player_id": "mallory", "reason": "cheat"});
    let (status, reply) = project.api.post("/v1/bans", ban).await;
    assert_eq!(status, 200, "{reply}");

    let (status, reply) = project
        .api
        .post("/v1/matchmaking/tickets", ticket("mallory"))
        .await;
    assert_eq!((status, &reply["error"]), (403, &json!("forbidden")));
    assert_eq!(reply["message"], "forbidden: banned: cheat");
    let (_, queue) = project.api.get("/v1/matchmaking").await;
    assert_eq!(queue["searching"], json!([]), "{queue}");

    // The counter is written off the request.
    for _ in 0..50 {
        let (_, rejections) = project.api.get("/v1/rejections").await;
        if rejections["total"]["banned"] == 1 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the refusal was not counted");
}

#[tokio::test]
async fn ticket_is_not_taken_for_a_room_larger_than_the_project_may_have() {
    let node = Node::start().await;
    let project = node.project("mm-seats").await;
    let limits = format!("/v1/operator/projects/{}/limits", project.id);
    let (status, reply) = project
        .operator
        .post(&limits, json!({"max_players_per_room": 4}))
        .await;
    assert!(status < 300, "{reply}");

    let mut large = ticket("ann");
    large["max_players"] = json!(5);
    let (status, reply) = project.api.post("/v1/matchmaking/tickets", large).await;
    assert_eq!((status, &reply["error"]), (400, &json!("invalid_request")));
    assert_eq!(reply["message"], "invalid request: max_players: at most 4");

    let mut fits = ticket("ann");
    fits["max_players"] = json!(4);
    file(&project, fits).await;
}

#[tokio::test]
async fn cancel_of_a_cancelled_ticket_succeeds_and_of_a_matched_one_is_refused() {
    let node = Node::start().await;
    let project = node.project("mm-cancel-twice").await;

    let lone = file(&project, ticket("ann")).await;
    let path = format!("/v1/matchmaking/tickets/{lone}");
    for _ in 0..2 {
        let (status, reply) = project.api.delete(&path).await;
        assert_eq!(status, 204, "{reply}");
    }
    assert_eq!(state(&project, &lone).await["state"], "cancelled");

    let bob = file(&project, ticket("bob")).await;
    let cat = file(&project, ticket("cat")).await;
    let room = matched(&project, &bob).await;
    let path = format!("/v1/matchmaking/tickets/{bob}");
    let (status, reply) = project.api.delete(&path).await;
    assert_eq!((status, &reply["error"]), (409, &json!("conflict")));
    assert_eq!(matched(&project, &bob).await, room);
    assert_eq!(matched(&project, &cat).await, room);

    let unknown = format!("/v1/matchmaking/tickets/{}", uuid::Uuid::now_v7());
    assert_eq!(project.api.delete(&unknown).await.0, 404);
}

#[tokio::test]
async fn ticket_is_not_taken_with_a_field_the_node_would_have_to_guess_or_cut() {
    let node = Node::start().await;
    let project = node.project("mm-fields").await;
    for (field, bad, good) in [
        ("region", json!("nowhere"), json!("local")),
        ("match_type", json!("fullest"), json!("serial")),
        (
            "props",
            json!({"map": "x".repeat(16_384)}),
            json!({"map": "x".repeat(8_000)}),
        ),
    ] {
        let mut request = ticket("ann");
        request[field] = bad;
        let (status, reply) = project.api.post("/v1/matchmaking/tickets", request).await;
        assert_eq!(
            (status, &reply["error"]),
            (400, &json!("invalid_request")),
            "{field}"
        );

        let mut request = ticket("ann");
        request[field] = good;
        file(&project, request).await;
    }
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
async fn running_room_whose_last_seat_is_held_is_filled_by_its_player_alone() {
    let node = Node::start().await;
    let project = node.project("mm-held").await;
    let mut host = project.player("host").await;
    let held = json!({"t": "create", "name": "a-held", "max_players": 2, "reserved": ["friend"]});
    joined(&host.call(held).await);
    let mut other = project.player("other").await;
    let open = json!({"t": "create", "name": "b-open", "max_players": 2});
    joined(&other.call(open).await);

    // Of two rooms with one player each, the first by name is filled.
    let ann = file(
        &project,
        json!({"player_id": "ann", "max_players": 2, "fill_existing": true}),
    )
    .await;
    assert_eq!(matched(&project, &ann).await, "b-open");

    let friend = file(
        &project,
        json!({"player_id": "friend", "max_players": 2, "fill_existing": true}),
    )
    .await;
    assert_eq!(matched(&project, &friend).await, "a-held");
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

#[tokio::test]
async fn tickets_filed_at_once_by_one_player_leave_one_waiting() {
    let node = Node::start().await;
    let project = &node.project("mm-twice").await;
    let filed = join_all((0..20).map(|_| {
        file(
            project,
            json!({"player_id": "ann", "max_players": 2, "min_players": 2}),
        )
    }))
    .await;
    let states = join_all(filed.iter().map(|ticket| state(project, ticket))).await;
    let count = |name: &str| states.iter().filter(|s| s["state"] == name).count();
    assert_eq!(
        (count("searching"), count("cancelled")),
        (1, 19),
        "{states:?}"
    );
}

#[tokio::test]
async fn filing_holds_while_thousands_wait_in_the_project() {
    let node = Node::start_with(&[
        ("NET__LIMITS__HTTP_RPS_PER_IP", "100000"),
        ("NET__LIMITS__WRITES_PER_SEC_PER_KEY", "100000"),
    ])
    .await;
    let project = &node.project("mm-crowd").await;
    let mut tickets = Vec::new();
    for batch in 0..30 {
        // A map of its own keeps every ticket waiting.
        let filed = join_all((0..100).map(|i| {
            let player = batch * 100 + i;
            let props = json!({"map": player});
            let request =
                json!({"player_id": format!("p{player}"), "max_players": 2, "props": props});
            file(project, request)
        }))
        .await;
        tickets.extend(filed);
    }
    for batch in tickets.chunks(100) {
        join_all(batch.iter().map(|ticket| async move {
            let path = format!("/v1/matchmaking/tickets/{ticket}");
            project.api.delete(&path).await
        }))
        .await;
    }
}
