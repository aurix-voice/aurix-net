//! Live check of `Client::rejoin`: joins a lockstep room, waits for `left{reason:"rehost"}`
//! (node shutting down), rejoins through the fallback URL and prints what it sees.
//! Usage: rehost <http_url> <grant> <room_name> <fallback_url>
use net_client::{Client, Event, Options};
use net_rooms::protocol::{ClientMsg, ServerMsg};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().collect();
    let (url, grant, room, fallback) = (&a[1], &a[2], &a[3], &a[4]);
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
    let (mut c, j) = Client::connect_and_join(url, grant, Options::default(), join).await?;
    println!("joined room={} actor={} on {url}", j.name, j.actor.0);
    let mut bundles = 0u32;
    loop {
        match c.next_event().await {
            Some(Event::Control(ServerMsg::Left { reason })) => {
                println!("left: {reason} after {bundles} bundles");
                if reason != "rehost" {
                    anyhow::bail!("unexpected leave");
                }
                let (mut c2, j2) = c.rejoin(&[fallback.as_str()]).await?;
                println!(
                    "rejoined room={} actor={} (same actor: {})",
                    j2.name,
                    j2.actor.0,
                    j2.actor == j.actor
                );
                let mut n = 0;
                while n < 60 {
                    match c2.next_event().await {
                        Some(Event::Data { .. }) => n += 1,
                        Some(Event::Closed(r)) => anyhow::bail!("closed after rejoin: {r}"),
                        None => anyhow::bail!("stream ended after rejoin"),
                        _ => {}
                    }
                }
                println!("REHOST_OK: {n} data frames after rejoin");
                return Ok(());
            }
            Some(Event::Data { .. }) => bundles += 1,
            Some(Event::Closed(r)) => anyhow::bail!("closed: {r}"),
            None => anyhow::bail!("stream ended"),
            _ => {}
        }
    }
}
