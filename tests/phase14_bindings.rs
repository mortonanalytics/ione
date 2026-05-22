use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

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

async fn spawn_second_app() -> (String, PgPool) {
    std::env::set_var("IONE_AUTH_MODE", "local");
    std::env::set_var("IONE_OAUTH_STATIC_BEARER", TEST_STATIC_BEARER);

    let db_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_owned());
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("failed to connect second Postgres pool");
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("second migration failed");

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

async fn insert_hmac_trust_issuer(
    pool: &PgPool,
    org_id: Uuid,
    issuer_url: &str,
    audience: &str,
) -> (Uuid, Vec<u8>) {
    use base64::Engine as _;
    let secret: Vec<u8> = (0u8..32).collect();
    let jwks_uri = format!(
        "secret:{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&secret)
    );
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO trust_issuers (org_id, issuer_url, audience, jwks_uri, claim_mapping)
         VALUES ($1, $2, $3, $4, '{}'::jsonb)
         RETURNING id",
    )
    .bind(org_id)
    .bind(issuer_url)
    .bind(audience)
    .bind(jwks_uri)
    .fetch_one(pool)
    .await
    .expect("insert hmac trust issuer");
    (id, secret)
}

fn mint_jwt(subject: &str, issuer: &str, audience: &str, secret: &[u8]) -> String {
    use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
    let claims = json!({
        "sub": subject,
        "iss": issuer,
        "aud": audience,
        "exp": chrono::Utc::now().timestamp() + 3600,
        "iat": chrono::Utc::now().timestamp(),
    });
    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret),
    )
    .expect("mint jwt")
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

async fn insert_binding(
    pool: &PgPool,
    workspace_id: Uuid,
    peer_id: Uuid,
    tenant: &str,
    foreign_workspace_id: Option<&str>,
    status: &str,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO workspace_peer_bindings
           (workspace_id, peer_id, foreign_tenant_id, foreign_workspace_id, status)
         VALUES ($1, $2, $3, $4, $5::binding_status)
         RETURNING id",
    )
    .bind(workspace_id)
    .bind(peer_id)
    .bind(tenant)
    .bind(foreign_workspace_id)
    .bind(status)
    .fetch_one(pool)
    .await
    .expect("insert binding")
}

async fn insert_signal(pool: &PgPool, workspace_id: Uuid, title: &str, severity: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO signals (workspace_id, source, title, body, severity, evidence)
         VALUES ($1, 'rule'::signal_source, $2, 'test body', $3::severity, '[]'::jsonb)
         RETURNING id",
    )
    .bind(workspace_id)
    .bind(title)
    .bind(severity)
    .fetch_one(pool)
    .await
    .expect("insert signal")
}

async fn insert_survivor(pool: &PgPool, signal_id: Uuid) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO survivors
           (signal_id, critic_model, verdict, rationale, confidence, chain_of_reasoning)
         VALUES ($1, 'test-model', 'survive'::critic_verdict, 'ok', 0.9, '[]'::jsonb)
         RETURNING id",
    )
    .bind(signal_id)
    .fetch_one(pool)
    .await
    .expect("insert survivor")
}

async fn insert_peer_routing(pool: &PgPool, survivor_id: Uuid, peer_id: Uuid) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO routing_decisions
           (survivor_id, target_kind, target_ref, classifier_model, rationale)
         VALUES ($1, 'peer'::routing_target, $2, 'test', 'peer route')
         RETURNING id",
    )
    .bind(survivor_id)
    .bind(json!({ "peer_id": peer_id }))
    .fetch_one(pool)
    .await
    .expect("insert peer routing")
}

async fn insert_mcp_connector_and_stream(
    pool: &PgPool,
    workspace_id: Uuid,
    peer_id: Uuid,
    mcp_url: &str,
    stream_name: &str,
) -> (Uuid, Uuid) {
    let connector_id: Uuid = sqlx::query_scalar(
        "INSERT INTO connectors (workspace_id, kind, name, config)
         VALUES ($1, 'mcp'::connector_kind, 'peer:test', $2)
         RETURNING id",
    )
    .bind(workspace_id)
    .bind(json!({
        "mcp_url": mcp_url,
        "bearer_token": TEST_STATIC_BEARER,
        "peer_id": peer_id,
        "workspace_id": workspace_id,
    }))
    .fetch_one(pool)
    .await
    .expect("insert connector");

    let stream_id: Uuid = sqlx::query_scalar(
        "INSERT INTO streams (connector_id, name, schema)
         VALUES ($1, $2, '{}'::jsonb)
         RETURNING id",
    )
    .bind(connector_id)
    .bind(stream_name)
    .fetch_one(pool)
    .await
    .expect("insert stream");
    (connector_id, stream_id)
}

struct RecordedMcp {
    mcp_url: String,
    requests: Arc<Mutex<Vec<Value>>>,
}

async fn spawn_recorded_mcp(workspaces: Vec<&str>, tenant: &str) -> RecordedMcp {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&requests);
    let workspace_values: Vec<Value> = workspaces
        .into_iter()
        .map(|id| json!({ "id": id, "name": id }))
        .collect();
    let tenant = tenant.to_string();

    let app = axum::Router::new().route(
        "/mcp",
        axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
            let captured = Arc::clone(&captured);
            let workspace_values = workspace_values.clone();
            let tenant = tenant.clone();
            async move {
                captured.lock().expect("requests mutex").push(body.clone());
                let id = body.get("id").cloned().unwrap_or(Value::Null);
                let method_name = body["method"].as_str().unwrap_or("");
                let result = match method_name {
                    "resources/read" => json!({
                        "contents": [{
                            "uri": "whoami://",
                            "mimeType": "application/vnd.ione.whoami+json",
                            "text": json!({
                                "foreign_tenant_id": tenant,
                                "foreign_tenant_name": "Recorded Tenant",
                                "foreign_workspace_id": "recorded-ws",
                                "foreign_user_id": "recorded-user",
                                "foreign_user_email": "recorded@example.test",
                                "foreign_roles": ["operator"]
                            }).to_string()
                        }]
                    }),
                    "tools/list" => json!({
                        "tools": [
                            { "name": "list_workspaces", "description": "list", "inputSchema": {} },
                            { "name": "list_survivors", "description": "list", "inputSchema": {} },
                            { "name": "search_stream_events", "description": "search", "inputSchema": {} },
                            { "name": "propose_artifact", "description": "propose", "inputSchema": {} }
                        ]
                    }),
                    "tools/call" => {
                        let tool = body["params"]["name"].as_str().unwrap_or("");
                        let text = match tool {
                            "list_workspaces" => json!({ "workspaces": workspace_values }).to_string(),
                            "list_survivors" => json!({ "survivors": [] }).to_string(),
                            "search_stream_events" => json!({ "events": [] }).to_string(),
                            "propose_artifact" => json!({
                                "artifact_id": Uuid::new_v4(),
                                "approval_id": Uuid::new_v4()
                            }).to_string(),
                            _ => json!({}).to_string(),
                        };
                        json!({ "content": [{ "type": "text", "text": text }], "isError": false })
                    }
                    _ => json!({}),
                };
                axum::Json(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
            }
        }),
    );

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind mcp");
    let addr = listener.local_addr().expect("mcp addr");
    tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("recorded mcp server");
    });

    RecordedMcp {
        mcp_url: format!("http://{addr}/mcp"),
        requests,
    }
}

fn requests_for_tool(recorded: &RecordedMcp, tool: &str) -> Vec<Value> {
    recorded
        .requests
        .lock()
        .expect("requests mutex")
        .iter()
        .filter(|body| {
            body["method"] == "tools/call" && body["params"]["name"].as_str() == Some(tool)
        })
        .cloned()
        .collect()
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
async fn ione_to_ione_subscribe_produces_active_binding() {
    let (base_a, pool_a) = spawn_app().await;
    let (base_b, _pool_b) = spawn_second_app().await;
    let org_id = default_org_id(&pool_a).await;
    let workspace_id = default_workspace_id(&pool_a).await;
    let issuer_id = insert_trust_issuer(&pool_a, org_id, "https://iss-ione-to-ione.test").await;
    let peer_id = insert_peer(&pool_a, "Node B", &format!("{base_b}/mcp"), issuer_id).await;

    let resp = reqwest::Client::new()
        .post(format!(
            "{base_a}/api/v1/workspaces/{workspace_id}/peers/{peer_id}/subscribe"
        ))
        .json(&json!({}))
        .send()
        .await
        .expect("subscribe");

    assert_eq!(resp.status(), StatusCode::OK);
    let (status, tenant) = binding_status_and_tenant(&pool_a, workspace_id, peer_id).await;
    assert_eq!(status, "active");
    assert_eq!(tenant, org_id.to_string());
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
async fn patch_can_clear_foreign_workspace_id_via_null() {
    let (base, pool) = spawn_app().await;
    let org_id = default_org_id(&pool).await;
    let workspace_id = default_workspace_id(&pool).await;
    let issuer_id = insert_trust_issuer(&pool, org_id, "https://iss-clear.test").await;
    let peer_id = insert_peer(&pool, "Clear Peer", "http://127.0.0.1:9/mcp", issuer_id).await;
    let binding_id = insert_binding(
        &pool,
        workspace_id,
        peer_id,
        "t-clear",
        Some("fws-clear"),
        "active",
    )
    .await;

    let resp = reqwest::Client::new()
        .patch(format!(
            "{base}/api/v1/workspaces/{workspace_id}/bindings/{binding_id}"
        ))
        .json(&json!({ "foreignWorkspaceId": Value::Null }))
        .send()
        .await
        .expect("patch clear");

    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("json");
    assert!(body["foreignWorkspaceId"].is_null());
}

#[tokio::test]
#[ignore]
async fn refresh_returns_409_on_tenant_drift_and_preserves_stored_value() {
    let (base, pool) = spawn_app().await;
    let org_id = default_org_id(&pool).await;
    let workspace_id = default_workspace_id(&pool).await;
    let mock = spawn_recorded_mcp(vec!["remote-ws"], "t-new").await;
    let issuer_id = insert_trust_issuer(&pool, org_id, "https://iss-refresh-drift.test").await;
    let peer_id = insert_peer(&pool, "Refresh Drift Peer", &mock.mcp_url, issuer_id).await;
    let binding_id = insert_binding(
        &pool,
        workspace_id,
        peer_id,
        "t-old",
        Some("fws-old"),
        "active",
    )
    .await;

    let resp = reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/workspaces/{workspace_id}/bindings/{binding_id}/refresh"
        ))
        .send()
        .await
        .expect("refresh");

    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let (status, tenant) = binding_status_and_tenant(&pool, workspace_id, peer_id).await;
    assert_eq!(status, "conflict");
    assert_eq!(tenant, "t-old");
}

#[tokio::test]
#[ignore]
async fn refresh_returns_502_when_peer_unreachable() {
    let (base, pool) = spawn_app().await;
    let org_id = default_org_id(&pool).await;
    let workspace_id = default_workspace_id(&pool).await;
    let issuer_id = insert_trust_issuer(&pool, org_id, "https://iss-refresh-down.test").await;
    let peer_id = insert_peer(
        &pool,
        "Refresh Down Peer",
        "http://127.0.0.1:9/mcp",
        issuer_id,
    )
    .await;
    let binding_id = insert_binding(
        &pool,
        workspace_id,
        peer_id,
        "t-down",
        Some("fws-down"),
        "active",
    )
    .await;

    let resp = reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/workspaces/{workspace_id}/bindings/{binding_id}/refresh"
        ))
        .send()
        .await
        .expect("refresh");

    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
}

#[tokio::test]
#[ignore]
async fn delete_binding_does_not_revoke_peer() {
    let (base, pool) = spawn_app().await;
    let org_id = default_org_id(&pool).await;
    let workspace_id = default_workspace_id(&pool).await;
    let issuer_id = insert_trust_issuer(&pool, org_id, "https://iss-delete-binding.test").await;
    let peer_id = insert_peer(
        &pool,
        "Delete Binding Peer",
        "http://127.0.0.1:9/mcp",
        issuer_id,
    )
    .await;
    let binding_id = insert_binding(
        &pool,
        workspace_id,
        peer_id,
        "t-delete",
        Some("fws-delete"),
        "active",
    )
    .await;

    let resp = reqwest::Client::new()
        .delete(format!(
            "{base}/api/v1/workspaces/{workspace_id}/bindings/{binding_id}"
        ))
        .send()
        .await
        .expect("delete binding");

    assert_eq!(resp.status(), StatusCode::OK);
    let peer_status: String = sqlx::query_scalar("SELECT status::TEXT FROM peers WHERE id = $1")
        .bind(peer_id)
        .fetch_one(&pool)
        .await
        .expect("peer status");
    assert_ne!(peer_status, "revoked");
}

#[tokio::test]
#[ignore]
async fn list_for_workspace_returns_only_that_workspaces_bindings() {
    let (base, pool) = spawn_app().await;
    let org_id = default_org_id(&pool).await;
    let ws_a = default_workspace_id(&pool).await;
    let ws_b = insert_workspace(&pool, org_id, "Second WS").await;
    let issuer_id = insert_trust_issuer(&pool, org_id, "https://iss-list-ws.test").await;
    let peer_a = insert_peer(&pool, "List A", "http://127.0.0.1:91/mcp", issuer_id).await;
    let peer_b = insert_peer(&pool, "List B", "http://127.0.0.1:92/mcp", issuer_id).await;
    let binding_a = insert_binding(&pool, ws_a, peer_a, "t-a", None, "active").await;
    let _binding_b = insert_binding(&pool, ws_b, peer_b, "t-b", None, "active").await;

    let resp = reqwest::Client::new()
        .get(format!("{base}/api/v1/workspaces/{ws_a}/bindings"))
        .send()
        .await
        .expect("list workspace bindings");
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("json");
    let items = body["items"].as_array().expect("items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"], binding_a.to_string());
}

#[tokio::test]
#[ignore]
async fn cross_org_binding_routes_do_not_leak_or_mutate() {
    let (base, pool) = spawn_app().await;
    let other_org = insert_org(&pool, "Binding Other Org").await;
    let other_ws = insert_workspace(&pool, other_org, "Other WS").await;
    let issuer_id = insert_trust_issuer(&pool, other_org, "https://iss-other-bindings.test").await;
    let peer_id = insert_peer(
        &pool,
        "Other Binding Peer",
        "http://127.0.0.1:9/mcp",
        issuer_id,
    )
    .await;
    let binding_id = insert_binding(
        &pool,
        other_ws,
        peer_id,
        "t-other",
        Some("fws-other"),
        "active",
    )
    .await;
    let client = reqwest::Client::new();

    let get_resp = client
        .get(format!(
            "{base}/api/v1/workspaces/{other_ws}/bindings/{binding_id}"
        ))
        .send()
        .await
        .expect("get");
    let patch_resp = client
        .patch(format!(
            "{base}/api/v1/workspaces/{other_ws}/bindings/{binding_id}"
        ))
        .json(&json!({ "foreignTenantId": "t-mutated" }))
        .send()
        .await
        .expect("patch");
    let delete_resp = client
        .delete(format!(
            "{base}/api/v1/workspaces/{other_ws}/bindings/{binding_id}"
        ))
        .send()
        .await
        .expect("delete");
    let refresh_resp = client
        .post(format!(
            "{base}/api/v1/workspaces/{other_ws}/bindings/{binding_id}/refresh"
        ))
        .send()
        .await
        .expect("refresh");
    let list_resp = client
        .get(format!("{base}/api/v1/peers/{peer_id}/bindings"))
        .send()
        .await
        .expect("peer binding list");

    assert_eq!(get_resp.status(), StatusCode::NOT_FOUND);
    assert_eq!(patch_resp.status(), StatusCode::NOT_FOUND);
    assert_eq!(delete_resp.status(), StatusCode::NOT_FOUND);
    assert_eq!(refresh_resp.status(), StatusCode::NOT_FOUND);
    assert_eq!(list_resp.status(), StatusCode::OK);
    assert_eq!(
        list_resp.json::<Value>().await.expect("json")["items"]
            .as_array()
            .expect("items")
            .len(),
        0
    );
    let (status, tenant) = binding_status_and_tenant(&pool, other_ws, peer_id).await;
    assert_eq!(status, "active");
    assert_eq!(tenant, "t-other");
}

#[tokio::test]
#[ignore]
async fn duplicate_manual_create_returns_409() {
    let (base, pool) = spawn_app().await;
    let org_id = default_org_id(&pool).await;
    let workspace_id = default_workspace_id(&pool).await;
    let issuer_id = insert_trust_issuer(&pool, org_id, "https://iss-duplicate.test").await;
    let peer_id = insert_peer(&pool, "Duplicate Peer", "http://127.0.0.1:9/mcp", issuer_id).await;
    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/workspaces/{workspace_id}/bindings");

    let first = client
        .post(&url)
        .json(&json!({ "peerId": peer_id, "foreignTenantId": "t-dup" }))
        .send()
        .await
        .expect("first create");
    let second = client
        .post(&url)
        .json(&json!({ "peerId": peer_id, "foreignTenantId": "t-dup" }))
        .send()
        .await
        .expect("second create");

    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(second.status(), StatusCode::CONFLICT);
}

#[tokio::test]
#[ignore]
async fn manual_create_with_cross_org_peer_returns_404() {
    let (base, pool) = spawn_app().await;
    let workspace_id = default_workspace_id(&pool).await;
    let other_org = insert_org(&pool, "Manual Other Org").await;
    let issuer_id = insert_trust_issuer(&pool, other_org, "https://iss-manual-cross.test").await;
    let peer_id = insert_peer(
        &pool,
        "Manual Cross Peer",
        "http://127.0.0.1:9/mcp",
        issuer_id,
    )
    .await;

    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/workspaces/{workspace_id}/bindings"))
        .json(&json!({ "peerId": peer_id, "foreignTenantId": "t-cross" }))
        .send()
        .await
        .expect("manual create");

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM workspace_peer_bindings WHERE peer_id = $1")
            .bind(peer_id)
            .fetch_one(&pool)
            .await
            .expect("binding count");
    assert_eq!(count, 0);
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

#[tokio::test]
#[ignore]
async fn mcp_client_poll_uses_binding_foreign_workspace_id_when_active() {
    let (base, pool) = spawn_app().await;
    let org_id = default_org_id(&pool).await;
    let workspace_id = default_workspace_id(&pool).await;
    let mcp = spawn_recorded_mcp(vec!["remote-a", "remote-b"], "t-remote").await;
    let issuer_id = insert_trust_issuer(&pool, org_id, "https://iss-poll-binding.test").await;
    let peer_id = insert_peer(&pool, "Poll Binding Peer", &mcp.mcp_url, issuer_id).await;
    let (_connector_id, stream_id) = insert_mcp_connector_and_stream(
        &pool,
        workspace_id,
        peer_id,
        &mcp.mcp_url,
        "list_survivors",
    )
    .await;
    insert_binding(
        &pool,
        workspace_id,
        peer_id,
        "t-bound",
        Some("fws-bound"),
        "active",
    )
    .await;

    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/streams/{stream_id}/poll"))
        .send()
        .await
        .expect("poll");

    assert_eq!(resp.status(), StatusCode::OK);
    let list_workspaces_calls = requests_for_tool(&mcp, "list_workspaces");
    let survivor_calls = requests_for_tool(&mcp, "list_survivors");
    assert!(list_workspaces_calls.is_empty());
    assert_eq!(survivor_calls.len(), 1);
    assert_eq!(
        survivor_calls[0]["params"]["arguments"]["workspace_id"],
        "fws-bound"
    );
}

#[tokio::test]
#[ignore]
async fn mcp_client_poll_falls_back_when_binding_inactive_or_missing() {
    for status in [Some("pending"), None] {
        let (base, pool) = spawn_app().await;
        let org_id = default_org_id(&pool).await;
        let workspace_id = default_workspace_id(&pool).await;
        let mcp = spawn_recorded_mcp(vec!["remote-a", "remote-b"], "t-remote").await;
        let issuer_id = insert_trust_issuer(&pool, org_id, "https://iss-poll-fallback.test").await;
        let peer_id = insert_peer(&pool, "Poll Fallback Peer", &mcp.mcp_url, issuer_id).await;
        let (_connector_id, stream_id) = insert_mcp_connector_and_stream(
            &pool,
            workspace_id,
            peer_id,
            &mcp.mcp_url,
            "list_survivors",
        )
        .await;
        if let Some(status) = status {
            insert_binding(
                &pool,
                workspace_id,
                peer_id,
                "t-fallback",
                Some("fws-bound"),
                status,
            )
            .await;
        }

        let resp = reqwest::Client::new()
            .post(format!("{base}/api/v1/streams/{stream_id}/poll"))
            .send()
            .await
            .expect("poll");

        assert_eq!(resp.status(), StatusCode::OK);
        let list_workspaces_calls = requests_for_tool(&mcp, "list_workspaces");
        let survivor_calls = requests_for_tool(&mcp, "list_survivors");
        assert_eq!(list_workspaces_calls.len(), 1);
        let called_workspaces: Vec<String> = survivor_calls
            .iter()
            .filter_map(|call| call["params"]["arguments"]["workspace_id"].as_str())
            .map(str::to_string)
            .collect();
        assert_eq!(called_workspaces, vec!["remote-a", "remote-b"]);
    }
}

#[tokio::test]
#[ignore]
async fn delivery_peer_routing_uses_binding_when_active_and_audits_tenant() {
    let (_base, pool) = spawn_app().await;
    let org_id = default_org_id(&pool).await;
    let workspace_id = default_workspace_id(&pool).await;
    let mcp = spawn_recorded_mcp(vec!["remote-a", "remote-b"], "t-remote").await;
    let issuer_id = insert_trust_issuer(&pool, org_id, "https://iss-delivery-binding.test").await;
    let peer_id = insert_peer(&pool, "Delivery Binding Peer", &mcp.mcp_url, issuer_id).await;
    insert_mcp_connector_and_stream(&pool, workspace_id, peer_id, &mcp.mcp_url, "list_survivors")
        .await;
    insert_binding(
        &pool,
        workspace_id,
        peer_id,
        "t-delivery",
        Some("fws-delivery"),
        "active",
    )
    .await;
    let signal_id = insert_signal(&pool, workspace_id, "Bound peer signal", "routine").await;
    let survivor_id = insert_survivor(&pool, signal_id).await;
    let routing_id = insert_peer_routing(&pool, survivor_id, peer_id).await;
    let (_router, state) = ione::app_with_state(pool.clone()).await;

    ione::services::delivery::process_routing_decision(&state, routing_id)
        .await
        .expect("process peer routing");

    let propose_calls = requests_for_tool(&mcp, "propose_artifact");
    assert_eq!(propose_calls.len(), 1);
    assert_eq!(
        propose_calls[0]["params"]["arguments"]["workspace_id"],
        "fws-delivery"
    );
    let audit_tenant: Option<String> = sqlx::query_scalar(
        "SELECT foreign_tenant_id
         FROM audit_events
         WHERE workspace_id = $1 AND object_id = $2 AND verb = 'peer_delivered'
         LIMIT 1",
    )
    .bind(workspace_id)
    .bind(peer_id)
    .fetch_one(&pool)
    .await
    .expect("audit foreign tenant");
    assert_eq!(audit_tenant.as_deref(), Some("t-delivery"));
}

#[tokio::test]
#[ignore]
async fn peer_delivery_audit_has_null_foreign_tenant_for_pending_or_inactive_bindings() {
    for status in ["pending", "inactive"] {
        let (_base, pool) = spawn_app().await;
        let org_id = default_org_id(&pool).await;
        let workspace_id = default_workspace_id(&pool).await;
        let mcp = spawn_recorded_mcp(vec!["remote-a"], "t-remote").await;
        let issuer_id = insert_trust_issuer(
            &pool,
            org_id,
            &format!("https://iss-delivery-{status}.test"),
        )
        .await;
        let peer_id = insert_peer(&pool, "Delivery Null Peer", &mcp.mcp_url, issuer_id).await;
        insert_mcp_connector_and_stream(
            &pool,
            workspace_id,
            peer_id,
            &mcp.mcp_url,
            "list_survivors",
        )
        .await;
        insert_binding(
            &pool,
            workspace_id,
            peer_id,
            "t-null",
            Some("fws-null"),
            status,
        )
        .await;
        let signal_id = insert_signal(&pool, workspace_id, "Null peer signal", "routine").await;
        let survivor_id = insert_survivor(&pool, signal_id).await;
        let routing_id = insert_peer_routing(&pool, survivor_id, peer_id).await;
        let (_router, state) = ione::app_with_state(pool.clone()).await;

        ione::services::delivery::process_routing_decision(&state, routing_id)
            .await
            .expect("process peer routing");

        let audit_tenant: Option<String> = sqlx::query_scalar(
            "SELECT foreign_tenant_id
             FROM audit_events
             WHERE workspace_id = $1 AND object_id = $2 AND verb = 'peer_delivered'
             LIMIT 1",
        )
        .bind(workspace_id)
        .bind(peer_id)
        .fetch_one(&pool)
        .await
        .expect("audit foreign tenant");
        assert!(
            audit_tenant.is_none(),
            "status {status} should not enrich audit"
        );
    }
}

#[tokio::test]
#[ignore]
async fn peer_soft_revoke_cascades_bindings_to_inactive() {
    let (base, pool) = spawn_app().await;
    let org_id = default_org_id(&pool).await;
    let workspace_id = default_workspace_id(&pool).await;
    let issuer_id = insert_trust_issuer(&pool, org_id, "https://iss-revoke.test").await;
    let peer_id = insert_peer(&pool, "Revoke Peer", "http://127.0.0.1:9/mcp", issuer_id).await;
    insert_binding(
        &pool,
        workspace_id,
        peer_id,
        "t-revoke",
        Some("fws-revoke"),
        "active",
    )
    .await;

    let resp = reqwest::Client::new()
        .delete(format!("{base}/api/v1/peers/{peer_id}"))
        .send()
        .await
        .expect("delete peer");
    assert_eq!(resp.status(), StatusCode::OK);

    let peer_status: String = sqlx::query_scalar("SELECT status::TEXT FROM peers WHERE id = $1")
        .bind(peer_id)
        .fetch_one(&pool)
        .await
        .expect("peer status");
    let (binding_status, tenant) = binding_status_and_tenant(&pool, workspace_id, peer_id).await;
    assert_eq!(peer_status, "revoked");
    assert_eq!(binding_status, "inactive");
    assert_eq!(tenant, "t-revoke");
}

#[tokio::test]
#[ignore]
async fn approval_for_inbound_peer_proposal_carries_foreign_tenant_id() {
    let (base, pool) = spawn_app().await;
    let org_id = default_org_id(&pool).await;
    let workspace_id = default_workspace_id(&pool).await;
    let issuer = "https://iss-inbound-peer.test";
    let audience = "mcp";
    let (issuer_id, secret) = insert_hmac_trust_issuer(&pool, org_id, issuer, audience).await;
    let _peer_id = insert_peer(&pool, "Inbound Peer", "http://127.0.0.1:9/mcp", issuer_id).await;
    let token = mint_jwt("peer-subject", issuer, audience, &secret);
    let previous_static_bearer = std::env::var("IONE_OAUTH_STATIC_BEARER").ok();
    std::env::set_var("IONE_OAUTH_STATIC_BEARER", &token);

    let resp = reqwest::Client::new()
        .post(format!("{base}/mcp"))
        .bearer_auth(token)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "propose_artifact",
                "arguments": {
                    "workspace_id": workspace_id.to_string(),
                    "kind": "briefing",
                    "content": { "title": "Inbound peer proposal" }
                }
            }
        }))
        .send()
        .await
        .expect("mcp propose");
    if let Some(previous) = previous_static_bearer {
        std::env::set_var("IONE_OAUTH_STATIC_BEARER", previous);
    }
    assert_eq!(resp.status(), StatusCode::OK);

    let tenant: Option<String> = sqlx::query_scalar(
        "SELECT ap.foreign_tenant_id
         FROM approvals ap
         JOIN artifacts art ON art.id = ap.artifact_id
         WHERE art.workspace_id = $1
         LIMIT 1",
    )
    .bind(workspace_id)
    .fetch_one(&pool)
    .await
    .expect("approval tenant");
    assert_eq!(tenant.as_deref(), Some(org_id.to_string().as_str()));
}

#[tokio::test]
#[ignore]
async fn approval_for_unbound_local_proposal_has_null_foreign_tenant_id() {
    let (base, pool) = spawn_app().await;
    let workspace_id = default_workspace_id(&pool).await;

    let resp = reqwest::Client::new()
        .post(format!("{base}/mcp"))
        .bearer_auth(TEST_STATIC_BEARER)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "propose_artifact",
                "arguments": {
                    "workspace_id": workspace_id.to_string(),
                    "kind": "briefing",
                    "content": { "title": "Local proposal" }
                }
            }
        }))
        .send()
        .await
        .expect("mcp propose");
    assert_eq!(resp.status(), StatusCode::OK);

    let tenant: Option<String> = sqlx::query_scalar(
        "SELECT ap.foreign_tenant_id
         FROM approvals ap
         JOIN artifacts art ON art.id = ap.artifact_id
         WHERE art.workspace_id = $1
         LIMIT 1",
    )
    .bind(workspace_id)
    .fetch_one(&pool)
    .await
    .expect("approval tenant");
    assert!(tenant.is_none());
}
