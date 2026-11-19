//! Game modules of a project on a running node.

mod common;

use common::{joined, Node};
use serde_json::json;
use std::time::{Duration, Instant};

/// A module large enough to take seconds to compile.
fn module() -> String {
    let mut wat = String::from(
        "(module (memory (export \"memory\") 1) (func (export \"aurix_init\") (param i64)) \
         (func (export \"aurix_step\"))",
    );
    for f in 0..2000 {
        wat.push_str(&format!(
            "(func (result i32) (i32.add (i32.const 1) (i32.mul (i32.const {f}) (i32.const 7))))"
        ));
    }
    wat.push(')');
    wat
}

#[tokio::test(flavor = "multi_thread")]
async fn compiling_a_room_module_does_not_stall_other_players() {
    let uploader = Node::start().await;
    // One runtime thread, as on a single-core machine.
    let node = Node::start_with(&[("TOKIO_WORKER_THREADS", "1")]).await;
    let project = uploader.project("modules").await;
    let resp = reqwest::Client::new()
        .put(format!("{}/v1/modules/big", project.api.http))
        .bearer_auth(&project.api.token)
        .body(module())
        .send()
        .await
        .expect("upload");
    assert!(resp.status().is_success(), "upload: {}", resp.status());

    let project = node.same_project(&project);
    let mut ann = project.player("ann").await;
    let mut bob = project.player("bob").await;
    let create = tokio::spawn(async move {
        let props = json!({"tick_hz": 30, "logic": "big"});
        bob.send(json!({"t": "create", "name": "arena", "mode": "sync", "props": props}))
            .await;
        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline {
            if let Some(reply) = bob.poll(Duration::from_secs(1)).await {
                return reply;
            }
        }
        panic!("the room was not created");
    });

    let mut worst = Duration::ZERO;
    while !create.is_finished() {
        let sent = Instant::now();
        ann.send(json!({"t": "ping", "ts": 1})).await;
        ann.until("pong").await;
        worst = worst.max(sent.elapsed());
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    joined(&create.await.expect("create"));
    assert!(worst < Duration::from_secs(1), "a pong took {worst:?}");
}
