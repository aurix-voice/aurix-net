//! A client that loses its room hands the game the reliable events the node did not acknowledge.

use base64::Engine;
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use net_client::net_common::{ActorId, Channel, RoomMode};
use net_client::protocol::{ClientMsg, DataEndpoint, ServerMsg};
use net_client::{CacheOp, Client, Event, EventFrame, EventTarget, Options};
use net_transport::Link;
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
    }
}

/// A node that lets a client into a room over the WebSocket tunnel and acknowledges what it is
/// sent up to the event with code 1, which it answers. It answers a `leave`; at a `stats` it
/// takes the client out of the room with `end`, or closes the connection when there is none.
async fn node(end: Option<ServerMsg>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        let mut link: Option<Link> = None;
        let mut deaf = false;
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
                    };
                    ws.send(text(&joined)).await.unwrap();
                }
                Message::Text(request) if request.contains("\"leave\"") => {
                    ws.send(text(&left("left"))).await.unwrap();
                }
                Message::Text(request) if request.contains("\"stats\"") => match &end {
                    Some(end) => ws.send(text(end)).await.unwrap(),
                    None => break,
                },
                Message::Binary(datagram) => {
                    let now = Instant::now();
                    let link = match &mut link {
                        Some(link) if deaf => continue,
                        Some(link) => {
                            for message in link.receive(datagram, now) {
                                if EventFrame::decode(message.payload).is_ok_and(|e| e.code == 1) {
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
/// event and reliable data that is not an event. The room ends with `end`, or the game leaves.
async fn told_after(end: Option<ServerMsg>, leaves: bool) -> Vec<String> {
    let options = Options {
        tunnel: true,
        ..Options::default()
    };
    let mut client = Client::connect(&node(end).await, "grant", options)
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
    told
}

#[tokio::test]
async fn a_room_the_node_ends_hands_back_the_events_it_did_not_acknowledge() {
    for reason in ["rehost", "kicked", "room closed"] {
        assert_eq!(
            told_after(Some(left(reason)), false).await,
            [format!("{:?}", unacknowledged()), format!("left: {reason}")]
        );
    }
}

#[tokio::test]
async fn a_lost_connection_hands_back_the_events_the_node_did_not_acknowledge() {
    assert_eq!(
        told_after(None, false).await,
        [
            format!("{:?}", unacknowledged()),
            "closed: control channel closed".into()
        ]
    );
}

#[tokio::test]
async fn a_game_that_leaves_is_handed_nothing_back() {
    assert_eq!(told_after(None, true).await, ["left: left"]);
}
