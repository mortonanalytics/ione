//! Contract tests for the "Demo Workspace" slice (Slice 1).
//!
//! These tests encode the *target* behavior.  Tests 1 and 5 are implemented
//! (seeder + purge land in T1.1).  Tests 2, 3, 4 remain red until T1.2
//! (canned chat) and T1.3 (write guard) land.
//!
//! Prerequisites:
//!   docker compose up -d postgres
//!   DATABASE_URL=postgres://ione:ione@localhost:5433/ione
//!
//! Run:
//!   DATABASE_URL=postgres://ione:ione@localhost:5433/ione IONE_SEED_DEMO=1 \
//!     cargo test --test contract_demo_workspace -- --ignored --test-threads=1

use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use std::net::SocketAddr;
use tokio::net::TcpListener;
use uuid::{uuid, Uuid};

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione";

/// Canonical demo workspace UUID — must match the constant in ione-complete-contract.md.
const DEMO_WORKSPACE_ID: Uuid = uuid!("00000000-0000-0000-0000-000000000d30");

// ─── Harness ──────────────────────────────────────────────────────────────────

async fn make_pool() -> PgPool {
    let db_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_owned());
    PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("failed to connect to Postgres")
}

async fn spawn_app() -> (String, PgPool) {
    let pool = make_pool().await;

    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("migration failed");

    truncate_all(&pool).await;

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind");
    let addr: SocketAddr = listener.local_addr().expect("get addr");
    let app = ione::app(pool.clone()).await;
    tokio::spawn(async move { axum::serve(listener, app).await.expect("server error") });

    (format!("http://{}", addr), pool)
}

async fn truncate_all(pool: &PgPool) {
    sqlx::query(
        "TRUNCATE audit_events, approvals, artifacts,
                  trust_issuers, peers, routing_decisions, survivors, signals,
                  stream_events, streams, connectors,
                  memberships, roles, messages, conversations,
                  workspaces, users, organizations
         RESTART IDENTITY CASCADE",
    )
    .execute(pool)
    .await
    .expect("truncate failed");
}

// ─── Test 1: seed_is_reentrant ────────────────────────────────────────────────

/// AC-1.1 / AC-1.2: `seed_demo_if_enabled` is idempotent.
/// Calling it twice must not duplicate the demo workspace or its stream events.
///
/// Requires IONE_SEED_DEMO=1 in the environment.
#[tokio::test]
#[ignore]
async fn seed_is_reentrant() {
    std::env::set_var("IONE_SEED_DEMO", "1");

    let pool = make_pool().await;
    sqlx::migrate!("./migrations").run(&pool).await.expect("migration failed");
    truncate_all(&pool).await;

    // First seed
    ione::demo::seeder::seed_demo_if_enabled(&pool)
        .await
        .expect("first seed failed");

    // Second seed — must be a no-op (idempotent)
    ione::demo::seeder::seed_demo_if_enabled(&pool)
        .await
        .expect("second seed failed");

    // Exactly one demo workspace row
    let ws_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM workspaces WHERE id = $1",
    )
    .bind(DEMO_WORKSPACE_ID)
    .fetch_one(&pool)
    .await
    .expect("workspace count query failed");

    assert_eq!(
        ws_count, 1,
        "seed_demo_if_enabled must be idempotent: expected exactly 1 demo workspace, got {}",
        ws_count
    );

    // At least 13 canned stream events scoped to the demo workspace
    let event_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM stream_events se
           JOIN streams s ON s.id = se.stream_id
           JOIN connectors c ON c.id = s.connector_id
          WHERE c.workspace_id = $1",
    )
    .bind(DEMO_WORKSPACE_ID)
    .fetch_one(&pool)
    .await
    .expect("event count query failed");

    assert!(
        event_count >= 13,
        "expected ≥13 demo stream events, got {}",
        event_count
    );
}

// ─── Test 2: demo_blocks_writes_with_demo_read_only_error ────────────────────

/// AC-1.3 (write-guard contract): any mutating request to the demo workspace
/// must return 403 with JSON body `{"error":"demo_read_only","message":"..."}`.
///
/// Fails until T1.3 (write guard middleware) lands.
#[tokio::test]
#[ignore]
async fn demo_blocks_writes_with_demo_read_only_error() {
    let (base, pool) = spawn_app().await;

    let org_id: Uuid =
        sqlx::query_scalar("SELECT id FROM organizations WHERE name = 'Default Org' LIMIT 1")
            .fetch_one(&pool)
            .await
            .expect("Default Org not found after bootstrap");

    sqlx::query(
        "INSERT INTO workspaces (id, org_id, name, domain, lifecycle)
         VALUES ($1, $2, 'Demo Workspace', 'generic', 'continuous')
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(DEMO_WORKSPACE_ID)
    .bind(org_id)
    .execute(&pool)
    .await
    .expect("insert demo workspace");

    let client = reqwest::Client::new();

    let resp = client
        .post(format!(
            "{}/api/v1/workspaces/{}/connectors",
            base, DEMO_WORKSPACE_ID
        ))
        .json(&json!({ "kind": "rust_native", "name": "test", "config": {} }))
        .send()
        .await
        .expect("POST connectors request failed");

    let status = resp.status();
    let body: Value = resp.json().await.expect("connectors response not JSON");

    assert_eq!(
        status,
        reqwest::StatusCode::FORBIDDEN,
        "POST to demo workspace connectors must return 403 with demo_read_only guard, \
         got {}. Body: {}",
        status,
        body
    );
    assert_eq!(
        body["error"].as_str(),
        Some("demo_read_only"),
        "error field must be 'demo_read_only', got: {}",
        body
    );
    assert!(
        body["message"].as_str().map(|s| !s.is_empty()).unwrap_or(false),
        "message field must be a non-empty string, got: {}",
        body
    );

    let fake_peer_id = Uuid::new_v4();
    let resp2 = client
        .post(format!(
            "{}/api/v1/workspaces/{}/peers/{}/subscribe",
            base, DEMO_WORKSPACE_ID, fake_peer_id
        ))
        .json(&json!({}))
        .send()
        .await
        .expect("POST subscribe request failed");

    let status2 = resp2.status();
    let body2: Value = resp2.json().await.expect("subscribe response not JSON");

    assert_eq!(
        status2,
        reqwest::StatusCode::FORBIDDEN,
        "POST to demo workspace peers/subscribe must return 403 with demo_read_only guard, \
         got {}. Body: {}",
        status2,
        body2
    );
    assert_eq!(
        body2["error"].as_str(),
        Some("demo_read_only"),
        "error field must be 'demo_read_only' for subscribe, got: {}",
        body2
    );
}

// ─── Test 3: canned_chat_bypasses_ollama ─────────────────────────────────────

/// AC-1.4 (canned prompts): known demo prompt receives a canned reply.
///
/// Fails until T1.2 (canned chat layer) lands.
#[tokio::test]
#[ignore]
async fn canned_chat_bypasses_ollama() {
    todo!("ione::demo::seeder and canned chat not yet implemented — intentionally red")
}

// ─── Test 4: canned_unmatched_returns_stock_reply ────────────────────────────

/// AC-1.4 (stock reply): an unrecognized prompt in the demo conversation
/// must return 200 with model=="canned" and content starting with "I can answer".
///
/// Fails until T1.2 (canned chat layer) lands.
#[tokio::test]
#[ignore]
async fn canned_unmatched_returns_stock_reply() {
    todo!("canned chat stock reply not yet implemented — intentionally red")
}

// ─── Test 5: demo_purge_removes_workspace_and_audit_events ───────────────────

/// AC-1.5 (purge contract): `purge_demo` removes the demo workspace AND
/// explicitly deletes all audit_events rows whose workspace_id equals
/// DEMO_WORKSPACE_ID.
#[tokio::test]
#[ignore]
async fn demo_purge_removes_workspace_and_audit_events() {
    std::env::set_var("IONE_SEED_DEMO", "1");

    let pool = make_pool().await;
    sqlx::migrate!("./migrations").run(&pool).await.expect("migration failed");
    truncate_all(&pool).await;

    // Seed the demo workspace.
    ione::demo::seeder::seed_demo_if_enabled(&pool)
        .await
        .expect("seed failed");

    // Verify at least one audit_events row exists for the demo workspace.
    let audit_before: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE workspace_id = $1",
    )
    .bind(DEMO_WORKSPACE_ID)
    .fetch_one(&pool)
    .await
    .expect("audit before count failed");

    assert!(
        audit_before > 0,
        "expected ≥1 audit_events row for demo workspace after seed, got 0"
    );

    // Purge.
    ione::demo::seeder::purge_demo(&pool)
        .await
        .expect("purge_demo failed");

    // Workspace must be gone.
    let ws_after: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM workspaces WHERE id = $1",
    )
    .bind(DEMO_WORKSPACE_ID)
    .fetch_one(&pool)
    .await
    .expect("workspace after count failed");

    assert_eq!(
        ws_after, 0,
        "purge_demo must remove the demo workspace, but {} rows remain",
        ws_after
    );

    // Audit events must also be gone.
    let audit_after: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE workspace_id = $1",
    )
    .bind(DEMO_WORKSPACE_ID)
    .fetch_one(&pool)
    .await
    .expect("audit after count failed");

    assert_eq!(
        audit_after, 0,
        "purge_demo must explicitly delete audit_events for the demo workspace \
         (ON DELETE SET NULL alone is not sufficient); {} rows remain",
        audit_after
    );
}
