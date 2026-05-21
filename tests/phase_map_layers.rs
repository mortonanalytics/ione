use std::net::SocketAddr;

use reqwest::StatusCode;
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione";
const TEST_STATIC_BEARER: &str = "phase-map-test-bearer";

async fn spawn_app() -> (String, PgPool) {
    std::env::set_var("IONE_AUTH_MODE", "local");
    std::env::set_var("IONE_OAUTH_STATIC_BEARER", TEST_STATIC_BEARER);

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
        "TRUNCATE workspace_peer_bindings, audit_events, approvals, artifacts,
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

async fn insert_org(pool: &PgPool, name: &str) -> Uuid {
    sqlx::query_scalar("INSERT INTO organizations (name) VALUES ($1) RETURNING id")
        .bind(name)
        .fetch_one(pool)
        .await
        .expect("insert org")
}

async fn insert_workspace(pool: &PgPool, org_id: Uuid, name: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO workspaces (org_id, name, domain, lifecycle)
         VALUES ($1, $2, 'test', 'continuous'::workspace_lifecycle)
         RETURNING id",
    )
    .bind(org_id)
    .bind(name)
    .fetch_one(pool)
    .await
    .expect("insert workspace")
}

async fn insert_trust_issuer(pool: &PgPool, org_id: Uuid, issuer_url: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO trust_issuers (org_id, issuer_url, audience, jwks_uri, claim_mapping)
         VALUES ($1, $2, 'aud', 'secret:test', '{}'::jsonb)
         RETURNING id",
    )
    .bind(org_id)
    .bind(issuer_url)
    .fetch_one(pool)
    .await
    .expect("insert trust issuer")
}

async fn insert_peer(pool: &PgPool, name: &str, mcp_url: &str, issuer_id: Uuid) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO peers (name, mcp_url, issuer_id, sharing_policy, tool_allowlist, status)
         VALUES ($1, $2, $3, '{}'::jsonb, '[]'::jsonb, 'active'::peer_status)
         RETURNING id",
    )
    .bind(name)
    .bind(mcp_url)
    .bind(issuer_id)
    .fetch_one(pool)
    .await
    .expect("insert peer")
}

async fn insert_binding(pool: &PgPool, workspace_id: Uuid, peer_id: Uuid) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO workspace_peer_bindings
           (workspace_id, peer_id, foreign_tenant_id, status)
         VALUES ($1, $2, 'tenant-1', 'active'::binding_status)
         RETURNING id",
    )
    .bind(workspace_id)
    .bind(peer_id)
    .fetch_one(pool)
    .await
    .expect("insert binding")
}

fn map_resource(uri: &str, name: &str, tile_url: &str) -> Value {
    json!({
        "uri": uri,
        "name": name,
        "metadata": {
            "ione_view": "map",
            "tile_url": tile_url,
            "bounds": [-180, -85, 180, 85],
            "attribution": "OpenStreetMap"
        }
    })
}

async fn mock_resources(mock: &MockServer, resources: Vec<Value>) {
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": { "resources": resources }
        })))
        .mount(mock)
        .await;
}

async fn seed_active_peer(pool: &PgPool, workspace_id: Uuid, name: &str, url: &str) -> Uuid {
    let org_id = default_org_id(pool).await;
    let issuer_id = insert_trust_issuer(pool, org_id, &format!("https://{name}.issuer.test")).await;
    let peer_id = insert_peer(pool, name, url, issuer_id).await;
    insert_binding(pool, workspace_id, peer_id).await;
    peer_id
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
async fn map_layers_returns_one_map_resource() {
    let (base, pool) = spawn_app().await;
    let workspace_id = default_workspace_id(&pool).await;
    let mock = MockServer::start().await;
    mock_resources(
        &mock,
        vec![map_resource(
            "stub://layer/1",
            "World tiles",
            "https://tile.openstreetmap.org/{z}/{x}/{y}.png",
        )],
    )
    .await;
    let peer_id = seed_active_peer(&pool, workspace_id, "map-peer", &mock.uri()).await;

    let resp = get_layers(&base, workspace_id).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        body["items"][0]["meta"]["tileUrl"],
        "https://tile.openstreetmap.org/{z}/{x}/{y}.png"
    );
    assert_eq!(body["peersOk"][0], peer_id.to_string());
}

#[tokio::test]
#[ignore]
async fn map_layers_rejects_cross_org_workspace_without_leak() {
    let (base, pool) = spawn_app().await;
    let other_org = insert_org(&pool, "Other Org").await;
    let other_workspace = insert_workspace(&pool, other_org, "Other Workspace").await;

    let resp = get_layers(&base, other_workspace).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["error"], "not_found");
    assert_eq!(body["message"], "workspace not found");
}

#[tokio::test]
#[ignore]
async fn map_layers_returns_partial_success_when_one_peer_fails() {
    let (base, pool) = spawn_app().await;
    let workspace_id = default_workspace_id(&pool).await;
    let ok = MockServer::start().await;
    mock_resources(
        &ok,
        vec![map_resource(
            "stub://layer/1",
            "World tiles",
            "https://tile.openstreetmap.org/{z}/{x}/{y}.png",
        )],
    )
    .await;
    let failing = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&failing)
        .await;

    let ok_peer = seed_active_peer(&pool, workspace_id, "ok-peer", &ok.uri()).await;
    let fail_peer = seed_active_peer(&pool, workspace_id, "fail-peer", &failing.uri()).await;

    let resp = get_layers(&base, workspace_id).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["items"].as_array().unwrap().len(), 1);
    assert_eq!(body["peersOk"][0], ok_peer.to_string());
    assert_eq!(body["peersFailed"][0]["peerId"], fail_peer.to_string());
    assert!(!body["peersFailed"][0]["error"].as_str().unwrap().is_empty());
}

#[tokio::test]
#[ignore]
async fn map_layers_excludes_non_map_resources() {
    let (base, pool) = spawn_app().await;
    let workspace_id = default_workspace_id(&pool).await;
    let mock = MockServer::start().await;
    mock_resources(
        &mock,
        vec![
            map_resource(
                "stub://layer/1",
                "World tiles",
                "https://tile.openstreetmap.org/{z}/{x}/{y}.png",
            ),
            json!({
                "uri": "stub://chart/1",
                "name": "Chart",
                "metadata": { "ione_view": "chart" }
            }),
        ],
    )
    .await;
    seed_active_peer(&pool, workspace_id, "map-peer", &mock.uri()).await;

    let resp = get_layers(&base, workspace_id).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["items"].as_array().unwrap().len(), 1);
    assert_eq!(body["items"][0]["uri"], "stub://layer/1");
}

#[tokio::test]
#[ignore]
async fn map_layers_returns_empty_arrays_without_bindings() {
    let (base, pool) = spawn_app().await;
    let workspace_id = default_workspace_id(&pool).await;

    let resp = get_layers(&base, workspace_id).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["items"].as_array().unwrap().len(), 0);
    assert_eq!(body["peersOk"].as_array().unwrap().len(), 0);
    assert_eq!(body["peersFailed"].as_array().unwrap().len(), 0);
}

#[tokio::test]
#[ignore]
async fn map_layers_peer_id_filter_limits_fanout() {
    let (base, pool) = spawn_app().await;
    let workspace_id = default_workspace_id(&pool).await;
    let peer_x_mock = MockServer::start().await;
    let peer_y_mock = MockServer::start().await;
    mock_resources(
        &peer_x_mock,
        vec![map_resource(
            "stub://layer/x",
            "X",
            "https://x.test/{z}/{x}/{y}.png",
        )],
    )
    .await;
    mock_resources(
        &peer_y_mock,
        vec![map_resource(
            "stub://layer/y",
            "Y",
            "https://y.test/{z}/{x}/{y}.png",
        )],
    )
    .await;
    let peer_x = seed_active_peer(&pool, workspace_id, "peer-x", &peer_x_mock.uri()).await;
    let peer_y = seed_active_peer(&pool, workspace_id, "peer-y", &peer_y_mock.uri()).await;

    let resp = reqwest::Client::new()
        .get(format!(
            "{base}/api/v1/workspaces/{workspace_id}/map-layers?peer_id={peer_x}"
        ))
        .bearer_auth(TEST_STATIC_BEARER)
        .send()
        .await
        .expect("map layers response");
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["items"].as_array().unwrap().len(), 1);
    assert_eq!(body["items"][0]["peerId"], peer_x.to_string());
    assert_eq!(body["peersOk"][0], peer_x.to_string());
    assert!(!body["peersOk"].as_array().unwrap().contains(&json!(peer_y)));
}

#[tokio::test]
#[ignore]
async fn unauthorized_peer_token_lands_in_peers_failed() {
    let (base, pool) = spawn_app().await;
    let workspace_id = default_workspace_id(&pool).await;
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&mock)
        .await;
    let peer_id = seed_active_peer(&pool, workspace_id, "unauthorized-peer", &mock.uri()).await;

    let resp = get_layers(&base, workspace_id).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["items"].as_array().unwrap().len(), 0);
    assert_eq!(body["peersOk"].as_array().unwrap().len(), 0);
    assert_eq!(body["peersFailed"][0]["peerId"], peer_id.to_string());
    assert!(!body["peersFailed"][0]["error"].as_str().unwrap().is_empty());
}
