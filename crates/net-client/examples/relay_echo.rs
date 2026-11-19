//! Two clients join one relay room; A sends an event, B receives it. Then A leaves and the node
//! kicks B: neither describes the room afterwards. With `NET_BREAK_HTTP` (see `sdk/cpp/README.md`)
//! a client then loses its room with reliable events the node did not acknowledge.
//! `NET_API_KEY=an_... cargo run -p net-client --example relay_echo`

use bytes::Bytes;
use net_client::protocol::{ClientMsg, ServerMsg};
use net_client::{issue_grant, CacheOp, Client, Event, EventFrame, EventTarget, Options};
use net_common::{ActorId, Channel, RoomMode};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let http = std::env::var("NET_HTTP").unwrap_or_else(|_| "http://127.0.0.1:8090".into());
    let key = std::env::var("NET_API_KEY")?;
    let room = format!("echo-{}", std::process::id());
    let join = |name: &str| ClientMsg::JoinOrCreate {
        name: name.to_string(),
        mode: RoomMode::Relay,
        max_players: Some(4),
        visible: true,
        open: true,
        props: Default::default(),
        lobby_props: vec![],
        region: None,
        player_props: Default::default(),
        reserved: Vec::new(),
        id: None,
    };

    let alice_grant = issue_grant(&http, &key, "alice", "Alice").await?;
    let bob_grant = issue_grant(&http, &key, "bob", "Bob").await?;
    let mut alice = Client::connect(&http, &alice_grant, Options::default()).await?;
    let mut bob = Client::connect(
        &http,
        &bob_grant,
        Options {
            tunnel: true,
            ..Default::default()
        },
    )
    .await?;
    let alice_joined = alice.join(join(&room)).await?;
    let bob_joined = bob.join(join(&room)).await?;
    println!(
        "alice actor={} (quic), bob actor={} (ws tunnel), room={}",
        alice_joined.actor.0, bob_joined.actor.0, alice_joined.name
    );

    // A: reliable hello, then 10 unreliable ticks
    alice.send_event(1, EventTarget::Others, Bytes::from_static(b"hello"), true)?;
    for i in 0..10u8 {
        alice.send_event(2, EventTarget::Others, Bytes::from(vec![i]), false)?;
        alice.flush();
        tokio::time::sleep(std::time::Duration::from_millis(16)).await;
    }

    let mut got_hello = false;
    let mut ticks = 0;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    while let Ok(Some(ev)) = tokio::time::timeout_at(deadline, bob.next_event()).await {
        match ev {
            Event::Data { payload, .. } => {
                let frame = EventFrame::decode(payload)?;
                match frame.code {
                    1 => {
                        got_hello =
                            frame.payload.as_ref() == b"hello" && frame.from == alice_joined.actor
                    }
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
            Event::Closed(reason) => return Err(format!("closed: {reason}").into()),
            _ => {}
        }
    }
    println!(
        "bob got hello={got_hello} ticks={ticks}/10 stats={:?}",
        bob.link_stats()
    );
    assert!(got_hello && ticks == 10, "echo failed");

    alice.leave().await?;
    assert!(alice.joined.is_none(), "alice still describes the room");

    reqwest::Client::new()
        .delete(format!(
            "{http}/v1/rooms/{}/players/{}",
            bob_joined.room, bob_joined.actor.0
        ))
        .bearer_auth(&key)
        .send()
        .await?
        .error_for_status()?;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    while let Ok(Some(ev)) = tokio::time::timeout_at(deadline, bob.next_event()).await {
        if let Event::Control(ServerMsg::Left { reason, .. }) = ev {
            println!("bob is out: {reason}");
            break;
        }
    }
    assert!(bob.joined.is_none(), "bob still describes the room");

    // over a link that takes 100 ms each way and closes on `stats`: the reliable events the node
    // did not acknowledge come before the room is lost, and not to a client that leaves
    let Ok(broken) = std::env::var("NET_BREAK_HTTP") else {
        println!("broken link: skipped (NET_BREAK_HTTP not set)");
        return Ok(());
    };
    for how in ["kick", "cut", "leave"] {
        let room = format!("{room}-{how}");
        let master_grant = issue_grant(&http, &key, "master", "Master").await?;
        let sender_grant = issue_grant(&http, &key, "sender", "Sender").await?;
        let mut master = Client::connect(&http, &master_grant, Options::default()).await?;
        let mut sender = Client::connect(
            &broken,
            &sender_grant,
            Options {
                tunnel: true,
                ..Default::default()
            },
        )
        .await?;
        master.join(join(&room)).await?;
        let actor = sender
            .join(ClientMsg::Join {
                name: room,
                player_props: Default::default(),
                rejoin_actor: None,
                schema_hash: None,
                spectator: false,
                id: None,
            })
            .await?
            .actor;
        sender.send_event(1, EventTarget::Others, Bytes::from_static(b"1"), true)?;
        sender.flush();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
        while let Ok(Some(ev)) = tokio::time::timeout_at(deadline, master.next_event()).await {
            if matches!(ev, Event::Data { .. }) {
                break;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;

        let to_actors = EventTarget::Actors(vec![ActorId(7), ActorId(9)]);
        sender.send_event(2, to_actors, Bytes::from_static(b"2"), true)?;
        sender.send_event(3, EventTarget::Others, Bytes::from_static(b"3"), false)?;
        let big = Bytes::from(vec![0; 3000]);
        sender.send_event_ex(5, EventTarget::Master, big, true, None, CacheOp::Add)?;
        sender.send_event_on(
            Channel::ReliableUnordered,
            6,
            EventTarget::All,
            Bytes::from_static(b"6"),
            Some(9),
            CacheOp::None,
        )?;
        sender.flush();
        match how {
            "kick" => {
                let reason = String::new();
                master.send(ClientMsg::Kick { actor, reason }).await?
            }
            "cut" => sender.send(ClientMsg::Stats { id: None }).await?,
            _ => sender.leave().await?,
        }

        let mut told = Vec::new();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(1);
        while let Ok(Some(ev)) = tokio::time::timeout_at(deadline, sender.next_event()).await {
            match ev {
                Event::Undelivered(events) => told.push(
                    events
                        .iter()
                        .map(|event| format!("{}:{}", event.code, event.payload.len()))
                        .collect::<Vec<_>>()
                        .join(" "),
                ),
                Event::Control(ServerMsg::Left { reason, .. }) => {
                    told.push(format!("left {reason}"))
                }
                Event::Closed(_) => told.push("closed".to_string()),
                _ => {}
            }
        }
        println!("undelivered events, {how}: {told:?}");
        let want: &[&str] = match how {
            "kick" => &["2:1 5:3000 6:1", "left kicked"],
            "cut" => &["2:1 5:3000 6:1", "closed"],
            _ => &["left left"],
        };
        assert_eq!(told, want, "undelivered events, {how}");
    }
    Ok(())
}
