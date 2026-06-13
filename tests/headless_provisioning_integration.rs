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
    let ws_id: Uuid =
        sqlx::query_scalar("SELECT id FROM workspaces WHERE name = 'Operations' LIMIT 1")
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

/// Insert a service-account token row directly (bypassing the issue endpoint +
/// session-admin setup), returning the plaintext and id. Used by the /provision
/// tests that need a token in a specific org with specific permissions.
async fn insert_token_directly(
    pool: &PgPool,
    org_id: Uuid,
    permissions: &[&str],
    max_coc: i32,
) -> (String, Uuid) {
    let plaintext = format!("ione_sat_{}", Uuid::new_v4().simple());
    let hash = ione::auth::sha256_hex(&plaintext);
    let name = format!("direct-{}", Uuid::new_v4().simple());
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO service_account_tokens (org_id, name, token_hash, permissions, provisionable_max_coc)
         VALUES ($1, $2, $3, $4::jsonb, $5) RETURNING id",
    )
    .bind(org_id)
    .bind(name)
    .bind(hash)
    .bind(json!(permissions))
    .bind(max_coc)
    .fetch_one(pool)
    .await
    .expect("insert token directly");
    (plaintext, id)
}

async fn default_org_id(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("SELECT org_id FROM users LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("default org")
}

async fn provision(base: &str, token: &str, spec: Value) -> reqwest::Response {
    client()
        .post(format!("{base}/api/v1/provision"))
        .bearer_auth(token)
        .json(&spec)
        .send()
        .await
        .expect("provision request failed")
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
    let token = body["token"].as_str().expect("plaintext token").to_owned();
    assert!(token.starts_with("ione_sat_"), "{token}");

    // AC-1: the token authenticates headlessly against POST /provision and
    // applies a minimal spec.
    let resp = provision(
        &base,
        &token,
        json!({ "version": "v1", "workspace": { "name": "ac1-ws" } }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("json");
    assert!(body["workspaceId"].as_str().is_some(), "{body}");
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
    assert!(
        !serialized.contains(&plaintext),
        "list body contained plaintext"
    );
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
    sqlx::query(
        "UPDATE service_account_tokens SET expires_at = now() - interval '1 hour' WHERE id = $1",
    )
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

// ─── AC-9 (Phase-2 form): the connector idempotency constraint exists ────────
// Fully exercised via /provision in Phase 3; here we assert the unique
// constraint directly so the ON CONFLICT target Slice 3 binds to is present.

#[tokio::test]
#[ignore]
async fn connector_unique_constraint_present() {
    let (_base, pool) = spawn_app().await;
    let ws: Uuid =
        sqlx::query_scalar("SELECT id FROM workspaces WHERE name = 'Operations' LIMIT 1")
            .fetch_one(&pool)
            .await
            .expect("Operations workspace");

    let insert = |name: &'static str| {
        let pool = pool.clone();
        async move {
            sqlx::query(
                "INSERT INTO connectors (workspace_id, kind, name, config, status)
                 VALUES ($1, 'rust_native'::connector_kind, $2, '{}'::jsonb, 'active'::connector_status)",
            )
            .bind(ws)
            .bind(name)
            .execute(&pool)
            .await
        }
    };

    insert("dup").await.expect("first insert");
    let err = insert("dup")
        .await
        .expect_err("duplicate must violate unique constraint");
    let msg = err.to_string();
    assert!(
        msg.contains("connectors_workspace_id_name_key")
            || msg.contains("duplicate")
            || msg.contains("unique"),
        "expected a unique violation, got: {msg}"
    );
}

// ═══ Phase 3: /provision ═════════════════════════════════════════════════════

/// AC-5: a spec applied twice — first lists the workspace under `created`, the
/// second creates nothing new (unchanged covers it); row counts don't grow.
#[tokio::test]
#[ignore]
async fn provision_idempotent() {
    let (base, pool) = spawn_app().await;
    let org = default_org_id(&pool).await;
    let (token, _) =
        insert_token_directly(&pool, org, &["provisioning:apply", "workspace:write"], 0).await;

    let spec = json!({
        "version": "v1",
        "workspace": { "name": "idem-ws" },
        "connectors": [{ "name": "c1", "kind": "rust_native", "config": {} }]
    });

    let r1 = provision(&base, &token, spec.clone()).await;
    assert_eq!(r1.status(), StatusCode::OK);
    let b1: Value = r1.json().await.unwrap();
    let created_kinds: Vec<&str> = b1["created"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert!(created_kinds.contains(&"workspace"), "{b1}");
    assert!(created_kinds.contains(&"connector"), "{b1}");

    let ws_count_1: i64 =
        sqlx::query_scalar("SELECT count(*) FROM workspaces WHERE org_id=$1 AND name='idem-ws'")
            .bind(org)
            .fetch_one(&pool)
            .await
            .unwrap();
    let conn_count_1: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM connectors c JOIN workspaces w ON w.id=c.workspace_id WHERE w.org_id=$1 AND c.name='c1'")
        .bind(org).fetch_one(&pool).await.unwrap();

    let r2 = provision(&base, &token, spec).await;
    assert_eq!(r2.status(), StatusCode::OK);
    let b2: Value = r2.json().await.unwrap();
    assert_eq!(
        b2["created"].as_array().unwrap().len(),
        0,
        "second apply created rows: {b2}"
    );
    assert!(b2["unchangedCount"].as_i64().unwrap() >= 1, "{b2}");

    let ws_count_2: i64 =
        sqlx::query_scalar("SELECT count(*) FROM workspaces WHERE org_id=$1 AND name='idem-ws'")
            .bind(org)
            .fetch_one(&pool)
            .await
            .unwrap();
    let conn_count_2: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM connectors c JOIN workspaces w ON w.id=c.workspace_id WHERE w.org_id=$1 AND c.name='c1'")
        .bind(org).fetch_one(&pool).await.unwrap();
    assert_eq!(ws_count_1, ws_count_2);
    assert_eq!(conn_count_1, conn_count_2);
}

/// AC-9 (re-pointed): two applies changing a connector's config leave exactly
/// one row whose config is the second value.
#[tokio::test]
#[ignore]
async fn connector_upsert_single_row() {
    let (base, pool) = spawn_app().await;
    let org = default_org_id(&pool).await;
    let (token, _) =
        insert_token_directly(&pool, org, &["provisioning:apply", "workspace:write"], 0).await;

    let mk = |v: i64| {
        json!({
            "version": "v1",
            "workspace": { "name": "cfg-ws" },
            "connectors": [{ "name": "c1", "kind": "rust_native", "config": { "n": v } }]
        })
    };

    assert_eq!(
        provision(&base, &token, mk(1)).await.status(),
        StatusCode::OK
    );
    let r2 = provision(&base, &token, mk(2)).await;
    assert_eq!(r2.status(), StatusCode::OK);
    let b2: Value = r2.json().await.unwrap();
    let updated = b2["updated"].as_array().unwrap();
    assert!(
        updated.iter().any(|u| u["kind"] == "connector"
            && u["changedFields"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f == "config")),
        "{b2}"
    );

    let row_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM connectors c JOIN workspaces w ON w.id=c.workspace_id WHERE w.org_id=$1 AND c.name='c1'")
        .bind(org).fetch_one(&pool).await.unwrap();
    assert_eq!(row_count, 1);
    let cfg: Value = sqlx::query_scalar(
        "SELECT c.config FROM connectors c JOIN workspaces w ON w.id=c.workspace_id WHERE w.org_id=$1 AND c.name='c1'")
        .bind(org).fetch_one(&pool).await.unwrap();
    assert_eq!(cfg["n"], 2);
}

/// AC-6: a spec whose third connector has an invalid kind → 422 naming it, and
/// nothing from the spec persists (workspace included).
#[tokio::test]
#[ignore]
async fn provision_rolls_back() {
    let (base, pool) = spawn_app().await;
    let org = default_org_id(&pool).await;
    let (token, _) =
        insert_token_directly(&pool, org, &["provisioning:apply", "workspace:write"], 0).await;

    let spec = json!({
        "version": "v1",
        "workspace": { "name": "rollback-ws" },
        "connectors": [
            { "name": "ok1", "kind": "rust_native", "config": {} },
            { "name": "ok2", "kind": "rust_native", "config": {} },
            { "name": "bad3", "kind": "bogus_kind", "config": {} }
        ]
    });
    let resp = provision(&base, &token, spec).await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(
        body["name"], "bad3",
        "422 must name the failing connector: {body}"
    );

    let ws_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM workspaces WHERE org_id=$1 AND name='rollback-ws'",
    )
    .bind(org)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(ws_count, 0, "workspace must not persist after rollback");
    let conn_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM connectors WHERE name IN ('ok1','ok2','bad3')")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(conn_count, 0, "no connector from the spec may persist");
}

/// AC-7: escalation guard — a role granting a permission the actor lacks, or a
/// coc_level above provisionable_max_coc, → 409.
#[tokio::test]
#[ignore]
async fn provision_escalation_409() {
    let (base, pool) = spawn_app().await;
    let org = default_org_id(&pool).await;

    // (a) role grants peers:manage which the token does not hold.
    let (token, _) =
        insert_token_directly(&pool, org, &["provisioning:apply", "workspace:write"], 80).await;
    let spec = json!({
        "version": "v1",
        "workspace": { "name": "esc-ws" },
        "roles": [{ "name": "r1", "cocLevel": 10, "permissions": ["peers:manage"] }]
    });
    let resp = provision(&base, &token, spec).await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "permission_escalation");

    // (b) role coc_level above the token's provisionable_max_coc.
    let (token2, _) =
        insert_token_directly(&pool, org, &["provisioning:apply", "workspace:write"], 10).await;
    let spec2 = json!({
        "version": "v1",
        "workspace": { "name": "esc-ws2" },
        "roles": [{ "name": "r2", "cocLevel": 50, "permissions": [] }]
    });
    let resp2 = provision(&base, &token2, spec2).await;
    assert_eq!(resp2.status(), StatusCode::CONFLICT);
    assert_eq!(
        resp2.json::<Value>().await.unwrap()["error"],
        "permission_escalation"
    );
}

/// AC-8: a token holding [provisioning:apply, roles:manage] provisions W, then
/// a roles:manage-gated call on W with the same token authorizes.
#[tokio::test]
#[ignore]
async fn provision_grants_capped_membership() {
    let (base, pool) = spawn_app().await;
    let org = default_org_id(&pool).await;
    let (token, _) =
        insert_token_directly(&pool, org, &["provisioning:apply", "roles:manage"], 0).await;

    let resp = provision(
        &base,
        &token,
        json!({ "version": "v1", "workspace": { "name": "managed-ws" } }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let ws_id = resp.json::<Value>().await.unwrap()["workspaceId"]
        .as_str()
        .unwrap()
        .to_owned();

    // roles:manage-gated endpoint authorizes for the same token.
    let resp = client()
        .get(format!("{base}/api/v1/workspaces/{ws_id}/roles"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("roles request failed");
    assert_eq!(resp.status(), StatusCode::OK);
}

/// AC-10: a successful apply writes one provisioning.applied audit row with
/// actor_ref = the token id and no connector secret values in the payload.
#[tokio::test]
#[ignore]
async fn provision_audited_no_secrets() {
    let (base, pool) = spawn_app().await;
    let org = default_org_id(&pool).await;
    let (token, token_id) =
        insert_token_directly(&pool, org, &["provisioning:apply", "workspace:write"], 0).await;

    let spec = json!({
        "version": "v1",
        "workspace": { "name": "audit-ws" },
        "connectors": [{ "name": "c1", "kind": "rust_native", "config": { "api_key": "SUPERSECRET" } }]
    });
    assert_eq!(
        provision(&base, &token, spec).await.status(),
        StatusCode::OK
    );

    let (actor_ref, payload): (String, Value) = sqlx::query_as(
        "SELECT actor_ref, payload FROM audit_events
         WHERE verb = 'provisioning.applied' ORDER BY created_at DESC LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .expect("provisioning.applied row");
    assert_eq!(actor_ref, token_id.to_string());
    let serialized = serde_json::to_string(&payload).unwrap();
    assert!(
        !serialized.contains("SUPERSECRET"),
        "audit payload leaked a secret: {serialized}"
    );

    let kind: String = sqlx::query_scalar(
        "SELECT actor_kind::text FROM audit_events WHERE verb='provisioning.applied' ORDER BY created_at DESC LIMIT 1")
        .fetch_one(&pool).await.unwrap();
    assert_eq!(kind, "service_account");
}

/// AC-11 (HP-M5): org A already has 'mission-1'; an org-B token provisioning
/// 'mission-1' succeeds in org B as a distinct row, revealing nothing about A.
#[tokio::test]
#[ignore]
async fn provision_cross_org_isolated() {
    let (base, pool) = spawn_app().await;
    let org_a = default_org_id(&pool).await;
    let ws_a: Uuid = sqlx::query_scalar(
        "INSERT INTO workspaces (org_id, name, domain, lifecycle)
         VALUES ($1, 'mission-1', 'test', 'continuous'::workspace_lifecycle) RETURNING id",
    )
    .bind(org_a)
    .fetch_one(&pool)
    .await
    .unwrap();

    let org_b: Uuid =
        sqlx::query_scalar("INSERT INTO organizations (name) VALUES ('Org B') RETURNING id")
            .fetch_one(&pool)
            .await
            .unwrap();
    let (token_b, _) = insert_token_directly(&pool, org_b, &["provisioning:apply"], 0).await;

    let resp = provision(
        &base,
        &token_b,
        json!({ "version": "v1", "workspace": { "name": "mission-1" } }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    let ws_b = body["workspaceId"].as_str().unwrap().to_owned();
    assert_ne!(
        ws_b,
        ws_a.to_string(),
        "must be a distinct workspace from org A's"
    );

    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM workspaces WHERE name='mission-1'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(total, 2);
    // The response never references org A's workspace id.
    assert!(!serde_json::to_string(&body)
        .unwrap()
        .contains(&ws_a.to_string()));
}
