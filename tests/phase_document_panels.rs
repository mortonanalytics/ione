use std::net::SocketAddr;

use reqwest::StatusCode;
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione";
const TEST_STATIC_BEARER: &str = "phase-document-panel-test-bearer";

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

fn document_resource(uri: &str, name: &str, download_url: &str, mime_type: &str) -> Value {
    json!({
        "uri": uri,
        "name": name,
        "metadata": {
            "ione_view": "document",
            "download_url": download_url,
            "mime_type": mime_type,
            "file_size_bytes": 2048,
            "last_modified": "2026-05-29T12:00:00Z"
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

async fn get_document_panels(base: &str, workspace_id: Uuid) -> reqwest::Response {
    reqwest::Client::new()
        .get(format!(
            "{base}/api/v1/workspaces/{workspace_id}/document-panels"
        ))
        .bearer_auth(TEST_STATIC_BEARER)
        .send()
        .await
        .expect("document panels response")
}

#[tokio::test]
#[ignore]
async fn document_panels_lists_peer_documents_with_camel_case_fields_and_nosniff() {
    let (base, pool) = spawn_app().await;
    let workspace_id = default_workspace_id(&pool).await;
    let mock = MockServer::start().await;
    mock_resources_list(
        &mock,
        vec![document_resource(
            "stub://document/1",
            "Incident report",
            "https://docs.example.com/incident.pdf",
            "application/pdf",
        )],
    )
    .await;
    let peer_id = seed_active_peer(&pool, workspace_id, "document-peer", &mock.uri()).await;

    let resp = get_document_panels(&base, workspace_id).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get(reqwest::header::X_CONTENT_TYPE_OPTIONS)
            .and_then(|value| value.to_str().ok()),
        Some("nosniff")
    );
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["peerDocuments"].as_array().unwrap().len(), 1);
    assert_eq!(body["peerDocuments"][0]["peerId"], peer_id.to_string());
    assert_eq!(body["peerDocuments"][0]["peerName"], "document-peer");
    assert_eq!(body["peerDocuments"][0]["uri"], "stub://document/1");
    assert_eq!(
        body["peerDocuments"][0]["downloadUrl"],
        "https://docs.example.com/incident.pdf"
    );
    assert_eq!(body["peerDocuments"][0]["mimeType"], "application/pdf");
    assert_eq!(body["peerDocuments"][0]["source"], "peer");
    assert_eq!(body["peerDocuments"][0]["fileSizeBytes"], 2048);
    assert_eq!(
        body["peerDocuments"][0]["lastModified"],
        "2026-05-29T12:00:00Z"
    );
}

#[tokio::test]
#[ignore]
async fn document_panels_omits_unsafe_urls_and_allows_on_prem_https() {
    let (base, pool) = spawn_app().await;
    let workspace_id = default_workspace_id(&pool).await;
    let mock = MockServer::start().await;
    mock_resources_list(
        &mock,
        vec![
            document_resource(
                "stub://document/good-public",
                "Good public",
                "https://example.com/good.pdf",
                "application/pdf",
            ),
            document_resource(
                "stub://document/good-private",
                "Good private",
                "https://10.0.0.5/good.pdf",
                "application/pdf",
            ),
            document_resource(
                "stub://document/javascript",
                "Bad javascript",
                "javascript:alert(1)",
                "application/pdf",
            ),
            document_resource(
                "stub://document/file",
                "Bad file",
                "file:///tmp/report.pdf",
                "application/pdf",
            ),
            document_resource(
                "stub://document/http-public",
                "Bad public http",
                "http://example.com/report.pdf",
                "application/pdf",
            ),
            document_resource(
                "stub://document/http-loopback",
                "Bad loopback http",
                "http://127.0.0.1/report.pdf",
                "application/pdf",
            ),
            document_resource(
                "stub://document/http-localhost",
                "Bad localhost http",
                "http://localhost/report.pdf",
                "application/pdf",
            ),
            document_resource(
                "stub://document/http-private",
                "Bad private http",
                "http://10.0.0.5/report.pdf",
                "application/pdf",
            ),
            document_resource(
                "stub://document/link-local",
                "Bad link local",
                "https://169.254.169.254/report.pdf",
                "application/pdf",
            ),
        ],
    )
    .await;
    seed_active_peer(&pool, workspace_id, "document-peer", &mock.uri()).await;

    let resp = get_document_panels(&base, workspace_id).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("json");
    let uris: Vec<&str> = body["peerDocuments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["uri"].as_str().unwrap())
        .collect();
    assert_eq!(
        uris,
        vec![
            "stub://document/good-public",
            "stub://document/good-private"
        ]
    );
}

#[tokio::test]
#[ignore]
async fn document_panels_partial_peer_failure_keeps_reachable_documents() {
    let (base, pool) = spawn_app().await;
    let workspace_id = default_workspace_id(&pool).await;
    let ok = MockServer::start().await;
    mock_resources_list(
        &ok,
        vec![document_resource(
            "stub://document/1",
            "Incident report",
            "https://docs.example.com/incident.pdf",
            "application/pdf",
        )],
    )
    .await;
    let failing = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&failing)
        .await;
    seed_active_peer(&pool, workspace_id, "ok-peer", &ok.uri()).await;
    let fail_peer = seed_active_peer(&pool, workspace_id, "fail-peer", &failing.uri()).await;

    let resp = get_document_panels(&base, workspace_id).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["peerDocuments"].as_array().unwrap().len(), 1);
    assert_eq!(body["peerErrors"][0]["peerId"], fail_peer.to_string());
    assert!(!body["peerErrors"][0]["error"].as_str().unwrap().is_empty());
}

#[tokio::test]
#[ignore]
async fn document_panels_rejects_cross_org_workspace_without_leak() {
    let (base, pool) = spawn_app().await;
    let other_org = insert_org(&pool, "Other Org").await;
    let other_workspace = insert_workspace(&pool, other_org, "Other Workspace").await;

    let resp = get_document_panels(&base, other_workspace).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["error"], "not_found");
    assert_eq!(body["message"], "workspace not found");
}
