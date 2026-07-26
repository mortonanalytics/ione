//! `peers.tool_allowlist` on the **federated invocation** paths.
//!
//! The allowlist is what `POST /api/v1/peers/:id/authorize` writes, and writing
//! it is also what promotes a peer to Active — so an operator who authorizes a
//! peer with a narrow list has stated the full set of that peer's tools anyone
//! may invoke. It used to be read in exactly one place, `services/delivery.rs`,
//! which gates IONe's *outbound* `propose_artifact` delivery. The inbound
//! federation path gated on an Active binding, the `tool_invoke` grant, and
//! manifest membership, and never consulted it: a caller holding
//! `tool_invoke:<peer>:<tool>` could invoke any tool the peer advertised,
//! authorized or not.
//!
//! Every test here drives a real peer over HTTP, so a denial that is only a
//! return value — and not an absence on the wire — fails.
//!
//! Empty-allowlist semantics are pinned by
//! `an_empty_allowlist_is_treated_as_unconfigured` below; read that test's
//! comment before changing them.
//!
//! Run (serial, ignored):
//!   DATABASE_URL=postgres://ione:ione@localhost:5433/ione_w15 \
//!     SQLX_OFFLINE=true IONE_SKIP_LIVE=1 \
//!     cargo test --test tool_allowlist_integration -- --ignored --test-threads=1

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ione::auth::AuthContext;
use ione::state::AppState;
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;
use uuid::Uuid;

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione_w15";
const TEST_TOKEN_KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
const TEST_STATIC_BEARER: &str = "tool-allowlist-bearer";

/// Advertised by the stub, authorized by the operator.
const TOOL_ALLOWED: &str = "read_alerts";
/// Advertised by the stub, deliberately *not* authorized.
const TOOL_BLOCKED: &str = "drop_database";
/// Advertised by the stub with `ione_approval.required`, so it routes through
/// the pending-approval path instead of dispatching immediately.
const TOOL_GATED: &str = "purge_records";

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

/// A peer that advertises three tools and records every `tools/call` it is
/// asked to run. The recording is the point: an allowlist denial has to happen
/// *before* the outbound call, not after it.
struct StubPeer {
    mcp_url: String,
    invoked: Arc<Mutex<Vec<String>>>,
}

impl StubPeer {
    fn invoked(&self) -> Vec<String> {
        self.invoked.lock().expect("invoked mutex").clone()
    }
}

async fn spawn_stub_peer() -> StubPeer {
    let invoked = Arc::new(Mutex::new(Vec::new()));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind peer");
    let addr: SocketAddr = listener.local_addr().expect("peer addr");

    let recorder = Arc::clone(&invoked);
    let mcp = move |axum::Json(body): axum::Json<Value>| {
        let recorder = Arc::clone(&recorder);
        async move {
            let id = body.get("id").cloned().unwrap_or(Value::Null);
            let method = body
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let result = match method.as_str() {
                "tools/list" => json!({
                    "tools": [
                        { "name": TOOL_ALLOWED, "description": "read", "inputSchema": { "type": "object" } },
                        { "name": TOOL_BLOCKED, "description": "destructive", "inputSchema": { "type": "object" } },
                        {
                            "name": TOOL_GATED,
                            "description": "destructive, approval-gated",
                            "inputSchema": { "type": "object" },
                            "ione_approval": { "required": true }
                        }
                    ]
                }),
                "resources/list" => json!({ "resources": [] }),
                "resources/read" => json!({
                    "contents": [{
                        "uri": "slice://",
                        "mimeType": "application/json",
                        "text": json!({ "schema_version": "1" }).to_string()
                    }]
                }),
                "tools/call" => {
                    let name = body["params"]["name"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string();
                    recorder.lock().expect("invoked mutex").push(name.clone());
                    json!({ "content": [{ "type": "text", "text": name }], "isError": false })
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
    StubPeer {
        mcp_url: format!("http://{addr}/mcp"),
        invoked,
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

/// An Active peer carrying exactly the allowlist the operator authorized.
async fn insert_active_peer(
    pool: &PgPool,
    org_id: Uuid,
    mcp_url: &str,
    issuer_id: Uuid,
    allowlist: Value,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO peers
           (org_id, name, mcp_url, issuer_id, sharing_policy, tool_allowlist, status, tool_prefix)
         VALUES ($1, 'Allowlist Peer', $2, $3, '{}'::jsonb, $4, 'active'::peer_status, 'gate')
         RETURNING id",
    )
    .bind(org_id)
    .bind(mcp_url)
    .bind(issuer_id)
    .bind(allowlist)
    .fetch_one(pool)
    .await
    .expect("insert peer")
}

async fn insert_active_binding(pool: &PgPool, workspace_id: Uuid, peer_id: Uuid) {
    sqlx::query(
        "INSERT INTO workspace_peer_bindings
           (workspace_id, peer_id, foreign_tenant_id, foreign_workspace_id, status)
         VALUES ($1, $2, 'allowlist-tenant', 'remote-ws', 'active'::binding_status)",
    )
    .bind(workspace_id)
    .bind(peer_id)
    .execute(pool)
    .await
    .expect("insert binding");
}

/// A caller holding `tool_invoke:*:*`, so the RBAC gate is satisfied for every
/// tool and the only thing that can still deny a call is the allowlist.
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
    user_id: Uuid,
    workspace_id: Uuid,
    peer_id: Uuid,
    peer: StubPeer,
    auth: AuthContext,
}

async fn fixture(allowlist: Value) -> Fixture {
    let (pool, state) = spawn_state().await;
    let org_id = default_org_id(&pool).await;
    let user_id = default_user_id(&pool).await;
    let workspace_id = default_workspace_id(&pool).await;
    let peer = spawn_stub_peer().await;
    let issuer_id = insert_trust_issuer(&pool, org_id, "https://iss-allowlist.test").await;
    let peer_id = insert_active_peer(&pool, org_id, &peer.mcp_url, issuer_id, allowlist).await;
    insert_active_binding(&pool, workspace_id, peer_id).await;
    let auth = tool_caller(org_id, user_id);
    Fixture {
        pool,
        state,
        user_id,
        workspace_id,
        peer_id,
        peer,
        auth,
    }
}

/// The `interaction_events` writer batches on a 500ms timer, so denial rows are
/// polled for rather than read once.
async fn await_interaction_event(pool: &PgPool, tool_name: &str, outcome: &str) -> Value {
    for _ in 0..40 {
        let detail: Option<Value> = sqlx::query_scalar(
            "SELECT detail FROM interaction_events
             WHERE tool_name = $1 AND outcome = $2
             ORDER BY recorded_at DESC LIMIT 1",
        )
        .bind(tool_name)
        .bind(outcome)
        .fetch_optional(pool)
        .await
        .expect("read interaction events");
        if let Some(detail) = detail {
            return detail;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("no '{outcome}' interaction event was recorded for {tool_name}");
}

// ─── tests ────────────────────────────────────────────────────────────────────

/// The core regression. `TOOL_BLOCKED` is advertised by the peer and the caller
/// holds `tool_invoke:*:*` for it, so the binding, the grant and the manifest
/// check all pass. Only the operator's allowlist says no — and that has to be
/// enough, before anything reaches the peer.
#[tokio::test]
#[ignore]
async fn a_tool_absent_from_the_allowlist_is_denied_on_the_invocation_path() {
    let f = fixture(json!([TOOL_ALLOWED, TOOL_GATED])).await;

    let allowed = ione::services::federation::route_tool_call(
        &f.state,
        f.workspace_id,
        &format!("gate:{TOOL_ALLOWED}"),
        json!({}),
        &f.auth,
    )
    .await
    .expect("an allowlisted tool must still dispatch");
    assert_eq!(allowed["isError"], json!(false), "{allowed}");

    let denied = ione::services::federation::route_tool_call(
        &f.state,
        f.workspace_id,
        &format!("gate:{TOOL_BLOCKED}"),
        json!({}),
        &f.auth,
    )
    .await;
    let err = denied.expect_err("a tool outside the allowlist must be denied");
    assert!(
        err.to_string().starts_with("FORBIDDEN:"),
        "an allowlist denial must surface as FORBIDDEN (→ -32403 / 403), got: {err}"
    );
    assert!(
        err.to_string().contains("allowlist"),
        "the denial must name the allowlist as the reason, got: {err}"
    );

    assert_eq!(
        f.peer.invoked(),
        vec![TOOL_ALLOWED.to_string()],
        "the denied tool must never have reached the peer"
    );

    let detail = await_interaction_event(&f.pool, TOOL_BLOCKED, "deny").await;
    assert_eq!(
        detail["code"].as_str(),
        Some("tool_not_allowlisted"),
        "the denial must be audited as an allowlist denial, got {detail}"
    );
}

/// The approved-execution path resolves its own peer handle and dispatches
/// without re-entering `route_tool_call`, so it needs its own check. An approval
/// is consent to run one call; it is not consent to run a tool the operator has
/// since removed from the allowlist.
#[tokio::test]
#[ignore]
async fn an_approved_call_for_a_tool_removed_from_the_allowlist_is_denied() {
    let f = fixture(json!([TOOL_ALLOWED, TOOL_GATED])).await;

    let pending = ione::services::federation::route_tool_call(
        &f.state,
        f.workspace_id,
        &format!("gate:{TOOL_GATED}"),
        json!({ "target": "site-1" }),
        &f.auth,
    )
    .await
    .expect("enqueue the approval-gated call");
    assert_eq!(
        pending["status"].as_str(),
        Some("pending_approval"),
        "an approval-gated tool must not dispatch immediately: {pending}"
    );

    let pending_id =
        Uuid::parse_str(pending["pending_id"].as_str().expect("pending id")).expect("pending uuid");
    let approval_id: Uuid =
        sqlx::query_scalar("SELECT approval_id FROM pending_peer_tool_calls WHERE id = $1")
            .bind(pending_id)
            .fetch_one(&f.pool)
            .await
            .expect("approval id");

    // The operator narrows the allowlist between enqueue and approval.
    ione::repos::PeerRepo::new(f.pool.clone())
        .set_allowlist(f.peer_id, &json!([TOOL_ALLOWED]))
        .await
        .expect("narrow the allowlist");

    let err =
        ione::services::federation::execute_pending_tool_call(&f.state, approval_id, f.user_id)
            .await
            .expect_err("approving a de-authorized tool must not execute it");
    assert!(
        err.to_string().starts_with("FORBIDDEN:"),
        "an allowlist denial on the approved path must surface as FORBIDDEN, got: {err}"
    );

    assert!(
        f.peer.invoked().is_empty(),
        "the de-authorized tool must never have reached the peer, saw {:?}",
        f.peer.invoked()
    );

    let blocked: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_events
         WHERE verb = 'peer_tool_blocked' AND object_id = $1",
    )
    .bind(pending_id)
    .fetch_one(&f.pool)
    .await
    .expect("count peer_tool_blocked audit rows");
    assert_eq!(
        blocked, 1,
        "the approved-path denial must be audited as peer_tool_blocked"
    );
}

/// An empty allowlist on a peer that was **never authorized** means "not
/// configured", not "nothing allowed".
///
/// `migrations/0016_peers_oauth.sql` declares the column
/// `JSONB NOT NULL DEFAULT '[]'`, so the value alone cannot distinguish a peer
/// the operator never took through the authorize route from one authorized with
/// zero tools. `migrations/0049` adds `tool_allowlist_configured` to separate
/// them; this test covers the not-configured half, where the row still carries
/// the column default and must keep working (that is how the demo seeder and
/// most fixtures create peers). The configured half is
/// `an_authorized_empty_allowlist_denies_every_tool`.
#[tokio::test]
#[ignore]
async fn an_empty_allowlist_is_treated_as_unconfigured() {
    let f = fixture(json!([])).await;

    let result = ione::services::federation::route_tool_call(
        &f.state,
        f.workspace_id,
        &format!("gate:{TOOL_BLOCKED}"),
        json!({}),
        &f.auth,
    )
    .await
    .expect("an empty allowlist must not gate; see this test's doc comment");
    assert_eq!(result["isError"], json!(false), "{result}");
    assert_eq!(f.peer.invoked(), vec![TOOL_BLOCKED.to_string()]);
}

/// The fail-closed half: once the operator has actually been through
/// `POST /api/v1/peers/:id/authorize`, `tool_allowlist_configured` is true and an
/// empty allowlist means the peer may invoke **nothing**. Before
/// `migrations/0049` this was indistinguishable from the column default, so the
/// federated path could not fail closed here without denying every
/// directly-seeded peer. `delivery.rs` has always been fail-closed on empty;
/// this brings the federated path into line.
#[tokio::test]
#[ignore]
async fn an_authorized_empty_allowlist_denies_every_tool() {
    let f = fixture(json!([])).await;

    // Exactly what the authorize route does: write the allowlist and flag it.
    sqlx::query("UPDATE peers SET tool_allowlist = '[]'::jsonb, tool_allowlist_configured = true WHERE id = $1")
        .bind(f.peer_id)
        .execute(&f.pool)
        .await
        .expect("mark the allowlist configured");

    let err = ione::services::federation::route_tool_call(
        &f.state,
        f.workspace_id,
        &format!("gate:{TOOL_BLOCKED}"),
        json!({}),
        &f.auth,
    )
    .await
    .expect_err("an authorized-but-empty allowlist must deny every tool");
    assert!(
        err.to_string().contains("allowlist"),
        "denial must name the allowlist, got: {err}"
    );
    assert!(
        f.peer.invoked().is_empty(),
        "a denied call must not reach the peer, got {:?}",
        f.peer.invoked()
    );
}
