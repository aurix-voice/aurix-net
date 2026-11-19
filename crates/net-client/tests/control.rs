//! The control channel: what does not fit a message of it, and what the node closes it with.

use futures::{SinkExt, StreamExt};
use net_client::protocol::{ClientMsg, ServerMsg};
use net_client::{
    Client, ClientError, Event, Options, MAX_CONTROL_MESSAGE, MAX_CONTROL_MESSAGE_BEFORE_AUTH,
};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::Message;

const WAIT: Duration = Duration::from_millis(300);

/// A node that signs a client in and tells the size of every request after it; with `closes`,
/// it closes the control channel on the first one as a node does on a message over its limit.
async fn node(closes: bool) -> (String, mpsc::UnboundedReceiver<usize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (sizes, read) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        while let Some(Ok(message)) = ws.next().await {
            let Message::Text(request) = message else {
                continue;
            };
            if request.contains("\"auth\"") {
                let authed = ServerMsg::Authed {
                    player_id: "ann".into(),
                    name: "Ann".into(),
                };
                let authed = serde_json::to_string(&authed).unwrap();
                ws.send(Message::Text(authed.into())).await.unwrap();
            } else if !request.contains("\"ping\"") {
                let _ = sizes.send(request.len());
                if closes {
                    let frame = CloseFrame {
                        code: CloseCode::Size,
                        reason: "message too large: at most 69632 bytes".into(),
                    };
                    ws.close(Some(frame)).await.unwrap();
                }
            }
        }
    });
    (url, read)
}

/// A request whose JSON is `bytes` long.
fn request_of(bytes: usize) -> ClientMsg {
    let of = |name: String| ClientMsg::FindFriends {
        players: vec![name],
        id: None,
    };
    let empty = serde_json::to_string(&of(String::new())).unwrap().len();
    of("a".repeat(bytes - empty))
}

#[tokio::test]
async fn a_control_message_over_what_the_node_reads_is_refused_and_not_sent() {
    let (url, mut read) = node(false).await;
    let client = Client::connect(&url, "grant", Options::default())
        .await
        .unwrap();
    let over = client.send(request_of(MAX_CONTROL_MESSAGE + 1)).await;
    assert!(
        matches!(over, Err(ClientError::TooLarge(bytes, MAX_CONTROL_MESSAGE)) if bytes == MAX_CONTROL_MESSAGE + 1),
        "{over:?}"
    );
    client.send(request_of(MAX_CONTROL_MESSAGE)).await.unwrap();
    let got = tokio::time::timeout(WAIT, read.recv()).await.unwrap();
    assert_eq!(got, Some(MAX_CONTROL_MESSAGE));
}

#[tokio::test]
async fn what_the_node_closed_the_control_channel_with_is_told() {
    let (url, _read) = node(true).await;
    let mut client = Client::connect(&url, "grant", Options::default())
        .await
        .unwrap();
    client.stats(1).await.unwrap();
    let closed = loop {
        match tokio::time::timeout(WAIT, client.next_event()).await {
            Ok(Some(Event::Closed(reason))) => break reason,
            Ok(Some(_)) => {}
            other => panic!("{other:?}"),
        }
    };
    assert_eq!(
        closed,
        "control channel closed: 1009 message too large: at most 69632 bytes"
    );
    let after = client.stats(2).await;
    assert!(
        matches!(&after, Err(ClientError::Closed(said)) if said == "1009 message too large: at most 69632 bytes"),
        "{after:?}"
    );
}

/// A node that closes on the sign-in, as a node does on one over its limit, and tells its size.
async fn node_that_reads_no_sign_in() -> (String, mpsc::UnboundedReceiver<usize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (sizes, read) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        while let Some(Ok(message)) = ws.next().await {
            let Message::Text(request) = message else {
                continue;
            };
            if request.contains("\"auth\"") {
                let _ = sizes.send(request.len());
                let frame = CloseFrame {
                    code: CloseCode::Size,
                    reason: "message too large: at most 8192 bytes".into(),
                };
                ws.close(Some(frame)).await.unwrap();
            }
        }
    });
    (url, read)
}

#[tokio::test]
async fn a_sign_in_over_what_the_node_reads_before_it_is_refused_and_not_sent() {
    let (url, mut read) = node_that_reads_no_sign_in().await;
    let grant = "g".repeat(MAX_CONTROL_MESSAGE_BEFORE_AUTH);
    let refused = Client::connect(&url, &grant, Options::default()).await;
    assert!(
        matches!(
            refused,
            Err(ClientError::TooLarge(bytes, MAX_CONTROL_MESSAGE_BEFORE_AUTH)) if bytes > MAX_CONTROL_MESSAGE_BEFORE_AUTH
        ),
        "{:?}",
        refused.err()
    );
    let heard = tokio::time::timeout(WAIT, read.recv()).await;
    assert!(!matches!(heard, Ok(Some(_))), "{heard:?}");
}

#[tokio::test]
async fn what_the_node_closed_the_sign_in_with_is_why_the_connect_failed() {
    let (url, _read) = node_that_reads_no_sign_in().await;
    let refused = Client::connect(&url, "grant", Options::default()).await;
    assert!(
        matches!(
            &refused,
            Err(ClientError::Closed(said)) if said == "1009 message too large: at most 8192 bytes"
        ),
        "{:?}",
        refused.err()
    );
}
