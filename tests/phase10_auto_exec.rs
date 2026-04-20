/// Phase 10 contract tests — rule-authorized auto-execution of commands-down.
///
/// These tests are written against:
///   - Contract: md/design/ione-v1-contract.md  (workspace.metadata.auto_exec_policies,
///               audit_event verbs: auto_authorized / delivered / auto_exec_error)
///   - Plan:     md/plans/ione-v1-plan.md        (Phase 10 scope)
///
/// ALL tests FAIL today because Phase 10 (`src/services/auto_exec.rs`) does not
/// yet exist.
///
/// ──────────────────────────────────────────────────────────────────────────
/// Prerequisites:
///   docker compose up -d postgres
///   DATABASE_URL=postgres://ione:ione@localhost:5433/ione
///
/// Run (serial, ignored):
///   DATABASE_URL=postgres://ione:ione@localhost:5433/ione \
///     cargo test --test phase10_auto_exec -- --ignored --test-threads=1
///
/// ──────────────────────────────────────────────────────────────────────────
/// Contract targets (Phase 10 plan + md/design/ione-v1-contract.md):
///
///   workspace.metadata.auto_exec_policies (JSONB array):
///     {
///       "name": string,
///       "trigger": { "signal_title_prefix": string, "severity_at_most": string },
///       "connector_id": UUID string,
///       "op": string,
///       "args_template": { "text": string (may contain {{signal.title}}, {{signal.body}}) },
///       "rate_limit_per_min": u32,
///       "severity_cap": string  (default "flagged")
///     }
///
///   src/services/auto_exec.rs (to be created):
///     pub async fn evaluate(state, survivor_id) -> Option<AutoExecDecision>
///     pub async fn evaluate_and_invoke(state, survivor_id) -> anyhow::Result<()>
///     pub fn test_reset_rate_limit(workspace_id: Uuid, policy_name: &str)
///
///   Router integration (src/services/router.rs or equivalent):
///     Before creating an approval draft, call auto_exec::evaluate_and_invoke;
///     if a policy matches, skip approval creation.
///
///   Audit rows written on auto-exec:
///     1. verb='auto_authorized'  actor_kind='system'  actor_ref='auto_exec:<policy_name>'
///     2. verb='delivered'        actor_kind='system'  actor_ref='auto_exec:<policy_name>'
///        — OR verb='delivery_failed' on connector error
///
///   Severity cap:
///     severity_cap='flagged' (default) means 'command' signals NEVER auto-execute.
///     Enforced regardless of policy match.
///
///   Rate limit:
///     In-memory token bucket keyed by (workspace_id, policy_name).
///     rate_limit_per_min=N allows N invocations per 60-second window.
///     Excess signals fall through to the approval draft path.
///
/// ──────────────────────────────────────────────────────────────────────────
/// All tests are #[ignore]-gated and must be run with --test-threads=1.
/// ──────────────────────────────────────────────────────────────────────────
use serde_json::json;
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Stand-in return type for `auto_exec::evaluate` until AutoExecDecision is defined.
/// The real type will be `anyhow::Result<Option<AutoExecDecision>>`.
/// Using `serde_json::Value` as the `Some` payload lets us call `.is_some()`/`.is_none()`
/// and print debug output without depending on the concrete decision type.
#[allow(dead_code)]
type EvalResult = anyhow::Result<Option<serde_json::Value>>;

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione";

// ─── Harness ──────────────────────────────────────────────────────────────────

/// Connect, run all migrations, truncate tables in FK-safe order, and boot
/// the server on a random port.  Returns `(base_url, pool)`.
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
        .expect("migration failed");

    // Truncate in reverse-FK order.
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
    .expect("truncate failed");

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind random port");
    let addr = listener.local_addr().expect("failed to get local addr");

    let app = ione::app(pool.clone()).await;
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("server error");
    });

    (format!("http://{}", addr), pool)
}

// ─── Seed helpers ─────────────────────────────────────────────────────────────

async fn ops_workspace_id(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("SELECT id FROM workspaces WHERE name = 'Operations' LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("Operations workspace not found — bootstrap seed missing (expected failure)")
}

#[allow(dead_code)]
async fn default_user_id(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("SELECT id FROM users WHERE email = 'default@localhost' LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("default user not found — bootstrap seed missing (expected failure)")
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

/// Insert a survivor with verdict='survive' and return its id.
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

/// Insert a rust_native connector and return its id.
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

/// Set workspace.metadata.auto_exec_policies to the given policies array.
async fn set_auto_exec_policies(pool: &PgPool, workspace_id: Uuid, policies: serde_json::Value) {
    sqlx::query(
        "UPDATE workspaces
         SET metadata = jsonb_set(metadata, '{auto_exec_policies}', $2, true)
         WHERE id = $1",
    )
    .bind(workspace_id)
    .bind(policies)
    .execute(pool)
    .await
    .expect("failed to set auto_exec_policies on workspace");
}

/// Build an AppState (pool connection) to call service functions directly.
async fn make_state(pool: PgPool) -> ione::state::AppState {
    let db_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_owned());
    let state_pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("failed to connect for state pool");
    let (_router, state) = ione::app_with_state(state_pool).await;
    let _ = pool; // passed for symmetry with Phase 9 helpers
    state
}

// ─── 1. auto_exec_matching_policy_skips_approval ──────────────────────────────

/// A workspace with a policy that matches the signal title prefix and severity
/// causes the router to auto-execute directly — no `approvals` row is created.
///
/// Preferred behavior (documented here): auto-exec skips BOTH the approval
/// creation AND the `notification_draft` artifact creation.  If the
/// implementation creates the artifact but skips the approval, the test
/// documents which shape was observed in the assertion message.
///
/// Contract targets:
///   - plan Phase 10: "auto_exec::evaluate_and_invoke fires directly; no approval row"
///   - workspace.metadata.auto_exec_policies trigger match
///
/// REASON: requires src/services/auto_exec.rs and router integration — not yet implemented.
#[tokio::test]
#[ignore]
async fn auto_exec_matching_policy_skips_approval() {
    let (_base, pool) = spawn_app().await;

    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .mount(&mock_server)
        .await;

    let ws_id = ops_workspace_id(&pool).await;

    let connector_id = insert_connector(
        &pool,
        ws_id,
        "slack",
        json!({ "webhook_url": format!("{}/", mock_server.uri()) }),
    )
    .await;

    // Install a matching policy.
    set_auto_exec_policies(
        &pool,
        ws_id,
        json!([{
            "name": "auto-file spot weather",
            "trigger": {
                "signal_title_prefix": "Spot weather update",
                "severity_at_most": "flagged"
            },
            "connector_id": connector_id.to_string(),
            "op": "send",
            "args_template": { "text": "{{signal.title}}: {{signal.body}}" },
            "rate_limit_per_min": 10,
            "severity_cap": "flagged"
        }]),
    )
    .await;

    let signal_id = insert_signal(
        &pool,
        ws_id,
        "Spot weather update: Red Flag Warning",
        "flagged",
    )
    .await;
    let survivor_id = insert_survivor(&pool, signal_id).await;

    let state = make_state(pool.clone()).await;

    // evaluate_and_invoke must exist and return Ok.
    let _ = ione::services::auto_exec::evaluate_and_invoke(&state, survivor_id)
        .await
        .expect(
            "auto_exec::evaluate_and_invoke must exist and return Ok when a policy matches \
             (not implemented — expected failure)",
        );

    // No approvals row must have been created.
    let approval_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)
         FROM approvals a
         JOIN artifacts art ON art.id = a.artifact_id
         WHERE art.workspace_id = $1",
    )
    .bind(ws_id)
    .fetch_one(&pool)
    .await
    .expect("approvals count query failed");

    assert_eq!(
        approval_count, 0,
        "auto-exec path must NOT create an approvals row; a matching policy should bypass the \
         approval queue entirely. Got {} approvals rows. \
         (Implementation note: if the approval count is > 0 the router is still creating an \
         approval draft instead of auto-executing.)",
        approval_count
    );

    // Document the artifact shape: preferred impl skips the artifact too.
    // If an artifact IS created, that is a valid alternative but must be read-only.
    let artifact_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM artifacts WHERE workspace_id = $1")
            .bind(ws_id)
            .fetch_one(&pool)
            .await
            .expect("artifacts count query failed");

    // Accept both 0 (preferred: no artifact) and 1 (artifact created but no approval).
    // The test is explicit about which outcome it found.
    if artifact_count > 1 {
        panic!(
            "auto-exec must produce at most one artifact row, got {}; \
             the implementation may be creating multiple artifacts",
            artifact_count
        );
    }
    // No assertion on artifact_count == 0 here: both 0 and 1 are acceptable per plan.
    // The strong requirement is zero approvals.
}

// ─── 2. auto_exec_writes_two_audit_rows ───────────────────────────────────────

/// After a successful auto-exec, audit_events contains exactly two rows:
/// one with verb='auto_authorized' and one with verb='delivered'.
/// Both have actor_kind='system' and actor_ref='auto_exec:<policy_name>'.
///
/// Contract targets:
///   - plan Phase 10: "two audit_events rows: auto_authorized then delivered"
///   - audit_event.actor_ref = 'auto_exec:<policy_name>'
///
/// REASON: requires src/services/auto_exec.rs — not yet implemented.
#[tokio::test]
#[ignore]
async fn auto_exec_writes_two_audit_rows() {
    let (_base, pool) = spawn_app().await;

    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .mount(&mock_server)
        .await;

    let ws_id = ops_workspace_id(&pool).await;

    let connector_id = insert_connector(
        &pool,
        ws_id,
        "slack",
        json!({ "webhook_url": format!("{}/", mock_server.uri()) }),
    )
    .await;

    let policy_name = "weather-auto-send";
    set_auto_exec_policies(
        &pool,
        ws_id,
        json!([{
            "name": policy_name,
            "trigger": {
                "signal_title_prefix": "Fire weather advisory",
                "severity_at_most": "flagged"
            },
            "connector_id": connector_id.to_string(),
            "op": "send",
            "args_template": { "text": "{{signal.title}}: {{signal.body}}" },
            "rate_limit_per_min": 10,
            "severity_cap": "flagged"
        }]),
    )
    .await;

    let signal_id = insert_signal(&pool, ws_id, "Fire weather advisory issued", "flagged").await;
    let survivor_id = insert_survivor(&pool, signal_id).await;

    let state = make_state(pool.clone()).await;

    let _ = ione::services::auto_exec::evaluate_and_invoke(&state, survivor_id)
        .await
        .expect(
            "evaluate_and_invoke must return Ok for a matching policy \
             (not implemented — expected failure)",
        );

    // Must have exactly the auto_authorized row.
    let authorized_rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT actor_kind::TEXT, actor_ref, verb
         FROM audit_events
         WHERE workspace_id = $1
           AND verb = 'auto_authorized'
         ORDER BY created_at",
    )
    .bind(ws_id)
    .fetch_all(&pool)
    .await
    .expect("audit_events query for auto_authorized failed");

    assert_eq!(
        authorized_rows.len(),
        1,
        "auto-exec must write exactly one 'auto_authorized' audit row, got {}; \
         rows: {:?}",
        authorized_rows.len(),
        authorized_rows
    );

    let (auth_actor_kind, auth_actor_ref, auth_verb) = &authorized_rows[0];
    assert_eq!(
        auth_verb, "auto_authorized",
        "first audit row verb must be 'auto_authorized', got: {auth_verb}"
    );
    assert_eq!(
        auth_actor_kind, "system",
        "auto_authorized audit row must have actor_kind='system', got: {auth_actor_kind}"
    );
    assert!(
        auth_actor_ref.contains(policy_name),
        "auto_authorized audit row actor_ref must contain the policy name '{policy_name}', \
         got: {auth_actor_ref}"
    );

    // Must have exactly the delivered row.
    let delivered_rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT actor_kind::TEXT, actor_ref, verb
         FROM audit_events
         WHERE workspace_id = $1
           AND verb = 'delivered'
         ORDER BY created_at",
    )
    .bind(ws_id)
    .fetch_all(&pool)
    .await
    .expect("audit_events query for delivered failed");

    assert_eq!(
        delivered_rows.len(),
        1,
        "auto-exec must write exactly one 'delivered' audit row, got {}; \
         rows: {:?}",
        delivered_rows.len(),
        delivered_rows
    );

    let (del_actor_kind, del_actor_ref, del_verb) = &delivered_rows[0];
    assert_eq!(
        del_verb, "delivered",
        "second audit row verb must be 'delivered', got: {del_verb}"
    );
    assert_eq!(
        del_actor_kind, "system",
        "delivered audit row must have actor_kind='system', got: {del_actor_kind}"
    );
    assert!(
        del_actor_ref.contains(policy_name),
        "delivered audit row actor_ref must contain the policy name '{policy_name}', \
         got: {del_actor_ref}"
    );
}

// ─── 3. auto_exec_respects_severity_cap ───────────────────────────────────────

/// A `command`-severity signal NEVER auto-executes even when the trigger prefix
/// matches.  It falls through to the normal approval draft path.
///
/// The severity_cap default is 'flagged'; any signal with severity > 'flagged'
/// (i.e., 'command') is blocked.
///
/// Contract targets:
///   - plan Phase 10: "severity_cap='flagged' blocks command-severity signals"
///   - "a command-severity signal NEVER auto-executes under any policy"
///
/// REASON: requires src/services/auto_exec.rs severity_cap enforcement.
#[tokio::test]
#[ignore]
async fn auto_exec_respects_severity_cap() {
    let (_base, pool) = spawn_app().await;

    // The mock must NOT be called: command-severity auto-exec is blocked.
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .expect(0)
        .mount(&mock_server)
        .await;

    let ws_id = ops_workspace_id(&pool).await;

    let connector_id = insert_connector(
        &pool,
        ws_id,
        "slack",
        json!({ "webhook_url": format!("{}/", mock_server.uri()) }),
    )
    .await;

    // Policy with explicit severity_cap='flagged' and a title prefix that
    // matches the signal below.
    set_auto_exec_policies(
        &pool,
        ws_id,
        json!([{
            "name": "capped-policy",
            "trigger": {
                "signal_title_prefix": "Mandatory evacuation",
                "severity_at_most": "command"  // policy is permissive…
            },
            "connector_id": connector_id.to_string(),
            "op": "send",
            "args_template": { "text": "{{signal.title}}: {{signal.body}}" },
            "rate_limit_per_min": 10,
            "severity_cap": "flagged"  // …but cap blocks command
        }]),
    )
    .await;

    // Fire a command-severity signal.
    let signal_id = insert_signal(&pool, ws_id, "Mandatory evacuation order", "command").await;
    let survivor_id = insert_survivor(&pool, signal_id).await;

    let state = make_state(pool.clone()).await;

    // evaluate must return None (no match) due to severity_cap.
    let decision: Option<serde_json::Value> =
        ione::services::auto_exec::evaluate(&state, survivor_id)
            .await
            .expect(
                "auto_exec::evaluate must exist and return Ok \
                 (not implemented — expected failure)",
            );

    assert!(
        decision.is_none(),
        "auto_exec::evaluate must return None for a 'command'-severity signal when \
         severity_cap='flagged'; a matching policy does NOT override the cap. Got: {:?}",
        decision
    );

    // No auto_authorized or delivered audit rows may exist.
    let auto_audit_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_events
         WHERE workspace_id = $1
           AND verb IN ('auto_authorized', 'delivered')",
    )
    .bind(ws_id)
    .fetch_one(&pool)
    .await
    .expect("audit_events count query failed");

    assert_eq!(
        auto_audit_count, 0,
        "no auto_authorized or delivered audit rows must be written for a blocked command-severity \
         signal, got {}",
        auto_audit_count
    );

    // Wiremock: zero POSTs (verify the mock expectation).
    mock_server.verify().await;

    // The signal must fall through: an approvals draft must eventually be created
    // by the normal routing path.  We verify that no auto-exec occurred; we do NOT
    // assert a draft exists here because the normal routing path is exercised
    // separately in Phase 9 tests.
}

// ─── 4. auto_exec_rate_limit_blocks_excess ────────────────────────────────────

/// With rate_limit_per_min=1, the first matching signal auto-executes but the
/// second (within the same minute) falls through to the approval draft path.
///
/// Contract targets:
///   - plan Phase 10: "rate_limit_per_min honored via in-memory token bucket"
///   - second signal within the window must NOT auto-execute
///
/// REASON: requires src/services/auto_exec.rs in-memory token bucket.
#[tokio::test]
#[ignore]
async fn auto_exec_rate_limit_blocks_excess() {
    let (_base, pool) = spawn_app().await;

    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .mount(&mock_server)
        .await;

    let ws_id = ops_workspace_id(&pool).await;

    let connector_id = insert_connector(
        &pool,
        ws_id,
        "slack",
        json!({ "webhook_url": format!("{}/", mock_server.uri()) }),
    )
    .await;

    set_auto_exec_policies(
        &pool,
        ws_id,
        json!([{
            "name": "rate-limited-policy",
            "trigger": {
                "signal_title_prefix": "Spot weather update",
                "severity_at_most": "flagged"
            },
            "connector_id": connector_id.to_string(),
            "op": "send",
            "args_template": { "text": "{{signal.title}}: {{signal.body}}" },
            "rate_limit_per_min": 1,
            "severity_cap": "flagged"
        }]),
    )
    .await;

    let state = make_state(pool.clone()).await;

    // First signal — must auto-exec.
    let sig1_id = insert_signal(&pool, ws_id, "Spot weather update A", "flagged").await;
    let surv1_id = insert_survivor(&pool, sig1_id).await;

    let decision1: Option<serde_json::Value> =
        ione::services::auto_exec::evaluate(&state, surv1_id)
            .await
            .expect(
                "evaluate must return Ok for first signal (not implemented — expected failure)",
            );

    assert!(
        decision1.is_some(),
        "first signal within rate limit window must match the policy and return Some, got None"
    );

    // Consume the token by invoking.
    let _ = ione::services::auto_exec::evaluate_and_invoke(&state, surv1_id)
        .await
        .expect("evaluate_and_invoke for first signal must succeed (expected failure)");

    // Second signal within the same minute — must be rate-limited (None).
    let sig2_id = insert_signal(&pool, ws_id, "Spot weather update B", "flagged").await;
    let surv2_id = insert_survivor(&pool, sig2_id).await;

    let decision2: Option<serde_json::Value> =
        ione::services::auto_exec::evaluate(&state, surv2_id)
            .await
            .expect(
                "evaluate must return Ok for second signal (not implemented — expected failure)",
            );

    assert!(
        decision2.is_none(),
        "second signal within the rate limit window must be blocked (rate_limit_per_min=1); \
         evaluate must return None, got Some. The token bucket must persist the first \
         invocation state in memory."
    );

    // Exactly one 'auto_authorized' row: only the first signal was auto-executed.
    let authorized_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_events
         WHERE workspace_id = $1
           AND verb = 'auto_authorized'",
    )
    .bind(ws_id)
    .fetch_one(&pool)
    .await
    .expect("audit_events count query failed");

    assert_eq!(
        authorized_count, 1,
        "exactly one 'auto_authorized' audit row must exist after first auto-exec \
         (second was rate-limited), got {}",
        authorized_count
    );
}

// ─── 5. auto_exec_rate_limit_allows_after_window ──────────────────────────────

/// After calling the test hook `auto_exec::test_reset_rate_limit`, a previously
/// exhausted policy allows a new invocation.
///
/// This test is conditional: if `test_reset_rate_limit` is not exposed, the test
/// documents that the reset mechanism is required.
///
/// Contract targets:
///   - plan Phase 10: "expose test hook auto_exec::test_reset_rate_limit"
///
/// REASON: requires auto_exec::test_reset_rate_limit (not yet implemented).
#[tokio::test]
#[ignore]
async fn auto_exec_rate_limit_allows_after_window() {
    let (_base, pool) = spawn_app().await;

    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .mount(&mock_server)
        .await;

    let ws_id = ops_workspace_id(&pool).await;
    let policy_name = "reset-test-policy";

    let connector_id = insert_connector(
        &pool,
        ws_id,
        "slack",
        json!({ "webhook_url": format!("{}/", mock_server.uri()) }),
    )
    .await;

    set_auto_exec_policies(
        &pool,
        ws_id,
        json!([{
            "name": policy_name,
            "trigger": {
                "signal_title_prefix": "NWS advisory",
                "severity_at_most": "flagged"
            },
            "connector_id": connector_id.to_string(),
            "op": "send",
            "args_template": { "text": "{{signal.title}}: {{signal.body}}" },
            "rate_limit_per_min": 1,
            "severity_cap": "flagged"
        }]),
    )
    .await;

    let state = make_state(pool.clone()).await;

    // Exhaust the rate limit.
    let sig1_id = insert_signal(&pool, ws_id, "NWS advisory Red Flag", "flagged").await;
    let surv1_id = insert_survivor(&pool, sig1_id).await;

    let _ = ione::services::auto_exec::evaluate_and_invoke(&state, surv1_id)
        .await
        .expect("first evaluate_and_invoke must succeed (not implemented — expected failure)");

    // Confirm the second signal is now rate-limited.
    let sig2_id = insert_signal(&pool, ws_id, "NWS advisory Fire Weather Watch", "flagged").await;
    let surv2_id = insert_survivor(&pool, sig2_id).await;

    let blocked: Option<serde_json::Value> = ione::services::auto_exec::evaluate(&state, surv2_id)
        .await
        .expect("evaluate before reset (not implemented — expected failure)");
    assert!(
        blocked.is_none(),
        "second invocation before reset must be blocked (rate_limit_per_min=1)"
    );

    // Reset the rate limit via the test hook.
    ione::services::auto_exec::test_reset_rate_limit(ws_id, policy_name);

    // After reset, the same second signal must be allowed.
    let allowed: Option<serde_json::Value> = ione::services::auto_exec::evaluate(&state, surv2_id)
        .await
        .expect(
            "evaluate after reset must return Ok \
                 (not implemented — expected failure: test_reset_rate_limit not exposed)",
        );

    assert!(
        allowed.is_some(),
        "after test_reset_rate_limit the policy must match again (token bucket was reset), \
         got None"
    );
}

// ─── 6. auto_exec_policy_with_nonexistent_connector_is_skipped_gracefully ─────

/// If the policy references a connector_id that does not exist in the database,
/// auto-exec must NOT panic and must NOT produce a delivery.  The signal falls
/// through to the approval draft path (or is otherwise handled gracefully).
///
/// Contract targets:
///   - plan Phase 10: "gracefully skip if connector not found"
///   - no panic, no auto-exec, fall through to draft
///
/// REASON: requires auto_exec::evaluate's connector-lookup error handling.
#[tokio::test]
#[ignore]
async fn auto_exec_policy_with_nonexistent_connector_is_skipped_gracefully() {
    let (_base, pool) = spawn_app().await;

    // Slack must NOT be called.
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .expect(0)
        .mount(&mock_server)
        .await;

    let ws_id = ops_workspace_id(&pool).await;
    let nonexistent_connector_id = Uuid::new_v4(); // random, definitely not in DB

    set_auto_exec_policies(
        &pool,
        ws_id,
        json!([{
            "name": "bad-connector-policy",
            "trigger": {
                "signal_title_prefix": "Air quality alert",
                "severity_at_most": "flagged"
            },
            "connector_id": nonexistent_connector_id.to_string(),
            "op": "send",
            "args_template": { "text": "{{signal.title}}: {{signal.body}}" },
            "rate_limit_per_min": 10,
            "severity_cap": "flagged"
        }]),
    )
    .await;

    let signal_id =
        insert_signal(&pool, ws_id, "Air quality alert: PM2.5 elevated", "flagged").await;
    let survivor_id = insert_survivor(&pool, signal_id).await;

    let state = make_state(pool.clone()).await;

    // evaluate must return None (policy skipped, connector not found) — must not panic.
    let result: anyhow::Result<Option<serde_json::Value>> =
        ione::services::auto_exec::evaluate(&state, survivor_id).await;

    assert!(
        result.is_ok(),
        "auto_exec::evaluate must not return Err when connector is not found; \
         it must skip the policy gracefully. Got: {:?}",
        result
    );

    let decision = result.unwrap();
    assert!(
        decision.is_none(),
        "auto_exec::evaluate must return None when the policy's connector_id does not \
         exist in the DB; got Some. Falling through to draft is the correct behavior."
    );

    // No auto-exec audit rows.
    let auto_audit_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_events
         WHERE workspace_id = $1
           AND verb IN ('auto_authorized', 'delivered')",
    )
    .bind(ws_id)
    .fetch_one(&pool)
    .await
    .expect("audit_events count query failed");

    assert_eq!(
        auto_audit_count, 0,
        "no auto_authorized or delivered audit rows must exist when connector lookup fails, \
         got {}",
        auto_audit_count
    );

    mock_server.verify().await;
}

// ─── 7. auto_exec_policy_with_invalid_args_template_records_defer ─────────────

/// A policy whose `args_template` contains an unresolvable template expression
/// (e.g., `{{signal.nonexistent_field}}`) must:
///   1. NOT deliver (no HTTP call).
///   2. Write an audit_events row with verb='auto_exec_error'.
///   3. Fall through to the normal approval draft path (evaluate returns None).
///
/// Contract targets:
///   - plan Phase 10: "bad template → audit verb='auto_exec_error', fall through"
///   - connector must NOT be called
///
/// REASON: requires auto_exec template rendering error handling.
#[tokio::test]
#[ignore]
async fn auto_exec_policy_with_invalid_args_template_records_defer() {
    let (_base, pool) = spawn_app().await;

    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .expect(0) // must not be called
        .mount(&mock_server)
        .await;

    let ws_id = ops_workspace_id(&pool).await;

    let connector_id = insert_connector(
        &pool,
        ws_id,
        "slack",
        json!({ "webhook_url": format!("{}/", mock_server.uri()) }),
    )
    .await;

    // args_template references an invalid / nonexistent field.
    set_auto_exec_policies(
        &pool,
        ws_id,
        json!([{
            "name": "bad-template-policy",
            "trigger": {
                "signal_title_prefix": "Template error test",
                "severity_at_most": "flagged"
            },
            "connector_id": connector_id.to_string(),
            "op": "send",
            "args_template": {
                "text": "{{signal.nonexistent_field_xyz}}: {{signal.another_missing}}"
            },
            "rate_limit_per_min": 10,
            "severity_cap": "flagged"
        }]),
    )
    .await;

    let signal_id = insert_signal(&pool, ws_id, "Template error test signal", "flagged").await;
    let survivor_id = insert_survivor(&pool, signal_id).await;

    let state = make_state(pool.clone()).await;

    // evaluate_and_invoke must NOT panic.
    let result = ione::services::auto_exec::evaluate_and_invoke(&state, survivor_id).await;

    // The function may return Ok (error handled gracefully) or Err with context.
    // Either is acceptable as long as the side-effects below hold.
    // We do not assert Ok vs Err here — only the audit row and zero delivery matter.

    let _ = result; // not asserting Ok/Err; checking side-effects

    // audit_events must have an 'auto_exec_error' row.
    let error_row: Option<(String, serde_json::Value)> = sqlx::query_as(
        "SELECT verb, payload
         FROM audit_events
         WHERE workspace_id = $1
           AND verb = 'auto_exec_error'
         LIMIT 1",
    )
    .bind(ws_id)
    .fetch_optional(&pool)
    .await
    .expect("audit_events query for auto_exec_error failed");

    let (verb, payload) = error_row.expect(
        "audit_events must have a row with verb='auto_exec_error' when the args_template \
         cannot be rendered (expected failure — error path not implemented)",
    );

    assert_eq!(
        verb, "auto_exec_error",
        "audit_event.verb must be 'auto_exec_error' on template failure, got: {verb}"
    );

    // payload must mention the policy name.
    let payload_str = payload.to_string();
    assert!(
        payload_str.contains("bad-template-policy"),
        "auto_exec_error audit payload must reference the policy name, got: {payload_str}"
    );

    // No delivery.
    mock_server.verify().await;

    // No 'auto_authorized' row.
    let authorized_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_events
         WHERE workspace_id = $1 AND verb = 'auto_authorized'",
    )
    .bind(ws_id)
    .fetch_one(&pool)
    .await
    .expect("auto_authorized count query failed");

    assert_eq!(
        authorized_count, 0,
        "no 'auto_authorized' row must be written when template rendering fails, got {}",
        authorized_count
    );
}

// ─── 8. auto_exec_policies_must_be_explicitly_configured ──────────────────────

/// A workspace whose `metadata` has no `auto_exec_policies` key never
/// auto-executes: `evaluate` returns None for any signal.
///
/// Contract targets:
///   - plan Phase 10: "policies off by default; must be explicitly added to metadata"
///
/// REASON: requires auto_exec::evaluate to check for the absent key.
#[tokio::test]
#[ignore]
async fn auto_exec_policies_must_be_explicitly_configured() {
    let (_base, pool) = spawn_app().await;

    let ws_id = ops_workspace_id(&pool).await;

    // Confirm the workspace has no auto_exec_policies key.
    let metadata: serde_json::Value =
        sqlx::query_scalar("SELECT metadata FROM workspaces WHERE id = $1")
            .bind(ws_id)
            .fetch_one(&pool)
            .await
            .expect("workspace metadata query failed");

    // The bootstrap workspace must not have auto_exec_policies.
    // If it does, remove it so the test is clean.
    if metadata.get("auto_exec_policies").is_some() {
        sqlx::query(
            "UPDATE workspaces
             SET metadata = metadata - 'auto_exec_policies'
             WHERE id = $1",
        )
        .bind(ws_id)
        .execute(&pool)
        .await
        .expect("failed to remove auto_exec_policies from workspace metadata");
    }

    // Insert a signal whose title would match any conceivable prefix.
    let signal_id = insert_signal(
        &pool,
        ws_id,
        "Spot weather update: critical conditions",
        "flagged",
    )
    .await;
    let survivor_id = insert_survivor(&pool, signal_id).await;

    let state = make_state(pool.clone()).await;

    let decision: Option<serde_json::Value> =
        ione::services::auto_exec::evaluate(&state, survivor_id)
            .await
            .expect("auto_exec::evaluate must return Ok (not implemented — expected failure)");

    assert!(
        decision.is_none(),
        "auto_exec::evaluate must return None when workspace.metadata has no \
         'auto_exec_policies' key — policies must be explicitly configured. Got: {:?}",
        decision
    );

    // No auto-exec audit rows.
    let auto_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_events
         WHERE workspace_id = $1
           AND verb IN ('auto_authorized', 'delivered', 'auto_exec_error')",
    )
    .bind(ws_id)
    .fetch_one(&pool)
    .await
    .expect("audit_events count query failed");

    assert_eq!(
        auto_count, 0,
        "no auto-exec audit rows must exist for a workspace with no auto_exec_policies, \
         got {}",
        auto_count
    );
}

// ─── 9. audit_events_record_payload_includes_policy_name ──────────────────────

/// The 'auto_authorized' audit row's `payload.policy_name` equals the matching
/// policy's `name` field exactly.
///
/// Contract targets:
///   - plan Phase 10: "payload.policy_name = matching policy name"
///   - audit_event.payload (contract § audit_event)
///
/// REASON: requires auto_exec::evaluate_and_invoke with structured payload writing.
#[tokio::test]
#[ignore]
async fn audit_events_record_payload_includes_policy_name() {
    let (_base, pool) = spawn_app().await;

    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .mount(&mock_server)
        .await;

    let ws_id = ops_workspace_id(&pool).await;

    let connector_id = insert_connector(
        &pool,
        ws_id,
        "slack",
        json!({ "webhook_url": format!("{}/", mock_server.uri()) }),
    )
    .await;

    let expected_policy_name = "precise-payload-policy";
    set_auto_exec_policies(
        &pool,
        ws_id,
        json!([{
            "name": expected_policy_name,
            "trigger": {
                "signal_title_prefix": "Payload test signal",
                "severity_at_most": "flagged"
            },
            "connector_id": connector_id.to_string(),
            "op": "send",
            "args_template": { "text": "{{signal.title}}: {{signal.body}}" },
            "rate_limit_per_min": 10,
            "severity_cap": "flagged"
        }]),
    )
    .await;

    let signal_id = insert_signal(&pool, ws_id, "Payload test signal fires", "flagged").await;
    let survivor_id = insert_survivor(&pool, signal_id).await;

    let state = make_state(pool.clone()).await;

    let _ = ione::services::auto_exec::evaluate_and_invoke(&state, survivor_id)
        .await
        .expect("evaluate_and_invoke must return Ok (not implemented — expected failure)");

    // Fetch the auto_authorized audit row's payload.
    let payload: Option<serde_json::Value> = sqlx::query_scalar(
        "SELECT payload
         FROM audit_events
         WHERE workspace_id = $1
           AND verb = 'auto_authorized'
         LIMIT 1",
    )
    .bind(ws_id)
    .fetch_optional(&pool)
    .await
    .expect("audit_events payload query failed");

    let payload = payload.expect(
        "audit_events must have an 'auto_authorized' row with a payload \
         (expected failure — auto_exec not implemented)",
    );

    let policy_name_in_payload = payload
        .get("policy_name")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    assert_eq!(
        policy_name_in_payload, expected_policy_name,
        "audit_event.payload.policy_name must equal the matching policy's 'name' field \
         ('{expected_policy_name}'), got: '{policy_name_in_payload}'. \
         Payload was: {payload}"
    );
}

// ─── Mutation check ───────────────────────────────────────────────────────────
//
// Mutations verified against the assertions above once the implementation exists.
// Basis: https://engineering.fb.com/2025/09/30/security/llms-are-the-key-to-mutation-testing-and-better-compliance/
//
// - Mutant: remove severity_cap check (allow command-severity to auto-exec)
//   → caught by auto_exec_respects_severity_cap
//     (decision.is_none() fails; mock expect(0) fails) ✓
//
// - Mutant: omit the 'auto_authorized' audit INSERT
//   → caught by auto_exec_writes_two_audit_rows
//     (authorized_rows.len() assert_eq!(1) fails) ✓
//
// - Mutant: omit the 'delivered' audit INSERT after successful send
//   → caught by auto_exec_writes_two_audit_rows
//     (delivered_rows.len() assert_eq!(1) fails) ✓
//
// - Mutant: write actor_kind='user' instead of 'system' for auto_authorized
//   → caught by auto_exec_writes_two_audit_rows
//     (auth_actor_kind assert_eq!("system") fails) ✓
//
// - Mutant: omit policy_name from the auto_authorized payload
//   → caught by audit_events_record_payload_includes_policy_name
//     (policy_name_in_payload assert_eq! fails) ✓
//
// - Mutant: use wrong policy_name in payload (e.g., hardcode "policy")
//   → caught by audit_events_record_payload_includes_policy_name
//     (exact string assertion fails) ✓
//
// - Mutant: allow second token past rate limit (token bucket not decrementing)
//   → caught by auto_exec_rate_limit_blocks_excess
//     (decision2.is_none() fails; authorized_count assert_eq!(1) fails) ✓
//
// - Mutant: panic or return Err on missing connector instead of None
//   → caught by auto_exec_policy_with_nonexistent_connector_is_skipped_gracefully
//     (result.is_ok() fails; decision.is_none() fails) ✓
//
// - Mutant: create an approval row even when auto-exec matches
//   → caught by auto_exec_matching_policy_skips_approval
//     (approval_count assert_eq!(0) fails) ✓
//
// - Mutant: auto-exec when metadata has no auto_exec_policies key
//   → caught by auto_exec_policies_must_be_explicitly_configured
//     (decision.is_none() fails) ✓
//
// - Mutant: skip writing 'auto_exec_error' on template failure
//   → caught by auto_exec_policy_with_invalid_args_template_records_defer
//     (error_row.expect panics) ✓
//
// - Mutant: deliver even when template rendering fails
//   → caught by auto_exec_policy_with_invalid_args_template_records_defer
//     (mock expect(0) fails) ✓
//
// - Mutant: test_reset_rate_limit is a no-op (bucket not actually reset)
//   → caught by auto_exec_rate_limit_allows_after_window
//     (allowed.is_some() fails after reset) ✓
