/// Phase 6 contract tests — adversarial critic → survivors.
///
/// These tests are written against:
///   - Contract: md/design/ione-v1-contract.md  (entity `survivor`, enum `critic_verdict`)
///   - Plan:     md/plans/ione-v1-plan.md        (Phase 6 scope)
///
/// ALL tests FAIL today because Phase 6 (migration 0005, services/critic.rs,
/// routes/survivors.rs) does not yet exist.
///
/// ──────────────────────────────────────────────────────────────────────────
/// Prerequisites:
///   docker compose up -d postgres
///   DATABASE_URL=postgres://ione:ione@localhost:5433/ione
///
/// Run (serial, ignored):
///   DATABASE_URL=postgres://ione:ione@localhost:5433/ione \
///     cargo test --test phase06_critic -- --ignored --test-threads=1
///
/// Skip Ollama-gated tests:
///   IONE_SKIP_LIVE=1 DATABASE_URL=... cargo test --test phase06_critic \
///     -- --ignored --test-threads=1
///
/// ──────────────────────────────────────────────────────────────────────────
/// Contract targets referenced per test (md/design/ione-v1-contract.md):
///
///   § Enums
///     critic_verdict : survive | reject | defer
///
///   § survivor fields (DB snake_case → JSON camelCase)
///     id                 → id               UUID
///     signal_id          → signalId          UUID (UNIQUE FK → signals ON DELETE CASCADE)
///     critic_model       → criticModel       TEXT
///     verdict            → verdict           critic_verdict
///     rationale          → rationale         TEXT
///     confidence         → confidence        REAL (0.0–1.0)
///     chain_of_reasoning → chainOfReasoning  JSONB (default '[]')
///     created_at         → createdAt         TIMESTAMPTZ
///
///   § API operations
///     GET /api/v1/workspaces/:id/survivors?verdict=survive|reject|defer
///       → { items: Survivor[] }   ordered by survivor.created_at DESC
///
///   § Relationships
///     survivor belongs to signal (1:1, FK CASCADE)
///     signal belongs to workspace (FK CASCADE) — cascade must reach survivor
///
///   § Migration 0005 (plan Phase 6)
///     enum: critic_verdict('survive','reject','defer')
///     table: survivors(id, signal_id UUID UNIQUE FK CASCADE, critic_model TEXT,
///                      verdict critic_verdict, rationale TEXT NOT NULL,
///                      confidence REAL NOT NULL, chain_of_reasoning JSONB default '[]',
///                      created_at)
///
///   § Critic service (plan Phase 6)
///     ione::services::critic::evaluate_signal(state, signal_id) -> Result<Option<Survivor>>
///     ione::services::critic::parse_response(raw: &str) -> (verdict, confidence, rationale, steps)
///     parse failure → defer, confidence 0.0, non-empty rationale, empty steps
///     network/Ollama error → defer (same path)
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

/// Connect, run all migrations (including 0005 which does not exist yet —
/// expected failure mode for contract-red), wipe tables in FK-safe order,
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
        .expect("migration failed — migration 0005 may not exist yet (expected failure)");

    // Truncate in reverse-FK order.
    // survivors must come before signals; signals before workspaces.
    sqlx::query(
        "TRUNCATE survivors, signals, stream_events, streams, connectors,
                  memberships, roles, messages, conversations,
                  workspaces, users, organizations
         RESTART IDENTITY CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate failed — survivors table may not exist yet (expected for contract-red)");

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
async fn insert_signal(
    pool: &PgPool,
    workspace_id: Uuid,
    title: &str,
    body: &str,
    severity: &str,
    evidence: serde_json::Value,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO signals
           (workspace_id, source, title, body, severity, evidence)
         VALUES ($1, 'rule'::signal_source, $2, $3, $4::severity, $5)
         RETURNING id",
    )
    .bind(workspace_id)
    .bind(title)
    .bind(body)
    .bind(severity)
    .bind(evidence)
    .fetch_one(pool)
    .await
    .expect("insert signal failed")
}

/// Insert a survivor directly into the DB.  Returns the new row id.
/// Mirrors the contract's survivors table exactly.
async fn insert_survivor(
    pool: &PgPool,
    signal_id: Uuid,
    critic_model: &str,
    verdict: &str,
    rationale: &str,
    confidence: f32,
    chain_of_reasoning: serde_json::Value,
    created_at: chrono::DateTime<chrono::Utc>,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO survivors
           (signal_id, critic_model, verdict, rationale, confidence, chain_of_reasoning, created_at)
         VALUES ($1, $2, $3::critic_verdict, $4, $5, $6, $7)
         RETURNING id",
    )
    .bind(signal_id)
    .bind(critic_model)
    .bind(verdict)
    .bind(rationale)
    .bind(confidence)
    .bind(chain_of_reasoning)
    .bind(created_at)
    .fetch_one(pool)
    .await
    .expect("insert survivor failed — survivors table or critic_verdict enum may not exist yet (expected failure)")
}

// ─── Enum shape tests ─────────────────────────────────────────────────────────

/// Contract § Enums — `critic_verdict` must have exactly three variants in order.
///
/// Target: ione-v1-contract.md § Enums → critic_verdict : survive | reject | defer
///
/// REASON: requires DATABASE_URL and migration 0005 (which does not exist yet).
#[tokio::test]
#[ignore]
async fn critic_verdict_enum_variants() {
    let (_base, pool) = spawn_app().await;

    let variants: Vec<String> =
        sqlx::query_scalar("SELECT unnest(enum_range(NULL::critic_verdict))::TEXT")
            .fetch_all(&pool)
            .await
            .expect(
                "query failed — critic_verdict enum not found \
                 (migration 0005 missing; expected failure)",
            );

    assert_eq!(
        variants,
        vec!["survive", "reject", "defer"],
        "critic_verdict enum must have exactly variants [survive, reject, defer] \
         in declaration order, got {:?}",
        variants
    );
}

// ─── Schema / constraint tests ────────────────────────────────────────────────

/// Contract § survivor.signal_id — UNIQUE constraint: inserting two survivors
/// for the same signal_id must fail with a unique violation.
///
/// Targets:
///   - contract § survivor: signal_id UUID UNIQUE
///   - plan Phase 6 migration 0005: UNIQUE constraint on signal_id
///
/// REASON: requires migration 0005.
#[tokio::test]
#[ignore]
async fn survivors_signal_id_is_unique() {
    let (_base, pool) = spawn_app().await;
    let ws_id = ops_workspace_id(&pool).await;

    let signal_id = insert_signal(
        &pool,
        ws_id,
        "Unique test signal",
        "body",
        "routine",
        json!([]),
    )
    .await;

    let now = chrono::Utc::now();

    // First insert — must succeed
    insert_survivor(
        &pool,
        signal_id,
        "phi4-reasoning:14b",
        "survive",
        "grounded",
        0.8,
        json!([]),
        now,
    )
    .await;

    // Second insert for the same signal_id — must fail
    let second = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO survivors
           (signal_id, critic_model, verdict, rationale, confidence, chain_of_reasoning, created_at)
         VALUES ($1, $2, $3::critic_verdict, $4, $5, $6, $7)
         RETURNING id",
    )
    .bind(signal_id)
    .bind("phi4-reasoning:14b")
    .bind("reject")
    .bind("duplicate attempt")
    .bind(0.1_f32)
    .bind(json!([]))
    .bind(now)
    .fetch_one(&pool)
    .await;

    assert!(
        second.is_err(),
        "inserting a second survivor for the same signal_id must fail \
         due to UNIQUE constraint — second insert unexpectedly succeeded"
    );

    // Confirm the error is a unique violation (not some other constraint)
    let err_str = second.unwrap_err().to_string();
    assert!(
        err_str.contains("unique") || err_str.contains("duplicate"),
        "error must mention unique/duplicate, got: {}",
        err_str
    );
}

/// Contract § Relationships — DELETE signal row must cascade to survivor row.
///
/// Targets:
///   - contract § Relationships: survivor belongs to signal (FK CASCADE)
///   - plan Phase 6 migration 0005: signal_id … ON DELETE CASCADE
///
/// REASON: requires migration 0005.
#[tokio::test]
#[ignore]
async fn survivor_cascades_on_signal_delete() {
    let (_base, pool) = spawn_app().await;
    let ws_id = ops_workspace_id(&pool).await;

    let signal_id = insert_signal(&pool, ws_id, "Cascade test", "body", "routine", json!([])).await;

    let now = chrono::Utc::now();
    let survivor_id = insert_survivor(
        &pool,
        signal_id,
        "phi4-reasoning:14b",
        "survive",
        "grounded signal",
        0.7,
        json!([]),
        now,
    )
    .await;

    // Confirm survivor exists
    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM survivors WHERE id = $1")
        .bind(survivor_id)
        .fetch_one(&pool)
        .await
        .expect("pre-delete count failed");
    assert_eq!(before, 1, "survivor must exist before signal delete");

    // Delete the parent signal
    sqlx::query("DELETE FROM signals WHERE id = $1")
        .bind(signal_id)
        .execute(&pool)
        .await
        .expect("signal delete failed");

    // Survivor must be gone
    let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM survivors WHERE id = $1")
        .bind(survivor_id)
        .fetch_one(&pool)
        .await
        .expect("post-delete count failed");

    assert_eq!(
        after, 0,
        "survivor must be deleted when its signal is deleted (ON DELETE CASCADE), got {}",
        after
    );
}

// ─── Critic service (Ollama-gated) tests ─────────────────────────────────────

/// (Ollama-gated) A trivially ungrounded signal ("Humidity is 200%, impossible")
/// with empty evidence must receive `verdict='reject'` from the live critic.
///
/// Targets:
///   - plan Phase 6 § Critic: stress-test prompt, reject implausible signals
///   - contract § survivor: verdict ∈ critic_verdict
///   - ione::services::critic::evaluate_signal(state, signal_id) public API
///
/// Skip via IONE_SKIP_LIVE=1.
///
/// REASON: requires live Ollama (OLLAMA_CRITIC_MODEL) and migration 0005.
#[tokio::test]
#[ignore] // REASON: requires live Ollama (OLLAMA_CRITIC_MODEL); set IONE_SKIP_LIVE=1 to skip
async fn critic_returns_reject_for_trivially_ungrounded_signal() {
    if std::env::var("IONE_SKIP_LIVE").is_ok() {
        eprintln!("IONE_SKIP_LIVE set — skipping live Ollama critic test");
        return;
    }

    let (_base, pool) = spawn_app().await;
    let ws_id = ops_workspace_id(&pool).await;

    // Seed a signal with impossible body and empty evidence
    let signal_id = insert_signal(
        &pool,
        ws_id,
        "Humidity is 200%",
        "Relative humidity cannot exceed 100% — this reading is physically impossible.",
        "flagged",
        json!([]),
    )
    .await;

    // Call the critic service (does not exist yet — expected failure)
    let state = build_app_state(&pool).await;
    let result: anyhow::Result<Option<ione::models::Survivor>> =
        ione::services::critic::evaluate_signal(&state, signal_id).await;

    let survivor = result
        .expect("evaluate_signal returned an error")
        .expect("evaluate_signal must return Some(Survivor) for a seeded signal");

    assert_eq!(
        survivor.verdict.as_str(),
        "reject",
        "critic must return verdict='reject' for a trivially impossible signal, got: {}",
        survivor.verdict.as_str()
    );
}

/// (Ollama-gated) A plausible weather alert signal with evidence referencing a
/// real event id must receive `verdict='survive'` and `confidence > 0.3`.
///
/// Targets:
///   - plan Phase 6 § Critic: grounded signal survives
///   - contract § survivor: confidence REAL (0.0–1.0)
///   - ione::services::critic::evaluate_signal(state, signal_id) public API
///
/// Skip via IONE_SKIP_LIVE=1.
///
/// REASON: requires live Ollama (OLLAMA_CRITIC_MODEL) and migration 0005.
#[tokio::test]
#[ignore] // REASON: requires live Ollama (OLLAMA_CRITIC_MODEL); set IONE_SKIP_LIVE=1 to skip
async fn critic_returns_survive_for_grounded_signal() {
    if std::env::var("IONE_SKIP_LIVE").is_ok() {
        eprintln!("IONE_SKIP_LIVE set — skipping live Ollama critic test");
        return;
    }

    let (_base, pool) = spawn_app().await;
    let ws_id = ops_workspace_id(&pool).await;

    // Seed a connector + stream + event so evidence has a real id
    let connector_id: Uuid = sqlx::query_scalar(
        "INSERT INTO connectors (workspace_id, kind, name, config, status)
         VALUES ($1, 'rust_native', 'NWS', '{\"lat\":46.8,\"lon\":-114.0}'::jsonb, 'active')
         RETURNING id",
    )
    .bind(ws_id)
    .fetch_one(&pool)
    .await
    .expect("insert connector failed");

    let stream_id: Uuid = sqlx::query_scalar(
        "INSERT INTO streams (connector_id, name, schema)
         VALUES ($1, 'observations', '{}'::jsonb)
         RETURNING id",
    )
    .bind(connector_id)
    .fetch_one(&pool)
    .await
    .expect("insert stream failed");

    let event_id: Uuid = sqlx::query_scalar(
        "INSERT INTO stream_events (stream_id, payload, observed_at, ingested_at)
         VALUES ($1, $2, now(), now())
         RETURNING id",
    )
    .bind(stream_id)
    .bind(json!({
        "humidity": 8,
        "temperature": 95,
        "wind_speed_mph": 35,
        "station": "KMSO",
        "alert": "Red Flag Warning — critically low humidity and high winds"
    }))
    .fetch_one(&pool)
    .await
    .expect("insert stream_event failed");

    // Signal with plausible body and a real evidence event id
    let signal_id = insert_signal(
        &pool,
        ws_id,
        "Red Flag Warning: critical fire weather",
        "Station KMSO reports humidity 8%, wind 35 mph — Red Flag Warning issued. \
         Conditions are dangerous for wildfire ignition and spread.",
        "command",
        json!([event_id.to_string()]),
    )
    .await;

    let state = build_app_state(&pool).await;
    let result: anyhow::Result<Option<ione::models::Survivor>> =
        ione::services::critic::evaluate_signal(&state, signal_id).await;

    let survivor = result
        .expect("evaluate_signal returned an error")
        .expect("evaluate_signal must return Some(Survivor) for a seeded signal");

    assert_eq!(
        survivor.verdict.as_str(),
        "survive",
        "critic must return verdict='survive' for a grounded weather alert, got: {}",
        survivor.verdict.as_str()
    );

    assert!(
        survivor.confidence > 0.3,
        "critic confidence must be > 0.3 for a grounded signal, got: {}",
        survivor.confidence
    );
}

// ─── Parse-path test (no Ollama required) ────────────────────────────────────

/// `parse_response` given garbage input must return
/// `(verdict=defer, confidence=0.0, rationale=<non-empty>, steps=[])`.
///
/// This test exercises the brace-count extractor / parse fallback path without
/// requiring a running Ollama instance.
///
/// Targets:
///   - plan Phase 6: "record verdict='defer' on persistent parse fail"
///   - plan Phase 6: reuse Phase 5's brace-count extractor
///   - ione::services::critic::parse_response (public test-only API)
///
/// REASON: requires src/services/critic.rs with a public `parse_response` fn.
#[tokio::test]
#[ignore]
async fn critic_parse_failure_records_defer() {
    // parse_response takes raw model output and returns a structured tuple.
    // On failure it must return defer + 0.0 confidence + non-empty rationale + empty steps.
    let garbage_inputs = [
        "",
        "I am a large language model and I cannot determine the answer.",
        "{ this is not valid json at all !!!",
        "{ verdict: UNKNOWN, confidence: \"high\" }",
        "null",
        "[]",
        "42",
    ];

    for raw in &garbage_inputs {
        let (verdict, confidence, rationale, steps): (String, f32, String, Vec<String>) =
            ione::services::critic::parse_response(raw);

        assert_eq!(
            verdict, "defer",
            "parse_response on garbage input {:?} must return verdict='defer', got: {}",
            raw, verdict
        );

        assert_eq!(
            confidence, 0.0_f32,
            "parse_response on garbage input {:?} must return confidence=0.0, got: {}",
            raw, confidence
        );

        assert!(
            !rationale.is_empty(),
            "parse_response on garbage input {:?} must return non-empty rationale",
            raw
        );

        assert!(
            steps.is_empty(),
            "parse_response on garbage input {:?} must return empty steps vec, got: {:?}",
            raw,
            steps
        );
    }
}

// ─── API endpoint tests ───────────────────────────────────────────────────────

/// GET /api/v1/workspaces/:id/survivors returns items ordered by created_at DESC.
///
/// Targets:
///   - contract § API: GET /api/v1/workspaces/:id/survivors → { items: Survivor[] }
///   - contract § survivor fields: id, signalId, criticModel, verdict, rationale,
///     confidence, chainOfReasoning, createdAt
///   - plan Phase 6: reverse chronological ordering
///
/// REASON: requires migration 0005 and src/routes/survivors.rs.
#[tokio::test]
#[ignore]
async fn list_survivors_returns_items() {
    let (base, pool) = spawn_app().await;
    let ws_id = ops_workspace_id(&pool).await;
    let client = reqwest::Client::new();

    let now = chrono::Utc::now();
    let t1 = now - chrono::Duration::minutes(30);
    let t2 = now - chrono::Duration::minutes(10);

    // Seed two signals
    let sig1_id = insert_signal(&pool, ws_id, "Signal A", "body A", "routine", json!([])).await;
    let sig2_id = insert_signal(&pool, ws_id, "Signal B", "body B", "flagged", json!([])).await;

    // Seed two survivors with distinct timestamps (t1 older, t2 newer)
    insert_survivor(
        &pool,
        sig1_id,
        "phi4-reasoning:14b",
        "survive",
        "grounded A",
        0.8,
        json!([{"step": "check evidence"}]),
        t1,
    )
    .await;

    insert_survivor(
        &pool,
        sig2_id,
        "phi4-reasoning:14b",
        "reject",
        "not grounded B",
        0.2,
        json!([]),
        t2,
    )
    .await;

    let resp = client
        .get(format!("{}/api/v1/workspaces/{}/survivors", base, ws_id))
        .send()
        .await
        .expect("GET /api/v1/workspaces/:id/survivors failed");

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "expected 200 from GET /api/v1/workspaces/:id/survivors, got {} \
         (route not registered — expected failure)",
        resp.status()
    );

    let body: Value = resp.json().await.expect("response body not JSON");

    let items = body["items"]
        .as_array()
        .expect("response must have an \"items\" array");

    assert_eq!(
        items.len(),
        2,
        "expected 2 survivor items, got {}",
        items.len()
    );

    // Verify reverse-chronological order: t2 (newer) must come first
    assert_eq!(
        items[0]["verdict"].as_str().unwrap_or(""),
        "reject",
        "first item must be the most recently created survivor (t2=reject), got: {}",
        items[0]["verdict"]
    );
    assert_eq!(
        items[1]["verdict"].as_str().unwrap_or(""),
        "survive",
        "second item must be the older survivor (t1=survive), got: {}",
        items[1]["verdict"]
    );

    // Verify all required contract field names are present on each item
    for item in items {
        assert!(item["id"].is_string(), "survivor must have 'id' field");
        assert!(
            item["signalId"].is_string(),
            "survivor must have 'signalId' field"
        );
        assert!(
            item["criticModel"].is_string(),
            "survivor must have 'criticModel' field"
        );
        assert!(
            item["verdict"].is_string(),
            "survivor must have 'verdict' field"
        );
        assert!(
            item["rationale"].is_string(),
            "survivor must have 'rationale' field"
        );
        assert!(
            item["confidence"].is_number(),
            "survivor must have 'confidence' field as number"
        );
        assert!(
            item["chainOfReasoning"].is_array(),
            "survivor must have 'chainOfReasoning' array field"
        );
        assert!(
            item["createdAt"].is_string(),
            "survivor must have 'createdAt' field"
        );
    }
}

/// GET /api/v1/workspaces/:id/survivors?verdict=survive returns only survive rows.
///
/// Targets:
///   - contract § API: GET /api/v1/workspaces/:id/survivors?verdict=…
///   - plan Phase 6: filter by verdict
///
/// REASON: requires migration 0005 and src/routes/survivors.rs.
#[tokio::test]
#[ignore]
async fn list_survivors_filters_by_verdict() {
    let (base, pool) = spawn_app().await;
    let ws_id = ops_workspace_id(&pool).await;
    let client = reqwest::Client::new();

    let now = chrono::Utc::now();

    // Seed three signals + three survivors — one of each verdict
    let verdicts = [
        ("survive", "Survive sig"),
        ("reject", "Reject sig"),
        ("defer", "Defer sig"),
    ];
    let severities = ["command", "routine", "flagged"];

    for (i, ((verdict, title), severity)) in verdicts.iter().zip(severities.iter()).enumerate() {
        let sig_id = insert_signal(&pool, ws_id, title, "body", severity, json!([])).await;
        insert_survivor(
            &pool,
            sig_id,
            "phi4-reasoning:14b",
            verdict,
            &format!("rationale for {}", verdict),
            0.5,
            json!([]),
            now - chrono::Duration::seconds(i as i64 * 10),
        )
        .await;
    }

    // Filter ?verdict=survive — must return exactly 1 item
    let resp = client
        .get(format!(
            "{}/api/v1/workspaces/{}/survivors?verdict=survive",
            base, ws_id
        ))
        .send()
        .await
        .expect("GET /survivors?verdict=survive failed");

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "expected 200 from GET /survivors?verdict=survive, got {}",
        resp.status()
    );

    let body: Value = resp.json().await.expect("response body not JSON");
    let items = body["items"].as_array().expect("items must be array");

    assert_eq!(
        items.len(),
        1,
        "?verdict=survive must return exactly 1 item, got {}",
        items.len()
    );
    assert_eq!(
        items[0]["verdict"].as_str().unwrap_or(""),
        "survive",
        "filtered item verdict must be 'survive', got: {}",
        items[0]["verdict"]
    );

    // Filter ?verdict=reject — must return exactly 1 item
    let resp_rej = client
        .get(format!(
            "{}/api/v1/workspaces/{}/survivors?verdict=reject",
            base, ws_id
        ))
        .send()
        .await
        .expect("GET /survivors?verdict=reject failed");

    assert_eq!(resp_rej.status(), StatusCode::OK);
    let body_rej: Value = resp_rej.json().await.expect("response body not JSON");
    let items_rej = body_rej["items"].as_array().expect("items must be array");
    assert_eq!(
        items_rej.len(),
        1,
        "?verdict=reject must return exactly 1 item, got {}",
        items_rej.len()
    );
    assert_eq!(items_rej[0]["verdict"].as_str().unwrap_or(""), "reject");

    // Filter ?verdict=defer — must return exactly 1 item
    let resp_def = client
        .get(format!(
            "{}/api/v1/workspaces/{}/survivors?verdict=defer",
            base, ws_id
        ))
        .send()
        .await
        .expect("GET /survivors?verdict=defer failed");

    assert_eq!(resp_def.status(), StatusCode::OK);
    let body_def: Value = resp_def.json().await.expect("response body not JSON");
    let items_def = body_def["items"].as_array().expect("items must be array");
    assert_eq!(
        items_def.len(),
        1,
        "?verdict=defer must return exactly 1 item, got {}",
        items_def.len()
    );
    assert_eq!(items_def[0]["verdict"].as_str().unwrap_or(""), "defer");
}

// ─── Scheduler integration test ───────────────────────────────────────────────

/// After `critic::evaluate_signal_with_response(pool, signal_id, "garbage")` is
/// called with a non-parseable response, a survivor row must exist in the DB
/// with `verdict='defer'` (defer-on-error is the documented fallback).
///
/// This is the simplest form of test 9: no scheduler wiring required — it
/// directly calls the test-seam that simulates a model response.
///
/// Targets:
///   - plan Phase 6: "record verdict='defer' on persistent parse fail"
///   - plan Phase 6: scheduler calls critic immediately after signal insert
///   - ione::services::critic::evaluate_signal_with_response (test hook)
///
/// REASON: requires migration 0005 and src/services/critic.rs.
#[tokio::test]
#[ignore]
async fn scheduler_runs_critic_after_generator() {
    let (_base, pool) = spawn_app().await;
    let ws_id = ops_workspace_id(&pool).await;

    let signal_id = insert_signal(
        &pool,
        ws_id,
        "Test signal",
        "test body",
        "routine",
        json!([]),
    )
    .await;

    // Call the test hook that simulates an unusable model response.
    // This must create a survivor with verdict='defer'.
    let _: ione::models::Survivor = ione::services::critic::evaluate_signal_with_response(
        &pool,
        signal_id,
        "this is not json at all — simulated broken model output",
    )
    .await
    .expect("evaluate_signal_with_response must not return Err");

    let row: Option<(String, f32)> =
        sqlx::query_as("SELECT verdict::TEXT, confidence FROM survivors WHERE signal_id = $1")
            .bind(signal_id)
            .fetch_optional(&pool)
            .await
            .expect("survivors query failed");

    let (verdict, confidence) = row.expect(
        "evaluate_signal_with_response must insert a survivor row \
         even when the model response is unparseable (expected failure — critic not implemented)",
    );

    assert_eq!(
        verdict, "defer",
        "survivor verdict must be 'defer' when model response is garbage, got: {}",
        verdict
    );

    assert_eq!(
        confidence, 0.0_f32,
        "survivor confidence must be 0.0 on parse failure, got: {}",
        confidence
    );
}

// ─── Field round-trip tests ───────────────────────────────────────────────────

/// A survivor inserted with `confidence=0.75` must round-trip as 0.75 (±epsilon).
///
/// Targets:
///   - contract § survivor.confidence: REAL (0.0–1.0)
///   - migration 0005: confidence REAL NOT NULL
///
/// REASON: requires migration 0005.
#[tokio::test]
#[ignore]
async fn survivor_confidence_in_range() {
    let (_base, pool) = spawn_app().await;
    let ws_id = ops_workspace_id(&pool).await;

    let signal_id = insert_signal(
        &pool,
        ws_id,
        "Confidence test",
        "body",
        "routine",
        json!([]),
    )
    .await;

    let now = chrono::Utc::now();
    let survivor_id = insert_survivor(
        &pool,
        signal_id,
        "phi4-reasoning:14b",
        "survive",
        "well-supported",
        0.75_f32,
        json!([]),
        now,
    )
    .await;

    let confidence: f32 = sqlx::query_scalar("SELECT confidence FROM survivors WHERE id = $1")
        .bind(survivor_id)
        .fetch_one(&pool)
        .await
        .expect("confidence query failed");

    assert!(
        (confidence - 0.75_f32).abs() < 1e-5,
        "confidence must round-trip as 0.75, got: {}",
        confidence
    );

    // Also assert it is within [0.0, 1.0]
    assert!(
        confidence >= 0.0 && confidence <= 1.0,
        "confidence must be in [0.0, 1.0], got: {}",
        confidence
    );
}

/// A survivor inserted with a `chain_of_reasoning` array of two step objects
/// must round-trip as a two-element JSONB array.
///
/// Targets:
///   - contract § survivor.chain_of_reasoning: JSONB (default '[]')
///   - plan Phase 6 § Critic: steps[] populates chain_of_reasoning
///
/// REASON: requires migration 0005.
#[tokio::test]
#[ignore]
async fn chain_of_reasoning_is_array_of_steps() {
    let (_base, pool) = spawn_app().await;
    let ws_id = ops_workspace_id(&pool).await;

    let signal_id = insert_signal(
        &pool,
        ws_id,
        "Chain test signal",
        "body",
        "routine",
        json!([]),
    )
    .await;

    let now = chrono::Utc::now();
    let chain = json!([
        {"step": "check-evidence"},
        {"step": "verify-signal-strength"}
    ]);

    let survivor_id = insert_survivor(
        &pool,
        signal_id,
        "phi4-reasoning:14b",
        "survive",
        "two-step reasoning",
        0.6,
        chain.clone(),
        now,
    )
    .await;

    let stored: serde_json::Value =
        sqlx::query_scalar("SELECT chain_of_reasoning FROM survivors WHERE id = $1")
            .bind(survivor_id)
            .fetch_one(&pool)
            .await
            .expect("chain_of_reasoning query failed");

    let arr = stored
        .as_array()
        .expect("chain_of_reasoning must be a JSON array");

    assert_eq!(
        arr.len(),
        2,
        "chain_of_reasoning must have exactly 2 elements, got {}",
        arr.len()
    );

    assert_eq!(
        arr[0].get("step").and_then(|v| v.as_str()),
        Some("check-evidence"),
        "first step must be 'check-evidence', got: {}",
        arr[0]
    );

    assert_eq!(
        arr[1].get("step").and_then(|v| v.as_str()),
        Some("verify-signal-strength"),
        "second step must be 'verify-signal-strength', got: {}",
        arr[1]
    );
}

// ─── Cascade from workspace tests ─────────────────────────────────────────────

/// Deleting a workspace must cascade through signals to survivors.
///
/// Targets:
///   - contract § Relationships: survivor belongs to signal (FK CASCADE);
///     signal belongs to workspace (FK CASCADE)
///   - plan Phase 6 migration 0005: ON DELETE CASCADE propagates through both FKs
///
/// REASON: requires migration 0005.
#[tokio::test]
#[ignore]
async fn survivors_cascade_on_workspace_delete() {
    let (base, pool) = spawn_app().await;
    let client = reqwest::Client::new();

    // Create a fresh workspace so we can delete it
    let ws_body: Value = client
        .post(format!("{}/api/v1/workspaces", base))
        .json(&json!({
            "name": "Cascade WS",
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

    let signal_id =
        insert_signal(&pool, ws_id, "Cascade signal", "body", "routine", json!([])).await;

    let now = chrono::Utc::now();
    let survivor_id = insert_survivor(
        &pool,
        signal_id,
        "phi4-reasoning:14b",
        "survive",
        "grounded",
        0.7,
        json!([]),
        now,
    )
    .await;

    // Confirm survivor exists
    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM survivors WHERE id = $1")
        .bind(survivor_id)
        .fetch_one(&pool)
        .await
        .expect("pre-delete count failed");
    assert_eq!(before, 1, "survivor must exist before workspace delete");

    // Delete workspace — cascade must reach signal, then survivor
    sqlx::query("DELETE FROM workspaces WHERE id = $1")
        .bind(ws_id)
        .execute(&pool)
        .await
        .expect("workspace delete failed");

    let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM survivors WHERE id = $1")
        .bind(survivor_id)
        .fetch_one(&pool)
        .await
        .expect("post-delete count failed");

    assert_eq!(
        after, 0,
        "survivor must be deleted when its workspace is deleted \
         (cascade: workspace → signal → survivor), got {}",
        after
    );
}

// ─── Helpers (only called inside Ollama-gated tests) ─────────────────────────

/// Build an AppState from the pool by calling through the same bootstrap path
/// the app uses.  Only needed in live-Ollama tests.
async fn build_app_state(pool: &PgPool) -> ione::state::AppState {
    let (_, state) = ione::app_with_state(pool.clone()).await;
    state
}
