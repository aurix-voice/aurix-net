//! `aurix-net`: command line over the REST API. Developer calls use an API key
//! (`--api-key` / `NET_API_KEY`), operator calls use `--operator-token` / `NET_OPERATOR_TOKEN`.

use clap::{Parser, Subcommand};
use serde_json::{json, Value};

#[derive(Parser)]
#[command(name = "aurix-net", version, about = "Aurix Net command line")]
struct Cli {
    #[arg(long, env = "NET_HTTP", default_value = "http://127.0.0.1:8090")]
    http: String,
    #[arg(long, env = "NET_API_KEY")]
    api_key: Option<String>,
    #[arg(long, env = "NET_OPERATOR_TOKEN")]
    operator_token: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Node health
    Health,
    /// Regions with live nodes
    Regions,
    /// Operator: create a project (prints the first API key once)
    ProjectCreate {
        name: String,
        #[arg(long, default_value = "indie")]
        plan: String,
    },
    /// Operator: list projects
    Projects,
    /// Operator: nodes
    Nodes,
    /// Create an API key (admin scope on the current key)
    KeyCreate {
        #[arg(long, default_value = "cli")]
        name: String,
        #[arg(long, value_delimiter = ',', default_value = "read,rooms,grants")]
        scopes: Vec<String>,
    },
    Keys,
    /// Issue a join grant for a player
    Grant {
        player_id: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        room: Option<String>,
    },
    /// Open rooms
    Rooms,
    Room {
        id: String,
    },
    RoomClose {
        id: String,
    },
    /// Download a lockstep replay (JSON) of a closed room
    Replay {
        id: String,
        #[arg(short, long)]
        out: Option<String>,
    },
    /// File a matchmaking ticket and wait for a match
    Match {
        player_id: String,
        #[arg(long, default_value = "relay")]
        mode: String,
        #[arg(long, default_value_t = 2)]
        max_players: u16,
        #[arg(long, default_value_t = 2)]
        min_players: u16,
        #[arg(long, default_value_t = 15)]
        max_wait_secs: u64,
    },
    /// Usage (node-hours, events, bytes)
    Usage,
    Webhooks,
    WebhookCreate {
        url: String,
        #[arg(long, value_delimiter = ',')]
        events: Vec<String>,
    },
    /// List the project's server-side game modules (WASM logic for sync rooms).
    Modules,
    /// Upload or replace a game module (.wasm binary or .wat text); rooms use it via props.logic.
    ModulePut {
        name: String,
        file: std::path::PathBuf,
    },
    ModuleDelete {
        name: String,
    },
}

struct Api {
    http: reqwest::Client,
    base: String,
    bearer: Option<String>,
}

impl Api {
    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> anyhow::Result<Value> {
        let mut r = self.http.request(method, format!("{}{}", self.base, path));
        if let Some(b) = &self.bearer {
            r = r.bearer_auth(b);
        }
        if let Some(b) = body {
            r = r.json(&b);
        }
        let resp = r.send().await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            anyhow::bail!("{status}: {text}");
        }
        if text.is_empty() {
            return Ok(json!({"ok": true}));
        }
        Ok(serde_json::from_str(&text).unwrap_or(Value::String(text)))
    }

    async fn call_raw(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Vec<u8>,
    ) -> anyhow::Result<Value> {
        let mut r = self
            .http
            .request(method, format!("{}{}", self.base, path))
            .header("content-type", "application/octet-stream")
            .body(body);
        if let Some(b) = &self.bearer {
            r = r.bearer_auth(b);
        }
        let resp = r.send().await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            anyhow::bail!("{status}: {text}");
        }
        Ok(serde_json::from_str(&text).unwrap_or(Value::String(text)))
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    const GET: reqwest::Method = reqwest::Method::GET;
    const POST: reqwest::Method = reqwest::Method::POST;
    const DELETE: reqwest::Method = reqwest::Method::DELETE;
    let dev = Api {
        http: reqwest::Client::new(),
        base: cli.http.clone(),
        bearer: cli.api_key.clone(),
    };
    let op = Api {
        http: reqwest::Client::new(),
        base: cli.http.clone(),
        bearer: cli.operator_token.clone(),
    };
    let need = |v: &Option<String>, what: &str| -> anyhow::Result<()> {
        if v.is_none() {
            anyhow::bail!("{what} required");
        }
        Ok(())
    };
    let out = match cli.cmd {
        Cmd::Health => dev.call(GET, "/healthz", None).await?,
        Cmd::Regions => dev.call(GET, "/v1/regions", None).await?,
        Cmd::ProjectCreate { name, plan } => {
            need(&cli.operator_token, "--operator-token")?;
            op.call(
                POST,
                "/v1/operator/projects",
                Some(json!({"name": name, "plan": plan})),
            )
            .await?
        }
        Cmd::Projects => {
            need(&cli.operator_token, "--operator-token")?;
            op.call(GET, "/v1/operator/projects", None).await?
        }
        Cmd::Nodes => {
            need(&cli.operator_token, "--operator-token")?;
            op.call(GET, "/v1/operator/nodes", None).await?
        }
        Cmd::KeyCreate { name, scopes } => {
            need(&cli.api_key, "--api-key")?;
            dev.call(
                POST,
                "/v1/keys",
                Some(json!({"name": name, "scopes": scopes})),
            )
            .await?
        }
        Cmd::Keys => {
            need(&cli.api_key, "--api-key")?;
            dev.call(GET, "/v1/keys", None).await?
        }
        Cmd::Grant {
            player_id,
            name,
            room,
        } => {
            need(&cli.api_key, "--api-key")?;
            dev.call(POST, "/v1/grants", Some(json!({"player_id": player_id, "name": name.unwrap_or_else(|| player_id_name(&player_id)), "room": room}))).await?
        }
        Cmd::Rooms => {
            need(&cli.api_key, "--api-key")?;
            dev.call(GET, "/v1/rooms", None).await?
        }
        Cmd::Room { id } => {
            need(&cli.api_key, "--api-key")?;
            dev.call(GET, &format!("/v1/rooms/{id}"), None).await?
        }
        Cmd::RoomClose { id } => {
            need(&cli.api_key, "--api-key")?;
            dev.call(DELETE, &format!("/v1/rooms/{id}"), None).await?
        }
        Cmd::Replay { id, out } => {
            need(&cli.api_key, "--api-key")?;
            let v = dev
                .call(GET, &format!("/v1/rooms/{id}/replay"), None)
                .await?;
            if let Some(path) = out {
                std::fs::write(&path, serde_json::to_vec(&v)?)?;
                json!({"written": path, "ticks": v["inputs"].as_array().map(|a| a.len()).unwrap_or(0)})
            } else {
                v
            }
        }
        Cmd::Match {
            player_id,
            mode,
            max_players,
            min_players,
            max_wait_secs,
        } => {
            need(&cli.api_key, "--api-key")?;
            let t = dev
                .call(POST, "/v1/matchmaking/tickets", Some(json!({"player_id": player_id, "mode": mode, "max_players": max_players, "min_players": min_players, "max_wait_secs": max_wait_secs})))
                .await?;
            let id = t["ticket"].as_str().unwrap_or_default().to_string();
            loop {
                let s = dev
                    .call(GET, &format!("/v1/matchmaking/tickets/{id}"), None)
                    .await?;
                if s["state"]["state"] != "searching" {
                    break s;
                }
                eprintln!("searching... waiting={}", s["state"]["waiting"]);
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        }
        Cmd::Usage => {
            need(&cli.api_key, "--api-key")?;
            dev.call(GET, "/v1/usage", None).await?
        }
        Cmd::Webhooks => {
            need(&cli.api_key, "--api-key")?;
            dev.call(GET, "/v1/webhooks", None).await?
        }
        Cmd::WebhookCreate { url, events } => {
            need(&cli.api_key, "--api-key")?;
            dev.call(
                POST,
                "/v1/webhooks",
                Some(json!({"url": url, "events": events})),
            )
            .await?
        }
        Cmd::Modules => {
            need(&cli.api_key, "--api-key")?;
            dev.call(GET, "/v1/modules", None).await?
        }
        Cmd::ModulePut { name, file } => {
            need(&cli.api_key, "--api-key")?;
            let bytes = std::fs::read(&file)?;
            dev.call_raw(reqwest::Method::PUT, &format!("/v1/modules/{name}"), bytes)
                .await?
        }
        Cmd::ModuleDelete { name } => {
            need(&cli.api_key, "--api-key")?;
            dev.call(
                reqwest::Method::DELETE,
                &format!("/v1/modules/{name}"),
                None,
            )
            .await?
        }
    };
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

fn player_id_name(p: &str) -> String {
    p.chars().take(32).collect()
}
