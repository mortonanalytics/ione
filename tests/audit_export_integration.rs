//! Integration tests for the audit & T&E event export feature
//! (md/plans/audit-event-export-plan.md). Phases 1–4 share this file;
//! tests are named `phase{N}_…` so each phase gate can run its slice.
//!
//! Run: cargo test --test audit_export_integration -- --ignored --test-threads=1

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

/// Make the local-mode default user an admin (coc 80) on the given workspace.
/// resolve_active_role_id picks the most recent membership, so inserting an
/// admin membership after bootstrap's coc-0 'member' one elevates the user.
async fn elevate_default_user_to_admin(pool: &PgPool, workspace_id: Uuid) {
    let user_id: Uuid = sqlx::query_scalar("SELECT id FROM users LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("default user");
    let role_id: Uuid = sqlx::query_scalar(
        "INSERT INTO roles (workspace_id, name, coc_level)
         VALUES ($1, 'admin', 80)
         RETURNING id",
    )
    .bind(workspace_id)
    .fetch_one(pool)
    .await
    .expect("insert admin role");
    sqlx::query(
        "INSERT INTO memberships (user_id, workspace_id, role_id, created_at)
         VALUES ($1, $2, $3, now() + interval '1 second')",
    )
    .bind(user_id)
    .bind(workspace_id)
    .bind(role_id)
    .execute(pool)
    .await
    .expect("insert admin membership");
}

async fn insert_connector(pool: &PgPool, workspace_id: Uuid, name: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO connectors (workspace_id, kind, name, config, status)
         VALUES ($1, 'rust_native'::connector_kind, $2, '{}'::jsonb, 'active'::connector_status)
         RETURNING id",
    )
    .bind(workspace_id)
    .bind(name)
    .fetch_one(pool)
    .await
    .expect("insert connector")
}

async fn seed_pipeline_event(
    pool: &PgPool,
    workspace_id: Uuid,
    connector_id: Option<Uuid>,
    stage: &str,
    occurred_at: DateTime<Utc>,
) {
    sqlx::query(
        "INSERT INTO pipeline_events (workspace_id, connector_id, stage, occurred_at)
         VALUES ($1, $2, $3, $4)",
    )
    .bind(workspace_id)
    .bind(connector_id)
    .bind(stage)
    .bind(occurred_at)
    .execute(pool)
    .await
    .expect("seed pipeline event");
}

/// Query-string-safe RFC 3339 (Z suffix — `+00:00` would decode as a space).
fn ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
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

    let stored: Value = sqlx::query_scalar("SELECT payload FROM audit_events WHERE id = $1")
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
        let verb = if i < 120 {
            "peer_tool_executed"
        } else {
            "other_verb"
        };
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
    seed_audit_event(
        &pool,
        ws,
        "peer",
        "peer-0",
        "peer_tool_executed",
        seed_base_time(),
    )
    .await;

    // Workspace in a different org: the default user's org check must 404.
    let other_org = insert_org(&pool, "Other Org").await;
    let other_ws = insert_workspace(&pool, other_org, "Other Workspace").await;
    let resp = get(
        &base,
        &format!("/api/v1/workspaces/{other_ws}/audit_events"),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ─── Phase 3: aggregates ─────────────────────────────────────────────────────

/// AC-2: events spanning 3 hours from two actors; bucket counts sum to the
/// seeded totals per actor.
#[tokio::test]
#[ignore]
async fn phase3_count_by_bucket_sums() {
    let (base, pool) = spawn_app().await;
    let ws = default_workspace_id(&pool).await;
    elevate_default_user_to_admin(&pool, ws).await;
    let t0 = seed_base_time();

    // actor-a: 2 + 1 + 3 across three hours; actor-b: 1 + 2 + 0.
    let seeds = [
        ("actor-a", 0, 2),
        ("actor-a", 1, 1),
        ("actor-a", 2, 3),
        ("actor-b", 0, 1),
        ("actor-b", 1, 2),
    ];
    for (actor, hour, n) in seeds {
        for i in 0..n {
            seed_audit_event(
                &pool,
                ws,
                "peer",
                actor,
                "peer_tool_executed",
                t0 + Duration::hours(hour) + Duration::seconds(i),
            )
            .await;
        }
    }

    let since = ts(t0);
    let until = ts(t0 + Duration::hours(3));
    let resp = get(
        &base,
        &format!(
            "/api/v1/workspaces/{ws}/audit-aggregates?op=count_by_bucket&bucket=hour&group_by=actor_ref&since={since}&until={until}"
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["op"], "count_by_bucket");
    assert_eq!(body["bucket"], "hour");
    let groups = body["groups"].as_array().expect("groups");
    let total_for = |actor: &str| -> i64 {
        groups
            .iter()
            .filter(|g| g["key"] == actor)
            .map(|g| g["count"].as_i64().unwrap())
            .sum()
    };
    assert_eq!(total_for("actor-a"), 6);
    assert_eq!(total_for("actor-b"), 3);
    // 3 hourly buckets for actor-a, 2 for actor-b
    assert_eq!(groups.len(), 5);
}

/// AC-3: count_by_actor returns exactly [A:5, B:3] descending; bucket=hour
/// with this op → 400.
#[tokio::test]
#[ignore]
async fn phase3_count_by_actor() {
    let (base, pool) = spawn_app().await;
    let ws = default_workspace_id(&pool).await;
    elevate_default_user_to_admin(&pool, ws).await;
    let t0 = seed_base_time();

    for i in 0..5 {
        seed_audit_event(
            &pool,
            ws,
            "user",
            "actor-A",
            "artifact_created",
            t0 + Duration::seconds(i),
        )
        .await;
    }
    for i in 0..3 {
        seed_audit_event(
            &pool,
            ws,
            "user",
            "actor-B",
            "artifact_created",
            t0 + Duration::seconds(100 + i),
        )
        .await;
    }

    let since = ts(t0);
    let until = ts(t0 + Duration::days(1));
    let resp = get(
        &base,
        &format!(
            "/api/v1/workspaces/{ws}/audit-aggregates?op=count_by_actor&since={since}&until={until}"
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["op"], "count_by_actor");
    let groups = body["groups"].as_array().expect("groups");
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0]["key"], "actor-A");
    assert_eq!(groups[0]["count"], 5);
    assert_eq!(groups[1]["key"], "actor-B");
    assert_eq!(groups[1]["count"], 3);

    let resp = get(
        &base,
        &format!(
            "/api/v1/workspaces/{ws}/audit-aggregates?op=count_by_actor&bucket=hour&since={since}&until={until}"
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// AC-4: error at T, same connector's publish_started at T+90s ⇒ one gap of
/// 90s with from_stage=error and occurred_at=T; summary.count == 1.
#[tokio::test]
#[ignore]
async fn phase3_recovery_gap() {
    let (base, pool) = spawn_app().await;
    let ws = default_workspace_id(&pool).await;
    elevate_default_user_to_admin(&pool, ws).await;
    let t0 = seed_base_time();
    let conn = insert_connector(&pool, ws, "gap-conn").await;

    seed_pipeline_event(&pool, ws, Some(conn), "error", t0).await;
    seed_pipeline_event(
        &pool,
        ws,
        Some(conn),
        "publish_started",
        t0 + Duration::seconds(90),
    )
    .await;

    let since = ts(t0 - Duration::hours(1));
    let until = ts(t0 + Duration::hours(1));
    let resp = get(
        &base,
        &format!(
            "/api/v1/workspaces/{ws}/pipeline-aggregates?op=recovery_gap&since={since}&until={until}"
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["op"], "recovery_gap");
    let items = body["items"].as_array().expect("items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["connector_id"], conn.to_string());
    assert_eq!(items[0]["gap_seconds"], 90.0);
    assert_eq!(items[0]["from_stage"], "error");
    let occurred: DateTime<Utc> =
        serde_json::from_value(items[0]["occurred_at"].clone()).expect("occurred_at");
    assert_eq!(occurred, t0);
    assert_eq!(body["summary"]["count"], 1);
    assert_eq!(body["summary"]["p50"], 90.0);
}

/// Codex finding 4 contract: intervening stages are ignored, gaps pair within
/// their own connector, unrecovered faults emit no row, NULL connectors are
/// excluded.
#[tokio::test]
#[ignore]
async fn phase3_recovery_gap_interleaved() {
    let (base, pool) = spawn_app().await;
    let ws = default_workspace_id(&pool).await;
    elevate_default_user_to_admin(&pool, ws).await;
    let t0 = seed_base_time();
    let conn_a = insert_connector(&pool, ws, "conn-a").await;
    let conn_b = insert_connector(&pool, ws, "conn-b").await;
    let conn_c = insert_connector(&pool, ws, "conn-c").await;

    // (a) interleaved first_event must not shorten the gap.
    seed_pipeline_event(&pool, ws, Some(conn_a), "error", t0).await;
    seed_pipeline_event(
        &pool,
        ws,
        Some(conn_a),
        "first_event",
        t0 + Duration::seconds(30),
    )
    .await;
    seed_pipeline_event(
        &pool,
        ws,
        Some(conn_a),
        "publish_started",
        t0 + Duration::seconds(90),
    )
    .await;

    // (b) conn-b's fault window overlaps conn-a's; pairs within its own connector.
    seed_pipeline_event(&pool, ws, Some(conn_b), "stall", t0 + Duration::seconds(10)).await;
    seed_pipeline_event(
        &pool,
        ws,
        Some(conn_b),
        "publish_started",
        t0 + Duration::seconds(40),
    )
    .await;

    // (c) unrecovered fault: no later publish_started on conn-c.
    seed_pipeline_event(&pool, ws, Some(conn_c), "error", t0 + Duration::seconds(20)).await;

    // (d) NULL-connector fault is excluded even though a recovery exists elsewhere.
    seed_pipeline_event(&pool, ws, None, "error", t0).await;

    let since = ts(t0 - Duration::hours(1));
    let until = ts(t0 + Duration::hours(1));
    let resp = get(
        &base,
        &format!(
            "/api/v1/workspaces/{ws}/pipeline-aggregates?op=recovery_gap&since={since}&until={until}"
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("json");
    let items = body["items"].as_array().expect("items");
    assert_eq!(items.len(), 2, "{items:?}");
    // ordered by occurred_at: conn-a's error at t0, conn-b's stall at t0+10
    assert_eq!(items[0]["connector_id"], conn_a.to_string());
    assert_eq!(items[0]["gap_seconds"], 90.0);
    assert_eq!(items[0]["from_stage"], "error");
    assert_eq!(items[1]["connector_id"], conn_b.to_string());
    assert_eq!(items[1]["gap_seconds"], 30.0);
    assert_eq!(items[1]["from_stage"], "stall");
    assert_eq!(body["summary"]["count"], 2);
}

/// AC-6 (403 half): a non-admin workspace member gets 403 from both
/// aggregate endpoints.
#[tokio::test]
#[ignore]
async fn phase3_non_admin_403() {
    let (base, pool) = spawn_app().await;
    let ws = default_workspace_id(&pool).await;
    // No elevation: bootstrap leaves the default user at coc 0.
    let resp = get(
        &base,
        &format!("/api/v1/workspaces/{ws}/audit-aggregates?op=count_by_actor"),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let resp = get(
        &base,
        &format!("/api/v1/workspaces/{ws}/pipeline-aggregates?op=recovery_gap"),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

// ─── Phase 4: bulk NDJSON export ─────────────────────────────────────────────

/// Bulk-seed audit events via generate_series (one-by-one INSERTs are too
/// slow for the 10,500-row AC-5 seed).
async fn bulk_seed_audit_events(pool: &PgPool, workspace_id: Uuid, n: i64, t0: DateTime<Utc>) {
    sqlx::query(
        "INSERT INTO audit_events
           (workspace_id, actor_kind, actor_ref, verb, object_kind, payload, created_at)
         SELECT $1, 'peer'::actor_kind, 'peer-1', 'peer_tool_executed', 'test_object',
                '{}'::jsonb, $2::timestamptz + make_interval(secs => i)
         FROM generate_series(1, $3) AS i",
    )
    .bind(workspace_id)
    .bind(t0)
    .bind(n)
    .execute(pool)
    .await
    .expect("bulk seed audit events");
}

/// AC-5: 10,500 rows in window → exactly 10,000 NDJSON lines + X-Next-Cursor;
/// the cursor-continued request returns the remaining 500 and no header.
#[tokio::test]
#[ignore]
async fn phase4_export_truncation_cursor() {
    let (base, pool) = spawn_app().await;
    let ws = default_workspace_id(&pool).await;
    elevate_default_user_to_admin(&pool, ws).await;
    let t0 = seed_base_time();
    bulk_seed_audit_events(&pool, ws, 10_500, t0).await;

    let since = ts(t0);
    let until = ts(t0 + Duration::days(7));
    let resp = get(
        &base,
        &format!("/api/v1/workspaces/{ws}/audit-export?since={since}&until={until}"),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "application/x-ndjson"
    );
    let cursor = resp
        .headers()
        .get("x-next-cursor")
        .expect("x-next-cursor present")
        .to_str()
        .unwrap()
        .to_owned();
    let body = resp.text().await.expect("body");
    let lines: Vec<&str> = body.lines().collect();
    assert_eq!(lines.len(), 10_000);
    for line in &lines {
        let parsed: Value = serde_json::from_str(line).expect("NDJSON line parses");
        assert!(parsed.is_object());
    }

    let resp = get(
        &base,
        &format!(
            "/api/v1/workspaces/{ws}/audit-export?since={since}&until={until}&cursor={cursor}"
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers().get("x-next-cursor").is_none());
    let body = resp.text().await.expect("body");
    assert_eq!(body.lines().count(), 500);
}

/// AC-7: 91-day span → 400; missing since → 400.
#[tokio::test]
#[ignore]
async fn phase4_export_validation() {
    let (base, pool) = spawn_app().await;
    let ws = default_workspace_id(&pool).await;
    elevate_default_user_to_admin(&pool, ws).await;
    let t0 = seed_base_time();

    let resp = get(
        &base,
        &format!(
            "/api/v1/workspaces/{ws}/audit-export?since={}&until={}",
            ts(t0),
            ts(t0 + Duration::days(91))
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["error"], "bad_request");

    let resp = get(
        &base,
        &format!("/api/v1/workspaces/{ws}/audit-export?until={}", ts(t0)),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// AC-9: while one export stream is open, a second request for the same org
/// is rejected with 429; dropping the first frees the slot.
#[tokio::test]
#[ignore]
async fn phase4_concurrent_export_429() {
    let (base, pool) = spawn_app().await;
    let ws = default_workspace_id(&pool).await;
    elevate_default_user_to_admin(&pool, ws).await;
    let t0 = seed_base_time();
    // Enough bytes that the server can't flush the whole stream into socket
    // buffers — the first response must still be in flight for the 429 check.
    bulk_seed_audit_events(&pool, ws, 10_500, t0).await;

    let since = ts(t0);
    let until = ts(t0 + Duration::days(7));
    let url = format!("/api/v1/workspaces/{ws}/audit-export?since={since}&until={until}");

    let first = get(&base, &url).await;
    assert_eq!(first.status(), StatusCode::OK);
    // Headers received, body unconsumed → permit still held.
    let second = get(&base, &url).await;
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);

    drop(first);
    // The permit frees when the server-side stream is dropped; poll briefly.
    let mut freed = false;
    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let retry = get(&base, &url).await;
        if retry.status() == StatusCode::OK {
            freed = true;
            break;
        }
        assert_eq!(retry.status(), StatusCode::TOO_MANY_REQUESTS);
    }
    assert!(freed, "export slot was never released after client drop");
}

/// AC-6 (export 403 half): non-admin member gets 403.
#[tokio::test]
#[ignore]
async fn phase4_non_admin_403() {
    let (base, pool) = spawn_app().await;
    let ws = default_workspace_id(&pool).await;
    let t0 = seed_base_time();
    let resp = get(
        &base,
        &format!(
            "/api/v1/workspaces/{ws}/audit-export?since={}&until={}",
            ts(t0),
            ts(t0 + Duration::days(1))
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
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
        let verb = if i % 3 == 0 {
            "peer_tool_executed"
        } else {
            "other_verb"
        };
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
