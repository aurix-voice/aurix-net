//! A connection that is gone refuses what the game sends on it.

use base64::Engine;
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use net_client::net_common::{ActorId, Channel, RoomMode};
use net_client::protocol::{ClientMsg, DataEndpoint, ServerMsg};
use net_client::{Client, ClientError, Event, EventTarget, Options};
use net_transport::Link;
use std::time::Instant;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

fn text(message: &ServerMsg) -> Message {
    Message::Text(serde_json::to_string(message).unwrap().into())
}

/// A node that lets a client into a room over the WebSocket tunnel and closes the connection.
async fn closing_node() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        while let Some(Ok(message)) = ws.next().await {
            match message {
                Message::Text(request) if request.contains("\"auth\"") => {
                    let authed = ServerMsg::Authed {
                        player_id: "ann".into(),
                        name: "Ann".into(),
                    };
                    ws.send(text(&authed)).await.unwrap();
                }
                Message::Text(_) => {
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
                        cached: Vec::new(),
                        schema_hash: None,
                        tick_hz: None,
                        tick: None,
                        seed: None,
                        next_spawn_tag: None,
                    };
                    ws.send(text(&joined)).await.unwrap();
                }
                Message::Binary(_) => {
                    let now = Instant::now();
                    let mut link = Link::new(now);
                    link.send(Channel::ReliableOrdered, Bytes::new(), now)
                        .unwrap();
                    while let Some(datagram) = link.next_outgoing() {
                        ws.send(Message::Binary(datagram)).await.unwrap();
                    }
                    break;
                }
                _ => {}
            }
        }
    });
    url
}

#[tokio::test]
async fn a_lost_connection_refuses_what_is_sent_on_it() {
    let node = closing_node().await;
    let options = Options {
        tunnel: true,
        ..Options::default()
    };
    let mut client = Client::connect(&node, "grant", options).await.unwrap();
    let join = ClientMsg::Join {
        name: "room".into(),
        player_props: Default::default(),
        rejoin_actor: None,
        schema_hash: None,
        spectator: false,
    };
    client.join(join).await.unwrap();
    while let Some(event) = client.next_event().await {
        if matches!(event, Event::Closed(_)) {
            break;
        }
    }
    let sent = client.send_event(1, EventTarget::Others, Bytes::new(), true);
    assert!(matches!(sent, Err(ClientError::Disconnected)));
}
