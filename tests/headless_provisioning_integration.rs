//! Integration tests for headless provisioning
//! (md/plans/headless-provisioning-plan.md). Phases 1–3 share this file;
//! tests are named per the acceptance-criteria map.
//!
//! Run: cargo test --test headless_provisioning_integration -- --ignored --test-threads=1

use std::net::SocketAddr;

use reqwest::StatusCode;
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;
use uuid::Uuid;

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione";

async fn spawn_app() -> (String, PgPool) {
    std::env::set_var("IONE_AUTH_MODE", "local");

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
        "TRUNCATE service_account_tokens, auto_exec_policies, org_memberships,
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

async fn default_user_and_org(pool: &PgPool) -> (Uuid, Uuid) {
    sqlx::query_as::<_, (Uuid, Uuid)>("SELECT id, org_id FROM users LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("default user")
}

/// Make the local-mode default user an admin (so the issuance escalation guard
/// is exempt) and grant the org-scoped `service_accounts:manage` /
/// `provisioning:apply` perms the token endpoints require.
async fn make_default_user_provisioning_admin(pool: &PgPool) {
    let (user_id, org_id) = default_user_and_org(pool).await;
    let ws_id: Uuid = sqlx::query_scalar("SELECT id FROM workspaces WHERE name = 'Operations' LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("Operations workspace");
    let role_id: Uuid = sqlx::query_scalar(
        "INSERT INTO roles (workspace_id, name, coc_level, permissions)
         VALUES ($1, 'admin', 80, '[\"admin\"]'::jsonb)
         RETURNING id",
    )
    .bind(ws_id)
    .fetch_one(pool)
    .await
    .expect("insert admin role");
    sqlx::query(
        "INSERT INTO memberships (user_id, workspace_id, role_id, created_at)
         VALUES ($1, $2, $3, now() + interval '1 second')",
    )
    .bind(user_id)
    .bind(ws_id)
    .bind(role_id)
    .execute(pool)
    .await
    .expect("insert admin membership");
    sqlx::query(
        "INSERT INTO org_memberships (user_id, org_id, permissions)
         VALUES ($1, $2, '[\"service_accounts:manage\",\"provisioning:apply\"]'::jsonb)
         ON CONFLICT (user_id, org_id) DO UPDATE SET permissions = EXCLUDED.permissions",
    )
    .bind(user_id)
    .bind(org_id)
    .execute(pool)
    .await
    .expect("grant org perms");
}

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

async fn issue_token(base: &str, body: Value) -> reqwest::Response {
    client()
        .post(format!("{base}/api/v1/service-account-tokens"))
        .json(&body)
        .send()
        .await
        .expect("issue request failed")
}

// ─── AC-1: token issuance + headless auth (Phase-1 form: authenticate against
// the service_accounts:manage-gated GET; re-pointed at /provision in Phase 3) ──

#[tokio::test]
#[ignore]
async fn token_authenticates() {
    let (base, pool) = spawn_app().await;
    make_default_user_provisioning_admin(&pool).await;

    // Phase-1 form: the token must hold the permission gating the stand-in
    // endpoint (service_accounts:manage); Phase 3 re-points AC-1 at /provision
    // (gated by provisioning:apply), which the launcher token also carries.
    let resp = issue_token(
        &base,
        json!({
            "name": "mission-launcher",
            "permissions": ["service_accounts:manage", "provisioning:apply", "workspace:write", "roles:manage"],
            "provisionableMaxCoc": 50
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body: Value = resp.json().await.expect("json");
    let token = body["token"].as_str().expect("plaintext token");
    assert!(token.starts_with("ione_sat_"), "{token}");

    // The token authenticates against a service_accounts:manage-gated endpoint.
    let resp = client()
        .get(format!("{base}/api/v1/service-account-tokens"))
        .bearer_auth(token)
        .send()
        .await
        .expect("authed request failed");
    assert_eq!(resp.status(), StatusCode::OK);
}

// ─── AC-2: token never re-exposed ────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn token_list_omits_secret() {
    let (base, pool) = spawn_app().await;
    make_default_user_provisioning_admin(&pool).await;

    let resp = issue_token(
        &base,
        json!({ "name": "t1", "permissions": ["provisioning:apply"], "provisionableMaxCoc": 0 }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let created: Value = resp.json().await.expect("json");
    let plaintext = created["token"].as_str().unwrap().to_owned();

    let resp = client()
        .get(format!("{base}/api/v1/service-account-tokens"))
        .send()
        .await
        .expect("list request failed");
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("json");
    let items = body["items"].as_array().expect("items");
    assert_eq!(items.len(), 1);
    for item in items {
        assert!(item.get("token").is_none(), "list leaked plaintext: {item}");
        assert!(item.get("tokenHash").is_none(), "list leaked hash: {item}");
        assert!(item.get("token_hash").is_none(), "list leaked hash: {item}");
    }
    let serialized = serde_json::to_string(&body).unwrap();
    assert!(!serialized.contains(&plaintext), "list body contained plaintext");
}

// ─── AC-3: revocation → 401 ──────────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn revoked_token_401() {
    let (base, pool) = spawn_app().await;
    make_default_user_provisioning_admin(&pool).await;

    let resp = issue_token(
        &base,
        json!({ "name": "doomed", "permissions": ["provisioning:apply"], "provisionableMaxCoc": 0 }),
    )
    .await;
    let created: Value = resp.json().await.expect("json");
    let id = created["id"].as_str().unwrap().to_owned();
    let token = created["token"].as_str().unwrap().to_owned();

    let resp = client()
        .delete(format!("{base}/api/v1/service-account-tokens/{id}"))
        .send()
        .await
        .expect("revoke request failed");
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let resp = client()
        .get(format!("{base}/api/v1/service-account-tokens"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("authed request failed");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ─── AC-4: expiry → 401 ──────────────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn expired_token_401() {
    let (base, pool) = spawn_app().await;
    make_default_user_provisioning_admin(&pool).await;

    let resp = issue_token(
        &base,
        json!({ "name": "stale", "permissions": ["provisioning:apply"], "provisionableMaxCoc": 0 }),
    )
    .await;
    let created: Value = resp.json().await.expect("json");
    let id = Uuid::parse_str(created["id"].as_str().unwrap()).unwrap();
    let token = created["token"].as_str().unwrap().to_owned();

    // Backdate the expiry past now.
    sqlx::query("UPDATE service_account_tokens SET expires_at = now() - interval '1 hour' WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .expect("backdate expiry");

    let resp = client()
        .get(format!("{base}/api/v1/service-account-tokens"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("authed request failed");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
