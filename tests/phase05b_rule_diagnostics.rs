use std::net::SocketAddr;

use reqwest::StatusCode;
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;
use uuid::Uuid;

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione";

async fn spawn_app() -> (String, PgPool, ione::state::AppState) {
    std::env::set_var("IONE_AUTH_MODE", "local");
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
        "TRUNCATE webhook_events_seen, workspace_peer_bindings, audit_events, approvals, artifacts,
                  pipeline_events, rule_diagnostics, trust_issuers, peers, routing_decisions,
                  survivors, signals, stream_events, streams, connectors,
                  memberships, roles, messages, conversations,
                  workspaces, users, organizations
         RESTART IDENTITY CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate failed");

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr: SocketAddr = listener.local_addr().expect("local addr");
    let (app, state) = ione::app_with_state(pool.clone()).await;
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("server error");
    });
    (format!("http://{}", addr), pool, state)
}

async fn workspace_id(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("SELECT id FROM workspaces WHERE name = 'Operations' LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("Operations workspace not found")
}

async fn seed_geojson_stream(pool: &PgPool, workspace_id: Uuid, field_types: Value) -> Uuid {
    let config = json!({
        "kind": "geojson_poll",
        "feed_url": "http://127.0.0.1:1/feed",
        "stream_name": "earthquakes",
        "observed_at_format": "none",
        "field_types": field_types
    });
    let connector_id: Uuid = sqlx::query_scalar(
        "INSERT INTO connectors (workspace_id, kind, name, config, status)
         VALUES ($1, 'rust_native', 'geojson-test', $2, 'active')
         RETURNING id",
    )
    .bind(workspace_id)
    .bind(config)
    .fetch_one(pool)
    .await
    .expect("insert connector");

    sqlx::query_scalar(
        "INSERT INTO streams (connector_id, name, schema)
         VALUES ($1, 'earthquakes', '{}'::jsonb)
         RETURNING id",
    )
    .bind(connector_id)
    .fetch_one(pool)
    .await
    .expect("insert stream")
}

async fn insert_event(pool: &PgPool, stream_id: Uuid, payload: Value) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO stream_events (stream_id, payload, observed_at, ingested_at)
         VALUES ($1, $2, now(), now())
         RETURNING id",
    )
    .bind(stream_id)
    .bind(payload)
    .fetch_one(pool)
    .await
    .expect("insert event")
}

async fn set_rules(pool: &PgPool, workspace_id: Uuid, rules: Value) {
    sqlx::query(
        "UPDATE workspaces
         SET metadata = jsonb_set(COALESCE(metadata, '{}'::jsonb), '{rules}', $2)
         WHERE id = $1",
    )
    .bind(workspace_id)
    .bind(rules)
    .execute(pool)
    .await
    .expect("set rules");
}

#[tokio::test]
#[ignore]
async fn rule_diagnostics_unknown_stream_benign_and_unparseable() {
    let (base, pool, state) = spawn_app().await;
    let workspace_id = workspace_id(&pool).await;
    let stream_id = seed_geojson_stream(&pool, workspace_id, json!({})).await;
    insert_event(&pool, stream_id, json!({ "properties": { "mag": 3.2 } })).await;

    set_rules(
        &pool,
        workspace_id,
        json!([
            {
                "stream": "does-not-exist",
                "when": "payload.properties.mag >= 6.0",
                "severity": "routine",
                "title": "Missing stream"
            },
            {
                "stream": "earthquakes",
                "when": "payload.properties.mag >= 6.0",
                "severity": "routine",
                "title": "Below threshold"
            }
        ]),
    )
    .await;
    ione::services::scheduler::run_tick(&state, true)
        .await
        .expect("tick");

    let body: Value = reqwest::Client::new()
        .get(format!(
            "{base}/api/v1/workspaces/{workspace_id}/rule-diagnostics"
        ))
        .send()
        .await
        .expect("diagnostics response")
        .json()
        .await
        .expect("diagnostics json");
    let items = body["items"].as_array().expect("items");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["status"], json!("stream_not_found"));
    assert!(!items[0]["skipReasons"].as_array().unwrap().is_empty());
    assert_eq!(items[1]["status"], json!("ok"));
    assert_eq!(items[1]["eventsEvaluated"], json!(1));
    assert_eq!(items[1]["matchCount"], json!(0));

    sqlx::query("UPDATE workspaces SET metadata = jsonb_build_object('rules', jsonb_build_object('bad', true)) WHERE id = $1")
        .bind(workspace_id)
        .execute(&pool)
        .await
        .expect("corrupt rules");
    ione::services::scheduler::run_tick(&state, true)
        .await
        .expect("tick corrupt");
    let body: Value = reqwest::Client::new()
        .get(format!(
            "{base}/api/v1/workspaces/{workspace_id}/rule-diagnostics"
        ))
        .send()
        .await
        .expect("diagnostics response")
        .json()
        .await
        .expect("diagnostics json");
    assert_eq!(body["items"][0]["status"], json!("rules_unparseable"));
}

#[tokio::test]
#[ignore]
async fn rule_deterministic_survivor_advances_command_offline() {
    std::env::set_var("OLLAMA_BASE_URL", "http://127.0.0.1:1");
    let (_base, pool, state) = spawn_app().await;
    let workspace_id = workspace_id(&pool).await;
    let stream_id = seed_geojson_stream(&pool, workspace_id, json!({})).await;
    insert_event(&pool, stream_id, json!({ "properties": { "mag": 6.4 } })).await;
    set_rules(
        &pool,
        workspace_id,
        json!([{
            "stream": "earthquakes",
            "when": "payload.properties.mag >= 6.0",
            "severity": "command",
            "title": "M6+"
        }]),
    )
    .await;

    ione::services::scheduler::run_tick(&state, false)
        .await
        .expect("offline tick");

    let signal: (Uuid, String, bool) = sqlx::query_as(
        "SELECT id, source::text, approval_required
         FROM signals
         WHERE workspace_id = $1 AND title = 'M6+'",
    )
    .bind(workspace_id)
    .fetch_one(&pool)
    .await
    .expect("rule signal");
    assert_eq!(signal.1, "rule");
    assert!(signal.2);

    let survivor: (String, String, f32) = sqlx::query_as(
        "SELECT verdict::text, critic_model, confidence
         FROM survivors
         WHERE signal_id = $1",
    )
    .bind(signal.0)
    .fetch_one(&pool)
    .await
    .expect("survivor");
    assert_eq!(survivor.0, "survive");
    assert_eq!(survivor.1, "rule-engine");
    assert_eq!(survivor.2, 1.0);

    let routing_kind: String = sqlx::query_scalar(
        "SELECT rd.target_kind::text
         FROM routing_decisions rd
         JOIN survivors sv ON sv.id = rd.survivor_id
         WHERE sv.signal_id = $1",
    )
    .bind(signal.0)
    .fetch_one(&pool)
    .await
    .expect("routing");
    assert_eq!(routing_kind, "draft");

    let approvals: i64 = sqlx::query_scalar(
        "SELECT count(*)
         FROM approvals ap
         JOIN artifacts art ON art.id = ap.artifact_id
         WHERE art.workspace_id = $1 AND ap.status = 'pending'",
    )
    .bind(workspace_id)
    .fetch_one(&pool)
    .await
    .expect("approval count");
    assert_eq!(approvals, 1);

    let delivered: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE workspace_id = $1 AND verb = 'delivered'",
    )
    .bind(workspace_id)
    .fetch_one(&pool)
    .await
    .expect("delivered count");
    assert_eq!(delivered, 0);
}

#[tokio::test]
#[ignore]
async fn rule_deterministic_survivor_signals_endpoint_not_critic_gated() {
    let (base, pool, _state) = spawn_app().await;
    let workspace_id = workspace_id(&pool).await;
    let signal_id: Uuid = sqlx::query_scalar(
        "INSERT INTO signals (workspace_id, source, title, body, severity, evidence, generator_model)
         VALUES ($1, 'generator', 'Deferred generator', 'body', 'routine', '[]'::jsonb, 'test-model')
         RETURNING id",
    )
    .bind(workspace_id)
    .fetch_one(&pool)
    .await
    .expect("insert signal");
    sqlx::query(
        "INSERT INTO survivors (signal_id, critic_model, verdict, rationale, confidence)
         VALUES ($1, 'test-critic', 'defer', 'offline', 0.0)",
    )
    .bind(signal_id)
    .execute(&pool)
    .await
    .expect("insert defer survivor");

    let body: Value = reqwest::Client::new()
        .get(format!("{base}/api/v1/workspaces/{workspace_id}/signals"))
        .send()
        .await
        .expect("signals response")
        .json()
        .await
        .expect("signals json");
    let items = body["items"].as_array().expect("items");
    assert!(items.iter().any(|item| item["id"] == json!(signal_id)));
}

#[tokio::test]
#[ignore]
async fn rule_diagnostics_pipeline_stage_constraint_allows_append() {
    let (_base, pool, _state) = spawn_app().await;
    let workspace_id = workspace_id(&pool).await;
    let repo = ione::repos::PipelineEventRepo::new(pool.clone());
    let event = repo
        .append(ione::models::PipelineEventInput {
            workspace_id,
            connector_id: None,
            stream_id: None,
            stage: ione::models::PipelineEventStage::RuleDiagnostic,
            detail: Some(json!({ "rules": 1 })),
        })
        .await
        .expect("append rule_diagnostic");
    assert_eq!(
        event.stage,
        ione::models::PipelineEventStage::RuleDiagnostic
    );
}

#[tokio::test]
#[ignore]
async fn rule_typed_declared_number_string_matches() {
    let (_base, pool, _state) = spawn_app().await;
    let workspace_id = workspace_id(&pool).await;
    let stream_id =
        seed_geojson_stream(&pool, workspace_id, json!({ "/properties/mag": "number" })).await;
    insert_event(&pool, stream_id, json!({ "properties": { "mag": "6.4" } })).await;
    set_rules(
        &pool,
        workspace_id,
        json!([{
            "stream": "earthquakes",
            "when": "payload.properties.mag >= 6.0",
            "severity": "command",
            "title": "M6+"
        }]),
    )
    .await;

    let report = ione::services::rules::evaluate_workspace(&pool, workspace_id)
        .await
        .expect("evaluate");
    assert_eq!(report.inserted, 1);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM signals WHERE source = 'rule'")
        .fetch_one(&pool)
        .await
        .expect("count signals");
    assert_eq!(count, 1);
}

#[tokio::test]
#[ignore]
async fn rule_typed_declared_string_and_mismatch_diagnostic() {
    let (_base, pool, state) = spawn_app().await;
    let workspace_id = workspace_id(&pool).await;
    let stream_id = seed_geojson_stream(
        &pool,
        workspace_id,
        json!({ "/properties/code": "string", "/properties/mag": "number" }),
    )
    .await;
    insert_event(
        &pool,
        stream_id,
        json!({ "properties": { "code": "01234", "mag": "high" } }),
    )
    .await;
    set_rules(
        &pool,
        workspace_id,
        json!([
            {
                "stream": "earthquakes",
                "when": "payload.properties.code == \"01234\"",
                "severity": "routine",
                "title": "Code match"
            },
            {
                "stream": "earthquakes",
                "when": "payload.properties.mag >= 6.0",
                "severity": "routine",
                "title": "Bad mag"
            }
        ]),
    )
    .await;

    ione::services::scheduler::run_tick(&state, true)
        .await
        .expect("tick");

    let signal_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM signals WHERE title = 'Code match'")
            .fetch_one(&pool)
            .await
            .expect("signal count");
    assert_eq!(signal_count, 1);

    let snap = ione::repos::RuleDiagnosticsRepo::new(pool.clone())
        .get(workspace_id)
        .await
        .expect("diagnostics")
        .expect("snapshot");
    let bad_mag = snap
        .1
        .iter()
        .find(|item| item.rule_title == "Bad mag")
        .expect("bad mag diagnostic");
    assert_eq!(bad_mag.status, ione::models::DiagStatus::TypeMismatch);
    assert!(bad_mag
        .skip_reasons
        .iter()
        .any(|reason| reason.code == "type_mismatch" && reason.detail == "/properties/mag"));
}

#[tokio::test]
#[ignore]
async fn rule_validation_invalid_patch_rejected_and_metadata_unchanged() {
    let (base, pool, _state) = spawn_app().await;
    let workspace_id = workspace_id(&pool).await;
    set_rules(
        &pool,
        workspace_id,
        json!([{
            "stream": "earthquakes",
            "when": "payload.properties.mag >= 6.0",
            "severity": "command",
            "title": "Original"
        }]),
    )
    .await;
    let before: Value = sqlx::query_scalar("SELECT metadata FROM workspaces WHERE id = $1")
        .bind(workspace_id)
        .fetch_one(&pool)
        .await
        .expect("before metadata");

    let resp = reqwest::Client::new()
        .patch(format!("{base}/api/v1/workspaces/{workspace_id}"))
        .json(&json!({
            "metadata": {
                "rules": [{
                    "stream": "earthquakes",
                    "when": "payload.mag >= (",
                    "severity": "command",
                    "title": "Broken"
                }]
            }
        }))
        .send()
        .await
        .expect("patch");
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body: Value = resp.json().await.expect("422 body");
    assert_eq!(body["ruleIndex"], json!(0));

    let after: Value = sqlx::query_scalar("SELECT metadata FROM workspaces WHERE id = $1")
        .bind(workspace_id)
        .fetch_one(&pool)
        .await
        .expect("after metadata");
    assert_eq!(after, before);
}
