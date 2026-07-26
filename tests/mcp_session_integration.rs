//! Outbound MCP session reuse, connector-side `tools/list` pagination, and what
//! counts as a peer-authored context slice.
//!
//! Three things that all live on the same wire and all used to be wrong in the
//! same direction — IONe treating one round trip's worth of evidence as if it
//! were the whole story:
//!
//!   * `federation::send_jsonrpc` initialized *reactively on every call* and
//!     threw the returned `MCP-Session-Id` away, so against a session-enforcing
//!     peer every single call cost a failed request, an `initialize`, a
//!     `notifications/initialized` and a retry — and left another session open on
//!     the peer that IONe never released.
//!   * `connectors::mcp_client::default_streams` read page one of `tools/list`
//!     and stopped, so a readable tool on page two got no derived stream.
//!   * `federation::peer_authored_slice` turned an unparsable or missing slice
//!     body into `Ok({})`, which the catalog then treated as the peer having
//!     removed every `sample_queries` entry.
//!
//! Run (serial, ignored):
//!   DATABASE_URL=postgres://ione:ione@localhost:5433/ione_w15 \
//!     SQLX_OFFLINE=true IONE_SKIP_LIVE=1 \
//!     cargo test --test mcp_session_integration -- --ignored --test-threads=1

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::http::{HeaderMap, StatusCode};
use chrono::{DateTime, Utc};
use ione::auth::AuthContext;
use ione::connectors::mcp_client::McpClientConnector;
use ione::connectors::ConnectorImpl;
use ione::repos::PeerRepo;
use ione::state::AppState;
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;
use uuid::Uuid;

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione_w15";
const TEST_TOKEN_KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
const TEST_STATIC_BEARER: &str = "mcp-session-bearer";

// ─── harness ──────────────────────────────────────────────────────────────────

async fn spawn_state() -> (PgPool, AppState) {
    std::env::set_var("IONE_AUTH_MODE", "local");
    std::env::set_var("IONE_TOKEN_KEY", TEST_TOKEN_KEY);
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
        "TRUNCATE workspace_peer_delegations, workspace_peer_delegation_pending,
                  workspace_peer_credentials, pending_peer_tool_calls,
                  service_account_tokens, org_memberships, webhook_events_seen,
                  workspace_peer_bindings, interaction_events, audit_events,
                  pipeline_events, peer_catalog_entries,
                  approvals, artifacts, trust_issuers, peers,
                  routing_decisions, survivors, signals,
                  stream_events, streams, connectors,
                  memberships, roles, messages, conversations,
                  workspaces, users, organizations
         RESTART IDENTITY CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate failed");

    let (_router, state) = ione::app_with_state(pool.clone()).await;
    (pool, state)
}

/// A peer that enforces `MCP-Session-Id` the way the streamable-HTTP spec says
/// to: `initialize` mints a session and returns it in the header, every other
/// POST must present a session the server still knows, and one it does not know
/// is answered with **HTTP 404** — the transport-level "your session is gone"
/// signal, not a JSON-RPC error.
///
/// `DELETE` is accepted and recorded, so a client that opens sessions without
/// ever releasing them is visible as a count.
struct SessionPeer {
    mcp_url: String,
    methods: Arc<Mutex<Vec<String>>>,
    live_sessions: Arc<Mutex<HashSet<String>>>,
    deleted_sessions: Arc<Mutex<Vec<String>>>,
    minted_sessions: Arc<Mutex<Vec<String>>>,
}

impl SessionPeer {
    fn methods(&self) -> Vec<String> {
        self.methods.lock().expect("method log").clone()
    }

    fn count_of(&self, method: &str) -> usize {
        self.methods().iter().filter(|m| *m == method).count()
    }

    fn minted(&self) -> Vec<String> {
        self.minted_sessions.lock().expect("minted").clone()
    }

    fn deleted(&self) -> Vec<String> {
        self.deleted_sessions.lock().expect("deleted").clone()
    }

    /// Forget every session, as a restarted or expiring peer would.
    fn expire_all_sessions(&self) {
        self.live_sessions.lock().expect("sessions").clear();
    }
}

async fn spawn_session_peer() -> SessionPeer {
    let methods = Arc::new(Mutex::new(Vec::new()));
    let live_sessions = Arc::new(Mutex::new(HashSet::new()));
    let deleted_sessions = Arc::new(Mutex::new(Vec::new()));
    let minted_sessions = Arc::new(Mutex::new(Vec::new()));

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind peer");
    let addr: SocketAddr = listener.local_addr().expect("peer addr");

    let post_methods = Arc::clone(&methods);
    let post_live = Arc::clone(&live_sessions);
    let post_minted = Arc::clone(&minted_sessions);
    let post = move |headers: HeaderMap, body: String| {
        let methods = Arc::clone(&post_methods);
        let live = Arc::clone(&post_live);
        let minted = Arc::clone(&post_minted);
        async move {
            let body: Value = serde_json::from_str(&body).expect("peer received non-JSON body");
            let method = body["method"].as_str().unwrap_or_default().to_string();
            methods.lock().expect("method log").push(method.clone());
            let id = body["id"].clone();

            if method.starts_with("notifications/") {
                return (StatusCode::ACCEPTED, HeaderMap::new(), String::new());
            }

            if method == "initialize" {
                let session_id = Uuid::new_v4().to_string();
                live.lock().expect("sessions").insert(session_id.clone());
                minted.lock().expect("minted").push(session_id.clone());
                let mut out = HeaderMap::new();
                out.insert("MCP-Session-Id", session_id.parse().expect("header value"));
                out.insert("content-type", "application/json".parse().expect("ct"));
                let reply = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": { "protocolVersion": "2025-11-25", "capabilities": {} }
                });
                return (StatusCode::OK, out, reply.to_string());
            }

            let presented = headers
                .get("mcp-session-id")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            let known = presented
                .as_deref()
                .map(|value| live.lock().expect("sessions").contains(value))
                .unwrap_or(false);
            if !known {
                // Spec: an expired or unknown session is a transport 404.
                return (StatusCode::NOT_FOUND, HeaderMap::new(), String::new());
            }

            let result = match method.as_str() {
                "tools/list" => json!({ "tools": [
                    { "name": "probe", "description": "session probe", "inputSchema": { "type": "object" } }
                ] }),
                "resources/list" => json!({ "resources": [] }),
                "resources/read" => json!({
                    "contents": [{
                        "uri": "slice://",
                        "mimeType": "application/json",
                        "text": json!({ "schema_version": "1" }).to_string()
                    }]
                }),
                "tools/call" => {
                    json!({ "content": [{ "type": "text", "text": "ok" }], "isError": false })
                }
                _ => json!({ "ok": true }),
            };
            let mut out = HeaderMap::new();
            out.insert("content-type", "application/json".parse().expect("ct"));
            (
                StatusCode::OK,
                out,
                json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string(),
            )
        }
    };

    let delete_methods = Arc::clone(&methods);
    let delete_live = Arc::clone(&live_sessions);
    let delete_log = Arc::clone(&deleted_sessions);
    let delete = move |headers: HeaderMap| {
        let methods = Arc::clone(&delete_methods);
        let live = Arc::clone(&delete_live);
        let log = Arc::clone(&delete_log);
        async move {
            methods
                .lock()
                .expect("method log")
                .push("DELETE".to_string());
            if let Some(session_id) = headers
                .get("mcp-session-id")
                .and_then(|value| value.to_str().ok())
            {
                live.lock().expect("sessions").remove(session_id);
                log.lock().expect("deleted").push(session_id.to_string());
            }
            StatusCode::NO_CONTENT
        }
    };

    let app = axum::Router::new().route("/mcp", axum::routing::post(post).delete(delete));
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("peer server error");
    });

    SessionPeer {
        mcp_url: format!("http://{addr}/mcp"),
        methods,
        live_sessions,
        deleted_sessions,
        minted_sessions,
    }
}

// ─── fixtures ─────────────────────────────────────────────────────────────────

async fn default_org_id(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("SELECT id FROM organizations WHERE name = 'Default Org' LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("Default Org not found")
}

async fn default_user_id(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("SELECT id FROM users ORDER BY created_at LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("no seeded user")
}

async fn default_workspace_id(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("SELECT id FROM workspaces WHERE name = 'Operations' LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("Operations workspace not found")
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

async fn insert_active_peer(
    pool: &PgPool,
    org_id: Uuid,
    name: &str,
    prefix: &str,
    mcp_url: &str,
    issuer_id: Uuid,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO peers
           (org_id, name, mcp_url, issuer_id, sharing_policy, tool_allowlist, status, tool_prefix)
         VALUES ($1, $2, $3, $4, '{}'::jsonb, '[]'::jsonb, 'active'::peer_status, $5)
         RETURNING id",
    )
    .bind(org_id)
    .bind(name)
    .bind(mcp_url)
    .bind(issuer_id)
    .bind(prefix)
    .fetch_one(pool)
    .await
    .expect("insert peer")
}

async fn insert_active_binding(pool: &PgPool, workspace_id: Uuid, peer_id: Uuid) {
    sqlx::query(
        "INSERT INTO workspace_peer_bindings
           (workspace_id, peer_id, foreign_tenant_id, foreign_workspace_id, status)
         VALUES ($1, $2, 'session-tenant', 'remote-ws', 'active'::binding_status)",
    )
    .bind(workspace_id)
    .bind(peer_id)
    .execute(pool)
    .await
    .expect("insert binding");
}

fn tool_caller(org_id: Uuid, user_id: Uuid) -> AuthContext {
    AuthContext {
        user_id,
        org_id,
        is_oidc: false,
        is_mcp_peer: false,
        active_role_id: None,
        session_id: None,
        mfa_verified: true,
        is_service_account: true,
        service_account_token_id: None,
        permissions: vec!["tool_invoke:*:*".to_string()],
    }
}

// ─── F8: session reuse ────────────────────────────────────────────────────────

/// One `initialize` serves every subsequent call to the same peer.
///
/// Six outbound JSON-RPC calls (`tools/list`, `resources/list`, then four
/// `tools/call`s) used to mean six handshakes and six abandoned server-side
/// sessions. The peer here mints a fresh id per `initialize` and records them,
/// so both the round-trip cost and the orphan count are asserted directly.
#[tokio::test]
#[ignore]
async fn one_initialize_serves_every_call_to_a_session_enforcing_peer() {
    let (pool, state) = spawn_state().await;
    let org_id = default_org_id(&pool).await;
    let user_id = default_user_id(&pool).await;
    let workspace_id = default_workspace_id(&pool).await;
    let peer = spawn_session_peer().await;
    let issuer_id = insert_trust_issuer(&pool, org_id, "https://iss-session-reuse.test").await;
    let peer_id = insert_active_peer(
        &pool,
        org_id,
        "Session Peer",
        "sess",
        &peer.mcp_url,
        issuer_id,
    )
    .await;
    insert_active_binding(&pool, workspace_id, peer_id).await;
    let auth = tool_caller(org_id, user_id);

    // Populates the workspace-scoped manifest: tools/list + resources/list.
    ione::services::federation::workspace_peer_manifest(&state, workspace_id, peer_id, &auth)
        .await
        .expect("workspace manifest against a session-enforcing peer");

    for call in 0..4 {
        ione::services::federation::route_tool_call(
            &state,
            workspace_id,
            "sess:probe",
            json!({ "n": call }),
            &auth,
        )
        .await
        .expect("tools/call against a session-enforcing peer");
    }

    assert_eq!(
        peer.count_of("initialize"),
        1,
        "the negotiated session must be reused, saw {:?}",
        peer.methods()
    );
    assert_eq!(
        peer.minted().len(),
        1,
        "every extra initialize is a server-side session IONe abandoned"
    );
    assert_eq!(
        peer.count_of("notifications/initialized"),
        1,
        "the lifecycle notification belongs to the handshake, not to every call"
    );
    assert_eq!(
        peer.count_of("tools/call"),
        4,
        "each call must still reach the peer exactly once, saw {:?}",
        peer.methods()
    );
}

/// An HTTP 404 is how a spec-conforming server says "that session is gone". It
/// used to surface as a bare `peer returned HTTP 404`, which
/// `looks_like_missing_session` did not match, so the call failed outright
/// instead of re-handshaking. One 404 must buy exactly one new session — not
/// zero, and not one per call afterwards.
#[tokio::test]
#[ignore]
async fn an_http_404_triggers_exactly_one_reinitialize() {
    let (pool, state) = spawn_state().await;
    let org_id = default_org_id(&pool).await;
    let user_id = default_user_id(&pool).await;
    let workspace_id = default_workspace_id(&pool).await;
    let peer = spawn_session_peer().await;
    let issuer_id = insert_trust_issuer(&pool, org_id, "https://iss-session-404.test").await;
    let peer_id = insert_active_peer(
        &pool,
        org_id,
        "Expiring Peer",
        "exp",
        &peer.mcp_url,
        issuer_id,
    )
    .await;
    insert_active_binding(&pool, workspace_id, peer_id).await;
    let auth = tool_caller(org_id, user_id);

    ione::services::federation::workspace_peer_manifest(&state, workspace_id, peer_id, &auth)
        .await
        .expect("first manifest establishes a session");
    assert_eq!(peer.count_of("initialize"), 1);

    // The peer forgets its sessions. The id IONe holds now 404s.
    peer.expire_all_sessions();

    for call in 0..3 {
        ione::services::federation::route_tool_call(
            &state,
            workspace_id,
            "exp:probe",
            json!({ "n": call }),
            &auth,
        )
        .await
        .expect("a 404'd session must be renegotiated, not surfaced");
    }

    assert_eq!(
        peer.count_of("initialize"),
        2,
        "one expiry means one re-handshake, not one per call; saw {:?}",
        peer.methods()
    );
}

/// A session IONe stops using is handed back with `DELETE`, not abandoned.
///
/// The TTL is the only teardown point inside the federation service — it is
/// where IONe decides it is done with an id — so it is where the streamable-HTTP
/// termination request belongs. Driven through `IONE_MCP_SESSION_TTL_SECS`
/// rather than by waiting out the hour-long default.
#[tokio::test]
#[ignore]
async fn an_expired_session_is_released_with_delete() {
    std::env::set_var("IONE_MCP_SESSION_TTL_SECS", "1");
    let (pool, state) = spawn_state().await;
    let org_id = default_org_id(&pool).await;
    let user_id = default_user_id(&pool).await;
    let workspace_id = default_workspace_id(&pool).await;
    let peer = spawn_session_peer().await;
    let issuer_id = insert_trust_issuer(&pool, org_id, "https://iss-session-delete.test").await;
    let peer_id = insert_active_peer(
        &pool,
        org_id,
        "Released Peer",
        "rel",
        &peer.mcp_url,
        issuer_id,
    )
    .await;
    insert_active_binding(&pool, workspace_id, peer_id).await;
    let auth = tool_caller(org_id, user_id);

    ione::services::federation::workspace_peer_manifest(&state, workspace_id, peer_id, &auth)
        .await
        .expect("first manifest establishes a session");
    let first_session = peer
        .minted()
        .first()
        .cloned()
        .expect("the peer minted a session");

    // Comfortably past a 1s TTL measured in whole seconds.
    tokio::time::sleep(Duration::from_millis(2200)).await;

    ione::services::federation::route_tool_call(
        &state,
        workspace_id,
        "rel:probe",
        json!({}),
        &auth,
    )
    .await
    .expect("a call after the TTL renegotiates");

    let mut deleted = Vec::new();
    for _ in 0..40 {
        deleted = peer.deleted();
        if !deleted.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    std::env::remove_var("IONE_MCP_SESSION_TTL_SECS");

    assert_eq!(
        deleted,
        vec![first_session],
        "the expired session must be released with DELETE, saw {:?}",
        peer.methods()
    );
}

// ─── F10b: connector-side tools/list pagination ───────────────────────────────

/// A peer that paginates `tools/list`, with one readable tool on each page. No
/// database is involved: `McpClientConnector::from_config` with a literal bearer
/// and no pool exercises the connector alone.
struct PagingPeer {
    mcp_url: String,
    list_calls: Arc<AtomicUsize>,
}

async fn spawn_paging_peer() -> PagingPeer {
    let list_calls = Arc::new(AtomicUsize::new(0));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind peer");
    let addr: SocketAddr = listener.local_addr().expect("peer addr");

    let calls = Arc::clone(&list_calls);
    let mcp = move |axum::Json(body): axum::Json<Value>| {
        let calls = Arc::clone(&calls);
        async move {
            let id = body.get("id").cloned().unwrap_or(Value::Null);
            let method = body["method"].as_str().unwrap_or_default().to_string();
            if method != "tools/list" {
                return axum::Json(json!({ "jsonrpc": "2.0", "id": id, "result": { "ok": true } }));
            }
            calls.fetch_add(1, Ordering::SeqCst);
            let result = match body["params"]["cursor"].as_str() {
                None => json!({
                    "tools": [
                        { "name": "list_survivors", "description": "page one" },
                        { "name": "not_readable", "description": "page one filler" }
                    ],
                    "nextCursor": "tools-page-2"
                }),
                Some("tools-page-2") => json!({
                    "tools": [
                        { "name": "search_stream_events", "description": "page two" }
                    ],
                    "nextCursor": null
                }),
                Some(other) => panic!("peer received an unexpected cursor: {other}"),
            };
            axum::Json(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
        }
    };

    let app = axum::Router::new().route("/mcp", axum::routing::post(mcp));
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("peer server error");
    });
    PagingPeer {
        mcp_url: format!("http://{addr}/mcp"),
        list_calls,
    }
}

/// Contract v1 §8: every list path follows `nextCursor`. `search_stream_events`
/// is readable and sits on page two, so a single un-cursored `tools/list` gives
/// it no derived stream at all — the connector silently polls half the peer.
#[tokio::test]
#[ignore]
async fn default_streams_follows_tools_list_pagination() {
    let peer = spawn_paging_peer().await;
    let connector = McpClientConnector::from_config(
        &json!({ "mcp_url": peer.mcp_url, "bearer_token": "paging-test-bearer" }),
        None,
    )
    .expect("build mcp_client connector");

    let streams = connector
        .default_streams()
        .await
        .expect("default_streams against a paginating peer");

    let mut names: Vec<String> = streams.into_iter().map(|s| s.name).collect();
    names.sort();
    assert_eq!(
        names,
        vec![
            "list_survivors".to_string(),
            "search_stream_events".to_string()
        ],
        "a readable tool on page two must still get a derived stream"
    );
    assert_eq!(
        peer.list_calls.load(Ordering::SeqCst),
        2,
        "both pages must be requested exactly once"
    );
}

// ─── F11: what counts as a peer-authored slice ────────────────────────────────

/// What the peer's `resources/read slice://` answers with.
#[derive(Clone, Copy, PartialEq)]
enum SliceMode {
    /// A real slice carrying one sample query.
    Good,
    /// A 200 whose `contents[0].text` is not JSON — a truncated body.
    Garbled,
    /// A 200 whose `contents` array is empty: the peer answered, but served no
    /// slice document at all.
    NoContents,
    /// A valid, deliberately empty slice: the peer *says* it has no sample
    /// queries.
    ValidEmpty,
}

struct SlicePeer {
    mcp_url: String,
    mode: Arc<Mutex<SliceMode>>,
}

impl SlicePeer {
    fn set_mode(&self, mode: SliceMode) {
        *self.mode.lock().expect("slice mode") = mode;
    }
}

async fn spawn_slice_peer() -> SlicePeer {
    let mode = Arc::new(Mutex::new(SliceMode::Good));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind peer");
    let addr: SocketAddr = listener.local_addr().expect("peer addr");

    let peer_mode = Arc::clone(&mode);
    let mcp = move |axum::Json(body): axum::Json<Value>| {
        let peer_mode = Arc::clone(&peer_mode);
        async move {
            let id = body.get("id").cloned().unwrap_or(Value::Null);
            let method = body["method"].as_str().unwrap_or_default().to_string();
            let result = match method.as_str() {
                "tools/list" => json!({ "tools": [{
                    "name": "probe",
                    "description": "catalog probe",
                    "inputSchema": { "type": "object", "properties": { "target": { "type": "string" } } }
                }] }),
                "resources/list" => json!({ "resources": [] }),
                "resources/read" => {
                    let mode = *peer_mode.lock().expect("slice mode");
                    match mode {
                        SliceMode::Good => json!({ "contents": [{
                            "uri": "slice://",
                            "mimeType": "application/vnd.ione.slice+json",
                            "text": json!({
                                "schema_version": "1",
                                "sample_queries": { "probe": ["how many probes are open?"] }
                            }).to_string()
                        }] }),
                        SliceMode::Garbled => json!({ "contents": [{
                            "uri": "slice://",
                            "mimeType": "application/vnd.ione.slice+json",
                            "text": "{\"schema_version\": \"1\", \"sample_queri"
                        }] }),
                        SliceMode::NoContents => json!({ "contents": [] }),
                        SliceMode::ValidEmpty => json!({ "contents": [{
                            "uri": "slice://",
                            "mimeType": "application/vnd.ione.slice+json",
                            "text": "{}"
                        }] }),
                    }
                }
                _ => json!({ "ok": true }),
            };
            axum::Json(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
        }
    };

    let app = axum::Router::new().route("/mcp", axum::routing::post(mcp));
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("peer server error");
    });
    SlicePeer {
        mcp_url: format!("http://{addr}/mcp"),
        mode,
    }
}

async fn catalog_probe_row(pool: &PgPool, peer_id: Uuid) -> (Vec<String>, String, DateTime<Utc>) {
    sqlx::query_as(
        "SELECT sample_queries, content_hash, updated_at FROM peer_catalog_entries
         WHERE peer_id = $1 AND namespaced_name = 'slice:probe'",
    )
    .bind(peer_id)
    .fetch_one(pool)
    .await
    .expect("catalog row for slice:probe")
}

/// Seed the catalog from a healthy slice, then hand the reindex path back to the
/// caller with the peer switched into `mode`.
async fn slice_fixture() -> (PgPool, AppState, SlicePeer, Uuid) {
    let (pool, state) = spawn_state().await;
    let org_id = default_org_id(&pool).await;
    let workspace_id = default_workspace_id(&pool).await;
    let peer = spawn_slice_peer().await;
    let issuer_id = insert_trust_issuer(&pool, org_id, "https://iss-slice-body.test").await;
    let peer_id = insert_active_peer(
        &pool,
        org_id,
        "Slice Peer",
        "slice",
        &peer.mcp_url,
        issuer_id,
    )
    .await;
    insert_active_binding(&pool, workspace_id, peer_id).await;

    ione::services::federation::refresh_manifest_if_changed(&state, peer_id)
        .await
        .expect("first manifest refresh");
    (pool, state, peer, peer_id)
}

/// A peer *answering* is not a peer *serving a valid slice*. A truncated body,
/// and a reply with no slice document in it at all, are failed reads wearing a
/// 200 — treating either as an empty slice wipes every row's `sample_queries`
/// and flips its `content_hash`.
#[tokio::test]
#[ignore]
async fn an_unusable_slice_body_preserves_indexed_sample_queries() {
    let (pool, state, peer, peer_id) = slice_fixture().await;

    let (queries, hash, updated_at) = catalog_probe_row(&pool, peer_id).await;
    assert_eq!(
        queries,
        vec!["how many probes are open?".to_string()],
        "the healthy slice should have seeded sample_queries"
    );

    for mode in [SliceMode::Garbled, SliceMode::NoContents] {
        peer.set_mode(mode);
        state.peer_slice_cache.clear();
        ione::services::federation::refresh_manifest_if_changed(&state, peer_id)
            .await
            .expect("manifest refresh with an unusable slice body");

        let (bad_queries, bad_hash, bad_updated_at) = catalog_probe_row(&pool, peer_id).await;
        assert_eq!(
            bad_queries, queries,
            "an unusable slice body wiped the indexed sample_queries"
        );
        assert_eq!(
            bad_hash, hash,
            "content_hash churned on an unusable slice body"
        );
        assert_eq!(
            bad_updated_at, updated_at,
            "the row was rewritten on an unusable slice body"
        );
    }
}

/// The other half of the distinction: a peer that serves `{}` has *said* it has
/// no sample queries, and that answer is authoritative — including for removals.
/// Preserving the old values here would make a slice impossible to empty.
#[tokio::test]
#[ignore]
async fn a_valid_empty_slice_clears_sample_queries() {
    let (pool, state, peer, peer_id) = slice_fixture().await;
    let (queries, hash, _) = catalog_probe_row(&pool, peer_id).await;
    assert_eq!(queries.len(), 1, "precondition: a sample query is indexed");

    peer.set_mode(SliceMode::ValidEmpty);
    state.peer_slice_cache.clear();
    ione::services::federation::refresh_manifest_if_changed(&state, peer_id)
        .await
        .expect("manifest refresh with a valid empty slice");

    let (cleared, new_hash, _) = catalog_probe_row(&pool, peer_id).await;
    assert!(
        cleared.is_empty(),
        "a valid empty slice must clear sample_queries, saw {cleared:?}"
    );
    assert_ne!(
        new_hash, hash,
        "clearing sample_queries must be reflected in content_hash"
    );
}

/// `fetch_slice` keeps papering over a failed read with a manifest-derived
/// stand-in — that is what the render paths want, and tightening
/// `peer_authored_slice` must not change it.
#[tokio::test]
#[ignore]
async fn fetch_slice_still_falls_back_when_the_body_is_unusable() {
    let (pool, state, peer, peer_id) = slice_fixture().await;
    peer.set_mode(SliceMode::Garbled);
    let handle = PeerRepo::new(pool.clone())
        .get(peer_id)
        .await
        .expect("peer lookup")
        .expect("peer row");

    let slice = ione::services::federation::fetch_slice(&state, &handle)
        .await
        .expect("fetch_slice must not fail on an unusable body");
    assert_eq!(
        slice.body["schema_version"].as_str(),
        Some("0"),
        "the render path expects the synthesized stand-in, got {}",
        slice.body
    );
}
