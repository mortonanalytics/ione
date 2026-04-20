/// Phase 9 contract tests — delivery (Slack + SMTP) + artifacts + approvals + audit.
///
/// These tests are written against:
///   - Contract: md/design/ione-v1-contract.md  (entities `artifact`, `approval`,
///               `audit_event`; enums `artifact_kind`, `approval_status`, `actor_kind`)
///   - Plan:     md/plans/ione-v1-plan.md        (Phase 9 scope)
///
/// ALL tests FAIL today because Phase 9 (migration 0009, src/connectors/slack.rs,
/// src/connectors/smtp.rs, src/services/delivery.rs, src/routes/artifacts.rs,
/// src/routes/approvals.rs, src/audit.rs) does not yet exist.
///
/// ──────────────────────────────────────────────────────────────────────────
/// Prerequisites:
///   docker compose up -d postgres
///   DATABASE_URL=postgres://ione:ione@localhost:5433/ione
///
/// Run (serial, ignored):
///   DATABASE_URL=postgres://ione:ione@localhost:5433/ione \
///     cargo test --test phase09_delivery -- --ignored --test-threads=1
///
/// Skip SMTP live-send tests:
///   IONE_SKIP_LIVE=1 DATABASE_URL=... cargo test --test phase09_delivery \
///     -- --ignored --test-threads=1
///
/// ──────────────────────────────────────────────────────────────────────────
/// Contract targets (md/design/ione-v1-contract.md):
///
///   Enums:
///     artifact_kind  : briefing | notification_draft | resource_order | message | report
///     approval_status: pending | approved | rejected
///     actor_kind     : user | system | peer
///
///   artifact fields (JSON camelCase):
///     id, workspaceId, kind, sourceSurvivorId, content, blobRef, createdAt
///
///   approval fields (JSON camelCase):
///     id, artifactId, approverUserId, status, comment, decidedAt
///
///   audit_event fields (JSON camelCase):
///     id, workspaceId, actorKind, actorRef, verb, objectKind, objectId,
///     payload, createdAt
///
///   Migration 0009 (plan Phase 9):
///     enums:  artifact_kind, approval_status, actor_kind
///     tables: artifacts, approvals, audit_events
///     FKs:    artifacts.workspace_id → workspaces ON DELETE CASCADE
///             artifacts.source_survivor_id → survivors ON DELETE SET NULL
///             approvals.artifact_id → artifacts ON DELETE CASCADE
///             approvals.approver_user_id → users (no action / nullable)
///             audit_events.workspace_id → workspaces ON DELETE SET NULL
///
///   API:
///     GET  /api/v1/workspaces/:id/artifacts           → { items: Artifact[] }
///     GET  /api/v1/workspaces/:id/approvals?status=   → { items: Approval[] }
///     POST /api/v1/approvals/:id                      → Approval
///       body: { decision: "approved" | "rejected", comment?: string }
///
///   Services:
///     ione::services::delivery::process_routing_decision(state, routing_decision_id)
///
///   Connectors:
///     ione::connectors::slack::SlackConnector (kind=rust_native, config.webhook_url)
///     ione::connectors::smtp::SmtpConnector  (kind=rust_native, config.{host,port,from,starttls})
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
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione";

// ─── Harness ──────────────────────────────────────────────────────────────────

/// Connect, run migrations (including 0009 which does not exist yet — expected
/// failure for contract-red), truncate tables in FK-safe order, and boot the
/// server on a random port.  Returns `(base_url, pool)`.
async fn spawn_app() -> (String, PgPool) {
    let db_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_owned());

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("failed to connect to Postgres — is `docker compose up -d postgres` running?");

    sqlx::migrate!("./migrations").run(&pool).await.expect(
        "migration failed — migration 0009 (artifacts/approvals/audit_events) \
             may not exist yet (expected failure for contract-red)",
    );

    // Truncate in reverse-FK order including Phase 9 tables.
    // If any table does not exist yet this fails — that is the expected
    // contract-red failure mode.
    sqlx::query(
        "TRUNCATE audit_events, approvals, artifacts,
                  trust_issuers, routing_decisions, survivors, signals,
                  stream_events, streams, connectors,
                  memberships, roles, messages, conversations,
                  workspaces, users, organizations
         RESTART IDENTITY CASCADE",
    )
    .execute(&pool)
    .await
    .expect(
        "truncate failed — audit_events / approvals / artifacts table may not exist yet \
         (expected for contract-red)",
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

// ─── Seed helpers ─────────────────────────────────────────────────────────────

/// Returns the id of the seeded "Operations" workspace.
async fn ops_workspace_id(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("SELECT id FROM workspaces WHERE name = 'Operations' LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("Operations workspace not found — bootstrap seed missing (expected failure)")
}

/// Returns the id of the seeded default user.
async fn default_user_id(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("SELECT id FROM users WHERE email = 'default@localhost' LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("default user not found — bootstrap seed missing (expected failure)")
}

/// Returns the first role_id for the given workspace.
async fn first_role_id(pool: &PgPool, workspace_id: Uuid) -> Uuid {
    sqlx::query_scalar("SELECT id FROM roles WHERE workspace_id = $1 LIMIT 1")
        .bind(workspace_id)
        .fetch_one(pool)
        .await
        .expect("no roles in workspace — bootstrap seed missing (expected failure)")
}

/// Insert a signal and return its id.
async fn insert_signal(pool: &PgPool, workspace_id: Uuid, title: &str, severity: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO signals (workspace_id, source, title, body, severity, evidence)
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

/// Insert a survivor and return its id.
async fn insert_survivor(pool: &PgPool, signal_id: Uuid) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO survivors
           (signal_id, critic_model, verdict, rationale, confidence, chain_of_reasoning)
         VALUES ($1, 'phi4-reasoning:14b', 'survive'::critic_verdict,
                 'test rationale', 0.9, '[]'::jsonb)
         RETURNING id",
    )
    .bind(signal_id)
    .fetch_one(pool)
    .await
    .expect("insert survivor failed — survivors table or critic_verdict enum may not exist yet")
}

/// Insert a connector of kind rust_native with the given name and config.
async fn insert_connector(
    pool: &PgPool,
    workspace_id: Uuid,
    name: &str,
    config: serde_json::Value,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO connectors (workspace_id, kind, name, config)
         VALUES ($1, 'rust_native'::connector_kind, $2, $3)
         RETURNING id",
    )
    .bind(workspace_id)
    .bind(name)
    .bind(config)
    .fetch_one(pool)
    .await
    .expect("insert connector failed")
}

/// Insert a routing_decision and return its id.
async fn insert_routing_decision(
    pool: &PgPool,
    survivor_id: Uuid,
    target_kind: &str,
    target_ref: serde_json::Value,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO routing_decisions
           (survivor_id, target_kind, target_ref, classifier_model, rationale)
         VALUES ($1, $2::routing_target, $3, 'qwen3:8b', 'test rationale')
         RETURNING id",
    )
    .bind(survivor_id)
    .bind(target_kind)
    .bind(target_ref)
    .fetch_one(pool)
    .await
    .expect(
        "insert routing_decision failed — routing_decisions table or routing_target enum \
         may not exist yet (expected failure)",
    )
}

// ─── 1. artifact_kind_enum_variants ───────────────────────────────────────────

/// Contract § Enums — `artifact_kind` must have exactly 5 variants in order.
///
/// Target:
///   - contract § Enums → artifact_kind: briefing | notification_draft |
///     resource_order | message | report
///   - plan Phase 9 migration 0009
///
/// REASON: requires DATABASE_URL and migration 0009 (artifact_kind enum).
#[tokio::test]
#[ignore]
async fn artifact_kind_enum_variants() {
    let (_base, pool) = spawn_app().await;

    let variants: Vec<String> =
        sqlx::query_scalar("SELECT unnest(enum_range(NULL::artifact_kind))::TEXT")
            .fetch_all(&pool)
            .await
            .expect(
                "query failed — artifact_kind enum not found \
                 (migration 0009 missing; expected failure)",
            );

    assert_eq!(
        variants,
        vec![
            "briefing",
            "notification_draft",
            "resource_order",
            "message",
            "report",
        ],
        "artifact_kind enum must have exactly 5 variants in declaration order, got {:?}",
        variants
    );
}

// ─── 2. approval_status_enum_variants ─────────────────────────────────────────

/// Contract § Enums — `approval_status` must have exactly 3 variants.
///
/// Target:
///   - contract § Enums → approval_status: pending | approved | rejected
///   - plan Phase 9 migration 0009
///
/// REASON: requires DATABASE_URL and migration 0009.
#[tokio::test]
#[ignore]
async fn approval_status_enum_variants() {
    let (_base, pool) = spawn_app().await;

    let variants: Vec<String> =
        sqlx::query_scalar("SELECT unnest(enum_range(NULL::approval_status))::TEXT")
            .fetch_all(&pool)
            .await
            .expect(
                "query failed — approval_status enum not found \
                 (migration 0009 missing; expected failure)",
            );

    assert_eq!(
        variants,
        vec!["pending", "approved", "rejected"],
        "approval_status enum must have exactly variants [pending, approved, rejected] \
         in declaration order, got {:?}",
        variants
    );
}

// ─── 3. actor_kind_enum_variants ──────────────────────────────────────────────

/// Contract § Enums — `actor_kind` must have exactly 3 variants.
///
/// Target:
///   - contract § Enums → actor_kind: user | system | peer
///   - plan Phase 9 migration 0009
///
/// REASON: requires DATABASE_URL and migration 0009.
#[tokio::test]
#[ignore]
async fn actor_kind_enum_variants() {
    let (_base, pool) = spawn_app().await;

    let variants: Vec<String> =
        sqlx::query_scalar("SELECT unnest(enum_range(NULL::actor_kind))::TEXT")
            .fetch_all(&pool)
            .await
            .expect(
                "query failed — actor_kind enum not found \
                 (migration 0009 missing; expected failure)",
            );

    assert_eq!(
        variants,
        vec!["user", "system", "peer"],
        "actor_kind enum must have exactly variants [user, system, peer] \
         in declaration order, got {:?}",
        variants
    );
}

// ─── 4. notification_routing_produces_delivery ────────────────────────────────

/// A `notification` routing_decision for a Slack connector triggers an immediate
/// send, records an audit_events row with verb='delivered', and wiremock receives
/// exactly one POST containing the signal title in the JSON body.
///
/// Contract targets:
///   - plan Phase 9: "notification targets → enqueue immediate send via outbound connector"
///   - audit_event.verb = 'delivered'
///   - audit_event.actor_kind = 'system', actor_ref = 'router' (autonomous path)
///   - connector row has config.webhook_url used for the HTTP call
///
/// REASON: requires ione::services::delivery::process_routing_decision,
///         src/connectors/slack.rs (rust_native, invoke op="send"),
///         audit_events table (migration 0009).
#[tokio::test]
#[ignore]
async fn notification_routing_produces_delivery() {
    let (_base, pool) = spawn_app().await;

    // Boot a wiremock server to stand in for the Slack webhook.
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .expect(1) // exactly one call
        .mount(&mock_server)
        .await;

    let ws_id = ops_workspace_id(&pool).await;
    let role_id = first_role_id(&pool, ws_id).await;

    let signal_id = insert_signal(&pool, ws_id, "High smoke density detected", "flagged").await;
    let survivor_id = insert_survivor(&pool, signal_id).await;

    // Register a Slack connector pointing at the wiremock URL.
    let connector_id = insert_connector(
        &pool,
        ws_id,
        "slack",
        json!({ "webhook_url": format!("{}/", mock_server.uri()) }),
    )
    .await;

    // Insert a notification routing_decision referencing that connector + role.
    let routing_id = insert_routing_decision(
        &pool,
        survivor_id,
        "notification",
        json!({ "connector_id": connector_id, "role_id": role_id }),
    )
    .await;

    // Construct the AppState the service requires.
    let db_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_owned());
    let state_pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("failed to connect for state pool");
    let (_router, state) = ione::app_with_state(state_pool).await;

    // Call the delivery service — this is the function under test.
    ione::services::delivery::process_routing_decision(&state, routing_id)
        .await
        .expect(
            "process_routing_decision must succeed for notification path \
             (not implemented — expected failure)",
        );

    // Wiremock assertion: exactly 1 POST was received.
    // (wiremock asserts the `expect(1)` count on drop)
    mock_server.verify().await;

    // The POST body must contain the signal title.
    let requests = mock_server.received_requests().await.expect("no requests");
    assert_eq!(
        requests.len(),
        1,
        "exactly one HTTP POST must have been sent to the Slack webhook, got {}",
        requests.len()
    );
    let body_str =
        std::str::from_utf8(&requests[0].body).expect("webhook request body must be valid UTF-8");
    assert!(
        body_str.contains("High smoke density detected"),
        "webhook POST body must contain the signal title 'High smoke density detected', \
         got: {}",
        body_str
    );

    // audit_events must have a 'delivered' row for this connector.
    let audit_row: Option<(String, String, String)> = sqlx::query_as(
        "SELECT actor_kind::TEXT, actor_ref, verb
         FROM audit_events
         WHERE verb = 'delivered'
           AND object_id = $1
         LIMIT 1",
    )
    .bind(connector_id)
    .fetch_optional(&pool)
    .await
    .expect("audit_events query failed");

    let (actor_kind, _actor_ref, verb) = audit_row.expect(
        "audit_events must have a row with verb='delivered' referencing the connector \
         after notification send (expected failure — delivery not implemented)",
    );

    assert_eq!(
        verb, "delivered",
        "audit_event.verb must be 'delivered', got: {verb}"
    );
    assert_eq!(
        actor_kind, "system",
        "autonomous notification send must record actor_kind='system', got: {actor_kind}"
    );
}

// ─── 5. draft_routing_creates_artifact_and_pending_approval ───────────────────

/// A `draft` routing_decision creates an artifact with kind='notification_draft'
/// and a pending approval row.  No HTTP call is made to Slack.
///
/// Contract targets:
///   - plan Phase 9: "draft targets → create artifact + pending approval"
///   - artifact.kind = 'notification_draft'
///   - approval.status = 'pending'
///   - approval.artifact_id = artifact.id
///   - no outbound HTTP call for the draft path
///
/// REASON: requires ione::services::delivery::process_routing_decision,
///         artifacts table and approvals table (migration 0009).
#[tokio::test]
#[ignore]
async fn draft_routing_creates_artifact_and_pending_approval() {
    let (_base, pool) = spawn_app().await;

    // Slack mock — must NOT receive any calls for the draft path.
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .expect(0) // zero calls expected for a draft
        .mount(&mock_server)
        .await;

    let ws_id = ops_workspace_id(&pool).await;
    let role_id = first_role_id(&pool, ws_id).await;

    let signal_id = insert_signal(&pool, ws_id, "Command-level resource request", "command").await;
    let survivor_id = insert_survivor(&pool, signal_id).await;

    let connector_id = insert_connector(
        &pool,
        ws_id,
        "slack",
        json!({ "webhook_url": format!("{}/", mock_server.uri()) }),
    )
    .await;

    let routing_id = insert_routing_decision(
        &pool,
        survivor_id,
        "draft",
        json!({ "connector_id": connector_id, "role_id": role_id }),
    )
    .await;

    let db_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_owned());
    let state_pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("failed to connect for state pool");
    let (_router, state) = ione::app_with_state(state_pool).await;

    ione::services::delivery::process_routing_decision(&state, routing_id)
        .await
        .expect(
            "process_routing_decision must succeed for draft path \
             (not implemented — expected failure)",
        );

    // wiremock: zero POSTs
    mock_server.verify().await;

    // artifacts table must have a notification_draft row.
    let artifact_row: Option<(Uuid, String, Uuid)> = sqlx::query_as(
        "SELECT id, kind::TEXT, source_survivor_id
         FROM artifacts
         WHERE workspace_id = $1
           AND kind = 'notification_draft'::artifact_kind
         LIMIT 1",
    )
    .bind(ws_id)
    .fetch_optional(&pool)
    .await
    .expect("artifacts query failed");

    let (artifact_id, kind, src_survivor_id) = artifact_row.expect(
        "artifacts table must contain a 'notification_draft' row after draft routing \
         (expected failure — artifacts not implemented)",
    );

    assert_eq!(
        kind, "notification_draft",
        "artifact.kind must be 'notification_draft', got: {kind}"
    );
    assert_eq!(
        src_survivor_id, survivor_id,
        "artifact.source_survivor_id must equal the survivor id, got: {src_survivor_id}"
    );

    // approvals table must have a pending row for that artifact.
    let approval_row: Option<(Uuid, String)> = sqlx::query_as(
        "SELECT id, status::TEXT
         FROM approvals
         WHERE artifact_id = $1
           AND status = 'pending'::approval_status
         LIMIT 1",
    )
    .bind(artifact_id)
    .fetch_optional(&pool)
    .await
    .expect("approvals query failed");

    let (approval_id, status) = approval_row.expect(
        "approvals table must contain a 'pending' row for the new artifact \
         (expected failure — approvals not implemented)",
    );

    assert_eq!(
        status, "pending",
        "approval.status must be 'pending', got: {status}"
    );

    let _ = approval_id; // used below in approval tests
}

// ─── 6. approval_approve_triggers_send ────────────────────────────────────────

/// POST /api/v1/approvals/:id with {decision:"approved"} sends to Slack,
/// returns the updated approval with status='approved', and records
/// audit_events rows for both 'approved' and 'delivered'.
///
/// Contract targets:
///   - POST /api/v1/approvals/:id → Approval (contract § API operations)
///   - plan Phase 9: "approval triggers send"
///   - audit_event.verb in {'approved','delivered'}
///
/// REASON: requires src/routes/approvals.rs with POST /api/v1/approvals/:id,
///         migration 0009.
#[tokio::test]
#[ignore]
async fn approval_approve_triggers_send() {
    let (base, pool) = spawn_app().await;

    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .expect(1)
        .mount(&mock_server)
        .await;

    let ws_id = ops_workspace_id(&pool).await;
    let role_id = first_role_id(&pool, ws_id).await;

    let signal_id = insert_signal(&pool, ws_id, "Approval test signal", "command").await;
    let survivor_id = insert_survivor(&pool, signal_id).await;

    let connector_id = insert_connector(
        &pool,
        ws_id,
        "slack",
        json!({ "webhook_url": format!("{}/", mock_server.uri()) }),
    )
    .await;

    let routing_id = insert_routing_decision(
        &pool,
        survivor_id,
        "draft",
        json!({ "connector_id": connector_id, "role_id": role_id }),
    )
    .await;

    // Drive the draft creation.
    let db_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_owned());
    let state_pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("state pool connect failed");
    let (_router, state) = ione::app_with_state(state_pool).await;

    ione::services::delivery::process_routing_decision(&state, routing_id)
        .await
        .expect("draft creation failed (expected failure)");

    // Retrieve the pending approval id.
    let approval_id: Uuid = sqlx::query_scalar(
        "SELECT a.id
         FROM approvals a
         JOIN artifacts art ON art.id = a.artifact_id
         WHERE art.workspace_id = $1
           AND a.status = 'pending'::approval_status
         LIMIT 1",
    )
    .bind(ws_id)
    .fetch_one(&pool)
    .await
    .expect("pending approval not found — draft creation may not have run");

    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{}/api/v1/approvals/{}", base, approval_id))
        .json(&json!({ "decision": "approved" }))
        .send()
        .await
        .expect("POST /api/v1/approvals/:id failed");

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "POST /api/v1/approvals/:id must return 200, got {} \
         (route not registered — expected failure)",
        resp.status()
    );

    let body: Value = resp.json().await.expect("response body not JSON");

    // Response must be an Approval with status='approved'.
    assert_eq!(
        body["status"], "approved",
        "decide approval response must have status='approved', got: {}",
        body["status"]
    );
    assert_eq!(
        body["id"],
        approval_id.to_string(),
        "decide approval response id must match the requested approval id"
    );
    assert!(
        !body["decidedAt"].is_null(),
        "decided_at must be non-null after approval, got: {}",
        body["decidedAt"]
    );

    // Slack must have received one POST.
    mock_server.verify().await;

    // audit_events must include both 'approved' and 'delivered'.
    let verbs: Vec<String> = sqlx::query_scalar(
        "SELECT verb FROM audit_events
         WHERE workspace_id = $1
           AND verb IN ('approved', 'delivered')
         ORDER BY created_at",
    )
    .bind(ws_id)
    .fetch_all(&pool)
    .await
    .expect("audit_events query failed");

    assert!(
        verbs.contains(&"approved".to_string()),
        "audit_events must contain a row with verb='approved' after approval decision, \
         got verbs: {:?}",
        verbs
    );
    assert!(
        verbs.contains(&"delivered".to_string()),
        "audit_events must contain a row with verb='delivered' after approval triggers send, \
         got verbs: {:?}",
        verbs
    );
}

// ─── 7. approval_reject_does_not_send ─────────────────────────────────────────

/// POST /api/v1/approvals/:id with {decision:"rejected", comment:"too noisy"}
/// does NOT call Slack, returns approval with status='rejected', and records
/// only verb='rejected' (no 'delivered').
///
/// Contract targets:
///   - POST /api/v1/approvals/:id → Approval
///   - plan Phase 9: "rejection does not trigger send"
///   - approval.comment populated from body
///
/// REASON: requires src/routes/approvals.rs, migration 0009.
#[tokio::test]
#[ignore]
async fn approval_reject_does_not_send() {
    let (base, pool) = spawn_app().await;

    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .expect(0) // must NOT be called
        .mount(&mock_server)
        .await;

    let ws_id = ops_workspace_id(&pool).await;
    let role_id = first_role_id(&pool, ws_id).await;

    let signal_id = insert_signal(&pool, ws_id, "Rejection test signal", "command").await;
    let survivor_id = insert_survivor(&pool, signal_id).await;

    let connector_id = insert_connector(
        &pool,
        ws_id,
        "slack",
        json!({ "webhook_url": format!("{}/", mock_server.uri()) }),
    )
    .await;

    let routing_id = insert_routing_decision(
        &pool,
        survivor_id,
        "draft",
        json!({ "connector_id": connector_id, "role_id": role_id }),
    )
    .await;

    let db_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_owned());
    let state_pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("state pool connect failed");
    let (_router, state) = ione::app_with_state(state_pool).await;

    ione::services::delivery::process_routing_decision(&state, routing_id)
        .await
        .expect("draft creation failed (expected failure)");

    let approval_id: Uuid = sqlx::query_scalar(
        "SELECT a.id
         FROM approvals a
         JOIN artifacts art ON art.id = a.artifact_id
         WHERE art.workspace_id = $1
           AND a.status = 'pending'::approval_status
         LIMIT 1",
    )
    .bind(ws_id)
    .fetch_one(&pool)
    .await
    .expect("pending approval not found");

    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{}/api/v1/approvals/{}", base, approval_id))
        .json(&json!({ "decision": "rejected", "comment": "too noisy" }))
        .send()
        .await
        .expect("POST /api/v1/approvals/:id failed");

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "POST /api/v1/approvals/:id (reject) must return 200, got {} (expected failure)",
        resp.status()
    );

    let body: Value = resp.json().await.expect("response body not JSON");

    assert_eq!(
        body["status"], "rejected",
        "approval response must have status='rejected', got: {}",
        body["status"]
    );
    assert_eq!(
        body["comment"], "too noisy",
        "approval.comment must be 'too noisy', got: {}",
        body["comment"]
    );

    // wiremock: zero POSTs
    mock_server.verify().await;

    // audit_events: 'rejected' present, 'delivered' absent.
    let rejected_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_events WHERE workspace_id = $1 AND verb = 'rejected'",
    )
    .bind(ws_id)
    .fetch_one(&pool)
    .await
    .expect("audit_events count query failed");

    assert_eq!(
        rejected_count, 1,
        "audit_events must have exactly one 'rejected' row, got {}",
        rejected_count
    );

    let delivered_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_events WHERE workspace_id = $1 AND verb = 'delivered'",
    )
    .bind(ws_id)
    .fetch_one(&pool)
    .await
    .expect("audit_events delivered count query failed");

    assert_eq!(
        delivered_count, 0,
        "audit_events must have zero 'delivered' rows after rejection, got {}",
        delivered_count
    );
}

// ─── 8. approval_idempotent_on_repeat_approve ─────────────────────────────────

/// Approving the same approval_id twice → only one delivery attempt; the second
/// call returns the already-approved approval without re-sending.
///
/// Contract targets:
///   - plan Phase 9: "send is idempotency-keyed on (approval_id)"
///   - wiremock receives exactly 1 POST even after two approve calls
///
/// REASON: requires POST /api/v1/approvals/:id with idempotency guard,
///         migration 0009.
#[tokio::test]
#[ignore]
async fn approval_idempotent_on_repeat_approve() {
    let (base, pool) = spawn_app().await;

    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .expect(1) // exactly 1 send even with two approve calls
        .mount(&mock_server)
        .await;

    let ws_id = ops_workspace_id(&pool).await;
    let role_id = first_role_id(&pool, ws_id).await;

    let signal_id = insert_signal(&pool, ws_id, "Idempotency test signal", "command").await;
    let survivor_id = insert_survivor(&pool, signal_id).await;

    let connector_id = insert_connector(
        &pool,
        ws_id,
        "slack",
        json!({ "webhook_url": format!("{}/", mock_server.uri()) }),
    )
    .await;

    let routing_id = insert_routing_decision(
        &pool,
        survivor_id,
        "draft",
        json!({ "connector_id": connector_id, "role_id": role_id }),
    )
    .await;

    let db_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_owned());
    let state_pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("state pool connect failed");
    let (_router, state) = ione::app_with_state(state_pool).await;

    ione::services::delivery::process_routing_decision(&state, routing_id)
        .await
        .expect("draft creation failed (expected failure)");

    let approval_id: Uuid = sqlx::query_scalar(
        "SELECT a.id
         FROM approvals a
         JOIN artifacts art ON art.id = a.artifact_id
         WHERE art.workspace_id = $1
           AND a.status = 'pending'::approval_status
         LIMIT 1",
    )
    .bind(ws_id)
    .fetch_one(&pool)
    .await
    .expect("pending approval not found");

    let client = reqwest::Client::new();
    let approve_url = format!("{}/api/v1/approvals/{}", base, approval_id);

    // First approval
    let resp1 = client
        .post(&approve_url)
        .json(&json!({ "decision": "approved" }))
        .send()
        .await
        .expect("first POST failed");
    assert_eq!(
        resp1.status(),
        StatusCode::OK,
        "first approve must return 200, got {} (expected failure)",
        resp1.status()
    );

    // Second approval on same id — must be idempotent.
    let resp2 = client
        .post(&approve_url)
        .json(&json!({ "decision": "approved" }))
        .send()
        .await
        .expect("second POST failed");
    assert_eq!(
        resp2.status(),
        StatusCode::OK,
        "second approve call must also return 200 (idempotent), got {}",
        resp2.status()
    );

    let body2: Value = resp2.json().await.expect("second response not JSON");
    assert_eq!(
        body2["status"], "approved",
        "second approve must return status='approved', got: {}",
        body2["status"]
    );

    // wiremock: exactly 1 POST (idempotency gate prevents second send)
    mock_server.verify().await;

    // audit_events: exactly 1 'delivered' row (not 2)
    let delivered_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_events WHERE workspace_id = $1 AND verb = 'delivered'",
    )
    .bind(ws_id)
    .fetch_one(&pool)
    .await
    .expect("delivered count query failed");

    assert_eq!(
        delivered_count, 1,
        "idempotent approve must produce exactly 1 'delivered' audit row, got {}",
        delivered_count
    );
}

// ─── 9. audit_event_actor_ref_on_approval_is_user ─────────────────────────────

/// When the default user approves, audit_events.actor_kind='user' and
/// actor_ref equals the default user's UUID.
///
/// Contract targets:
///   - audit_event.actorKind (contract § audit_event)
///   - audit_event.actorRef (contract § audit_event)
///   - plan Phase 9: "audit row written per approval decision"
///
/// REASON: requires src/routes/approvals.rs with AuthContext injection,
///         migration 0009.
#[tokio::test]
#[ignore]
async fn audit_event_actor_ref_on_approval_is_user() {
    let (base, pool) = spawn_app().await;

    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .mount(&mock_server)
        .await;

    let ws_id = ops_workspace_id(&pool).await;
    let role_id = first_role_id(&pool, ws_id).await;
    let user_id = default_user_id(&pool).await;

    let signal_id = insert_signal(&pool, ws_id, "Actor ref test signal", "command").await;
    let survivor_id = insert_survivor(&pool, signal_id).await;

    let connector_id = insert_connector(
        &pool,
        ws_id,
        "slack",
        json!({ "webhook_url": format!("{}/", mock_server.uri()) }),
    )
    .await;

    let routing_id = insert_routing_decision(
        &pool,
        survivor_id,
        "draft",
        json!({ "connector_id": connector_id, "role_id": role_id }),
    )
    .await;

    let db_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_owned());
    let state_pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("state pool connect failed");
    let (_router, state) = ione::app_with_state(state_pool).await;

    ione::services::delivery::process_routing_decision(&state, routing_id)
        .await
        .expect("draft creation failed (expected failure)");

    let approval_id: Uuid = sqlx::query_scalar(
        "SELECT a.id
         FROM approvals a
         JOIN artifacts art ON art.id = a.artifact_id
         WHERE art.workspace_id = $1
           AND a.status = 'pending'::approval_status
         LIMIT 1",
    )
    .bind(ws_id)
    .fetch_one(&pool)
    .await
    .expect("pending approval not found");

    let client = reqwest::Client::new();
    client
        .post(format!("{}/api/v1/approvals/{}", base, approval_id))
        .json(&json!({ "decision": "approved" }))
        .send()
        .await
        .expect("POST /api/v1/approvals/:id failed")
        .error_for_status()
        .expect("approve response was not 2xx (expected failure)");

    // The 'approved' audit row must show actor_kind='user' and actor_ref=user_id.
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT actor_kind::TEXT, actor_ref
         FROM audit_events
         WHERE workspace_id = $1
           AND verb = 'approved'
         LIMIT 1",
    )
    .bind(ws_id)
    .fetch_optional(&pool)
    .await
    .expect("audit_events query failed");

    let (actor_kind, actor_ref) = row.expect(
        "audit_events must have a 'approved' row with actor_kind/actor_ref \
         (expected failure — audit not implemented)",
    );

    assert_eq!(
        actor_kind, "user",
        "audit_event.actor_kind must be 'user' for approval decision, got: {actor_kind}"
    );

    assert_eq!(
        actor_ref,
        user_id.to_string(),
        "audit_event.actor_ref must equal the approving user's UUID ({}), got: {}",
        user_id,
        actor_ref
    );
}

// ─── 10. audit_event_actor_ref_on_autonomous_notification_is_system ───────────

/// For the non-draft (direct notification) path, audit_events.actor_kind='system'
/// and actor_ref is a recognizable system identifier ('router' or similar).
///
/// Contract targets:
///   - audit_event.actorKind = 'system' for autonomous delivery
///   - audit_event.actorRef = 'router' (or similar system token, not a UUID)
///   - plan Phase 9: "notification targets dispatch autonomously; actor=system"
///
/// REASON: requires ione::services::delivery::process_routing_decision,
///         src/audit.rs, migration 0009.
#[tokio::test]
#[ignore]
async fn audit_event_actor_ref_on_autonomous_notification_is_system() {
    let (_base, pool) = spawn_app().await;

    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .mount(&mock_server)
        .await;

    let ws_id = ops_workspace_id(&pool).await;
    let role_id = first_role_id(&pool, ws_id).await;

    let signal_id = insert_signal(&pool, ws_id, "System actor test signal", "flagged").await;
    let survivor_id = insert_survivor(&pool, signal_id).await;

    let connector_id = insert_connector(
        &pool,
        ws_id,
        "slack",
        json!({ "webhook_url": format!("{}/", mock_server.uri()) }),
    )
    .await;

    // notification path (not draft) — autonomous
    let routing_id = insert_routing_decision(
        &pool,
        survivor_id,
        "notification",
        json!({ "connector_id": connector_id, "role_id": role_id }),
    )
    .await;

    let db_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_owned());
    let state_pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("state pool connect failed");
    let (_router, state) = ione::app_with_state(state_pool).await;

    ione::services::delivery::process_routing_decision(&state, routing_id)
        .await
        .expect("process_routing_decision failed (expected failure)");

    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT actor_kind::TEXT, actor_ref
         FROM audit_events
         WHERE workspace_id = $1
           AND verb = 'delivered'
         LIMIT 1",
    )
    .bind(ws_id)
    .fetch_optional(&pool)
    .await
    .expect("audit_events query failed");

    let (actor_kind, actor_ref) = row.expect(
        "audit_events must have a 'delivered' row with actor info \
         (expected failure — audit not implemented)",
    );

    assert_eq!(
        actor_kind, "system",
        "autonomous notification must record actor_kind='system', got: {actor_kind}"
    );

    // actor_ref must be a recognizable system identifier, not a UUID.
    // The plan says "router" or similar; we accept anything non-empty and not a UUID.
    assert!(
        !actor_ref.is_empty(),
        "audit_event.actor_ref must be non-empty for system actor, got empty string"
    );
    // A UUID has the form xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx (36 chars with dashes)
    let looks_like_uuid =
        actor_ref.len() == 36 && actor_ref.chars().filter(|c| *c == '-').count() == 4;
    assert!(
        !looks_like_uuid,
        "audit_event.actor_ref for system-initiated delivery must be a system identifier \
         like 'router', not a UUID; got: {actor_ref}"
    );
}

// ─── 11. artifacts_cascade_on_workspace_delete ────────────────────────────────

/// DELETE workspace → artifacts + approvals cascade (ON DELETE CASCADE);
/// audit_events workspace_id becomes NULL (ON DELETE SET NULL), but the row
/// persists.
///
/// Contract targets:
///   - artifacts.workspace_id FK → workspaces ON DELETE CASCADE
///   - approvals.artifact_id FK → artifacts ON DELETE CASCADE
///   - audit_events.workspace_id FK → workspaces ON DELETE SET NULL
///   - plan Phase 9 migration 0009
///
/// REASON: requires migration 0009 with correct FK semantics.
#[tokio::test]
#[ignore]
async fn artifacts_cascade_on_workspace_delete() {
    let (_base, pool) = spawn_app().await;

    let ws_id = ops_workspace_id(&pool).await;

    // Seed an artifact in that workspace.
    let artifact_id: Uuid = sqlx::query_scalar(
        "INSERT INTO artifacts (workspace_id, kind, content)
         VALUES ($1, 'notification_draft'::artifact_kind, '{\"text\":\"cascade test\"}'::jsonb)
         RETURNING id",
    )
    .bind(ws_id)
    .fetch_one(&pool)
    .await
    .expect("insert artifact failed — artifacts table may not exist yet (expected failure)");

    // Seed a pending approval for that artifact.
    let approval_id: Uuid = sqlx::query_scalar(
        "INSERT INTO approvals (artifact_id, status)
         VALUES ($1, 'pending'::approval_status)
         RETURNING id",
    )
    .bind(artifact_id)
    .fetch_one(&pool)
    .await
    .expect("insert approval failed — approvals table may not exist yet (expected failure)");

    // Seed an audit_event for that workspace.
    let _audit_id: Uuid = sqlx::query_scalar(
        "INSERT INTO audit_events
           (workspace_id, actor_kind, actor_ref, verb, object_kind, payload)
         VALUES ($1, 'system'::actor_kind, 'router', 'cascade_test', 'artifact', '{}'::jsonb)
         RETURNING id",
    )
    .bind(ws_id)
    .fetch_one(&pool)
    .await
    .expect("insert audit_event failed — audit_events table may not exist yet (expected failure)");

    // Delete the workspace (cascade must fire).
    sqlx::query("DELETE FROM workspaces WHERE id = $1")
        .bind(ws_id)
        .execute(&pool)
        .await
        .expect("workspace delete failed");

    // artifacts must be gone.
    let artifact_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM artifacts WHERE id = $1")
        .bind(artifact_id)
        .fetch_one(&pool)
        .await
        .expect("artifact count query failed");
    assert_eq!(
        artifact_count, 0,
        "artifacts must be cascade-deleted when workspace is deleted, got {} rows",
        artifact_count
    );

    // approvals must be gone (cascades from artifact deletion).
    let approval_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM approvals WHERE id = $1")
        .bind(approval_id)
        .fetch_one(&pool)
        .await
        .expect("approval count query failed");
    assert_eq!(
        approval_count, 0,
        "approvals must be cascade-deleted when artifact is cascade-deleted, got {} rows",
        approval_count
    );

    // audit_events must still exist, but workspace_id must be NULL.
    let audit_row: Option<Option<Uuid>> =
        sqlx::query_scalar("SELECT workspace_id FROM audit_events WHERE id = $1")
            .bind(_audit_id)
            .fetch_optional(&pool)
            .await
            .expect("audit_event query failed");

    let workspace_id_after = audit_row.expect(
        "audit_events row must still exist after workspace delete (ON DELETE SET NULL, \
         not CASCADE); expected failure if audit_events missing",
    );

    assert!(
        workspace_id_after.is_none(),
        "audit_event.workspace_id must be NULL after workspace delete (ON DELETE SET NULL), \
         got: {:?}",
        workspace_id_after
    );
}

// ─── 12. list_approvals_filter_pending_returns_pending_only ───────────────────

/// GET /api/v1/workspaces/:id/approvals?status=pending returns only pending
/// approvals; already-approved rows are excluded.
///
/// Contract targets:
///   - GET /api/v1/workspaces/:id/approvals?status= (contract § API operations)
///   - plan Phase 9: "list approvals with status filter"
///
/// REASON: requires src/routes/approvals.rs (GET handler), migration 0009.
#[tokio::test]
#[ignore]
async fn list_approvals_filter_pending_returns_pending_only() {
    let (base, pool) = spawn_app().await;

    let ws_id = ops_workspace_id(&pool).await;

    // Seed artifact 1 → pending approval.
    let artifact_pending: Uuid = sqlx::query_scalar(
        "INSERT INTO artifacts (workspace_id, kind, content)
         VALUES ($1, 'notification_draft'::artifact_kind, '{\"text\":\"pending\"}'::jsonb)
         RETURNING id",
    )
    .bind(ws_id)
    .fetch_one(&pool)
    .await
    .expect("insert artifact (pending) failed — expected failure");

    sqlx::query(
        "INSERT INTO approvals (artifact_id, status) VALUES ($1, 'pending'::approval_status)",
    )
    .bind(artifact_pending)
    .execute(&pool)
    .await
    .expect("insert pending approval failed — expected failure");

    // Seed artifact 2 → approved approval.
    let artifact_approved: Uuid = sqlx::query_scalar(
        "INSERT INTO artifacts (workspace_id, kind, content)
         VALUES ($1, 'notification_draft'::artifact_kind, '{\"text\":\"approved\"}'::jsonb)
         RETURNING id",
    )
    .bind(ws_id)
    .fetch_one(&pool)
    .await
    .expect("insert artifact (approved) failed");

    sqlx::query(
        "INSERT INTO approvals (artifact_id, status, decided_at)
         VALUES ($1, 'approved'::approval_status, now())",
    )
    .bind(artifact_approved)
    .execute(&pool)
    .await
    .expect("insert approved approval failed");

    let client = reqwest::Client::new();
    let resp = client
        .get(format!(
            "{}/api/v1/workspaces/{}/approvals?status=pending",
            base, ws_id
        ))
        .send()
        .await
        .expect("GET approvals failed");

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "GET /api/v1/workspaces/:id/approvals?status=pending must return 200, got {} \
         (route not registered — expected failure)",
        resp.status()
    );

    let body: Value = resp.json().await.expect("response not JSON");
    let items = body["items"]
        .as_array()
        .expect("response must have an 'items' array (expected failure — route not implemented)");

    assert_eq!(
        items.len(),
        1,
        "GET approvals?status=pending must return exactly 1 item, got {}; \
         items: {:?}",
        items.len(),
        items
    );

    assert_eq!(
        items[0]["status"], "pending",
        "returned approval must have status='pending', got: {}",
        items[0]["status"]
    );
}

// ─── 13. slack_send_failure_records_error_status_and_preserves_audit ──────────

/// When the Slack webhook returns 500, delivery records audit_events with
/// verb='delivery_failed', the connector's status becomes 'error', and
/// connector.last_error is populated.
///
/// Contract targets:
///   - connector.status = 'error' on HTTP failure
///   - connector.lastError populated
///   - audit_event.verb = 'delivery_failed'
///   - plan Phase 9: "surface last_error in UI; connector.status='error' on send fail"
///
/// REASON: requires ione::services::delivery::process_routing_decision with error
///         handling, src/connectors/slack.rs returning Err on non-2xx,
///         connector.update_status(...), migration 0009.
#[tokio::test]
#[ignore]
async fn slack_send_failure_records_error_status_and_preserves_audit() {
    let (_base, pool) = spawn_app().await;

    // Wiremock returns 500 to simulate Slack being down.
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(500).set_body_string("internal error"))
        .expect(1)
        .mount(&mock_server)
        .await;

    let ws_id = ops_workspace_id(&pool).await;
    let role_id = first_role_id(&pool, ws_id).await;

    let signal_id = insert_signal(&pool, ws_id, "Failure test signal", "flagged").await;
    let survivor_id = insert_survivor(&pool, signal_id).await;

    let connector_id = insert_connector(
        &pool,
        ws_id,
        "slack",
        json!({ "webhook_url": format!("{}/", mock_server.uri()) }),
    )
    .await;

    let routing_id = insert_routing_decision(
        &pool,
        survivor_id,
        "notification",
        json!({ "connector_id": connector_id, "role_id": role_id }),
    )
    .await;

    let db_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_owned());
    let state_pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("state pool connect failed");
    let (_router, state) = ione::app_with_state(state_pool).await;

    // The delivery call may return Ok (failure recorded gracefully) or Err —
    // either is acceptable as long as the side-effects below are present.
    let _ = ione::services::delivery::process_routing_decision(&state, routing_id).await;

    mock_server.verify().await;

    // audit_events must have a 'delivery_failed' row with error info in payload.
    let audit_row: Option<(String, serde_json::Value)> = sqlx::query_as(
        "SELECT verb, payload
         FROM audit_events
         WHERE workspace_id = $1
           AND verb = 'delivery_failed'
         LIMIT 1",
    )
    .bind(ws_id)
    .fetch_optional(&pool)
    .await
    .expect("audit_events query failed");

    let (verb, payload) = audit_row.expect(
        "audit_events must have a 'delivery_failed' row when Slack returns 500 \
         (expected failure — error path not implemented)",
    );

    assert_eq!(
        verb, "delivery_failed",
        "audit_event.verb must be 'delivery_failed', got: {verb}"
    );

    // payload must include some error description.
    let payload_str = payload.to_string();
    assert!(
        payload_str.contains("500") || payload_str.to_lowercase().contains("error"),
        "audit_event.payload must reference the HTTP error, got: {payload_str}"
    );

    // connector.status must be 'error'.
    let (connector_status, last_error): (String, Option<String>) =
        sqlx::query_as("SELECT status::TEXT, last_error FROM connectors WHERE id = $1")
            .bind(connector_id)
            .fetch_one(&pool)
            .await
            .expect("connector status query failed");

    assert_eq!(
        connector_status, "error",
        "connector.status must be 'error' after a failed send, got: {connector_status}"
    );

    assert!(
        last_error.is_some() && !last_error.as_deref().unwrap_or("").is_empty(),
        "connector.last_error must be populated after a failed send, got: {:?}",
        last_error
    );
}

// ─── 14. smtp_connector_send_is_invoked ───────────────────────────────────────

/// The delivery service selects the SMTP connector kind for an SMTP connector row
/// and attempts a send.  This test is guarded by `IONE_SKIP_LIVE` because a real
/// MX stub is heavy — when the env var is set the test verifies only the audit
/// row exists.  Without the guard the test verifies via
/// `ione::connectors::smtp::last_test_sent()` if available, or the audit row.
///
/// Contract targets:
///   - connector.kind = 'rust_native', config.{host, port, from, starttls}
///   - plan Phase 9: "SMTP connector src/connectors/smtp.rs"
///   - delivery::process_routing_decision selects correct connector kind
///
/// REASON: requires src/connectors/smtp.rs (rust_native), migration 0009.
#[tokio::test]
#[ignore]
async fn smtp_connector_send_is_invoked() {
    let (_base, pool) = spawn_app().await;

    let ws_id = ops_workspace_id(&pool).await;
    let role_id = first_role_id(&pool, ws_id).await;

    // Use a loopback SMTP address.  If IONE_SKIP_LIVE is set we verify only the
    // audit row (the delivery call may fail to connect); otherwise the
    // implementation is expected to expose a test hook.
    let skip_live = std::env::var("IONE_SKIP_LIVE").is_ok();

    let smtp_config = json!({
        "host": "127.0.0.1",
        "port": 2525,
        "from": "ione@test.local",
        "starttls": false,
        "to": "recipient@test.local"
    });

    let signal_id = insert_signal(&pool, ws_id, "SMTP delivery test signal", "flagged").await;
    let survivor_id = insert_survivor(&pool, signal_id).await;

    let connector_id = insert_connector(&pool, ws_id, "smtp", smtp_config).await;

    let routing_id = insert_routing_decision(
        &pool,
        survivor_id,
        "notification",
        json!({ "connector_id": connector_id, "role_id": role_id }),
    )
    .await;

    let db_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_owned());
    let state_pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("state pool connect failed");
    let (_router, state) = ione::app_with_state(state_pool).await;

    // We accept both Ok and Err from the call — the test checks side-effects.
    let delivery_result: anyhow::Result<()> =
        ione::services::delivery::process_routing_decision(&state, routing_id).await;

    if skip_live {
        // In IONE_SKIP_LIVE mode we only verify the correct connector kind was
        // selected by confirming an audit row was written (either 'delivered' or
        // 'delivery_failed' — both prove the SMTP path was entered, not Slack).
        let audit_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_events
             WHERE workspace_id = $1
               AND (verb = 'delivered' OR verb = 'delivery_failed')
               AND object_id = $2",
        )
        .bind(ws_id)
        .bind(connector_id)
        .fetch_one(&pool)
        .await
        .expect("audit_events count query failed");

        assert!(
            audit_count >= 1,
            "SMTP delivery path must produce an audit row (delivered or delivery_failed) \
             when process_routing_decision is called with an smtp connector; got {} rows \
             (expected failure — smtp connector not implemented)",
            audit_count
        );
    } else {
        // Without IONE_SKIP_LIVE: check the test hook if available, otherwise
        // assert the delivery succeeded and the audit row is present.
        delivery_result.expect(
            "process_routing_decision with smtp connector must succeed \
             (expected failure — smtp connector not implemented)",
        );

        // Check test hook if the implementation exposes one.
        // REASON: #[cfg(any())] is permanently false — the smtp::last_test_sent
        // test hook does not exist yet; this block documents the expected interface
        // without triggering an unexpected_cfg warning.
        #[cfg(any())]
        {
            let sent = ione::connectors::smtp::last_test_sent();
            assert!(
                sent.is_some(),
                "smtp::last_test_sent() must return Some after a successful send \
                 (expected failure — test hook not implemented)"
            );
            let email = sent.unwrap();
            assert!(
                email.body.contains("SMTP delivery test signal"),
                "sent email body must contain the signal title, got: {}",
                email.body
            );
        }

        let delivered_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_events
             WHERE workspace_id = $1
               AND verb = 'delivered'
               AND object_id = $2",
        )
        .bind(ws_id)
        .bind(connector_id)
        .fetch_one(&pool)
        .await
        .expect("audit delivered count query failed");

        assert_eq!(
            delivered_count, 1,
            "SMTP delivery must produce exactly one 'delivered' audit row, got {}",
            delivered_count
        );
    }
}

// ─── Mutation check ───────────────────────────────────────────────────────────
//
// These mutations document test strength once the implementation exists.
// Basis: https://engineering.fb.com/2025/09/30/security/llms-are-the-key-to-mutation-testing-and-better-compliance/
//
// - Mutant: remove 'notification_draft' from artifact_kind enum declaration
//   → caught by artifact_kind_enum_variants (exact variant list assertion) ✓
//
// - Mutant: change artifact_kind enum to ['pending','approved','rejected']
//   → caught by approval_status_enum_variants (separate count + exact list) ✓
//
// - Mutant: send HTTP call even on draft path (remove draft/notification branch)
//   → caught by draft_routing_creates_artifact_and_pending_approval
//     (wiremock expects(0) fails) ✓
//
// - Mutant: omit artifacts INSERT for draft path
//   → caught by draft_routing_creates_artifact_and_pending_approval
//     (artifact_row.expect panics) ✓
//
// - Mutant: skip writing approval row after artifact creation
//   → caught by draft_routing_creates_artifact_and_pending_approval
//     (approval_row.expect panics) ✓
//
// - Mutant: trigger send on rejection (swap branch)
//   → caught by approval_reject_does_not_send
//     (wiremock expects(0) fails; delivered_count assert_eq!(0) fails) ✓
//
// - Mutant: allow second approve to re-send
//   → caught by approval_idempotent_on_repeat_approve
//     (wiremock expects(1) fails; delivered_count assert_eq!(1) fails) ✓
//
// - Mutant: record actor_kind='system' for human approval decision
//   → caught by audit_event_actor_ref_on_approval_is_user
//     (actor_kind assert_eq!('user') fails) ✓
//
// - Mutant: record actor_kind='user' for autonomous notification
//   → caught by audit_event_actor_ref_on_autonomous_notification_is_system
//     (actor_kind assert_eq!('system') fails) ✓
//
// - Mutant: use ON DELETE CASCADE for audit_events.workspace_id instead of SET NULL
//   → caught by artifacts_cascade_on_workspace_delete
//     (audit_row.expect panics — row gone) ✓
//
// - Mutant: use ON DELETE CASCADE for artifacts.workspace_id instead of SET NULL
//   → would leave artifacts orphaned; caught by artifacts_cascade_on_workspace_delete
//     (artifact_count assert_eq!(0) fails when cascade is broken) ✓
//
// - Mutant: omit status filter in list_approvals query (returns all statuses)
//   → caught by list_approvals_filter_pending_returns_pending_only
//     (items.len() assert_eq!(1) fails — would return 2) ✓
//
// - Mutant: skip writing audit_event on send failure
//   → caught by slack_send_failure_records_error_status_and_preserves_audit
//     (audit_row.expect panics) ✓
//
// - Mutant: skip setting connector.status='error' on send failure
//   → caught by slack_send_failure_records_error_status_and_preserves_audit
//     (connector_status assert_eq!('error') fails) ✓
//
// - Mutant: delivery selects Slack connector for smtp connector row
//   → caught by smtp_connector_send_is_invoked
//     (in non-SKIP_LIVE mode: real SMTP op would differ; audit row assertion) ✓
