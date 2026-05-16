use std::net::SocketAddr;

use reqwest::StatusCode;
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione";
const TEST_STATIC_BEARER: &str = "phase14-test-bearer";

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
        "INSERT INTO peers (name, mcp_url, issuer_id, sharing_policy, tool_allowlist)
         VALUES ($1, $2, $3, '{}'::jsonb, '[\"propose_artifact\"]'::jsonb)
         RETURNING id",
    )
    .bind(name)
    .bind(mcp_url)
    .bind(issuer_id)
    .fetch_one(pool)
    .await
    .expect("insert peer")
}

async fn binding_status_and_tenant(
    pool: &PgPool,
    workspace_id: Uuid,
    peer_id: Uuid,
) -> (String, String) {
    sqlx::query_as(
        "SELECT status::TEXT, foreign_tenant_id
         FROM workspace_peer_bindings
         WHERE workspace_id = $1 AND peer_id = $2",
    )
    .bind(workspace_id)
    .bind(peer_id)
    .fetch_one(pool)
    .await
    .expect("binding row")
}

#[tokio::test]
#[ignore]
async fn subscribe_creates_pending_binding_when_whoami_unavailable() {
    let (base, pool) = spawn_app().await;
    let org_id = default_org_id(&pool).await;
    let workspace_id = default_workspace_id(&pool).await;
    let issuer_id = insert_trust_issuer(&pool, org_id, "https://iss-pending.test").await;
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/mcp"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": { "code": -32601, "message": "method not found" }
        })))
        .mount(&mock)
        .await;
    let peer_id = insert_peer(
        &pool,
        "Pending Peer",
        &format!("{}/mcp", mock.uri()),
        issuer_id,
    )
    .await;

    let resp = reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/workspaces/{workspace_id}/peers/{peer_id}/subscribe"
        ))
        .json(&json!({}))
        .send()
        .await
        .expect("subscribe");

    assert_eq!(resp.status(), StatusCode::OK);
    let (status, tenant) = binding_status_and_tenant(&pool, workspace_id, peer_id).await;
    assert_eq!(status, "pending");
    assert_eq!(tenant, "");
}

#[tokio::test]
#[ignore]
async fn subscribe_creates_active_binding_when_whoami_returns_tenant() {
    let (base, pool) = spawn_app().await;
    let org_id = default_org_id(&pool).await;
    let workspace_id = default_workspace_id(&pool).await;
    let issuer_id = insert_trust_issuer(&pool, org_id, "https://iss-active.test").await;
    let mock = MockServer::start().await;
    let whoami = json!({
        "foreign_tenant_id": "t-acme",
        "foreign_tenant_name": "Acme",
        "foreign_workspace_id": "fws-acme",
        "foreign_user_id": "u-acme",
        "foreign_user_email": "user@example.test",
        "foreign_roles": ["operator"]
    });
    Mock::given(method("POST"))
        .and(path("/mcp"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "contents": [{
                    "uri": "whoami://",
                    "mimeType": "application/vnd.ione.whoami+json",
                    "text": whoami.to_string()
                }]
            }
        })))
        .mount(&mock)
        .await;
    let peer_id = insert_peer(
        &pool,
        "Active Peer",
        &format!("{}/mcp", mock.uri()),
        issuer_id,
    )
    .await;

    let resp = reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/workspaces/{workspace_id}/peers/{peer_id}/subscribe"
        ))
        .json(&json!({}))
        .send()
        .await
        .expect("subscribe");

    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["connector"]["kind"], "mcp");
    assert_eq!(body["binding"]["status"], "active");
    let (status, tenant) = binding_status_and_tenant(&pool, workspace_id, peer_id).await;
    assert_eq!(status, "active");
    assert_eq!(tenant, "t-acme");
}

#[tokio::test]
#[ignore]
async fn resubscribe_with_whoami_failure_preserves_active_binding() {
    let (base, pool) = spawn_app().await;
    let org_id = default_org_id(&pool).await;
    let workspace_id = default_workspace_id(&pool).await;
    let issuer_id = insert_trust_issuer(&pool, org_id, "https://iss-preserve.test").await;
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/mcp"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": { "code": -32601, "message": "method not found" }
        })))
        .mount(&mock)
        .await;
    let peer_id = insert_peer(
        &pool,
        "Preserve Peer",
        &format!("{}/mcp", mock.uri()),
        issuer_id,
    )
    .await;
    sqlx::query(
        "INSERT INTO workspace_peer_bindings
           (workspace_id, peer_id, foreign_tenant_id, foreign_workspace_id, status)
         VALUES ($1, $2, 't-existing', 'fws-existing', 'active'::binding_status)",
    )
    .bind(workspace_id)
    .bind(peer_id)
    .execute(&pool)
    .await
    .expect("insert active binding");

    let resp = reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/workspaces/{workspace_id}/peers/{peer_id}/subscribe"
        ))
        .json(&json!({}))
        .send()
        .await
        .expect("subscribe");

    assert_eq!(resp.status(), StatusCode::OK);
    let (status, tenant) = binding_status_and_tenant(&pool, workspace_id, peer_id).await;
    assert_eq!(status, "active");
    assert_eq!(tenant, "t-existing");
}

#[tokio::test]
#[ignore]
async fn cross_org_subscribe_returns_404_without_connector_or_binding() {
    let (base, pool) = spawn_app().await;
    let workspace_id = default_workspace_id(&pool).await;
    let other_org = insert_org(&pool, "Subscribe Other Org").await;
    let issuer_id = insert_trust_issuer(&pool, other_org, "https://iss-sub-cross.test").await;
    let peer_id = insert_peer(&pool, "Cross Org Peer", "http://127.0.0.1:9/mcp", issuer_id).await;

    let resp = reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/workspaces/{workspace_id}/peers/{peer_id}/subscribe"
        ))
        .json(&json!({}))
        .send()
        .await
        .expect("subscribe");

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let connector_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM connectors WHERE workspace_id = $1 AND config->>'peer_id' = $2",
    )
    .bind(workspace_id)
    .bind(peer_id.to_string())
    .fetch_one(&pool)
    .await
    .expect("connector count");
    let binding_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM workspace_peer_bindings WHERE workspace_id = $1 AND peer_id = $2",
    )
    .bind(workspace_id)
    .bind(peer_id)
    .fetch_one(&pool)
    .await
    .expect("binding count");
    assert_eq!(connector_count, 0);
    assert_eq!(binding_count, 0);
}

#[tokio::test]
#[ignore]
async fn cross_org_binding_insert_raises_exception() {
    let (_, pool) = spawn_app().await;
    let org_a = default_org_id(&pool).await;
    let ws_a = default_workspace_id(&pool).await;
    let org_b = insert_org(&pool, "Other Org").await;
    let issuer_b = insert_trust_issuer(&pool, org_b, "https://iss-cross.test").await;
    let peer_b = insert_peer(&pool, "Other Peer", "http://127.0.0.1:9/mcp", issuer_b).await;
    assert_ne!(org_a, org_b);

    let err = sqlx::query(
        "INSERT INTO workspace_peer_bindings (workspace_id, peer_id, foreign_tenant_id)
         VALUES ($1, $2, 't-cross')",
    )
    .bind(ws_a)
    .bind(peer_b)
    .execute(&pool)
    .await
    .expect_err("cross-org insert must fail");

    assert!(err
        .to_string()
        .contains("cross-org bindings are not allowed"));
}

#[tokio::test]
#[ignore]
async fn binding_cascade_on_workspace_delete() {
    let (_, pool) = spawn_app().await;
    let org_id = default_org_id(&pool).await;
    let workspace_id = insert_workspace(&pool, org_id, "Cascade WS").await;
    let issuer_id = insert_trust_issuer(&pool, org_id, "https://iss-cascade.test").await;
    let peer_id = insert_peer(&pool, "Cascade Peer", "http://127.0.0.1:9/mcp", issuer_id).await;
    sqlx::query(
        "INSERT INTO workspace_peer_bindings (workspace_id, peer_id, foreign_tenant_id, status)
         VALUES ($1, $2, 't-cascade', 'active'::binding_status)",
    )
    .bind(workspace_id)
    .bind(peer_id)
    .execute(&pool)
    .await
    .expect("insert binding");

    sqlx::query("DELETE FROM workspaces WHERE id = $1")
        .bind(workspace_id)
        .execute(&pool)
        .await
        .expect("delete workspace");
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM workspace_peer_bindings WHERE workspace_id = $1")
            .bind(workspace_id)
            .fetch_one(&pool)
            .await
            .expect("count");
    assert_eq!(count, 0);
}

#[tokio::test]
#[ignore]
async fn patch_validation_and_pending_to_active_work() {
    let (base, pool) = spawn_app().await;
    let org_id = default_org_id(&pool).await;
    let workspace_id = default_workspace_id(&pool).await;
    let issuer_id = insert_trust_issuer(&pool, org_id, "https://iss-patch.test").await;
    let peer_id = insert_peer(&pool, "Patch Peer", "http://127.0.0.1:9/mcp", issuer_id).await;
    let binding_id: Uuid = sqlx::query_scalar(
        "INSERT INTO workspace_peer_bindings (workspace_id, peer_id)
         VALUES ($1, $2)
         RETURNING id",
    )
    .bind(workspace_id)
    .bind(peer_id)
    .fetch_one(&pool)
    .await
    .expect("insert pending binding");

    let client = reqwest::Client::new();
    let empty_resp = client
        .patch(format!(
            "{base}/api/v1/workspaces/{workspace_id}/bindings/{binding_id}"
        ))
        .json(&json!({ "foreignTenantId": "   " }))
        .send()
        .await
        .expect("patch empty");
    assert_eq!(empty_resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let scope_resp = client
        .patch(format!(
            "{base}/api/v1/workspaces/{workspace_id}/bindings/{binding_id}"
        ))
        .json(&json!({ "scope": [] }))
        .send()
        .await
        .expect("patch scope");
    assert_eq!(scope_resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let workspace_type_resp = client
        .patch(format!(
            "{base}/api/v1/workspaces/{workspace_id}/bindings/{binding_id}"
        ))
        .json(&json!({ "foreignWorkspaceId": 42 }))
        .send()
        .await
        .expect("patch workspace type");
    assert_eq!(
        workspace_type_resp.status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );

    let ok_resp = client
        .patch(format!(
            "{base}/api/v1/workspaces/{workspace_id}/bindings/{binding_id}"
        ))
        .json(&json!({ "foreignTenantId": "t-manual", "foreignWorkspaceId": "fws-manual" }))
        .send()
        .await
        .expect("patch ok");
    assert_eq!(ok_resp.status(), StatusCode::OK);
    let body: Value = ok_resp.json().await.expect("json");
    assert_eq!(body["status"], "active");
    assert_eq!(body["foreignTenantId"], "t-manual");
}

#[tokio::test]
#[ignore]
async fn ione_whoami_resource_returns_caller_identity() {
    let (base, pool) = spawn_app().await;
    let org_id = default_org_id(&pool).await;

    let resp = reqwest::Client::new()
        .post(format!("{base}/mcp"))
        .bearer_auth(TEST_STATIC_BEARER)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "resources/read",
            "params": { "uri": "whoami://" }
        }))
        .send()
        .await
        .expect("mcp");
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("json");
    let text = body["result"]["contents"][0]["text"]
        .as_str()
        .expect("text");
    let whoami: Value = serde_json::from_str(text).expect("whoami json");
    assert_eq!(whoami["foreign_tenant_id"], org_id.to_string());
}
