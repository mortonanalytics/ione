//! Issue #15 — MCP peer-client conformance against a spec-conforming server.
//!
//! The acceptance clause "tool invocation and resource reads work over HTTP
//! against *any* spec-conforming MCP server" cannot be pinned against a public
//! reference server: CI runs offline. Instead this suite stands up a strict stub
//! that deliberately exercises the parts of the streamable-HTTP transport IONe
//! used to assume away — SSE-framed POST replies, `Accept` negotiation,
//! `MCP-Protocol-Version`, the `notifications/initialized` lifecycle step,
//! JSON-RPC id correlation, and cursor pagination that terminates with an
//! explicit `nextCursor: null`.
//!
//! Every stub reply is SSE-framed. Where a reply is preceded by an unrelated
//! server-initiated notification frame, the client must still select its own
//! reply by id rather than grabbing the first frame on the stream.
//!
//! Run:
//!   DATABASE_URL=postgres://ione:ione@localhost:5433/ione_w15 \
//!     SQLX_OFFLINE=true IONE_SKIP_LIVE=1 \
//!     cargo test --test mcp_conformance_integration -- --ignored --test-threads=1

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ione::{auth::AuthContext, repos::PeerRepo, state::AppState};
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;
use uuid::Uuid;

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione_w15";
const TEST_STATIC_BEARER: &str = "mcp-conformance-bearer";
const TEST_TOKEN_KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
const PROTOCOL_VERSION: &str = "2025-11-25";

// ─── Spec-conforming stub MCP server ──────────────────────────────────────────

/// How the stub's final pagination page signals "no more pages".
#[derive(Clone, Copy, PartialEq)]
enum Terminator {
    /// `"nextCursor": null` — permitted by app-integration-contract-v1 §8.1.
    Null,
    /// `"nextCursor": ""` — an empty opaque cursor, also terminal.
    EmptyString,
    /// The `nextCursor` key is simply absent.
    Absent,
}

#[derive(Clone)]
struct StubConfig {
    /// Enforce the `MCP-Session-Id` header on everything except `initialize`.
    /// Sessions are optional in the MCP spec, so both settings are conforming;
    /// the session-requiring mode is what drives IONe through the handshake.
    require_session: bool,
    /// JSON-RPC method for which the stub answers with a deliberately wrong id.
    mismatch_id_for: Option<String>,
    terminator: Terminator,
    /// Answer with `application/json` instead of SSE framing. Both are legal;
    /// this is the shape IONe-to-IONe federation already relies on, so it is
    /// exercised alongside SSE rather than replaced by it.
    plain_json: bool,
}

impl Default for StubConfig {
    fn default() -> Self {
        Self {
            require_session: false,
            mismatch_id_for: None,
            terminator: Terminator::Null,
            plain_json: false,
        }
    }
}

/// One observed inbound request, as the stub saw it on the wire.
#[derive(Clone, Debug)]
struct Recorded {
    /// JSON-RPC method, or `"GET"` for the long-lived notification stream.
    method: String,
    accept: Option<String>,
    protocol_version: Option<String>,
    session_id: Option<String>,
    cursor: Option<String>,
    id: Value,
}

struct Stub {
    config: StubConfig,
    requests: Mutex<Vec<Recorded>>,
    sessions: Mutex<HashSet<String>>,
}

struct StubHandle {
    mcp_url: String,
    stub: Arc<Stub>,
}

impl StubHandle {
    fn requests(&self) -> Vec<Recorded> {
        self.stub.requests.lock().expect("stub mutex").clone()
    }

    fn methods(&self) -> Vec<String> {
        self.requests().into_iter().map(|r| r.method).collect()
    }

    fn count_of(&self, method: &str) -> usize {
        self.methods().iter().filter(|m| *m == method).count()
    }

    fn calls_to(&self, method: &str) -> Vec<Recorded> {
        self.requests()
            .into_iter()
            .filter(|r| r.method == method)
            .collect()
    }
}

async fn spawn_stub(config: StubConfig) -> StubHandle {
    let stub = Arc::new(Stub {
        config,
        requests: Mutex::new(Vec::new()),
        sessions: Mutex::new(HashSet::new()),
    });

    let app = axum::Router::new()
        .route("/mcp", axum::routing::post(stub_post).get(stub_get))
        .with_state(Arc::clone(&stub));

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind stub");
    let addr = listener.local_addr().expect("stub addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("stub server");
    });

    StubHandle {
        mcp_url: format!("http://{addr}/mcp"),
        stub,
    }
}

/// The long-lived notification stream. Held open for the life of the test so a
/// boot-started session does not reconnect-loop and inflate request counts.
async fn stub_get(
    axum::extract::State(stub): axum::extract::State<Arc<Stub>>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    record(&stub, "GET", &headers, &Value::Null, None);
    let body = axum::body::Body::from_stream(futures_util::stream::pending::<
        Result<Vec<u8>, std::io::Error>,
    >());
    axum::response::Response::builder()
        .status(200)
        .header("content-type", "text/event-stream")
        .body(body)
        .expect("stub sse response")
}

async fn stub_post(
    axum::extract::State(stub): axum::extract::State<Arc<Stub>>,
    headers: axum::http::HeaderMap,
    body: String,
) -> axum::response::Response {
    let body: Value = serde_json::from_str(&body).expect("stub received non-JSON body");
    let method = body["method"].as_str().unwrap_or_default().to_string();
    let cursor = body["params"]["cursor"].as_str().map(str::to_string);
    record(&stub, &method, &headers, &body["id"], cursor.clone());

    // A notification carries no id and gets no JSON-RPC reply, only 202.
    if method.starts_with("notifications/") {
        return axum::response::Response::builder()
            .status(202)
            .body(axum::body::Body::empty())
            .expect("stub 202");
    }

    let id = body["id"].clone();
    let plain = stub.config.plain_json;

    if method == "initialize" {
        let session_id = Uuid::new_v4().to_string();
        stub.sessions
            .lock()
            .expect("stub sessions")
            .insert(session_id.clone());
        // The session id is delivered ONLY via the header, the way a spec
        // server does it — not as a `sessionId` field in the result.
        return jsonrpc_reply(
            plain,
            &id,
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "serverInfo": { "name": "conformance-stub", "version": "1" },
                "capabilities": { "tools": {}, "resources": {} }
            }),
            Some(&session_id),
        );
    }

    if stub.config.require_session {
        let presented = header_str(&headers, "mcp-session-id");
        let known = presented
            .as_deref()
            .map(|value| stub.sessions.lock().expect("stub sessions").contains(value))
            .unwrap_or(false);
        if !known {
            return jsonrpc_error(
                plain,
                &id,
                -32002,
                "Bad Request: Mcp-Session-Id header is required",
            );
        }
    }

    let reply_id = match stub.config.mismatch_id_for.as_deref() {
        // Deliberately answer a different call than the one that was made.
        Some(target) if target == method => json!(999_999_999u64),
        _ => id.clone(),
    };

    match method.as_str() {
        "tools/list" => jsonrpc_reply(
            plain,
            &reply_id,
            page(&stub.config, "tools", cursor.as_deref()),
            None,
        ),
        "resources/list" => jsonrpc_reply(
            plain,
            &reply_id,
            page(&stub.config, "resources", cursor.as_deref()),
            None,
        ),
        "tools/call" => {
            let name = body["params"]["name"].as_str().unwrap_or_default();
            jsonrpc_reply(
                plain,
                &reply_id,
                json!({
                    "content": [{ "type": "text", "text": json!({ "invoked": name }).to_string() }],
                    "isError": false
                }),
                None,
            )
        }
        "resources/read" => {
            let uri = body["params"]["uri"].as_str().unwrap_or_default();
            jsonrpc_reply(
                plain,
                &reply_id,
                json!({
                    "contents": [{
                        "uri": uri,
                        "mimeType": "application/json",
                        "text": json!({ "summary": "stub slice body" }).to_string()
                    }]
                }),
                None,
            )
        }
        other => jsonrpc_error(plain, &id, -32601, &format!("method not found: {other}")),
    }
}

/// Two-page listings. Page one hands back a cursor; page two terminates using
/// whichever terminator the test configured.
fn page(config: &StubConfig, field: &str, cursor: Option<&str>) -> Value {
    let page_two_cursor = format!("{field}-page-2");
    if cursor.is_none() {
        let items = match field {
            "tools" => json!([
                { "name": "alpha", "description": "first", "inputSchema": { "type": "object" } },
                { "name": "beta", "description": "second", "inputSchema": { "type": "object" } }
            ]),
            _ => json!([{ "name": "res_one", "uri": "stub://one", "description": "first" }]),
        };
        return json!({ field: items, "nextCursor": page_two_cursor });
    }

    let items = match field {
        "tools" => json!([
            { "name": "gamma", "description": "third", "inputSchema": { "type": "object" } }
        ]),
        _ => json!([{ "name": "res_two", "uri": "stub://two", "description": "second" }]),
    };
    let mut result = json!({ field: items });
    match config.terminator {
        Terminator::Null => result["nextCursor"] = Value::Null,
        Terminator::EmptyString => result["nextCursor"] = Value::String(String::new()),
        Terminator::Absent => {}
    }
    result
}

fn record(
    stub: &Arc<Stub>,
    method: &str,
    headers: &axum::http::HeaderMap,
    id: &Value,
    cursor: Option<String>,
) {
    stub.requests.lock().expect("stub mutex").push(Recorded {
        method: method.to_string(),
        accept: header_str(headers, "accept"),
        protocol_version: header_str(headers, "mcp-protocol-version"),
        session_id: header_str(headers, "mcp-session-id"),
        cursor,
        id: id.clone(),
    });
}

fn header_str(headers: &axum::http::HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

/// A JSON-RPC reply in whichever framing the stub is configured for.
///
/// In SSE framing the reply is preceded by an unrelated server-initiated
/// notification: a client that returns the first `data:` frame instead of the
/// frame matching its request id will pick up the notification and fail.
fn jsonrpc_reply(
    plain: bool,
    id: &Value,
    result: Value,
    session_id: Option<&str>,
) -> axum::response::Response {
    framed(
        plain,
        json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        session_id,
    )
}

fn jsonrpc_error(plain: bool, id: &Value, code: i64, message: &str) -> axum::response::Response {
    framed(
        plain,
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": code, "message": message }
        }),
        None,
    )
}

fn framed(plain: bool, message: Value, session_id: Option<&str>) -> axum::response::Response {
    let (content_type, body) = if plain {
        ("application/json", message.to_string())
    } else {
        let distractor = json!({
            "jsonrpc": "2.0",
            "method": "notifications/message",
            "params": { "level": "info", "data": "unrelated server-initiated frame" }
        });
        (
            "text/event-stream",
            format!("event: message\ndata: {distractor}\n\nevent: message\ndata: {message}\n\n"),
        )
    };
    let mut builder = axum::response::Response::builder()
        .status(200)
        .header("content-type", content_type);
    if let Some(session_id) = session_id {
        builder = builder.header("MCP-Session-Id", session_id);
    }
    builder
        .body(axum::body::Body::from(body))
        .expect("stub reply")
}

// ─── Harness ──────────────────────────────────────────────────────────────────

async fn setup_pool() -> PgPool {
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
        "TRUNCATE peer_catalog_entries, pending_peer_tool_calls, webhook_events_seen,
                  workspace_peer_bindings, audit_events, approvals, artifacts,
                  peers, trust_issuers, routing_decisions, survivors, signals,
                  stream_events, streams, connectors,
                  memberships, roles, messages, conversations,
                  workspaces, users, organizations
         RESTART IDENTITY CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate failed");

    pool
}

async fn spawn_state() -> (PgPool, AppState) {
    let pool = setup_pool().await;
    let (_router, state) = ione::app_with_state(pool.clone()).await;
    (pool, state)
}

async fn default_org_id(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("SELECT id FROM organizations WHERE name = 'Default Org' LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("default org")
}

async fn default_workspace_id(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("SELECT id FROM workspaces WHERE name = 'Operations' LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("Operations workspace")
}

async fn default_user_id(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("SELECT id FROM users WHERE email = 'default@localhost' LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("default user")
}

async fn insert_trust_issuer(pool: &PgPool, org_id: Uuid, issuer_url: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO trust_issuers (org_id, issuer_url, audience, jwks_uri, claim_mapping)
         VALUES ($1, $2, 'mcp', 'secret:test', '{}'::jsonb)
         RETURNING id",
    )
    .bind(org_id)
    .bind(issuer_url)
    .fetch_one(pool)
    .await
    .expect("insert trust issuer")
}

/// An Active peer registered by MCP endpoint URL — the issue's registration path.
async fn insert_active_peer(pool: &PgPool, org_id: Uuid, name: &str, mcp_url: &str) -> Uuid {
    let issuer_id = insert_trust_issuer(pool, org_id, &format!("https://{name}.issuer.test")).await;
    sqlx::query_scalar(
        "INSERT INTO peers
           (org_id, name, mcp_url, issuer_id, sharing_policy, tool_allowlist, status, tool_prefix)
         VALUES ($1, $2, $3, $4, '{}'::jsonb, '[]'::jsonb, 'active'::peer_status, $2)
         RETURNING id",
    )
    .bind(org_id)
    .bind(name)
    .bind(mcp_url)
    .bind(issuer_id)
    .fetch_one(pool)
    .await
    .expect("insert peer")
}

async fn bind_peer(pool: &PgPool, workspace_id: Uuid, peer_id: Uuid) {
    sqlx::query(
        "INSERT INTO workspace_peer_bindings
           (workspace_id, peer_id, foreign_tenant_id, foreign_workspace_id, status)
         VALUES ($1, $2, 'conformance-tenant', 'conformance-ws', 'active'::binding_status)",
    )
    .bind(workspace_id)
    .bind(peer_id)
    .execute(pool)
    .await
    .expect("insert binding");
}

async fn grant(pool: &PgPool, workspace_id: Uuid, permissions: Value) {
    sqlx::query("UPDATE roles SET permissions = $2 WHERE workspace_id = $1 AND name = 'member'")
        .bind(workspace_id)
        .bind(permissions)
        .execute(pool)
        .await
        .expect("grant permissions");
}

async fn auth_for(pool: &PgPool) -> AuthContext {
    AuthContext {
        user_id: default_user_id(pool).await,
        org_id: default_org_id(pool).await,
        is_oidc: false,
        is_mcp_peer: false,
        active_role_id: None,
        session_id: None,
        mfa_verified: true,
        is_service_account: false,
        service_account_token_id: None,
        permissions: Vec::new(),
    }
}

/// Every POST must advertise both response content types and the protocol
/// revision. Asserted over the whole recorded transcript, not one sampled call.
fn assert_transport_headers(handle: &StubHandle) {
    let posts: Vec<Recorded> = handle
        .requests()
        .into_iter()
        .filter(|r| r.method != "GET")
        .collect();
    assert!(!posts.is_empty(), "stub recorded no POSTs");
    for request in posts {
        let accept = request
            .accept
            .unwrap_or_else(|| panic!("POST {} sent no Accept header", request.method));
        assert!(
            accept.contains("application/json"),
            "POST {} Accept must advertise application/json, got {accept}",
            request.method
        );
        assert!(
            accept.contains("text/event-stream"),
            "POST {} Accept must advertise text/event-stream, got {accept}",
            request.method
        );
        assert_eq!(
            request.protocol_version.as_deref(),
            Some(PROTOCOL_VERSION),
            "POST {} must carry MCP-Protocol-Version",
            request.method
        );
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

/// Gaps 1-4: the client negotiates both content types, parses an SSE-framed
/// POST reply, completes the `initialize` → `notifications/initialized`
/// lifecycle, and stamps `MCP-Protocol-Version` on every POST.
#[tokio::test]
#[ignore]
async fn handshake_completes_lifecycle_over_sse_framed_replies() {
    let handle = spawn_stub(StubConfig {
        require_session: true,
        ..StubConfig::default()
    })
    .await;
    let (pool, state) = spawn_state().await;
    let org_id = default_org_id(&pool).await;
    let workspace_id = default_workspace_id(&pool).await;
    let peer_id = insert_active_peer(&pool, org_id, "conform", &handle.mcp_url).await;
    bind_peer(&pool, workspace_id, peer_id).await;
    grant(&pool, workspace_id, json!(["tool_invoke:conform:alpha"])).await;
    let auth = auth_for(&pool).await;

    let result = ione::services::federation::route_tool_call(
        &state,
        workspace_id,
        "conform:alpha",
        json!({ "q": 1 }),
        &auth,
    )
    .await
    .expect("tools/call against a session-requiring stub must succeed");
    assert_eq!(
        result["content"][0]["text"].as_str(),
        Some(r#"{"invoked":"alpha"}"#),
        "the SSE-framed reply body must be what round-trips back"
    );

    assert_transport_headers(&handle);

    let methods = handle.methods();
    let initialize_at = methods
        .iter()
        .position(|m| m == "initialize")
        .expect("gap 3: client must call initialize");
    let initialized_at = methods
        .iter()
        .position(|m| m == "notifications/initialized")
        .expect("gap 3: client must send notifications/initialized after initialize");
    assert!(
        initialized_at > initialize_at,
        "notifications/initialized must follow initialize, saw {methods:?}"
    );

    // The successful tools/call must have presented the negotiated session.
    let session_calls: Vec<Recorded> = handle
        .calls_to("tools/call")
        .into_iter()
        .filter(|r| r.session_id.is_some())
        .collect();
    assert!(
        !session_calls.is_empty(),
        "the retried tools/call must present MCP-Session-Id, saw {methods:?}"
    );
}

/// Gap 7: an explicit `nextCursor: null` terminates pagination. Asserting only
/// the item count would let request amplification regress silently, so the
/// number of list calls is pinned too.
#[tokio::test]
#[ignore]
async fn null_next_cursor_terminates_pagination_without_amplification() {
    let handle = spawn_stub(StubConfig {
        terminator: Terminator::Null,
        ..StubConfig::default()
    })
    .await;
    let (pool, state) = spawn_state().await;
    let org_id = default_org_id(&pool).await;
    let peer_id = insert_active_peer(&pool, org_id, "paged", &handle.mcp_url).await;
    let auth = auth_for(&pool).await;

    let manifest = ione::services::federation::force_refresh_manifest(&state, peer_id, &auth)
        .await
        .expect("manifest fetch");

    let tool_names: Vec<&str> = manifest
        .tools
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert_eq!(
        tool_names,
        vec!["alpha", "beta", "gamma"],
        "both cursor pages must be consumed exactly once"
    );
    let resource_names: Vec<&str> = manifest
        .resources
        .iter()
        .filter_map(|item| item["name"].as_str())
        .collect();
    assert_eq!(resource_names, vec!["res_one", "res_two"]);

    assert_eq!(
        handle.count_of("tools/list"),
        2,
        "nextCursor: null must end paging; saw {:?}",
        handle.methods()
    );
    assert_eq!(
        handle.count_of("resources/list"),
        2,
        "nextCursor: null must end paging; saw {:?}",
        handle.methods()
    );

    // Page two must have been requested with the cursor page one handed back,
    // and no page must have been requested with a null/absent cursor twice.
    let cursors: Vec<Option<String>> = handle
        .calls_to("tools/list")
        .into_iter()
        .map(|r| r.cursor)
        .collect();
    assert_eq!(cursors, vec![None, Some("tools-page-2".to_string())]);
}

/// An empty-string cursor, and an absent `nextCursor` key, are equally terminal;
/// neither may be treated as a continuation.
#[tokio::test]
#[ignore]
async fn empty_and_absent_next_cursor_terminate_pagination() {
    for (label, terminator) in [
        ("emptycur", Terminator::EmptyString),
        ("nocur", Terminator::Absent),
    ] {
        let handle = spawn_stub(StubConfig {
            terminator,
            ..StubConfig::default()
        })
        .await;
        let (pool, state) = spawn_state().await;
        let org_id = default_org_id(&pool).await;
        let peer_id = insert_active_peer(&pool, org_id, label, &handle.mcp_url).await;
        let auth = auth_for(&pool).await;

        let manifest = ione::services::federation::force_refresh_manifest(&state, peer_id, &auth)
            .await
            .expect("manifest fetch");

        assert_eq!(manifest.tools.len(), 3, "{label}: both pages must be read");
        assert_eq!(
            handle.count_of("tools/list"),
            2,
            "{label}: terminal cursor must end paging; saw {:?}",
            handle.methods()
        );
    }
}

/// Gap 5's other half: ids must actually be unique per request. A client that
/// hardcodes one id has nothing to correlate against, so the rejection above
/// would be vacuous.
#[tokio::test]
#[ignore]
async fn every_outbound_request_carries_a_distinct_jsonrpc_id() {
    let handle = spawn_stub(StubConfig::default()).await;
    let (pool, state) = spawn_state().await;
    let org_id = default_org_id(&pool).await;
    let peer_id = insert_active_peer(&pool, org_id, "uniqids", &handle.mcp_url).await;
    let auth = auth_for(&pool).await;

    ione::services::federation::force_refresh_manifest(&state, peer_id, &auth)
        .await
        .expect("manifest fetch");

    let ids: Vec<Value> = handle
        .requests()
        .into_iter()
        .filter(|r| r.method != "GET" && !r.method.starts_with("notifications/"))
        .map(|r| r.id)
        .collect();
    assert!(ids.len() >= 4, "expected the four list calls, saw {ids:?}");
    let unique: HashSet<String> = ids.iter().map(Value::to_string).collect();
    assert_eq!(
        unique.len(),
        ids.len(),
        "each outbound JSON-RPC request needs its own id, saw {ids:?}"
    );
}

/// Gap 5: a reply carrying an id other than the one requested is refused rather
/// than handed back as this call's result. Checked in both response framings —
/// the SSE path selects by id across frames, the plain-JSON path compares the
/// single reply's id.
#[tokio::test]
#[ignore]
async fn reply_with_mismatched_id_is_rejected() {
    for (label, plain_json) in [("wrongidsse", false), ("wrongidjson", true)] {
        let handle = spawn_stub(StubConfig {
            mismatch_id_for: Some("tools/call".to_string()),
            plain_json,
            ..StubConfig::default()
        })
        .await;
        let (pool, state) = spawn_state().await;
        let org_id = default_org_id(&pool).await;
        let workspace_id = default_workspace_id(&pool).await;
        let peer_id = insert_active_peer(&pool, org_id, label, &handle.mcp_url).await;
        bind_peer(&pool, workspace_id, peer_id).await;
        grant(
            &pool,
            workspace_id,
            json!([format!("tool_invoke:{label}:alpha")]),
        )
        .await;
        let auth = auth_for(&pool).await;

        let err = ione::services::federation::route_tool_call(
            &state,
            workspace_id,
            &format!("{label}:alpha"),
            json!({}),
            &auth,
        )
        .await
        .expect_err("a reply for a different request id must not be accepted");

        let message = format!("{err:#}");
        assert!(
            message.contains("request id"),
            "{label}: mismatched id must surface as a correlation failure, got: {message}"
        );
        assert!(
            !message.contains("invoked"),
            "{label}: the mis-attributed payload must not leak into the result, got: {message}"
        );
    }
}

/// Backward compatibility: a peer that answers with plain `application/json` —
/// which is what IONe-to-IONe federation does today — must keep working. The SSE
/// tolerance added for conformance is additive, not a format switch.
#[tokio::test]
#[ignore]
async fn plain_json_replies_still_round_trip() {
    let handle = spawn_stub(StubConfig {
        plain_json: true,
        ..StubConfig::default()
    })
    .await;
    let (pool, state) = spawn_state().await;
    let org_id = default_org_id(&pool).await;
    let workspace_id = default_workspace_id(&pool).await;
    let peer_id = insert_active_peer(&pool, org_id, "plainjson", &handle.mcp_url).await;
    bind_peer(&pool, workspace_id, peer_id).await;
    grant(&pool, workspace_id, json!(["tool_invoke:plainjson:alpha"])).await;
    let auth = auth_for(&pool).await;

    let manifest = ione::services::federation::force_refresh_manifest(&state, peer_id, &auth)
        .await
        .expect("manifest fetch over plain JSON");
    assert_eq!(manifest.tools.len(), 3);
    assert_eq!(handle.count_of("tools/list"), 2);

    let result = ione::services::federation::route_tool_call(
        &state,
        workspace_id,
        "plainjson:alpha",
        json!({}),
        &auth,
    )
    .await
    .expect("tools/call over plain JSON");
    assert_eq!(
        result["content"][0]["text"].as_str(),
        Some(r#"{"invoked":"alpha"}"#)
    );
}

/// The other half of the acceptance clause: a resource read round-trips over an
/// SSE-framed reply and yields the peer's body, not the manifest fallback.
#[tokio::test]
#[ignore]
async fn resource_read_round_trips_over_sse() {
    let handle = spawn_stub(StubConfig::default()).await;
    let (pool, state) = spawn_state().await;
    let org_id = default_org_id(&pool).await;
    let peer_id = insert_active_peer(&pool, org_id, "resread", &handle.mcp_url).await;
    let peer = PeerRepo::new(pool.clone())
        .get(peer_id)
        .await
        .expect("peer lookup")
        .expect("peer row");

    let slice = ione::services::federation::fetch_slice(&state, &peer)
        .await
        .expect("resources/read");

    assert_eq!(
        slice.body["summary"].as_str(),
        Some("stub slice body"),
        "resources/read must return the peer's body, not the manifest fallback"
    );
    assert_eq!(handle.count_of("resources/read"), 1);
    assert_transport_headers(&handle);
}

/// Gap 6: peer registration is workspace-scoped and survives restart, and so
/// must the long-lived notification session. A fresh app built over a database
/// that already holds an Active peer must open its SSE stream at boot without
/// an operator hitting the reconnect endpoint.
#[tokio::test]
#[ignore]
async fn active_peer_sse_session_starts_at_boot() {
    let handle = spawn_stub(StubConfig::default()).await;
    // First boot: bootstraps the org/workspace, and starts no sessions because
    // no peer is registered yet.
    let (pool, _state) = spawn_state().await;
    let org_id = default_org_id(&pool).await;
    let workspace_id = default_workspace_id(&pool).await;
    let peer_id = insert_active_peer(&pool, org_id, "booted", &handle.mcp_url).await;
    bind_peer(&pool, workspace_id, peer_id).await;
    assert_eq!(
        handle.count_of("GET"),
        0,
        "registering a peer must not itself open a stream"
    );

    // Peer registration must be readable from storage alone — this is the
    // "survives restart" precondition the session start then builds on.
    let stored: (String, String) =
        sqlx::query_as("SELECT mcp_url, status::TEXT FROM peers WHERE id = $1")
            .bind(peer_id)
            .fetch_one(&pool)
            .await
            .expect("peer row survives");
    assert_eq!(stored.0, handle.mcp_url);
    assert_eq!(stored.1, "active");

    // Restart: build a new app over the same database.
    let (_router, _restarted) = ione::app_with_state(pool.clone()).await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if handle.count_of("GET") > 0 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "boot must start an SSE session for an Active peer; saw {:?}",
            handle.methods()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // The session opens with the full handshake before the stream GET.
    let methods = handle.methods();
    assert!(
        methods.iter().any(|m| m == "initialize"),
        "boot session must initialize, saw {methods:?}"
    );
    assert!(
        methods.iter().any(|m| m == "notifications/initialized"),
        "boot session must send notifications/initialized, saw {methods:?}"
    );

    let stream_get = handle
        .calls_to("GET")
        .into_iter()
        .next()
        .expect("recorded GET");
    assert_eq!(
        stream_get.protocol_version.as_deref(),
        Some(PROTOCOL_VERSION)
    );
    assert!(
        stream_get.session_id.is_some(),
        "the notification stream must present the negotiated session id"
    );
}
