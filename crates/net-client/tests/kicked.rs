//! A join the node ends with a kick fails with the reason of the kick, and the game hears of no
//! room it was never in.

use base64::Engine;
use futures::{SinkExt, StreamExt};
use net_client::net_common::{ActorId, RoomMode};
use net_client::protocol::{ClientMsg, DataEndpoint, ServerMsg, JOIN_ID};
use net_client::{Client, ClientError, Event, Options};
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

fn text(message: &ServerMsg) -> Message {
    Message::Text(serde_json::to_string(message).unwrap().into())
}

/// A node whose master kicks a player as soon as it sees it, and the data path of the room never
/// opens. The `left` of the kick follows the `joined` at once, or, when it is `late`, reaches
/// the client only after the join has failed: ahead of the answer to its leave. With the node
/// comes how many times the client asked to leave.
async fn node_with_a_blacklist(late: bool) -> (String, Arc<Mutex<u32>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let leaves = Arc::new(Mutex::new(0));
    let asked = leaves.clone();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        let hello = ServerMsg::Hello {
            session: "session".into(),
            version: "0".into(),
            region: "local".into(),
            supports: vec![JOIN_ID.into()],
        };
        ws.send(text(&hello)).await.unwrap();
        let kicked = ServerMsg::Left {
            reason: "cheater".into(),
            room: Some("id".into()),
            id: None,
        };
        while let Some(Ok(message)) = ws.next().await {
            let Message::Text(request) = message else {
                continue;
            };
            match serde_json::from_str(&request).unwrap() {
                ClientMsg::Auth { .. } => {
                    let authed = ServerMsg::Authed {
                        player_id: "ann".into(),
                        name: "Ann".into(),
                    };
                    ws.send(text(&authed)).await.unwrap();
                }
                ClientMsg::Join { id, .. } => {
                    let ticket = base64::engine::general_purpose::STANDARD.encode(b"ticket");
                    let joined = ServerMsg::Joined {
                        room: "id".into(),
                        name: "room".into(),
                        mode: RoomMode::Relay,
                        actor: ActorId(2),
                        master: ActorId(1),
                        max_players: 4,
                        props: Default::default(),
                        players: Vec::new(),
                        data: DataEndpoint {
                            host: "127.0.0.1".into(),
                            port: 0,
                            cert_sha256: String::new(),
                            ticket,
                            ws_tunnel: true,
                            webtransport_url: None,
                        },
                        cached: Vec::new(),
                        schema_hash: None,
                        tick_hz: None,
                        tick: None,
                        seed: None,
                        next_spawn_tag: None,
                        id,
                    };
                    ws.send(text(&joined)).await.unwrap();
                    if !late {
                        ws.send(text(&kicked)).await.unwrap();
                    }
                }
                ClientMsg::Leave { id } => {
                    *asked.lock() += 1;
                    if late {
                        ws.send(text(&kicked)).await.unwrap();
                    }
                    let left = ServerMsg::Left {
                        reason: "left".into(),
                        room: None,
                        id,
                    };
                    ws.send(text(&left)).await.unwrap();
                }
                _ => {}
            }
        }
    });
    (url, leaves)
}

async fn a_join_the_node_ends_with_a_kick(late: bool) {
    let (node, leaves) = node_with_a_blacklist(late).await;
    let options = Options {
        tunnel: true,
        connect_timeout: Duration::from_millis(if late { 300 } else { 10_000 }),
        ..Options::default()
    };
    let mut client = Client::connect(&node, "grant", options).await.unwrap();
    let join = ClientMsg::Join {
        name: "room".into(),
        player_props: Default::default(),
        rejoin_actor: None,
        schema_hash: None,
        spectator: false,
        id: None,
    };
    let began = Instant::now();
    let joined = client.join(join).await;
    assert!(
        matches!(&joined, Err(ClientError::Left(reason)) if reason == "cheater"),
        "{joined:?}"
    );
    assert!(began.elapsed() < Duration::from_secs(1));
    assert!(client.joined.is_none());

    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut lefts = Vec::new();
    while let Some(event) = client.try_next_event() {
        if let Event::Control(ServerMsg::Left { reason, room, .. }) = event {
            lefts.push((reason, room));
        }
    }
    assert_eq!(lefts, [], "a `left` of a room the game was never in");
    assert_eq!(*leaves.lock(), late as u32);
}

#[tokio::test]
async fn a_join_the_node_ends_with_a_kick_fails_at_once_with_the_reason() {
    a_join_the_node_ends_with_a_kick(false).await;
}

#[tokio::test]
async fn a_kick_that_comes_after_the_data_path_failed_is_still_why_the_join_failed() {
    a_join_the_node_ends_with_a_kick(true).await;
}
