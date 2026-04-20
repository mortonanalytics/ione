/// Phase 3 contract tests — workspaces, roles, memberships.
///
/// These tests are written against the contract in md/design/ione-v1-contract.md
/// and FAIL today because the Phase 3 implementation does not yet exist.
/// They are all `#[ignore]`-gated and require a running Postgres instance.
///
/// ──────────────────────────────────────────────────────────────────────────
/// Expected DATABASE_URL (default):
///   postgres://ione:ione@localhost:5433/ione
///
/// Bring up Postgres before running:
///   docker compose up -d postgres
///
/// Run this suite (serial to avoid DB contention):
///   DATABASE_URL=postgres://ione:ione@localhost:5433/ione \
///     cargo test --test phase03_workspaces -- --ignored --test-threads=1
///
/// ──────────────────────────────────────────────────────────────────────────
/// Contract targets (md/design/ione-v1-contract.md):
///
///   Workspace fields (JSON, camelCase):
///     id, orgId, parentId, name, domain, lifecycle,
///     endCondition, metadata, createdAt, closedAt
///
///   Role fields (JSON, camelCase):
///     id, workspaceId, name, cocLevel, permissions
///
///   Membership fields (JSON, camelCase):
///     id, userId, workspaceId, roleId, federatedClaimRef, createdAt
///
///   Lifecycle enum values: "continuous" | "bounded"
///
///   Endpoints (contract § API operations):
///     GET  /api/v1/workspaces              → { items: Workspace[] }
///     POST /api/v1/workspaces              ← { name, domain, lifecycle, parentId? }
///                                          → Workspace
///     GET  /api/v1/workspaces/:id          → Workspace
///     POST /api/v1/workspaces/:id/close    → Workspace (sets closedAt)
///
///   Conversation workspace scoping (plan Phase 3):
///     - conversations.workspace_id becomes NOT NULL (FK) after migration 0002
///     - POST /api/v1/conversations without workspaceId defaults to the seeded
///       "Operations" workspace so the Phase 2 UI path continues to work
///     - POST /api/v1/conversations with a non-existent workspaceId → 4xx
///     - Deleting a workspace cascades through conversations → messages
///
///   Seed on first boot (plan Phase 3):
///     - One "Operations" workspace in the default org, lifecycle='continuous'
///     - One "member" role scoped to that workspace
///     - One membership: default user × Operations × member role
///
/// ──────────────────────────────────────────────────────────────────────────
/// Run with --test-threads=1 (serial) because tests share one Postgres DB and
/// TRUNCATE between runs; parallel execution would cause races.
/// ──────────────────────────────────────────────────────────────────────────
use std::net::SocketAddr;

use reqwest::StatusCode;
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;
use uuid::Uuid;

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione";

/// Connect, run migrations, truncate in FK-safe order, boot on a random port.
/// Returns `(base_url, pool)`.
async fn spawn_app() -> (String, PgPool) {
    let db_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_owned());

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("failed to connect to Postgres — is `docker compose up -d postgres` running?");

    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("migration failed — migration 0002 may not exist yet (expected failure)");

    // Truncate in FK-safe order: memberships → roles → conversations/messages →
    // workspaces → users → organizations.
    // Phase 3 adds workspaces, roles, memberships; Phase 2 has the rest.
    // If the tables don't exist yet (pre-migration) this will fail — which is the
    // expected failure mode for a contract-red test.
    sqlx::query(
        "TRUNCATE memberships, roles, messages, conversations, workspaces, users, organizations
         RESTART IDENTITY CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate failed — tables may not exist yet (expected for contract-red)");

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind random port");
    let addr: SocketAddr = listener.local_addr().expect("failed to get local addr");

    // `ione::app(pool)` runs bootstrap (default org + user + Phase 3 seeds).
    let app = ione::app(pool.clone()).await;

    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("server error");
    });

    (format!("http://{}", addr), pool)
}

// ──────────────────────────────────────────────────────────────────────────────
// Seed assertions
// ──────────────────────────────────────────────────────────────────────────────

/// After `app(pool)` starts against a fresh DB the bootstrap must have seeded:
///   - exactly one `workspaces` row named "Operations" in the default org,
///     lifecycle='continuous', closed_at IS NULL
///   - at least one `roles` row named 'member' scoped to that workspace
///   - exactly one `memberships` row linking the default user to that workspace
///     via the member role
///
/// Contract targets:
///   - workspace fields: id, orgId, name, lifecycle, closedAt
///   - role fields: workspaceId, name
///   - membership fields: userId, workspaceId, roleId
///   - plan Phase 3 seed requirement: "Operations" workspace, "member" role,
///     membership for default user
///
/// REASON: requires DATABASE_URL pointing at a running Postgres instance with
/// migration 0002 applied (which does not yet exist).
#[tokio::test]
#[ignore]
async fn default_operations_workspace_seeded() {
    let (_base, pool) = spawn_app().await;

    // --- workspace ---
    let ws_row: (Uuid, String, String, Option<chrono::DateTime<chrono::Utc>>) = sqlx::query_as(
        "SELECT w.id, w.name, w.lifecycle::TEXT, w.closed_at
             FROM workspaces w
             JOIN organizations o ON o.id = w.org_id
             WHERE w.name = 'Operations'
             LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .expect("no 'Operations' workspace row — bootstrap seed missing (expected failure)");

    let ops_workspace_id = ws_row.0;
    assert_eq!(
        ws_row.1, "Operations",
        "workspace name must be 'Operations', got: {}",
        ws_row.1
    );
    assert_eq!(
        ws_row.2, "continuous",
        "Operations workspace lifecycle must be 'continuous', got: {}",
        ws_row.2
    );
    assert!(
        ws_row.3.is_none(),
        "Operations workspace closed_at must be NULL on seed, got: {:?}",
        ws_row.3
    );

    // exactly one workspace named "Operations"
    let ws_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM workspaces WHERE name = 'Operations'")
            .fetch_one(&pool)
            .await
            .expect("workspace count query failed");
    assert_eq!(
        ws_count, 1,
        "expected exactly one 'Operations' workspace, got {}",
        ws_count
    );

    // --- role ---
    let role_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM roles WHERE workspace_id = $1 AND name = 'member'",
    )
    .bind(ops_workspace_id)
    .fetch_one(&pool)
    .await
    .expect("role count query failed");

    assert!(
        role_count >= 1,
        "expected at least one 'member' role for the Operations workspace, got {}",
        role_count
    );

    // --- membership ---
    let member_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)
         FROM memberships m
         JOIN users u ON u.id = m.user_id
         JOIN roles r  ON r.id = m.role_id
         WHERE m.workspace_id = $1
           AND u.email = 'default@localhost'
           AND r.name  = 'member'",
    )
    .bind(ops_workspace_id)
    .fetch_one(&pool)
    .await
    .expect("membership count query failed");

    assert_eq!(
        member_count, 1,
        "expected exactly one membership (default user × Operations × member), got {}",
        member_count
    );
}

/// Equivalent to `default_operations_workspace_seeded` but focused on the
/// memberships table in isolation, expressed as its own named assertion per spec.
///
/// Contract targets:
///   - membership fields: userId, workspaceId, roleId (contract § membership)
///   - plan Phase 3: "Seed script creates an 'Operations' workspace on first boot"
///     + "a default 'member' role + a membership for the default user"
///
/// REASON: requires DATABASE_URL pointing at a running Postgres instance with
/// migration 0002 applied.
#[tokio::test]
#[ignore]
async fn memberships_bootstrapped_for_default_user_in_operations() {
    let (_base, pool) = spawn_app().await;

    let row: (Uuid, Uuid, Uuid) = sqlx::query_as(
        "SELECT m.user_id, m.workspace_id, m.role_id
         FROM memberships m
         JOIN users u       ON u.id = m.user_id
         JOIN workspaces w  ON w.id = m.workspace_id
         JOIN roles r       ON r.id = m.role_id
         WHERE u.email    = 'default@localhost'
           AND w.name     = 'Operations'
           AND r.name     = 'member'
         LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .expect(
        "no membership row for (default@localhost × Operations × member) — \
         bootstrap seed missing (expected failure)",
    );

    // All three FKs must be non-nil UUIDs
    assert_ne!(row.0, Uuid::nil(), "membership.user_id must not be nil");
    assert_ne!(
        row.1,
        Uuid::nil(),
        "membership.workspace_id must not be nil"
    );
    assert_ne!(row.2, Uuid::nil(), "membership.role_id must not be nil");

    // Exactly one such row
    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)
         FROM memberships m
         JOIN users u      ON u.id = m.user_id
         JOIN workspaces w ON w.id = m.workspace_id
         JOIN roles r      ON r.id = m.role_id
         WHERE u.email = 'default@localhost'
           AND w.name  = 'Operations'
           AND r.name  = 'member'",
    )
    .fetch_one(&pool)
    .await
    .expect("membership total count query failed");

    assert_eq!(
        total, 1,
        "expected exactly 1 membership row for (default@localhost × Operations × member), got {}",
        total
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// Workspace CRUD
// ──────────────────────────────────────────────────────────────────────────────

/// POST /api/v1/workspaces { "name":"Lolo NF", "domain":"fire-ops", "lifecycle":"continuous" }
/// → 200, body has the correct shape and values, DB row present.
///
/// Contract targets (contract § workspace fields):
///   id, orgId, parentId, name, domain, lifecycle,
///   endCondition, metadata, createdAt, closedAt
///   Lifecycle value: "continuous"
///
/// Contract targets (contract § API operations):
///   POST /api/v1/workspaces ← { name, domain, lifecycle, parentId? } → Workspace
///
/// REASON: requires DATABASE_URL and migration 0002 + workspaces route.
#[tokio::test]
#[ignore]
async fn create_workspace_returns_workspace() {
    let (base, pool) = spawn_app().await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{}/api/v1/workspaces", base))
        .json(&json!({
            "name": "Lolo NF",
            "domain": "fire-ops",
            "lifecycle": "continuous"
        }))
        .send()
        .await
        .expect("POST /api/v1/workspaces request failed");

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "expected 200 from POST /api/v1/workspaces, got {}",
        resp.status()
    );

    let body: Value = resp.json().await.expect("body is not JSON");

    // id — valid UUID
    let id_str = body["id"]
        .as_str()
        .expect("response must have an \"id\" string field");
    let id = Uuid::parse_str(id_str).expect("\"id\" must be a valid UUID");

    // name
    assert_eq!(
        body["name"], "Lolo NF",
        "name must be 'Lolo NF', got: {}",
        body["name"]
    );

    // domain
    assert_eq!(
        body["domain"], "fire-ops",
        "domain must be 'fire-ops', got: {}",
        body["domain"]
    );

    // lifecycle
    assert_eq!(
        body["lifecycle"], "continuous",
        "lifecycle must be 'continuous', got: {}",
        body["lifecycle"]
    );

    // createdAt — present and non-null
    assert!(
        !body["createdAt"].is_null(),
        "createdAt must be non-null, got: {}",
        body
    );

    // closedAt — must be null for a new workspace
    assert!(
        body["closedAt"].is_null(),
        "closedAt must be null for a newly created workspace, got: {}",
        body["closedAt"]
    );

    // parentId — null when not supplied
    assert!(
        body["parentId"].is_null(),
        "parentId must be null when not supplied, got: {}",
        body["parentId"]
    );

    // endCondition — null when not supplied
    assert!(
        body["endCondition"].is_null(),
        "endCondition must be null when not supplied, got: {}",
        body["endCondition"]
    );

    // DB row must exist
    let db_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workspaces WHERE id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .expect("db count query failed");

    assert_eq!(
        db_count, 1,
        "expected one workspaces row with id={}, found {}",
        id, db_count
    );
}

/// POST a parent workspace, then POST a child with parentId set.
/// → 200, child body has parentId equal to the parent's id.
///
/// Contract targets:
///   - workspace.parentId (contract § workspace)
///   - POST /api/v1/workspaces ← { ..., parentId? }
///
/// REASON: requires DATABASE_URL and migration 0002.
#[tokio::test]
#[ignore]
async fn create_child_workspace_sets_parent_id() {
    let (base, _pool) = spawn_app().await;
    let client = reqwest::Client::new();

    // Create parent
    let parent: Value = client
        .post(format!("{}/api/v1/workspaces", base))
        .json(&json!({
            "name": "Bitterroot NF",
            "domain": "fire-ops",
            "lifecycle": "continuous"
        }))
        .send()
        .await
        .expect("POST parent workspace failed")
        .json()
        .await
        .expect("parent body not JSON");

    let parent_id = parent["id"].as_str().expect("parent must have an id field");

    // Create child referencing the parent
    let resp = client
        .post(format!("{}/api/v1/workspaces", base))
        .json(&json!({
            "name": "Sula District",
            "domain": "fire-ops",
            "lifecycle": "bounded",
            "parentId": parent_id
        }))
        .send()
        .await
        .expect("POST child workspace failed");

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "expected 200 from POST /api/v1/workspaces (child), got {}",
        resp.status()
    );

    let child: Value = resp.json().await.expect("child body not JSON");

    assert_eq!(
        child["parentId"], parent_id,
        "child workspace parentId must equal parent's id ({}), got: {}",
        parent_id, child["parentId"]
    );
}

/// GET /api/v1/workspaces returns { items: Workspace[] }.
/// Closed workspaces remain in the list but have a non-null closedAt.
///
/// Behavior choice: the contract specifies list returns items; the plan does not
/// specify filtering. Default: closed workspaces ARE included in the list with
/// a non-null closedAt so callers can decide what to display.
///
/// Test creates 2 workspaces (in addition to the seeded Operations workspace),
/// closes one via POST /api/v1/workspaces/:id/close, then asserts:
///   - items array has all 3 workspaces (Operations + open + closed)
///   - the closed one has non-null closedAt
///   - the open one has null closedAt
///
/// Contract targets:
///   - GET /api/v1/workspaces → { items: Workspace[] }
///   - workspace.closedAt
///   - POST /api/v1/workspaces/:id/close → Workspace
///
/// REASON: requires DATABASE_URL and migration 0002.
#[tokio::test]
#[ignore]
async fn list_workspaces_returns_items_excluding_closed_by_default() {
    let (base, _pool) = spawn_app().await;
    let client = reqwest::Client::new();

    // Create workspace A (will remain open)
    let ws_a: Value = client
        .post(format!("{}/api/v1/workspaces", base))
        .json(&json!({
            "name": "WS Alpha",
            "domain": "ops",
            "lifecycle": "continuous"
        }))
        .send()
        .await
        .expect("create WS A failed")
        .json()
        .await
        .expect("WS A body not JSON");
    let ws_a_id = ws_a["id"].as_str().expect("WS A must have id");

    // Create workspace B (will be closed)
    let ws_b: Value = client
        .post(format!("{}/api/v1/workspaces", base))
        .json(&json!({
            "name": "WS Beta",
            "domain": "ops",
            "lifecycle": "bounded"
        }))
        .send()
        .await
        .expect("create WS B failed")
        .json()
        .await
        .expect("WS B body not JSON");
    let ws_b_id = ws_b["id"].as_str().expect("WS B must have id");

    // Close workspace B
    let close_resp = client
        .post(format!("{}/api/v1/workspaces/{}/close", base, ws_b_id))
        .json(&json!({}))
        .send()
        .await
        .expect("close WS B failed");

    assert_eq!(
        close_resp.status(),
        StatusCode::OK,
        "expected 200 from POST /api/v1/workspaces/:id/close, got {}",
        close_resp.status()
    );

    // List all workspaces
    let list_resp = client
        .get(format!("{}/api/v1/workspaces", base))
        .send()
        .await
        .expect("GET /api/v1/workspaces failed");

    assert_eq!(
        list_resp.status(),
        StatusCode::OK,
        "expected 200 from GET /api/v1/workspaces, got {}",
        list_resp.status()
    );

    let list_body: Value = list_resp.json().await.expect("list body not JSON");
    let items = list_body["items"]
        .as_array()
        .expect("response must have an \"items\" array");

    // Should include Operations + WS Alpha + WS Beta = 3 total
    assert_eq!(
        items.len(),
        3,
        "expected 3 workspaces in list (Operations + WS Alpha + WS Beta), got {}; items: {}",
        items.len(),
        list_body["items"]
    );

    // Find WS Beta by id and assert closedAt is non-null
    let closed_ws = items
        .iter()
        .find(|w| w["id"].as_str() == Some(ws_b_id))
        .expect("WS Beta must appear in list even when closed");

    assert!(
        !closed_ws["closedAt"].is_null(),
        "closed workspace must have non-null closedAt in list, got: {}",
        closed_ws
    );

    // Find WS Alpha by id and assert closedAt is null
    let open_ws = items
        .iter()
        .find(|w| w["id"].as_str() == Some(ws_a_id))
        .expect("WS Alpha must appear in list");

    assert!(
        open_ws["closedAt"].is_null(),
        "open workspace must have null closedAt in list, got: {}",
        open_ws
    );
}

/// POST /api/v1/workspaces/:id/close sets closedAt; subsequent GET confirms it.
///
/// Contract targets:
///   - POST /api/v1/workspaces/:id/close → Workspace (sets closedAt)
///   - GET  /api/v1/workspaces/:id → Workspace
///   - workspace.closedAt (contract § workspace fields)
///
/// REASON: requires DATABASE_URL and migration 0002.
#[tokio::test]
#[ignore]
async fn close_workspace_sets_closed_at() {
    let (base, pool) = spawn_app().await;
    let client = reqwest::Client::new();

    // Create a workspace
    let ws: Value = client
        .post(format!("{}/api/v1/workspaces", base))
        .json(&json!({
            "name": "Temporary Op",
            "domain": "logistics",
            "lifecycle": "bounded"
        }))
        .send()
        .await
        .expect("create workspace failed")
        .json()
        .await
        .expect("create body not JSON");

    let ws_id = ws["id"].as_str().expect("workspace must have id");

    // closedAt must be null before close
    assert!(
        ws["closedAt"].is_null(),
        "closedAt must be null before closing, got: {}",
        ws["closedAt"]
    );

    // Close it
    let close_body: Value = client
        .post(format!("{}/api/v1/workspaces/{}/close", base, ws_id))
        .json(&json!({}))
        .send()
        .await
        .expect("close workspace failed")
        .json()
        .await
        .expect("close body not JSON");

    // Immediate response has closedAt set
    assert!(
        !close_body["closedAt"].is_null(),
        "POST .../close response must have non-null closedAt, got: {}",
        close_body
    );

    // GET the workspace and confirm closedAt persisted
    let get_resp = client
        .get(format!("{}/api/v1/workspaces/{}", base, ws_id))
        .send()
        .await
        .expect("GET workspace failed");

    assert_eq!(
        get_resp.status(),
        StatusCode::OK,
        "expected 200 from GET /api/v1/workspaces/:id, got {}",
        get_resp.status()
    );

    let get_body: Value = get_resp.json().await.expect("GET body not JSON");

    assert!(
        !get_body["closedAt"].is_null(),
        "GET /workspaces/:id must return non-null closedAt after close, got: {}",
        get_body
    );

    // DB: closed_at must be non-null
    let ws_uuid = Uuid::parse_str(ws_id).expect("ws_id must be a valid UUID");
    let db_closed_at: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT closed_at FROM workspaces WHERE id = $1")
            .bind(ws_uuid)
            .fetch_one(&pool)
            .await
            .expect("db closed_at query failed");

    assert!(
        db_closed_at.is_some(),
        "DB closed_at must be non-null after close, got None"
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// Conversation workspace scoping
// ──────────────────────────────────────────────────────────────────────────────

/// POST /api/v1/conversations without workspaceId must default to the seeded
/// "Operations" workspace so the Phase 2 UI path continues to work.
///
/// Behavior choice (plan Phase 3):
///   "Mitigate: migration assigns any null workspace_id to the seeded 'Operations'
///   workspace before adding the FK."
///   → For new conversations, the handler defaults to Operations when no
///   workspaceId is supplied, so existing UI code doesn't break.
///
/// Contract targets:
///   - POST /api/v1/conversations ← { title?, workspaceId? } → Conversation
///   - conversation.workspaceId (contract § conversation)
///   - plan Phase 3: conversations become workspace-scoped, NOT NULL FK
///
/// REASON: requires DATABASE_URL and migration 0002.
#[tokio::test]
#[ignore]
async fn conversation_requires_workspace_id_after_phase_3() {
    let (base, pool) = spawn_app().await;
    let client = reqwest::Client::new();

    // Fetch the seeded Operations workspace id from DB
    let ops_id: Uuid =
        sqlx::query_scalar("SELECT id FROM workspaces WHERE name = 'Operations' LIMIT 1")
            .fetch_one(&pool)
            .await
            .expect("Operations workspace not found in DB (migration 0002 missing?)");

    // POST without workspaceId
    let resp = client
        .post(format!("{}/api/v1/conversations", base))
        .json(&json!({ "title": "default workspace test" }))
        .send()
        .await
        .expect("POST /api/v1/conversations failed");

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "expected 200 from POST /api/v1/conversations (no workspaceId), got {}",
        resp.status()
    );

    let body: Value = resp.json().await.expect("body not JSON");

    // workspaceId must be present and non-null
    assert!(
        !body["workspaceId"].is_null(),
        "conversation.workspaceId must be non-null when created without explicit workspaceId \
         (should default to Operations), got: {}",
        body
    );

    // workspaceId must equal the Operations workspace
    let returned_ws_id = body["workspaceId"]
        .as_str()
        .expect("workspaceId must be a string");
    let returned_uuid = Uuid::parse_str(returned_ws_id).expect("workspaceId must be a valid UUID");

    assert_eq!(
        returned_uuid, ops_id,
        "conversation created without workspaceId must default to Operations workspace \
         ({ops_id}), got {returned_uuid}"
    );
}

/// POST /api/v1/conversations with an unknown workspaceId must return 4xx.
///
/// Contract targets:
///   - conversation.workspaceId FK constraint (contract § conversation,
///     plan Phase 3: workspace_id NOT NULL REFERENCES workspaces)
///
/// REASON: requires DATABASE_URL and migration 0002.
#[tokio::test]
#[ignore]
async fn conversation_with_nonexistent_workspace_id_returns_400_or_404() {
    let (base, _pool) = spawn_app().await;
    let client = reqwest::Client::new();

    let random_id = Uuid::new_v4();

    let resp = client
        .post(format!("{}/api/v1/conversations", base))
        .json(&json!({
            "title": "bad workspace",
            "workspaceId": random_id.to_string()
        }))
        .send()
        .await
        .expect("POST /api/v1/conversations failed");

    let status = resp.status().as_u16();
    assert!(
        status == 400 || status == 404 || status == 422,
        "expected 4xx when workspaceId does not exist, got {}",
        status
    );
}

/// Creating a conversation in a workspace, posting a message, then deleting the
/// workspace directly cascades through conversations → messages.
///
/// Plan Phase 3: "conversations.workspace_id … ON DELETE CASCADE"
///
/// Contract targets:
///   - workspace.id FK on conversations (plan § migration 0002)
///   - conversation FK on messages (contract § message.conversationId)
///
/// REASON: requires DATABASE_URL and migration 0002.
#[tokio::test]
#[ignore]
async fn conversations_cascade_on_workspace_delete() {
    let (base, pool) = spawn_app().await;
    let client = reqwest::Client::new();

    // Create a workspace W
    let ws: Value = client
        .post(format!("{}/api/v1/workspaces", base))
        .json(&json!({
            "name": "Ephemeral WS",
            "domain": "test",
            "lifecycle": "bounded"
        }))
        .send()
        .await
        .expect("create workspace failed")
        .json()
        .await
        .expect("ws body not JSON");

    let ws_id_str = ws["id"].as_str().expect("ws must have id");
    let ws_uuid = Uuid::parse_str(ws_id_str).expect("ws id must be UUID");

    // Create a conversation in W
    let conv: Value = client
        .post(format!("{}/api/v1/conversations", base))
        .json(&json!({
            "title": "cascade test conv",
            "workspaceId": ws_id_str
        }))
        .send()
        .await
        .expect("create conversation failed")
        .json()
        .await
        .expect("conv body not JSON");

    let conv_id_str = conv["id"].as_str().expect("conversation must have id");
    let conv_uuid = Uuid::parse_str(conv_id_str).expect("conv id must be UUID");

    // Insert a message directly so we don't need Ollama
    sqlx::query(
        "INSERT INTO messages (conversation_id, role, content)
         VALUES ($1, 'user'::message_role, 'cascade test message')",
    )
    .bind(conv_uuid)
    .execute(&pool)
    .await
    .expect("message insert failed");

    // Sanity: conversation and message exist
    let conv_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM conversations WHERE id = $1")
        .bind(conv_uuid)
        .fetch_one(&pool)
        .await
        .expect("conv count query failed");
    assert_eq!(
        conv_before, 1,
        "conversation must exist before workspace delete"
    );

    let msg_before: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM messages WHERE conversation_id = $1")
            .bind(conv_uuid)
            .fetch_one(&pool)
            .await
            .expect("msg count query failed");
    assert_eq!(msg_before, 1, "message must exist before workspace delete");

    // Delete the workspace directly via sqlx (bypasses the API to test the FK)
    sqlx::query("DELETE FROM workspaces WHERE id = $1")
        .bind(ws_uuid)
        .execute(&pool)
        .await
        .expect("workspace delete failed");

    // Conversation must be gone (ON DELETE CASCADE)
    let conv_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM conversations WHERE id = $1")
        .bind(conv_uuid)
        .fetch_one(&pool)
        .await
        .expect("conv count after delete failed");
    assert_eq!(
        conv_after, 0,
        "conversation must be deleted when workspace is deleted (cascade), found {}",
        conv_after
    );

    // Messages must be gone (cascade through conversation)
    let msg_after: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM messages WHERE conversation_id = $1")
            .bind(conv_uuid)
            .fetch_one(&pool)
            .await
            .expect("msg count after delete failed");
    assert_eq!(
        msg_after, 0,
        "messages must be deleted when workspace is deleted (two-hop cascade), found {}",
        msg_after
    );
}

/// Phase 2 regression: POST /api/v1/conversations with an explicit workspaceId
/// (the Operations workspace) must still return 200 with a matching workspaceId.
///
/// Contract targets:
///   - POST /api/v1/conversations ← { title?, workspaceId? } → Conversation
///   - plan Phase 3: "Updating to workspace-scoping must not break Phase 2 tests"
///
/// REASON: requires DATABASE_URL and migration 0002.
#[tokio::test]
#[ignore]
async fn phase_2_create_conversation_with_workspace_id_still_works() {
    let (base, pool) = spawn_app().await;
    let client = reqwest::Client::new();

    // Look up the seeded Operations workspace id
    let ops_id: Uuid =
        sqlx::query_scalar("SELECT id FROM workspaces WHERE name = 'Operations' LIMIT 1")
            .fetch_one(&pool)
            .await
            .expect("Operations workspace not found — bootstrap seed missing (expected failure)");

    let resp = client
        .post(format!("{}/api/v1/conversations", base))
        .json(&json!({
            "title": "phase 2 regression",
            "workspaceId": ops_id.to_string()
        }))
        .send()
        .await
        .expect("POST /api/v1/conversations failed");

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "expected 200 from POST /api/v1/conversations with explicit workspaceId, got {}",
        resp.status()
    );

    let body: Value = resp.json().await.expect("body not JSON");

    let returned_ws_id = body["workspaceId"]
        .as_str()
        .expect("conversation must have workspaceId field");
    let returned_uuid = Uuid::parse_str(returned_ws_id).expect("workspaceId must be a valid UUID");

    assert_eq!(
        returned_uuid, ops_id,
        "conversation.workspaceId must equal the supplied Operations id ({ops_id}), \
         got {returned_uuid}"
    );
}
