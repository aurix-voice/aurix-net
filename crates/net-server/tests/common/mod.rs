//! A real node per test: the built `aurix-net-server` on free ports against the development
//! Postgres and Redis (`deploy/docker-compose.dev.yml`, the services of the CI job), with a
//! project of its own so tests do not see each other's rooms.
#![allow(dead_code)]

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

pub const ADMIN: &str = "test-admin-token";
const WAIT: Duration = Duration::from_secs(5);

pub struct Node {
    child: Child,
    pub http: String,
    log: std::path::PathBuf,
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("free port")
        .port()
}

impl Node {
    pub async fn start() -> Node {
        Self::start_with(&[]).await
    }

    /// `env` overrides the node's configuration (`NET__SECTION__KEY`).
    pub async fn start_with(env: &[(&str, &str)]) -> Node {
        let port = free_port();
        let log =
            std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("node-{port}.log"));
        let out = std::fs::File::create(&log).expect("node log");
        let child = Command::new(env!("CARGO_BIN_EXE_aurix-net-server"))
            .env("NET__SERVER__HTTP_ADDR", format!("127.0.0.1:{port}"))
            .env("NET__TRANSPORT__QUIC_ADDR", "127.0.0.1:0")
            .env("NET__TRANSPORT__WEBTRANSPORT_ADDR", "127.0.0.1:0")
            .env("NET__AUTH__ADMIN_TOKEN", ADMIN)
            .envs(env.iter().copied())
            .stdout(out.try_clone().expect("node log"))
            .stderr(out)
            .stdin(Stdio::null())
            .spawn()
            .expect("start aurix-net-server");
        let mut node = Node {
            child,
            http: format!("http://127.0.0.1:{port}"),
            log,
        };
        let client = reqwest::Client::new();
        for _ in 0..300 {
            if let Ok(Some(status)) = node.child.try_wait() {
                panic!(
                    "the node exited at start ({status}); it needs Postgres and Redis: \
                     docker compose -f deploy/docker-compose.dev.yml up -d\n{}",
                    node.log_tail()
                );
            }
            let up = client.get(format!("{}/healthz", node.http)).send().await;
            if up.is_ok_and(|r| r.status().is_success()) {
                return node;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("the node did not come up in 30 s\n{}", node.log_tail());
    }

    /// Freezes the node process (SIGSTOP) for `time`: what a stalled host or a long pause of
    /// the database connection looks like to the rest of the fleet.
    pub async fn freeze(&self, time: Duration) {
        let pid = self.child.id().to_string();
        let _ = Command::new("kill").args(["-STOP", &pid]).status();
        tokio::time::sleep(time).await;
        let _ = Command::new("kill").args(["-CONT", &pid]).status();
    }

    pub fn log_tail(&self) -> String {
        let text = std::fs::read_to_string(&self.log).unwrap_or_default();
        let lines: Vec<&str> = text.lines().collect();
        lines[lines.len().saturating_sub(40)..].join("\n")
    }

    /// The same project reached through this node.
    pub fn same_project(&self, p: &Project) -> Project {
        let at = |api: &Api| Api {
            http: self.http.clone(),
            token: api.token.clone(),
        };
        Project {
            id: p.id.clone(),
            api: at(&p.api),
            operator: at(&p.operator),
        }
    }

    /// A new project with its API key.
    pub async fn project(&self, name: &str) -> Project {
        let api = Api {
            http: self.http.clone(),
            token: ADMIN.into(),
        };
        let (status, body) = api
            .post(
                "/v1/operator/projects",
                json!({"name": format!("{name}-{}", free_port())}),
            )
            .await;
        assert_eq!(status, 200, "create project: {body}");
        Project {
            id: body["project"]["id"].as_str().expect("project id").into(),
            api: Api {
                http: self.http.clone(),
                token: body["key"]["key"].as_str().expect("api key").into(),
            },
            operator: api,
        }
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        // SIGTERM lets the node remove its row from the fleet; kill it if that takes too long.
        let _ = Command::new("kill")
            .args(["-TERM", &self.child.id().to_string()])
            .status();
        for _ in 0..50 {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// REST calls with one bearer token; answers come back as (status, JSON or null).
#[derive(Clone)]
pub struct Api {
    pub http: String,
    pub token: String,
}

impl Api {
    async fn call(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> (u16, Value) {
        let mut req = reqwest::Client::new()
            .request(method, format!("{}{path}", self.http))
            .bearer_auth(&self.token);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.send().await.expect("http request");
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        let value = serde_json::from_str(&text).unwrap_or(Value::String(text));
        (status, value)
    }
    pub async fn get(&self, path: &str) -> (u16, Value) {
        self.call(reqwest::Method::GET, path, None).await
    }
    pub async fn post(&self, path: &str, body: Value) -> (u16, Value) {
        self.call(reqwest::Method::POST, path, Some(body)).await
    }
    pub async fn delete(&self, path: &str) -> (u16, Value) {
        self.call(reqwest::Method::DELETE, path, None).await
    }
}

pub struct Project {
    pub id: String,
    /// The project's own API key.
    pub api: Api,
    /// The operator token of the node.
    pub operator: Api,
}

impl Project {
    pub async fn grant(&self, player: &str) -> String {
        self.grant_with(json!({"player_id": player, "name": player}))
            .await
    }

    pub async fn grant_with(&self, body: Value) -> String {
        let (status, body) = self.api.post("/v1/grants", body).await;
        assert_eq!(status, 200, "grant: {body}");
        body["grant"].as_str().expect("grant").into()
    }

    /// An authenticated control session of `player`.
    pub async fn player(&self, player: &str) -> Ws {
        Ws::open(&self.api.http, &self.grant(player).await).await
    }

    /// Names of the rooms the project's REST listing shows.
    pub async fn room_names(&self) -> Vec<String> {
        let (status, body) = self.api.get("/v1/rooms").await;
        assert_eq!(status, 200, "list rooms: {body}");
        body["rooms"]
            .as_array()
            .expect("rooms")
            .iter()
            .map(|r| r["name"].as_str().unwrap_or_default().to_string())
            .collect()
    }
}

/// One control WebSocket, messages as JSON values.
pub struct Ws {
    ws: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
}

impl Ws {
    pub async fn open(http: &str, grant: &str) -> Ws {
        let url = http.replacen("http", "ws", 1) + "/ws";
        let (ws, _) = tokio_tungstenite::connect_async(&url)
            .await
            .expect("ws connect");
        let mut c = Ws { ws };
        assert_eq!(c.next().await["t"], "hello");
        let authed = c.call(json!({"t": "auth", "grant": grant})).await;
        assert_eq!(authed["t"], "authed", "auth: {authed}");
        c
    }

    pub async fn send(&mut self, msg: Value) {
        self.ws
            .send(Message::text(msg.to_string()))
            .await
            .expect("ws send");
    }

    /// The next control message within `wait`, None when nothing came (or the socket closed).
    pub async fn poll(&mut self, wait: Duration) -> Option<Value> {
        loop {
            match tokio::time::timeout(wait, self.ws.next()).await {
                Ok(Some(Ok(Message::Text(t)))) => {
                    return Some(serde_json::from_str(t.as_str()).expect("json from the node"))
                }
                Ok(Some(Ok(Message::Close(_)))) | Ok(Some(Err(_))) | Ok(None) | Err(_) => {
                    return None
                }
                Ok(Some(Ok(_))) => {}
            }
        }
    }

    pub async fn next(&mut self) -> Value {
        self.poll(WAIT).await.expect("a message from the node")
    }

    /// Sends `msg` and returns the first message that follows.
    pub async fn call(&mut self, msg: Value) -> Value {
        self.send(msg).await;
        self.next().await
    }

    /// Skips messages until one of type `t`.
    pub async fn until(&mut self, t: &str) -> Value {
        let end = tokio::time::Instant::now() + WAIT;
        loop {
            let left = end.saturating_duration_since(tokio::time::Instant::now());
            match self.poll(left).await {
                Some(m) if m["t"] == t => return m,
                Some(_) => {}
                None => panic!("no `{t}` from the node"),
            }
        }
    }

    /// Everything that arrives until the socket is quiet for `quiet`.
    pub async fn drain(&mut self, quiet: Duration) -> Vec<Value> {
        let mut out = Vec::new();
        while let Some(m) = self.poll(quiet).await {
            out.push(m);
        }
        out
    }
}

/// The reply to a join or create that must succeed.
pub fn joined(reply: &Value) -> u64 {
    assert_eq!(reply["t"], "joined", "expected joined, got {reply}");
    reply["actor"].as_u64().expect("actor")
}

/// The reply must be an error with this code.
pub fn refused(reply: &Value, code: &str) {
    assert_eq!(reply["t"], "error", "expected an error, got {reply}");
    assert_eq!(reply["code"], code, "{reply}");
}
