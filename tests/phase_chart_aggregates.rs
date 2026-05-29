use std::net::SocketAddr;

use chrono::{Duration, TimeZone, Utc};
use reqwest::StatusCode;
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;
use uuid::Uuid;

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione";
const TEST_STATIC_BEARER: &str = "phase-chart-agg-test-bearer";

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
        "TRUNCATE webhook_events_seen, workspace_peer_bindings, audit_events, approvals, artifacts,
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

async fn insert_stream(pool: &PgPool, workspace_id: Uuid) -> Uuid {
    let connector_id: Uuid = sqlx::query_scalar(
        "INSERT INTO connectors (workspace_id, kind, name, config, status)
         VALUES ($1, 'rust_native'::connector_kind, 'chart-test', '{}'::jsonb, 'active'::connector_status)
         RETURNING id",
    )
    .bind(workspace_id)
    .fetch_one(pool)
    .await
    .expect("insert connector");

    sqlx::query_scalar(
        "INSERT INTO streams (connector_id, name, schema, view_config)
         VALUES ($1, 'Earthquakes', '{}'::jsonb, $2)
         RETURNING id",
    )
    .bind(connector_id)
    .bind(json!({
        "lon_pointer": "/geometry/coordinates/0",
        "lat_pointer": "/geometry/coordinates/1",
        "property_fields": [
            { "pointer": "/properties/mag", "name": "mag" },
            { "pointer": "/properties/type", "name": "type" }
        ]
    }))
    .fetch_one(pool)
    .await
    .expect("insert stream")
}

async fn insert_event(pool: &PgPool, stream_id: Uuid, day: i64, seq: i64, payload: Value) {
    let observed_at = Utc.with_ymd_and_hms(2026, 5, 1, 0, 0, 0).unwrap()
        + Duration::days(day)
        + Duration::seconds(seq);
    sqlx::query(
        "INSERT INTO stream_events (stream_id, payload, observed_at)
         VALUES ($1, $2, $3)",
    )
    .bind(stream_id)
    .bind(payload)
    .bind(observed_at)
    .execute(pool)
    .await
    .expect("insert event");
}

async fn get_aggregates(base: &str, workspace_id: Uuid, query: &str) -> reqwest::Response {
    reqwest::Client::new()
        .get(format!(
            "{base}/api/v1/workspaces/{workspace_id}/event-aggregates?{query}"
        ))
        .bearer_auth(TEST_STATIC_BEARER)
        .send()
        .await
        .expect("event aggregates response")
}

#[tokio::test]
#[ignore]
async fn chart_aggregates_cover_count_numeric_percentile_group_and_nonnumeric() {
    let (base, pool) = spawn_app().await;
    let workspace_id = default_workspace_id(&pool).await;
    let stream_id = insert_stream(&pool, workspace_id).await;

    for (idx, (day, mag, kind)) in [
        (0, json!(4.0), "earthquake"),
        (0, json!(6.0), "earthquake"),
        (1, json!(5.0), "earthquake"),
        (1, json!(7.0), "quarry blast"),
        (1, json!("n/a"), "earthquake"),
    ]
    .into_iter()
    .enumerate()
    {
        insert_event(
            &pool,
            stream_id,
            day,
            idx as i64,
            json!({
                "geometry": { "coordinates": [-122.0, 37.0] },
                "properties": { "mag": mag, "type": kind }
            }),
        )
        .await;
    }

    let window =
        format!("stream_id={stream_id}&since=2026-05-01T00:00:00Z&until=2026-05-03T00:00:00Z");

    let resp = get_aggregates(
        &base,
        workspace_id,
        &format!("{window}&op=count&bucket=day"),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["rows"].as_array().unwrap().len(), 2);
    assert_eq!(body["rows"][0]["value"], 2);
    assert_eq!(body["rows"][1]["value"], 3);
    // epoch *milliseconds* (a regression to seconds would be ~1.78e9, not ~1.78e12)
    assert!(body["rows"][0]["bucketStartMs"].as_i64().unwrap() > 1_700_000_000_000);

    let resp = get_aggregates(
        &base,
        workspace_id,
        &format!("{window}&op=avg&bucket=day&value_pointer=/properties/mag"),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["rows"][0]["avg"], 5.0);
    assert_eq!(body["rows"][0]["max"], 6.0);
    assert_eq!(body["rows"][1]["eventCount"], 3);
    assert_eq!(body["rows"][1]["validCount"], 2);

    let resp = get_aggregates(
        &base,
        workspace_id,
        &format!("{window}&op=percentile&bucket=day&value_pointer=/properties/mag&percentile=0.5"),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["rows"][0]["percentileValue"], 5.0);

    let resp = get_aggregates(
        &base,
        workspace_id,
        &format!("{window}&op=group_by&bucket=day&group_by_pointer=/properties/type"),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["rows"][0]["groupKey"], "earthquake");
    assert_eq!(body["rows"][0]["eventCount"], 4);
    assert_eq!(body["rows"][1]["groupKey"], "quarry blast");
    assert_eq!(body["rows"][1]["eventCount"], 1);
}

#[tokio::test]
#[ignore]
async fn chart_aggregates_baseline_guardrail_and_cross_org_scope() {
    let (base, pool) = spawn_app().await;
    let workspace_id = default_workspace_id(&pool).await;
    let stream_id = insert_stream(&pool, workspace_id).await;
    for day in -30..35 {
        insert_event(
            &pool,
            stream_id,
            day,
            0,
            json!({
                "geometry": { "coordinates": [-122.0, 37.0] },
                "properties": { "mag": 4.0, "type": "earthquake" }
            }),
        )
        .await;
    }

    let resp = get_aggregates(
        &base,
        workspace_id,
        &format!(
            "stream_id={stream_id}&op=baseline&bucket=day&since=2026-05-01T00:00:00Z&until=2026-06-05T00:00:00Z"
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["rows"].as_array().unwrap().len(), 35);
    // one event per day → trailing 30-day average is exactly 1.0 at every output bucket
    let avg = body["rows"][30]["trailing30dAvg"].as_f64().unwrap();
    assert!((avg - 1.0).abs() < 1e-9, "expected trailing avg 1.0, got {avg}");
    assert!(body["rows"][0]["bucketStart"].as_str().unwrap() >= "2026-05-01");

    let resp = get_aggregates(
        &base,
        workspace_id,
        &format!(
            "stream_id={stream_id}&op=baseline&bucket=week&since=2026-05-01T00:00:00Z&until=2026-06-05T00:00:00Z"
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let resp = get_aggregates(
        &base,
        workspace_id,
        &format!(
            "stream_id={stream_id}&op=count&bucket=hour&since=2026-05-01T00:00:00Z&until=2026-07-30T00:00:00Z"
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let other_org = insert_org(&pool, "Other Org").await;
    let other_workspace = insert_workspace(&pool, other_org, "Other Workspace").await;
    let resp = get_aggregates(
        &base,
        other_workspace,
        &format!(
            "stream_id={stream_id}&op=count&bucket=day&since=2026-05-01T00:00:00Z&until=2026-05-02T00:00:00Z"
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
