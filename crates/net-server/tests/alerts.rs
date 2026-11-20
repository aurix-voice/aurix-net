//! The work one node does for the whole fleet, when that node stops. The tests of this file do
//! not share Redis with a node of another test: who holds a lock is then known.

mod common;

use common::Node;
use std::time::Duration;

/// Who looks for operator alerts, and who makes the matches.
const LOCKS: [&str; 2] = ["aurix:alerts:leader", "net:mm:leader"];

async fn node_id(node: &Node) -> String {
    let project = node.project("locks").await;
    let (status, body) = project.operator.get("/v1/operator/nodes").await;
    assert_eq!(status, 200, "{body}");
    let nodes = body["nodes"].as_array().expect("nodes");
    let own = nodes
        .iter()
        .find(|row| row["http_url"] == node.http.as_str());
    own.expect("the node lists itself")["id"]
        .as_str()
        .expect("id")
        .to_string()
}

async fn holder(redis: &mut redis::aio::MultiplexedConnection, lock: &str) -> Option<String> {
    redis::cmd("GET")
        .arg(lock)
        .query_async(redis)
        .await
        .expect("get")
}

/// Waits until `node` holds every lock; a lock left by a node that was killed runs out first.
async fn leads(redis: &mut redis::aio::MultiplexedConnection, node: &str, wait: Duration) {
    for lock in LOCKS {
        let asked = std::time::Instant::now();
        while holder(redis, lock).await.as_deref() != Some(node) {
            assert!(asked.elapsed() < wait, "{lock} is not held by the node");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

#[tokio::test]
async fn node_that_stops_leaves_its_locks_to_the_next_node() {
    let url = std::env::var("NET__REDIS__URL").unwrap_or_else(|_| "redis://127.0.0.1:6379/".into());
    let mut redis = redis::Client::open(url)
        .expect("redis url")
        .get_multiplexed_async_connection()
        .await
        .expect("redis");
    let mut first = Node::start().await;
    let id = node_id(&first).await;
    leads(&mut redis, &id, Duration::from_secs(125)).await;

    first.terminate();
    for lock in LOCKS {
        assert_eq!(holder(&mut redis, lock).await, None, "{lock}");
    }

    let second = Node::start().await;
    let id = node_id(&second).await;
    leads(&mut redis, &id, Duration::from_secs(5)).await;
}
