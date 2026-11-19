//! A node that stops answering does not hold `connect` and `join` forever.

use futures::{SinkExt, StreamExt};
use net_client::{Client, ClientError, Options};
use net_rooms::protocol::{ClientMsg, ServerMsg};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

/// A node that takes control connections, answers `auth` when `authed` is set and nothing else.
async fn silent_node(authed: bool) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                while let Some(Ok(message)) = ws.next().await {
                    let auth = matches!(&message, Message::Text(text) if text.contains("\"auth\""));
                    if authed && auth {
                        let answer = ServerMsg::Authed {
                            player_id: "ann".into(),
                            name: "Ann".into(),
                        };
                        let text = serde_json::to_string(&answer).unwrap();
                        ws.send(Message::Text(text.into())).await.unwrap();
                    }
                }
            });
        }
    });
    url
}

fn short() -> Options {
    Options {
        connect_timeout: Duration::from_millis(200),
        ..Options::default()
    }
}

#[tokio::test]
async fn connect_gives_up_when_auth_is_not_answered() {
    let node = silent_node(false).await;
    let result = Client::connect(&node, "grant", short()).await;
    assert!(matches!(result, Err(ClientError::Timeout)));
}

#[tokio::test]
async fn join_gives_up_when_it_is_not_answered() {
    let node = silent_node(true).await;
    let mut client = Client::connect(&node, "grant", short()).await.unwrap();
    let join = ClientMsg::Join {
        name: "room".into(),
        player_props: Default::default(),
        rejoin_actor: None,
        schema_hash: None,
        spectator: false,
        id: None,
    };
    assert!(matches!(client.join(join).await, Err(ClientError::Timeout)));
}
