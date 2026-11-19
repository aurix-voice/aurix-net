//! Live check of `Client::rejoin`: joins a lockstep room, waits for `left{reason:"rehost"}`
//! (node shutting down), rejoins through the fallback URL and prints what it sees.
//! Usage: rehost <http_url> <grant> <room_name> <fallback_url>
use net_client::{Client, Event, Options};
use net_rooms::protocol::{ClientMsg, ServerMsg};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let (url, grant, room, fallback) = (&args[1], &args[2], &args[3], &args[4]);
    let join = ClientMsg::JoinOrCreate {
        name: room.clone(),
        mode: net_common::types::RoomMode::Lockstep,
        max_players: Some(4),
        props: serde_json::from_str(r#"{"tick_hz":60}"#)?,
        lobby_props: vec![],
        region: None,
        player_props: Default::default(),
        reserved: Vec::new(),
    };
    let (mut client, joined) =
        Client::connect_and_join(url, grant, Options::default(), join).await?;
    println!(
        "joined room={} actor={} on {url}",
        joined.name, joined.actor.0
    );
    let mut bundles = 0u32;
    loop {
        match client.next_event().await {
            Some(Event::Control(ServerMsg::Left { reason, .. })) => {
                println!("left: {reason} after {bundles} bundles");
                if reason != "rehost" {
                    anyhow::bail!("unexpected leave");
                }
                if client.joined.is_some() {
                    anyhow::bail!("still describes the room the node handed off");
                }
                let (mut rejoined_client, rejoined) = client.rejoin(&[fallback.as_str()]).await?;
                println!(
                    "rejoined room={} actor={} (same actor: {})",
                    rejoined.name,
                    rejoined.actor.0,
                    rejoined.actor == joined.actor
                );
                let mut frames = 0;
                while frames < 60 {
                    match rejoined_client.next_event().await {
                        Some(Event::Data { .. }) => frames += 1,
                        Some(Event::Closed(reason)) => {
                            anyhow::bail!("closed after rejoin: {reason}")
                        }
                        None => anyhow::bail!("stream ended after rejoin"),
                        _ => {}
                    }
                }
                println!("REHOST_OK: {frames} data frames after rejoin");
                return Ok(());
            }
            Some(Event::Data { .. }) => bundles += 1,
            Some(Event::Closed(reason)) => anyhow::bail!("closed: {reason}"),
            None => anyhow::bail!("stream ended"),
            _ => {}
        }
    }
}
