//! Integration tests for the audit & T&E event export feature
//! (md/plans/audit-event-export-plan.md). Phases 1–4 share this file;
//! tests are named `phase{N}_…` so each phase gate can run its slice.
//!
//! Run: cargo test --test phase_audit_export -- --ignored --test-threads=1

use std::net::SocketAddr;

use chrono::{DateTime, Duration, TimeZone, Utc};
use reqwest::StatusCode;
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;
use uuid::Uuid;

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione";
const TEST_STATIC_BEARER: &str = "phase-audit-export-test-bearer";

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

fn seed_base_time() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap()
}

async fn seed_audit_event(
    pool: &PgPool,
    workspace_id: Uuid,
    actor_kind: &str,
    actor_ref: &str,
    verb: &str,
    created_at: DateTime<Utc>,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO audit_events
           (workspace_id, actor_kind, actor_ref, verb, object_kind, payload, created_at)
         VALUES ($1, $2::actor_kind, $3, $4, 'test_object', '{}'::jsonb, $5)
         RETURNING id",
    )
    .bind(workspace_id)
    .bind(actor_kind)
    .bind(actor_ref)
    .bind(verb)
    .bind(created_at)
    .fetch_one(pool)
    .await
    .expect("seed audit event")
}

async fn get(base: &str, path_and_query: &str) -> reqwest::Response {
    reqwest::Client::new()
        .get(format!("{base}{path_and_query}"))
        .bearer_auth(TEST_STATIC_BEARER)
        .send()
        .await
        .expect("request failed")
}

// ─── Phase 1: write-time scrub through the repo choke point (AC-8) ───────────

#[tokio::test]
#[ignore]
async fn phase1_repo_write_scrub() {
    let (_base, pool) = spawn_app().await;
    let ws = default_workspace_id(&pool).await;

    let body_4kb = "x".repeat(4096);
    let repo = ione::repos::AuditEventRepo::new(pool.clone());
    let inserted = repo
        .insert(
            Some(ws),
            ione::models::ActorKind::System,
            "delivery-service",
            "delivery_failed",
            "delivery",
            None,
            json!({ "error": format!("https://user:secret@host/path failed: {body_4kb}") }),
        )
        .await
        .expect("insert audit event");

    let stored: Value =
        sqlx::query_scalar("SELECT payload FROM audit_events WHERE id = $1")
            .bind(inserted.id)
            .fetch_one(&pool)
            .await
            .expect("read back payload");
    let error_text = stored["error"].as_str().expect("error field is a string");
    assert!(!error_text.contains("secret"), "{error_text}");
    assert!(!error_text.contains("user:"), "{error_text}");
    assert!(
        error_text.chars().count() <= 257,
        "expected <= 257 chars, got {}",
        error_text.chars().count()
    );
}

// ─── Phase 2: filterable list + keyset cursor + indexes ──────────────────────

/// AC-1: 350 events, 120 with verb peer_tool_executed; filter + two-page walk.
#[tokio::test]
#[ignore]
async fn phase2_filtered_list_cursor_walk() {
    let (base, pool) = spawn_app().await;
    let ws = default_workspace_id(&pool).await;
    let t0 = seed_base_time();

    for i in 0..350 {
        let verb = if i < 120 { "peer_tool_executed" } else { "other_verb" };
        seed_audit_event(
            &pool,
            ws,
            "peer",
            &format!("peer-{}", i % 3),
            verb,
            t0 + Duration::seconds(i),
        )
        .await;
    }

    let resp = get(
        &base,
        &format!("/api/v1/workspaces/{ws}/audit_events?verb=peer_tool_executed&limit=100"),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("json");
    let items = body["items"].as_array().expect("items");
    assert_eq!(items.len(), 100);
    for item in items {
        assert_eq!(item["verb"], "peer_tool_executed");
    }
    let cursor = body["next_cursor"].as_str().expect("non-null next_cursor");

    let resp = get(
        &base,
        &format!(
            "/api/v1/workspaces/{ws}/audit_events?verb=peer_tool_executed&limit=100&cursor={cursor}"
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("json");
    let items = body["items"].as_array().expect("items");
    assert_eq!(items.len(), 20);
    for item in items {
        assert_eq!(item["verb"], "peer_tool_executed");
    }
    assert!(body["next_cursor"].is_null(), "{}", body["next_cursor"]);
}

/// Malformed cursor → 400, not 500.
#[tokio::test]
#[ignore]
async fn phase2_invalid_cursor_400() {
    let (base, pool) = spawn_app().await;
    let ws = default_workspace_id(&pool).await;
    let resp = get(
        &base,
        &format!("/api/v1/workspaces/{ws}/audit_events?cursor=%2Fnot-base64%2F"),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// AC-6 (404 half): a member of a different org gets 404, zero foreign rows.
#[tokio::test]
#[ignore]
async fn phase2_cross_org_404() {
    let (base, pool) = spawn_app().await;
    let ws = default_workspace_id(&pool).await;
    seed_audit_event(&pool, ws, "peer", "peer-0", "peer_tool_executed", seed_base_time()).await;

    // Workspace in a different org: the default user's org check must 404.
    let other_org = insert_org(&pool, "Other Org").await;
    let other_ws = insert_workspace(&pool, other_org, "Other Workspace").await;
    let resp = get(&base, &format!("/api/v1/workspaces/{other_ws}/audit_events")).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

/// AC-10: EXPLAIN of the real cursor query shape (id tiebreaker + cursor
/// predicate) uses an index scan — no Seq Scan, no Sort node.
#[tokio::test]
#[ignore]
async fn phase2_explain_index_scan() {
    let (_base, pool) = spawn_app().await;
    let ws = default_workspace_id(&pool).await;
    let t0 = seed_base_time();
    for i in 0..1000 {
        let verb = if i % 3 == 0 { "peer_tool_executed" } else { "other_verb" };
        seed_audit_event(&pool, ws, "peer", "peer-0", verb, t0 + Duration::seconds(i)).await;
    }
    sqlx::query("ANALYZE audit_events")
        .execute(&pool)
        .await
        .expect("analyze");

    let since = t0;
    let cursor_ts = t0 + Duration::seconds(900);
    let cursor_id = Uuid::new_v4();
    let plan: Vec<String> = sqlx::query_scalar(
        "EXPLAIN SELECT id FROM audit_events
         WHERE workspace_id=$1 AND verb=$2 AND created_at>=$3
           AND (created_at, id) < ($4, $5)
         ORDER BY created_at DESC, id DESC LIMIT 100",
    )
    .bind(ws)
    .bind("peer_tool_executed")
    .bind(since)
    .bind(cursor_ts)
    .bind(cursor_id)
    .fetch_all(&pool)
    .await
    .expect("explain");
    let text = plan.join("\n");
    assert!(text.contains("Index"), "expected index scan: {text}");
    assert!(!text.contains("Seq Scan"), "unexpected seq scan: {text}");
    assert!(!text.contains("Sort"), "unexpected sort node: {text}");
}
