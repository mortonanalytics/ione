//! Pre-broker per-(workspace, peer) static credentials (issue #19).
//!
//! Design: `md/design/pre-broker-peer-credentials.md`.
//!
//! Run (serial, ignored):
//!   DATABASE_URL=postgres://ione:ione@localhost:5433/ione_w19 \
//!     IONE_SKIP_LIVE=1 \
//!     cargo test --test peer_credential_integration -- --ignored --test-threads=1

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::http::HeaderMap;
use reqwest::StatusCode;
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;
use uuid::Uuid;

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione_w19";
const TEST_TOKEN_KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
/// The process-global fallback (precedence tier 3). Every assertion that a
/// workspace did NOT get a per-workspace credential checks for this instead.
const TEST_STATIC_BEARER: &str = "peer-cred-env-fallback";

// ─── harness ──────────────────────────────────────────────────────────────────

async fn spawn_app() -> (String, PgPool) {
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
        "TRUNCATE workspace_peer_credentials, service_account_tokens, org_memberships,
                  webhook_events_seen, workspace_peer_bindings, audit_events, pipeline_events,
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

/// A peer that answers `resources/read` with a minimal table resource and
/// records the `Authorization` header of every request it receives.
struct RecordedPeer {
    mcp_url: String,
    auth_headers: Arc<Mutex<Vec<String>>>,
}

impl RecordedPeer {
    fn bearers(&self) -> Vec<String> {
        self.auth_headers.lock().expect("auth header mutex").clone()
    }

    fn last_bearer(&self) -> String {
        self.bearers().last().cloned().expect("no request recorded")
    }
}

async fn spawn_recorded_peer() -> RecordedPeer {
    let auth_headers = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&auth_headers);

    let app = axum::Router::new().route(
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
    );

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind peer");
    let addr: SocketAddr = listener.local_addr().expect("peer addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("peer server error");
    });
    RecordedPeer {
        mcp_url: format!("http://{}/mcp", addr),
        auth_headers,
    }
}

// ─── fixtures ─────────────────────────────────────────────────────────────────

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

async fn insert_active_peer(pool: &PgPool, name: &str, mcp_url: &str, issuer_id: Uuid) -> Uuid {
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

/// A service-account token is the only way to drive the RBAC gate with an exact
/// grant set (and an exact org), so every credential-management call uses one.
async fn insert_token(pool: &PgPool, org_id: Uuid, permissions: &[&str]) -> String {
    let plaintext = format!("ione_sat_{}", Uuid::new_v4().simple());
    let hash = ione::auth::sha256_hex(&plaintext);
    sqlx::query(
        "INSERT INTO service_account_tokens (org_id, name, token_hash, permissions)
         VALUES ($1, $2, $3, $4::jsonb)",
    )
    .bind(org_id)
    .bind(format!("cred-{}", Uuid::new_v4().simple()))
    .bind(hash)
    .bind(json!(permissions))
    .execute(pool)
    .await
    .expect("insert service account token");
    plaintext
}

/// Standard fixture: default org/workspace, an active peer backed by a recorded
/// mock, an active binding, and a `peers:manage` token.
struct Fixture {
    base: String,
    pool: PgPool,
    org_id: Uuid,
    workspace_id: Uuid,
    peer_id: Uuid,
    peer: RecordedPeer,
    token: String,
}

async fn fixture() -> Fixture {
    let (base, pool) = spawn_app().await;
    let org_id = default_org_id(&pool).await;
    let workspace_id = default_workspace_id(&pool).await;
    let peer = spawn_recorded_peer().await;
    let issuer_id = insert_trust_issuer(&pool, org_id, "https://iss-peer-cred.test").await;
    let peer_id = insert_active_peer(&pool, "Cred Peer", &peer.mcp_url, issuer_id).await;
    insert_active_binding(&pool, workspace_id, peer_id).await;
    let token = insert_token(&pool, org_id, &["peers:manage"]).await;
    Fixture {
        base,
        pool,
        org_id,
        workspace_id,
        peer_id,
        peer,
        token,
    }
}

// ─── API helpers ──────────────────────────────────────────────────────────────

fn credential_url(base: &str, workspace_id: Uuid, peer_id: Uuid) -> String {
    format!("{base}/api/v1/workspaces/{workspace_id}/peers/{peer_id}/credential")
}

async fn put_credential(
    base: &str,
    token: &str,
    workspace_id: Uuid,
    peer_id: Uuid,
    credential: Option<&str>,
) -> reqwest::Response {
    let body = match credential {
        Some(value) => json!({ "credential": value }),
        None => json!({}),
    };
    reqwest::Client::new()
        .put(credential_url(base, workspace_id, peer_id))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .expect("put credential")
}

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

// ─── tests ────────────────────────────────────────────────────────────────────

/// The stored credential is presented verbatim as `Authorization: Bearer <value>`
/// on an outbound MCP request made in that workspace's scope.
#[tokio::test]
#[ignore]
async fn credential_is_presented_as_bearer_on_outbound_mcp_request() {
    let f = fixture().await;
    let secret = "peer-issued-key-abc123";

    let resp = put_credential(&f.base, &f.token, f.workspace_id, f.peer_id, Some(secret)).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body: Value = resp.json().await.expect("put body");
    assert_eq!(
        body["credential"], secret,
        "plaintext returned once: {body}"
    );

    let resp = read_table_data(&f.base, f.workspace_id, f.peer_id).await;
    assert_eq!(resp.status(), StatusCode::OK, "table-data should succeed");
    assert_eq!(f.peer.last_bearer(), format!("Bearer {secret}"));
}

/// A second workspace bound to the same peer does not inherit the first
/// workspace's credential; it falls through to the process-global fallback.
#[tokio::test]
#[ignore]
async fn credential_does_not_leak_to_another_workspace_on_the_same_peer() {
    let f = fixture().await;
    let other_workspace = insert_workspace(&f.pool, f.org_id, "Other Workspace").await;
    insert_active_binding(&f.pool, other_workspace, f.peer_id).await;

    let secret = "workspace-a-only-key";
    assert_eq!(
        put_credential(&f.base, &f.token, f.workspace_id, f.peer_id, Some(secret))
            .await
            .status(),
        StatusCode::CREATED
    );

    assert_eq!(
        read_table_data(&f.base, f.workspace_id, f.peer_id)
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(f.peer.last_bearer(), format!("Bearer {secret}"));

    assert_eq!(
        read_table_data(&f.base, other_workspace, f.peer_id)
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        f.peer.last_bearer(),
        format!("Bearer {TEST_STATIC_BEARER}"),
        "the unscoped workspace must not receive workspace A's credential"
    );
}

/// The column holds ciphertext, not the secret, and no read surface echoes it.
#[tokio::test]
#[ignore]
async fn credential_is_encrypted_at_rest_and_never_echoed_by_reads() {
    let f = fixture().await;
    let secret = "never-echo-this-value";
    assert_eq!(
        put_credential(&f.base, &f.token, f.workspace_id, f.peer_id, Some(secret))
            .await
            .status(),
        StatusCode::CREATED
    );

    let ciphertext: Vec<u8> = sqlx::query_scalar(
        "SELECT credential_ciphertext FROM workspace_peer_credentials
         WHERE workspace_id = $1 AND peer_id = $2",
    )
    .bind(f.workspace_id)
    .bind(f.peer_id)
    .fetch_one(&f.pool)
    .await
    .expect("ciphertext row");
    assert_ne!(ciphertext, secret.as_bytes(), "column stored the plaintext");
    assert!(
        !String::from_utf8_lossy(&ciphertext).contains(secret),
        "ciphertext contains the plaintext"
    );
    assert_eq!(
        ciphertext.first().copied(),
        Some(ione::util::token_crypto::TOKEN_KEY_VERSION_CURRENT),
        "credential is not stored in the versioned envelope"
    );

    let get_body = reqwest::Client::new()
        .get(credential_url(&f.base, f.workspace_id, f.peer_id))
        .bearer_auth(&f.token)
        .send()
        .await
        .expect("get credential")
        .text()
        .await
        .expect("get body");
    assert!(
        !get_body.contains(secret),
        "GET echoed the secret: {get_body}"
    );
    assert!(get_body.contains("createdAt"), "GET body: {get_body}");

    let list_body = reqwest::Client::new()
        .get(format!(
            "{}/api/v1/workspaces/{}/peer-credentials",
            f.base, f.workspace_id
        ))
        .bearer_auth(&f.token)
        .send()
        .await
        .expect("list credentials")
        .text()
        .await
        .expect("list body");
    assert!(
        !list_body.contains(secret),
        "list echoed the secret: {list_body}"
    );
}

/// Rotation replaces the credential in place: the new value is sent, the old one
/// stops being sent, and the applied-migration count is unchanged.
#[tokio::test]
#[ignore]
async fn rotation_replaces_the_credential_without_a_migration() {
    let f = fixture().await;
    let migrations_before: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations")
        .fetch_one(&f.pool)
        .await
        .expect("migration count");

    let first = "rotation-v1";
    let second = "rotation-v2";
    assert_eq!(
        put_credential(&f.base, &f.token, f.workspace_id, f.peer_id, Some(first))
            .await
            .status(),
        StatusCode::CREATED
    );
    assert_eq!(
        read_table_data(&f.base, f.workspace_id, f.peer_id)
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(f.peer.last_bearer(), format!("Bearer {first}"));

    let resp = put_credential(&f.base, &f.token, f.workspace_id, f.peer_id, Some(second)).await;
    assert_eq!(resp.status(), StatusCode::OK, "rotation is not a create");
    let body: Value = resp.json().await.expect("rotate body");
    assert_eq!(body["credential"], second);
    assert!(!body["rotatedAt"].is_null(), "rotatedAt not set: {body}");

    assert_eq!(
        read_table_data(&f.base, f.workspace_id, f.peer_id)
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(f.peer.last_bearer(), format!("Bearer {second}"));
    assert!(
        !f.peer
            .bearers()
            .iter()
            .skip_while(|bearer| bearer.as_str() != format!("Bearer {second}"))
            .any(|bearer| bearer == &format!("Bearer {first}")),
        "the rotated-out credential was still sent"
    );

    let rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM workspace_peer_credentials WHERE workspace_id = $1 AND peer_id = $2",
    )
    .bind(f.workspace_id)
    .bind(f.peer_id)
    .fetch_one(&f.pool)
    .await
    .expect("credential count");
    assert_eq!(rows, 1, "rotation must replace, not accumulate");

    let migrations_after: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations")
        .fetch_one(&f.pool)
        .await
        .expect("migration count");
    assert_eq!(
        migrations_before, migrations_after,
        "rotation must not involve a migration"
    );
}

/// Precedence: a brokered OAuth access token outranks the static credential, so
/// #12 supersedes this mode without a flag day.
#[tokio::test]
#[ignore]
async fn oauth_token_takes_precedence_over_static_credential() {
    let f = fixture().await;
    let static_secret = "static-should-lose";
    let oauth_token = "brokered-access-token";
    assert_eq!(
        put_credential(
            &f.base,
            &f.token,
            f.workspace_id,
            f.peer_id,
            Some(static_secret)
        )
        .await
        .status(),
        StatusCode::CREATED
    );

    // peer_tokens decrypts access_token_ciphertext with the legacy un-versioned
    // envelope, matching how the OAuth callback writes it.
    let ciphertext =
        ione::util::token_crypto::encrypt_token(oauth_token).expect("encrypt oauth token");
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
    assert_eq!(f.peer.last_bearer(), format!("Bearer {oauth_token}"));
}

/// Delete removes the credential; outbound auth falls back on the next request.
#[tokio::test]
#[ignore]
async fn delete_removes_the_credential_and_falls_back() {
    let f = fixture().await;
    assert_eq!(
        put_credential(
            &f.base,
            &f.token,
            f.workspace_id,
            f.peer_id,
            Some("to-be-deleted")
        )
        .await
        .status(),
        StatusCode::CREATED
    );

    let resp = reqwest::Client::new()
        .delete(credential_url(&f.base, f.workspace_id, f.peer_id))
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

    assert_eq!(
        reqwest::Client::new()
            .get(credential_url(&f.base, f.workspace_id, f.peer_id))
            .bearer_auth(&f.token)
            .send()
            .await
            .expect("get after delete")
            .status(),
        StatusCode::NOT_FOUND
    );
}

/// Create / rotate / delete each emit an audit row, and none of them carries the
/// secret in the payload.
#[tokio::test]
#[ignore]
async fn credential_lifecycle_is_audited_without_secrets() {
    let f = fixture().await;
    let first = "audit-secret-one";
    let second = "audit-secret-two";
    assert_eq!(
        put_credential(&f.base, &f.token, f.workspace_id, f.peer_id, Some(first))
            .await
            .status(),
        StatusCode::CREATED
    );
    assert_eq!(
        put_credential(&f.base, &f.token, f.workspace_id, f.peer_id, Some(second))
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        reqwest::Client::new()
            .delete(credential_url(&f.base, f.workspace_id, f.peer_id))
            .bearer_auth(&f.token)
            .send()
            .await
            .expect("delete credential")
            .status(),
        StatusCode::NO_CONTENT
    );

    let rows: Vec<(String, String, Value)> = sqlx::query_as(
        "SELECT verb, actor_kind::text, payload FROM audit_events
         WHERE object_kind = 'peer_credential' ORDER BY created_at",
    )
    .fetch_all(&f.pool)
    .await
    .expect("audit rows");
    let verbs: Vec<&str> = rows.iter().map(|(verb, _, _)| verb.as_str()).collect();
    assert_eq!(
        verbs,
        vec![
            "peer_credential.created",
            "peer_credential.rotated",
            "peer_credential.deleted"
        ]
    );
    for (verb, actor_kind, payload) in &rows {
        assert_eq!(actor_kind, "service_account", "{verb}");
        let serialized = serde_json::to_string(payload).expect("payload json");
        assert!(
            !serialized.contains(first) && !serialized.contains(second),
            "audit payload for {verb} leaked a secret: {serialized}"
        );
    }
}

/// RBAC: managing a credential is gated on the same `peers:manage` used by the
/// binding routes.
#[tokio::test]
#[ignore]
async fn credential_management_403_without_peers_manage() {
    let f = fixture().await;
    let weak = insert_token(&f.pool, f.org_id, &["workspace:read"]).await;

    assert_eq!(
        put_credential(&f.base, &weak, f.workspace_id, f.peer_id, Some("nope"))
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        reqwest::Client::new()
            .get(credential_url(&f.base, f.workspace_id, f.peer_id))
            .bearer_auth(&weak)
            .send()
            .await
            .expect("get credential")
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        reqwest::Client::new()
            .delete(credential_url(&f.base, f.workspace_id, f.peer_id))
            .bearer_auth(&weak)
            .send()
            .await
            .expect("delete credential")
            .status(),
        StatusCode::FORBIDDEN
    );
}

/// Cross-org access 404s rather than leaking the existence of the workspace or
/// peer, even when the caller holds `peers:manage` in its own org.
#[tokio::test]
#[ignore]
async fn credential_management_404_across_orgs() {
    let f = fixture().await;
    let other_org = insert_org(&f.pool, "Other Org").await;
    let outsider = insert_token(&f.pool, other_org, &["peers:manage"]).await;

    assert_eq!(
        put_credential(&f.base, &outsider, f.workspace_id, f.peer_id, Some("nope"))
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        reqwest::Client::new()
            .get(credential_url(&f.base, f.workspace_id, f.peer_id))
            .bearer_auth(&outsider)
            .send()
            .await
            .expect("get credential")
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        reqwest::Client::new()
            .delete(credential_url(&f.base, f.workspace_id, f.peer_id))
            .bearer_auth(&outsider)
            .send()
            .await
            .expect("delete credential")
            .status(),
        StatusCode::NOT_FOUND
    );

    // The credential the in-org token created is still there and untouched.
    assert_eq!(
        put_credential(&f.base, &f.token, f.workspace_id, f.peer_id, Some("mine"))
            .await
            .status(),
        StatusCode::CREATED
    );
}

/// Omitting `credential` has IONe generate one; it is still returned exactly
/// once and presented verbatim.
#[tokio::test]
#[ignore]
async fn generated_credential_is_returned_once_and_presented() {
    let f = fixture().await;
    let resp = put_credential(&f.base, &f.token, f.workspace_id, f.peer_id, None).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body: Value = resp.json().await.expect("put body");
    let generated = body["credential"].as_str().expect("generated credential");
    assert!(generated.len() >= 32, "generated credential too short");

    assert_eq!(
        read_table_data(&f.base, f.workspace_id, f.peer_id)
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(f.peer.last_bearer(), format!("Bearer {generated}"));
}

/// An empty or whitespace-only credential is rejected rather than stored as a
/// credential that would send `Authorization: Bearer `.
#[tokio::test]
#[ignore]
async fn blank_credential_is_rejected() {
    let f = fixture().await;
    assert_eq!(
        put_credential(&f.base, &f.token, f.workspace_id, f.peer_id, Some("   "))
            .await
            .status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM workspace_peer_credentials")
        .fetch_one(&f.pool)
        .await
        .expect("credential count");
    assert_eq!(rows, 0);
}
