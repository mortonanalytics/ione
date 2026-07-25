//! Outbound credential presentation on the **federation** paths.
//!
//! `tests/peer_credential_integration.rs` and `tests/identity_broker_integration.rs`
//! both drive outbound auth through exactly one route (`/table-data`), which
//! resolves its peer handle via `list_active_peers_for_workspace` — a repo
//! method that tags the handle for you. That is precisely why a federation path
//! that forgets `Peer::scoped_to` survived those suites: `route_tool_call`,
//! approved-tool execution, and the workspace manifest/resource reads all
//! resolve their own peer handle and never touch that repo method.
//!
//! Every test here therefore asserts the literal `Authorization` header the peer
//! received on a **federation** call, plus the two rules that govern which
//! credential is allowed to appear there:
//!
//!   * a 401 against a workspace-scoped bearer is surfaced, never retried with
//!     the peer-global grant (md/design/identity-broker.md, "Delegated-token
//!     precedence"),
//!   * the `mcp_client` connector's config `bearer_token` literal is the last
//!     resort, below the delegated token and the per-workspace credential.
//!
//! The peer context-slice cache TTL is covered here too: the slice body is peer
//! payload served on the same workspace path, and issue #18 promises a *bounded*
//! render cache.
//!
//! Design: `md/design/pre-broker-peer-credentials.md`,
//! `md/design/identity-broker.md`. Precedence contract: the doc comment on
//! `services::peer_tokens::resolve_access_token`.
//!
//! Run (serial, ignored):
//!   DATABASE_URL=postgres://ione:ione@localhost:5433/ione_w15 \
//!     IONE_SKIP_LIVE=1 \
//!     cargo test --test credential_presentation_integration -- --ignored --test-threads=1

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::http::HeaderMap;
use chrono::{DateTime, Duration, Utc};
use ione::auth::AuthContext;
use ione::connectors::mcp_client::McpClientConnector;
use ione::connectors::ConnectorImpl;
use ione::services::federation::{PeerManifest, SliceEntry};
use ione::state::AppState;
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;
use uuid::Uuid;

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione_w15";
const TEST_TOKEN_KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
/// Precedence tier 4. Every assertion that a workspace did NOT receive a
/// workspace-scoped credential checks for this instead.
const TEST_STATIC_BEARER: &str = "federation-env-fallback";
/// What the peer's token endpoint hands back if the peer-global refresh is ever
/// attempted. Seeing this on the wire is the silent downgrade.
const REFRESHED_PEER_GLOBAL: &str = "peer-global-refreshed";
/// `federation::SLICE_TTL_SECONDS`, mirrored so the test can age an entry past it.
const SLICE_TTL_SECONDS: i64 = 300;

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
                  workspace_peer_bindings, audit_events, pipeline_events,
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

/// A stub peer that records the `Authorization` header of every MCP request,
/// can be flipped to answer 401, and exposes an OAuth discovery + token endpoint
/// so a peer-global refresh would *succeed* if it were attempted — a downgrade
/// then shows up as a second, different bearer rather than as a transport error.
struct RecordedPeer {
    mcp_url: String,
    auth_headers: Arc<Mutex<Vec<String>>>,
    unauthorized: Arc<AtomicBool>,
    refresh_hits: Arc<AtomicUsize>,
}

impl RecordedPeer {
    fn bearers(&self) -> Vec<String> {
        self.auth_headers.lock().expect("auth header mutex").clone()
    }

    fn last_bearer(&self) -> String {
        self.bearers().last().cloned().expect("no request recorded")
    }

    fn refresh_hits(&self) -> usize {
        self.refresh_hits.load(Ordering::SeqCst)
    }

    fn reject_with_401(&self) {
        self.unauthorized.store(true, Ordering::SeqCst);
    }
}

async fn spawn_recorded_peer() -> RecordedPeer {
    let auth_headers = Arc::new(Mutex::new(Vec::new()));
    let unauthorized = Arc::new(AtomicBool::new(false));
    let refresh_hits = Arc::new(AtomicUsize::new(0));

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind peer");
    let addr: SocketAddr = listener.local_addr().expect("peer addr");
    let base = format!("http://{addr}");

    let captured = Arc::clone(&auth_headers);
    let reject = Arc::clone(&unauthorized);
    let mcp = move |headers: HeaderMap, axum::Json(body): axum::Json<Value>| {
        let captured = Arc::clone(&captured);
        let reject = Arc::clone(&reject);
        async move {
            let header = headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .unwrap_or("")
                .to_string();
            captured.lock().expect("auth header mutex").push(header);
            if reject.load(Ordering::SeqCst) {
                return (
                    axum::http::StatusCode::UNAUTHORIZED,
                    axum::Json(json!({ "error": "unauthorized" })),
                );
            }
            let id = body.get("id").cloned().unwrap_or(Value::Null);
            (
                axum::http::StatusCode::OK,
                axum::Json(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "ok": true,
                        "contents": [{
                            "uri": "slice://",
                            "mimeType": "application/json",
                            "text": json!({ "summary": "fresh-from-peer" }).to_string()
                        }]
                    }
                })),
            )
        }
    };

    let discovery_base = base.clone();
    let discovery = move || {
        let discovery_base = discovery_base.clone();
        async move {
            axum::Json(json!({
                "issuer": discovery_base,
                "authorization_endpoint": format!("{discovery_base}/authorize"),
                "token_endpoint": format!("{discovery_base}/token"),
                "registration_endpoint": format!("{discovery_base}/register"),
            }))
        }
    };

    let hits = Arc::clone(&refresh_hits);
    let token = move || {
        let hits = Arc::clone(&hits);
        async move {
            hits.fetch_add(1, Ordering::SeqCst);
            axum::Json(json!({
                "access_token": REFRESHED_PEER_GLOBAL,
                "refresh_token": "peer-global-refresh-2",
                "expires_in": 3600,
            }))
        }
    };

    let app = axum::Router::new()
        .route("/mcp", axum::routing::post(mcp))
        .route(
            "/mcp/.well-known/oauth-authorization-server",
            axum::routing::get(discovery),
        )
        .route("/token", axum::routing::post(token));

    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("peer server error");
    });
    RecordedPeer {
        mcp_url: format!("{base}/mcp"),
        auth_headers,
        unauthorized,
        refresh_hits,
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

async fn insert_active_peer(
    pool: &PgPool,
    org_id: Uuid,
    name: &str,
    tool_prefix: &str,
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
    .bind(tool_prefix)
    .fetch_one(pool)
    .await
    .expect("insert peer")
}

async fn insert_active_binding(pool: &PgPool, workspace_id: Uuid, peer_id: Uuid) {
    sqlx::query(
        "INSERT INTO workspace_peer_bindings
           (workspace_id, peer_id, foreign_tenant_id, foreign_workspace_id, status)
         VALUES ($1, $2, 'tenant-a', 'remote-ws', 'active'::binding_status)",
    )
    .bind(workspace_id)
    .bind(peer_id)
    .execute(pool)
    .await
    .expect("insert binding");
}

/// The pre-broker per-(workspace, peer) static credential (tier 3), written the
/// way `PUT /credential` writes it.
async fn insert_workspace_credential(
    pool: &PgPool,
    workspace_id: Uuid,
    peer_id: Uuid,
    credential: &str,
) {
    let ciphertext = ione::util::token_crypto::encrypt_versioned(credential.as_bytes())
        .expect("encrypt credential");
    sqlx::query(
        "INSERT INTO workspace_peer_credentials (workspace_id, peer_id, credential_ciphertext)
         VALUES ($1, $2, $3)",
    )
    .bind(workspace_id)
    .bind(peer_id)
    .bind(&ciphertext)
    .execute(pool)
    .await
    .expect("insert workspace credential");
}

/// The brokered delegated token for (workspace, peer) (tier 1), as the peer
/// delegation callback would have written it.
async fn insert_delegation(
    pool: &PgPool,
    workspace_id: Uuid,
    peer_id: Uuid,
    token_endpoint: &str,
    access: &str,
    expires_at: Option<DateTime<Utc>>,
) {
    let access_cipher =
        ione::util::token_crypto::encrypt_versioned(access.as_bytes()).expect("encrypt access");
    sqlx::query(
        "INSERT INTO workspace_peer_delegations
           (workspace_id, peer_id, oauth_client_id, token_endpoint,
            access_token_ciphertext, token_expires_at)
         VALUES ($1, $2, 'ione-test-client', $3, $4, $5)",
    )
    .bind(workspace_id)
    .bind(peer_id)
    .bind(token_endpoint)
    .bind(&access_cipher)
    .bind(expires_at)
    .execute(pool)
    .await
    .expect("insert delegation");
}

/// Give the peer a refreshable peer-global OAuth grant (tier 2). Present in the
/// no-downgrade tests so the peer-global refresh path is *available* — the point
/// is that it must not be taken.
async fn give_peer_refreshable_oauth(pool: &PgPool, peer_id: Uuid, access: Option<&str>) {
    let access_cipher = access
        .map(|token| ione::util::token_crypto::encrypt_token(token).expect("encrypt peer access"));
    let refresh_cipher = ione::util::token_crypto::encrypt_token("peer-global-refresh")
        .expect("encrypt peer refresh");
    sqlx::query(
        "UPDATE peers
            SET oauth_client_id = 'peer-global-client',
                access_token_ciphertext = $2,
                refresh_token_ciphertext = $3,
                token_expires_at = now() + interval '1 hour'
          WHERE id = $1",
    )
    .bind(peer_id)
    .bind(access_cipher.as_deref())
    .bind(&refresh_cipher)
    .execute(pool)
    .await
    .expect("set peer oauth grant");
}

fn cache_manifest(state: &AppState, peer_id: Uuid, tool: &str, approval_required: bool) {
    state.peer_manifest_cache.insert(
        peer_id,
        PeerManifest {
            peer_id,
            tools: vec![json!({
                "name": tool,
                "description": "credential presentation probe",
                "inputSchema": { "type": "object", "properties": {} },
                "ione_approval": { "required": approval_required }
            })],
            resources: vec![],
            fetched_at: Utc::now(),
            etag: None,
            stale: false,
        },
    );
}

/// A service-account context holding `tool_invoke:*:*`, so `route_tool_call`'s
/// C-2 permission gate passes without depending on seeded role rows.
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

struct Fixture {
    pool: PgPool,
    state: AppState,
    org_id: Uuid,
    user_id: Uuid,
    workspace_id: Uuid,
    peer_id: Uuid,
    peer: RecordedPeer,
    auth: AuthContext,
}

async fn fixture() -> Fixture {
    let (pool, state) = spawn_state().await;
    let org_id = default_org_id(&pool).await;
    let user_id = default_user_id(&pool).await;
    let workspace_id = default_workspace_id(&pool).await;
    let peer = spawn_recorded_peer().await;
    let issuer_id = insert_trust_issuer(&pool, org_id, "https://iss-fed-cred.test").await;
    let peer_id =
        insert_active_peer(&pool, org_id, "Fed Peer", "fed", &peer.mcp_url, issuer_id).await;
    insert_active_binding(&pool, workspace_id, peer_id).await;
    let auth = tool_caller(org_id, user_id);
    Fixture {
        pool,
        state,
        org_id,
        user_id,
        workspace_id,
        peer_id,
        peer,
        auth,
    }
}

// ─── federated tools/call ─────────────────────────────────────────────────────

/// The federated `tools/call` path presents the per-(workspace, peer) static
/// credential. Before the fix this path resolved an unscoped peer handle and
/// fell through to `IONE_OAUTH_STATIC_BEARER`.
#[tokio::test]
#[ignore]
async fn federated_tool_call_presents_workspace_static_credential() {
    let f = fixture().await;
    let secret = "fed-workspace-key-abc";
    insert_workspace_credential(&f.pool, f.workspace_id, f.peer_id, secret).await;
    cache_manifest(&f.state, f.peer_id, "probe", false);

    let result = ione::services::federation::route_tool_call(
        &f.state,
        f.workspace_id,
        "fed:probe",
        json!({}),
        &f.auth,
    )
    .await
    .expect("federated tool call");
    assert_eq!(result["ok"], json!(true), "peer result: {result}");

    assert_eq!(
        f.peer.last_bearer(),
        format!("Bearer {secret}"),
        "federated tools/call must present the workspace credential, not {TEST_STATIC_BEARER}"
    );
}

/// Same path, tier 1: a brokered delegated token for this workspace outranks
/// everything below it and must reach the peer.
#[tokio::test]
#[ignore]
async fn federated_tool_call_presents_delegated_token() {
    let f = fixture().await;
    let delegated = "fed-delegated-token-xyz";
    // Both lower tiers are populated, so a fall-through is visible rather than
    // indistinguishable from a missing credential.
    insert_workspace_credential(&f.pool, f.workspace_id, f.peer_id, "static-should-lose").await;
    give_peer_refreshable_oauth(&f.pool, f.peer_id, Some("peer-global-should-lose")).await;
    insert_delegation(
        &f.pool,
        f.workspace_id,
        f.peer_id,
        "https://unused.test/token",
        delegated,
        Some(Utc::now() + Duration::hours(1)),
    )
    .await;
    cache_manifest(&f.state, f.peer_id, "probe", false);

    ione::services::federation::route_tool_call(
        &f.state,
        f.workspace_id,
        "fed:probe",
        json!({}),
        &f.auth,
    )
    .await
    .expect("federated tool call");

    assert_eq!(f.peer.last_bearer(), format!("Bearer {delegated}"));
}

/// A second workspace bound to the same peer does not inherit the first
/// workspace's credential on the federation path.
#[tokio::test]
#[ignore]
async fn federated_tool_call_credential_does_not_leak_to_another_workspace() {
    let f = fixture().await;
    let other_workspace = insert_workspace(&f.pool, f.org_id, "Other Workspace").await;
    insert_active_binding(&f.pool, other_workspace, f.peer_id).await;

    let secret = "workspace-a-only-fed-key";
    insert_workspace_credential(&f.pool, f.workspace_id, f.peer_id, secret).await;
    cache_manifest(&f.state, f.peer_id, "probe", false);

    ione::services::federation::route_tool_call(
        &f.state,
        f.workspace_id,
        "fed:probe",
        json!({}),
        &f.auth,
    )
    .await
    .expect("workspace A tool call");
    assert_eq!(f.peer.last_bearer(), format!("Bearer {secret}"));

    ione::services::federation::route_tool_call(
        &f.state,
        other_workspace,
        "fed:probe",
        json!({}),
        &f.auth,
    )
    .await
    .expect("workspace B tool call");
    assert_eq!(
        f.peer.last_bearer(),
        format!("Bearer {TEST_STATIC_BEARER}"),
        "workspace B must not receive workspace A's credential"
    );
    assert!(
        !f.peer
            .bearers()
            .iter()
            .skip(1)
            .any(|bearer| bearer == &format!("Bearer {secret}")),
        "workspace A's credential was sent on a call it did not make: {:?}",
        f.peer.bearers()
    );
}

/// The approved-execution path resolves its own peer handle from
/// `pending.peer_id`, so it needs its own scope tag: the executed call must
/// carry the same bearer the un-gated call would have.
#[tokio::test]
#[ignore]
async fn approved_peer_tool_execution_presents_workspace_credential() {
    let f = fixture().await;
    let secret = "approved-exec-workspace-key";
    insert_workspace_credential(&f.pool, f.workspace_id, f.peer_id, secret).await;
    cache_manifest(&f.state, f.peer_id, "danger", true);

    let pending = ione::services::federation::route_tool_call(
        &f.state,
        f.workspace_id,
        "fed:danger",
        json!({ "target": "site-1" }),
        &f.auth,
    )
    .await
    .expect("enqueue approval-gated tool call");
    assert_eq!(
        pending["status"].as_str(),
        Some("pending_approval"),
        "approval-gated tool must not execute immediately: {pending}"
    );
    assert!(
        f.peer.bearers().is_empty(),
        "no outbound call should have been made yet: {:?}",
        f.peer.bearers()
    );

    let pending_id =
        Uuid::parse_str(pending["pending_id"].as_str().expect("pending id")).expect("pending uuid");
    let approval_id: Uuid =
        sqlx::query_scalar("SELECT approval_id FROM pending_peer_tool_calls WHERE id = $1")
            .bind(pending_id)
            .fetch_one(&f.pool)
            .await
            .expect("approval id");

    let executed =
        ione::services::federation::execute_pending_tool_call(&f.state, approval_id, f.user_id)
            .await
            .expect("execute approved tool call");
    assert_eq!(executed.expect("execution result")["ok"], json!(true));

    assert_eq!(
        f.peer.last_bearer(),
        format!("Bearer {secret}"),
        "approved execution must present the workspace credential"
    );
}

// ─── no silent downgrade on 401 ───────────────────────────────────────────────

/// A 401 against a tier-1 delegated token is surfaced. It must not trigger the
/// peer-global refresh-and-retry, which would put a credential the operator
/// never scoped to this workspace on the wire.
#[tokio::test]
#[ignore]
async fn unauthorized_delegated_token_is_not_retried_with_the_peer_global_token() {
    let f = fixture().await;
    let delegated = "delegated-that-gets-401";
    give_peer_refreshable_oauth(&f.pool, f.peer_id, Some("peer-global-access")).await;
    insert_delegation(
        &f.pool,
        f.workspace_id,
        f.peer_id,
        "https://unused.test/token",
        delegated,
        Some(Utc::now() + Duration::hours(1)),
    )
    .await;
    cache_manifest(&f.state, f.peer_id, "probe", false);
    f.peer.reject_with_401();

    let err = ione::services::federation::route_tool_call(
        &f.state,
        f.workspace_id,
        "fed:probe",
        json!({}),
        &f.auth,
    )
    .await
    .expect_err("a 401 on the delegated token must surface");
    assert!(
        err.to_string().contains("401"),
        "the 401 must be propagated, got: {err}"
    );

    assert_eq!(
        f.peer.bearers(),
        vec![format!("Bearer {delegated}")],
        "exactly one attempt, on the delegated token"
    );
    assert_eq!(
        f.peer.refresh_hits(),
        0,
        "the peer-global refresh must not be attempted"
    );
    assert!(
        !f.peer
            .bearers()
            .iter()
            .any(|bearer| bearer.contains(REFRESHED_PEER_GLOBAL)),
        "the peer-global token was presented after a workspace-scoped 401"
    );
}

/// Same rule for tier 3: the per-workspace static credential. The peer here has
/// refresh material but no peer-global access token, which is exactly the shape
/// the old `can_refresh(peer)` guard mistook for "refreshable".
#[tokio::test]
#[ignore]
async fn unauthorized_workspace_credential_is_not_retried_with_the_peer_global_token() {
    let f = fixture().await;
    let secret = "workspace-cred-that-gets-401";
    insert_workspace_credential(&f.pool, f.workspace_id, f.peer_id, secret).await;
    give_peer_refreshable_oauth(&f.pool, f.peer_id, None).await;
    cache_manifest(&f.state, f.peer_id, "probe", false);
    f.peer.reject_with_401();

    let err = ione::services::federation::route_tool_call(
        &f.state,
        f.workspace_id,
        "fed:probe",
        json!({}),
        &f.auth,
    )
    .await
    .expect_err("a 401 on the workspace credential must surface");
    assert!(
        err.to_string().contains("401"),
        "the 401 must be propagated, got: {err}"
    );

    assert_eq!(
        f.peer.bearers(),
        vec![format!("Bearer {secret}")],
        "exactly one attempt, on the workspace credential"
    );
    assert_eq!(
        f.peer.refresh_hits(),
        0,
        "the peer-global refresh must not be attempted"
    );
}

// ─── connector poll path ──────────────────────────────────────────────────────

/// The `mcp_client` connector's config `bearer_token` is the last resort. A
/// workspace with a brokered delegated token must stop sending the literal.
#[tokio::test]
#[ignore]
async fn connector_prefers_delegated_token_over_config_bearer_literal() {
    let f = fixture().await;
    let literal = "connector-config-literal";
    let connector = McpClientConnector::from_config(
        &json!({
            "mcp_url": f.peer.mcp_url,
            "bearer_token": literal,
            "workspace_id": f.workspace_id.to_string(),
            "peer_id": f.peer_id.to_string(),
        }),
        Some(f.pool.clone()),
    )
    .expect("build mcp_client connector");

    // Backward compatibility: with nothing else configured, the literal is used.
    connector
        .invoke("probe", json!({}))
        .await
        .expect("connector invoke with literal only");
    assert_eq!(f.peer.last_bearer(), format!("Bearer {literal}"));

    let delegated = "connector-delegated-token";
    insert_delegation(
        &f.pool,
        f.workspace_id,
        f.peer_id,
        "https://unused.test/token",
        delegated,
        Some(Utc::now() + Duration::hours(1)),
    )
    .await;

    connector
        .invoke("probe", json!({}))
        .await
        .expect("connector invoke with delegation");
    assert_eq!(
        f.peer.last_bearer(),
        format!("Bearer {delegated}"),
        "the delegated token must outrank the connector-config literal"
    );
}

// ─── slice cache TTL ──────────────────────────────────────────────────────────

/// `peer_slice_cache` holds peer payload. Eviction on `resources/updated` is a
/// peer courtesy, so the read side must bound the entry's age itself.
#[tokio::test]
#[ignore]
async fn slice_cache_does_not_serve_an_entry_older_than_its_ttl() {
    let f = fixture().await;

    // Fresh entries are still served from cache without touching the peer.
    f.state.peer_slice_cache.insert(
        f.peer_id,
        SliceEntry {
            peer_id: f.peer_id,
            body: json!({ "summary": "still-fresh" }),
            fetched_at: Utc::now(),
        },
    );
    let fresh =
        ione::services::federation::workspace_context_slices(&f.state, f.workspace_id, &f.auth)
            .await
            .expect("context slices (fresh)");
    assert_eq!(fresh.len(), 1);
    assert_eq!(fresh[0].body["summary"], json!("still-fresh"));
    assert!(
        f.peer.bearers().is_empty(),
        "a fresh cache entry must not hit the peer: {:?}",
        f.peer.bearers()
    );

    // One second past the TTL is stale, and the peer is re-read.
    f.state.peer_slice_cache.insert(
        f.peer_id,
        SliceEntry {
            peer_id: f.peer_id,
            body: json!({ "summary": "stale-and-never-evicted" }),
            fetched_at: Utc::now() - Duration::seconds(SLICE_TTL_SECONDS + 1),
        },
    );
    let refetched =
        ione::services::federation::workspace_context_slices(&f.state, f.workspace_id, &f.auth)
            .await
            .expect("context slices (stale)");
    assert_eq!(refetched.len(), 1);
    assert_eq!(
        refetched[0].body["summary"],
        json!("fresh-from-peer"),
        "an entry past the TTL must not be served"
    );
    assert!(
        !f.peer.bearers().is_empty(),
        "the stale entry should have triggered an outbound resources/read"
    );

    let cached = f
        .state
        .peer_slice_cache
        .get(&f.peer_id)
        .expect("slice re-cached")
        .value()
        .clone();
    assert_eq!(cached.body["summary"], json!("fresh-from-peer"));
}
