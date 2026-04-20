/// Phase 7 contract tests — routing classifier + role-scoped feed.
///
/// These tests are written against:
///   - Contract: md/design/ione-v1-contract.md  (entity `routing_decision`,
///               enum `routing_target`)
///   - Plan:     md/plans/ione-v1-plan.md        (Phase 7 scope)
///
/// ALL tests FAIL today because Phase 7 (migration 0006, services/router.rs,
/// feed endpoint) does not yet exist.
///
/// ──────────────────────────────────────────────────────────────────────────
/// Prerequisites:
///   docker compose up -d postgres
///   DATABASE_URL=postgres://ione:ione@localhost:5433/ione
///
/// Run (serial, ignored):
///   DATABASE_URL=postgres://ione:ione@localhost:5433/ione \
///     cargo test --test phase07_routing -- --ignored --test-threads=1
///
/// Skip Ollama-gated tests:
///   IONE_SKIP_LIVE=1 DATABASE_URL=... cargo test --test phase07_routing \
///     -- --ignored --test-threads=1
///
/// ──────────────────────────────────────────────────────────────────────────
/// Contract targets referenced per test (md/design/ione-v1-contract.md):
///
///   § Enums
///     routing_target : feed | notification | draft | peer
///
///   § routing_decision fields (DB snake_case → JSON camelCase)
///     id               → id               UUID
///     survivor_id      → survivorId       UUID (FK → survivors ON DELETE CASCADE)
///     target_kind      → targetKind       routing_target
///     target_ref       → targetRef        JSONB
///     classifier_model → classifierModel  TEXT NOT NULL
///     rationale        → rationale        TEXT NOT NULL
///     created_at       → createdAt        TIMESTAMPTZ
///
///   § API operations
///     GET /api/v1/workspaces/:id/feed?roleId=<uuid>&limit=
///       → { items: SurvivorRow[] }  (survivors with feed routing_decision for that role)
///
///   § Relationships
///     routing_decision belongs to survivor (FK CASCADE)
///     survivor → signal → workspace (cascade must reach routing_decision)
///
///   § Migration 0006 (plan Phase 7)
///     enum:  routing_target('feed','notification','draft','peer')
///     table: routing_decisions(id, survivor_id UUID FK CASCADE, target_kind routing_target,
///                              target_ref JSONB NOT NULL, classifier_model TEXT NOT NULL,
///                              rationale TEXT NOT NULL, created_at)
///     index: routing_decisions_survivor ON routing_decisions(survivor_id)
///
///   § Router service (plan Phase 7)
///     ione::services::router::parse_response(raw, severity) -> Vec<RoutingDecision>
///     ione::services::router::classify_with_response(pool, survivor_id, raw) -> Vec<RoutingDecision>
///     ione::services::router::classify_survivor(state, survivor_id) -> Vec<RoutingDecision>
///
/// ──────────────────────────────────────────────────────────────────────────
/// All tests are #[ignore]-gated and must be run with --test-threads=1.
/// ──────────────────────────────────────────────────────────────────────────
use std::net::SocketAddr;

use reqwest::StatusCode;
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;
use uuid::Uuid;

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione";

// ─── harness ──────────────────────────────────────────────────────────────────

/// Connect, run all migrations (including 0006 which does not exist yet —
/// expected failure mode for contract-red), truncate tables in FK-safe order,
/// and boot on a random port.  Returns `(base_url, pool)`.
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
        .expect("migration failed — migration 0006 may not exist yet (expected failure)");

    // Truncate in reverse-FK order.
    sqlx::query(
        "TRUNCATE routing_decisions, survivors, signals, stream_events, streams,
                  connectors, memberships, roles, messages, conversations,
                  workspaces, users, organizations
         RESTART IDENTITY CASCADE",
    )
    .execute(&pool)
    .await
    .expect(
        "truncate failed — routing_decisions table may not exist yet (expected for contract-red)",
    );

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind random port");
    let addr: SocketAddr = listener.local_addr().expect("failed to get local addr");

    let app = ione::app(pool.clone()).await;

    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("server error");
    });

    (format!("http://{}", addr), pool)
}

/// Return the id of the seeded "Operations" workspace.
async fn ops_workspace_id(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("SELECT id FROM workspaces WHERE name = 'Operations' LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("Operations workspace not found — bootstrap seed missing (expected failure)")
}

/// Insert a signal directly into the DB.  Returns the new row id.
async fn insert_signal(pool: &PgPool, workspace_id: Uuid, title: &str, severity: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO signals
           (workspace_id, source, title, body, severity, evidence)
         VALUES ($1, 'rule'::signal_source, $2, 'test body', $3::severity, '[]'::jsonb)
         RETURNING id",
    )
    .bind(workspace_id)
    .bind(title)
    .bind(severity)
    .fetch_one(pool)
    .await
    .expect("insert signal failed")
}

/// Insert a survivor directly into the DB.  Returns the new row id.
async fn insert_survivor(
    pool: &PgPool,
    signal_id: Uuid,
    verdict: &str,
    created_at: chrono::DateTime<chrono::Utc>,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO survivors
           (signal_id, critic_model, verdict, rationale, confidence, chain_of_reasoning, created_at)
         VALUES ($1, 'phi4-reasoning:14b', $2::critic_verdict, 'test rationale', 0.8, '[]'::jsonb, $3)
         RETURNING id",
    )
    .bind(signal_id)
    .bind(verdict)
    .bind(created_at)
    .fetch_one(pool)
    .await
    .expect("insert survivor failed — survivors table or critic_verdict enum may not exist yet")
}

/// Insert a routing_decision directly into the DB.  Returns the new row id.
async fn insert_routing_decision(
    pool: &PgPool,
    survivor_id: Uuid,
    target_kind: &str,
    target_ref: serde_json::Value,
    classifier_model: &str,
    rationale: &str,
    created_at: chrono::DateTime<chrono::Utc>,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO routing_decisions
           (survivor_id, target_kind, target_ref, classifier_model, rationale, created_at)
         VALUES ($1, $2::routing_target, $3, $4, $5, $6)
         RETURNING id",
    )
    .bind(survivor_id)
    .bind(target_kind)
    .bind(target_ref)
    .bind(classifier_model)
    .bind(rationale)
    .bind(created_at)
    .fetch_one(pool)
    .await
    .expect(
        "insert routing_decision failed — routing_decisions table or routing_target enum \
         may not exist yet (expected failure)",
    )
}

// ─── 1. Enum shape test ────────────────────────────────────────────────────────

/// Contract § Enums — `routing_target` must have exactly four variants in order.
///
/// Target: ione-v1-contract.md § Enums → routing_target : feed | notification | draft | peer
///
/// REASON: requires DATABASE_URL and migration 0006 (which does not exist yet).
#[tokio::test]
#[ignore]
async fn routing_target_enum_variants() {
    let (_base, pool) = spawn_app().await;

    let variants: Vec<String> =
        sqlx::query_scalar("SELECT unnest(enum_range(NULL::routing_target))::TEXT")
            .fetch_all(&pool)
            .await
            .expect(
                "query failed — routing_target enum not found \
                 (migration 0006 missing; expected failure)",
            );

    assert_eq!(
        variants,
        vec!["feed", "notification", "draft", "peer"],
        "routing_target enum must have exactly variants [feed, notification, draft, peer] \
         in declaration order, got {:?}",
        variants
    );
}

// ─── 2. Cascade on survivor delete ────────────────────────────────────────────

/// Contract § Relationships — DELETE survivor row must cascade to routing_decision rows.
///
/// Targets:
///   - contract § routing_decision: survivor_id UUID FK → survivors ON DELETE CASCADE
///   - plan Phase 7 migration 0006: survivor_id … ON DELETE CASCADE
///
/// REASON: requires migration 0006.
#[tokio::test]
#[ignore]
async fn routing_decisions_cascade_on_survivor_delete() {
    let (_base, pool) = spawn_app().await;
    let ws_id = ops_workspace_id(&pool).await;

    let signal_id = insert_signal(&pool, ws_id, "Cascade signal", "routine").await;
    let survivor_id = insert_survivor(&pool, signal_id, "survive", chrono::Utc::now()).await;
    let role_id = Uuid::new_v4();
    let decision_id = insert_routing_decision(
        &pool,
        survivor_id,
        "feed",
        json!({ "role_id": role_id }),
        "qwen3:8b",
        "routed to feed",
        chrono::Utc::now(),
    )
    .await;

    // Confirm decision exists
    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM routing_decisions WHERE id = $1")
        .bind(decision_id)
        .fetch_one(&pool)
        .await
        .expect("pre-delete count failed");
    assert_eq!(
        before, 1,
        "routing_decision must exist before survivor delete"
    );

    // Delete the parent survivor
    sqlx::query("DELETE FROM survivors WHERE id = $1")
        .bind(survivor_id)
        .execute(&pool)
        .await
        .expect("survivor delete failed");

    // routing_decision must be gone
    let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM routing_decisions WHERE id = $1")
        .bind(decision_id)
        .fetch_one(&pool)
        .await
        .expect("post-delete count failed");

    assert_eq!(
        after, 0,
        "routing_decision must be deleted when its survivor is deleted (ON DELETE CASCADE), got {}",
        after
    );
}

// ─── 3. Parse fallback by severity ────────────────────────────────────────────

/// `router::parse_response(raw, severity)` with garbage input must fall back to
/// severity-based routing:
///   - severity=routine   → one decision, target_kind=feed
///   - severity=flagged   → one decision, target_kind=notification
///   - severity=command   → one decision, target_kind=draft
///
/// Targets:
///   - plan Phase 7 § Classifier: "Fallback if LLM parse fails: map by severity"
///   - ione::services::router::parse_response (public test-hook)
///
/// REASON: requires src/services/router.rs with a public `parse_response` fn.
#[tokio::test]
#[ignore]
async fn router_parse_fallback_by_severity() {
    let garbage = "this is absolutely not valid json {{{broken";

    // routine → feed
    let decisions = ione::services::router::parse_response(garbage, "routine");
    assert_eq!(
        decisions.len(),
        1,
        "garbage input + severity=routine must produce exactly 1 fallback decision, got {}",
        decisions.len()
    );
    assert_eq!(
        decisions[0].target_kind.as_str(),
        "feed",
        "severity=routine fallback must map to target_kind=feed, got: {}",
        decisions[0].target_kind.as_str()
    );

    // flagged → notification
    let decisions = ione::services::router::parse_response(garbage, "flagged");
    assert_eq!(
        decisions.len(),
        1,
        "garbage input + severity=flagged must produce exactly 1 fallback decision, got {}",
        decisions.len()
    );
    assert_eq!(
        decisions[0].target_kind.as_str(),
        "notification",
        "severity=flagged fallback must map to target_kind=notification, got: {}",
        decisions[0].target_kind.as_str()
    );

    // command → draft
    let decisions = ione::services::router::parse_response(garbage, "command");
    assert_eq!(
        decisions.len(),
        1,
        "garbage input + severity=command must produce exactly 1 fallback decision, got {}",
        decisions.len()
    );
    assert_eq!(
        decisions[0].target_kind.as_str(),
        "draft",
        "severity=command fallback must map to target_kind=draft, got: {}",
        decisions[0].target_kind.as_str()
    );
}

// ─── 4. Explicit targets when parsed ──────────────────────────────────────────

/// `router::parse_response` given a well-formed `{targets:[{kind:"notification",...}]}`
/// must return one notification decision whose rationale matches the JSON input.
///
/// Targets:
///   - plan Phase 7 § Classifier: JSON schema `{targets: [{kind, role_id?, peer_id?, rationale}]}`
///   - ione::services::router::parse_response (public test-hook)
///
/// REASON: requires src/services/router.rs with a public `parse_response` fn.
#[tokio::test]
#[ignore]
async fn router_respects_explicit_targets_when_parsed() {
    let raw = r#"{"targets":[{"kind":"notification","rationale":"urgent weather alert for duty officer"}]}"#;
    let decisions = ione::services::router::parse_response(raw, "flagged");

    assert_eq!(
        decisions.len(),
        1,
        "well-formed single-target response must produce exactly 1 decision, got {}",
        decisions.len()
    );
    assert_eq!(
        decisions[0].target_kind.as_str(),
        "notification",
        "parsed target kind must be 'notification', got: {}",
        decisions[0].target_kind.as_str()
    );
    assert_eq!(
        decisions[0].rationale, "urgent weather alert for duty officer",
        "parsed rationale must match JSON field, got: {:?}",
        decisions[0].rationale
    );
}

// ─── 5. Multiple decisions per survivor ───────────────────────────────────────

/// `router::parse_response` given a well-formed JSON with two targets must return
/// exactly two decisions, both referencing the parsed fields.
///
/// Targets:
///   - plan Phase 7: "one survivor may produce multiple routing decisions (fan-out)"
///   - ione::services::router::parse_response (public test-hook)
///
/// REASON: requires src/services/router.rs with a public `parse_response` fn.
#[tokio::test]
#[ignore]
async fn multiple_decisions_per_survivor() {
    let role_id = Uuid::new_v4();
    let raw = format!(
        r#"{{"targets":[
            {{"kind":"feed","role_id":"{role_id}","rationale":"routine feed routing"}},
            {{"kind":"notification","rationale":"also notify duty officer"}}
        ]}}"#,
        role_id = role_id
    );

    let decisions = ione::services::router::parse_response(&raw, "flagged");

    assert_eq!(
        decisions.len(),
        2,
        "two-target response must produce exactly 2 decisions, got {}",
        decisions.len()
    );

    let kinds: Vec<&str> = decisions.iter().map(|d| d.target_kind.as_str()).collect();

    assert!(
        kinds.contains(&"feed"),
        "decisions must include a feed target, got: {:?}",
        kinds
    );
    assert!(
        kinds.contains(&"notification"),
        "decisions must include a notification target, got: {:?}",
        kinds
    );
}

// ─── 6. Scheduler insert via classify_with_response ───────────────────────────

/// `router::classify_with_response(pool, survivor_id, raw)` with a canned response
/// must insert routing_decisions rows into the DB.
///
/// Targets:
///   - plan Phase 7: scheduler runs classifier after critic, per survivor
///   - ione::services::router::classify_with_response (test hook)
///   - contract § routing_decision fields: all NOT NULL columns stored correctly
///
/// REASON: requires migration 0006 and src/services/router.rs.
#[tokio::test]
#[ignore]
async fn scheduler_run_router_for_survivor() {
    let (_base, pool) = spawn_app().await;
    let ws_id = ops_workspace_id(&pool).await;

    let signal_id = insert_signal(&pool, ws_id, "Router test signal", "flagged").await;
    let survivor_id = insert_survivor(&pool, signal_id, "survive", chrono::Utc::now()).await;

    let raw = r#"{"targets":[{"kind":"notification","rationale":"flagged signal — notify duty officer"}]}"#;

    // Call the test hook that simulates a canned model response (no Ollama needed).
    let decisions: Vec<ione::models::RoutingDecision> =
        ione::services::router::classify_with_response(&pool, survivor_id, raw, "flagged")
            .await
            .expect("classify_with_response must not return Err — router not implemented (expected failure)");

    assert!(
        !decisions.is_empty(),
        "classify_with_response must insert at least one routing_decision row (expected failure)"
    );

    // Verify rows exist in the DB.
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM routing_decisions WHERE survivor_id = $1")
            .bind(survivor_id)
            .fetch_one(&pool)
            .await
            .expect("routing_decisions count query failed");

    assert!(
        count >= 1,
        "at least one routing_decisions row must exist in DB after classify_with_response, got {}",
        count
    );
}

// ─── 7. Feed endpoint returns role-scoped survivors ───────────────────────────

/// GET /api/v1/workspaces/:id/feed?roleId=<uuid> returns survivors with a feed
/// routing_decision targeting that role_id only.
///
/// Targets:
///   - contract § API: GET /api/v1/workspaces/:id/feed?roleId=… → { items: SurvivorRow[] }
///   - plan Phase 7: target_ref JSON shape for feed: { role_id: "<uuid>" }
///   - org isolation: second survivor routed to a different role is not returned
///
/// REASON: requires migration 0006, src/services/router.rs, and feed route.
#[tokio::test]
#[ignore]
async fn feed_endpoint_returns_role_scoped_survivors() {
    let (base, pool) = spawn_app().await;
    let ws_id = ops_workspace_id(&pool).await;
    let client = reqwest::Client::new();

    // Create two role IDs — one is the "member" role we filter on.
    let member_role_id: Uuid = sqlx::query_scalar(
        "INSERT INTO roles (workspace_id, name, coc_level, permissions)
         VALUES ($1, 'member', 1, '{}'::jsonb)
         RETURNING id",
    )
    .bind(ws_id)
    .fetch_one(&pool)
    .await
    .expect("insert member role failed");

    let other_role_id: Uuid = sqlx::query_scalar(
        "INSERT INTO roles (workspace_id, name, coc_level, permissions)
         VALUES ($1, 'supervisor', 0, '{}'::jsonb)
         RETURNING id",
    )
    .bind(ws_id)
    .fetch_one(&pool)
    .await
    .expect("insert supervisor role failed");

    let now = chrono::Utc::now();

    // Survivor 1: routed to feed for member_role_id
    let sig1_id = insert_signal(&pool, ws_id, "Signal for member feed", "routine").await;
    let surv1_id = insert_survivor(&pool, sig1_id, "survive", now).await;
    insert_routing_decision(
        &pool,
        surv1_id,
        "feed",
        json!({ "role_id": member_role_id }),
        "qwen3:8b",
        "routine → member feed",
        now,
    )
    .await;

    // Survivor 2: routed to feed for other_role_id — must NOT appear in member feed
    let sig2_id = insert_signal(&pool, ws_id, "Signal for supervisor feed", "routine").await;
    let surv2_id = insert_survivor(&pool, sig2_id, "survive", now).await;
    insert_routing_decision(
        &pool,
        surv2_id,
        "feed",
        json!({ "role_id": other_role_id }),
        "qwen3:8b",
        "routine → supervisor feed",
        now,
    )
    .await;

    // Request feed for member role
    let resp = client
        .get(format!(
            "{}/api/v1/workspaces/{}/feed?roleId={}",
            base, ws_id, member_role_id
        ))
        .send()
        .await
        .expect("GET /api/v1/workspaces/:id/feed failed");

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "expected 200 from GET /api/v1/workspaces/:id/feed?roleId=…, got {} \
         (feed route not registered — expected failure)",
        resp.status()
    );

    let body: Value = resp.json().await.expect("response body not JSON");
    let items = body["items"]
        .as_array()
        .expect("feed response must have an \"items\" array");

    assert_eq!(
        items.len(),
        1,
        "feed for member role must return exactly 1 item (the survivor routed to member), got {}",
        items.len()
    );

    // Verify the returned item is survivor 1, not survivor 2
    let returned_id = items[0]["id"].as_str().unwrap_or("");
    assert_eq!(
        returned_id,
        surv1_id.to_string(),
        "feed must return the survivor routed to member_role_id, \
         got survivor id: {}",
        returned_id
    );
}

// ─── 8. Feed endpoint rejects missing roleId ──────────────────────────────────

/// GET /api/v1/workspaces/:id/feed without ?roleId= must return 400.
///
/// Targets:
///   - contract § API: roleId is required for the feed endpoint
///   - plan Phase 7: feed endpoint requires roleId query param
///
/// REASON: requires the feed route to exist.
#[tokio::test]
#[ignore]
async fn feed_endpoint_rejects_missing_role_id() {
    let (base, pool) = spawn_app().await;
    let ws_id = ops_workspace_id(&pool).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{}/api/v1/workspaces/{}/feed", base, ws_id))
        .send()
        .await
        .expect("GET /api/v1/workspaces/:id/feed (no roleId) failed");

    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "feed endpoint without roleId must return 400, got {} \
         (feed route not registered — expected failure)",
        resp.status()
    );
}

// ─── 9. classifier_model stored on decision ───────────────────────────────────

/// `classify_with_response` must store a non-null `classifier_model` on each
/// routing_decisions row it inserts.
///
/// Targets:
///   - contract § routing_decision.classifier_model: TEXT NOT NULL
///   - plan Phase 7: classifier uses `OLLAMA_ROUTER_MODEL` (default `qwen3:8b`)
///
/// REASON: requires migration 0006 and src/services/router.rs.
#[tokio::test]
#[ignore]
async fn classifier_model_stored_on_decision() {
    let (_base, pool) = spawn_app().await;
    let ws_id = ops_workspace_id(&pool).await;

    let signal_id = insert_signal(&pool, ws_id, "Model stored test", "routine").await;
    let survivor_id = insert_survivor(&pool, signal_id, "survive", chrono::Utc::now()).await;

    let raw = r#"{"targets":[{"kind":"feed","rationale":"model storage test"}]}"#;

    ione::services::router::classify_with_response(&pool, survivor_id, raw, "routine")
        .await
        .expect("classify_with_response failed (expected failure — router not implemented)");

    let model: Option<String> = sqlx::query_scalar(
        "SELECT classifier_model FROM routing_decisions WHERE survivor_id = $1 LIMIT 1",
    )
    .bind(survivor_id)
    .fetch_optional(&pool)
    .await
    .expect("classifier_model query failed");

    let model = model.expect("routing_decisions row must exist after classify_with_response");

    assert!(
        !model.is_empty(),
        "classifier_model must be non-empty on every routing_decisions row, got empty string"
    );
}

// ─── 10. Routing cascades on workspace delete ─────────────────────────────────

/// Deleting a workspace must cascade through signals → survivors → routing_decisions.
///
/// Targets:
///   - contract § Relationships: routing_decision → survivor → signal → workspace
///   - plan Phase 7 migration 0006: ON DELETE CASCADE propagates through the full chain
///
/// REASON: requires migration 0006.
#[tokio::test]
#[ignore]
async fn routing_cascades_on_workspace_delete() {
    let (base, pool) = spawn_app().await;
    let client = reqwest::Client::new();

    // Create a fresh workspace so we can delete it
    let ws_body: Value = client
        .post(format!("{}/api/v1/workspaces", base))
        .json(&json!({
            "name": "Routing Cascade WS",
            "domain": "test",
            "lifecycle": "bounded"
        }))
        .send()
        .await
        .expect("create workspace failed")
        .json()
        .await
        .expect("workspace body not JSON");

    let ws_id = Uuid::parse_str(ws_body["id"].as_str().expect("ws must have id"))
        .expect("ws id must be UUID");

    let now = chrono::Utc::now();
    let signal_id = insert_signal(&pool, ws_id, "Cascade chain signal", "routine").await;
    let survivor_id = insert_survivor(&pool, signal_id, "survive", now).await;
    let role_id = Uuid::new_v4();
    let decision_id = insert_routing_decision(
        &pool,
        survivor_id,
        "feed",
        json!({ "role_id": role_id }),
        "qwen3:8b",
        "cascade test",
        now,
    )
    .await;

    // Confirm decision exists
    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM routing_decisions WHERE id = $1")
        .bind(decision_id)
        .fetch_one(&pool)
        .await
        .expect("pre-delete count failed");
    assert_eq!(
        before, 1,
        "routing_decision must exist before workspace delete"
    );

    // Delete workspace — cascade must reach signal → survivor → routing_decision
    sqlx::query("DELETE FROM workspaces WHERE id = $1")
        .bind(ws_id)
        .execute(&pool)
        .await
        .expect("workspace delete failed");

    let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM routing_decisions WHERE id = $1")
        .bind(decision_id)
        .fetch_one(&pool)
        .await
        .expect("post-delete count failed");

    assert_eq!(
        after, 0,
        "routing_decision must be deleted when its workspace is deleted \
         (cascade: workspace → signal → survivor → routing_decision), got {}",
        after
    );
}

// ─── 11. target_ref roundtrips JSONB ─────────────────────────────────────────

/// A routing_decision inserted with `target_ref = {"role_id":"<uuid>","reason":"test"}`
/// must survive a DB round-trip and come back with the same JSONB.
///
/// Targets:
///   - contract § routing_decision.target_ref: JSONB
///   - plan Phase 7: target_ref stores role_id / peer_id / connector_id etc.
///
/// REASON: requires migration 0006.
#[tokio::test]
#[ignore]
async fn target_ref_roundtrips_jsonb() {
    let (_base, pool) = spawn_app().await;
    let ws_id = ops_workspace_id(&pool).await;

    let signal_id = insert_signal(&pool, ws_id, "JSONB roundtrip test", "routine").await;
    let survivor_id = insert_survivor(&pool, signal_id, "survive", chrono::Utc::now()).await;

    let role_id = Uuid::new_v4();
    let expected_ref = json!({ "role_id": role_id, "reason": "test" });

    let decision_id = insert_routing_decision(
        &pool,
        survivor_id,
        "feed",
        expected_ref.clone(),
        "qwen3:8b",
        "jsonb roundtrip",
        chrono::Utc::now(),
    )
    .await;

    let stored: serde_json::Value =
        sqlx::query_scalar("SELECT target_ref FROM routing_decisions WHERE id = $1")
            .bind(decision_id)
            .fetch_one(&pool)
            .await
            .expect("target_ref query failed");

    assert_eq!(
        stored["role_id"].as_str().unwrap_or(""),
        role_id.to_string(),
        "target_ref.role_id must roundtrip through JSONB, got: {}",
        stored["role_id"]
    );

    assert_eq!(
        stored["reason"].as_str().unwrap_or(""),
        "test",
        "target_ref.reason must roundtrip through JSONB, got: {}",
        stored["reason"]
    );
}

// ─── 12. Feed ordering is newest first ────────────────────────────────────────

/// Two routing_decisions to the same role, the newer survivor appears first in
/// the feed response.
///
/// Targets:
///   - plan Phase 7: feed ordered by survivor.created_at DESC (newest first)
///   - contract § API: GET /api/v1/workspaces/:id/feed?roleId=…
///
/// REASON: requires migration 0006 and feed route.
#[tokio::test]
#[ignore]
async fn feed_ordering_is_newest_first() {
    let (base, pool) = spawn_app().await;
    let ws_id = ops_workspace_id(&pool).await;
    let client = reqwest::Client::new();

    let role_id: Uuid = sqlx::query_scalar(
        "INSERT INTO roles (workspace_id, name, coc_level, permissions)
         VALUES ($1, 'duty_officer', 0, '{}'::jsonb)
         RETURNING id",
    )
    .bind(ws_id)
    .fetch_one(&pool)
    .await
    .expect("insert duty_officer role failed");

    let older_time = chrono::Utc::now() - chrono::Duration::minutes(30);
    let newer_time = chrono::Utc::now() - chrono::Duration::minutes(5);

    // Older survivor
    let sig_old_id = insert_signal(&pool, ws_id, "Older signal", "routine").await;
    let surv_old_id = insert_survivor(&pool, sig_old_id, "survive", older_time).await;
    insert_routing_decision(
        &pool,
        surv_old_id,
        "feed",
        json!({ "role_id": role_id }),
        "qwen3:8b",
        "older feed decision",
        older_time,
    )
    .await;

    // Newer survivor
    let sig_new_id = insert_signal(&pool, ws_id, "Newer signal", "routine").await;
    let surv_new_id = insert_survivor(&pool, sig_new_id, "survive", newer_time).await;
    insert_routing_decision(
        &pool,
        surv_new_id,
        "feed",
        json!({ "role_id": role_id }),
        "qwen3:8b",
        "newer feed decision",
        newer_time,
    )
    .await;

    let resp = client
        .get(format!(
            "{}/api/v1/workspaces/{}/feed?roleId={}",
            base, ws_id, role_id
        ))
        .send()
        .await
        .expect("GET /api/v1/workspaces/:id/feed failed");

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "expected 200 from GET /api/v1/workspaces/:id/feed, got {} \
         (feed route not registered — expected failure)",
        resp.status()
    );

    let body: Value = resp.json().await.expect("response body not JSON");
    let items = body["items"]
        .as_array()
        .expect("feed response must have an \"items\" array");

    assert_eq!(
        items.len(),
        2,
        "feed must return 2 items for this role, got {}",
        items.len()
    );

    // First item must be the newer survivor
    let first_id = items[0]["id"].as_str().unwrap_or("");
    assert_eq!(
        first_id,
        surv_new_id.to_string(),
        "feed must return newest survivor first (created_at DESC), \
         expected newer survivor id {}, got {}",
        surv_new_id,
        first_id
    );

    // Second item must be the older survivor
    let second_id = items[1]["id"].as_str().unwrap_or("");
    assert_eq!(
        second_id,
        surv_old_id.to_string(),
        "feed must return older survivor second, \
         expected older survivor id {}, got {}",
        surv_old_id,
        second_id
    );
}
