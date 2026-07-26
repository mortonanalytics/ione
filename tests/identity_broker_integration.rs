//! Identity broker — HTTP-level coverage for issue #12.
//!
//! Design: `md/design/identity-broker.md` (S5 brokered SaaS OAuth, S6 identity
//! audit, AC-15 RLS). Precedence contract: the doc comment on
//! `services::peer_tokens::resolve_access_token`.
//!
//! Three areas:
//!   * brokered SaaS OAuth refresh/revoke actually reach the provider and audit
//!     the true outcome (previously `POST .../refresh` was a placeholder that
//!     audited unconditional success without contacting anyone),
//!   * brokered delegated tokens are stored and resolved per (workspace, peer),
//!     above the peer-global token and above issue #19's static credential,
//!   * the RLS policies shipped on the identity tables are inert as deployed —
//!     asserted here so the gap is a known, tested limitation and not a claim.
//!
//! Run (serial, ignored):
//!   DATABASE_URL=postgres://ione:ione@localhost:5433/ione_w12 \
//!     IONE_SKIP_LIVE=1 \
//!     cargo test --test identity_broker_integration -- --ignored --test-threads=1

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::http::HeaderMap;
use chrono::{DateTime, Duration, Utc};
use reqwest::StatusCode;
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione_w12";
const TEST_TOKEN_KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
/// Precedence tier 4. Every assertion that a workspace did NOT receive a
/// delegated or static credential checks for this instead.
const TEST_STATIC_BEARER: &str = "identity-broker-env-fallback";

// ─── harness ──────────────────────────────────────────────────────────────────

async fn spawn_app() -> (String, PgPool) {
    std::env::set_var("IONE_AUTH_MODE", "local");
    std::env::set_var("IONE_TOKEN_KEY", TEST_TOKEN_KEY);
    std::env::set_var("IONE_OAUTH_STATIC_BEARER", TEST_STATIC_BEARER);
    // The stub peer and the mock provider both live on loopback.
    std::env::set_var("IONE_ALLOW_PRIVATE_PEERS", "1");
    std::env::set_var("IONE_PRIVATE_PEER_ALLOWLIST", "127.0.0.1,localhost");
    std::env::remove_var("IONE_TEST_REVOKE_URL");

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
                  workspace_peer_credentials, broker_credentials, identity_audit_events,
                  user_sessions, mfa_enrollments, mfa_recovery_codes,
                  service_account_tokens, org_memberships, webhook_events_seen,
                  workspace_peer_bindings, audit_events, pipeline_events,
                  approvals, artifacts,
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

/// A peer that answers `resources/read`, publishes an OAuth authorization-server
/// metadata document, and runs a token endpoint — enough to complete a real
/// delegation dance and to record the bearer of every MCP request it receives.
struct StubPeer {
    mcp_url: String,
    auth_headers: Arc<Mutex<Vec<String>>>,
    grants: Arc<Mutex<Vec<String>>>,
}

/// What the stub peer's token endpoint returns for each grant type. Distinct
/// values so a test can tell an authorization-code grant from a refresh grant by
/// looking at the bearer the peer received.
const PEER_CODE_ACCESS: &str = "peer-delegated-access-from-code";
const PEER_REFRESHED_ACCESS: &str = "peer-delegated-access-from-refresh";

impl StubPeer {
    fn bearers(&self) -> Vec<String> {
        self.auth_headers.lock().expect("auth header mutex").clone()
    }

    fn last_bearer(&self) -> String {
        self.bearers().last().cloned().expect("no request recorded")
    }

    fn grants(&self) -> Vec<String> {
        self.grants.lock().expect("grant mutex").clone()
    }
}

async fn spawn_stub_peer() -> StubPeer {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind peer");
    let addr: SocketAddr = listener.local_addr().expect("peer addr");
    let base = format!("http://{addr}");

    let auth_headers = Arc::new(Mutex::new(Vec::new()));
    let grants = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&auth_headers);
    let captured_grants = Arc::clone(&grants);
    let discovery_base = base.clone();

    let app = axum::Router::new()
        .route(
            "/mcp",
            axum::routing::post(
                move |headers: HeaderMap, axum::Json(body): axum::Json<Value>| {
                    let captured = Arc::clone(&captured);
                    async move {
                        let header = headers
                            .get(axum::http::header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .unwrap_or("")
                            .to_string();
                        captured.lock().expect("auth header mutex").push(header);
                        let id = body.get("id").cloned().unwrap_or(Value::Null);
                        axum::Json(json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "contents": [{
                                    "uri": "table://demo",
                                    "mimeType": "application/json",
                                    "text": json!({
                                        "schema": [{ "name": "col", "type": "string" }],
                                        "rows": []
                                    }).to_string()
                                }]
                            }
                        }))
                    }
                },
            ),
        )
        .route(
            "/mcp/.well-known/oauth-authorization-server",
            axum::routing::get(move || {
                let base = discovery_base.clone();
                async move {
                    axum::Json(json!({
                        "authorization_endpoint": format!("{base}/authorize"),
                        "token_endpoint": format!("{base}/token"),
                        "registration_endpoint": format!("{base}/register"),
                    }))
                }
            }),
        )
        .route(
            "/token",
            axum::routing::post(
                move |axum::Form(form): axum::Form<HashMap<String, String>>| {
                    let captured_grants = Arc::clone(&captured_grants);
                    async move {
                        let grant = form.get("grant_type").cloned().unwrap_or_default();
                        captured_grants
                            .lock()
                            .expect("grant mutex")
                            .push(grant.clone());
                        let access = if grant == "refresh_token" {
                            PEER_REFRESHED_ACCESS
                        } else {
                            PEER_CODE_ACCESS
                        };
                        axum::Json(json!({
                            "access_token": access,
                            "refresh_token": "peer-delegated-refresh",
                            "expires_in": 3600,
                        }))
                    }
                },
            ),
        );

    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("peer server error");
    });
    StubPeer {
        mcp_url: format!("{base}/mcp"),
        auth_headers,
        grants,
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
    sqlx::query_scalar("SELECT id FROM users WHERE email = 'default@localhost' LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("default user not found")
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

/// An active peer that has already completed federation (`oauth_client_id` set),
/// which is the precondition for a workspace delegation.
async fn insert_active_peer(pool: &PgPool, name: &str, mcp_url: &str, issuer_id: Uuid) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO peers (name, mcp_url, issuer_id, sharing_policy, tool_allowlist,
                            status, oauth_client_id)
         VALUES ($1, $2, $3, '{}'::jsonb, '[]'::jsonb, 'active'::peer_status, 'ione-test-client')
         RETURNING id",
    )
    .bind(name)
    .bind(mcp_url)
    .bind(issuer_id)
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

async fn insert_token(pool: &PgPool, org_id: Uuid, permissions: &[&str]) -> String {
    let plaintext = format!("ione_sat_{}", Uuid::new_v4().simple());
    let hash = ione::auth::sha256_hex(&plaintext);
    sqlx::query(
        "INSERT INTO service_account_tokens (org_id, name, token_hash, permissions)
         VALUES ($1, $2, $3, $4::jsonb)",
    )
    .bind(org_id)
    .bind(format!("broker-{}", Uuid::new_v4().simple()))
    .bind(hash)
    .bind(json!(permissions))
    .execute(pool)
    .await
    .expect("insert service account token");
    plaintext
}

/// A connected SaaS broker credential owned by the default user (the principal
/// the broker connection routes resolve to in `IONE_AUTH_MODE=local`).
async fn insert_broker_credential(
    pool: &PgPool,
    user_id: Uuid,
    org_id: Uuid,
    access: &str,
    refresh: Option<&str>,
) -> Uuid {
    let access_cipher =
        ione::util::token_crypto::encrypt_versioned(access.as_bytes()).expect("encrypt access");
    let refresh_cipher = refresh.map(|token| {
        ione::util::token_crypto::encrypt_versioned(token.as_bytes()).expect("encrypt refresh")
    });
    sqlx::query_scalar(
        "INSERT INTO broker_credentials
           (user_id, org_id, provider, label, scopes, access_token_ciphertext,
            refresh_token_ciphertext, token_expires_at)
         VALUES ($1, $2, 'generic-test', 'primary', ARRAY['read'], $3, $4, now() - interval '1 minute')
         RETURNING id",
    )
    .bind(user_id)
    .bind(org_id)
    .bind(&access_cipher)
    .bind(refresh_cipher.as_deref())
    .fetch_one(pool)
    .await
    .expect("insert broker credential")
}

/// A brokered delegated token for (workspace, peer), as the callback would have
/// written it. `org_id` is derived by the wpd_check_same_org trigger.
async fn insert_delegation(
    pool: &PgPool,
    workspace_id: Uuid,
    peer_id: Uuid,
    token_endpoint: &str,
    access: &str,
    refresh: Option<&str>,
    expires_at: Option<DateTime<Utc>>,
) {
    let access_cipher =
        ione::util::token_crypto::encrypt_versioned(access.as_bytes()).expect("encrypt access");
    let refresh_cipher = refresh.map(|token| {
        ione::util::token_crypto::encrypt_versioned(token.as_bytes()).expect("encrypt refresh")
    });
    sqlx::query(
        "INSERT INTO workspace_peer_delegations
           (workspace_id, peer_id, oauth_client_id, token_endpoint,
            access_token_ciphertext, refresh_token_ciphertext, token_expires_at)
         VALUES ($1, $2, 'ione-test-client', $3, $4, $5, $6)",
    )
    .bind(workspace_id)
    .bind(peer_id)
    .bind(token_endpoint)
    .bind(&access_cipher)
    .bind(refresh_cipher.as_deref())
    .bind(expires_at)
    .execute(pool)
    .await
    .expect("insert delegation");
}

struct Fixture {
    base: String,
    pool: PgPool,
    org_id: Uuid,
    user_id: Uuid,
    workspace_id: Uuid,
    peer_id: Uuid,
    peer: StubPeer,
    token: String,
}

async fn fixture() -> Fixture {
    let (base, pool) = spawn_app().await;
    let org_id = default_org_id(&pool).await;
    let user_id = default_user_id(&pool).await;
    let workspace_id = default_workspace_id(&pool).await;
    let peer = spawn_stub_peer().await;
    let issuer_id = insert_trust_issuer(&pool, org_id, "https://iss-identity-broker.test").await;
    let peer_id = insert_active_peer(&pool, "Broker Peer", &peer.mcp_url, issuer_id).await;
    insert_active_binding(&pool, workspace_id, peer_id).await;
    let token = insert_token(&pool, org_id, &["peers:manage"]).await;
    Fixture {
        base,
        pool,
        org_id,
        user_id,
        workspace_id,
        peer_id,
        peer,
        token,
    }
}

// ─── API helpers ──────────────────────────────────────────────────────────────

/// Drives a real outbound MCP request in `workspace_id`'s scope: the table-data
/// route resolves the peer through `list_active_peers_for_workspace`, which is
/// what carries the workspace scope into outbound auth.
async fn read_table_data(base: &str, workspace_id: Uuid, peer_id: Uuid) -> reqwest::Response {
    reqwest::Client::new()
        .get(format!(
            "{base}/api/v1/workspaces/{workspace_id}/table-data?peerId={peer_id}&uri=table://demo"
        ))
        .send()
        .await
        .expect("table data")
}

fn delegation_url(base: &str, workspace_id: Uuid, peer_id: Uuid) -> String {
    format!("{base}/api/v1/workspaces/{workspace_id}/peers/{peer_id}/delegation")
}

async fn stored_access_token(pool: &PgPool, connection_id: Uuid) -> String {
    let ciphertext: Vec<u8> =
        sqlx::query_scalar("SELECT access_token_ciphertext FROM broker_credentials WHERE id = $1")
            .bind(connection_id)
            .fetch_one(pool)
            .await
            .expect("broker credential row");
    String::from_utf8(
        ione::util::token_crypto::decrypt_versioned(&ciphertext).expect("decrypt access token"),
    )
    .expect("utf-8 token")
}

async fn identity_audit_rows(pool: &PgPool, event_type: &str) -> Vec<(String, Value)> {
    sqlx::query_as(
        "SELECT outcome, detail FROM identity_audit_events
         WHERE event_type = $1 ORDER BY occurred_at",
    )
    .bind(event_type)
    .fetch_all(pool)
    .await
    .expect("identity audit rows")
}

// ─── GAP A: brokered SaaS OAuth refresh is real ───────────────────────────────

/// The refresh endpoint reaches the provider, stores a NEW ciphertext, and
/// audits success. Before #12 this endpoint contacted nobody and wrote an
/// unconditional `outcome = "success"` row.
#[tokio::test]
#[ignore]
async fn refresh_exchanges_at_the_provider_and_rewrites_the_stored_token() {
    let f = fixture().await;
    let provider = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "rotated-access-token",
            "refresh_token": "rotated-refresh-token",
            "expires_in": 3600,
        })))
        .expect(1)
        .mount(&provider)
        .await;
    std::env::set_var("IONE_TEST_TOKEN_URL", format!("{}/token", provider.uri()));

    let connection_id = insert_broker_credential(
        &f.pool,
        f.user_id,
        f.org_id,
        "stale-access-token",
        Some("stored-refresh-token"),
    )
    .await;
    let before: Vec<u8> =
        sqlx::query_scalar("SELECT access_token_ciphertext FROM broker_credentials WHERE id = $1")
            .bind(connection_id)
            .fetch_one(&f.pool)
            .await
            .expect("ciphertext before");

    let resp = reqwest::Client::new()
        .post(format!(
            "{}/api/v1/broker/connections/{connection_id}/refresh",
            f.base
        ))
        .send()
        .await
        .expect("refresh");
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("refresh body");
    assert!(!body["expiresAt"].is_null(), "no expiry returned: {body}");

    // The provider was actually called with the stored refresh token.
    let requests = provider
        .received_requests()
        .await
        .expect("recorded requests");
    assert_eq!(requests.len(), 1);
    let form = String::from_utf8(requests[0].body.clone()).expect("form body");
    assert!(
        form.contains("grant_type=refresh_token") && form.contains("stored-refresh-token"),
        "refresh grant not sent: {form}"
    );

    let after: Vec<u8> =
        sqlx::query_scalar("SELECT access_token_ciphertext FROM broker_credentials WHERE id = $1")
            .bind(connection_id)
            .fetch_one(&f.pool)
            .await
            .expect("ciphertext after");
    assert_ne!(before, after, "ciphertext was not rewritten");
    assert_eq!(
        stored_access_token(&f.pool, connection_id).await,
        "rotated-access-token"
    );

    let rows = identity_audit_rows(&f.pool, "token_broker_refresh").await;
    assert_eq!(rows.len(), 1, "expected one refresh audit row: {rows:?}");
    assert_eq!(rows[0].0, "success");
}

/// A provider failure is audited as a failure and does NOT clobber the stored
/// token — a failed refresh must never cost the operator a working connection.
#[tokio::test]
#[ignore]
async fn refresh_failure_audits_failure_and_preserves_the_stored_token() {
    let f = fixture().await;
    let provider = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(500).set_body_string("provider is down"))
        .mount(&provider)
        .await;
    std::env::set_var("IONE_TEST_TOKEN_URL", format!("{}/token", provider.uri()));

    let connection_id = insert_broker_credential(
        &f.pool,
        f.user_id,
        f.org_id,
        "surviving-access-token",
        Some("stored-refresh-token"),
    )
    .await;

    let resp = reqwest::Client::new()
        .post(format!(
            "{}/api/v1/broker/connections/{connection_id}/refresh",
            f.base
        ))
        .send()
        .await
        .expect("refresh");
    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    let body: Value = resp.json().await.expect("error body");
    assert_eq!(body["error"], "broker_upstream", "{body}");

    assert_eq!(
        stored_access_token(&f.pool, connection_id).await,
        "surviving-access-token",
        "a failed refresh clobbered the stored token"
    );

    let rows = identity_audit_rows(&f.pool, "token_broker_refresh").await;
    assert_eq!(rows.len(), 1, "expected one refresh audit row: {rows:?}");
    assert_eq!(rows[0].0, "failure");
    assert!(
        rows[0].1["failureReason"]
            .as_str()
            .unwrap_or_default()
            .starts_with("upstream:"),
        "failure reason not recorded: {:?}",
        rows[0].1
    );
    assert!(
        !serde_json::to_string(&rows[0].1)
            .expect("detail json")
            .contains("stored-refresh-token"),
        "audit detail leaked token material"
    );
}

/// A connection with no stored refresh token cannot be refreshed; that is a
/// client-state error, still audited as a failure.
#[tokio::test]
#[ignore]
async fn refresh_without_a_refresh_token_is_rejected_and_audited() {
    let f = fixture().await;
    let connection_id =
        insert_broker_credential(&f.pool, f.user_id, f.org_id, "access-only", None).await;

    let resp = reqwest::Client::new()
        .post(format!(
            "{}/api/v1/broker/connections/{connection_id}/refresh",
            f.base
        ))
        .send()
        .await
        .expect("refresh");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let rows = identity_audit_rows(&f.pool, "token_broker_refresh").await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "failure");
    assert_eq!(rows[0].1["failureReason"], "no_refresh_token");
}

/// DELETE attempts upstream revocation before removing the local row.
#[tokio::test]
#[ignore]
async fn delete_revokes_upstream_before_removing_the_local_row() {
    let f = fixture().await;
    let provider = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/revoke"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&provider)
        .await;
    std::env::set_var("IONE_TEST_REVOKE_URL", format!("{}/revoke", provider.uri()));

    let connection_id = insert_broker_credential(
        &f.pool,
        f.user_id,
        f.org_id,
        "access-to-revoke",
        Some("refresh-to-revoke"),
    )
    .await;

    let resp = reqwest::Client::new()
        .delete(format!(
            "{}/api/v1/broker/connections/{connection_id}",
            f.base
        ))
        .send()
        .await
        .expect("delete");
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let requests = provider
        .received_requests()
        .await
        .expect("recorded requests");
    assert_eq!(requests.len(), 1, "upstream revocation was not attempted");
    let form = String::from_utf8(requests[0].body.clone()).expect("form body");
    assert!(
        form.contains("refresh-to-revoke") && form.contains("token_type_hint=refresh_token"),
        "revocation did not present the refresh token: {form}"
    );

    let remaining: i64 =
        sqlx::query_scalar("SELECT count(*) FROM broker_credentials WHERE id = $1")
            .bind(connection_id)
            .fetch_one(&f.pool)
            .await
            .expect("count");
    assert_eq!(remaining, 0, "local row survived the delete");

    assert!(
        identity_audit_rows(&f.pool, "token_broker_revoke_upstream_failed")
            .await
            .is_empty()
    );
    let rows = identity_audit_rows(&f.pool, "token_broker_revoke").await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "success");
    assert_eq!(rows[0].1["upstreamRevoked"], json!(true));

    std::env::remove_var("IONE_TEST_REVOKE_URL");
}

/// An upstream revocation failure emits `token_broker_revoke_upstream_failed`
/// and still removes the local row — the documented behavior from
/// md/design/identity-broker.md S5. The audit row is the record that a
/// credential may still be live at the provider.
#[tokio::test]
#[ignore]
async fn delete_upstream_failure_is_audited_and_still_removes_the_local_row() {
    let f = fixture().await;
    let provider = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/revoke"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&provider)
        .await;
    std::env::set_var("IONE_TEST_REVOKE_URL", format!("{}/revoke", provider.uri()));

    let connection_id = insert_broker_credential(
        &f.pool,
        f.user_id,
        f.org_id,
        "access-to-revoke",
        Some("refresh-to-revoke"),
    )
    .await;

    let resp = reqwest::Client::new()
        .delete(format!(
            "{}/api/v1/broker/connections/{connection_id}",
            f.base
        ))
        .send()
        .await
        .expect("delete");
    assert_eq!(
        resp.status(),
        StatusCode::NO_CONTENT,
        "an upstream failure must not orphan the local row"
    );

    let remaining: i64 =
        sqlx::query_scalar("SELECT count(*) FROM broker_credentials WHERE id = $1")
            .bind(connection_id)
            .fetch_one(&f.pool)
            .await
            .expect("count");
    assert_eq!(remaining, 0, "local row survived the delete");

    let failed = identity_audit_rows(&f.pool, "token_broker_revoke_upstream_failed").await;
    assert_eq!(failed.len(), 1, "upstream failure was not audited");
    assert_eq!(failed[0].0, "failure");
    assert!(
        !serde_json::to_string(&failed[0].1)
            .expect("detail json")
            .contains("refresh-to-revoke"),
        "audit detail leaked token material"
    );

    let revoked = identity_audit_rows(&f.pool, "token_broker_revoke").await;
    assert_eq!(revoked.len(), 1);
    assert_eq!(revoked[0].1["upstreamRevoked"], json!(false));

    std::env::remove_var("IONE_TEST_REVOKE_URL");
}

// ─── GAP B: delegated-token storage per (workspace, peer) ─────────────────────

/// A brokered delegated token stored for (workspace, peer) is the bearer on
/// outbound MCP requests made in that workspace's scope.
#[tokio::test]
#[ignore]
async fn delegated_token_resolves_per_workspace_and_peer() {
    let f = fixture().await;
    insert_delegation(
        &f.pool,
        f.workspace_id,
        f.peer_id,
        "http://unused.invalid/token",
        "workspace-a-delegated-token",
        None,
        Some(Utc::now() + Duration::hours(1)),
    )
    .await;

    assert_eq!(
        read_table_data(&f.base, f.workspace_id, f.peer_id)
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(f.peer.last_bearer(), "Bearer workspace-a-delegated-token");
}

/// A second workspace bound to the same peer does not inherit the first
/// workspace's delegation — the delegation is scoped, not peer-wide.
#[tokio::test]
#[ignore]
async fn delegated_token_does_not_leak_to_another_workspace_on_the_same_peer() {
    let f = fixture().await;
    let other_workspace = insert_workspace(&f.pool, f.org_id, "Other Workspace").await;
    insert_active_binding(&f.pool, other_workspace, f.peer_id).await;
    insert_delegation(
        &f.pool,
        f.workspace_id,
        f.peer_id,
        "http://unused.invalid/token",
        "workspace-a-only-delegated",
        None,
        Some(Utc::now() + Duration::hours(1)),
    )
    .await;

    assert_eq!(
        read_table_data(&f.base, f.workspace_id, f.peer_id)
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(f.peer.last_bearer(), "Bearer workspace-a-only-delegated");

    assert_eq!(
        read_table_data(&f.base, other_workspace, f.peer_id)
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        f.peer.last_bearer(),
        format!("Bearer {TEST_STATIC_BEARER}"),
        "the unscoped workspace must not receive workspace A's delegation"
    );
}

/// Backward compatibility: a peer-global OAuth token keeps working untouched
/// when no delegation exists for the handle.
#[tokio::test]
#[ignore]
async fn peer_global_oauth_token_still_works_without_a_delegation() {
    let f = fixture().await;
    let ciphertext = ione::util::token_crypto::encrypt_token("peer-global-oauth-token")
        .expect("encrypt peer token");
    sqlx::query(
        "UPDATE peers SET access_token_ciphertext = $2, token_expires_at = now() + interval '1 hour'
         WHERE id = $1",
    )
    .bind(f.peer_id)
    .bind(&ciphertext)
    .execute(&f.pool)
    .await
    .expect("set peer oauth token");

    assert_eq!(
        read_table_data(&f.base, f.workspace_id, f.peer_id)
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(f.peer.last_bearer(), "Bearer peer-global-oauth-token");
}

/// Full precedence: delegation > peer-global OAuth > per-(workspace, peer)
/// static credential (#19) > env bearer. Removing each tier reveals the next,
/// which is exactly issue #19's stated fallback semantics.
#[tokio::test]
#[ignore]
async fn precedence_is_delegation_then_peer_oauth_then_static_then_env() {
    let f = fixture().await;

    // Tier 3 (#19).
    let put = reqwest::Client::new()
        .put(format!(
            "{}/api/v1/workspaces/{}/peers/{}/credential",
            f.base, f.workspace_id, f.peer_id
        ))
        .bearer_auth(&f.token)
        .json(&json!({ "credential": "static-credential-tier-3" }))
        .send()
        .await
        .expect("put credential");
    assert_eq!(put.status(), StatusCode::CREATED);

    // Tier 2.
    let ciphertext =
        ione::util::token_crypto::encrypt_token("peer-global-tier-2").expect("encrypt peer token");
    sqlx::query(
        "UPDATE peers SET access_token_ciphertext = $2, token_expires_at = now() + interval '1 hour'
         WHERE id = $1",
    )
    .bind(f.peer_id)
    .bind(&ciphertext)
    .execute(&f.pool)
    .await
    .expect("set peer oauth token");

    // Tier 1.
    insert_delegation(
        &f.pool,
        f.workspace_id,
        f.peer_id,
        "http://unused.invalid/token",
        "delegated-tier-1",
        None,
        Some(Utc::now() + Duration::hours(1)),
    )
    .await;

    assert_eq!(
        read_table_data(&f.base, f.workspace_id, f.peer_id)
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(f.peer.last_bearer(), "Bearer delegated-tier-1");

    // Drop tier 1 → tier 2.
    let resp = reqwest::Client::new()
        .delete(delegation_url(&f.base, f.workspace_id, f.peer_id))
        .bearer_auth(&f.token)
        .send()
        .await
        .expect("delete delegation");
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        read_table_data(&f.base, f.workspace_id, f.peer_id)
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(f.peer.last_bearer(), "Bearer peer-global-tier-2");

    // Drop tier 2 → tier 3 (#19's static credential).
    sqlx::query("UPDATE peers SET access_token_ciphertext = NULL WHERE id = $1")
        .bind(f.peer_id)
        .execute(&f.pool)
        .await
        .expect("clear peer oauth token");
    assert_eq!(
        read_table_data(&f.base, f.workspace_id, f.peer_id)
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(f.peer.last_bearer(), "Bearer static-credential-tier-3");

    // Drop tier 3 → tier 4 (env).
    let resp = reqwest::Client::new()
        .delete(format!(
            "{}/api/v1/workspaces/{}/peers/{}/credential",
            f.base, f.workspace_id, f.peer_id
        ))
        .bearer_auth(&f.token)
        .send()
        .await
        .expect("delete credential");
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        read_table_data(&f.base, f.workspace_id, f.peer_id)
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(f.peer.last_bearer(), format!("Bearer {TEST_STATIC_BEARER}"));
}

/// An expired delegation with a refresh token is refreshed at the peer's token
/// endpoint on the next outbound call, and the new material is persisted.
#[tokio::test]
#[ignore]
async fn expired_delegation_is_refreshed_at_the_peer_token_endpoint() {
    let f = fixture().await;
    let token_endpoint = f.peer.mcp_url.replace("/mcp", "/token");
    insert_delegation(
        &f.pool,
        f.workspace_id,
        f.peer_id,
        &token_endpoint,
        "expired-delegated-token",
        Some("peer-delegated-refresh"),
        Some(Utc::now() - Duration::minutes(5)),
    )
    .await;

    assert_eq!(
        read_table_data(&f.base, f.workspace_id, f.peer_id)
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(f.peer.grants(), vec!["refresh_token".to_string()]);
    assert_eq!(
        f.peer.last_bearer(),
        format!("Bearer {PEER_REFRESHED_ACCESS}")
    );

    let expires_at: Option<DateTime<Utc>> = sqlx::query_scalar(
        "SELECT token_expires_at FROM workspace_peer_delegations
         WHERE workspace_id = $1 AND peer_id = $2",
    )
    .bind(f.workspace_id)
    .bind(f.peer_id)
    .fetch_one(&f.pool)
    .await
    .expect("delegation row");
    assert!(
        expires_at.expect("expiry") > Utc::now(),
        "refreshed delegation still expired"
    );

    // A second call uses the persisted token without another refresh grant.
    assert_eq!(
        read_table_data(&f.base, f.workspace_id, f.peer_id)
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(f.peer.grants().len(), 1, "token was refreshed twice");
}

/// An expired delegation with no refresh token is unusable, so outbound auth
/// falls through to the next precedence tier rather than presenting a token the
/// peer will reject.
#[tokio::test]
#[ignore]
async fn expired_delegation_without_refresh_falls_through() {
    let f = fixture().await;
    insert_delegation(
        &f.pool,
        f.workspace_id,
        f.peer_id,
        "http://unused.invalid/token",
        "dead-delegated-token",
        None,
        Some(Utc::now() - Duration::minutes(5)),
    )
    .await;

    assert_eq!(
        read_table_data(&f.base, f.workspace_id, f.peer_id)
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(f.peer.last_bearer(), format!("Bearer {TEST_STATIC_BEARER}"));
}

/// Delegation management is gated on `peers:manage` and is org-scoped.
#[tokio::test]
#[ignore]
async fn delegation_management_is_rbac_and_org_scoped() {
    let f = fixture().await;
    let weak = insert_token(&f.pool, f.org_id, &["workspace:read"]).await;
    let other_org = insert_org(&f.pool, "Other Org").await;
    let outsider = insert_token(&f.pool, other_org, &["peers:manage"]).await;

    assert_eq!(
        reqwest::Client::new()
            .post(delegation_url(&f.base, f.workspace_id, f.peer_id))
            .bearer_auth(&weak)
            .send()
            .await
            .expect("begin delegation")
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        reqwest::Client::new()
            .post(delegation_url(&f.base, f.workspace_id, f.peer_id))
            .bearer_auth(&outsider)
            .send()
            .await
            .expect("begin delegation")
            .status(),
        StatusCode::NOT_FOUND
    );
}

/// End-to-end acceptance for issue #12: the operator authenticates once against
/// IONe, completes one delegation, and every later peer `/mcp` call carries the
/// brokered delegated token — no second login at the peer, and no request ever
/// leaves IONe without a bearer.
#[tokio::test]
#[ignore]
async fn operator_authenticates_once_then_peer_mcp_carries_the_delegated_token() {
    let f = fixture().await;
    // One IONe credential for every operator-side call in this test.
    let operator = reqwest::Client::new();

    let begin: Value = operator
        .post(delegation_url(&f.base, f.workspace_id, f.peer_id))
        .bearer_auth(&f.token)
        .send()
        .await
        .expect("begin delegation")
        .json()
        .await
        .expect("begin body");
    let authorize_url = begin["authorizeUrl"].as_str().expect("authorizeUrl");
    assert!(
        authorize_url.contains("code_challenge_method=S256"),
        "authorize url is not PKCE: {authorize_url}"
    );

    // The peer's authorization server would redirect the operator back with the
    // code and the state nonce it was handed.
    let nonce: String =
        sqlx::query_scalar("SELECT nonce FROM workspace_peer_delegation_pending LIMIT 1")
            .fetch_one(&f.pool)
            .await
            .expect("pending delegation");
    let callback = operator
        .get(format!(
            "{}/auth/broker/peer-callback?code=peer-auth-code&state={}",
            f.base,
            urlencoding::encode(&nonce)
        ))
        .send()
        .await
        .expect("peer callback");
    assert!(
        callback.status().is_success(),
        "callback failed: {}",
        callback.status()
    );
    assert_eq!(f.peer.grants(), vec!["authorization_code".to_string()]);

    // The pending row is single-use: a replay finds nothing.
    let replay = operator
        .get(format!(
            "{}/auth/broker/peer-callback?code=peer-auth-code&state={}",
            f.base,
            urlencoding::encode(&nonce)
        ))
        .send()
        .await
        .expect("replayed callback");
    assert_eq!(replay.status(), StatusCode::BAD_REQUEST);

    // Two later workspace requests, no further operator authentication anywhere.
    for _ in 0..2 {
        assert_eq!(
            read_table_data(&f.base, f.workspace_id, f.peer_id)
                .await
                .status(),
            StatusCode::OK
        );
    }
    let bearers = f.peer.bearers();
    assert_eq!(bearers.len(), 2, "unexpected peer traffic: {bearers:?}");
    for bearer in &bearers {
        assert_eq!(bearer, &format!("Bearer {PEER_CODE_ACCESS}"));
    }

    // The grant is audited at the fabric level, against the peer, with no token
    // material in the detail payload.
    let rows = identity_audit_rows(&f.pool, "token_broker_grant").await;
    assert_eq!(rows.len(), 1, "grant not audited: {rows:?}");
    assert_eq!(rows[0].0, "success");
    assert_eq!(rows[0].1["workspaceId"], json!(f.workspace_id));
    assert_eq!(rows[0].1["peerId"], json!(f.peer_id));
    assert!(
        !serde_json::to_string(&rows[0].1)
            .expect("detail json")
            .contains(PEER_CODE_ACCESS),
        "audit detail leaked the delegated token"
    );
    let peer_id_column: Option<Uuid> = sqlx::query_scalar(
        "SELECT peer_id FROM identity_audit_events WHERE event_type = 'token_broker_grant'",
    )
    .fetch_one(&f.pool)
    .await
    .expect("audit peer_id");
    assert_eq!(peer_id_column, Some(f.peer_id));

    // No delegated token material is readable back out of the API.
    let metadata = operator
        .get(delegation_url(&f.base, f.workspace_id, f.peer_id))
        .bearer_auth(&f.token)
        .send()
        .await
        .expect("get delegation")
        .text()
        .await
        .expect("get body");
    assert!(
        !metadata.contains(PEER_CODE_ACCESS) && !metadata.contains("peer-delegated-refresh"),
        "delegation read surface echoed token material: {metadata}"
    );
}

// ─── GAP C: RLS policies still do not bind the default connection role ────────

/// AC-15 of md/design/identity-broker.md is satisfied only for a deployment that
/// connects as the restricted `ione_app` role. Under the default `ione`
/// connection — every existing test, `docker compose`, CI, and the dev loop —
/// the policies still do not filter anything, and this test proves the actual
/// state rather than the intended one.
///
/// Migration 0050 fixed two of the three original reasons: every org-scoped
/// table now declares `FORCE ROW LEVEL SECURITY`, and a non-owner,
/// non-BYPASSRLS role exists. What remains is the third: `ione` is SUPERUSER and
/// holds BYPASSRLS, and Postgres lets such a role past the row-security system
/// unconditionally — `FORCE` does not apply to it. So the guard on this
/// connection is still the application-layer `WHERE org_id = $n` predicate,
/// covered by `delegation_management_is_rbac_and_org_scoped` and the existing
/// rbac/binding suites.
///
/// The positive half — the same policies filtering rows for real — lives in
/// tests/rls_enforcement_integration.rs, which connects as `ione_app`.
///
/// This test fails the moment the default role stops bypassing RLS, which is the
/// point at which every unmigrated repository query becomes org-blind and the
/// remaining migration work in md/design/identity-broker.md must be finished.
#[tokio::test]
#[ignore]
async fn rls_policies_are_present_but_inert_as_deployed() {
    let f = fixture().await;
    let guarded = [
        "broker_credentials",
        "mfa_enrollments",
        "mfa_recovery_codes",
        "identity_audit_events",
        "service_account_tokens",
        "workspace_peer_bindings",
        "workspace_peer_delegations",
    ];

    for table in guarded {
        let policies: i64 =
            sqlx::query_scalar("SELECT count(*) FROM pg_policies WHERE tablename = $1")
                .bind(table)
                .fetch_one(&f.pool)
                .await
                .expect("policy count");
        assert!(policies > 0, "{table} has no RLS policy");

        let (enabled, forced): (bool, bool) = sqlx::query_as(
            "SELECT c.relrowsecurity, c.relforcerowsecurity
             FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
             WHERE n.nspname = 'public' AND c.relname = $1",
        )
        .bind(table)
        .fetch_one(&f.pool)
        .await
        .expect("relation flags");
        assert!(enabled, "{table} does not have RLS enabled");
        assert!(
            forced,
            "{table} does not FORCE RLS — migration 0050 regressed"
        );
    }

    // The default role bypasses regardless of FORCE, because it is SUPERUSER and
    // holds BYPASSRLS. That is why activating RLS broke no existing suite.
    let (superuser, bypassrls): (bool, bool) =
        sqlx::query_as("SELECT rolsuper, rolbypassrls FROM pg_roles WHERE rolname = current_user")
            .fetch_one(&f.pool)
            .await
            .expect("default role attributes");
    assert!(
        superuser || bypassrls,
        "the default connection role no longer bypasses RLS — every unmigrated \
         repository query is now org-blind; finish the AC-15 migration"
    );

    // Only the migrated repositories set the org context, and they set it with
    // transaction scope, so nothing is left behind on an idle pooled connection.
    assert_eq!(
        read_table_data(&f.base, f.workspace_id, f.peer_id)
            .await
            .status(),
        StatusCode::OK
    );
    let org_context: Option<String> =
        sqlx::query_scalar("SELECT current_setting('app.current_org_id', true)")
            .fetch_one(&f.pool)
            .await
            .expect("org context");
    assert!(
        org_context.is_none() || org_context.as_deref() == Some(""),
        "app.current_org_id is unexpectedly set to {org_context:?}"
    );

    // And even with the variable set to a foreign org, the policy does not
    // constrain the application role.
    let other_org = insert_org(&f.pool, "RLS Other Org").await;
    insert_broker_credential(&f.pool, f.user_id, f.org_id, "visible-anyway", None).await;

    let mut conn = f.pool.acquire().await.expect("pinned connection");
    sqlx::query(&format!("SET app.current_org_id = '{other_org}'"))
        .execute(&mut *conn)
        .await
        .expect("set org context");
    let visible: i64 =
        sqlx::query_scalar("SELECT count(*) FROM broker_credentials WHERE org_id = $1")
            .bind(f.org_id)
            .fetch_one(&mut *conn)
            .await
            .expect("count under foreign org context");
    assert_eq!(
        visible, 1,
        "RLS is now enforced for the default connection role — AC-15 can be \
         claimed for the whole binary and this test should be replaced"
    );
}
