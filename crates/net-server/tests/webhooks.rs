//! Webhook subscriptions of a project on a running node.

mod common;

use common::{joined, Node, Sink};
use serde_json::json;
use std::time::{Duration, Instant};

#[tokio::test]
async fn webhook_created_after_the_first_event_gets_the_next_one() {
    let node = Node::start().await;
    let project = node.project("hooks-new").await;
    let mut alice = project.player("alice").await;
    joined(&alice.call(json!({"t": "create", "name": "arena"})).await);
    tokio::time::sleep(Duration::from_millis(300)).await;

    let mut sink = Sink::start().await;
    let hook = json!({"url": sink.url, "events": ["player.joined"]});
    let (status, reply) = project.api.post("/v1/webhooks", hook).await;
    assert_eq!(status, 200, "{reply}");
    let mut bob = project.player("bob").await;
    let actor = joined(&bob.call(json!({"t": "join", "name": "arena"})).await);
    assert_eq!(sink.next("player.joined").await["data"]["actor"], actor);
}

#[tokio::test]
async fn deleted_webhook_gets_nothing_more() {
    let node = Node::start().await;
    let project = node.project("hooks-gone").await;
    let mut sink = Sink::start().await;
    let hook = json!({"url": sink.url, "events": ["player.joined"]});
    let (status, reply) = project.api.post("/v1/webhooks", hook).await;
    assert_eq!(status, 200, "{reply}");
    let mut alice = project.player("alice").await;
    joined(&alice.call(json!({"t": "create", "name": "arena"})).await);
    sink.next("player.joined").await;

    let path = format!("/v1/webhooks/{}", reply["id"].as_str().expect("webhook id"));
    assert_eq!(project.api.delete(&path).await.0, 204);
    let mut bob = project.player("bob").await;
    joined(&bob.call(json!({"t": "join", "name": "arena"})).await);
    assert!(
        sink.quiet(Duration::from_secs(1)).await,
        "a deleted webhook was called"
    );
}

#[tokio::test]
async fn webhook_created_on_one_node_gets_the_events_of_another() {
    let node_a = Node::start().await;
    let node_b = Node::start().await;
    let pa = node_a.project("hooks-far").await;
    let pb = node_b.same_project(&pa);
    let mut alice = pb.player("alice").await;
    joined(&alice.call(json!({"t": "create", "name": "arena"})).await);
    tokio::time::sleep(Duration::from_millis(300)).await;

    let mut sink = Sink::start().await;
    let hook = json!({"url": sink.url, "events": ["player.joined"]});
    let (status, reply) = pa.api.post("/v1/webhooks", hook).await;
    assert_eq!(status, 200, "{reply}");
    // The notice reaches node B through Redis, so the first joins may still be too early.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut players = Vec::new();
    loop {
        let mut player = pb.player(&format!("bob{}", players.len())).await;
        joined(&player.call(json!({"t": "join", "name": "arena"})).await);
        players.push(player);
        if !sink.quiet(Duration::from_millis(500)).await {
            break;
        }
        assert!(Instant::now() < deadline, "no `player.joined` webhook");
    }
}

#[tokio::test]
async fn webhook_deleted_on_one_node_is_not_called_by_another() {
    let node_a = Node::start().await;
    let node_b = Node::start().await;
    let pa = node_a.project("hooks-far-gone").await;
    let pb = node_b.same_project(&pa);
    let mut sink = Sink::start().await;
    let hook = json!({"url": sink.url, "events": ["player.joined"]});
    let (status, reply) = pa.api.post("/v1/webhooks", hook).await;
    assert_eq!(status, 200, "{reply}");
    let mut alice = pb.player("alice").await;
    joined(&alice.call(json!({"t": "create", "name": "arena"})).await);
    sink.next("player.joined").await;

    let path = format!("/v1/webhooks/{}", reply["id"].as_str().expect("webhook id"));
    assert_eq!(pa.api.delete(&path).await.0, 204);
    // The notice reaches node B through Redis, so the first joins may still be too early.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut players = Vec::new();
    loop {
        let mut player = pb.player(&format!("bob{}", players.len())).await;
        joined(&player.call(json!({"t": "join", "name": "arena"})).await);
        players.push(player);
        if sink.quiet(Duration::from_secs(1)).await {
            break;
        }
        assert!(Instant::now() < deadline, "a deleted webhook was called");
    }
}

#[tokio::test]
async fn secret_rotated_on_one_node_signs_the_events_of_another() {
    let node_a = Node::start().await;
    let node_b = Node::start().await;
    let pa = node_a.project("hooks-far-rotated").await;
    let pb = node_b.same_project(&pa);
    let mut sink = Sink::start().await;
    let hook = json!({"url": sink.url, "events": ["player.joined"]});
    let (status, reply) = pa.api.post("/v1/webhooks", hook).await;
    assert_eq!(status, 200, "{reply}");
    let mut alice = pb.player("alice").await;
    joined(&alice.call(json!({"t": "create", "name": "arena"})).await);
    let (headers, _) = sink.next_with_headers("player.joined").await;
    assert!(!headers.contains_key("x-aurix-signature-previous"));

    let path = format!(
        "/v1/webhooks/{}/rotate",
        reply["id"].as_str().expect("webhook id")
    );
    let (status, reply) = pa.api.post(&path, json!({"grace_secs": 600})).await;
    assert_eq!(status, 200, "{reply}");
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut bob = pb.player("bob").await;
    joined(&bob.call(json!({"t": "join", "name": "arena"})).await);
    let (headers, _) = sink.next_with_headers("player.joined").await;
    assert!(
        headers.contains_key("x-aurix-signature-previous"),
        "the delivery is still signed with the old secret alone"
    );
}

/// Answers each delivery with the next status of `answers`, then with 200.
async fn receiver(answers: &'static [u16]) -> String {
    counting_receiver(answers).await.0
}

/// `receiver` and the number of deliveries it has had.
async fn counting_receiver(
    answers: &'static [u16],
) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = calls.clone();
    let app = axum::Router::new().fallback(axum::routing::post(move || {
        let call = calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let status = answers.get(call).copied().unwrap_or(200);
        async move { axum::http::StatusCode::from_u16(status).expect("status") }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("receiver port");
    let url = format!("http://{}/hook", listener.local_addr().expect("address"));
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (url, seen)
}

// `terminate` blocks its thread; the receiver needs one of its own to answer meanwhile.
#[tokio::test(flavor = "multi_thread")]
async fn delivery_under_way_is_finished_before_the_node_exits() {
    let mut node = Node::start().await;
    let project = node.project("hooks-exit").await;
    let (url, calls) = counting_receiver(&[503]).await;
    let (status, hook) = project
        .api
        .post("/v1/webhooks", json!({"url": url, "events": ["test"]}))
        .await;
    assert_eq!(status, 200, "{hook}");
    let id = hook["id"].as_str().expect("webhook id");
    let (status, reply) = project
        .api
        .post(&format!("/v1/webhooks/{id}/test"), json!({}))
        .await;
    assert_eq!(status, 200, "{reply}");

    // The receiver turns the first attempt down; the second is due half a second later.
    node.terminate();
    assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn request_that_ends_after_the_node_stopped_listening_has_its_webhook_delivered() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut node = Node::start().await;
    let project = node.project("hooks-late").await;
    let (url, calls) = counting_receiver(&[]).await;
    let (status, hook) = project
        .api
        .post("/v1/webhooks", json!({"url": url, "events": ["test"]}))
        .await;
    assert_eq!(status, 200, "{hook}");
    let id = hook["id"].as_str().expect("webhook id");

    // The request is under way when the node is told to stop: its body comes afterwards.
    let address = node.http.trim_start_matches("http://");
    let mut request = tokio::net::TcpStream::connect(address)
        .await
        .expect("connects");
    let head = format!(
        "POST /v1/webhooks/{id}/test HTTP/1.1\r\nhost: {address}\r\n\
         authorization: Bearer {}\r\ncontent-type: application/json\r\n\
         content-length: 2\r\nconnection: close\r\n\r\n",
        project.api.token
    );
    request.write_all(head.as_bytes()).await.expect("head");
    tokio::time::sleep(Duration::from_millis(200)).await;
    node.stop();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !node.log_tail().contains("shutdown: rooms handed off") {
        assert!(Instant::now() < deadline, "the node did not start to stop");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    request.write_all(b"{}").await.expect("body");
    let mut reply = String::new();
    request.read_to_string(&mut reply).await.expect("reply");
    assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");

    node.terminate();
    assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn receiver_that_does_not_answer_holds_the_node_for_its_flush_time_only() {
    let mut node = Node::start().await;
    let project = node.project("hooks-hang").await;
    let app = axum::Router::new().fallback(axum::routing::post(|| async {
        tokio::time::sleep(Duration::from_secs(60)).await;
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("receiver port");
    let url = format!("http://{}/hook", listener.local_addr().expect("address"));
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let (status, hook) = project
        .api
        .post("/v1/webhooks", json!({"url": url, "events": ["test"]}))
        .await;
    assert_eq!(status, 200, "{hook}");
    let id = hook["id"].as_str().expect("webhook id");
    let (status, reply) = project
        .api
        .post(&format!("/v1/webhooks/{id}/test"), json!({}))
        .await;
    assert_eq!(status, 200, "{reply}");
    tokio::time::sleep(Duration::from_millis(300)).await;

    let asked = Instant::now();
    node.terminate();
    let took = asked.elapsed();
    assert!(
        node.log_tail()
            .contains("shutdown: webhooks still undelivered"),
        "{}",
        node.log_tail()
    );
    assert!(took < Duration::from_secs(8), "the node took {took:?}");
}

/// The log entry of the one `test` delivery to a webhook at `url`.
async fn test_delivery(project: &common::Project, url: String) -> serde_json::Value {
    let (status, hook) = project
        .api
        .post("/v1/webhooks", json!({"url": url, "events": ["test"]}))
        .await;
    assert_eq!(status, 200, "{hook}");
    let id = hook["id"].as_str().expect("webhook id");
    let (status, reply) = project
        .api
        .post(&format!("/v1/webhooks/{id}/test"), json!({}))
        .await;
    assert_eq!(status, 200, "{reply}");
    for _ in 0..50 {
        let (_, log) = project
            .api
            .get(&format!("/v1/webhooks/{id}/deliveries"))
            .await;
        if let Some(delivery) = log["deliveries"].get(0) {
            return delivery.clone();
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("no delivery in the log of {id}");
}

#[tokio::test]
async fn delivery_is_tried_three_times_and_a_refusal_once() {
    let node = Node::start().await;
    let project = node.project("hooks-retry").await;

    let recovered = test_delivery(&project, receiver(&[500, 503]).await).await;
    assert_eq!(recovered["ok"], true, "{recovered}");
    assert_eq!(recovered["attempts"], 3, "{recovered}");

    let down = test_delivery(&project, receiver(&[500, 500, 500]).await).await;
    assert_eq!(down["ok"], false, "{down}");
    assert_eq!(down["attempts"], 3, "{down}");
    assert_eq!(down["status"], 500, "{down}");

    let refused = test_delivery(&project, receiver(&[410]).await).await;
    assert_eq!(refused["ok"], false, "{refused}");
    assert_eq!(refused["attempts"], 1, "{refused}");
    assert_eq!(refused["status"], 410, "{refused}");
}

/// Answers every call with `answer`.
async fn backend(answer: serde_json::Value) -> String {
    let app = axum::Router::new().fallback(axum::routing::post(move || {
        let answer = answer.clone();
        async move { axum::Json(answer) }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("backend port");
    let url = format!("http://{}/hook", listener.local_addr().expect("address"));
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    url
}

#[tokio::test]
async fn join_validator_refuses_and_stops_when_deleted() {
    let node = Node::start().await;
    let project = node.project("hooks-validate").await;
    let mut alice = project.player("alice").await;
    joined(&alice.call(json!({"t": "create", "name": "arena"})).await);

    let url = backend(json!({"allow": false, "reason": "not on the list"})).await;
    let hook = json!({"url": url, "events": ["join.validate"]});
    let (status, hook) = project.api.post("/v1/webhooks", hook).await;
    assert_eq!(status, 200, "{hook}");
    let mut bob = project.player("bob").await;
    let reply = bob.call(json!({"t": "join", "name": "arena"})).await;
    common::refused(&reply, "forbidden");
    let message = reply["message"].as_str().unwrap_or_default();
    assert!(
        message.ends_with("join rejected: not on the list"),
        "{reply}"
    );

    let path = format!("/v1/webhooks/{}", hook["id"].as_str().expect("webhook id"));
    assert_eq!(project.api.delete(&path).await.0, 204);
    joined(&bob.call(json!({"t": "join", "name": "arena"})).await);
}

#[tokio::test]
async fn rpc_reaches_the_backend_once_it_is_subscribed() {
    let node = Node::start().await;
    let project = node.project("hooks-rpc").await;
    let mut alice = project.player("alice").await;
    alice
        .send(json!({"t": "rpc", "id": 1, "name": "shop.buy"}))
        .await;
    let none = alice.until("rpc_result").await;
    assert_eq!(none["ok"], false, "{none}");

    let url = backend(json!({"coins": 7})).await;
    let hook = json!({"url": url, "events": ["rpc"]});
    let (status, hook) = project.api.post("/v1/webhooks", hook).await;
    assert_eq!(status, 200, "{hook}");
    alice
        .send(json!({"t": "rpc", "id": 2, "name": "shop.buy"}))
        .await;
    let bought = alice.until("rpc_result").await;
    assert_eq!(bought["ok"], true, "{bought}");
    assert_eq!(bought["body"]["coins"], 7, "{bought}");
}

#[tokio::test]
async fn room_continued_on_another_node_is_reported_with_what_was_restored() {
    let fast = &[("NET__SERVER__HEARTBEAT_SECS", "1")];
    let node_a = Node::start_with(fast).await;
    let node_b = Node::start_with(fast).await;
    let pa = node_a.project("hooks-rehost").await;
    let pb = node_b.same_project(&pa);
    let mut sink = Sink::start().await;
    let (status, hook) = pa
        .api
        .post(
            "/v1/webhooks",
            json!({"url": sink.url, "events": ["room.rehosted"]}),
        )
        .await;
    assert_eq!(status, 200, "{hook}");
    let mut ann = pa.player("ann").await;
    ann.send(json!({"t": "create", "name": "arena", "mode": "lockstep"}))
        .await;
    ann.until("joined").await;
    // A lockstep room is checkpointed every five seconds.
    tokio::time::sleep(Duration::from_secs(6)).await;

    let frozen = node_a.freeze(Duration::from_secs(8));
    let moved = async {
        tokio::time::sleep(Duration::from_secs(4)).await;
        let mut bob = pb.player("bob").await;
        bob.send(json!({"t": "join", "name": "arena"})).await;
        bob.until("joined").await;
        sink.next("room.rehosted").await["data"].clone()
    };
    let (_, data) = tokio::join!(frozen, moved);
    assert_eq!(data["restored"], "lockstep", "{data}");
    assert_eq!(data["mode"], "lockstep", "{data}");
}
