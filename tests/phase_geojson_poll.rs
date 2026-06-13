/// GeoJSON poll connector integration tests.
///
/// Prerequisites:
///   docker compose up -d postgres
///   DATABASE_URL=postgres://ione:ione@localhost:5433/ione
///
/// Run:
///   DATABASE_URL=... cargo test --test phase_geojson_poll -- --ignored --test-threads=1
use std::net::SocketAddr;

use chrono::{TimeZone, Utc};
use reqwest::StatusCode;
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use ione::repos::{InsertOutcome, StreamEventRepo, StreamRepo};

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione";
const TEST_STATIC_BEARER: &str = "phase-geojson-test-bearer";

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

    // create_connector is gated by workspace:write (HP-H1). Grant the bootstrap
    // 'member' role on Operations so the connector-create tests authorize.
    sqlx::query(
        "UPDATE roles SET permissions = '[\"workspace:write\"]'::jsonb
         WHERE name = 'member'
           AND workspace_id = (SELECT id FROM workspaces WHERE name = 'Operations' LIMIT 1)",
    )
    .execute(&pool)
    .await
    .expect("grant member workspace:write");

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

async fn insert_connector(pool: &PgPool, workspace_id: Uuid, name: &str, config: Value) -> Uuid {
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
    .expect("insert connector")
}

async fn insert_stream(pool: &PgPool, connector_id: Uuid, view_config: Option<Value>) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO streams (connector_id, name, schema, view_config)
         VALUES ($1, 'earthquakes', '{}'::jsonb, $2)
         RETURNING id",
    )
    .bind(connector_id)
    .bind(view_config)
    .fetch_one(pool)
    .await
    .expect("insert stream")
}

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

fn bearer(req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    req.bearer_auth(TEST_STATIC_BEARER)
}

fn view_config(property_name: &str) -> Value {
    json!({
        "lon_pointer": "/geometry/coordinates/0",
        "lat_pointer": "/geometry/coordinates/1",
        "property_fields": [{ "pointer": "/properties/mag", "name": property_name }]
    })
}

fn geojson_config(feed_url: &str) -> Value {
    json!({
        "kind": "geojson_poll",
        "feed_url": feed_url,
        "stream_name": "earthquakes",
        "items_pointer": "/features",
        "observed_at_pointer": "/properties/time",
        "observed_at_format": "epoch_ms",
        "dedup_pointer": "/id",
        "type_filter": { "pointer": "/properties/type", "allow": ["earthquake"] },
        "view_config": view_config("magnitude")
    })
}

fn usgs_fixture(id: &str, mag: f64, event_type: &str) -> Value {
    json!({
        "type": "FeatureCollection",
        "features": [{
            "type": "Feature",
            "id": id,
            "geometry": { "type": "Point", "coordinates": [-122.1, 38.2, 7.0] },
            "properties": {
                "mag": mag,
                "type": event_type,
                "time": 1779991039445_i64
            }
        }]
    })
}

async fn mount_feed(server: &MockServer, body: Value) {
    Mock::given(method("GET"))
        .and(path("/feed"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

#[tokio::test]
#[ignore]
async fn upsert_named_returns_view_config() {
    let (_, pool) = spawn_app().await;
    let workspace_id = default_workspace_id(&pool).await;
    let connector_id = insert_connector(&pool, workspace_id, "geojson-test", json!({})).await;
    let config = view_config("magnitude");

    let stream = StreamRepo::new(pool.clone())
        .upsert_named(connector_id, "earthquakes", json!({}), Some(config.clone()))
        .await
        .expect("upsert stream");

    assert_eq!(stream.view_config, Some(config));
}

#[tokio::test]
#[ignore]
async fn geojson_create_polls_and_renders_event_layer() {
    let (base, pool) = spawn_app().await;
    let workspace_id = default_workspace_id(&pool).await;
    let feed = MockServer::start().await;
    mount_feed(&feed, usgs_fixture("us7000sp4x", 4.6, "earthquake")).await;

    let resp = bearer(client().post(format!(
        "{base}/api/v1/workspaces/{workspace_id}/connectors"
    )))
    .json(&json!({
        "kind": "rust_native",
        "name": "geojson-usgs",
        "config": geojson_config(&format!("{}/feed", feed.uri()))
    }))
    .send()
    .await
    .expect("create connector response");
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "{}",
        resp.text().await.unwrap()
    );

    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM stream_events")
        .fetch_one(&pool)
        .await
        .expect("stream event count");
    let row: (String, Value, chrono::DateTime<Utc>) =
        sqlx::query_as("SELECT dedup_key, payload, observed_at FROM stream_events LIMIT 1")
            .fetch_one(&pool)
            .await
            .expect("stream event row");
    assert_eq!(total, 1);
    assert_eq!(row.0, "us7000sp4x");
    assert_eq!(row.1["properties"]["mag"], json!(4.6));
    assert_eq!(
        row.2,
        Utc.timestamp_millis_opt(1779991039445).single().unwrap()
    );

    let layers: Value = bearer(
        client()
            .get(format!(
                "{base}/api/v1/workspaces/{workspace_id}/event-layers"
            ))
            .query(&[
                ("since", "2026-05-28T00:00:00Z"),
                ("until", "2026-05-29T00:00:00Z"),
            ]),
    )
    .send()
    .await
    .expect("event layers response")
    .json()
    .await
    .expect("event layers json");

    let feature = &layers["layers"][0]["collection"]["features"][0];
    assert_eq!(feature["geometry"]["coordinates"], json!([-122.1, 38.2]));
    assert_eq!(feature["properties"]["magnitude"], json!(4.6));
}

#[tokio::test]
#[ignore]
async fn view_config_put_validates_audits_and_survives_poll() {
    let (base, pool) = spawn_app().await;
    let workspace_id = default_workspace_id(&pool).await;
    let feed = MockServer::start().await;
    mount_feed(&feed, usgs_fixture("us7000sp4x", 4.6, "earthquake")).await;

    let create = bearer(client().post(format!(
        "{base}/api/v1/workspaces/{workspace_id}/connectors"
    )))
    .json(&json!({
        "kind": "rust_native",
        "name": "geojson-usgs",
        "config": geojson_config(&format!("{}/feed", feed.uri()))
    }))
    .send()
    .await
    .expect("create connector response");
    assert_eq!(create.status(), StatusCode::OK);

    let stream_id: Uuid = sqlx::query_scalar("SELECT id FROM streams LIMIT 1")
        .fetch_one(&pool)
        .await
        .expect("stream id");

    let bad = bearer(client().put(format!("{base}/api/v1/streams/{stream_id}/view-config")))
        .json(&json!({ "lon_pointer": "/geometry/coordinates/0" }))
        .send()
        .await
        .expect("bad view config response");
    assert_eq!(bad.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let custom = view_config("customMagnitude");
    let good = bearer(client().put(format!("{base}/api/v1/streams/{stream_id}/view-config")))
        .json(&custom)
        .send()
        .await
        .expect("good view config response");
    assert_eq!(good.status(), StatusCode::OK);
    let body: Value = good.json().await.expect("view config response json");
    assert_eq!(body["viewConfig"], custom);

    let audit_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events
         WHERE verb = 'stream.view_config.updated' AND object_id = $1",
    )
    .bind(stream_id)
    .fetch_one(&pool)
    .await
    .expect("audit count");
    assert_eq!(audit_count, 1);

    let poll = bearer(client().post(format!("{base}/api/v1/streams/{stream_id}/poll")))
        .send()
        .await
        .expect("poll response");
    assert_eq!(poll.status(), StatusCode::OK);
    let persisted: Value = sqlx::query_scalar("SELECT view_config FROM streams WHERE id = $1")
        .bind(stream_id)
        .fetch_one(&pool)
        .await
        .expect("persisted view_config");
    assert_eq!(persisted, custom);
}

#[tokio::test]
#[ignore]
async fn cross_org_stream_routes_are_scoped() {
    let (base, pool) = spawn_app().await;
    let other_org = insert_org(&pool, "Other Org").await;
    let other_workspace = insert_workspace(&pool, other_org, "Other Workspace").await;
    let connector_id = insert_connector(&pool, other_workspace, "foreign", json!({})).await;
    let stream_id = insert_stream(&pool, connector_id, Some(view_config("magnitude"))).await;

    let list: Value =
        bearer(client().get(format!("{base}/api/v1/connectors/{connector_id}/streams")))
            .send()
            .await
            .expect("list streams response")
            .json()
            .await
            .expect("list streams json");
    assert_eq!(list["items"].as_array().unwrap().len(), 0);

    let poll = bearer(client().post(format!("{base}/api/v1/streams/{stream_id}/poll")))
        .send()
        .await
        .expect("poll response");
    assert_eq!(poll.status(), StatusCode::NOT_FOUND);

    // AC-9: cross-org PUT view-config returns 404 and leaves the config untouched.
    let put = bearer(client().put(format!("{base}/api/v1/streams/{stream_id}/view-config")))
        .json(&view_config("hacked"))
        .send()
        .await
        .expect("cross-org put response");
    assert_eq!(put.status(), StatusCode::NOT_FOUND);
    let persisted: Value = sqlx::query_scalar("SELECT view_config FROM streams WHERE id = $1")
        .bind(stream_id)
        .fetch_one(&pool)
        .await
        .expect("persisted view_config");
    assert_eq!(persisted, view_config("magnitude"));

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM stream_events")
        .fetch_one(&pool)
        .await
        .expect("event count");
    assert_eq!(count, 0);
}

#[tokio::test]
#[ignore]
async fn dedup_insert_reports_update_not_insert() {
    let (_, pool) = spawn_app().await;
    let workspace_id = default_workspace_id(&pool).await;
    let connector_id = insert_connector(&pool, workspace_id, "geojson-test", json!({})).await;
    let stream_id = insert_stream(&pool, connector_id, Some(view_config("magnitude"))).await;
    let repo = StreamEventRepo::new(pool.clone());
    let observed_at = Utc.timestamp_millis_opt(1779991039445).single().unwrap();

    let first = repo
        .insert_event(
            stream_id,
            json!({ "properties": { "mag": 4.6 } }),
            observed_at,
            Some("us7000sp4x"),
        )
        .await
        .expect("first insert");
    let second = repo
        .insert_event(
            stream_id,
            json!({ "properties": { "mag": 4.8 } }),
            observed_at,
            Some("us7000sp4x"),
        )
        .await
        .expect("second insert");

    assert_eq!(first, InsertOutcome::Inserted);
    assert_eq!(second, InsertOutcome::Updated);
    let row: (i64, Value) = sqlx::query_as(
        "SELECT count(*) OVER () AS total, payload FROM stream_events WHERE dedup_key = $1",
    )
    .bind("us7000sp4x")
    .fetch_one(&pool)
    .await
    .expect("dedup row");
    assert_eq!(row.0, 1);
    assert_eq!(row.1["properties"]["mag"], json!(4.8));
}

#[tokio::test]
#[ignore]
async fn create_rejects_link_local_feed_urls_without_insert() {
    let (base, pool) = spawn_app().await;
    let workspace_id = default_workspace_id(&pool).await;

    for raw in [
        "http://169.254.169.254/latest/meta-data",
        "https://169.254.169.254/",
        "http://169.254.10.10/",
        "http://[fe80::1]/",
        "https://[fe80::1]/",
    ] {
        let resp = bearer(client().post(format!(
            "{base}/api/v1/workspaces/{workspace_id}/connectors"
        )))
        .json(&json!({
            "kind": "rust_native",
            "name": "geojson-usgs",
            "config": geojson_config(raw)
        }))
        .send()
        .await
        .expect("create connector response");
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY, "{raw}");
    }

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM connectors")
        .fetch_one(&pool)
        .await
        .expect("connector count");
    assert_eq!(count, 0);
}

async fn create_geojson_connector(base: &str, workspace_id: Uuid, config: Value) -> StatusCode {
    let resp = bearer(client().post(format!(
        "{base}/api/v1/workspaces/{workspace_id}/connectors"
    )))
    .json(&json!({ "kind": "rust_native", "name": "geojson-usgs", "config": config }))
    .send()
    .await
    .expect("create connector response");
    resp.status()
}

/// AC-4: a feature whose type is outside `type_filter.allow` is excluded.
#[tokio::test]
#[ignore]
async fn type_filter_excludes_non_matching_features() {
    let (base, pool) = spawn_app().await;
    let workspace_id = default_workspace_id(&pool).await;
    let feed = MockServer::start().await;
    mount_feed(
        &feed,
        json!({
            "type": "FeatureCollection",
            "features": [
                { "type": "Feature", "id": "eq1",
                  "geometry": { "type": "Point", "coordinates": [-122.1, 38.2, 7.0] },
                  "properties": { "mag": 4.6, "type": "earthquake", "time": 1779991039445_i64 } },
                { "type": "Feature", "id": "qb1",
                  "geometry": { "type": "Point", "coordinates": [-121.0, 37.0, 2.0] },
                  "properties": { "mag": 1.2, "type": "quarry blast", "time": 1779991039446_i64 } }
            ]
        }),
    )
    .await;

    let status = create_geojson_connector(
        &base,
        workspace_id,
        geojson_config(&format!("{}/feed", feed.uri())),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM stream_events")
        .fetch_one(&pool)
        .await
        .expect("count");
    assert_eq!(total, 1);
    let dedup: String = sqlx::query_scalar("SELECT dedup_key FROM stream_events LIMIT 1")
        .fetch_one(&pool)
        .await
        .expect("dedup key");
    assert_eq!(dedup, "eq1");
}

/// AC-13: features with missing or empty dedup keys are skipped, not written.
#[tokio::test]
#[ignore]
async fn dedup_key_edge_cases_skip_invalid_features() {
    let (base, pool) = spawn_app().await;
    let workspace_id = default_workspace_id(&pool).await;
    let feed = MockServer::start().await;
    mount_feed(
        &feed,
        json!({
            "type": "FeatureCollection",
            "features": [
                { "type": "Feature", "id": "good1",
                  "geometry": { "type": "Point", "coordinates": [-122.1, 38.2, 7.0] },
                  "properties": { "mag": 4.6, "type": "earthquake", "time": 1779991039445_i64 } },
                { "type": "Feature",
                  "geometry": { "type": "Point", "coordinates": [-122.2, 38.3, 7.0] },
                  "properties": { "mag": 4.7, "type": "earthquake", "time": 1779991039446_i64 } },
                { "type": "Feature", "id": "",
                  "geometry": { "type": "Point", "coordinates": [-122.3, 38.4, 7.0] },
                  "properties": { "mag": 4.8, "type": "earthquake", "time": 1779991039447_i64 } }
            ]
        }),
    )
    .await;

    let status = create_geojson_connector(
        &base,
        workspace_id,
        geojson_config(&format!("{}/feed", feed.uri())),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM stream_events")
        .fetch_one(&pool)
        .await
        .expect("count");
    assert_eq!(total, 1);
    let dedup: String = sqlx::query_scalar("SELECT dedup_key FROM stream_events LIMIT 1")
        .fetch_one(&pool)
        .await
        .expect("dedup key");
    assert_eq!(dedup, "good1");
}

/// AC-5: a structurally distinct feed (different items_pointer, rfc3339 timestamps,
/// different field mappings) ingests via config alone — no new connector code.
#[tokio::test]
#[ignore]
async fn second_distinct_feed_ingests_by_config_only() {
    let (base, pool) = spawn_app().await;
    let workspace_id = default_workspace_id(&pool).await;
    let feed = MockServer::start().await;
    mount_feed(
        &feed,
        json!({
            "items": [
                { "eventId": "a1", "lon": -120.0, "lat": 36.0, "severity": "high",
                  "issued": "2026-05-28T17:00:00Z" }
            ]
        }),
    )
    .await;

    let config = json!({
        "kind": "geojson_poll",
        "feed_url": format!("{}/feed", feed.uri()),
        "stream_name": "alerts",
        "items_pointer": "/items",
        "observed_at_pointer": "/issued",
        "observed_at_format": "rfc3339",
        "dedup_pointer": "/eventId",
        "view_config": {
            "lon_pointer": "/lon",
            "lat_pointer": "/lat",
            "property_fields": [{ "pointer": "/severity", "name": "severity" }]
        }
    });

    let status = create_geojson_connector(&base, workspace_id, config).await;
    assert_eq!(status, StatusCode::OK);

    let row: (String, Value) =
        sqlx::query_as("SELECT dedup_key, payload FROM stream_events LIMIT 1")
            .fetch_one(&pool)
            .await
            .expect("stream event row");
    assert_eq!(row.0, "a1");
    assert_eq!(row.1["severity"], json!("high"));
}
