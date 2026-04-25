/// Phase 11 contract tests — IONe-as-MCP-server.
///
/// Targets:
///   - Contract: md/design/ione-v1-contract.md  §API `/mcp`
///   - Plan:     md/plans/ione-v1-plan.md        Phase 11
///
/// Transport: HTTP+SSE (JSON-RPC 2.0 over POST /mcp; SSE over GET /mcp/sse).
/// Auth: `Authorization: Bearer <token>` is required by the /mcp middleware.
///
/// Prerequisites:
///   docker compose up -d postgres
///   DATABASE_URL=postgres://ione:ione@localhost:5433/ione
///
/// Run:
///   DATABASE_URL=postgres://ione:ione@localhost:5433/ione \
///     IONE_SKIP_LIVE=1 \
///     cargo test --test phase11_mcp_server -- --ignored --test-threads=1
///
/// All tests are #[ignore]-gated and must be run with --test-threads=1.
use std::net::SocketAddr;

use reqwest::StatusCode;
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione";
const TEST_STATIC_BEARER: &str = "phase11-test-bearer";

// ─── Harness ──────────────────────────────────────────────────────────────────

async fn spawn_app() -> (String, PgPool) {
    spawn_app_with_auth_mode("local").await
}

async fn spawn_app_with_auth_mode(auth_mode: &str) -> (String, PgPool) {
    std::env::set_var("IONE_AUTH_MODE", auth_mode);
    std::env::set_var("IONE_OAUTH_STATIC_BEARER", TEST_STATIC_BEARER);

    let db_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_owned());

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("failed to connect to Postgres — is `docker compose up -d postgres` running?");

    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("migration failed");

    sqlx::query(
        "TRUNCATE audit_events, approvals, artifacts,
                  trust_issuers, routing_decisions, survivors, signals,
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
        .expect("failed to bind random port");
    let addr: SocketAddr = listener.local_addr().expect("failed to get local addr");

    let app = ione::app(pool.clone()).await;

    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("server error");
    });

    (format!("http://{}", addr), pool)
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

async fn default_user_id(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("SELECT id FROM users WHERE email = 'default@localhost' LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("default user not found")
}

async fn default_org_id(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("SELECT id FROM organizations WHERE name = 'Default Org' LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("Default Org not found")
}

/// POST a JSON-RPC request to /mcp and return parsed response body.
async fn mcp_post(base: &str, body: Value) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{}/mcp", base))
        .header("Authorization", format!("Bearer {}", TEST_STATIC_BEARER))
        .json(&body)
        .send()
        .await
        .expect("POST /mcp failed")
}

/// POST a JSON-RPC request to /mcp without Authorization.
async fn mcp_post_unauthenticated(base: &str, body: Value) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{}/mcp", base))
        .json(&body)
        .send()
        .await
        .expect("POST /mcp failed")
}

/// POST to /mcp with bearer auth and a valid session cookie attached.
async fn mcp_post_with_cookie(base: &str, body: Value, cookie: &str) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{}/mcp", base))
        .header("Authorization", format!("Bearer {}", TEST_STATIC_BEARER))
        .header("Cookie", cookie)
        .json(&body)
        .send()
        .await
        .expect("POST /mcp with cookie failed")
}

/// POST to /mcp with an Authorization: Bearer token.
async fn mcp_post_with_bearer(base: &str, body: Value, token: &str) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{}/mcp", base))
        .header("Authorization", format!("Bearer {}", token))
        .json(&body)
        .send()
        .await
        .expect("POST /mcp with bearer failed")
}

/// Issue a signed test session cookie for the given user.
async fn make_session_cookie(user_id: Uuid) -> String {
    ione::auth::issue_session_cookie(user_id)
        .await
        .expect("failed to issue session cookie")
}

/// Seed a workspace and return its id.
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
    .expect("insert workspace failed")
}

/// Seed a signal and return its id.
async fn insert_signal(pool: &PgPool, workspace_id: Uuid, title: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO signals (workspace_id, source, title, body, severity, evidence)
         VALUES ($1, 'rule'::signal_source, $2, 'test body', 'routine'::severity, '[]'::jsonb)
         RETURNING id",
    )
    .bind(workspace_id)
    .bind(title)
    .fetch_one(pool)
    .await
    .expect("insert signal failed")
}

/// Seed a survivor for the given signal and return its id.
async fn insert_survivor(pool: &PgPool, signal_id: Uuid) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO survivors
           (signal_id, critic_model, verdict, rationale, confidence, chain_of_reasoning)
         VALUES ($1, 'phi4-reasoning:14b', 'survive'::critic_verdict,
                 'test rationale', 0.9, '[]'::jsonb)
         RETURNING id",
    )
    .bind(signal_id)
    .fetch_one(pool)
    .await
    .expect("insert survivor failed")
}

/// Seed a connector and stream, insert one stream event, return (connector_id, stream_id).
async fn insert_connector_and_event(
    pool: &PgPool,
    workspace_id: Uuid,
    name: &str,
    config: Value,
) -> (Uuid, Uuid) {
    let connector_id: Uuid = sqlx::query_scalar(
        "INSERT INTO connectors (workspace_id, kind, name, config)
         VALUES ($1, 'rust_native'::connector_kind, $2, $3)
         RETURNING id",
    )
    .bind(workspace_id)
    .bind(name)
    .bind(config)
    .fetch_one(pool)
    .await
    .expect("insert connector failed");

    let stream_id: Uuid = sqlx::query_scalar(
        "INSERT INTO streams (connector_id, name, schema)
         VALUES ($1, 'test_stream', '{}'::jsonb)
         RETURNING id",
    )
    .bind(connector_id)
    .fetch_one(pool)
    .await
    .expect("insert stream failed");

    sqlx::query(
        "INSERT INTO stream_events (stream_id, payload, observed_at)
         VALUES ($1, '{\"value\": 42}'::jsonb, now())",
    )
    .bind(stream_id)
    .execute(pool)
    .await
    .expect("insert stream event failed");

    (connector_id, stream_id)
}

/// Mint a signed HS256 JWT for testing.
fn mint_jwt(subject: &str, issuer: &str, audience: &str, secret: &[u8]) -> String {
    use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};

    let claims = json!({
        "sub": subject,
        "iss": issuer,
        "aud": audience,
        "exp": chrono::Utc::now().timestamp() + 3600,
        "iat": chrono::Utc::now().timestamp(),
    });
    let header = Header::new(Algorithm::HS256);
    encode(&header, &claims, &EncodingKey::from_secret(secret)).expect("failed to mint test JWT")
}

// ─── Test 1: initialize handshake ────────────────────────────────────────────

/// MCP initialize → 200 with serverInfo and capabilities.tools.
///
/// Contract: `POST /mcp` with `{"method":"initialize"}` → `{serverInfo, capabilities:{tools}}`.
#[tokio::test]
#[ignore]
async fn mcp_initialize_returns_capabilities() {
    let (base, _pool) = spawn_app().await;

    let resp = mcp_post(
        &base,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "protocolVersion": "2025-03", "capabilities": {} }
        }),
    )
    .await;

    assert_eq!(resp.status(), StatusCode::OK, "initialize must return 200");

    let body: Value = resp.json().await.expect("response not JSON");

    assert_eq!(
        body["jsonrpc"], "2.0",
        "response must have jsonrpc='2.0', got: {}",
        body["jsonrpc"]
    );
    assert!(
        !body["result"]["serverInfo"]["name"].is_null(),
        "result.serverInfo.name must be present, got: {}",
        body["result"]
    );
    assert!(
        !body["result"]["capabilities"]["tools"].is_null(),
        "result.capabilities.tools must be present, got: {}",
        body["result"]
    );
    assert_eq!(
        body["error"],
        Value::Null,
        "no error expected on initialize, got: {}",
        body["error"]
    );
}

// ─── Test 2: tools/list ───────────────────────────────────────────────────────

/// tools/list → includes all 5 required tools with schemas.
#[tokio::test]
#[ignore]
async fn mcp_tools_list_returns_all_tools() {
    let (base, _pool) = spawn_app().await;

    let resp = mcp_post(
        &base,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": null
        }),
    )
    .await;

    assert_eq!(resp.status(), StatusCode::OK, "tools/list must return 200");

    let body: Value = resp.json().await.expect("response not JSON");
    let tools = body["result"]["tools"]
        .as_array()
        .expect("result.tools must be an array");

    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();

    for required in &[
        "list_workspaces",
        "list_survivors",
        "search_stream_events",
        "propose_artifact",
        "deliver_notification",
    ] {
        assert!(
            names.contains(required),
            "tools/list must include tool '{}', got names: {:?}",
            required,
            names
        );
    }

    // Each tool must have an inputSchema.
    for tool in tools {
        assert!(
            !tool["inputSchema"].is_null(),
            "tool '{}' must have an inputSchema, got: {}",
            tool["name"],
            tool
        );
    }
}

// ─── Test 3: list_workspaces returns items ───────────────────────────────────

/// list_workspaces → returns array including the seeded Operations workspace.
#[tokio::test]
#[ignore]
async fn mcp_tools_call_list_workspaces_returns_items() {
    let (base, pool) = spawn_app().await;

    let user_id = default_user_id(&pool).await;
    let cookie = make_session_cookie(user_id).await;

    let resp = mcp_post_with_cookie(
        &base,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": { "name": "list_workspaces", "arguments": {} }
        }),
        &cookie,
    )
    .await;

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "list_workspaces must return 200"
    );

    let body: Value = resp.json().await.expect("response not JSON");
    assert!(
        body["error"].is_null(),
        "list_workspaces must not return an error, got: {}",
        body["error"]
    );

    // Unpack the MCP tools/call result envelope: result.content[0].text
    let text = body["result"]["content"][0]["text"]
        .as_str()
        .expect("result.content[0].text must be a string");
    let data: Value = serde_json::from_str(text).expect("content text must be JSON");

    let workspaces = data["workspaces"]
        .as_array()
        .expect("workspaces key must be an array");

    assert!(
        !workspaces.is_empty(),
        "list_workspaces must return at least one workspace (the seeded Operations workspace)"
    );

    let ops = workspaces
        .iter()
        .find(|w| w["name"].as_str() == Some("Operations"));
    assert!(
        ops.is_some(),
        "workspaces must include the seeded 'Operations' workspace, got: {:?}",
        workspaces
    );

    // Shape check: each workspace must have id, name, domain, lifecycle.
    for ws in workspaces {
        assert!(!ws["id"].is_null(), "workspace must have id");
        assert!(!ws["name"].is_null(), "workspace must have name");
        assert!(!ws["domain"].is_null(), "workspace must have domain");
        assert!(!ws["lifecycle"].is_null(), "workspace must have lifecycle");
    }
}

// ─── Test 4: list_survivors filters by workspace ─────────────────────────────

/// list_survivors with workspace_id=A returns only A's survivors; workspace_id=B returns empty.
#[tokio::test]
#[ignore]
async fn mcp_tools_call_list_survivors_filters_by_workspace() {
    let (base, pool) = spawn_app().await;

    let user_id = default_user_id(&pool).await;
    let org_id = default_org_id(&pool).await;
    let cookie = make_session_cookie(user_id).await;

    let ws_a = insert_workspace(&pool, org_id, "MCP Test WS A").await;
    let ws_b = insert_workspace(&pool, org_id, "MCP Test WS B").await;

    let sig_a = insert_signal(&pool, ws_a, "Signal in A").await;
    insert_survivor(&pool, sig_a).await;

    // workspace_id=A → should return 1 survivor
    let resp_a = mcp_post_with_cookie(
        &base,
        json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": "list_survivors",
                "arguments": { "workspace_id": ws_a.to_string(), "limit": 10 }
            }
        }),
        &cookie,
    )
    .await;

    assert_eq!(resp_a.status(), StatusCode::OK);

    let body_a: Value = resp_a.json().await.expect("response not JSON");
    assert!(
        body_a["error"].is_null(),
        "list_survivors (A) must not error"
    );

    let text_a = body_a["result"]["content"][0]["text"]
        .as_str()
        .expect("content text must be string");
    let data_a: Value = serde_json::from_str(text_a).expect("content must be JSON");
    let survivors_a = data_a["survivors"]
        .as_array()
        .expect("survivors must be array");
    assert_eq!(
        survivors_a.len(),
        1,
        "workspace A must have exactly 1 survivor, got {}",
        survivors_a.len()
    );

    // workspace_id=B → should return 0 survivors
    let resp_b = mcp_post_with_cookie(
        &base,
        json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": {
                "name": "list_survivors",
                "arguments": { "workspace_id": ws_b.to_string(), "limit": 10 }
            }
        }),
        &cookie,
    )
    .await;

    assert_eq!(resp_b.status(), StatusCode::OK);

    let body_b: Value = resp_b.json().await.expect("response not JSON");
    assert!(
        body_b["error"].is_null(),
        "list_survivors (B) must not error"
    );

    let text_b = body_b["result"]["content"][0]["text"]
        .as_str()
        .expect("content text");
    let data_b: Value = serde_json::from_str(text_b).expect("content JSON");
    let survivors_b = data_b["survivors"].as_array().expect("survivors array");
    assert_eq!(
        survivors_b.len(),
        0,
        "workspace B must have 0 survivors, got {}",
        survivors_b.len()
    );
}

// ─── Test 5: search_stream_events returns events ─────────────────────────────

/// search_stream_events → returns items for a seeded workspace.
#[tokio::test]
#[ignore]
async fn mcp_tools_call_search_stream_events_returns_recent() {
    let (base, pool) = spawn_app().await;

    let user_id = default_user_id(&pool).await;
    let org_id = default_org_id(&pool).await;
    let cookie = make_session_cookie(user_id).await;

    let ws_id = insert_workspace(&pool, org_id, "SSE Test WS").await;
    insert_connector_and_event(&pool, ws_id, "nws", json!({"lat": 46.87, "lon": -113.99})).await;

    let resp = mcp_post_with_cookie(
        &base,
        json!({
            "jsonrpc": "2.0",
            "id": 6,
            "method": "tools/call",
            "params": {
                "name": "search_stream_events",
                "arguments": { "workspace_id": ws_id.to_string(), "limit": 10 }
            }
        }),
        &cookie,
    )
    .await;

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "search_stream_events must return 200"
    );

    let body: Value = resp.json().await.expect("response not JSON");
    assert!(
        body["error"].is_null(),
        "search_stream_events must not error"
    );

    let text = body["result"]["content"][0]["text"]
        .as_str()
        .expect("content text");
    let data: Value = serde_json::from_str(text).expect("content JSON");
    let events = data["events"].as_array().expect("events must be array");

    assert_eq!(
        events.len(),
        1,
        "must return exactly 1 seeded stream event, got {}",
        events.len()
    );
    assert!(
        !events[0]["id"].is_null(),
        "stream event must have id field"
    );
    assert!(
        !events[0]["payload"].is_null(),
        "stream event must have payload field"
    );
}

// ─── Test 6: propose_artifact creates artifact + approval ─────────────────────

/// propose_artifact with kind='briefing' → artifact + pending approval in DB.
#[tokio::test]
#[ignore]
async fn mcp_tools_call_propose_artifact_creates_approval() {
    let (base, pool) = spawn_app().await;

    let user_id = default_user_id(&pool).await;
    let org_id = default_org_id(&pool).await;
    let cookie = make_session_cookie(user_id).await;

    let ws_id = insert_workspace(&pool, org_id, "Artifact Test WS").await;

    let resp = mcp_post_with_cookie(
        &base,
        json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/call",
            "params": {
                "name": "propose_artifact",
                "arguments": {
                    "workspace_id": ws_id.to_string(),
                    "kind": "briefing",
                    "content": { "summary": "Test briefing from MCP" }
                }
            }
        }),
        &cookie,
    )
    .await;

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "propose_artifact must return 200"
    );

    let body: Value = resp.json().await.expect("response not JSON");
    assert!(
        body["error"].is_null(),
        "propose_artifact must not error for kind='briefing', got: {}",
        body["error"]
    );

    let text = body["result"]["content"][0]["text"]
        .as_str()
        .expect("content text");
    let data: Value = serde_json::from_str(text).expect("content JSON");

    let artifact_id_str = data["artifact_id"]
        .as_str()
        .expect("artifact_id must be present in response");
    let approval_id_str = data["approval_id"]
        .as_str()
        .expect("approval_id must be present in response");

    let artifact_id = Uuid::parse_str(artifact_id_str).expect("artifact_id must be valid UUID");
    let approval_id = Uuid::parse_str(approval_id_str).expect("approval_id must be valid UUID");

    // Verify artifact exists in DB with correct kind.
    let kind: String = sqlx::query_scalar("SELECT kind::TEXT FROM artifacts WHERE id = $1")
        .bind(artifact_id)
        .fetch_one(&pool)
        .await
        .expect("artifact not found in DB");
    assert_eq!(
        kind, "briefing",
        "artifact.kind must be 'briefing', got: {kind}"
    );

    // Verify approval is pending.
    let status: String = sqlx::query_scalar("SELECT status::TEXT FROM approvals WHERE id = $1")
        .bind(approval_id)
        .fetch_one(&pool)
        .await
        .expect("approval not found in DB");
    assert_eq!(
        status, "pending",
        "approval.status must be 'pending', got: {status}"
    );
}

// ─── Test 7: propose_artifact rejects forbidden kinds ─────────────────────────

/// propose_artifact with kind='notification_draft' → MCP error response.
#[tokio::test]
#[ignore]
async fn mcp_tools_call_propose_artifact_rejects_forbidden_kind() {
    let (base, pool) = spawn_app().await;

    let user_id = default_user_id(&pool).await;
    let org_id = default_org_id(&pool).await;
    let cookie = make_session_cookie(user_id).await;

    let ws_id = insert_workspace(&pool, org_id, "Forbidden Kind WS").await;

    let resp = mcp_post_with_cookie(
        &base,
        json!({
            "jsonrpc": "2.0",
            "id": 8,
            "method": "tools/call",
            "params": {
                "name": "propose_artifact",
                "arguments": {
                    "workspace_id": ws_id.to_string(),
                    "kind": "notification_draft",
                    "content": { "text": "should be forbidden" }
                }
            }
        }),
        &cookie,
    )
    .await;

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "MCP errors are returned in the response body with HTTP 200"
    );

    let body: Value = resp.json().await.expect("response not JSON");

    // The MCP error object must be present.
    assert!(
        !body["error"].is_null(),
        "propose_artifact with kind='notification_draft' must return a JSON-RPC error, got: {}",
        body
    );
    let error_msg = body["error"]["message"]
        .as_str()
        .expect("error.message must be a string");
    assert!(
        error_msg.to_lowercase().contains("forbidden"),
        "error message must mention FORBIDDEN, got: {}",
        error_msg
    );

    // No artifact should have been created.
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM artifacts WHERE workspace_id = $1")
        .bind(ws_id)
        .fetch_one(&pool)
        .await
        .expect("artifact count query failed");
    assert_eq!(
        count, 0,
        "no artifact must be created for forbidden kind, got count={}",
        count
    );
}

// ─── Test 8: deliver_notification invokes connector ────────────────────────────

/// deliver_notification → wiremock receives POST; audit row written with actor_kind=peer for bearer.
#[tokio::test]
#[ignore]
async fn mcp_tools_call_deliver_notification_invokes_connector() {
    let (base, pool) = spawn_app().await;

    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .expect(1)
        .mount(&mock_server)
        .await;

    let user_id = default_user_id(&pool).await;
    let org_id = default_org_id(&pool).await;
    let cookie = make_session_cookie(user_id).await;

    let ws_id = insert_workspace(&pool, org_id, "Deliver Test WS").await;

    let connector_id: Uuid = sqlx::query_scalar(
        "INSERT INTO connectors (workspace_id, kind, name, config)
         VALUES ($1, 'rust_native'::connector_kind, 'slack',
                 jsonb_build_object('webhook_url', $2))
         RETURNING id",
    )
    .bind(ws_id)
    .bind(format!("{}/", mock_server.uri()))
    .fetch_one(&pool)
    .await
    .expect("insert connector failed");

    let resp = mcp_post_with_cookie(
        &base,
        json!({
            "jsonrpc": "2.0",
            "id": 9,
            "method": "tools/call",
            "params": {
                "name": "deliver_notification",
                "arguments": {
                    "workspace_id": ws_id.to_string(),
                    "connector_id": connector_id.to_string(),
                    "text": "MCP delivery test message"
                }
            }
        }),
        &cookie,
    )
    .await;

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "deliver_notification must return 200"
    );

    let body: Value = resp.json().await.expect("response not JSON");
    assert!(
        body["error"].is_null(),
        "deliver_notification must not error, got: {}",
        body["error"]
    );

    // Wiremock: exactly 1 POST.
    mock_server.verify().await;

    // Audit row: verb='delivered', actor_kind='user' (cookie session, not bearer).
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT actor_kind::TEXT, verb FROM audit_events
         WHERE workspace_id = $1 AND verb = 'delivered'
         LIMIT 1",
    )
    .bind(ws_id)
    .fetch_optional(&pool)
    .await
    .expect("audit query failed");

    let (actor_kind, verb) = row.expect("deliver_notification must write an audit row");
    assert_eq!(
        verb, "delivered",
        "audit.verb must be 'delivered', got: {verb}"
    );
    // Cookie session (not bearer JWT) → actor_kind='user'
    assert_eq!(
        actor_kind, "user",
        "cookie-authenticated call must have actor_kind='user', got: {actor_kind}"
    );
}

// ─── Test 9: unauthenticated request rejected by /mcp middleware ──────────────

/// No bearer token → the /mcp middleware rejects the request before JSON-RPC dispatch.
#[tokio::test]
#[ignore]
async fn mcp_unauthenticated_request_is_rejected() {
    let (base, _pool) = spawn_app().await;

    // No cookie, no bearer.
    let resp = mcp_post_unauthenticated(
        &base,
        json!({
            "jsonrpc": "2.0",
            "id": 10,
            "method": "tools/call",
            "params": { "name": "list_workspaces", "arguments": {} }
        }),
    )
    .await;

    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "unauthenticated /mcp requests must be rejected by bearer middleware"
    );

    let body: Value = resp.json().await.expect("response not JSON");
    assert_eq!(
        body["error"], "unauthorized",
        "unauthenticated /mcp must return the standard unauthorized envelope"
    );
}

// ─── Test 10: static bearer is accepted ──────────────────────────────────────

/// The CI/headless static bearer escape hatch authenticates /mcp requests.
#[tokio::test]
#[ignore]
async fn mcp_static_bearer_is_accepted() {
    let (base, _pool) = spawn_app().await;

    let resp = mcp_post_with_bearer(
        &base,
        json!({
            "jsonrpc": "2.0",
            "id": 11,
            "method": "tools/call",
            "params": { "name": "list_workspaces", "arguments": {} }
        }),
        TEST_STATIC_BEARER,
    )
    .await;

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "static bearer must return 200"
    );

    let body: Value = resp.json().await.expect("response not JSON");
    assert!(
        body["error"].is_null(),
        "static bearer must not produce a JSON-RPC error, got: {}",
        body["error"]
    );
    assert!(
        !body["result"].is_null(),
        "static bearer must return a result, got: {}",
        body
    );
}

// ─── Test 11: bearer JWT from untrusted issuer is rejected ───────────────────

/// A bearer value that is not an issued OAuth token and does not match the static bearer is rejected.
#[tokio::test]
#[ignore]
async fn mcp_bearer_jwt_from_untrusted_issuer_is_rejected() {
    let (base, _pool) = spawn_app().await;

    let issuer_url = "https://untrusted.issuer.local";
    let audience = "ione-mcp";
    let wrong_secret: Vec<u8> = (100u8..132).collect(); // different from registered secret

    let token = mint_jwt("bad-actor", issuer_url, audience, &wrong_secret);

    let resp = mcp_post_with_bearer(
        &base,
        json!({
            "jsonrpc": "2.0",
            "id": 12,
            "method": "tools/call",
            "params": { "name": "list_workspaces", "arguments": {} }
        }),
        &token,
    )
    .await;

    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "invalid bearer values must be rejected by bearer middleware"
    );

    let body: Value = resp.json().await.expect("response not JSON");
    assert_eq!(
        body["error"], "unauthorized",
        "invalid bearer must return the standard unauthorized envelope"
    );
}

// ─── Test 12: schema validation on invalid args ───────────────────────────────

/// tools/call for list_survivors without workspace_id → MCP schema-validation error.
#[tokio::test]
#[ignore]
async fn mcp_schema_validation_on_invalid_args() {
    let (base, pool) = spawn_app().await;

    let user_id = default_user_id(&pool).await;
    let cookie = make_session_cookie(user_id).await;

    let resp = mcp_post_with_cookie(
        &base,
        json!({
            "jsonrpc": "2.0",
            "id": 13,
            "method": "tools/call",
            "params": {
                "name": "list_survivors",
                "arguments": {}
                // workspace_id is intentionally missing
            }
        }),
        &cookie,
    )
    .await;

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "schema validation errors are returned in JSON-RPC body with HTTP 200"
    );

    let body: Value = resp.json().await.expect("response not JSON");
    assert!(
        !body["error"].is_null(),
        "missing workspace_id must produce a JSON-RPC error, got: {}",
        body
    );

    let error = &body["error"];
    let code = error["code"].as_i64().unwrap_or(0);
    assert_eq!(
        code, -32602,
        "schema validation error must have code -32602 (invalid params), got: {}",
        code
    );

    let msg = error["message"].as_str().unwrap_or("");
    assert!(
        msg.contains("workspace_id"),
        "error message must mention 'workspace_id', got: {}",
        msg
    );
}

// ─── Mutation check ───────────────────────────────────────────────────────────
//
// - Mutant: tools/list omits 'propose_artifact'
//   → caught by mcp_tools_list_returns_all_tools (names.contains check) ✓
//
// - Mutant: tools/call reaches JSON-RPC without bearer auth
//   → caught by mcp_unauthenticated_request_is_rejected (expects HTTP 401) ✓
//
// - Mutant: propose_artifact allows notification_draft kind
//   → caught by mcp_tools_call_propose_artifact_rejects_forbidden_kind
//     (error.is_null() fails; artifact count assert_eq!(0) fails) ✓
//
// - Mutant: list_survivors ignores workspace_id filter (returns all survivors)
//   → caught by mcp_tools_call_list_survivors_filters_by_workspace
//     (workspace B assert_eq!(0) fails) ✓
//
// - Mutant: invalid bearer value is accepted
//   → caught by mcp_bearer_jwt_from_untrusted_issuer_is_rejected
//     (expects HTTP 401) ✓
//
// - Mutant: schema validation skipped (workspace_id not checked)
//   → caught by mcp_schema_validation_on_invalid_args
//     (error.is_null() fails) ✓
//
// - Mutant: deliver_notification does not write audit row
//   → caught by mcp_tools_call_deliver_notification_invokes_connector
//     (audit row.expect panics) ✓
