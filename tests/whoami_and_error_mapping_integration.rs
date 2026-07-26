//! Regressions for three adversarial-review findings:
//!
//! * F5 — contract v1 §6 says a whoami value may be `null` and that a consumer
//!   must not conflate "missing" with "null". `foreign_roles` was typed
//!   `#[serde(default)] Vec<String>`, which accepts an absent key and rejects an
//!   explicit `null`, failing the whole whoami and leaving a `pending` binding.
//! * F7 — the binding-refresh path ran `fetch_whoami` with no per-call timeout,
//!   so a peer that accepts the connection and never answers held an
//!   operator-facing endpoint open for the 15 s client timeout (×3 requests).
//! * F6 — contract v1 §7.3 maps peer JSON-RPC `-32002` to 404 and every other
//!   code to 502. The chart path collapsed `-32002` to 502.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use reqwest::StatusCode;
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione";
const TEST_STATIC_BEARER: &str = "whoami-error-mapping-test-bearer";

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

    grant_default_user_admin(&pool).await;

    (format!("http://{}", addr), pool)
}

/// `peers:manage` gates subscribe; the bootstrap member role needs it.
async fn grant_default_user_admin(pool: &PgPool) {
    let ws = default_workspace_id(pool).await;
    sqlx::query(
        "UPDATE roles SET permissions = '[\"admin\"]'::jsonb
         WHERE workspace_id = $1 AND name = 'member'",
    )
    .bind(ws)
    .execute(pool)
    .await
    .expect("grant admin to member role");
    let user_id: Uuid = sqlx::query_scalar("SELECT id FROM users LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("default user");
    ione::repos::OrgMembershipRepo::new(pool.clone())
        .grant(
            user_id,
            default_org_id(pool).await,
            &["trust_issuers:manage", "peers:manage"],
        )
        .await
        .expect("grant org permissions");
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

async fn insert_binding(
    pool: &PgPool,
    workspace_id: Uuid,
    peer_id: Uuid,
    tenant: &str,
    status: &str,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO workspace_peer_bindings
           (workspace_id, peer_id, foreign_tenant_id, status)
         VALUES ($1, $2, $3, $4::binding_status)
         RETURNING id",
    )
    .bind(workspace_id)
    .bind(peer_id)
    .bind(tenant)
    .bind(status)
    .fetch_one(pool)
    .await
    .expect("insert binding")
}

async fn binding_row(pool: &PgPool, workspace_id: Uuid, peer_id: Uuid) -> (String, Vec<String>) {
    sqlx::query_as(
        "SELECT status::TEXT, foreign_roles
         FROM workspace_peer_bindings
         WHERE workspace_id = $1 AND peer_id = $2",
    )
    .bind(workspace_id)
    .bind(peer_id)
    .fetch_one(pool)
    .await
    .expect("binding row")
}

/// Mounts a peer whose `whoami://` read returns `whoami` as `contents[0].text`.
async fn mock_whoami_peer(mock: &MockServer, whoami: Value) {
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
        .mount(mock)
        .await;
}

async fn subscribe(base: &str, workspace_id: Uuid, peer_id: Uuid) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/workspaces/{workspace_id}/peers/{peer_id}/subscribe"
        ))
        .json(&json!({}))
        .send()
        .await
        .expect("subscribe")
}

async fn seed_chart_peer(pool: &PgPool, workspace_id: Uuid, name: &str, url: &str) -> Uuid {
    let org_id = default_org_id(pool).await;
    let issuer_id = insert_trust_issuer(pool, org_id, &format!("https://{name}.issuer.test")).await;
    let peer_id = insert_peer(pool, name, url, issuer_id).await;
    insert_binding(pool, workspace_id, peer_id, "tenant-1", "active").await;
    peer_id
}

async fn get_chart_data(base: &str, workspace_id: Uuid, peer_id: Uuid) -> reqwest::Response {
    reqwest::Client::new()
        .get(format!(
            "{base}/api/v1/workspaces/{workspace_id}/chart-data?peer_id={peer_id}&uri=stub%3A%2F%2Fchart%2F1"
        ))
        .bearer_auth(TEST_STATIC_BEARER)
        .send()
        .await
        .expect("chart-data response")
}

/// F5 — §6 field sweep at the deserializer, no DB needed: every optional key
/// tolerates both an absent key and an explicit `null`. `foreign_tenant_id` is
/// the one exception — it is the binding key, so a null or absent value must
/// fail the whoami rather than produce an unbindable identity.
#[test]
fn whoami_accepts_null_and_absent_interchangeably_for_every_optional_field() {
    use ione::services::workspace_peer_binding::WhoamiResponse;

    for key in [
        "peer_id",
        "foreign_tenant_name",
        "foreign_workspace_id",
        "foreign_user_id",
        "foreign_user_email",
        "foreign_roles",
    ] {
        let mut null_case = json!({
            "peer_id": "stub",
            "foreign_tenant_id": "t-1",
            "foreign_tenant_name": "Acme",
            "foreign_workspace_id": "fws-1",
            "foreign_user_id": "u-1",
            "foreign_user_email": "u@example.test",
            "foreign_roles": ["operator"],
        });
        let mut absent_case = null_case.clone();
        null_case[key] = Value::Null;
        absent_case.as_object_mut().expect("object").remove(key);

        let from_null: WhoamiResponse = serde_json::from_value(null_case)
            .unwrap_or_else(|e| panic!("§6: an explicit null {key} must deserialize: {e}"));
        let from_absent: WhoamiResponse = serde_json::from_value(absent_case)
            .unwrap_or_else(|e| panic!("an absent {key} must deserialize: {e}"));
        assert_eq!(
            format!("{from_null:?}"),
            format!("{from_absent:?}"),
            "{key}: null and absent must produce the same WhoamiResponse"
        );
    }

    // The binding key is not optional in either spelling.
    for tenant in [json!(null), json!(""), json!(0)] {
        let body = json!({ "foreign_tenant_id": tenant, "foreign_roles": [] });
        let parsed = serde_json::from_value::<WhoamiResponse>(body)
            .map(|w| w.foreign_tenant_id)
            .ok()
            .filter(|t| !t.is_empty());
        assert!(
            parsed.is_none(),
            "foreign_tenant_id = {tenant} must not yield a usable binding key"
        );
    }
}

/// F5 — §6: an explicit `null` is legal and must behave exactly like an absent
/// key (empty role list, `active` binding). A wrongly-typed value still fails.
#[tokio::test]
#[ignore]
async fn whoami_null_foreign_roles_binds_active_like_an_absent_key() {
    let (base, pool) = spawn_app().await;
    let org_id = default_org_id(&pool).await;
    let workspace_id = default_workspace_id(&pool).await;

    let base_whoami = json!({
        "peer_id": "stub",
        "foreign_tenant_id": "t-null-roles",
        "foreign_tenant_name": "Acme",
        "foreign_workspace_id": null,
        "foreign_user_id": "u-1",
        "foreign_user_email": null,
    });

    // (1) explicit null
    let null_mock = MockServer::start().await;
    let mut whoami = base_whoami.clone();
    whoami["foreign_roles"] = Value::Null;
    mock_whoami_peer(&null_mock, whoami).await;
    let null_issuer = insert_trust_issuer(&pool, org_id, "https://iss-null-roles.test").await;
    let null_peer = insert_peer(
        &pool,
        "Null Roles Peer",
        &format!("{}/mcp", null_mock.uri()),
        null_issuer,
    )
    .await;

    let resp = subscribe(&base, workspace_id, null_peer).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(
        body["binding"]["status"], "active",
        "§6 permits a null value; a null foreign_roles must not fail the whoami"
    );
    let (status, roles) = binding_row(&pool, workspace_id, null_peer).await;
    assert_eq!(status, "active");
    assert!(
        roles.is_empty(),
        "null foreign_roles must materialize as an empty role list, got {roles:?}"
    );

    // (2) absent key — must be indistinguishable from (1)
    let absent_mock = MockServer::start().await;
    let mut whoami = base_whoami.clone();
    whoami["foreign_tenant_id"] = json!("t-absent-roles");
    mock_whoami_peer(&absent_mock, whoami).await;
    let absent_issuer = insert_trust_issuer(&pool, org_id, "https://iss-absent-roles.test").await;
    let absent_peer = insert_peer(
        &pool,
        "Absent Roles Peer",
        &format!("{}/mcp", absent_mock.uri()),
        absent_issuer,
    )
    .await;

    let resp = subscribe(&base, workspace_id, absent_peer).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let absent_row = binding_row(&pool, workspace_id, absent_peer).await;
    assert_eq!(
        absent_row,
        (status, roles),
        "an absent foreign_roles key and an explicit null must produce identical bindings"
    );

    // (3) genuinely malformed value still fails → pending binding
    let bad_mock = MockServer::start().await;
    let mut whoami = base_whoami;
    whoami["foreign_tenant_id"] = json!("t-bad-roles");
    whoami["foreign_roles"] = json!("operator");
    mock_whoami_peer(&bad_mock, whoami).await;
    let bad_issuer = insert_trust_issuer(&pool, org_id, "https://iss-bad-roles.test").await;
    let bad_peer = insert_peer(
        &pool,
        "Bad Roles Peer",
        &format!("{}/mcp", bad_mock.uri()),
        bad_issuer,
    )
    .await;

    let resp = subscribe(&base, workspace_id, bad_peer).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let (bad_status, bad_roles) = binding_row(&pool, workspace_id, bad_peer).await;
    assert_eq!(
        bad_status, "pending",
        "a string where an array belongs is malformed and must still fail the whoami"
    );
    assert!(bad_roles.is_empty());
}

/// F7 — a peer that accepts the connection and never answers must not hold the
/// operator-facing refresh endpoint open for the 15 s HTTP client timeout.
#[tokio::test]
#[ignore]
async fn refresh_times_out_quickly_when_peer_accepts_but_never_responds() {
    let (base, pool) = spawn_app().await;
    let org_id = default_org_id(&pool).await;
    let workspace_id = default_workspace_id(&pool).await;

    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/mcp"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(60)))
        .mount(&mock)
        .await;

    let issuer_id = insert_trust_issuer(&pool, org_id, "https://iss-refresh-silent.test").await;
    let peer_id = insert_peer(
        &pool,
        "Silent Peer",
        &format!("{}/mcp", mock.uri()),
        issuer_id,
    )
    .await;
    let binding_id = insert_binding(&pool, workspace_id, peer_id, "t-silent", "active").await;

    let started = Instant::now();
    let resp = reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/workspaces/{workspace_id}/bindings/{binding_id}/refresh"
        ))
        .send()
        .await
        .expect("refresh");
    let elapsed = started.elapsed();

    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    assert!(
        elapsed < Duration::from_secs(10),
        "refresh must be bounded well under the 15 s HTTP client timeout, took {elapsed:?}"
    );
}

/// F6 — §7.3: peer JSON-RPC `-32002` is 404, every other code is 502.
#[tokio::test]
#[ignore]
async fn chart_data_maps_resource_not_found_to_404_and_other_codes_to_502() {
    let (base, pool) = spawn_app().await;
    let workspace_id = default_workspace_id(&pool).await;

    let not_found = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": { "code": -32002, "message": "resource not found" }
        })))
        .mount(&not_found)
        .await;
    let not_found_peer =
        seed_chart_peer(&pool, workspace_id, "chart-nf-peer", &not_found.uri()).await;
    let resp = get_chart_data(&base, workspace_id, not_found_peer).await;
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "§7.3: -32002 is a missing resource, not a peer fault"
    );

    // -32601 "Method not found" is a peer fault: still 502, never 404.
    let broken = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": { "code": -32601, "message": "method not found" }
        })))
        .mount(&broken)
        .await;
    let broken_peer =
        seed_chart_peer(&pool, workspace_id, "chart-broken-peer", &broken.uri()).await;
    let resp = get_chart_data(&base, workspace_id, broken_peer).await;
    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);

    // A peer that refuses the connection is also 502.
    let down_peer =
        seed_chart_peer(&pool, workspace_id, "chart-down-peer", "http://127.0.0.1:1").await;
    let resp = get_chart_data(&base, workspace_id, down_peer).await;
    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
}
