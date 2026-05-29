use std::net::SocketAddr;

use chrono::{Duration, Utc};
use reqwest::StatusCode;
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;
use uuid::Uuid;

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione";
const TEST_STATIC_BEARER: &str = "phase-table-test-bearer";

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

async fn insert_stream(pool: &PgPool, workspace_id: Uuid, view_config: Value) -> Uuid {
    let connector_id: Uuid = sqlx::query_scalar(
        "INSERT INTO connectors (workspace_id, kind, name, config, status)
         VALUES ($1, 'rust_native'::connector_kind, 'table-test', '{}'::jsonb, 'active'::connector_status)
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
    .bind(view_config)
    .fetch_one(pool)
    .await
    .expect("insert stream")
}

async fn insert_event(
    pool: &PgPool,
    stream_id: Uuid,
    observed_at: chrono::DateTime<Utc>,
    payload: Value,
) {
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

fn view_config() -> Value {
    json!({
        "lon_pointer": "/geometry/coordinates/0",
        "lat_pointer": "/geometry/coordinates/1",
        "property_fields": [
            { "pointer": "/properties/mag", "name": "mag" },
            { "pointer": "/properties/type", "name": "type" },
            { "pointer": "/properties/observed_at", "name": "observed_at" },
            { "pointer": "/properties/missing", "name": "missing" }
        ]
    })
}

async fn get_table(base: &str, workspace_id: Uuid, query: &str) -> reqwest::Response {
    reqwest::Client::new()
        .get(format!(
            "{base}/api/v1/workspaces/{workspace_id}/event-table?{query}"
        ))
        .bearer_auth(TEST_STATIC_BEARER)
        .send()
        .await
        .expect("event-table response")
}

#[tokio::test]
#[ignore]
async fn event_table_projects_paginates_sorts_filters_and_avoids_timestamp_collision() {
    let (base, pool) = spawn_app().await;
    let workspace_id = default_workspace_id(&pool).await;
    let stream_id = insert_stream(&pool, workspace_id, view_config()).await;
    let start = Utc::now() - Duration::days(5);

    for idx in 0..60 {
        let mag = if idx == 7 { 10 } else { idx % 9 + 1 };
        let kind = if idx % 2 == 0 {
            "earthquake"
        } else {
            "quarry blast"
        };
        insert_event(
            &pool,
            stream_id,
            start + Duration::minutes(idx),
            json!({
                "geometry": { "coordinates": [-122.0, 37.0] },
                "properties": {
                    "mag": mag,
                    "type": kind,
                    "observed_at": format!("payload-{idx}")
                }
            }),
        )
        .await;
    }
    insert_event(
        &pool,
        stream_id,
        start + Duration::hours(2),
        json!({
            "geometry": { "coordinates": [-122.0, 37.0] },
            "properties": { "mag": 2, "type": "earthquake", "observed_at": "payload-missing" }
        }),
    )
    .await;

    let window = format!(
        "stream_id={stream_id}&since={}&until={}",
        (start - Duration::minutes(1)).to_rfc3339(),
        (start + Duration::days(1)).to_rfc3339()
    );
    let resp = get_table(&base, workspace_id, &format!("{window}&page=1&per_page=25")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("json");
    let names: Vec<_> = body["columns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|col| col["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec!["_observed_at", "mag", "type", "observed_at", "missing"]
    );
    assert_eq!(body["rows"].as_array().unwrap().len(), 25);
    assert_eq!(body["totalCount"], 61);
    assert_eq!(body["truncated"], true);
    assert!(body["rows"][0]["_observed_at"]
        .as_str()
        .unwrap()
        .contains('T'));
    assert_ne!(
        body["rows"][0]["_observed_at"],
        body["rows"][0]["observed_at"]
    );

    let resp = get_table(&base, workspace_id, &format!("{window}&page=3&per_page=25")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["rows"].as_array().unwrap().len(), 11);
    assert_eq!(body["truncated"], false);

    let resp = get_table(
        &base,
        workspace_id,
        &format!("{window}&sort_by=mag&sort_dir=desc&per_page=1"),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["rows"][0]["mag"], "10");

    let resp = get_table(
        &base,
        workspace_id,
        &format!("{window}&filter_col=type&filter_val=QUAKE&per_page=200"),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("json");
    assert!(body["rows"]
        .as_array()
        .unwrap()
        .iter()
        .all(|row| row["type"].as_str().unwrap().contains("quake")));
    assert_eq!(body["totalCount"], 31);

    let row_with_missing = body["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["observed_at"] == "payload-missing")
        .expect("missing fixture row");
    assert!(row_with_missing["missing"].is_null());
}

#[tokio::test]
#[ignore]
async fn event_table_guardrails_default_window_and_cross_org_scope() {
    let (base, pool) = spawn_app().await;
    let workspace_id = default_workspace_id(&pool).await;
    let stream_id = insert_stream(&pool, workspace_id, view_config()).await;
    let recent = Utc::now() - Duration::days(1);
    let old = Utc::now() - Duration::days(35);
    for (observed_at, label) in [(recent, "recent"), (old, "old")] {
        insert_event(
            &pool,
            stream_id,
            observed_at,
            json!({
                "geometry": { "coordinates": [-122.0, 37.0] },
                "properties": { "mag": 5, "type": label, "observed_at": label }
            }),
        )
        .await;
    }

    let resp = get_table(&base, workspace_id, &format!("stream_id={stream_id}")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["totalCount"], 1);
    assert_eq!(body["rows"][0]["type"], "recent");

    for query in [
        format!("stream_id={stream_id}&per_page=500"),
        format!("stream_id={stream_id}&page=100&per_page=200"),
        format!("stream_id={stream_id}&sort_by=evil%3B%20DROP"),
        format!("stream_id={stream_id}&filter_col=type"),
        format!("stream_id={stream_id}&filter_val=quake"),
        format!(
            "stream_id={stream_id}&since={}&until={}",
            recent.to_rfc3339(),
            old.to_rfc3339()
        ),
        format!(
            "stream_id={stream_id}&since={}&until={}",
            (recent - Duration::days(100)).to_rfc3339(),
            recent.to_rfc3339()
        ),
    ] {
        let resp = get_table(&base, workspace_id, &query).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{query}");
    }

    let other_org = insert_org(&pool, "Other Org").await;
    let other_workspace = insert_workspace(&pool, other_org, "Other Workspace").await;
    let resp = get_table(
        &base,
        other_workspace,
        &format!("stream_id={stream_id}&per_page=25"),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
