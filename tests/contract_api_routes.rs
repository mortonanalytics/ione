/// Contract API route tests — assert that every route defined in the contract
/// is registered in the router (i.e., returns anything other than 404 or 405).
///
/// These tests do NOT assert success codes or response shape — that belongs to
/// implementation tests. The sole contract is: the path is registered.
///
/// All tests are expected to FAIL until the routes are implemented.
///
/// Prerequisites:
///   docker compose up -d postgres
///   DATABASE_URL=postgres://ione:ione@localhost:5433/ione
///
/// Run:
///   DATABASE_URL=postgres://ione:ione@localhost:5433/ione \
///     cargo test --test contract_api_routes -- --ignored --test-threads=1

use sqlx::{postgres::PgPoolOptions, PgPool};
use std::net::SocketAddr;
use tokio::net::TcpListener;
use uuid::Uuid;

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione";

// ─── Harness ──────────────────────────────────────────────────────────────────

async fn spawn_app() -> (String, PgPool) {
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

    truncate_all(&pool).await;

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind");
    let addr: SocketAddr = listener.local_addr().expect("get addr");
    let app = ione::app(pool.clone()).await;
    tokio::spawn(async move { axum::serve(listener, app).await.expect("server error") });

    (format!("http://{}", addr), pool)
}

async fn truncate_all(pool: &PgPool) {
    sqlx::query(
        "TRUNCATE audit_events, approvals, artifacts,
                  trust_issuers, peers, routing_decisions, survivors, signals,
                  stream_events, streams, connectors,
                  memberships, roles, messages, conversations,
                  workspaces, users, organizations
         RESTART IDENTITY CASCADE",
    )
    .execute(pool)
    .await
    .expect("truncate failed");
}

/// A response status is "route registered" if it is NOT 404 and NOT 405.
fn route_registered(status: u16) -> bool {
    status != 404 && status != 405
}

// ─── Health ───────────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn route_get_health_ollama_registered() {
    let (base, _pool) = spawn_app().await;
    let resp = reqwest::Client::new()
        .get(format!("{}/api/v1/health/ollama", base))
        .send()
        .await
        .expect("request failed");
    assert!(
        route_registered(resp.status().as_u16()),
        "GET /api/v1/health/ollama returned {} (404 or 405 means route not registered)",
        resp.status()
    );
}

// ─── Activation ───────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn route_get_activation_registered() {
    let (base, _pool) = spawn_app().await;
    let resp = reqwest::Client::new()
        .get(format!("{}/api/v1/activation", base))
        .send()
        .await
        .expect("request failed");
    assert!(
        route_registered(resp.status().as_u16()),
        "GET /api/v1/activation returned {} — route not registered",
        resp.status()
    );
}

#[tokio::test]
#[ignore]
async fn route_post_activation_events_registered() {
    let (base, _pool) = spawn_app().await;
    let resp = reqwest::Client::new()
        .post(format!("{}/api/v1/activation/events", base))
        .json(&serde_json::json!({}))
        .send()
        .await
        .expect("request failed");
    assert!(
        route_registered(resp.status().as_u16()),
        "POST /api/v1/activation/events returned {} — route not registered",
        resp.status()
    );
}

#[tokio::test]
#[ignore]
async fn route_post_activation_dismiss_registered() {
    let (base, _pool) = spawn_app().await;
    let resp = reqwest::Client::new()
        .post(format!("{}/api/v1/activation/dismiss", base))
        .json(&serde_json::json!({}))
        .send()
        .await
        .expect("request failed");
    assert!(
        route_registered(resp.status().as_u16()),
        "POST /api/v1/activation/dismiss returned {} — route not registered",
        resp.status()
    );
}

// ─── Connectors validate ──────────────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn route_post_connectors_validate_registered() {
    let (base, _pool) = spawn_app().await;
    let resp = reqwest::Client::new()
        .post(format!("{}/api/v1/connectors/validate", base))
        .json(&serde_json::json!({"kind": "rust_native", "name": "nws", "config": {}}))
        .send()
        .await
        .expect("request failed");
    assert!(
        route_registered(resp.status().as_u16()),
        "POST /api/v1/connectors/validate returned {} — route not registered",
        resp.status()
    );
}

// ─── Workspace events ─────────────────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn route_get_workspace_events_registered() {
    let (base, _pool) = spawn_app().await;
    let ws_id = Uuid::new_v4();
    let resp = reqwest::Client::new()
        .get(format!("{}/api/v1/workspaces/{}/events", base, ws_id))
        .send()
        .await
        .expect("request failed");
    assert!(
        route_registered(resp.status().as_u16()),
        "GET /api/v1/workspaces/:id/events returned {} — route not registered",
        resp.status()
    );
}

#[tokio::test]
#[ignore]
async fn route_get_workspace_events_stream_registered() {
    let (base, _pool) = spawn_app().await;
    let ws_id = Uuid::new_v4();
    let resp = reqwest::Client::new()
        .get(format!(
            "{}/api/v1/workspaces/{}/events/stream",
            base, ws_id
        ))
        .send()
        .await
        .expect("request failed");
    assert!(
        route_registered(resp.status().as_u16()),
        "GET /api/v1/workspaces/:id/events/stream returned {} — route not registered",
        resp.status()
    );
}

// ─── OAuth / MCP authorization server ────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn route_get_oauth_authorization_server_metadata_registered() {
    let (base, _pool) = spawn_app().await;
    let resp = reqwest::Client::new()
        .get(format!("{}/.well-known/oauth-authorization-server", base))
        .send()
        .await
        .expect("request failed");
    assert!(
        route_registered(resp.status().as_u16()),
        "GET /.well-known/oauth-authorization-server returned {} — route not registered",
        resp.status()
    );
}

#[tokio::test]
#[ignore]
async fn route_post_mcp_oauth_register_registered() {
    let (base, _pool) = spawn_app().await;
    let resp = reqwest::Client::new()
        .post(format!("{}/mcp/oauth/register", base))
        .json(&serde_json::json!({}))
        .send()
        .await
        .expect("request failed");
    assert!(
        route_registered(resp.status().as_u16()),
        "POST /mcp/oauth/register returned {} — route not registered",
        resp.status()
    );
}

#[tokio::test]
#[ignore]
async fn route_get_mcp_oauth_authorize_registered() {
    let (base, _pool) = spawn_app().await;
    let resp = reqwest::Client::new()
        .get(format!("{}/mcp/oauth/authorize", base))
        .send()
        .await
        .expect("request failed");
    assert!(
        route_registered(resp.status().as_u16()),
        "GET /mcp/oauth/authorize returned {} — route not registered",
        resp.status()
    );
}

#[tokio::test]
#[ignore]
async fn route_post_mcp_oauth_token_registered() {
    let (base, _pool) = spawn_app().await;
    let resp = reqwest::Client::new()
        .post(format!("{}/mcp/oauth/token", base))
        .json(&serde_json::json!({}))
        .send()
        .await
        .expect("request failed");
    assert!(
        route_registered(resp.status().as_u16()),
        "POST /mcp/oauth/token returned {} — route not registered",
        resp.status()
    );
}

#[tokio::test]
#[ignore]
async fn route_post_mcp_oauth_revoke_registered() {
    let (base, _pool) = spawn_app().await;
    let resp = reqwest::Client::new()
        .post(format!("{}/mcp/oauth/revoke", base))
        .json(&serde_json::json!({}))
        .send()
        .await
        .expect("request failed");
    assert!(
        route_registered(resp.status().as_u16()),
        "POST /mcp/oauth/revoke returned {} — route not registered",
        resp.status()
    );
}

// ─── MCP client management ────────────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn route_get_mcp_clients_registered() {
    let (base, _pool) = spawn_app().await;
    let resp = reqwest::Client::new()
        .get(format!("{}/api/v1/mcp/clients", base))
        .send()
        .await
        .expect("request failed");
    assert!(
        route_registered(resp.status().as_u16()),
        "GET /api/v1/mcp/clients returned {} — route not registered",
        resp.status()
    );
}

#[tokio::test]
#[ignore]
async fn route_delete_mcp_clients_id_registered() {
    let (base, _pool) = spawn_app().await;
    let client_id = Uuid::new_v4();
    let resp = reqwest::Client::new()
        .delete(format!("{}/api/v1/mcp/clients/{}", base, client_id))
        .send()
        .await
        .expect("request failed");
    assert!(
        route_registered(resp.status().as_u16()),
        "DELETE /api/v1/mcp/clients/:id returned {} — route not registered",
        resp.status()
    );
}

// ─── Peer manifest and authorize ──────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn route_get_peers_manifest_registered() {
    let (base, _pool) = spawn_app().await;
    let peer_id = Uuid::new_v4();
    let resp = reqwest::Client::new()
        .get(format!("{}/api/v1/peers/{}/manifest", base, peer_id))
        .send()
        .await
        .expect("request failed");
    assert!(
        route_registered(resp.status().as_u16()),
        "GET /api/v1/peers/:id/manifest returned {} — route not registered",
        resp.status()
    );
}

#[tokio::test]
#[ignore]
async fn route_post_peers_authorize_registered() {
    let (base, _pool) = spawn_app().await;
    let peer_id = Uuid::new_v4();
    let resp = reqwest::Client::new()
        .post(format!("{}/api/v1/peers/{}/authorize", base, peer_id))
        .json(&serde_json::json!({}))
        .send()
        .await
        .expect("request failed");
    assert!(
        route_registered(resp.status().as_u16()),
        "POST /api/v1/peers/:id/authorize returned {} — route not registered",
        resp.status()
    );
}

// ─── Telemetry and admin funnel ───────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn route_post_telemetry_events_registered() {
    let (base, _pool) = spawn_app().await;
    let resp = reqwest::Client::new()
        .post(format!("{}/api/v1/telemetry/events", base))
        .json(&serde_json::json!({}))
        .send()
        .await
        .expect("request failed");
    assert!(
        route_registered(resp.status().as_u16()),
        "POST /api/v1/telemetry/events returned {} — route not registered",
        resp.status()
    );
}

#[tokio::test]
#[ignore]
async fn route_get_admin_funnel_registered() {
    let (base, _pool) = spawn_app().await;
    let resp = reqwest::Client::new()
        .get(format!("{}/api/v1/admin/funnel", base))
        .send()
        .await
        .expect("request failed");
    assert!(
        route_registered(resp.status().as_u16()),
        "GET /api/v1/admin/funnel returned {} — route not registered",
        resp.status()
    );
}
