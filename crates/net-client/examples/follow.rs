//! Joins a room by name through any node of the region; `connect_and_join` follows the
//! `room_elsewhere:<url>` redirect to the node hosting it.
//! Usage: follow <http_url> <api_key> <room>
use net_client::{Client, Options};
use net_rooms::protocol::ClientMsg;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let a: Vec<String> = std::env::args().collect();
    let (http, key, room) = (&a[1], &a[2], &a[3]);
    let grant: serde_json::Value = reqwest::Client::new()
        .post(format!("{http}/v1/grants"))
        .bearer_auth(key)
        .json(&serde_json::json!({"player_id": "follow-1", "name": "follow"}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let msg: ClientMsg = serde_json::from_value(serde_json::json!({
        "t": "join_or_create", "name": room, "mode": "relay", "max_players": 8, "props": {}
    }))?;
    let (c, j) = Client::connect_and_join(
        http,
        grant["grant"].as_str().unwrap(),
        Options::default(),
        msg,
    )
    .await?;
    println!(
        "joined room={} actor={:?} via data host={}:{}",
        j.name, j.actor, j.data.host, j.data.port
    );
    drop(c);
    Ok(())
}
