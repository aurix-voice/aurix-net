//! A client reaches a node that stands behind TLS: the control channel over `wss://`, and the
//! data path through it.

use base64::Engine;
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use net_client::net_common::{ActorId, Channel, RoomMode};
use net_client::protocol::{ClientMsg, DataEndpoint, ServerMsg};
use net_client::{Client, ClientError, Event, EventTarget, Options};
use net_transport::{quic, Link};
use rustls::pki_types::CertificateDer;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::tungstenite::Message;

fn text(message: &ServerMsg) -> Message {
    Message::Text(serde_json::to_string(message).unwrap().into())
}

fn joined() -> ServerMsg {
    ServerMsg::Joined {
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
            ticket: base64::engine::general_purpose::STANDARD.encode(b"ticket"),
            ws_tunnel: true,
            webtransport_url: None,
        },
        cached: Vec::new(),
        schema_hash: None,
        tick_hz: None,
        tick: None,
        seed: None,
        next_spawn_tag: None,
    }
}

/// A node behind TLS with a certificate of its own for `localhost`. It lets a client into a
/// room over the WebSocket tunnel and sends back every message the data path brings.
async fn node() -> (String, CertificateDer<'static>) {
    let identity = quic::server_identity(None, None, &["localhost".into()]).unwrap();
    let certificate = identity.cert_chain[0].clone();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(identity.cert_chain, identity.key)
        .unwrap();
    let acceptor = TlsAcceptor::from(Arc::new(config));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "https://localhost:{}",
        listener.local_addr().unwrap().port()
    );
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let Ok(stream) = acceptor.accept(stream).await else {
            return;
        };
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        let mut link: Option<Link> = None;
        while let Some(Ok(message)) = ws.next().await {
            let now = Instant::now();
            match message {
                Message::Text(request) if request.contains("\"auth\"") => {
                    let authed = ServerMsg::Authed {
                        player_id: "ann".into(),
                        name: "Ann".into(),
                    };
                    ws.send(text(&authed)).await.unwrap();
                }
                Message::Text(_) => ws.send(text(&joined())).await.unwrap(),
                Message::Binary(datagram) => {
                    let link = match &mut link {
                        Some(link) => {
                            for message in link.receive(datagram, now) {
                                link.send(message.channel, message.payload, now).unwrap();
                            }
                            link
                        }
                        None => {
                            assert_eq!(&datagram[..], b"ticket");
                            let link = link.insert(Link::new(now, Duration::from_secs(15)));
                            link.send(Channel::ReliableOrdered, Bytes::new(), now)
                                .unwrap();
                            link
                        }
                    };
                    link.poll(now).unwrap();
                    link.flush_batch();
                    while let Some(datagram) = link.next_outgoing() {
                        ws.send(Message::Binary(datagram)).await.unwrap();
                    }
                }
                _ => {}
            }
        }
    });
    (url, certificate)
}

#[tokio::test]
async fn a_client_joins_and_sends_through_a_node_behind_tls() {
    let (node, certificate) = node().await;
    let options = Options {
        tunnel: true,
        roots: vec![certificate],
        ..Options::default()
    };
    let mut client = Client::connect(&node, "grant", options).await.unwrap();
    assert_eq!((client.player_id(), client.player_name()), ("ann", "Ann"));
    let join = ClientMsg::Join {
        name: "room".into(),
        player_props: Default::default(),
        rejoin_actor: None,
        schema_hash: None,
        spectator: false,
    };
    client.join(join).await.unwrap();
    client
        .send_event(
            7,
            EventTarget::Others,
            Bytes::from_static(b"over tls"),
            true,
        )
        .unwrap();
    let back = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match client.next_event().await {
                Some(Event::Data { payload, .. }) if !payload.is_empty() => break payload,
                Some(_) => {}
                None => panic!("the connection closed before the event came back"),
            }
        }
    })
    .await
    .unwrap();
    assert!(back.ends_with(b"over tls"), "{back:?}");
}

#[tokio::test]
async fn a_certificate_no_trusted_authority_signed_is_refused() {
    let (node, _) = node().await;
    let refused = Client::connect(&node, "grant", Options::default()).await;
    let Err(ClientError::Ws(reason)) = refused else {
        panic!("connected to a node whose certificate nothing vouches for");
    };
    assert!(reason.contains("UnknownIssuer"), "{reason}");
}
