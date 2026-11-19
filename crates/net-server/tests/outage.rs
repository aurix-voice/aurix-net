//! One node losing its database or its Redis while the rest of the suite keeps the shared ones.

mod common;

use common::{joined, refused, Node, Sink};
use serde_json::json;
use std::time::Duration;
use tokio::io::copy_bidirectional;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

/// A TCP relay in front of Postgres or Redis that the test can cut.
struct Relay {
    url: String,
    up: watch::Sender<bool>,
}

impl Relay {
    async fn start() -> Relay {
        let url = std::env::var("NET__DATABASE__URL")
            .unwrap_or_else(|_| "postgres://aurix:aurix@127.0.0.1:5432/aurix_net".into());
        Self::before(&url).await
    }

    async fn redis() -> Relay {
        let url =
            std::env::var("NET__REDIS__URL").unwrap_or_else(|_| "redis://127.0.0.1:6379/".into());
        Self::before(&url).await
    }

    async fn before(url: &str) -> Relay {
        let mut url = reqwest::Url::parse(url).expect("url");
        let upstream = format!(
            "{}:{}",
            url.host_str().expect("host"),
            url.port_or_known_default().expect("port")
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("relay port");
        let port = listener.local_addr().expect("relay address").port();
        url.set_host(Some("127.0.0.1")).expect("relay host");
        url.set_port(Some(port)).expect("relay port");
        let (up, state) = watch::channel(true);
        tokio::spawn(async move {
            while let Ok((mut client, _)) = listener.accept().await {
                if !*state.borrow() {
                    continue;
                }
                let upstream = upstream.clone();
                let mut state = state.clone();
                tokio::spawn(async move {
                    let Ok(mut server) = TcpStream::connect(upstream).await else {
                        return;
                    };
                    tokio::select! {
                        _ = copy_bidirectional(&mut client, &mut server) => {}
                        _ = state.wait_for(|up| !up) => {}
                    }
                });
            }
        });
        Relay {
            url: url.into(),
            up,
        }
    }

    /// Cuts the open connections and refuses new ones until `set(true)`.
    fn set(&self, up: bool) {
        self.up.send_replace(up);
    }
}

// The relay has to keep running while the node deletes its project on drop.
#[tokio::test(flavor = "multi_thread")]
async fn webhooks_resume_as_soon_as_the_database_is_back() {
    let relay = Relay::start().await;
    let node = Node::start_with(&[("NET__DATABASE__URL", &relay.url)]).await;
    let project = node.project("outage").await;
    let mut alice = project.player("alice").await;
    joined(&alice.call(json!({"t": "create", "name": "arena"})).await);
    let mut bob = project.player("bob").await;
    joined(&bob.call(json!({"t": "join", "name": "arena"})).await);
    let carol = project.grant("carol").await;
    let mut sink = Sink::start().await;
    let hook = json!({"url": sink.url, "events": ["player.joined", "player.left"]});
    let (status, reply) = project.api.post("/v1/webhooks", hook).await;
    assert_eq!(status, 200, "{reply}");

    // Bob leaves while the database is away: the node cannot read who to tell.
    relay.set(false);
    bob.send(json!({"t": "leave"})).await;
    tokio::time::sleep(Duration::from_secs(7)).await;
    relay.set(true);

    let mut carol = common::Ws::open(&project.api.http, &carol).await;
    let actor = joined(&carol.call(json!({"t": "join", "name": "arena"})).await);
    assert_eq!(sink.next("player.joined").await["data"]["actor"], actor);
}

// The relay has to keep running while the node deletes its project on drop.
#[tokio::test(flavor = "multi_thread")]
async fn webhooks_go_out_through_an_outage_longer_than_the_cache() {
    let relay = Relay::start().await;
    let node = Node::start_with(&[("NET__DATABASE__URL", &relay.url)]).await;
    let project = node.project("outage-long").await;
    let mut sink = Sink::start().await;
    let hook = json!({"url": sink.url, "events": ["player.joined", "player.left"]});
    let (status, reply) = project.api.post("/v1/webhooks", hook).await;
    assert_eq!(status, 200, "{reply}");
    let mut alice = project.player("alice").await;
    joined(&alice.call(json!({"t": "create", "name": "arena"})).await);
    let mut bob = project.player("bob").await;
    let actor = joined(&bob.call(json!({"t": "join", "name": "arena"})).await);
    while sink.next("player.joined").await["data"]["actor"] != actor {}

    // The node keeps each project's webhooks for 30 seconds.
    relay.set(false);
    tokio::time::sleep(Duration::from_secs(32)).await;
    bob.send(json!({"t": "leave"})).await;
    assert_eq!(sink.next("player.left").await["data"]["actor"], actor);
    relay.set(true);
}

// The relay has to keep running while the node deletes its project on drop.
#[tokio::test(flavor = "multi_thread")]
async fn room_closed_right_after_redis_came_back_leaves_the_lobby_list() {
    let relay = Relay::redis().await;
    let node = Node::start_with(&[("NET__REDIS__URL", &relay.url)]).await;
    let project = node.project("outage-lobby").await;
    let mut ann = project.player("ann").await;
    ann.send(json!({"t": "lobby", "subscribe": true})).await;
    ann.until("lobby_rooms").await;
    let mut bob = project.player("bob").await;
    joined(&bob.call(json!({"t": "create", "name": "arena"})).await);
    assert_eq!(ann.until("lobby_update").await["room"]["name"], "arena");

    relay.set(false);
    tokio::time::sleep(Duration::from_millis(100)).await;
    relay.set(true);
    // The node's first command after the cut finds the connection gone and opens a new one.
    tokio::time::sleep(Duration::from_millis(300)).await;
    bob.send(json!({"t": "set_room_props", "props": {"map": "dust"}}))
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    bob.send(json!({"t": "leave"})).await;

    // The node was not listening when the room closed, so it sends the whole list again.
    let lists: Vec<_> = ann
        .drain(Duration::from_secs(5))
        .await
        .into_iter()
        .filter(|msg| msg["t"] == "lobby_rooms")
        .collect();
    let last = lists.last().expect("no room list after the break");
    assert_eq!(
        last["rooms"],
        json!([]),
        "the closed room stayed in the list"
    );
}

/// The ids of the rooms a lobby subscriber holds after `messages`, starting from `listed`.
fn listed_after(mut listed: Vec<String>, messages: &[serde_json::Value]) -> Vec<String> {
    for msg in messages {
        let id = |room: &serde_json::Value| room["id"].as_str().expect("room id").to_string();
        if msg["t"] == "lobby_rooms" {
            listed = msg["rooms"]
                .as_array()
                .expect("rooms")
                .iter()
                .map(id)
                .collect();
        } else if msg["t"] == "lobby_update" {
            listed.retain(|known| *known != id(&msg["room"]));
            if msg["removed"] != true {
                listed.push(id(&msg["room"]));
            }
        }
    }
    listed
}

// The relay has to keep running while the node deletes its project on drop.
#[tokio::test(flavor = "multi_thread")]
async fn room_closed_on_another_node_during_a_redis_outage_leaves_the_lobby_list() {
    let relay = Relay::redis().await;
    let host = Node::start().await;
    let cut_off = Node::start_with(&[("NET__REDIS__URL", &relay.url)]).await;
    let project = host.project("outage-fleet").await;
    let mut ann = cut_off.same_project(&project).player("ann").await;
    ann.send(json!({"t": "lobby", "subscribe": true})).await;
    ann.until("lobby_rooms").await;
    let mut bob = project.player("bob").await;
    joined(&bob.call(json!({"t": "create", "name": "arena"})).await);
    let room = ann.until("lobby_update").await["room"]["id"].clone();
    let room = room.as_str().expect("room id").to_string();

    relay.set(false);
    tokio::time::sleep(Duration::from_secs(1)).await;
    bob.send(json!({"t": "leave"})).await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    relay.set(true);

    let heard = ann.drain(Duration::from_secs(5)).await;
    assert_eq!(listed_after(vec![room], &heard), Vec::<String>::new());
}

// The relay has to keep running while the node deletes its project on drop.
#[tokio::test(flavor = "multi_thread")]
async fn room_closed_during_a_redis_outage_leaves_the_lobby_list() {
    let relay = Relay::redis().await;
    let node = Node::start_with(&[("NET__REDIS__URL", &relay.url)]).await;
    let project = node.project("outage-long").await;
    let mut ann = project.player("ann").await;
    ann.send(json!({"t": "lobby", "subscribe": true})).await;
    ann.until("lobby_rooms").await;
    let mut bob = project.player("bob").await;
    joined(&bob.call(json!({"t": "create", "name": "arena"})).await);
    let room = ann.until("lobby_update").await["room"]["id"].clone();
    let room = room.as_str().expect("room id").to_string();

    relay.set(false);
    tokio::time::sleep(Duration::from_secs(1)).await;
    bob.send(json!({"t": "leave"})).await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    relay.set(true);

    let heard = ann.drain(Duration::from_secs(5)).await;
    assert_eq!(listed_after(vec![room], &heard), Vec::<String>::new());
}

// The relay has to keep running while the node deletes its project on drop.
#[tokio::test(flavor = "multi_thread")]
async fn room_created_during_a_redis_outage_is_listed_once_redis_is_back() {
    let relay = Relay::redis().await;
    let node = Node::start_with(&[("NET__REDIS__URL", &relay.url)]).await;
    let project = node.project("outage-create").await;
    let mut ann = project.player("ann").await;

    relay.set(false);
    tokio::time::sleep(Duration::from_secs(1)).await;
    joined(&ann.call(json!({"t": "create", "name": "arena"})).await);
    tokio::time::sleep(Duration::from_secs(8)).await;
    relay.set(true);
    tokio::time::sleep(Duration::from_secs(4)).await;

    let (status, list) = project.api.get("/v1/rooms").await;
    assert_eq!(status, 200, "{list}");
    assert_eq!(list["rooms"][0]["name"], "arena", "{list}");
}

// The relay has to keep running while the node deletes its project on drop.
#[tokio::test(flavor = "multi_thread")]
async fn room_list_request_fails_while_redis_is_away() {
    let relay = Relay::redis().await;
    let node = Node::start_with(&[("NET__REDIS__URL", &relay.url)]).await;
    let project = node.project("outage-list").await;
    let mut ann = project.player("ann").await;
    joined(&ann.call(json!({"t": "create", "name": "arena"})).await);

    relay.set(false);
    tokio::time::sleep(Duration::from_secs(1)).await;
    let (status, reply) = project.api.get("/v1/rooms").await;
    relay.set(true);

    // An empty list would tell the caller that the project has no rooms.
    assert_eq!(status, 500, "{reply}");
    assert_eq!(reply["error"], "redis", "{reply}");
}

// The relay has to keep running while the node deletes its project on drop.
#[tokio::test(flavor = "multi_thread")]
async fn project_is_not_deleted_while_its_rooms_cannot_be_read() {
    let relay = Relay::redis().await;
    let node = Node::start_with(&[("NET__REDIS__URL", &relay.url)]).await;
    let project = node.project("outage-delete").await;
    let mut ann = project.player("ann").await;
    joined(&ann.call(json!({"t": "create", "name": "arena"})).await);
    let path = format!("/v1/operator/projects/{}", project.id);

    relay.set(false);
    tokio::time::sleep(Duration::from_secs(1)).await;
    let (status, reply) = project.operator.delete(&path).await;
    relay.set(true);
    // Deleted now, the project would keep the rooms nobody could list.
    assert_eq!(status, 500, "{reply}");
    assert_eq!(reply["error"], "redis", "{reply}");

    tokio::time::sleep(Duration::from_secs(3)).await;
    let (status, reply) = project.operator.delete(&path).await;
    assert_eq!(status, 200, "{reply}");
    assert_eq!(reply["closed_rooms"], 1, "{reply}");
    assert_eq!(ann.until("left").await["reason"], "room closed");
}

// The relay has to keep running while the node deletes its project on drop.
#[tokio::test(flavor = "multi_thread")]
async fn operator_reads_fail_while_redis_is_away() {
    let relay = Relay::redis().await;
    let node = Node::start_with(&[("NET__REDIS__URL", &relay.url)]).await;
    let project = node.project("outage-operator").await;
    let mut ann = project.player("ann").await;
    joined(&ann.call(json!({"t": "create", "name": "arena"})).await);

    relay.set(false);
    tokio::time::sleep(Duration::from_secs(1)).await;
    let mut replies = Vec::new();
    for path in [
        format!("/v1/operator/projects/{}/rooms", project.id),
        format!("/v1/operator/projects/{}", project.id),
        "/v1/operator/rooms".to_string(),
        "/v1/operator/attention".to_string(),
    ] {
        replies.push((project.operator.get(&path).await, path));
    }
    relay.set(true);

    // Each of them reads Redis; answered without it, they would show no rooms and no rejections.
    for ((status, reply), path) in replies {
        assert_eq!(status, 500, "{path}: {reply}");
        assert_eq!(reply["error"], "redis", "{path}: {reply}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn room_on_another_node_is_not_reported_closed_while_redis_is_away() {
    let relay = Relay::redis().await;
    let host = Node::start().await;
    let cut_off = Node::start_with(&[("NET__REDIS__URL", &relay.url)]).await;
    let project = host.project("outage-close").await;
    let mut ann = project.player("ann").await;
    joined(&ann.call(json!({"t": "create", "name": "arena"})).await);
    let (_, list) = project.api.get("/v1/rooms").await;
    let path = format!(
        "/v1/rooms/{}",
        list["rooms"][0]["room"].as_str().expect("room")
    );
    let asked = cut_off.same_project(&project);

    relay.set(false);
    tokio::time::sleep(Duration::from_secs(1)).await;
    let (status, reply) = asked.api.delete(&path).await;
    relay.set(true);
    // The node asked cannot learn which node hosts the room, so it cannot have closed it.
    assert_eq!(status, 500, "{reply}");
    assert_eq!(reply["error"], "redis", "{reply}");
    assert!(ann.drain(Duration::from_secs(1)).await.is_empty());

    tokio::time::sleep(Duration::from_secs(3)).await;
    let (status, reply) = asked.api.delete(&path).await;
    assert_eq!(status, 204, "{reply}");
    assert_eq!(ann.until("left").await["reason"], "room closed");
}

#[tokio::test(flavor = "multi_thread")]
async fn room_is_not_continued_while_its_checkpoint_cannot_be_read() {
    let relay = Relay::redis().await;
    let node_a = Node::start_with(&[("NET__SERVER__HEARTBEAT_SECS", "1")]).await;
    let node_b = Node::start_with(&[
        ("NET__SERVER__HEARTBEAT_SECS", "1"),
        ("NET__REDIS__URL", &relay.url),
    ])
    .await;
    let pa = node_a.project("outage-continue").await;
    let pb = node_b.same_project(&pa);
    let mut ann = pa.player("ann").await;
    ann.send(json!({"t": "create", "name": "arena", "mode": "lockstep"}))
        .await;
    let seed = ann.until("joined").await["seed"].clone();
    // A lockstep room is checkpointed every five seconds.
    tokio::time::sleep(Duration::from_secs(6)).await;

    // A is silent, so B continues the room for the player who asks; its seed is in the checkpoint.
    let frozen = node_a.freeze(Duration::from_secs(12));
    let moved = async {
        tokio::time::sleep(Duration::from_secs(4)).await;
        let mut bob = pb.player("bob").await;
        relay.set(false);
        tokio::time::sleep(Duration::from_millis(300)).await;
        let refusal = bob.call(json!({"t": "join", "name": "arena"})).await;
        relay.set(true);
        tokio::time::sleep(Duration::from_secs(3)).await;
        bob.send(json!({"t": "join", "name": "arena"})).await;
        (refusal, bob.until("joined").await["seed"].clone())
    };
    let (_, (refusal, after)) = tokio::join!(frozen, moved);
    refused(&refusal, "redis");
    assert_eq!(after, seed);
}

// The relays have to keep running while the node deletes its project on drop.
#[tokio::test(flavor = "multi_thread")]
async fn failure_inside_the_node_goes_to_its_log_and_not_to_the_caller() {
    let database = Relay::start().await;
    let redis = Relay::redis().await;
    let node = Node::start_with(&[
        ("NET__DATABASE__URL", &database.url),
        ("NET__REDIS__URL", &redis.url),
    ])
    .await;
    let project = node.project("outage-text").await;
    let mut ann = project.player("ann").await;

    database.set(false);
    tokio::time::sleep(Duration::from_secs(1)).await;
    let refusal = ann.call(json!({"t": "create", "name": "arena"})).await;
    database.set(true);
    tokio::time::sleep(Duration::from_secs(3)).await;
    redis.set(false);
    tokio::time::sleep(Duration::from_secs(1)).await;
    let (status, reply) = project.api.get("/v1/rooms").await;
    redis.set(true);

    assert_eq!(
        refusal,
        json!({"t": "error", "code": "database",
            "message": "database: the node could not complete a database request"})
    );
    assert_eq!(status, 500);
    assert_eq!(
        reply,
        json!({"error": "redis", "message": "redis: the node could not complete a Redis request"})
    );

    // What failed is the operator's to read, with the message or the route and the project.
    let logged = node.logged();
    let cause = |line: &serde_json::Value, code: &str| {
        line["level"] == "ERROR" && line["error"].as_str().is_some_and(|e| e.starts_with(code))
    };
    assert!(
        logged.iter().any(|line| cause(line, "database: ")
            && line["request"] == "create"
            && line["project"] == project.id
            && line["player"] == "ann"),
        "{}",
        node.log_tail()
    );
    assert!(
        logged.iter().any(|line| cause(line, "redis: ")
            && line["span"]["path"] == "/v1/rooms"
            && line["span"]["project"] == project.id),
        "{}",
        node.log_tail()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn room_is_not_reported_closed_while_its_node_cannot_be_asked() {
    let host = Node::start_with(&[("NET__SERVER__HEARTBEAT_SECS", "2")]).await;
    let asked = Node::start().await;
    let project = host.project("outage-silent").await;
    let mut ann = project.player("ann").await;
    joined(&ann.call(json!({"t": "create", "name": "arena"})).await);
    let (_, list) = project.api.get("/v1/rooms").await;
    let room = format!(
        "/v1/rooms/{}",
        list["rooms"][0]["room"].as_str().expect("room")
    );
    let there = asked.same_project(&project);
    let deleted = format!("/v1/operator/projects/{}", project.id);

    // Both requests wait for the silent node; the project's key is still good for the first.
    let silent = host.freeze(Duration::from_secs(7));
    let requests = async {
        tokio::time::sleep(Duration::from_millis(500)).await;
        tokio::join!(there.api.delete(&room), async {
            tokio::time::sleep(Duration::from_millis(500)).await;
            there.operator.delete(&deleted).await
        })
    };
    let (_, ((status, reply), (project_status, project_reply))) = tokio::join!(silent, requests);

    assert_eq!(status, 500, "{reply}");
    assert_eq!(reply["error"], "internal", "{reply}");
    assert_eq!(project_status, 200, "{project_reply}");
    assert_eq!(project_reply["closed_rooms"], 0, "{project_reply}");
    // Its node closes the room of the deleted project once it is back.
    assert_eq!(ann.until("left").await["reason"], "room closed");
}

// The relay has to keep running while the node deletes its project on drop.
#[tokio::test(flavor = "multi_thread")]
async fn rooms_changed_during_a_redis_outage_reach_subscribers_on_another_node() {
    let relay = Relay::redis().await;
    let cut_off = Node::start_with(&[("NET__REDIS__URL", &relay.url)]).await;
    let other = Node::start().await;
    let project = cut_off.project("outage-told").await;
    let mut ann = other.same_project(&project).player("ann").await;
    ann.send(json!({"t": "lobby", "subscribe": true})).await;
    ann.until("lobby_rooms").await;
    let mut bob = project.player("bob").await;
    joined(&bob.call(json!({"t": "create", "name": "old"})).await);
    let old = ann.until("lobby_update").await["room"]["id"].clone();
    let old = old.as_str().expect("room id").to_string();

    relay.set(false);
    tokio::time::sleep(Duration::from_secs(1)).await;
    bob.send(json!({"t": "leave"})).await;
    let mut cat = project.player("cat").await;
    let new = cat.call(json!({"t": "create", "name": "new"})).await;
    joined(&new);
    tokio::time::sleep(Duration::from_secs(8)).await;
    relay.set(true);

    // Their node never lost Redis and sends them no list: what the node that did publishes
    // about each of its rooms once it is back is all they hear.
    let heard = ann.drain(Duration::from_secs(5)).await;
    assert_eq!(listed_after(vec![old], &heard), vec![new["room"].clone()]);
}
