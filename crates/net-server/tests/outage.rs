//! One node losing its database or its Redis while the rest of the suite keeps the shared ones.

mod common;

use common::{joined, Node, Sink};
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
