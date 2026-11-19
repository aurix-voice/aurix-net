//! What an API key opens through the REST API of a running node.

mod common;

use common::{joined, Api, Node};
use serde_json::json;

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
