/// RBAC role-management integration tests (design `md/design/rbac-scaffolding.md`).
///
/// Covers AC-3/4 (fail-closed on the management surface), AC-5 (vocabulary
/// validation), AC-6/7 (escalation guards), AC-12 (member-count list), AC-13
/// (audit trail), plus the membership grant/revoke contracts.
///
/// Run (serial, ignored):
///   DATABASE_URL=postgres://ione:ione@localhost:5433/ione \
///     cargo test --test rbac_integration -- --ignored --test-threads=1
use std::net::SocketAddr;

use reqwest::StatusCode;
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;
use uuid::Uuid;

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione";

async fn spawn_app() -> (String, PgPool) {
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
        "TRUNCATE webhook_events_seen, workspace_peer_bindings, audit_events, pipeline_events,
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

async fn ops_workspace_id(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("SELECT id FROM workspaces WHERE name = 'Operations' LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("Operations workspace not found")
}

async fn default_user_id(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("SELECT id FROM users WHERE email = 'default@localhost' LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("default user not found")
}

/// Set the default user's bootstrap 'member' role permissions in place.
async fn set_member_permissions(pool: &PgPool, workspace_id: Uuid, perms: Value) {
    sqlx::query(
        "UPDATE roles SET permissions = $2
         WHERE workspace_id = $1 AND name = 'member'",
    )
    .bind(workspace_id)
    .bind(perms)
    .execute(pool)
    .await
    .expect("set member permissions");
}

/// Insert a standalone role (no members) and return its id.
async fn insert_role(
    pool: &PgPool,
    workspace_id: Uuid,
    name: &str,
    coc_level: i32,
    perms: Value,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO roles (workspace_id, name, coc_level, permissions)
         VALUES ($1, $2, $3, $4)
         RETURNING id",
    )
    .bind(workspace_id)
    .bind(name)
    .bind(coc_level)
    .bind(perms)
    .fetch_one(pool)
    .await
    .expect("insert role")
}

async fn insert_user(pool: &PgPool, email: &str) -> Uuid {
    let org_id: Uuid = sqlx::query_scalar("SELECT id FROM organizations LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("org");
    sqlx::query_scalar(
        "INSERT INTO users (org_id, email, display_name) VALUES ($1, $2, $2) RETURNING id",
    )
    .bind(org_id)
    .bind(email)
    .fetch_one(pool)
    .await
    .expect("insert user")
}

fn roles_url(base: &str, ws: Uuid) -> String {
    format!("{base}/api/v1/workspaces/{ws}/roles")
}

fn put_url(base: &str, ws: Uuid, role: Uuid) -> String {
    format!("{base}/api/v1/workspaces/{ws}/roles/{role}/permissions")
}

// ─── AC-3 / AC-4: fail-closed management surface ──────────────────────────────

/// A member whose role has no `roles:manage` (empty grants) gets 403 from the
/// role list; the management surface fails closed.
#[tokio::test]
#[ignore]
async fn roles_list_403_without_roles_manage() {
    let (base, pool) = spawn_app().await;
    let ws = ops_workspace_id(&pool).await;
    // Bootstrap member role: permissions '{}' (never set) — no grants.
    let resp = reqwest::Client::new()
        .get(roles_url(&base, ws))
        .send()
        .await
        .expect("GET roles");
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    // Explicit empty array behaves the same.
    set_member_permissions(&pool, ws, json!([])).await;
    let resp = reqwest::Client::new()
        .get(roles_url(&base, ws))
        .send()
        .await
        .expect("GET roles");
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

// ─── AC-5: vocabulary validation ──────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn put_rejects_unknown_perm() {
    let (base, pool) = spawn_app().await;
    let ws = ops_workspace_id(&pool).await;
    set_member_permissions(&pool, ws, json!(["roles:manage", "audit:read"])).await;
    let role_id = insert_role(&pool, ws, "viewer", 0, json!(["audit:read"])).await;

    let resp = reqwest::Client::new()
        .put(put_url(&base, ws, role_id))
        .json(&json!({ "permissions": ["audit:read", "bogus:perm"] }))
        .send()
        .await
        .expect("PUT permissions");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body: Value = resp.json().await.expect("body");
    assert_eq!(body["error"], "invalid_permission");

    // Role unchanged.
    let perms: Value = sqlx::query_scalar("SELECT permissions FROM roles WHERE id = $1")
        .bind(role_id)
        .fetch_one(&pool)
        .await
        .expect("role perms");
    assert_eq!(perms, json!(["audit:read"]));

    // Org-scoped strings are not assignable through the role editor either.
    let resp = reqwest::Client::new()
        .put(put_url(&base, ws, role_id))
        .json(&json!({ "permissions": ["trust_issuers:manage"] }))
        .send()
        .await
        .expect("PUT permissions");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// ─── AC-6: escalation guard (permission) ──────────────────────────────────────

#[tokio::test]
#[ignore]
async fn escalation_permission_409() {
    let (base, pool) = spawn_app().await;
    let ws = ops_workspace_id(&pool).await;
    // Actor holds roles:manage + audit:read; NOT peers:manage, NOT admin.
    set_member_permissions(&pool, ws, json!(["roles:manage", "audit:read"])).await;
    let role_id = insert_role(&pool, ws, "target", 0, json!([])).await;

    let resp = reqwest::Client::new()
        .put(put_url(&base, ws, role_id))
        .json(&json!({ "permissions": ["peers:manage"] }))
        .send()
        .await
        .expect("PUT escalating permissions");
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body: Value = resp.json().await.expect("body");
    assert_eq!(body["error"], "permission_escalation");

    // Granting a permission the actor holds succeeds.
    let resp = reqwest::Client::new()
        .put(put_url(&base, ws, role_id))
        .json(&json!({ "permissions": ["audit:read"] }))
        .send()
        .await
        .expect("PUT held permission");
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("body");
    assert_eq!(body["permissions"], json!(["audit:read"]));
}

/// The `admin` holder is exempt from the escalation guard.
#[tokio::test]
#[ignore]
async fn escalation_admin_exempt() {
    let (base, pool) = spawn_app().await;
    let ws = ops_workspace_id(&pool).await;
    set_member_permissions(&pool, ws, json!(["admin"])).await;
    let role_id = insert_role(&pool, ws, "target", 0, json!([])).await;

    let resp = reqwest::Client::new()
        .put(put_url(&base, ws, role_id))
        .json(&json!({ "permissions": ["peers:manage"], "cocLevel": 95 }))
        .send()
        .await
        .expect("PUT as admin");
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("body");
    assert_eq!(body["cocLevel"], json!(95));
}

// ─── AC-7: escalation guard (coc_level) ───────────────────────────────────────

#[tokio::test]
#[ignore]
async fn escalation_coc_409() {
    let (base, pool) = spawn_app().await;
    let ws = ops_workspace_id(&pool).await;
    // Actor's effective max coc is the bootstrap member role's 0; no admin.
    set_member_permissions(&pool, ws, json!(["roles:manage"])).await;
    let role_id = insert_role(&pool, ws, "target", 10, json!([])).await;

    let resp = reqwest::Client::new()
        .put(put_url(&base, ws, role_id))
        .json(&json!({ "permissions": [], "cocLevel": 90 }))
        .send()
        .await
        .expect("PUT raising coc");
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body: Value = resp.json().await.expect("body");
    assert_eq!(body["error"], "permission_escalation");

    // Lowering is allowed.
    let resp = reqwest::Client::new()
        .put(put_url(&base, ws, role_id))
        .json(&json!({ "permissions": [], "cocLevel": 5 }))
        .send()
        .await
        .expect("PUT lowering coc");
    assert_eq!(resp.status(), StatusCode::OK);
}

// ─── AC-12: member-count list ─────────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn roles_with_member_count() {
    let (base, pool) = spawn_app().await;
    let ws = ops_workspace_id(&pool).await;
    set_member_permissions(&pool, ws, json!(["roles:manage"])).await;

    // R1 with 2 members (default user + one more); R2 with 0.
    let r1 = insert_role(&pool, ws, "R1", 0, json!(["audit:read"])).await;
    let _r2 = insert_role(&pool, ws, "R2", 0, json!([])).await;
    let u1 = default_user_id(&pool).await;
    let u2 = insert_user(&pool, "second@localhost").await;
    for u in [u1, u2] {
        sqlx::query("INSERT INTO memberships (user_id, workspace_id, role_id) VALUES ($1, $2, $3)")
            .bind(u)
            .bind(ws)
            .bind(r1)
            .execute(&pool)
            .await
            .expect("insert membership");
    }

    let body: Value = reqwest::Client::new()
        .get(roles_url(&base, ws))
        .send()
        .await
        .expect("GET roles")
        .json()
        .await
        .expect("body");
    let items = body["items"].as_array().expect("items array");
    let by_name = |name: &str| {
        items
            .iter()
            .find(|r| r["name"] == name)
            .unwrap_or_else(|| panic!("role {name} missing from list"))
    };
    assert_eq!(by_name("R1")["memberCount"], json!(2));
    assert_eq!(by_name("R1")["permissions"], json!(["audit:read"]));
    assert_eq!(by_name("R2")["memberCount"], json!(0));
    assert_eq!(by_name("R2")["permissions"], json!([]));
}

// ─── AC-13: audit trail ───────────────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn permission_change_audited() {
    let (base, pool) = spawn_app().await;
    let ws = ops_workspace_id(&pool).await;
    set_member_permissions(&pool, ws, json!(["roles:manage", "audit:read"])).await;
    let role_id = insert_role(&pool, ws, "target", 0, json!([])).await;
    let actor = default_user_id(&pool).await;

    let resp = reqwest::Client::new()
        .put(put_url(&base, ws, role_id))
        .json(&json!({ "permissions": ["audit:read"] }))
        .send()
        .await
        .expect("PUT permissions");
    assert_eq!(resp.status(), StatusCode::OK);

    let (actor_ref, payload): (String, Value) = sqlx::query_as(
        "SELECT actor_ref, payload FROM audit_events
         WHERE workspace_id = $1 AND verb = 'role.permissions.updated' AND object_id = $2",
    )
    .bind(ws)
    .bind(role_id)
    .fetch_one(&pool)
    .await
    .expect("audit row must exist");
    assert_eq!(actor_ref, actor.to_string());
    assert_eq!(payload["actor"], json!(actor));
    assert_eq!(payload["before"], json!([]));
    assert_eq!(payload["after"], json!(["audit:read"]));
}

// ─── Membership grant / revoke contracts ──────────────────────────────────────

#[tokio::test]
#[ignore]
async fn membership_grant_revoke_roundtrip() {
    let (base, pool) = spawn_app().await;
    let ws = ops_workspace_id(&pool).await;
    set_member_permissions(&pool, ws, json!(["roles:manage", "audit:read"])).await;
    let role_id = insert_role(&pool, ws, "granted-role", 0, json!(["audit:read"])).await;
    let user = insert_user(&pool, "grantee@localhost").await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/api/v1/workspaces/{ws}/memberships"))
        .json(&json!({ "user_id": user, "role_id": role_id }))
        .send()
        .await
        .expect("POST membership");
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("body");
    assert!(body["id"].is_string(), "grant must return the membership id");

    // Duplicate grant → 409 membership_exists.
    let resp = client
        .post(format!("{base}/api/v1/workspaces/{ws}/memberships"))
        .json(&json!({ "user_id": user, "role_id": role_id }))
        .send()
        .await
        .expect("POST duplicate membership");
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body: Value = resp.json().await.expect("body");
    assert_eq!(body["error"], "membership_exists");

    let resp = client
        .delete(format!(
            "{base}/api/v1/workspaces/{ws}/memberships/{user}/{role_id}"
        ))
        .send()
        .await
        .expect("DELETE membership");
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // Gone → 404 on repeat.
    let resp = client
        .delete(format!(
            "{base}/api/v1/workspaces/{ws}/memberships/{user}/{role_id}"
        ))
        .send()
        .await
        .expect("DELETE missing membership");
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // Both writes audited.
    let granted: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_events WHERE workspace_id = $1 AND verb = 'membership.granted'",
    )
    .bind(ws)
    .fetch_one(&pool)
    .await
    .expect("granted count");
    let revoked: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_events WHERE workspace_id = $1 AND verb = 'membership.revoked'",
    )
    .bind(ws)
    .fetch_one(&pool)
    .await
    .expect("revoked count");
    assert_eq!((granted, revoked), (1, 1));
}

/// Membership grants are escalation-guarded: a non-admin roles:manage holder
/// cannot hand out a role whose permissions exceed their own.
#[tokio::test]
#[ignore]
async fn membership_grant_escalation_409() {
    let (base, pool) = spawn_app().await;
    let ws = ops_workspace_id(&pool).await;
    set_member_permissions(&pool, ws, json!(["roles:manage"])).await;
    let admin_role = insert_role(&pool, ws, "admin", 80, json!(["admin"])).await;
    let user = default_user_id(&pool).await;

    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/workspaces/{ws}/memberships"))
        .json(&json!({ "user_id": user, "role_id": admin_role }))
        .send()
        .await
        .expect("POST escalating membership");
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body: Value = resp.json().await.expect("body");
    assert_eq!(body["error"], "permission_escalation");
}
