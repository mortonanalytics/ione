/// Workspace metadata PATCH integration tests (Slice 3 — rule install path).
///
/// Proves the load-bearing Epicenter contract: a rule installed via
/// `PATCH /api/v1/workspaces/:id` lands in `workspaces.metadata.rules` and is
/// subsequently evaluated by the scheduler tick, producing a signal.
///
/// Prerequisites:
///   docker compose up -d postgres
///   DATABASE_URL=postgres://ione:ione@localhost:5433/ione
///
/// Run:
///   DATABASE_URL=... cargo test --test phase_workspace_patch -- --ignored --test-threads=1
use std::net::SocketAddr;

use reqwest::StatusCode;
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use ione::state::AppState;

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione";
const TEST_STATIC_BEARER: &str = "phase-workspace-patch-bearer";

async fn spawn_app() -> (String, PgPool, AppState) {
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
    let (app, state) = ione::app_with_state(pool.clone()).await;
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("server error");
    });
    (format!("http://{}", addr), pool, state)
}

async fn default_workspace_id(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("SELECT id FROM workspaces WHERE name = 'Operations' LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("Operations workspace not found")
}

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

fn bearer(req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    req.bearer_auth(TEST_STATIC_BEARER)
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
        "view_config": {
            "lon_pointer": "/geometry/coordinates/0",
            "lat_pointer": "/geometry/coordinates/1",
            "property_fields": [{ "pointer": "/properties/mag", "name": "magnitude" }]
        }
    })
}

fn quake_fixture(id: &str, mag: f64) -> Value {
    json!({
        "type": "FeatureCollection",
        "features": [{
            "type": "Feature",
            "id": id,
            "geometry": { "type": "Point", "coordinates": [-122.1, 38.2, 7.0] },
            "properties": { "mag": mag, "type": "earthquake", "time": 1779991039445_i64 }
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

/// AC: PATCH merges `metadata.rules`, and a tick then evaluates that rule
/// against an ingested matching event, producing a Command signal that forces
/// an approval.
#[tokio::test]
#[ignore]
async fn patched_rule_is_evaluated_by_scheduler() {
    let (base, pool, state) = spawn_app().await;
    let workspace_id = default_workspace_id(&pool).await;

    // Ingest a magnitude-7 earthquake via the shipped geojson_poll connector
    // (create triggers an immediate poll).
    let feed = MockServer::start().await;
    mount_feed(&feed, quake_fixture("us7000sp4x", 7.1)).await;
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
    assert_eq!(
        create.status(),
        StatusCode::OK,
        "{}",
        create.text().await.unwrap()
    );
    let events: i64 = sqlx::query_scalar("SELECT count(*) FROM stream_events")
        .fetch_one(&pool)
        .await
        .expect("event count");
    assert_eq!(events, 1, "fixture must ingest exactly one quake");

    // Install the significant-quake rule via PATCH.
    let patch = bearer(client().patch(format!("{base}/api/v1/workspaces/{workspace_id}")))
        .json(&json!({
            "metadata": {
                "rules": [{
                    "stream": "earthquakes",
                    "when": "payload.properties.mag >= 6.0",
                    "severity": "command",
                    "title": "Significant earthquake"
                }]
            }
        }))
        .send()
        .await
        .expect("patch response");
    assert_eq!(
        patch.status(),
        StatusCode::OK,
        "{}",
        patch.text().await.unwrap()
    );

    // The rule must be persisted in workspaces.metadata.rules.
    let persisted: Value = sqlx::query_scalar("SELECT metadata FROM workspaces WHERE id = $1")
        .bind(workspace_id)
        .fetch_one(&pool)
        .await
        .expect("metadata");
    assert_eq!(
        persisted["rules"][0]["when"],
        json!("payload.properties.mag >= 6.0")
    );

    // Run a scheduler tick with live model stages disabled.
    ione::services::scheduler::run_tick(&state, true)
        .await
        .expect("run_tick");

    // Exactly one Command signal, requiring approval, from the rule source.
    let row: (i64, String, bool) = sqlx::query_as(
        "SELECT count(*) OVER (), title, approval_required
         FROM signals WHERE workspace_id = $1 AND source = 'rule'::signal_source",
    )
    .bind(workspace_id)
    .fetch_one(&pool)
    .await
    .expect("signal row");
    assert_eq!(row.0, 1, "exactly one rule signal expected");
    assert_eq!(row.1, "Significant earthquake");
    assert!(row.2, "command-severity rule signal must force approval");
}

/// AC: PATCH shallow-merges — keys absent from the patch body are preserved.
#[tokio::test]
#[ignore]
async fn patch_preserves_sibling_metadata_keys() {
    let (base, pool, _state) = spawn_app().await;
    let workspace_id = default_workspace_id(&pool).await;

    let first = bearer(client().patch(format!("{base}/api/v1/workspaces/{workspace_id}")))
        .json(&json!({ "metadata": { "product": "epicenter" } }))
        .send()
        .await
        .expect("first patch");
    assert_eq!(first.status(), StatusCode::OK);

    let second = bearer(client().patch(format!("{base}/api/v1/workspaces/{workspace_id}")))
        .json(&json!({ "metadata": { "default_map_center": [-122.0, 38.0] } }))
        .send()
        .await
        .expect("second patch");
    assert_eq!(second.status(), StatusCode::OK);

    let metadata: Value = sqlx::query_scalar("SELECT metadata FROM workspaces WHERE id = $1")
        .bind(workspace_id)
        .fetch_one(&pool)
        .await
        .expect("metadata");
    assert_eq!(metadata["product"], json!("epicenter"));
    assert_eq!(metadata["default_map_center"], json!([-122.0, 38.0]));
}

/// AC: PATCH against a workspace in another org is scoped to 404 and writes nothing.
#[tokio::test]
#[ignore]
async fn patch_is_org_scoped() {
    let (base, pool, _state) = spawn_app().await;

    let other_org: Uuid =
        sqlx::query_scalar("INSERT INTO organizations (name) VALUES ('Other') RETURNING id")
            .fetch_one(&pool)
            .await
            .expect("insert org");
    let other_ws: Uuid = sqlx::query_scalar(
        "INSERT INTO workspaces (org_id, name, domain, lifecycle)
         VALUES ($1, 'Foreign', 'test', 'continuous'::workspace_lifecycle) RETURNING id",
    )
    .bind(other_org)
    .fetch_one(&pool)
    .await
    .expect("insert workspace");

    let resp = bearer(client().patch(format!("{base}/api/v1/workspaces/{other_ws}")))
        .json(&json!({ "metadata": { "product": "intruder" } }))
        .send()
        .await
        .expect("patch response");
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    let metadata: Value = sqlx::query_scalar("SELECT metadata FROM workspaces WHERE id = $1")
        .bind(other_ws)
        .fetch_one(&pool)
        .await
        .expect("metadata");
    assert!(metadata.get("product").is_none());
}

/// AC: a non-object metadata body is rejected with 400.
#[tokio::test]
#[ignore]
async fn patch_rejects_non_object_metadata() {
    let (base, pool, _state) = spawn_app().await;
    let workspace_id = default_workspace_id(&pool).await;

    let resp = bearer(client().patch(format!("{base}/api/v1/workspaces/{workspace_id}")))
        .json(&json!({ "metadata": "not-an-object" }))
        .send()
        .await
        .expect("patch response");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}
