//! Project limits an operator sets through the REST API of a running node.

mod common;

use common::{joined, refused, Node, Project, Sink};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

async fn set_limits(project: &Project, limits: Value) -> (u16, Value) {
    let path = format!("/v1/operator/projects/{}/limits", project.id);
    project.operator.post(&path, limits).await
}

#[tokio::test]
async fn limits_that_do_not_parse_are_refused_and_change_nothing() {
    let node = Node::start().await;
    let project = node.project("limits").await;
    let (status, body) = set_limits(&project, json!({"max_rooms": 1})).await;
    assert!(status < 300, "{body}");

    for (bad, field) in [
        (
            json!({"max_rooms": 1, "max_players_per_room": 100_000}),
            "max_players_per_room",
        ),
        (
            json!({"max_rooms": 1, "monthly_bytes": "1"}),
            "monthly_bytes",
        ),
        (json!({"max_rooms": 1, "max_room": 2}), "max_room"),
    ] {
        let (status, body) = set_limits(&project, bad.clone()).await;
        assert_eq!(status, 400, "{bad}: {body}");
        let message = body["message"].as_str().expect("message");
        assert!(message.contains(field), "{message}");
    }

    let mut ann = project.player("ann").await;
    joined(&ann.call(json!({"t": "create", "name": "one"})).await);
    let mut bob = project.player("bob").await;
    refused(
        &bob.call(json!({"t": "create", "name": "two"})).await,
        "quota_exceeded",
    );
}

#[tokio::test]
async fn limits_of_an_unknown_project_are_not_found() {
    let node = Node::start().await;
    let project = node.project("limits").await;
    let path = "/v1/operator/projects/00000000-0000-7000-8000-000000000000/limits";
    let (status, body) = project.operator.post(path, json!({"max_rooms": 1})).await;
    assert_eq!(status, 404, "{body}");
}

/// Stores limits the API would not have taken, the way only a hand in the database can.
async fn store_unreadable_limits(project: &Project) {
    let url = std::env::var("NET__DATABASE__URL")
        .unwrap_or_else(|_| "postgres://aurix:aurix@127.0.0.1:5432/aurix_net".into());
    let pool = sqlx::PgPool::connect(&url).await.expect("postgres");
    sqlx::query(
        "UPDATE projects SET limits = '{\"max_rooms\": 1, \"max_players_per_room\": 100000}' \
         WHERE id = $1::uuid",
    )
    .bind(&project.id)
    .execute(&pool)
    .await
    .expect("write the limits");
}

#[tokio::test]
async fn project_with_unreadable_stored_limits_is_not_left_unlimited() {
    let node = Node::start().await;
    let project = node.project("limits").await;
    store_unreadable_limits(&project).await;

    let mut ann = project.player("ann").await;
    let reply = ann.call(json!({"t": "create", "name": "one"})).await;
    refused(&reply, "quota_exceeded");
    assert_eq!(
        reply["message"],
        "quota exceeded: the project's limits are unreadable; an operator has to set them again"
    );

    let (status, body) = project.operator.get("/v1/operator/attention").await;
    assert_eq!(status, 200, "{body}");
    let listed = body["invalid_limits"]
        .as_array()
        .expect("invalid_limits")
        .iter()
        .any(|row| row["project_id"] == project.id.as_str());
    assert!(listed, "the operator is not told: {body}");
    let detail = format!("/v1/operator/projects/{}", project.id);
    let (status, body) = project.operator.get(&detail).await;
    assert_eq!(status, 200, "{body}");
    assert!(body["limits_error"].is_string(), "{body}");

    let (status, body) = set_limits(&project, json!({"max_rooms": 1})).await;
    assert!(status < 300, "{body}");
    joined(&ann.call(json!({"t": "create", "name": "one"})).await);
}

#[tokio::test]
async fn operator_is_alerted_of_a_project_with_unreadable_stored_limits() {
    let node = Node::start().await;
    let project = node.project("limits-alert").await;
    let mut sink = Sink::start().await;
    let hook = json!({"url": sink.url, "events": ["project.invalid_limits"]});
    let (status, hook) = project.operator.post("/v1/operator/webhooks", hook).await;
    assert_eq!(status, 200, "{hook}");
    store_unreadable_limits(&project).await;

    let mut ann = project.player("ann").await;
    refused(
        &ann.call(json!({"t": "create", "name": "one"})).await,
        "quota_exceeded",
    );
    let path = format!("/v1/operator/projects/{}/rejections", project.id);
    let mut turned_away = Value::Null;
    for _ in 0..20 {
        turned_away = project.operator.get(&path).await.1;
        if turned_away["total"]["invalid_limits"] == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(turned_away["total"], json!({"invalid_limits": 1}));

    // One node of the fleet looks for alerts, and keeps that place for 90 s after its last
    // look: a node of an earlier test may have left with it.
    let redis =
        std::env::var("NET__REDIS__URL").unwrap_or_else(|_| "redis://127.0.0.1:6379/".into());
    let mut redis = redis::Client::open(redis)
        .expect("redis url")
        .get_multiplexed_async_connection()
        .await
        .expect("redis");
    let _: i64 = redis::cmd("DEL")
        .arg("aurix:alerts:leader")
        .query_async(&mut redis)
        .await
        .expect("del");
    let asked = Instant::now();
    let alert = loop {
        let left = Duration::from_secs(45)
            .checked_sub(asked.elapsed())
            .expect("an alert of the unreadable limits");
        match sink.poll(left).await {
            Some(alert) if alert["data"]["project_id"] == project.id.as_str() => break alert,
            _ => {}
        }
    };
    assert_eq!(alert["event"], "project.invalid_limits", "{alert}");
    assert!(alert["data"]["error"].is_string(), "{alert}");

    let hook = format!("/v1/operator/webhooks/{}", hook["id"].as_str().expect("id"));
    assert_eq!(project.operator.delete(&hook).await.0, 204);
}
