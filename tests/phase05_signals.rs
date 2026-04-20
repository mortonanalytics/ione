/// Phase 5 contract tests — rules engine, generator LLM pass, signals table.
///
/// These tests are written against:
///   - Contract: md/design/ione-v1-contract.md  (entity `signal`, enums `signal_source`,
///     `severity`)
///   - Plan:     md/plans/ione-v1-plan.md        (Phase 5 scope)
///
/// ALL tests FAIL today because Phase 5 (migration 0004, services/rules.rs,
/// services/generator.rs, routes/signals.rs) does not yet exist.
///
/// ──────────────────────────────────────────────────────────────────────────
/// Prerequisites:
///   docker compose up -d postgres
///   DATABASE_URL=postgres://ione:ione@localhost:5433/ione
///
/// Run (serial, ignored):
///   DATABASE_URL=postgres://ione:ione@localhost:5433/ione \
///     cargo test --test phase05_signals -- --ignored --test-threads=1
///
/// Skip Ollama-gated generator tests:
///   IONE_SKIP_LIVE=1 DATABASE_URL=... cargo test --test phase05_signals \
///     -- --ignored --test-threads=1
///
/// ──────────────────────────────────────────────────────────────────────────
/// Contract targets referenced per test (md/design/ione-v1-contract.md):
///
///   § Enums
///     signal_source : rule | connector_event | generator
///     severity      : routine | flagged | command
///
///   § signal fields (DB snake_case → JSON camelCase)
///     id             → id            UUID
///     workspace_id   → workspaceId   UUID
///     source         → source        signal_source
///     title          → title         TEXT
///     body           → body          TEXT
///     evidence       → evidence      JSONB (array)
///     severity       → severity      severity (default routine)
///     generator_model→ generatorModel TEXT NULL
///     created_at     → createdAt     TIMESTAMPTZ
///
///   § API operations
///     GET /api/v1/workspaces/:id/signals → { items: Signal[] }
///     optional query params: ?source=rule|generator|connector_event
///                            ?severity=routine|flagged|command
///                            ?limit (default 100, max 500)
///
///   § Relationships
///     signal belongs to workspace (FK CASCADE)
///     signal.evidence references stream_event ids
///
///   § Migration 0004 (plan Phase 5)
///     enums: signal_source, severity
///     table: signals(id, workspace_id FK CASCADE, source, title, body,
///                    evidence JSONB default '[]', severity default 'routine',
///                    generator_model TEXT NULL, created_at)
///     index: signals_workspace_created ON signals(workspace_id, created_at DESC)
///
///   § Rules engine (plan Phase 5)
///     workspace.metadata.rules = [{stream, when:"<evalexpr>", severity, title}]
///     match → insert signal with source='rule'
///     evaluate via ione::services::rules::evaluate_workspace(pool, workspace_id)
///
///   § Generator (plan Phase 5)
///     ione::services::generator::run_for_workspace(...)
///     source='generator', generator_model non-null, body non-empty
///     severity ∈ {routine, flagged, command}
///
///   § Scheduler (plan Phase 5)
///     idempotent per tick: no duplicate rule-signals for already-seen events
///     dedup key: (workspace_id, source, title, evidence event ids)
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

/// Connect, run all migrations, wipe in FK-safe order, boot on a random port.
/// Returns `(base_url, pool)`.
///
/// The TRUNCATE must include `signals` (the Phase 5 table).  If migration 0004
/// has not been applied yet the call will fail — the expected failure mode for a
/// contract-red test.
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
        .expect("migration failed — migration 0004 may not exist yet (expected failure)");

    // Truncate in reverse-FK order.  `signals` must come before `workspaces`.
    sqlx::query(
        "TRUNCATE signals, stream_events, streams, connectors,
                  memberships, roles, messages, conversations,
                  workspaces, users, organizations
         RESTART IDENTITY CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate failed — signals table may not exist yet (expected for contract-red)");

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

/// Insert a stream_event directly into the DB under `stream_id` with the given
/// JSONB payload.  Returns the new row id.
async fn insert_stream_event(pool: &PgPool, stream_id: Uuid, payload: serde_json::Value) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO stream_events (stream_id, payload, observed_at, ingested_at)
         VALUES ($1, $2, now(), now())
         RETURNING id",
    )
    .bind(stream_id)
    .bind(payload)
    .fetch_one(pool)
    .await
    .expect("insert stream_event failed")
}

/// Create a connector + auto-register the 'observations' stream in `workspace_id`.
/// Returns `(connector_id, stream_id)`.
async fn seed_connector_and_stream(pool: &PgPool, workspace_id: Uuid) -> (Uuid, Uuid) {
    let connector_id: Uuid = sqlx::query_scalar(
        "INSERT INTO connectors (workspace_id, kind, name, config, status)
         VALUES ($1, 'rust_native', 'NWS Test', '{\"lat\":46.8,\"lon\":-114.0}'::jsonb, 'active')
         RETURNING id",
    )
    .bind(workspace_id)
    .fetch_one(pool)
    .await
    .expect("insert connector failed");

    let stream_id: Uuid = sqlx::query_scalar(
        "INSERT INTO streams (connector_id, name, schema)
         VALUES ($1, 'observations', '{}'::jsonb)
         RETURNING id",
    )
    .bind(connector_id)
    .fetch_one(pool)
    .await
    .expect("insert stream failed");

    (connector_id, stream_id)
}

/// Insert a signal directly into the DB.  Returns the new row id.
async fn insert_signal(
    pool: &PgPool,
    workspace_id: Uuid,
    source: &str,
    title: &str,
    body: &str,
    severity: &str,
    evidence: serde_json::Value,
    generator_model: Option<&str>,
    created_at: chrono::DateTime<chrono::Utc>,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO signals
           (workspace_id, source, title, body, severity, evidence, generator_model, created_at)
         VALUES ($1, $2::signal_source, $3, $4, $5::severity, $6, $7, $8)
         RETURNING id",
    )
    .bind(workspace_id)
    .bind(source)
    .bind(title)
    .bind(body)
    .bind(severity)
    .bind(evidence)
    .bind(generator_model)
    .bind(created_at)
    .fetch_one(pool)
    .await
    .expect("insert signal failed")
}

// ─── Enum shape tests ─────────────────────────────────────────────────────────

/// Contract § Enums — `signal_source` must have exactly three variants in order.
///
/// Target: ione-v1-contract.md § Enums → signal_source : rule | connector_event | generator
///
/// REASON: requires DATABASE_URL and migration 0004 (which does not exist yet).
#[tokio::test]
#[ignore]
async fn signal_source_enum_variants() {
    let (_base, pool) = spawn_app().await;

    let variants: Vec<String> =
        sqlx::query_scalar("SELECT unnest(enum_range(NULL::signal_source))::TEXT")
            .fetch_all(&pool)
            .await
            .expect(
                "query failed — signal_source enum not found \
                 (migration 0004 missing; expected failure)",
            );

    assert_eq!(
        variants,
        vec!["rule", "connector_event", "generator"],
        "signal_source enum must have exactly variants [rule, connector_event, generator] \
         in declaration order, got {:?}",
        variants
    );
}

/// Contract § Enums — `severity` must have exactly three variants in order.
///
/// Target: ione-v1-contract.md § Enums → severity : routine | flagged | command
///
/// REASON: requires DATABASE_URL and migration 0004.
#[tokio::test]
#[ignore]
async fn severity_enum_variants() {
    let (_base, pool) = spawn_app().await;

    let variants: Vec<String> =
        sqlx::query_scalar("SELECT unnest(enum_range(NULL::severity))::TEXT")
            .fetch_all(&pool)
            .await
            .expect(
                "query failed — severity enum not found \
                 (migration 0004 missing; expected failure)",
            );

    assert_eq!(
        variants,
        vec!["routine", "flagged", "command"],
        "severity enum must have exactly variants [routine, flagged, command] \
         in declaration order, got {:?}",
        variants
    );
}

// ─── Rules engine tests ───────────────────────────────────────────────────────

/// A rule that matches the event payload causes `evaluate_workspace` to insert a
/// signal row with the expected fields.
///
/// Targets:
///   - plan Phase 5 § Rules: workspace.metadata.rules `[{stream, when, severity, title}]`
///   - contract § signal: source='rule', severity='flagged', evidence contains event id
///   - ione::services::rules::evaluate_workspace(pool, workspace_id) (public API)
///
/// REASON: requires migration 0004 and src/services/rules.rs.
#[tokio::test]
#[ignore]
async fn rule_match_creates_signal() {
    let (_base, pool) = spawn_app().await;
    let ws_id = ops_workspace_id(&pool).await;

    // Seed rule on workspace metadata
    sqlx::query(
        "UPDATE workspaces
         SET metadata = jsonb_set(
               metadata,
               '{rules}',
               '[{\"stream\":\"observations\",\"when\":\"payload.humidity < 15\",
                  \"severity\":\"flagged\",\"title\":\"Humidity critically low\"}]'::jsonb
             )
         WHERE id = $1",
    )
    .bind(ws_id)
    .execute(&pool)
    .await
    .expect("UPDATE workspace metadata failed");

    // Seed connector + stream
    let (_connector_id, stream_id) = seed_connector_and_stream(&pool, ws_id).await;

    // Insert an event whose humidity triggers the rule
    let event_id = insert_stream_event(
        &pool,
        stream_id,
        json!({ "humidity": 10, "temperature": 72 }),
    )
    .await;

    // Call the rules service (does not exist yet — expected failure)
    ione::services::rules::evaluate_workspace(&pool, ws_id)
        .await
        .expect("evaluate_workspace returned an error");

    // Assert a signals row was created
    let row: Option<(String, String, String, serde_json::Value)> = sqlx::query_as(
        "SELECT source::TEXT, severity::TEXT, title, evidence
         FROM signals
         WHERE workspace_id = $1 AND source = 'rule'
         LIMIT 1",
    )
    .bind(ws_id)
    .fetch_optional(&pool)
    .await
    .expect("signals query failed");

    let (source, severity, title, evidence) =
        row.expect("no signal row created — rule engine did not fire (expected failure)");

    assert_eq!(
        source, "rule",
        "signal.source must be 'rule', got: {}",
        source
    );
    assert_eq!(
        severity, "flagged",
        "signal.severity must be 'flagged' (from rule definition), got: {}",
        severity
    );
    assert_eq!(
        title, "Humidity critically low",
        "signal.title must match rule title, got: {}",
        title
    );

    // evidence must be a JSON array containing the triggering event id
    let evidence_arr = evidence
        .as_array()
        .expect("signal.evidence must be a JSON array");

    let event_id_str = event_id.to_string();
    let found = evidence_arr.iter().any(|v| {
        // accept either a plain UUID string or an object with an "id" key
        v.as_str() == Some(&event_id_str)
            || v.get("id").and_then(|x| x.as_str()) == Some(&event_id_str)
    });

    assert!(
        found,
        "signal.evidence must contain the triggering stream_event id ({}), got: {}",
        event_id_str, evidence
    );
}

/// A rule whose condition evaluates to false produces no signal.
///
/// Targets:
///   - plan Phase 5 § Rules: only matching rules produce signals
///   - contract § signal: no row when rule does not match
///
/// REASON: requires migration 0004 and src/services/rules.rs.
#[tokio::test]
#[ignore]
async fn rule_no_match_creates_no_signal() {
    let (_base, pool) = spawn_app().await;
    let ws_id = ops_workspace_id(&pool).await;

    sqlx::query(
        "UPDATE workspaces
         SET metadata = jsonb_set(
               metadata,
               '{rules}',
               '[{\"stream\":\"observations\",\"when\":\"payload.humidity < 15\",
                  \"severity\":\"flagged\",\"title\":\"Humidity critically low\"}]'::jsonb
             )
         WHERE id = $1",
    )
    .bind(ws_id)
    .execute(&pool)
    .await
    .expect("UPDATE workspace metadata failed");

    let (_connector_id, stream_id) = seed_connector_and_stream(&pool, ws_id).await;

    // humidity = 50 — does NOT satisfy `< 15`
    insert_stream_event(
        &pool,
        stream_id,
        json!({ "humidity": 50, "temperature": 72 }),
    )
    .await;

    ione::services::rules::evaluate_workspace(&pool, ws_id)
        .await
        .expect("evaluate_workspace returned an error");

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM signals WHERE workspace_id = $1")
        .bind(ws_id)
        .fetch_one(&pool)
        .await
        .expect("count query failed");

    assert_eq!(
        count, 0,
        "no signal must be created when the rule condition does not match, got {}",
        count
    );
}

/// A workspace with a syntactically invalid rule expression must not panic; the
/// scheduler tick returns without error, and the workspace + connector remain in
/// a healthy state.
///
/// Targets:
///   - plan Phase 5 § Rules: evalexpr error handling
///   - contract § connector.status: must remain 'active'
///
/// REASON: requires migration 0004 and src/services/rules.rs.
#[tokio::test]
#[ignore]
async fn invalid_rule_expression_does_not_crash() {
    let (_base, pool) = spawn_app().await;
    let ws_id = ops_workspace_id(&pool).await;

    // Malformed expression: `$$$` is not valid evalexpr syntax
    sqlx::query(
        "UPDATE workspaces
         SET metadata = jsonb_set(
               metadata,
               '{rules}',
               '[{\"stream\":\"observations\",\"when\":\"payload.humidity $$$\",
                  \"severity\":\"flagged\",\"title\":\"Broken rule\"}]'::jsonb
             )
         WHERE id = $1",
    )
    .bind(ws_id)
    .execute(&pool)
    .await
    .expect("UPDATE workspace metadata failed");

    let (connector_id, stream_id) = seed_connector_and_stream(&pool, ws_id).await;

    insert_stream_event(&pool, stream_id, json!({ "humidity": 10 })).await;

    // Must not panic
    let result = ione::services::rules::evaluate_workspace(&pool, ws_id).await;

    // The service must return Ok (absorbing the parse error) or an Err that
    // does not propagate as a panic.  Both are acceptable; a panic fails the test.
    // Logging or surfacing the error is acceptable.
    let _ = result;

    // No signal must be produced from a broken rule
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM signals WHERE workspace_id = $1")
        .bind(ws_id)
        .fetch_one(&pool)
        .await
        .expect("count query failed");

    assert_eq!(
        count, 0,
        "a broken rule expression must produce no signal, got {}",
        count
    );

    // Connector status must remain 'active' — broken rules must not poison
    // connector health
    let status: String = sqlx::query_scalar("SELECT status::TEXT FROM connectors WHERE id = $1")
        .bind(connector_id)
        .fetch_one(&pool)
        .await
        .expect("connector status query failed");

    assert_eq!(
        status, "active",
        "connector status must remain 'active' after a broken rule expression, got: {}",
        status
    );
}

// ─── Generator tests ──────────────────────────────────────────────────────────

/// The generator service produces at least one signal with source='generator'
/// when called with a workspace that has ≥3 stream events.
///
/// Targets:
///   - plan Phase 5 § Generator: structured JSON output, source='generator'
///   - contract § signal: generator_model non-null, body non-empty
///   - ione::services::generator::run_for_workspace(...)
///
/// NOTE: requires a running Ollama instance with OLLAMA_GENERATOR_MODEL available.
///       Set IONE_SKIP_LIVE=1 to skip.
///
/// REASON: requires migration 0004 and src/services/generator.rs.
#[tokio::test]
#[ignore] // REASON: requires live Ollama (OLLAMA_GENERATOR_MODEL); set IONE_SKIP_LIVE=1 to skip
async fn generator_produces_structured_signal() {
    if std::env::var("IONE_SKIP_LIVE").is_ok() {
        eprintln!("IONE_SKIP_LIVE set — skipping live Ollama generator test");
        return;
    }

    let (_base, pool) = spawn_app().await;
    let ws_id = ops_workspace_id(&pool).await;

    let (_connector_id, stream_id) = seed_connector_and_stream(&pool, ws_id).await;

    // Seed ≥3 events so the generator has material to summarize
    for i in 0..3_u32 {
        insert_stream_event(
            &pool,
            stream_id,
            json!({ "humidity": 10 + i, "temperature": 70 + i, "note": format!("event {}", i) }),
        )
        .await;
    }

    // Call the generator service (does not exist yet — expected failure)
    ione::services::generator::run_for_workspace(&pool, ws_id)
        .await
        .expect("generator::run_for_workspace returned an error");

    let rows: Vec<(String, Option<String>, String, String)> = sqlx::query_as(
        "SELECT source::TEXT, generator_model, body, severity::TEXT
         FROM signals
         WHERE workspace_id = $1 AND source = 'generator'",
    )
    .bind(ws_id)
    .fetch_all(&pool)
    .await
    .expect("signals query failed");

    assert!(
        !rows.is_empty(),
        "generator must produce at least one signal row (source='generator'), \
         got 0 rows (expected failure — generator not implemented)"
    );

    let valid_severities = ["routine", "flagged", "command"];

    for (source, generator_model, body, severity) in &rows {
        assert_eq!(
            source, "generator",
            "signal.source must be 'generator', got: {}",
            source
        );

        assert!(
            generator_model.is_some(),
            "signal.generatorModel must be non-null for generator signals"
        );
        assert!(
            !generator_model.as_deref().unwrap_or("").is_empty(),
            "signal.generatorModel must be non-empty string, got empty"
        );

        assert!(
            !body.is_empty(),
            "signal.body must be non-empty for generator signals"
        );

        assert!(
            valid_severities.contains(&severity.as_str()),
            "signal.severity must be one of [routine, flagged, command], got: {}",
            severity
        );
    }
}

// ─── API endpoint tests ───────────────────────────────────────────────────────

/// GET /api/v1/workspaces/:id/signals returns items in reverse-chronological order.
///
/// Targets:
///   - contract § API: GET /api/v1/workspaces/:id/signals → { items: Signal[] }
///   - contract § signal fields: id, workspaceId, source, title, body, evidence,
///     severity, generatorModel, createdAt
///   - plan Phase 5: reverse chronological, `signals_workspace_created` index
///
/// REASON: requires migration 0004 and src/routes/signals.rs.
#[tokio::test]
#[ignore]
async fn list_signals_returns_items_reverse_chronological() {
    let (base, pool) = spawn_app().await;
    let ws_id = ops_workspace_id(&pool).await;
    let client = reqwest::Client::new();

    let now = chrono::Utc::now();
    let t1 = now - chrono::Duration::minutes(30);
    let t2 = now - chrono::Duration::minutes(20);
    let t3 = now - chrono::Duration::minutes(10);

    // Insert 3 signals with explicit, distinct timestamps
    insert_signal(
        &pool,
        ws_id,
        "rule",
        "Oldest signal",
        "body A",
        "routine",
        json!([]),
        None,
        t1,
    )
    .await;
    insert_signal(
        &pool,
        ws_id,
        "rule",
        "Middle signal",
        "body B",
        "flagged",
        json!([]),
        None,
        t2,
    )
    .await;
    insert_signal(
        &pool,
        ws_id,
        "rule",
        "Newest signal",
        "body C",
        "command",
        json!([]),
        None,
        t3,
    )
    .await;

    let resp = client
        .get(format!("{}/api/v1/workspaces/{}/signals", base, ws_id))
        .send()
        .await
        .expect("GET /api/v1/workspaces/:id/signals failed");

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "expected 200 from GET /api/v1/workspaces/:id/signals, got {} \
         (route not registered — expected failure)",
        resp.status()
    );

    let body: Value = resp.json().await.expect("response body not JSON");

    let items = body["items"]
        .as_array()
        .expect("response must have an \"items\" array");

    assert_eq!(
        items.len(),
        3,
        "expected 3 signal items, got {}",
        items.len()
    );

    // Verify reverse-chronological order: items[0] is newest (t3)
    assert_eq!(
        items[0]["title"].as_str().unwrap_or(""),
        "Newest signal",
        "first item must be the newest signal (reverse chronological), got: {}",
        items[0]["title"]
    );
    assert_eq!(
        items[2]["title"].as_str().unwrap_or(""),
        "Oldest signal",
        "last item must be the oldest signal (reverse chronological), got: {}",
        items[2]["title"]
    );

    // Verify all required contract field names are present on each item
    for item in items {
        assert!(item["id"].is_string(), "signal must have 'id' field");
        assert!(
            item["workspaceId"].is_string(),
            "signal must have 'workspaceId' field"
        );
        assert!(
            item["source"].is_string(),
            "signal must have 'source' field"
        );
        assert!(item["title"].is_string(), "signal must have 'title' field");
        assert!(item["body"].is_string(), "signal must have 'body' field");
        assert!(
            item["evidence"].is_array(),
            "signal must have 'evidence' array field"
        );
        assert!(
            item["severity"].is_string(),
            "signal must have 'severity' field"
        );
        assert!(
            item["createdAt"].is_string(),
            "signal must have 'createdAt' field"
        );
        // generatorModel may be null but the key must be present
        assert!(
            item.get("generatorModel").is_some(),
            "signal must have 'generatorModel' key (may be null), item: {}",
            item
        );

        // workspaceId must match the requested workspace
        assert_eq!(
            item["workspaceId"].as_str().unwrap_or(""),
            ws_id.to_string().as_str(),
            "signal.workspaceId must equal the workspace path param, got: {}",
            item["workspaceId"]
        );
    }
}

/// GET /api/v1/workspaces/:id/signals?source=rule returns only rule-sourced signals.
///
/// Targets:
///   - plan Phase 5: optional ?source=rule|generator|connector_event filter
///   - contract § signal.source
///
/// REASON: requires migration 0004 and src/routes/signals.rs.
#[tokio::test]
#[ignore]
async fn list_signals_filters_by_source() {
    let (base, pool) = spawn_app().await;
    let ws_id = ops_workspace_id(&pool).await;
    let client = reqwest::Client::new();

    let now = chrono::Utc::now();

    // Insert one signal of each source
    insert_signal(
        &pool,
        ws_id,
        "rule",
        "Rule signal",
        "body",
        "routine",
        json!([]),
        None,
        now,
    )
    .await;
    insert_signal(
        &pool,
        ws_id,
        "generator",
        "Gen signal",
        "body",
        "routine",
        json!([]),
        Some("qwen3:14b"),
        now - chrono::Duration::seconds(1),
    )
    .await;
    insert_signal(
        &pool,
        ws_id,
        "connector_event",
        "Event signal",
        "body",
        "routine",
        json!([]),
        None,
        now - chrono::Duration::seconds(2),
    )
    .await;

    // Filter by source=rule
    let resp = client
        .get(format!(
            "{}/api/v1/workspaces/{}/signals?source=rule",
            base, ws_id
        ))
        .send()
        .await
        .expect("GET /signals?source=rule failed");

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "expected 200 from GET /signals?source=rule, got {}",
        resp.status()
    );

    let body: Value = resp.json().await.expect("response body not JSON");
    let items = body["items"].as_array().expect("items must be array");

    assert_eq!(
        items.len(),
        1,
        "?source=rule must return exactly 1 item, got {}",
        items.len()
    );
    assert_eq!(
        items[0]["source"].as_str().unwrap_or(""),
        "rule",
        "filtered item source must be 'rule', got: {}",
        items[0]["source"]
    );

    // Filter by source=generator
    let resp_gen = client
        .get(format!(
            "{}/api/v1/workspaces/{}/signals?source=generator",
            base, ws_id
        ))
        .send()
        .await
        .expect("GET /signals?source=generator failed");

    assert_eq!(resp_gen.status(), StatusCode::OK);

    let body_gen: Value = resp_gen.json().await.expect("response body not JSON");
    let items_gen = body_gen["items"].as_array().expect("items must be array");

    assert_eq!(
        items_gen.len(),
        1,
        "?source=generator must return exactly 1 item, got {}",
        items_gen.len()
    );
    assert_eq!(
        items_gen[0]["source"].as_str().unwrap_or(""),
        "generator",
        "filtered item source must be 'generator', got: {}",
        items_gen[0]["source"]
    );
}

/// GET /api/v1/workspaces/:id/signals?severity=flagged returns only flagged signals.
///
/// Targets:
///   - plan Phase 5: optional ?severity=routine|flagged|command filter
///   - contract § signal.severity
///
/// REASON: requires migration 0004 and src/routes/signals.rs.
#[tokio::test]
#[ignore]
async fn list_signals_filters_by_severity() {
    let (base, pool) = spawn_app().await;
    let ws_id = ops_workspace_id(&pool).await;
    let client = reqwest::Client::new();

    let now = chrono::Utc::now();

    insert_signal(
        &pool,
        ws_id,
        "rule",
        "Routine sig",
        "body",
        "routine",
        json!([]),
        None,
        now,
    )
    .await;
    insert_signal(
        &pool,
        ws_id,
        "rule",
        "Flagged sig",
        "body",
        "flagged",
        json!([]),
        None,
        now - chrono::Duration::seconds(1),
    )
    .await;
    insert_signal(
        &pool,
        ws_id,
        "rule",
        "Command sig",
        "body",
        "command",
        json!([]),
        None,
        now - chrono::Duration::seconds(2),
    )
    .await;

    let resp = client
        .get(format!(
            "{}/api/v1/workspaces/{}/signals?severity=flagged",
            base, ws_id
        ))
        .send()
        .await
        .expect("GET /signals?severity=flagged failed");

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "expected 200 from GET /signals?severity=flagged, got {}",
        resp.status()
    );

    let body: Value = resp.json().await.expect("response body not JSON");
    let items = body["items"].as_array().expect("items must be array");

    assert_eq!(
        items.len(),
        1,
        "?severity=flagged must return exactly 1 item, got {}",
        items.len()
    );
    assert_eq!(
        items[0]["severity"].as_str().unwrap_or(""),
        "flagged",
        "filtered item severity must be 'flagged', got: {}",
        items[0]["severity"]
    );
}

// ─── Cascade tests ────────────────────────────────────────────────────────────

/// Deleting a workspace row must cascade to all its signals.
///
/// Targets:
///   - contract § Relationships: signal belongs to workspace (FK CASCADE)
///   - plan Phase 5 migration 0004: workspace_id FK … ON DELETE CASCADE
///
/// REASON: requires migration 0004.
#[tokio::test]
#[ignore]
async fn signals_cascade_on_workspace_delete() {
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

    let now = chrono::Utc::now();
    let signal_id = insert_signal(
        &pool,
        ws_id,
        "rule",
        "Cascaded signal",
        "body",
        "routine",
        json!([]),
        None,
        now,
    )
    .await;

    // Confirm signal exists
    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM signals WHERE workspace_id = $1")
        .bind(ws_id)
        .fetch_one(&pool)
        .await
        .expect("count before delete failed");
    assert_eq!(before, 1, "must have 1 signal before workspace delete");

    // Delete workspace row directly
    sqlx::query("DELETE FROM workspaces WHERE id = $1")
        .bind(ws_id)
        .execute(&pool)
        .await
        .expect("workspace delete failed");

    // Signal must be gone
    let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM signals WHERE id = $1")
        .bind(signal_id)
        .fetch_one(&pool)
        .await
        .expect("count after delete failed");

    assert_eq!(
        after, 0,
        "signals must be deleted when their workspace is deleted (ON DELETE CASCADE), got {}",
        after
    );
}

// ─── Idempotency / dedup tests ────────────────────────────────────────────────

/// Running `evaluate_workspace` twice on the same event must not insert duplicate
/// rule-signals.  The dedup key is (workspace_id, source, title, evidence event
/// ids): the second call must leave the signal count unchanged.
///
/// Targets:
///   - plan Phase 5 § Scheduler: idempotent per tick
///   - plan Phase 5 § Rules: NOT EXISTS check before insert
///
/// REASON: requires migration 0004 and src/services/rules.rs.
#[tokio::test]
#[ignore]
async fn scheduler_tick_is_idempotent_for_already_ingested_events() {
    let (_base, pool) = spawn_app().await;
    let ws_id = ops_workspace_id(&pool).await;

    sqlx::query(
        "UPDATE workspaces
         SET metadata = jsonb_set(
               metadata,
               '{rules}',
               '[{\"stream\":\"observations\",\"when\":\"payload.humidity < 15\",
                  \"severity\":\"flagged\",\"title\":\"Humidity critically low\"}]'::jsonb
             )
         WHERE id = $1",
    )
    .bind(ws_id)
    .execute(&pool)
    .await
    .expect("UPDATE workspace metadata failed");

    let (_connector_id, stream_id) = seed_connector_and_stream(&pool, ws_id).await;

    insert_stream_event(&pool, stream_id, json!({ "humidity": 10 })).await;

    // First tick
    ione::services::rules::evaluate_workspace(&pool, ws_id)
        .await
        .expect("first evaluate_workspace call failed");

    let count_after_first: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM signals WHERE workspace_id = $1")
            .bind(ws_id)
            .fetch_one(&pool)
            .await
            .expect("count after first tick failed");

    assert_eq!(
        count_after_first, 1,
        "first evaluate_workspace call must produce exactly 1 signal, got {}",
        count_after_first
    );

    // Second tick — same event, same rule
    ione::services::rules::evaluate_workspace(&pool, ws_id)
        .await
        .expect("second evaluate_workspace call failed");

    let count_after_second: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM signals WHERE workspace_id = $1")
            .bind(ws_id)
            .fetch_one(&pool)
            .await
            .expect("count after second tick failed");

    assert_eq!(
        count_after_second, count_after_first,
        "second scheduler tick must not create duplicate signals for already-ingested events; \
         count before={} count after={} \
         (dedup check missing in rules engine — expected failure)",
        count_after_first, count_after_second
    );
}

// ─── Evidence content tests ───────────────────────────────────────────────────

/// After a rule match, `signal.evidence` is a JSONB array containing the id of
/// the stream_event that triggered the rule.
///
/// Targets:
///   - contract § signal.evidence: JSONB (array of stream_event IDs and excerpts)
///   - plan Phase 5 § Rules: evidence includes the event id used to trigger
///
/// REASON: requires migration 0004 and src/services/rules.rs.
#[tokio::test]
#[ignore]
async fn signals_include_evidence_event_ids() {
    let (_base, pool) = spawn_app().await;
    let ws_id = ops_workspace_id(&pool).await;

    sqlx::query(
        "UPDATE workspaces
         SET metadata = jsonb_set(
               metadata,
               '{rules}',
               '[{\"stream\":\"observations\",\"when\":\"payload.humidity < 15\",
                  \"severity\":\"flagged\",\"title\":\"Evidence test rule\"}]'::jsonb
             )
         WHERE id = $1",
    )
    .bind(ws_id)
    .execute(&pool)
    .await
    .expect("UPDATE workspace metadata failed");

    let (_connector_id, stream_id) = seed_connector_and_stream(&pool, ws_id).await;

    let event_id = insert_stream_event(&pool, stream_id, json!({ "humidity": 5 })).await;

    ione::services::rules::evaluate_workspace(&pool, ws_id)
        .await
        .expect("evaluate_workspace failed");

    let evidence: serde_json::Value =
        sqlx::query_scalar("SELECT evidence FROM signals WHERE workspace_id = $1 LIMIT 1")
            .bind(ws_id)
            .fetch_one(&pool)
            .await
            .expect("evidence query failed — no signal row (expected failure)");

    let arr = evidence
        .as_array()
        .expect("signal.evidence must be a JSON array");

    assert!(
        !arr.is_empty(),
        "signal.evidence must not be empty after a rule match, got: {}",
        evidence
    );

    let event_id_str = event_id.to_string();
    let found = arr.iter().any(|v| {
        v.as_str() == Some(&event_id_str)
            || v.get("id").and_then(|x| x.as_str()) == Some(&event_id_str)
    });

    assert!(
        found,
        "signal.evidence must contain the stream_event id that triggered the rule ({}), \
         got: {}",
        event_id_str, evidence
    );
}
