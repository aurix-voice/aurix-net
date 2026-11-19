//! A client that loses its room hands the game the reliable events the node did not acknowledge.

use base64::Engine;
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use net_client::net_common::{ActorId, Channel, RoomMode};
use net_client::protocol::{ClientMsg, DataEndpoint, ServerMsg};
use net_client::{CacheOp, Client, Event, EventFrame, EventTarget, Options, LEAVE_WAIT};
use net_transport::Link;
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

fn text(message: &ServerMsg) -> Message {
    Message::Text(serde_json::to_string(message).unwrap().into())
}

fn left(reason: &str) -> ServerMsg {
    ServerMsg::Left {
        reason: reason.into(),
        room: Some("id".into()),
        id: None,
    }
}

/// What a node does with the datagrams that follow the event with code 1.
#[derive(Clone, Copy, PartialEq)]
enum Later {
    /// It reads none of them.
    Deaf,
    /// It reads none of them and closes the connection at the third.
    Gone,
    /// The first three are lost on the way, the rest it reads and acknowledges.
    Lossy,
}

/// The codes of the reliable events a node read after the one with code 1, and 0 for a `leave`,
/// in the order they came.
type Heard = Arc<Mutex<Vec<u8>>>;

/// A node that lets a client into a room over the WebSocket tunnel and acknowledges what it is
/// sent up to the event with code 1, which it answers. It answers a `leave`; at a `stats` it
/// takes the client out of the room with `end`, or closes the connection when there is none.
async fn node(end: Option<ServerMsg>, later: Later) -> (String, Heard) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let heard = Heard::default();
    let codes = heard.clone();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        let mut link: Option<Link> = None;
        let mut deaf = false;
        let mut unread = 0;
        while let Some(Ok(message)) = ws.next().await {
            match message {
                Message::Text(request) if request.contains("\"auth\"") => {
                    let authed = ServerMsg::Authed {
                        player_id: "ann".into(),
                        name: "Ann".into(),
                    };
                    ws.send(text(&authed)).await.unwrap();
                }
                Message::Text(request) if request.contains("\"join\"") => {
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
                        id: None,
                    };
                    ws.send(text(&joined)).await.unwrap();
                }
                Message::Text(request) if request.contains("\"leave\"") => {
                    codes.lock().push(0);
                    ws.send(text(&left("left"))).await.unwrap();
                }
                Message::Text(request) if request.contains("\"stats\"") => match &end {
                    Some(end) => ws.send(text(end)).await.unwrap(),
                    None => break,
                },
                Message::Binary(datagram) => {
                    let now = Instant::now();
                    let link = match &mut link {
                        Some(_) if deaf && (later != Later::Lossy || unread < 3) => {
                            unread += 1;
                            if later == Later::Gone && unread == 3 {
                                break;
                            }
                            continue;
                        }
                        Some(link) => {
                            for message in link.receive(datagram, now) {
                                let reliable = message.channel != Channel::UnreliableSequenced;
                                let Ok(event) = EventFrame::decode(message.payload) else {
                                    continue;
                                };
                                if deaf && reliable {
                                    codes.lock().push(event.code);
                                } else if event.code == 1 {
                                    let heard = Bytes::from_static(b"heard");
                                    link.send(Channel::ReliableOrdered, heard, now).unwrap();
                                    deaf = true;
                                }
                            }
                            link
                        }
                        None => {
                            let link = link.insert(Link::new(now, Duration::from_secs(15)));
                            link.send(Channel::ReliableOrdered, Bytes::new(), now)
                                .unwrap();
                            link
                        }
                    };
                    let _ = link.poll(now);
                    while let Some(datagram) = link.next_outgoing() {
                        ws.send(Message::Binary(datagram)).await.unwrap();
                    }
                }
                _ => {}
            }
        }
    });
    (url, heard)
}

fn event(code: u8, target: EventTarget, payload: Vec<u8>) -> EventFrame {
    EventFrame {
        code,
        target,
        cache: CacheOp::None,
        group: None,
        from: ActorId(0),
        payload: payload.into(),
    }
}

/// The reliable events of [`told_after`] the node does not acknowledge.
fn unacknowledged() -> Vec<EventFrame> {
    vec![
        event(
            2,
            EventTarget::Actors(vec![ActorId(7), ActorId(9)]),
            vec![2],
        ),
        EventFrame {
            cache: CacheOp::Add,
            ..event(5, EventTarget::Master, vec![5; 3000])
        },
        EventFrame {
            group: Some(9),
            ..event(6, EventTarget::All, vec![6])
        },
    ]
}

/// What the game is told, up to the `left` or the `closed`, after it sent an event the node
/// acknowledged and messages it did not: the events of [`unacknowledged`], an unreliable
/// event and reliable data that is not an event. The game leaves then, or the room ends with
/// `end`. With it comes what the node read.
async fn told_after(end: Option<ServerMsg>, later: Later, leaves: bool) -> (Vec<String>, Vec<u8>) {
    let options = Options {
        tunnel: true,
        ..Options::default()
    };
    let (node, heard) = node(end, later).await;
    let mut client = Client::connect(&node, "grant", options).await.unwrap();
    let join = ClientMsg::Join {
        name: "room".into(),
        player_props: Default::default(),
        rejoin_actor: None,
        schema_hash: None,
        spectator: false,
        id: None,
    };
    client.join(join).await.unwrap();
    let send = |client: &Client, channel, frame: EventFrame| {
        let EventFrame {
            code,
            target,
            cache,
            group,
            payload,
            ..
        } = frame;
        client
            .send_event_on(channel, code, target, payload, group, cache)
            .unwrap();
    };
    send(
        &client,
        Channel::ReliableOrdered,
        event(1, EventTarget::Others, vec![1]),
    );
    client.flush();
    while !matches!(client.next_event().await, Some(Event::Data { .. })) {}

    let [first, large, loose] = unacknowledged().try_into().unwrap();
    send(&client, Channel::ReliableOrdered, first);
    send(
        &client,
        Channel::UnreliableSequenced,
        event(3, EventTarget::Others, vec![3]),
    );
    client
        .send_data(
            Channel::ReliableOrdered,
            event(4, EventTarget::Others, vec![4]).encode(),
        )
        .unwrap();
    send(&client, Channel::ReliableOrdered, large);
    send(&client, Channel::ReliableUnordered, loose);
    client.flush();
    if leaves {
        client.leave().await.unwrap();
    } else {
        client.stats(1).await.unwrap();
    }

    let mut told = Vec::new();
    while let Some(event) = client.next_event().await {
        match event {
            Event::Undelivered(events) => told.push(format!("{events:?}")),
            Event::Control(ServerMsg::Left { reason, .. }) => {
                told.push(format!("left: {reason}"));
                break;
            }
            Event::Closed(reason) => {
                told.push(format!("closed: {reason}"));
                break;
            }
            _ => {}
        }
    }
    let heard = heard.lock().clone();
    (told, heard)
}

#[tokio::test]
async fn a_room_the_node_ends_hands_back_the_events_it_did_not_acknowledge() {
    for reason in ["rehost", "kicked", "room closed"] {
        assert_eq!(
            told_after(Some(left(reason)), Later::Deaf, false).await.0,
            [format!("{:?}", unacknowledged()), format!("left: {reason}")]
        );
    }
}

#[tokio::test]
async fn a_lost_connection_hands_back_the_events_the_node_did_not_acknowledge() {
    assert_eq!(
        told_after(None, Later::Deaf, false).await.0,
        [
            format!("{:?}", unacknowledged()),
            "closed: control channel closed".into()
        ]
    );
}

#[tokio::test]
async fn a_leave_waits_for_the_node_to_have_what_the_game_sent_reliably() {
    let (told, mut heard) = told_after(None, Later::Lossy, true).await;
    assert_eq!(told, ["left: left"]);
    assert_eq!(heard.pop(), Some(0));
    heard.sort_unstable();
    assert_eq!(heard, [2, 4, 5, 6]);
}

#[tokio::test]
async fn a_leave_the_node_does_not_acknowledge_in_time_hands_the_events_back() {
    let began = Instant::now();
    assert_eq!(
        told_after(None, Later::Deaf, true).await.0,
        [format!("{:?}", unacknowledged()), "left: left".into()]
    );
    assert!(began.elapsed() < LEAVE_WAIT + Duration::from_secs(1));
}

#[tokio::test]
async fn a_connection_lost_under_a_leave_hands_back_the_events_as_any_lost_one() {
    assert_eq!(
        told_after(None, Later::Gone, true).await.0,
        [
            format!("{:?}", unacknowledged()),
            "closed: control channel closed".into()
        ]
    );
}
