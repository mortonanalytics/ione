//! Real peer manifest fetch regression test.
//!
//! Run:
//!   DATABASE_URL=postgres://ione:ione@localhost:5433/ione \
//!   IONE_TOKEN_KEY=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA= \
//!     cargo test --test integration_peer_manifest -- --ignored --test-threads=1

use std::net::SocketAddr;

use reqwest::StatusCode;
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;
use uuid::Uuid;
use wiremock::{
    matchers::{header, method, path},
    Mock, MockServer, ResponseTemplate,
};

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione";
const TEST_TOKEN_KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

async fn spawn_app() -> (String, PgPool) {
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

async fn default_org_id(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("SELECT id FROM organizations WHERE name = 'Default Org' LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("default org")
}

async fn insert_peer(pool: &PgPool, mcp_url: &str, access_token: &str) -> Uuid {
    let org_id = default_org_id(pool).await;
    let issuer_id: Uuid = sqlx::query_scalar(
        "INSERT INTO trust_issuers (org_id, issuer_url, audience, jwks_uri, claim_mapping)
         VALUES ($1, 'https://manifest-issuer.example.com', 'mcp', 'local', '{}'::jsonb)
         RETURNING id",
    )
    .bind(org_id)
    .fetch_one(pool)
    .await
    .expect("trust issuer");

    let ciphertext = ione::util::token_crypto::encrypt_token(access_token).expect("encrypt token");
    sqlx::query_scalar(
        "INSERT INTO peers (
            name, mcp_url, issuer_id, sharing_policy, status,
            access_token_hash, refresh_token_hash, access_token_ciphertext,
            token_expires_at, tool_allowlist
         )
         VALUES (
            'Manifest Peer', $1, $2, '{}'::jsonb, 'pending_allowlist'::peer_status,
            'access-hash', '', $3, now() + interval '1 hour', '[]'::jsonb
         )
         RETURNING id",
    )
    .bind(mcp_url)
    .bind(issuer_id)
    .bind(ciphertext)
    .fetch_one(pool)
    .await
    .expect("peer")
}

#[tokio::test]
#[ignore]
async fn peer_manifest_returns_real_tool_list() {
    let (base, pool) = spawn_app().await;
    let mock = MockServer::start().await;
    let access_token = "peer-access-token";

    Mock::given(method("POST"))
        .and(path("/mcp"))
        .and(header("authorization", "Bearer peer-access-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "tools": [
                    { "name": "list_survivors", "inputSchema": { "type": "object" } },
                    { "name": "propose_artifact", "inputSchema": { "type": "object" } }
                ]
            }
        })))
        .mount(&mock)
        .await;

    let peer_id = insert_peer(&pool, &mock.uri(), access_token).await;
    let resp = reqwest::Client::new()
        .get(format!("{base}/api/v1/peers/{peer_id}/manifest"))
        .send()
        .await
        .expect("manifest");
    assert_eq!(resp.status(), StatusCode::OK);

    let body: Value = resp.json().await.expect("manifest JSON");
    let tools = body["tools"].as_array().expect("tools array");
    let names: Vec<&str> = tools
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert_eq!(names, vec!["list_survivors", "propose_artifact"]);
}
