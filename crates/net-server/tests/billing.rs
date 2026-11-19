//! The month report for invoicing keeps counting a room after the janitor has deleted its row.

mod common;

use common::{joined, Node, Project};
use serde_json::{json, Value};

#[tokio::test]
async fn rooms_created_outlives_the_closed_room_rows() {
    let node = Node::start().await;
    let project = node.project("billing").await;
    let mut ann = project.player("ann").await;
    let reply = ann.call(json!({"t": "create", "name": "short"})).await;
    joined(&reply);
    let room = reply["room"].as_str().expect("room id").to_string();
    let (status, body) = project.api.delete(&format!("/v1/rooms/{room}")).await;
    assert!(status < 300, "close the room: {body}");
    assert_eq!(rooms_created(&project).await, 1);

    let url = std::env::var("NET__DATABASE__URL")
        .unwrap_or_else(|_| "postgres://aurix:aurix@127.0.0.1:5432/aurix_net".into());
    let pool = sqlx::PgPool::connect(&url).await.expect("postgres");
    let aged = sqlx::query(
        "UPDATE rooms SET closed_at = now() - interval '8 days' \
         WHERE project_id = $1::uuid AND state = 'closed'",
    )
    .bind(&project.id)
    .execute(&pool)
    .await
    .expect("age the room");
    assert_eq!(aged.rows_affected(), 1, "the room is closed");
    // What the janitor does every hour with the default `closed_rooms_days`.
    net_db::queries::purge_closed_rooms(&pool, 7)
        .await
        .expect("purge");

    assert_eq!(rooms_created(&project).await, 1);
}

/// `rooms_created` of the project in this month's report.
async fn rooms_created(project: &Project) -> Value {
    let (status, body) = project.operator.get("/v1/operator/billing").await;
    assert_eq!(status, 200, "{body}");
    body["projects"]
        .as_array()
        .expect("projects")
        .iter()
        .find(|row| row["project_id"] == project.id.as_str())
        .expect("the project is in the report")["rooms_created"]
        .clone()
}
