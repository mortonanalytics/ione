//! OAuth 2.1 security regression tests.
//!
//! Run:
//!   DATABASE_URL=postgres://ione:ione@localhost:5433/ione \
//!     cargo test --test integration_oauth_roundtrip -- --ignored --test-threads=1

use std::net::SocketAddr;

use base64::{engine::general_purpose, Engine};
use reqwest::{redirect::Policy, StatusCode};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione";
const REDIRECT_URI: &str = "http://127.0.0.1/callback";

async fn spawn_app() -> (String, PgPool) {
    std::env::remove_var("IONE_SSRF_DEV");
    let db_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_owned());

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("failed to connect to Postgres");

    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("migration failed");

    sqlx::query(
        "TRUNCATE oauth_auth_codes, oauth_access_tokens, oauth_refresh_tokens, oauth_clients,
                  audit_events, approvals, artifacts,
                  trust_issuers, peers, routing_decisions, survivors, signals,
                  stream_events, streams, connectors,
                  memberships, roles, messages, conversations,
                  workspaces, users, organizations
         RESTART IDENTITY CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate failed");

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind");
    let addr: SocketAddr = listener.local_addr().expect("local addr");
    let app = ione::app(pool.clone()).await;
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("server error");
    });

    (format!("http://{}", addr), pool)
}

async fn register_client(base: &str) -> String {
    let resp = reqwest::Client::new()
        .post(format!("{base}/mcp/oauth/register"))
        .json(&json!({
            "client_name": "OAuth Roundtrip Test",
            "redirect_uris": [REDIRECT_URI],
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "scope": "mcp",
            "token_endpoint_auth_method": "none"
        }))
        .send()
        .await
        .expect("register request");
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("register JSON");
    body["clientId"].as_str().expect("clientId").to_string()
}

fn code_challenge(verifier: &str) -> String {
    general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

async fn authorize_code(base: &str, client_id: &str, verifier: &str) -> String {
    let challenge = code_challenge(verifier);
    let auth_resp = reqwest::Client::new()
        .get(format!("{base}/mcp/oauth/authorize"))
        .query(&[
            ("response_type", "code"),
            ("client_id", client_id),
            ("redirect_uri", REDIRECT_URI),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
            ("scope", "mcp"),
            ("state", "abc123"),
        ])
        .send()
        .await
        .expect("authorize GET");
    assert_eq!(auth_resp.status(), StatusCode::OK);

    let consent = reqwest::Client::builder()
        .redirect(Policy::none())
        .build()
        .expect("client");
    let resp = consent
        .post(format!("{base}/mcp/oauth/authorize"))
        .form(&[
            ("client_id", client_id),
            ("redirect_uri", REDIRECT_URI),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
            ("scope", "mcp"),
            ("state", "abc123"),
            ("action", "allow"),
        ])
        .send()
        .await
        .expect("authorize POST");
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);

    let location = resp
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|h| h.to_str().ok())
        .expect("redirect location");
    let parsed = url::Url::parse(location).expect("redirect URL");
    assert_eq!(
        parsed
            .query_pairs()
            .find(|(key, _)| key == "state")
            .map(|(_, value)| value.to_string()),
        Some("abc123".to_string())
    );
    parsed
        .query_pairs()
        .find(|(key, _)| key == "code")
        .map(|(_, value)| value.to_string())
        .expect("authorization code")
}

async fn exchange_code(base: &str, client_id: &str, code: &str, verifier: &str) -> Value {
    let resp = reqwest::Client::new()
        .post(format!("{base}/mcp/oauth/token"))
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("code_verifier", verifier),
            ("client_id", client_id),
            ("redirect_uri", REDIRECT_URI),
        ])
        .send()
        .await
        .expect("token exchange");
    assert_eq!(resp.status(), StatusCode::OK);
    resp.json().await.expect("token JSON")
}

async fn refresh_tokens(base: &str, client_id: &str, refresh_token: &str) -> Value {
    let resp = reqwest::Client::new()
        .post(format!("{base}/mcp/oauth/token"))
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client_id),
        ])
        .send()
        .await
        .expect("refresh exchange");
    assert_eq!(resp.status(), StatusCode::OK);
    resp.json().await.expect("refresh JSON")
}

async fn mcp_tools_list_status(base: &str, access_token: &str) -> StatusCode {
    reqwest::Client::new()
        .post(format!("{base}/mcp"))
        .bearer_auth(access_token)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list"
        }))
        .send()
        .await
        .expect("tools/list")
        .status()
}

#[tokio::test]
#[ignore]
async fn oauth_roundtrip_refresh_revokes_old_access_token() {
    let (base, _pool) = spawn_app().await;

    let client_id = register_client(&base).await;
    let verifier = "roundtrip-verifier-with-enough-entropy";
    let code = authorize_code(&base, &client_id, verifier).await;
    let tokens = exchange_code(&base, &client_id, &code, verifier).await;
    let access = tokens["accessToken"].as_str().expect("accessToken");
    let refresh = tokens["refreshToken"].as_str().expect("refreshToken");

    assert_eq!(mcp_tools_list_status(&base, access).await, StatusCode::OK);

    let refreshed = refresh_tokens(&base, &client_id, refresh).await;
    let new_access = refreshed["accessToken"].as_str().expect("new accessToken");

    assert_eq!(
        mcp_tools_list_status(&base, access).await,
        StatusCode::UNAUTHORIZED,
        "refresh rotation must revoke the old access token"
    );
    assert_eq!(
        mcp_tools_list_status(&base, new_access).await,
        StatusCode::OK
    );
}

#[tokio::test]
#[ignore]
async fn oauth_rejects_unregistered_redirect_uri() {
    let (base, _pool) = spawn_app().await;
    let client_id = register_client(&base).await;

    let resp = reqwest::Client::new()
        .get(format!("{base}/mcp/oauth/authorize"))
        .query(&[
            ("response_type", "code"),
            ("client_id", &client_id),
            ("redirect_uri", "https://evil.example.com/callback"),
            ("code_challenge", &code_challenge("verifier")),
            ("code_challenge_method", "S256"),
        ])
        .send()
        .await
        .expect("authorize GET");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body: Value = resp.json().await.expect("error JSON");
    assert_eq!(body["error"].as_str(), Some("bad_request"));
    assert!(body["message"]
        .as_str()
        .unwrap_or_default()
        .contains("redirect_uri"));
}

#[tokio::test]
#[ignore]
async fn register_rejects_loopback_cimd_url_without_echoing_host() {
    let (base, _pool) = spawn_app().await;

    let resp = reqwest::Client::new()
        .post(format!("{base}/mcp/oauth/register"))
        .json(&json!({ "clientMetadataUrl": "http://127.0.0.1:65535/client.json" }))
        .send()
        .await
        .expect("register CIMD");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body: Value = resp.json().await.expect("error JSON");
    assert_eq!(body["error"].as_str(), Some("bad_request"));
    assert!(
        !body["message"]
            .as_str()
            .unwrap_or_default()
            .contains("127.0.0.1"),
        "SSRF rejection must not echo private host details: {body}"
    );
}

#[tokio::test]
#[ignore]
async fn revoke_wrong_client_id_does_not_revoke_token() {
    let (base, _pool) = spawn_app().await;

    let client_id = register_client(&base).await;
    let verifier = "revoke-verifier-with-enough-entropy";
    let code = authorize_code(&base, &client_id, verifier).await;
    let tokens = exchange_code(&base, &client_id, &code, verifier).await;
    let access = tokens["accessToken"].as_str().expect("accessToken");

    let resp = reqwest::Client::new()
        .post(format!("{base}/mcp/oauth/revoke"))
        .form(&[
            ("token", access),
            ("client_id", "wrong-client"),
            ("token_type_hint", "access_token"),
        ])
        .send()
        .await
        .expect("revoke");
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(mcp_tools_list_status(&base, access).await, StatusCode::OK);
}
