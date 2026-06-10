/// Nested-field rule condition integration tests.
///
/// Proves that a rule `when` expression using dotted evalexpr keys
/// (`payload.properties.mag >= 6.0`) resolves nested JSON payload fields:
/// an M6.2 event fires the rule (signal + deterministic survivor), an M5.1
/// event does not.
///
/// Prerequisites:
///   docker compose up -d postgres
///   DATABASE_URL=postgres://ione:ione@localhost:5433/ione
///
/// Run:
///   DATABASE_URL=... cargo test --test phase_rule_nested_field -- --ignored --test-threads=1
use std::net::SocketAddr;

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

fn m6_rule() -> Value {
    json!([{
        "stream": "earthquakes",
        "when": "payload.properties.mag >= 6.0",
        "severity": "command",
        "title": "M6+ nested"
    }])
}

#[tokio::test]
#[ignore] // REASON: requires Docker Postgres; run with --ignored like all phase tests
async fn rule_nested_field_fires_for_m6_2_event() {
    let (_base, pool, _state) = spawn_app().await;
    let workspace_id = workspace_id(&pool).await;
    let stream_id = seed_geojson_stream(&pool, workspace_id, json!({})).await;
    insert_event(&pool, stream_id, json!({ "properties": { "mag": 6.2 } })).await;
    set_rules(&pool, workspace_id, m6_rule()).await;

    let report = ione::services::rules::evaluate_workspace(&pool, workspace_id)
        .await
        .expect("evaluate");
    assert_eq!(
        report.inserted, 1,
        "M6.2 event must match nested-field condition payload.properties.mag >= 6.0"
    );

    let signal: (Uuid, String) = sqlx::query_as(
        "SELECT id, source::text
         FROM signals
         WHERE workspace_id = $1 AND title = 'M6+ nested'",
    )
    .bind(workspace_id)
    .fetch_one(&pool)
    .await
    .expect("rule signal");
    assert_eq!(signal.1, "rule", "signal source must be 'rule'");

    let survivor: (String, String, f32) = sqlx::query_as(
        "SELECT verdict::text, critic_model, confidence
         FROM survivors
         WHERE signal_id = $1",
    )
    .bind(signal.0)
    .fetch_one(&pool)
    .await
    .expect("survivor");
    assert_eq!(survivor.0, "survive", "deterministic verdict must survive");
    assert_eq!(survivor.1, "rule-engine", "critic model must be rule-engine");
    assert_eq!(survivor.2, 1.0, "deterministic confidence must be 1.0");
}

#[tokio::test]
#[ignore] // REASON: requires Docker Postgres; run with --ignored like all phase tests
async fn rule_nested_field_does_not_fire_for_m5_1_event() {
    let (_base, pool, _state) = spawn_app().await;
    let workspace_id = workspace_id(&pool).await;
    let stream_id = seed_geojson_stream(&pool, workspace_id, json!({})).await;
    insert_event(&pool, stream_id, json!({ "properties": { "mag": 5.1 } })).await;
    set_rules(&pool, workspace_id, m6_rule()).await;

    let report = ione::services::rules::evaluate_workspace(&pool, workspace_id)
        .await
        .expect("evaluate");
    assert_eq!(
        report.inserted, 0,
        "M5.1 event must not match payload.properties.mag >= 6.0"
    );

    let signal_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM signals WHERE workspace_id = $1")
            .bind(workspace_id)
            .fetch_one(&pool)
            .await
            .expect("signal count");
    assert_eq!(signal_count, 0, "no signal may be created below threshold");

    let survivor_count: i64 = sqlx::query_scalar("SELECT count(*) FROM survivors")
        .fetch_one(&pool)
        .await
        .expect("survivor count");
    assert_eq!(survivor_count, 0, "no survivor may be created below threshold");
}
