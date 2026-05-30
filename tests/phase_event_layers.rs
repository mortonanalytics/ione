use std::net::SocketAddr;

use chrono::{Duration, Utc};
use reqwest::StatusCode;
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;
use uuid::Uuid;

use ione::services::event_layers::{project_event_layers, GeoEventRow, GeoStreamRow};

mod support;
use support::event_layer_seeder::seed_geo_stream;

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione";
const TEST_STATIC_BEARER: &str = "phase-event-test-bearer";

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

fn usgs_config() -> Value {
    json!({
        "lon_pointer": "/geometry/coordinates/0",
        "lat_pointer": "/geometry/coordinates/1",
        "property_fields": [{ "pointer": "/properties/mag", "name": "mag" }]
    })
}

fn usgs_event(lon: f64, lat: f64, mag: f64) -> Value {
    json!({
        "type": "Feature",
        "geometry": { "type": "Point", "coordinates": [lon, lat] },
        "properties": { "mag": mag, "place": "somewhere", "internal_id": "SECRET" }
    })
}

fn event_row(stream_id: Uuid, payload: Value) -> GeoEventRow {
    GeoEventRow {
        event_id: Uuid::new_v4(),
        stream_id,
        payload,
        observed_at: Utc::now(),
    }
}

fn project_one_config(view_config: Value) -> Value {
    let stream_id = Uuid::new_v4();
    let resp = project_event_layers(
        vec![GeoStreamRow {
            stream_id,
            stream_name: "test-stream".to_string(),
            view_config,
        }],
        vec![],
        5000,
        Utc::now(),
    );
    json!(resp)
}

async fn get_event_layers(base: &str, workspace_id: Uuid, query: &str) -> reqwest::Response {
    let url = if query.is_empty() {
        format!("{base}/api/v1/workspaces/{workspace_id}/event-layers")
    } else {
        format!("{base}/api/v1/workspaces/{workspace_id}/event-layers?{query}")
    };
    reqwest::Client::new()
        .get(url)
        .bearer_auth(TEST_STATIC_BEARER)
        .send()
        .await
        .expect("event layers response")
}

#[test]
fn phase2_validator_rejects_bad_config_fixtures_with_field_names() {
    let cases = [
        (
            json!({ "lon_pointer": "x", "lat_pointer": "/lat" }),
            "lon_pointer",
        ),
        (json!({ "lat_pointer": "/lat" }), "lon_pointer"),
        (
            json!({
                "lon_pointer": "/lon",
                "lat_pointer": "/lat",
                "property_fields": [{ "pointer": "/mag", "name": "1mag" }]
            }),
            "property_fields",
        ),
        (
            json!({
                "lon_pointer": "/lon",
                "lat_pointer": "/lat",
                "property_fields": [
                    { "pointer": "/mag", "name": "mag" },
                    { "pointer": "/depth", "name": "mag" }
                ]
            }),
            "duplicate",
        ),
        (
            json!({
                "lon_pointer": "/lon",
                "lat_pointer": "/lat",
                "property_fields": [{ "pointer": "/mag", "name": "mag" }],
                "style": { "size_field": "mag", "size_domain": [1, 10] }
            }),
            "size",
        ),
        (
            json!({
                "lon_pointer": "/lon",
                "lat_pointer": "/lat",
                "property_fields": [{ "pointer": "/mag", "name": "mag" }],
                "style": {
                    "color_field": "depth",
                    "color_domain": [1, 10],
                    "color_range": ["#ffff00", "#ff0000"]
                }
            }),
            "color_field",
        ),
    ];

    for (config, expected) in cases {
        let body = project_one_config(config);
        let err = body["streamsFailed"][0]["error"].as_str().unwrap();
        assert!(
            err.contains(expected),
            "expected error to contain {expected}, got {err}"
        );
    }
}

#[test]
fn phase2_projection_skips_features_with_bad_coordinates_without_failing_stream() {
    let stream_id = Uuid::new_v4();
    let resp = project_event_layers(
        vec![GeoStreamRow {
            stream_id,
            stream_name: "mixed".to_string(),
            view_config: usgs_config(),
        }],
        vec![
            event_row(stream_id, usgs_event(-122.0, 37.0, 5.1)),
            event_row(
                stream_id,
                json!({ "geometry": { "coordinates": ["bad", null] } }),
            ),
        ],
        5000,
        Utc::now(),
    );

    assert_eq!(resp.streams_failed.len(), 0);
    assert_eq!(resp.layers.len(), 1);
    assert_eq!(resp.layers[0].features_skipped, 1);
    assert_eq!(
        resp.layers[0].collection["features"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

// AC-3 + AC-12: one bad stream lands in streamsFailed while a good stream still
// renders; projection returns a normal response instead of bubbling an error.
#[test]
fn phase2_bad_stream_config_is_partial_failure_not_whole_endpoint_failure() {
    let good_stream = Uuid::new_v4();
    let bad_stream = Uuid::new_v4();
    let resp = project_event_layers(
        vec![
            GeoStreamRow {
                stream_id: bad_stream,
                stream_name: "bad".to_string(),
                view_config: json!({
                    "lon_pointer": "/lon",
                    "lat_pointer": "/lat",
                    "property_fields": [{ "pointer": "/mag", "name": "mag" }],
                    "style": { "size_field": "mag", "size_range": [4, 20] }
                }),
            },
            GeoStreamRow {
                stream_id: good_stream,
                stream_name: "good".to_string(),
                view_config: usgs_config(),
            },
        ],
        vec![event_row(good_stream, usgs_event(-122.0, 37.0, 5.1))],
        5000,
        Utc::now(),
    );

    assert_eq!(resp.layers.len(), 1);
    assert_eq!(resp.streams_ok, vec![good_stream]);
    assert_eq!(resp.streams_failed.len(), 1);
    assert_eq!(resp.streams_failed[0].stream_id, bad_stream);
    assert!(resp.streams_failed[0].error.contains("size"));
}

// AC-1 + AC-4: happy-path projection, one feature per event, no payload leakage.
#[tokio::test]
#[ignore]
async fn event_layers_projects_features_without_leakage() {
    let (base, pool) = spawn_app().await;
    let workspace_id = default_workspace_id(&pool).await;
    let now = Utc::now();
    seed_geo_stream(
        &pool,
        workspace_id,
        "quakes",
        usgs_config(),
        vec![
            (usgs_event(-122.0, 37.0, 5.1), now - Duration::minutes(1)),
            (usgs_event(-120.0, 36.0, 4.2), now - Duration::minutes(2)),
            (usgs_event(-119.0, 35.0, 3.3), now - Duration::minutes(3)),
        ],
    )
    .await;

    let resp = get_event_layers(&base, workspace_id, "").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("json");

    assert_eq!(body["layers"].as_array().unwrap().len(), 1);
    let features = body["layers"][0]["collection"]["features"]
        .as_array()
        .unwrap();
    assert_eq!(features.len(), 3);
    assert_eq!(features[0]["geometry"]["type"], "Point");
    assert_eq!(
        features[0]["geometry"]["coordinates"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    let props = features[0]["properties"].as_object().unwrap();
    let mut keys: Vec<&String> = props.keys().collect();
    keys.sort();
    assert_eq!(keys, vec!["_event_id", "_observed_at", "mag"]);
}

// AC-5: cross-org workspace request returns 404 and leaks no event rows.
#[tokio::test]
#[ignore]
async fn event_layers_rejects_cross_org_workspace() {
    let (base, pool) = spawn_app().await;
    let other_org = insert_org(&pool, "Other Org").await;
    let workspace_b = insert_workspace(&pool, other_org, "Other Workspace").await;
    seed_geo_stream(
        &pool,
        workspace_b,
        "secret-quakes",
        usgs_config(),
        vec![(usgs_event(1.0, 2.0, 9.9), Utc::now())],
    )
    .await;

    let resp = get_event_layers(&base, workspace_b, "").await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["error"], "not_found");
}

// AC-6: truncation driven by LIMIT + 1 server-side; kept features are the newest.
// (Exercised at limit=2/3-events for speed; identical semantics to the 5000/6000
// case the design states. The 5000-scale logic is also covered in service unit tests.)
#[tokio::test]
#[ignore]
async fn event_layers_truncates_to_limit_keeping_newest() {
    let (base, pool) = spawn_app().await;
    let workspace_id = default_workspace_id(&pool).await;
    let now = Utc::now();
    seed_geo_stream(
        &pool,
        workspace_id,
        "busy",
        usgs_config(),
        vec![
            (usgs_event(-1.0, 1.0, 1.0), now - Duration::minutes(3)),
            (usgs_event(-2.0, 2.0, 2.0), now - Duration::minutes(2)),
            (usgs_event(-3.0, 3.0, 3.0), now - Duration::minutes(1)),
        ],
    )
    .await;

    let resp = get_event_layers(&base, workspace_id, "limit=2").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("json");

    assert_eq!(body["truncated"], true);
    let total: usize = body["layers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["collection"]["features"].as_array().unwrap().len())
        .sum();
    assert_eq!(total, 2);
    // Newest-first ordering: the two kept events are mag 3.0 and 2.0, not 1.0.
    let mags: Vec<f64> = body["layers"][0]["collection"]["features"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["properties"]["mag"].as_f64().unwrap())
        .collect();
    assert!(mags.contains(&3.0) && mags.contains(&2.0) && !mags.contains(&1.0));
}

// AC-7: time window > 30 days is rejected with 400 mentioning the cap.
#[tokio::test]
#[ignore]
async fn event_layers_rejects_window_over_thirty_days() {
    let (base, pool) = spawn_app().await;
    let workspace_id = default_workspace_id(&pool).await;

    let resp = get_event_layers(
        &base,
        workspace_id,
        "since=2026-01-01T00:00:00Z&until=2026-03-01T00:00:00Z",
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body: Value = resp.json().await.expect("json");
    assert!(body["message"].as_str().unwrap().contains("30"));
}
