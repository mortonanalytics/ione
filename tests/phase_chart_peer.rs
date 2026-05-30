use std::net::SocketAddr;

use reqwest::StatusCode;
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione";
const TEST_STATIC_BEARER: &str = "phase-chart-peer-test-bearer";

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
        "TRUNCATE webhook_events_seen, workspace_peer_bindings, audit_events, approvals, artifacts,
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

async fn insert_stream(pool: &PgPool, workspace_id: Uuid) -> Uuid {
    let connector_id: Uuid = sqlx::query_scalar(
        "INSERT INTO connectors (workspace_id, kind, name, config, status)
         VALUES ($1, 'rust_native'::connector_kind, 'chart-test', '{}'::jsonb, 'active'::connector_status)
         RETURNING id",
    )
    .bind(workspace_id)
    .fetch_one(pool)
    .await
    .expect("insert connector");

    sqlx::query_scalar(
        "INSERT INTO streams (connector_id, name, schema, view_config)
         VALUES ($1, 'Earthquakes', '{}'::jsonb, $2)
         RETURNING id",
    )
    .bind(connector_id)
    .bind(json!({
        "lon_pointer": "/geometry/coordinates/0",
        "lat_pointer": "/geometry/coordinates/1",
        "property_fields": [{ "pointer": "/properties/mag", "name": "mag" }]
    }))
    .fetch_one(pool)
    .await
    .expect("insert stream")
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

async fn insert_binding(pool: &PgPool, workspace_id: Uuid, peer_id: Uuid) {
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
}

async fn seed_active_peer(pool: &PgPool, workspace_id: Uuid, name: &str, url: &str) -> Uuid {
    let org_id = default_org_id(pool).await;
    let issuer_id = insert_trust_issuer(pool, org_id, &format!("https://{name}.issuer.test")).await;
    let peer_id = insert_peer(pool, name, url, issuer_id).await;
    insert_binding(pool, workspace_id, peer_id).await;
    peer_id
}

fn chart_resource(uri: &str) -> Value {
    json!({
        "uri": uri,
        "name": "Peer magnitude",
        "metadata": {
            "ione_view": "chart",
            "spec": {
                "chartType": "line",
                "xAxis": "bucket_start",
                "yAxis": "Magnitude",
                "series": ["value"]
            }
        }
    })
}

async fn mock_resources_list(mock: &MockServer, resources: Vec<Value>) {
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

async fn mock_resources_read(mock: &MockServer, body: Value) {
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "contents": [{
                    "uri": "stub://chart/1",
                    "mimeType": "application/vnd.ione.chart+json",
                    "text": body.to_string()
                }]
            }
        })))
        .mount(mock)
        .await;
}

async fn get_chart_panels(base: &str, workspace_id: Uuid) -> reqwest::Response {
    reqwest::Client::new()
        .get(format!(
            "{base}/api/v1/workspaces/{workspace_id}/chart-panels"
        ))
        .bearer_auth(TEST_STATIC_BEARER)
        .send()
        .await
        .expect("chart panels response")
}

#[tokio::test]
#[ignore]
async fn chart_panels_lists_ione_and_peer_sources_with_peer_id() {
    let (base, pool) = spawn_app().await;
    let workspace_id = default_workspace_id(&pool).await;
    insert_stream(&pool, workspace_id).await;
    let mock = MockServer::start().await;
    mock_resources_list(&mock, vec![chart_resource("stub://chart/1")]).await;
    let peer_id = seed_active_peer(&pool, workspace_id, "chart-peer", &mock.uri()).await;

    let resp = get_chart_panels(&base, workspace_id).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("json");
    assert!(!body["ioneCharts"].as_array().unwrap().is_empty());
    assert_eq!(body["peerCharts"].as_array().unwrap().len(), 1);
    assert_eq!(body["peerCharts"][0]["peerId"], peer_id.to_string());
    assert_eq!(body["peerCharts"][0]["uri"], "stub://chart/1");
}

#[tokio::test]
#[ignore]
async fn chart_data_reads_peer_resource_contents_text_and_requires_peer_id() {
    let (base, pool) = spawn_app().await;
    let workspace_id = default_workspace_id(&pool).await;
    let mock = MockServer::start().await;
    mock_resources_read(
        &mock,
        json!({
            "spec": {
                "chartType": "line",
                "xAxis": "bucket_start",
                "yAxis": "Magnitude",
                "series": ["value"]
            },
            "rows": [{ "bucketStart": "2026-05-28T00:00:00Z", "bucketStartMs": 1780099200000i64, "value": 5 }]
        }),
    )
    .await;
    let peer_id = seed_active_peer(&pool, workspace_id, "chart-peer", &mock.uri()).await;

    let resp = reqwest::Client::new()
        .get(format!(
            "{base}/api/v1/workspaces/{workspace_id}/chart-data?peer_id={peer_id}&uri=stub%3A%2F%2Fchart%2F1"
        ))
        .bearer_auth(TEST_STATIC_BEARER)
        .send()
        .await
        .expect("chart-data response");
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["rows"][0]["value"], 5);

    let resp = reqwest::Client::new()
        .get(format!(
            "{base}/api/v1/workspaces/{workspace_id}/chart-data?uri=stub%3A%2F%2Fchart%2F1"
        ))
        .bearer_auth(TEST_STATIC_BEARER)
        .send()
        .await
        .expect("chart-data response");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
#[ignore]
async fn chart_panels_partial_peer_failure_keeps_reachable_charts() {
    let (base, pool) = spawn_app().await;
    let workspace_id = default_workspace_id(&pool).await;
    let ok = MockServer::start().await;
    mock_resources_list(&ok, vec![chart_resource("stub://chart/1")]).await;
    let failing = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&failing)
        .await;
    seed_active_peer(&pool, workspace_id, "ok-peer", &ok.uri()).await;
    let fail_peer = seed_active_peer(&pool, workspace_id, "fail-peer", &failing.uri()).await;

    let resp = get_chart_panels(&base, workspace_id).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["peerCharts"].as_array().unwrap().len(), 1);
    assert_eq!(body["peerErrors"][0]["peerId"], fail_peer.to_string());
    assert!(!body["peerErrors"][0]["error"].as_str().unwrap().is_empty());
}
