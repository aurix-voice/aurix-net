//! Two clients join one relay room; A sends an event, B receives it.
//! `NET_API_KEY=an_... cargo run -p net-client --example relay_echo`

use bytes::Bytes;
use net_client::protocol::{ClientMsg, ServerMsg};
use net_client::{issue_grant, Client, Event, EventFrame, EventTarget, Options};
use net_common::RoomMode;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let http = std::env::var("NET_HTTP").unwrap_or_else(|_| "http://127.0.0.1:8090".into());
    let key = std::env::var("NET_API_KEY")?;
    let room = format!("echo-{}", std::process::id());
    let join = |name: &str| ClientMsg::JoinOrCreate {
        name: name.to_string(),
        mode: RoomMode::Relay,
        max_players: Some(4),
        props: Default::default(),
        lobby_props: vec![],
        region: None,
        player_props: Default::default(),
        reserved: Vec::new(),
    };

    let ga = issue_grant(&http, &key, "alice", "Alice").await?;
    let gb = issue_grant(&http, &key, "bob", "Bob").await?;
    let mut a = Client::connect(&http, &ga, Options::default()).await?;
    let mut b = Client::connect(
        &http,
        &gb,
        Options {
            tunnel: true,
            ..Default::default()
        },
    )
    .await?;
    let ja = a.join(join(&room)).await?;
    let jb = b.join(join(&room)).await?;
    println!(
        "alice actor={} (quic), bob actor={} (ws tunnel), room={}",
        ja.actor.0, jb.actor.0, ja.name
    );

    // A: reliable hello, then 10 unreliable ticks
    a.send_event(1, EventTarget::Others, Bytes::from_static(b"hello"), true)?;
    for i in 0..10u8 {
        a.send_event(2, EventTarget::Others, Bytes::from(vec![i]), false)?;
        a.flush();
        tokio::time::sleep(std::time::Duration::from_millis(16)).await;
    }

    let mut got_hello = false;
    let mut ticks = 0;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    while let Ok(Some(ev)) = tokio::time::timeout_at(deadline, b.next_event()).await {
        match ev {
            Event::Data { payload, .. } => {
                let f = EventFrame::decode(payload)?;
                match f.code {
                    1 => got_hello = f.payload.as_ref() == b"hello" && f.from == ja.actor,
                    2 => ticks += 1,
                    _ => {}
                }
                if got_hello && ticks == 10 {
                    break;
                }
            }
            Event::Control(ServerMsg::PlayerJoined { player }) => {
                println!("bob sees joined: {}", player.name)
            }
            Event::Closed(r) => return Err(format!("closed: {r}").into()),
            _ => {}
        }
    }
    println!(
        "bob got hello={got_hello} ticks={ticks}/10 stats={:?}",
        b.link_stats()
    );
    a.leave().await?;
    assert!(got_hello && ticks == 10, "echo failed");
    Ok(())
}
