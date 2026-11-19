//! What an API key opens through the REST API of a running node.

mod common;

use common::{joined, Api, Node};
use serde_json::{json, Value};

#[tokio::test]
async fn node_list_is_not_given_without_a_key() {
    let node = Node::start().await;
    let project = node.project("keys-nodes").await;

    let keyless = reqwest::get(format!("{}/v1/nodes", node.http))
        .await
        .expect("http request");
    assert_eq!(keyless.status(), 401);

    let (status, key) = project
        .api
        .post("/v1/keys", json!({"name": "reader", "scopes": ["read"]}))
        .await;
    assert_eq!(status, 200, "{key}");
    let reader = Api {
        http: node.http.clone(),
        token: key["key"].as_str().expect("key").to_string(),
    };
    let (status, nodes) = reader.get("/v1/nodes").await;
    assert_eq!(status, 200, "{nodes}");
    let listed = nodes.as_array().expect("nodes");
    assert!(listed.iter().any(|n| n["http_url"] == node.http), "{nodes}");
}

#[tokio::test]
async fn key_says_who_it_is_only_from_an_address_it_allows() {
    let node = Node::start().await;
    let project = node.project("keys-me").await;
    let key = |body| async {
        let (status, key) = project.api.post("/v1/keys", body).await;
        assert_eq!(status, 200, "{key}");
        Api {
            http: node.http.clone(),
            token: key["key"].as_str().expect("key").to_string(),
        }
    };

    let elsewhere = json!({"name": "office", "scopes": ["read"], "allowed_cidrs": ["10.0.0.0/8"]});
    let (status, reply) = key(elsewhere).await.get("/v1/me").await;
    assert_eq!(status, 403, "{reply}");

    // Who a key is takes no scope of its own.
    let (status, reply) = key(json!({"name": "game", "scopes": ["rooms"]}))
        .await
        .get("/v1/me")
        .await;
    assert_eq!(status, 200, "{reply}");
    assert_eq!(reply["key"]["scopes"], json!(["rooms"]));
}

#[tokio::test]
async fn grant_asked_for_too_long_is_cut_and_for_no_time_is_refused() {
    let node = Node::start().await;
    let project = node.project("keys-grants").await;
    let grant = |ttl: u64| {
        project
            .api
            .post("/v1/grants", json!({"player_id": "ann", "ttl_secs": ttl}))
    };

    let (_, usual) = grant(60).await;
    let now = usual["expires_at"].as_i64().expect("expires_at") - 60;
    for ttl in [1 << 63, u64::MAX] {
        let (status, reply) = grant(ttl).await;
        assert_eq!(status, 200, "{reply}");
        let lasts = reply["expires_at"].as_i64().expect("expires_at") - now;
        assert!((60..=3605).contains(&lasts), "{ttl}: {lasts}");
    }

    let (status, reply) = grant(0).await;
    assert_eq!((status, &reply["error"]), (400, &json!("invalid_request")));
}

#[tokio::test]
async fn player_named_in_128_letters_of_any_alphabet_gets_a_grant_and_a_room() {
    let node = Node::start().await;
    let project = node.project("keys-player-id").await;
    let ticket = |player: &str| {
        json!({"player_id": player, "mode": "relay", "max_players": 2, "min_players": 2,
               "max_wait_secs": 30, "props": {}})
    };

    let long = "я".repeat(128);
    let (status, reply) = project
        .api
        .post("/v1/matchmaking/tickets", ticket(&long))
        .await;
    assert_eq!(status, 200, "{reply}");
    let mut player = project.player(&long).await;
    joined(&player.call(json!({"t": "create", "name": "arena"})).await);

    let longer = "я".repeat(129);
    for (path, body) in [
        ("/v1/matchmaking/tickets", ticket(&longer)),
        ("/v1/grants", json!({"player_id": longer})),
        ("/v1/bans", json!({"player_id": longer})),
    ] {
        let (status, reply) = project.api.post(path, body).await;
        assert_eq!(
            (status, &reply["error"]),
            (400, &json!("invalid_request")),
            "{path}"
        );
    }
}

#[tokio::test]
async fn query_that_does_not_parse_is_refused_the_way_the_api_refuses() {
    let node = Node::start().await;
    let project = node.project("keys-query").await;
    let room = "01a1f4bd-985c-7460-8131-33049a5692bc";
    for path in [
        format!("/v1/rooms/{room}/replay?from=abc"),
        format!("/v1/rooms/{room}/replay?to=-1"),
        "/v1/audit?limit=abc".to_string(),
        "/v1/usage?from=yesterday".to_string(),
        "/v1/rejections?hours=many".to_string(),
    ] {
        let (status, body) = project.api.get(&path).await;
        assert_eq!(status, 400, "{path}");
        assert_eq!(body["error"], "invalid_request", "{path}: {body}");
        assert!(body["message"].is_string(), "{path}: {body}");
    }
}

#[tokio::test]
async fn request_refused_before_its_handler_is_refused_the_way_the_api_refuses() {
    let node = Node::start().await;
    let project = node.project("keys-refused").await;
    let json = "application/json";
    let cases = [
        (
            "GET",
            "/v1/rooms/not-a-room",
            json,
            "",
            400,
            "invalid_request",
        ),
        ("POST", "/v1/grants", json, "{", 400, "invalid_request"),
        (
            "POST",
            "/v1/grants",
            json,
            r#"{"player_id": 5}"#,
            422,
            "invalid_request",
        ),
        (
            "POST",
            "/v1/grants",
            "text/plain",
            "{}",
            415,
            "invalid_request",
        ),
        ("GET", "/v1/nothing", json, "", 404, "not_found"),
        ("PUT", "/v1/rooms", json, "{}", 405, "invalid_request"),
    ];
    for (method, path, content_type, body, status, error) in cases {
        let case = format!("{method} {path} {body}");
        let reply = reqwest::Client::new()
            .request(
                method.parse().expect("method"),
                format!("{}{path}", project.api.http),
            )
            .bearer_auth(&project.api.token)
            .header("content-type", content_type)
            .body(body)
            .send()
            .await
            .expect("reply");
        assert_eq!(reply.status(), status, "{case}");
        if status == 405 {
            assert_eq!(reply.headers()["allow"], "GET,HEAD,POST", "{case}");
        }
        let refusal: Value = reply.json().await.expect(&case);
        assert_eq!(refusal["error"], error, "{case}: {refusal}");
        assert!(refusal["message"].is_string(), "{case}: {refusal}");
    }

    let stranger = Api {
        http: project.api.http.clone(),
        token: "not-a-key".into(),
    };
    let mut refusal = (401, Value::Null);
    for _ in 0..40 {
        refusal = stranger.get("/v1/me").await;
        if refusal.0 == 429 {
            break;
        }
    }
    assert_eq!(refusal.0, 429);
    assert_eq!(refusal.1["error"], "rate_limited", "{}", refusal.1);
}
