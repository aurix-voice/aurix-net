//! What the node says while a call waits for its own answer still reaches the game.

use base64::Engine;
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use net_client::net_common::{ActorId, Channel, RoomMode};
use net_client::protocol::{ClientMsg, DataEndpoint, ServerMsg};
use net_client::{Client, Event, Options};
use net_transport::Link;
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

fn text(message: &ServerMsg) -> Message {
    Message::Text(serde_json::to_string(message).unwrap().into())
}

fn notice(text: &str) -> ServerMsg {
    ServerMsg::Notice {
        text: text.into(),
        level: "info".into(),
    }
}

/// A node that answers a join with `left` for the room before it and a notice, then `joined`,
/// and says one thing more before the first frame of the data path.
async fn node() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        let mut ticket_seen = false;
        while let Some(Ok(message)) = ws.next().await {
            match message {
                Message::Text(request) if request.contains("\"auth\"") => {
                    ws.send(text(&notice("before authed"))).await.unwrap();
                    let authed = ServerMsg::Authed {
                        player_id: "ann".into(),
                        name: "Ann".into(),
                    };
                    ws.send(text(&authed)).await.unwrap();
                }
                Message::Text(request) if request.contains("\"join\"") => {
                    let left = ServerMsg::Left {
                        reason: "left".into(),
                        room: Some("before".into()),
                    };
                    ws.send(text(&left)).await.unwrap();
                    ws.send(text(&notice("before joined"))).await.unwrap();
                    let ticket = base64::engine::general_purpose::STANDARD.encode(b"ticket");
                    let joined = ServerMsg::Joined {
                        room: "id".into(),
                        name: "room".into(),
                        mode: RoomMode::Relay,
                        actor: ActorId(1),
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
                        cached: vec![(7, ActorId(2), String::new())],
                        schema_hash: None,
                        tick_hz: None,
                        tick: None,
                        seed: None,
                        next_spawn_tag: None,
                    };
                    ws.send(text(&joined)).await.unwrap();
                }
                Message::Binary(_) if !ticket_seen => {
                    ticket_seen = true;
                    ws.send(text(&notice("before the data path")))
                        .await
                        .unwrap();
                    let now = Instant::now();
                    let mut link = Link::new(now, Duration::from_secs(15));
                    link.send(Channel::ReliableOrdered, Bytes::new(), now)
                        .unwrap();
                    while let Some(datagram) = link.next_outgoing() {
                        ws.send(Message::Binary(datagram)).await.unwrap();
                    }
                }
                _ => {}
            }
        }
    });
    url
}

#[tokio::test]
async fn what_the_node_says_during_a_join_reaches_the_game_in_order() {
    let options = Options {
        tunnel: true,
        ..Options::default()
    };
    let mut client = Client::connect(&node().await, "grant", options)
        .await
        .unwrap();
    let join = ClientMsg::Join {
        name: "room".into(),
        player_props: Default::default(),
        rejoin_actor: None,
        schema_hash: None,
        spectator: false,
    };
    client.join(join).await.unwrap();

    let mut heard = Vec::new();
    while heard.len() < 5 {
        let event = tokio::time::timeout(Duration::from_secs(2), client.next_event()).await;
        match event {
            Ok(Some(Event::Control(ServerMsg::Notice { text, .. }))) => heard.push(text),
            Ok(Some(Event::Control(ServerMsg::Left { reason, room }))) => {
                heard.push(format!("left: {reason} {}", room.unwrap_or_default()))
            }
            Ok(Some(Event::Data { .. })) => heard.push("cached event".into()),
            Ok(Some(_)) => {}
            _ => break,
        }
    }
    assert_eq!(
        heard,
        [
            "before authed",
            "left: left before",
            "before joined",
            "before the data path",
            "cached event",
        ]
    );
}
