//! Live check of `Client::rejoin`: joins a lockstep room, waits for `left{reason:"rehost"}`
//! (node shutting down), rejoins through the fallback URL and prints what it sees.
//! With NET_API_KEY set it also brings a client into a full room by a random join, beside an
//! open room the same join could match, and checks that its rejoin leads back to its own room.
//! Usage: rehost <http_url> <grant> <room_name> <fallback_url>
use net_client::{issue_grant, Client, Event, Options};
use net_rooms::protocol::{ClientMsg, ServerMsg};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let (url, grant, room, fallback) = (&args[1], &args[2], &args[3], &args[4]);
    let join = ClientMsg::JoinOrCreate {
        name: room.clone(),
        mode: net_common::types::RoomMode::Lockstep,
        max_players: Some(4),
        visible: true,
        open: true,
        props: serde_json::from_str(r#"{"tick_hz":60}"#)?,
        lobby_props: vec![],
        region: None,
        player_props: Default::default(),
        reserved: Vec::new(),
        id: None,
    };
    let (mut client, joined) =
        Client::connect_and_join(url, grant, Options::default(), join).await?;
    println!(
        "joined room={} actor={} on {url}",
        joined.name, joined.actor.0
    );
    let random = match std::env::var("NET_API_KEY") {
        Ok(key) => Some(random_join(url, &key, room).await?),
        Err(_) => None,
    };
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
                if let Some((client, name, actor, _others)) = random {
                    let (_client, back) = client.rejoin(&[fallback.as_str()]).await?;
                    println!(
                        "after a random join the rejoin led to {} actor={} (same room and actor: {})",
                        back.name,
                        back.actor.0,
                        back.name == name && back.actor.0 == actor
                    );
                    if back.name != name || back.actor.0 != actor {
                        anyhow::bail!("the rejoin after a random join led to another room");
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

fn relay_room(name: String, max_players: u16, tag: &str) -> ClientMsg {
    ClientMsg::JoinOrCreate {
        name,
        mode: net_common::types::RoomMode::Relay,
        max_players: Some(max_players),
        visible: true,
        open: true,
        props: [("tag".to_owned(), tag.into())].into_iter().collect(),
        lobby_props: vec![],
        region: None,
        player_props: Default::default(),
        reserved: Vec::new(),
        id: None,
    }
}

/// A client a random join brought into a room that is full with it, and the clients that keep
/// that room and an open one beside it.
async fn random_join(
    url: &str,
    key: &str,
    room: &str,
) -> anyhow::Result<(Client, String, u16, Vec<Client>)> {
    let tag = format!("{room}-tag");
    let enter = |player: &'static str, join: ClientMsg| async move {
        let grant = issue_grant(url, key, player, player).await?;
        Client::connect_and_join(url, &grant, Options::default(), join).await
    };
    let (host, _) = enter("rust-rehost-h", relay_room(format!("{room}-full"), 2, &tag)).await?;
    let random = ClientMsg::JoinRandom {
        filter: [("tag".to_owned(), tag.as_str().into())]
            .into_iter()
            .collect(),
        player_props: Default::default(),
        expr: None,
        match_type: None,
        id: None,
    };
    let (client, joined) = enter("rust-rehost-r", random).await?;
    let (beside, _) = enter("rust-rehost-o", relay_room(format!("{room}-open"), 8, &tag)).await?;
    println!(
        "a random join led to {} actor={}",
        joined.name, joined.actor.0
    );
    Ok((client, joined.name, joined.actor.0, vec![host, beside]))
}
