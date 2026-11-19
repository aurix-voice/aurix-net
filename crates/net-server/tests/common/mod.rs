//! A real node per test: the built `aurix-net-server` on ports the system picks against the development
//! Postgres and Redis (`deploy/docker-compose.dev.yml`, the services of the CI job), with a
//! project of its own so tests do not see each other's rooms. The node deletes the projects
//! it created when the test ends.
#![allow(dead_code)]

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

pub const ADMIN: &str = "test-admin-token";
const WAIT: Duration = Duration::from_secs(5);

pub struct Node {
    child: Child,
    pub http: String,
    log: std::path::PathBuf,
    projects: Mutex<Vec<String>>,
}

static STARTED: AtomicUsize = AtomicUsize::new(0);

impl Node {
    pub async fn start() -> Node {
        Self::start_with(&[]).await
    }

    /// `env` overrides the node's configuration (`NET__SECTION__KEY`).
    pub async fn start_with(env: &[(&str, &str)]) -> Node {
        let name = format!(
            "node-{}-{}.log",
            std::process::id(),
            STARTED.fetch_add(1, Ordering::Relaxed)
        );
        let log = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
        let out = std::fs::File::create(&log).expect("node log");
        let child = Command::new(env!("CARGO_BIN_EXE_aurix-net-server"))
            .env("NET__SERVER__HTTP_ADDR", "127.0.0.1:0")
            .env("NET__SERVER__LOG_FORMAT", "json")
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
            http: String::new(),
            log,
            projects: Mutex::default(),
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
            if let Some(address) = node.listening() {
                node.http = format!("http://{address}");
                let up = client.get(format!("{}/healthz", node.http)).send().await;
                if up.is_ok_and(|r| r.status().is_success()) {
                    return node;
                }
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

    /// What the node has logged so far, one JSON object per line.
    pub fn logged(&self) -> Vec<Value> {
        let text = std::fs::read_to_string(&self.log).unwrap_or_default();
        text.lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    /// The address the node says its HTTP listener got.
    fn listening(&self) -> Option<String> {
        self.logged()
            .into_iter()
            .find(|line| line["message"] == "control plane listening")
            .and_then(|line| line["addr"].as_str().map(String::from))
    }

    pub fn log_tail(&self) -> String {
        let text = std::fs::read_to_string(&self.log).unwrap_or_default();
        let lines: Vec<&str> = text.lines().collect();
        lines[lines.len().saturating_sub(40)..].join("\n")
    }

    /// The same project reached through this node.
    pub fn same_project(&self, project: &Project) -> Project {
        let at = |api: &Api| Api {
            http: self.http.clone(),
            token: api.token.clone(),
        };
        Project {
            id: project.id.clone(),
            api: at(&project.api),
            operator: at(&project.operator),
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
                json!({"name": format!("{name}-{}", uuid::Uuid::now_v7().simple())}),
            )
            .await;
        assert_eq!(status, 200, "create project: {body}");
        let id: String = body["project"]["id"].as_str().expect("project id").into();
        self.projects.lock().expect("projects").push(id.clone());
        Project {
            id,
            api: Api {
                http: self.http.clone(),
                token: body["key"]["key"].as_str().expect("api key").into(),
            },
            operator: api,
        }
    }
}

impl Node {
    /// Drop cannot await, so the calls run on a thread with a runtime of its own.
    fn delete_projects(&self) {
        let projects = std::mem::take(&mut *self.projects.lock().expect("projects"));
        let http = self.http.clone();
        let cleanup = std::thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            runtime.block_on(async {
                let client = reqwest::Client::new();
                for id in projects {
                    let _ = client
                        .delete(format!("{http}/v1/operator/projects/{id}"))
                        .bearer_auth(ADMIN)
                        .timeout(WAIT)
                        .send()
                        .await;
                }
            });
        });
        let _ = cleanup.join();
    }
}

impl Node {
    /// Tells the node to stop, as a deploy does, without waiting for it.
    pub fn stop(&self) {
        let _ = Command::new("kill")
            .args(["-TERM", &self.child.id().to_string()])
            .status();
    }

    /// Stops the node the way a deploy does and waits for it to exit. SIGTERM lets it remove
    /// its row from the fleet; it is killed if that takes too long.
    pub fn terminate(&mut self) {
        self.stop();
        for _ in 0..100 {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        self.delete_projects();
        self.terminate();
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
        if let Some(json) = body {
            req = req.json(&json);
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
        let mut client = Ws { ws };
        assert_eq!(client.next().await["t"], "hello");
        let authed = client.call(json!({"t": "auth", "grant": grant})).await;
        assert_eq!(authed["t"], "authed", "auth: {authed}");
        client
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

    /// Skips messages until one of type `kind`.
    pub async fn until(&mut self, kind: &str) -> Value {
        let end = tokio::time::Instant::now() + WAIT;
        loop {
            let left = end.saturating_duration_since(tokio::time::Instant::now());
            match self.poll(left).await {
                Some(msg) if msg["t"] == kind => return msg,
                Some(_) => {}
                None => panic!("no `{kind}` from the node"),
            }
        }
    }

    /// Everything that arrives until the socket is quiet for `quiet`.
    pub async fn drain(&mut self, quiet: Duration) -> Vec<Value> {
        let mut out = Vec::new();
        while let Some(msg) = self.poll(quiet).await {
            out.push(msg);
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

/// Receives the node's webhook deliveries on a local port.
pub struct Sink {
    pub url: String,
    rx: tokio::sync::mpsc::UnboundedReceiver<(axum::http::HeaderMap, Value)>,
}

impl Sink {
    pub async fn start() -> Sink {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let app = axum::Router::new().fallback(axum::routing::post(
            move |headers: axum::http::HeaderMap, axum::Json(body): axum::Json<Value>| {
                let tx = tx.clone();
                async move {
                    let _ = tx.send((headers, body));
                }
            },
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("webhook listener");
        let url = format!("http://{}/hook", listener.local_addr().expect("address"));
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Sink { url, rx }
    }

    /// The next delivery of `event`: the whole body (`event`, `project`, `at`, `data`).
    pub async fn next(&mut self, event: &str) -> Value {
        self.next_with_headers(event).await.1
    }

    pub async fn next_with_headers(&mut self, event: &str) -> (axum::http::HeaderMap, Value) {
        loop {
            let delivery = tokio::time::timeout(WAIT, self.rx.recv())
                .await
                .unwrap_or_else(|_| panic!("no `{event}` webhook from the node"))
                .expect("webhook sink closed");
            if delivery.1["event"] == event {
                return delivery;
            }
        }
    }

    /// True when nothing is delivered for `time`.
    pub async fn quiet(&mut self, time: Duration) -> bool {
        tokio::time::timeout(time, self.rx.recv()).await.is_err()
    }
}
