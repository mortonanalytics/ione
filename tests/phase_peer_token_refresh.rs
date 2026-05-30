//! Peer delegated-token refresh regression tests.
//!
//! Run:
//!   DATABASE_URL=postgres://ione:ione@localhost:5433/ione \
//!   IONE_TOKEN_KEY=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA= \
//!     cargo test --test phase_peer_token_refresh -- --ignored --test-threads=1

use std::net::SocketAddr;

use reqwest::StatusCode;
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;
use uuid::Uuid;
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione";
const TEST_STATIC_BEARER: &str = "phase-peer-refresh-static";
const TEST_TOKEN_KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

async fn spawn_app() -> (String, PgPool) {
    std::env::set_var("IONE_AUTH_MODE", "local");
    std::env::set_var("IONE_OAUTH_STATIC_BEARER", TEST_STATIC_BEARER);
    std::env::set_var("IONE_TOKEN_KEY", TEST_TOKEN_KEY);

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
        "TRUNCATE webhook_events_seen, peer_oauth_pending, workspace_peer_bindings,
                  broker_credentials, identity_audit_events, user_sessions,
                  oauth_auth_codes, oauth_access_tokens, oauth_refresh_tokens, oauth_clients,
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

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr: SocketAddr = listener.local_addr().expect("local addr");
    let app = ione::app(pool.clone()).await;
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("server error");
    });
    (format!("http://{}", addr), pool)
}

async fn default_org_id(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("SELECT id FROM organizations WHERE name = 'Default Org' LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("Default Org not found")
}

async fn default_workspace_id(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("SELECT id FROM workspaces WHERE name = 'Operations' LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("Operations workspace not found")
}

async fn insert_trust_issuer(pool: &PgPool, org_id: Uuid) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO trust_issuers (org_id, issuer_url, audience, jwks_uri, claim_mapping)
         VALUES ($1, 'https://refresh.issuer.test', 'aud', 'secret:test', '{}'::jsonb)
         RETURNING id",
    )
    .bind(org_id)
    .fetch_one(pool)
    .await
    .expect("insert trust issuer")
}

async fn insert_pending_peer(pool: &PgPool, mcp_url: &str) -> Uuid {
    let org_id = default_org_id(pool).await;
    let issuer_id = insert_trust_issuer(pool, org_id).await;
    sqlx::query_scalar(
        "INSERT INTO peers (
            name, mcp_url, issuer_id, sharing_policy, status, oauth_client_id, tool_allowlist
         )
         VALUES (
            'Pending Refresh Peer', $1, $2, '{}'::jsonb, 'pending_oauth'::peer_status,
            'client-1', '[]'::jsonb
         )
         RETURNING id",
    )
    .bind(mcp_url)
    .bind(issuer_id)
    .fetch_one(pool)
    .await
    .expect("insert pending peer")
}

async fn insert_peer_with_tokens(
    pool: &PgPool,
    workspace_id: Uuid,
    mcp_url: &str,
    access_token: &str,
    refresh_token: &str,
    expires_at_sql: &str,
) -> Uuid {
    let org_id = default_org_id(pool).await;
    let issuer_id = insert_trust_issuer(pool, org_id).await;
    let access_ciphertext =
        ione::util::token_crypto::encrypt_token(access_token).expect("encrypt access");
    let refresh_ciphertext =
        ione::util::token_crypto::encrypt_token(refresh_token).expect("encrypt refresh");
    let peer_id: Uuid = sqlx::query_scalar(&format!(
        "INSERT INTO peers (
            name, mcp_url, issuer_id, sharing_policy, status, oauth_client_id,
            access_token_hash, refresh_token_hash, access_token_ciphertext,
            refresh_token_ciphertext, token_expires_at, tool_allowlist
         )
         VALUES (
            'Refresh Peer', $1, $2, '{{}}'::jsonb, 'active'::peer_status, 'client-1',
            'old-access-hash', 'old-refresh-hash', $3, $4, {expires_at_sql}, '[]'::jsonb
         )
         RETURNING id"
    ))
    .bind(mcp_url)
    .bind(issuer_id)
    .bind(access_ciphertext)
    .bind(refresh_ciphertext)
    .fetch_one(pool)
    .await
    .expect("insert peer");

    sqlx::query(
        "INSERT INTO workspace_peer_bindings
           (workspace_id, peer_id, foreign_tenant_id, status)
         VALUES ($1, $2, 'tenant-1', 'active'::binding_status)",
    )
    .bind(workspace_id)
    .bind(peer_id)
    .execute(pool)
    .await
    .expect("insert binding");

    peer_id
}

fn map_resource() -> Value {
    json!({
        "uri": "stub://layer/refresh",
        "name": "Refreshed layer",
        "metadata": {
            "ione_view": "map",
            "tile_url": "https://tile.openstreetmap.org/{z}/{x}/{y}.png"
        }
    })
}

async fn mock_discovery_and_refresh(mock: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/.well-known/oauth-authorization-server"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "authorization_endpoint": format!("{}/authorize", mock.uri()),
            "token_endpoint": format!("{}/token", mock.uri()),
            "registration_endpoint": format!("{}/register", mock.uri())
        })))
        .mount(mock)
        .await;

    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("refresh_token=refresh-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "fresh-access",
            "refresh_token": "rotated-refresh",
            "expires_in": 3600
        })))
        .expect(1)
        .mount(mock)
        .await;
}

async fn get_layers(base: &str, workspace_id: Uuid) -> reqwest::Response {
    reqwest::Client::new()
        .get(format!(
            "{base}/api/v1/workspaces/{workspace_id}/map-layers"
        ))
        .bearer_auth(TEST_STATIC_BEARER)
        .send()
        .await
        .expect("map layers response")
}

#[tokio::test]
#[ignore]
async fn peer_oauth_callback_stores_refresh_token_ciphertext() {
    let (_base, pool) = spawn_app().await;
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=authorization_code"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "callback-access",
            "refresh_token": "callback-refresh",
            "expires_in": 600
        })))
        .expect(1)
        .mount(&mock)
        .await;

    let peer_id = insert_pending_peer(&pool, &mock.uri()).await;
    let state = ione::state::AppState::new(
        ione::config::Config::from_env(),
        pool.clone(),
        Uuid::nil(),
        Uuid::nil(),
    );
    let pending = ione::services::peer_oauth::PendingFederation {
        peer_id,
        peer_url: mock.uri(),
        discovery: ione::services::peer_oauth::PeerDiscovery {
            authorization_endpoint: format!("{}/authorize", mock.uri()),
            token_endpoint: format!("{}/token", mock.uri()),
            registration_endpoint: format!("{}/register", mock.uri()),
            client_id_metadata_document_supported: false,
        },
        code_verifier: "verifier".to_string(),
        code_challenge: "challenge".to_string(),
        client_id: "client-1".to_string(),
        redirect_uri: "http://localhost/api/v1/peers/callback".to_string(),
        nonce: "nonce".to_string(),
    };

    ione::services::peer_oauth::complete_callback(&state, &pending, "code-1")
        .await
        .expect("complete peer oauth callback");

    let (refresh_ciphertext, status): (Vec<u8>, String) =
        sqlx::query_as("SELECT refresh_token_ciphertext, status::text FROM peers WHERE id = $1")
            .bind(peer_id)
            .fetch_one(&pool)
            .await
            .expect("stored refresh token");
    let refresh_plaintext =
        ione::util::token_crypto::decrypt_token(&refresh_ciphertext).expect("decrypt refresh");
    assert_eq!(refresh_plaintext, "callback-refresh");
    assert_eq!(status, "pending_allowlist");
}

#[tokio::test]
#[ignore]
async fn expired_peer_token_refreshes_before_map_fanout() {
    let (base, pool) = spawn_app().await;
    let workspace_id = default_workspace_id(&pool).await;
    let mock = MockServer::start().await;
    mock_discovery_and_refresh(&mock).await;
    Mock::given(method("POST"))
        .and(path("/"))
        .and(header("authorization", "Bearer fresh-access"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": { "resources": [map_resource()] }
        })))
        .expect(1)
        .mount(&mock)
        .await;

    let peer_id = insert_peer_with_tokens(
        &pool,
        workspace_id,
        &mock.uri(),
        "stale-access",
        "refresh-token",
        "now() - interval '5 minutes'",
    )
    .await;

    let resp = get_layers(&base, workspace_id).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["items"].as_array().unwrap().len(), 1);
    assert_eq!(body["peersOk"][0], peer_id.to_string());

    let stored: Vec<u8> =
        sqlx::query_scalar("SELECT access_token_ciphertext FROM peers WHERE id = $1")
            .bind(peer_id)
            .fetch_one(&pool)
            .await
            .expect("stored access ciphertext");
    let decrypted = ione::util::token_crypto::decrypt_token(&stored).expect("decrypt stored token");
    assert_eq!(decrypted, "fresh-access");
}

#[tokio::test]
#[ignore]
async fn peer_token_refresh_retries_once_after_401() {
    let (base, pool) = spawn_app().await;
    let workspace_id = default_workspace_id(&pool).await;
    let mock = MockServer::start().await;
    mock_discovery_and_refresh(&mock).await;
    Mock::given(method("POST"))
        .and(path("/"))
        .and(header("authorization", "Bearer stale-access"))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/"))
        .and(header("authorization", "Bearer fresh-access"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": { "resources": [map_resource()] }
        })))
        .expect(1)
        .mount(&mock)
        .await;

    insert_peer_with_tokens(
        &pool,
        workspace_id,
        &mock.uri(),
        "stale-access",
        "refresh-token",
        "now() + interval '1 hour'",
    )
    .await;

    let resp = get_layers(&base, workspace_id).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["items"].as_array().unwrap().len(), 1);
}
