//! A client that left a room hears nothing more of it, and a join it gave up does not answer
//! the next one.

use base64::Engine;
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use net_client::net_common::{ActorId, Channel, RoomMode};
use net_client::protocol::{ClientMsg, DataEndpoint, ServerMsg};
use net_client::{Client, ClientError, Event, Options};
use net_transport::{quic, Link};
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

fn text(message: &ServerMsg) -> Message {
    Message::Text(serde_json::to_string(message).unwrap().into())
}

fn authed() -> Message {
    text(&ServerMsg::Authed {
        player_id: "ann".into(),
        name: "Ann".into(),
    })
}

fn joined(name: &str, data: DataEndpoint) -> Message {
    text(&ServerMsg::Joined {
        room: name.into(),
        name: name.into(),
        mode: RoomMode::Relay,
        actor: ActorId(1),
        master: ActorId(1),
        max_players: 4,
        props: Default::default(),
        players: Vec::new(),
        data,
        cached: Vec::new(),
        schema_hash: None,
        tick_hz: None,
        tick: None,
        seed: None,
        next_spawn_tag: None,
    })
}

fn left() -> Message {
    text(&ServerMsg::Left {
        reason: "left".into(),
        room: None,
    })
}

fn join(name: &str) -> ClientMsg {
    ClientMsg::Join {
        name: name.into(),
        player_props: Default::default(),
        rejoin_actor: None,
        schema_hash: None,
        spectator: false,
    }
}

/// The first frame of a data path, which tells the client the path is ready.
fn first_frames() -> Vec<Bytes> {
    let now = Instant::now();
    let mut link = Link::new(now, Duration::from_secs(15));
    link.send(Channel::ReliableOrdered, Bytes::new(), now)
        .unwrap();
    std::iter::from_fn(|| link.next_outgoing()).collect()
}

/// A node with a QUIC data path that never falls silent: after the first frame it sends
/// datagrams that carry nothing, one after another.
async fn busy_node() -> String {
    let identity = quic::server_identity(None, None, &["127.0.0.1".into()]).unwrap();
    let transport = quic::transport_config(Duration::from_secs(15), Duration::from_secs(5));
    let endpoint =
        quic::server_endpoint("127.0.0.1:0".parse().unwrap(), &identity, transport).unwrap();
    let data = DataEndpoint {
        host: "127.0.0.1".into(),
        port: endpoint.local_addr().unwrap().port(),
        cert_sha256: identity.fingerprint_sha256,
        ticket: base64::engine::general_purpose::STANDARD.encode(b"ticket"),
        ws_tunnel: false,
        webtransport_url: None,
    };
    tokio::spawn(async move {
        while let Some(incoming) = endpoint.accept().await {
            tokio::spawn(async move {
                let Ok(connection) = incoming.await else {
                    return;
                };
                if connection.read_datagram().await.is_err() {
                    return;
                }
                for frame in first_frames() {
                    let _ = connection.send_datagram(frame);
                }
                while connection.send_datagram(Bytes::from_static(&[0])).is_ok() {
                    tokio::task::yield_now().await;
                }
            });
        }
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        while let Some(Ok(Message::Text(request))) = ws.next().await {
            let answer = match serde_json::from_str(&request).unwrap() {
                ClientMsg::Auth { .. } => authed(),
                ClientMsg::Join { name, .. } => joined(&name, data.clone()),
                ClientMsg::Leave => left(),
                _ => continue,
            };
            ws.send(answer).await.unwrap();
        }
    });
    url
}

#[tokio::test(flavor = "multi_thread")]
async fn a_data_path_the_client_closed_says_nothing_of_its_end() {
    let mut client = Client::connect(&busy_node().await, "grant", Options::default())
        .await
        .unwrap();
    for round in 0..50 {
        client.join(join("room")).await.unwrap();
        client.leave().await.unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        while let Some(event) = client.try_next_event() {
            assert!(
                !matches!(event, Event::Closed(_)),
                "round {round}: {event:?}"
            );
        }
    }
}

/// A node over the WebSocket tunnel that answers the first join later than the client waits.
async fn late_node(wait: Duration) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let data = DataEndpoint {
        host: "127.0.0.1".into(),
        port: 0,
        cert_sha256: String::new(),
        ticket: base64::engine::general_purpose::STANDARD.encode(b"ticket"),
        ws_tunnel: true,
        webtransport_url: None,
    };
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        let mut late = Some(wait * 2);
        while let Some(Ok(message)) = ws.next().await {
            match message {
                Message::Text(request) => {
                    let answer = match serde_json::from_str(&request).unwrap() {
                        ClientMsg::Auth { .. } => authed(),
                        ClientMsg::Join { name, .. } => {
                            if let Some(late) = late.take() {
                                tokio::time::sleep(late).await;
                            }
                            joined(&name, data.clone())
                        }
                        ClientMsg::Leave => left(),
                        _ => continue,
                    };
                    ws.send(answer).await.unwrap();
                }
                Message::Binary(_) => {
                    for frame in first_frames() {
                        ws.send(Message::Binary(frame)).await.unwrap();
                    }
                }
                _ => {}
            }
        }
    });
    url
}

#[tokio::test]
async fn the_answer_to_a_join_the_client_gave_up_is_not_the_answer_to_the_next() {
    let wait = Duration::from_millis(200);
    let options = Options {
        tunnel: true,
        connect_timeout: wait,
        ..Options::default()
    };
    let mut client = Client::connect(&late_node(wait).await, "grant", options)
        .await
        .unwrap();
    let first = client.join(join("first")).await;
    assert!(matches!(first, Err(ClientError::Timeout)), "{first:?}");
    tokio::time::sleep(wait * 2).await;
    let second = client.join(join("second")).await.unwrap();
    assert_eq!(second.name, "second");
}
