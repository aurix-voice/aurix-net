//! A client that left a room hears nothing more of it, and a join it gave up does not answer
//! the next one, nor does an answer the node lost fail it.

use base64::Engine;
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use net_client::net_common::{ActorId, Channel, RoomMode};
use net_client::protocol::{ClientMsg, DataEndpoint, ServerMsg, JOIN_ID};
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

fn hello() -> Message {
    text(&ServerMsg::Hello {
        session: "session".into(),
        version: "0".into(),
        region: "local".into(),
        supports: vec![JOIN_ID.into()],
    })
}

fn joined(name: &str, data: DataEndpoint, id: Option<u32>) -> Message {
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
        id,
    })
}

fn left(id: Option<u32>) -> Message {
    text(&ServerMsg::Left {
        reason: "left".into(),
        room: None,
        id,
    })
}

fn kicked(room: &str) -> Message {
    text(&ServerMsg::Left {
        reason: "kicked".into(),
        room: Some(room.into()),
        id: None,
    })
}

fn join(name: &str) -> ClientMsg {
    ClientMsg::Join {
        name: name.into(),
        player_props: Default::default(),
        rejoin_actor: None,
        schema_hash: None,
        spectator: false,
        id: None,
    }
}

/// The first frame of a data path, which tells the client the path is ready, and `said`
/// messages of the room after it.
fn first_frames(said: usize) -> Vec<Bytes> {
    let now = Instant::now();
    let mut link = Link::new(now, Duration::from_secs(15));
    link.send(Channel::ReliableOrdered, Bytes::new(), now)
        .unwrap();
    for _ in 0..said {
        link.send(Channel::ReliableOrdered, Bytes::from_static(b"word"), now)
            .unwrap();
    }
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
                for frame in first_frames(0) {
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
                ClientMsg::Join { name, id, .. } => joined(&name, data.clone(), id),
                ClientMsg::Leave { id } => left(id),
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

/// What a node over the WebSocket tunnel does other than answer every request at once.
#[derive(Clone, Copy, Default)]
struct Node {
    /// Lists [`JOIN_ID`] and repeats the `id` of a request in its answer.
    join_ids: bool,
    /// Answers the first join this much later.
    late: Option<Duration>,
    /// Loses its answer to the first leave.
    loses_left: bool,
    /// Messages the room sends as soon as the client is in.
    says: usize,
    /// Takes the client out of its room when asked for `stats`, and sends this many messages
    /// of the room after the `left`.
    says_after_kick: usize,
}

impl Node {
    async fn listen(mut self) -> String {
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
            if self.join_ids {
                ws.send(hello()).await.unwrap();
            }
            while let Some(Ok(message)) = ws.next().await {
                match message {
                    Message::Text(request) => {
                        let answer = match serde_json::from_str(&request).unwrap() {
                            ClientMsg::Auth { .. } => authed(),
                            ClientMsg::Join { name, id, .. } => {
                                if let Some(late) = self.late.take() {
                                    tokio::time::sleep(late).await;
                                }
                                joined(&name, data.clone(), id.filter(|_| self.join_ids))
                            }
                            ClientMsg::Leave { .. } if std::mem::take(&mut self.loses_left) => {
                                continue
                            }
                            ClientMsg::Leave { id } => left(id.filter(|_| self.join_ids)),
                            ClientMsg::Stats { .. } => {
                                ws.send(kicked("room")).await.unwrap();
                                for frame in first_frames(self.says_after_kick) {
                                    ws.send(Message::Binary(frame)).await.unwrap();
                                }
                                continue;
                            }
                            _ => continue,
                        };
                        ws.send(answer).await.unwrap();
                    }
                    Message::Binary(_) => {
                        for frame in first_frames(self.says) {
                            ws.send(Message::Binary(frame)).await.unwrap();
                        }
                    }
                    _ => {}
                }
            }
        });
        url
    }
}

const WAIT: Duration = Duration::from_millis(200);

async fn client_of(node: Node) -> Client {
    let options = Options {
        tunnel: true,
        connect_timeout: WAIT,
        ..Options::default()
    };
    Client::connect(&node.listen().await, "grant", options)
        .await
        .unwrap()
}

async fn a_join_after_one_the_client_gave_up(node: Node) {
    let mut client = client_of(Node {
        late: Some(WAIT * 2),
        ..node
    })
    .await;
    let first = client.join(join("first")).await;
    assert!(matches!(first, Err(ClientError::Timeout)), "{first:?}");
    tokio::time::sleep(WAIT * 2).await;
    let second = client.join(join("second")).await.unwrap();
    assert_eq!(second.name, "second");
}

#[tokio::test]
async fn the_answer_to_a_join_the_client_gave_up_is_not_the_answer_to_the_next() {
    a_join_after_one_the_client_gave_up(Node {
        join_ids: true,
        ..Node::default()
    })
    .await;
}

#[tokio::test]
async fn a_node_that_repeats_no_id_answers_in_the_order_it_was_asked() {
    a_join_after_one_the_client_gave_up(Node::default()).await;
}

#[tokio::test]
async fn an_answer_to_a_leave_the_node_lost_fails_no_join_after_it() {
    let mut client = client_of(Node {
        join_ids: true,
        loses_left: true,
        ..Node::default()
    })
    .await;
    client.join(join("first")).await.unwrap();
    client.leave().await.unwrap();
    for name in ["second", "third"] {
        assert_eq!(client.join(join(name)).await.unwrap().name, name);
        client.leave().await.unwrap();
    }
}

/// A node that gives the data path up under the first join, with the control channel open, and
/// refuses the join after it.
async fn node_that_gives_the_first_join_up() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        ws.send(hello()).await.unwrap();
        let mut joins = 0;
        while let Some(Ok(Message::Text(request))) = ws.next().await {
            let (code, id) = match serde_json::from_str(&request).unwrap() {
                ClientMsg::Auth { .. } => {
                    ws.send(authed()).await.unwrap();
                    continue;
                }
                ClientMsg::Leave { id } => {
                    ws.send(left(id)).await.unwrap();
                    continue;
                }
                ClientMsg::Join { id, .. } => {
                    joins += 1;
                    // the id of a join the client gave up: this error is not its answer
                    match joins {
                        1 => ("transport", id.map(|id| id + 1)),
                        _ => ("not_found", id),
                    }
                }
                _ => continue,
            };
            let error = ServerMsg::Error {
                code: code.into(),
                message: String::new(),
                id,
            };
            ws.send(text(&error)).await.unwrap();
        }
    });
    url
}

#[tokio::test]
async fn a_loss_that_ended_a_join_does_not_end_the_join_after_it() {
    let url = node_that_gives_the_first_join_up().await;
    let mut client = Client::connect(&url, "grant", Options::default())
        .await
        .unwrap();
    let first = client.join(join("first")).await;
    assert!(
        matches!(first, Err(ClientError::Disconnected)),
        "{:?}",
        first.err()
    );
    let second = client.join(join("second")).await;
    assert!(
        matches!(&second, Err(ClientError::Server { code, .. }) if code == "not_found"),
        "{:?}",
        second.err()
    );
    // the game joined again before it asked: the loss was of the stay it gave up
    tokio::time::sleep(Duration::from_millis(20)).await;
    while let Some(event) = client.try_next_event() {
        assert!(!matches!(event, Event::Closed(_)), "{event:?}");
    }
}

#[tokio::test]
async fn what_the_room_sent_is_not_handed_out_after_the_leave() {
    let mut client = client_of(Node {
        says: 3,
        ..Node::default()
    })
    .await;
    client.join(join("first")).await.unwrap();
    tokio::time::sleep(WAIT).await;
    let heard = std::iter::from_fn(|| client.try_next_event())
        .filter(|event| matches!(event, Event::Data { .. }))
        .count();
    assert_eq!(heard, 3);
    client.leave().await.unwrap();
    client.join(join("second")).await.unwrap();
    tokio::time::sleep(WAIT).await;
    client.leave().await.unwrap();
    let after: Vec<Event> = std::iter::from_fn(|| client.try_next_event()).collect();
    assert!(
        after.iter().all(|event| matches!(event, Event::Control(_))),
        "{after:?}"
    );
}

#[tokio::test]
async fn what_the_room_sent_after_it_took_the_client_out_is_not_handed_out() {
    let mut client = client_of(Node {
        says_after_kick: 3,
        ..Node::default()
    })
    .await;
    client.join(join("room")).await.unwrap();
    client.stats(1).await.unwrap();
    tokio::time::sleep(WAIT).await;
    let after: Vec<Event> = std::iter::from_fn(|| client.try_next_event()).collect();
    assert!(
        matches!(
            after.as_slice(),
            [Event::Control(ServerMsg::Left { reason, .. })] if reason == "kicked"
        ),
        "{after:?}"
    );
}
