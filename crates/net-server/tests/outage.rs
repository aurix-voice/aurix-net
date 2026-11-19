//! One node losing its database while the rest of the suite keeps the shared Postgres.

mod common;

use common::{joined, Node, Sink};
use serde_json::json;
use std::time::Duration;
use tokio::io::copy_bidirectional;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

/// A TCP relay in front of Postgres that the test can cut.
struct Relay {
    url: String,
    up: watch::Sender<bool>,
}

impl Relay {
    async fn start() -> Relay {
        let mut url = reqwest::Url::parse(
            &std::env::var("NET__DATABASE__URL")
                .unwrap_or_else(|_| "postgres://aurix:aurix@127.0.0.1:5432/aurix_net".into()),
        )
        .expect("database url");
        let upstream = format!(
            "{}:{}",
            url.host_str().expect("database host"),
            url.port().unwrap_or(5432)
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

#[tokio::test]
async fn webhooks_resume_as_soon_as_the_database_is_back() {
    let relay = Relay::start().await;
    let node = Node::start_with(&[("NET__DATABASE__URL", &relay.url)]).await;
    let p = node.project("outage").await;
    let mut alice = p.player("alice").await;
    joined(&alice.call(json!({"t": "create", "name": "arena"})).await);
    let mut bob = p.player("bob").await;
    joined(&bob.call(json!({"t": "join", "name": "arena"})).await);
    let carol = p.grant("carol").await;
    let mut sink = Sink::start().await;
    let hook = json!({"url": sink.url, "events": ["player.joined", "player.left"]});
    let (status, reply) = p.api.post("/v1/webhooks", hook).await;
    assert_eq!(status, 200, "{reply}");

    // Bob leaves while the database is away: the node cannot read who to tell.
    relay.set(false);
    bob.send(json!({"t": "leave"})).await;
    tokio::time::sleep(Duration::from_secs(7)).await;
    relay.set(true);

    let mut carol = common::Ws::open(&p.api.http, &carol).await;
    let actor = joined(&carol.call(json!({"t": "join", "name": "arena"})).await);
    assert_eq!(sink.next("player.joined").await["data"]["actor"], actor);
}

#[tokio::test]
async fn webhooks_go_out_through_an_outage_longer_than_the_cache() {
    let relay = Relay::start().await;
    let node = Node::start_with(&[("NET__DATABASE__URL", &relay.url)]).await;
    let p = node.project("outage-long").await;
    let mut sink = Sink::start().await;
    let hook = json!({"url": sink.url, "events": ["player.joined", "player.left"]});
    let (status, reply) = p.api.post("/v1/webhooks", hook).await;
    assert_eq!(status, 200, "{reply}");
    let mut alice = p.player("alice").await;
    joined(&alice.call(json!({"t": "create", "name": "arena"})).await);
    let mut bob = p.player("bob").await;
    let actor = joined(&bob.call(json!({"t": "join", "name": "arena"})).await);
    while sink.next("player.joined").await["data"]["actor"] != actor {}

    // The node keeps each project's webhooks for 30 seconds.
    relay.set(false);
    tokio::time::sleep(Duration::from_secs(32)).await;
    bob.send(json!({"t": "leave"})).await;
    assert_eq!(sink.next("player.left").await["data"]["actor"], actor);
}
