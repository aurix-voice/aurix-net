//! Traffic is billed to the project whether or not the player is still in the room when the
//! node's metering pass comes, and counts against the project's monthly cap.

mod common;

use bytes::Bytes;
use common::{refused, Node, Project};
use net_client::{Client, Options};
use net_rooms::event::EventTarget;
use net_rooms::protocol::ClientMsg;
use serde_json::json;
use std::time::{Duration, Instant};

const EVENTS: usize = 100;
const PAYLOAD: usize = 500;

async fn player(node: &Node, project: &Project, name: &str) -> Client {
    let opts = Options {
        tunnel: true,
        ..Default::default()
    };
    let mut client = Client::connect(&node.http, &project.grant(name).await, opts)
        .await
        .expect("connects");
    let join: ClientMsg =
        serde_json::from_value(json!({"t": "join_or_create", "name": "arena"})).expect("message");
    client.join(join).await.expect("joins");
    client
}

async fn send(client: &Client) {
    for _ in 0..EVENTS {
        client
            .send_event(1, EventTarget::Others, Bytes::from(vec![7; PAYLOAD]), true)
            .expect("sends");
        client.flush();
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn bytes_in(project: &Project) -> f64 {
    let (status, body) = project.api.get("/v1/usage").await;
    assert_eq!(status, 200, "{body}");
    body["usage"]
        .as_array()
        .expect("usage rows")
        .iter()
        .filter(|row| row["metric"] == "bytes_in")
        .map(|row| row["quantity"].as_f64().expect("a quantity"))
        .sum()
}

#[tokio::test]
async fn traffic_of_a_player_who_left_before_the_metering_pass_is_billed() {
    let node = Node::start().await;
    let started = Instant::now();
    let stays = node.project("stays").await;
    let leaves = node.project("leaves").await;

    let staying = player(&node, &stays, "ann").await;
    let mut leaving = player(&node, &leaves, "bob").await;
    send(&staying).await;
    send(&leaving).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    leaving.leave().await.expect("bob leaves");
    drop(leaving);

    // The node meters once a minute.
    tokio::time::sleep(Duration::from_secs(63).saturating_sub(started.elapsed())).await;
    let sent = (EVENTS * PAYLOAD) as f64;
    let stayed = bytes_in(&stays).await;
    let left = bytes_in(&leaves).await;
    assert!(stayed >= sent, "the player who stayed: {stayed} bytes");
    assert!(left >= sent, "the player who left: {left} bytes");

    let limits = format!("/v1/operator/projects/{}/limits", leaves.id);
    let (status, body) = leaves
        .operator
        .post(&limits, json!({"monthly_bytes": 1}))
        .await;
    assert!(status < 300, "limits: {body}");
    let mut eve = leaves.player("eve").await;
    refused(
        &eve.call(json!({"t": "create", "name": "late"})).await,
        "quota_exceeded",
    );
}
