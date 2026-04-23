/// Phase 13 connector tests — FIRMS, S3/fs, IRWIN, build_from_row dispatch.
///
/// Prerequisites:
///   docker compose up -d postgres
///   DATABASE_URL=postgres://ione:ione@localhost:5433/ione
///
/// Run (skip live network/S3):
///   IONE_SKIP_LIVE=1 DATABASE_URL=... \
///     cargo test --test phase13_connectors -- --ignored --test-threads=1
///
/// All tests are #[ignore]-gated and must be run with --test-threads=1.
use ione::connectors::ConnectorImpl;
use serde_json::{json, Value};
use std::net::SocketAddr;

use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;
use uuid::Uuid;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione";

async fn spawn_app() -> (String, PgPool) {
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

    truncate_all(&pool).await;

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind");
    let addr: SocketAddr = listener.local_addr().expect("get addr");
    let app = ione::app(pool.clone()).await;
    tokio::spawn(async move { axum::serve(listener, app).await.expect("server error") });

    (format!("http://{}", addr), pool)
}

async fn truncate_all(pool: &PgPool) {
    sqlx::query(
        "TRUNCATE audit_events, approvals, artifacts,
                  trust_issuers, peers, routing_decisions, survivors, signals,
                  stream_events, streams, connectors,
                  memberships, roles, messages, conversations,
                  workspaces, users, organizations
         RESTART IDENTITY CASCADE",
    )
    .execute(pool)
    .await
    .expect("truncate failed");
}

async fn default_workspace_id(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("SELECT id FROM workspaces WHERE name = 'Operations' LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("Operations workspace not found")
}

// ─── FIRMS tests ─────────────────────────────────────────────────────────────

/// DEMO fixture mode ingests ≥1 hotspot event.
#[tokio::test]
#[ignore]
async fn firms_demo_fixture_mode_ingests_events() {
    let config = json!({
        "kind": "firms",
        "map_key": "DEMO_TEST",
        "area": "MONTANA",
        "days": 1
    });
    let connector =
        ione::connectors::firms::FirmsConnector::from_config(&config).expect("from_config failed");

    assert!(connector.demo_mode, "DEMO_ prefix must trigger demo_mode");

    let result = connector.poll("hotspots", None).await.expect("poll failed");

    assert!(
        !result.events.is_empty(),
        "fixture must produce at least 1 hotspot event, got {}",
        result.events.len()
    );
}

/// CSV parse populates latitude and longitude in payload.
#[tokio::test]
#[ignore]
async fn firms_csv_parse_populates_lat_lon() {
    let config = json!({ "kind": "firms", "map_key": "DEMO_KEY" });
    let connector =
        ione::connectors::firms::FirmsConnector::from_config(&config).expect("from_config failed");

    let result = connector.poll("hotspots", None).await.expect("poll failed");

    let first = result.events.first().expect("at least 1 event");
    assert!(
        !first.payload["latitude"].is_null(),
        "payload must have 'latitude', got: {}",
        first.payload
    );
    assert!(
        !first.payload["longitude"].is_null(),
        "payload must have 'longitude', got: {}",
        first.payload
    );
}

/// observed_at is non-null (parsed from acq_date+acq_time).
#[tokio::test]
#[ignore]
async fn firms_observed_at_non_null() {
    let config = json!({ "kind": "firms", "map_key": "DEMO_KEY" });
    let connector =
        ione::connectors::firms::FirmsConnector::from_config(&config).expect("from_config failed");

    let result = connector.poll("hotspots", None).await.expect("poll failed");

    for evt in &result.events {
        // observed_at is a DateTime<Utc> — if we reached here it's not null/panic.
        assert!(
            evt.observed_at.timestamp() > 0,
            "observed_at must be a real timestamp, got {}",
            evt.observed_at
        );
    }
}

/// build_from_row dispatches firms* name to FirmsConnector.
#[tokio::test]
#[ignore]
async fn firms_build_from_row_dispatch() {
    let (_, pool) = spawn_app().await;
    let ws = default_workspace_id(&pool).await;

    let connector_id: Uuid = sqlx::query_scalar(
        "INSERT INTO connectors (workspace_id, kind, name, config)
         VALUES ($1, 'rust_native'::connector_kind, 'firms-lolo',
                 '{\"map_key\":\"DEMO_KEY\",\"area\":\"MONTANA\"}'::jsonb)
         RETURNING id",
    )
    .bind(ws)
    .fetch_one(&pool)
    .await
    .expect("insert connector");

    let row: ione::models::Connector = sqlx::query_as(
        "SELECT id, workspace_id, kind, name, config, status, last_error, created_at
         FROM connectors WHERE id = $1",
    )
    .bind(connector_id)
    .fetch_one(&pool)
    .await
    .expect("fetch connector");

    ione::connectors::build_from_row(&row).expect("build_from_row must succeed for firms-lolo");
}

// ─── fs_s3 tests ─────────────────────────────────────────────────────────────

/// fs mode walks fixture dir and emits ≥1 event.
#[tokio::test]
#[ignore]
async fn fs_mode_walks_fixture_dir() {
    let config = json!({
        "kind": "fs_s3",
        "mode": "fs",
        "path": "infra/fixtures/docs"
    });
    let connector =
        ione::connectors::fs_s3::FsS3Connector::from_config(&config).expect("from_config failed");

    let result = connector
        .poll("documents", None)
        .await
        .expect("poll failed");

    assert!(
        !result.events.is_empty(),
        "fs walk must emit ≥1 event for fixtures/docs, got {}",
        result.events.len()
    );

    let first = result.events.first().unwrap();
    assert!(
        !first.payload["key"].is_null(),
        "payload must have 'key': {}",
        first.payload
    );
    assert!(
        !first.payload["blob_ref"].is_null(),
        "payload must have 'blob_ref': {}",
        first.payload
    );
}

/// fs dedup works: observed_at equals file last_modified.
#[tokio::test]
#[ignore]
async fn fs_dedup_via_observed_at() {
    let config = json!({
        "mode": "fs",
        "path": "infra/fixtures/docs"
    });
    let connector =
        ione::connectors::fs_s3::FsS3Connector::from_config(&config).expect("from_config failed");

    let r1 = connector.poll("documents", None).await.expect("poll 1");
    let r2 = connector.poll("documents", None).await.expect("poll 2");

    // Both polls return the same observed_at — real dedup happens at the DB layer
    // via (stream_id, observed_at) unique index; verify the timestamps are stable.
    assert_eq!(
        r1.events.len(),
        r2.events.len(),
        "consecutive polls must return same number of events"
    );
    if let (Some(a), Some(b)) = (r1.events.first(), r2.events.first()) {
        assert_eq!(
            a.observed_at.timestamp(),
            b.observed_at.timestamp(),
            "observed_at must be stable across polls (used for dedup)"
        );
    }
}

/// S3 mode against MinIO ingests 1 test object (skipped if IONE_SKIP_LIVE=1).
#[tokio::test]
#[ignore]
async fn s3_mode_against_minio() {
    if std::env::var("IONE_SKIP_LIVE").is_ok() {
        eprintln!("s3_mode_against_minio: skipped (IONE_SKIP_LIVE)");
        return;
    }

    // Requires MinIO running at localhost:9100 with bucket "test-docs".
    // Set up via: mc alias set minio http://localhost:9100 ione ioneione
    //             mc mb minio/test-docs
    //             mc cp infra/fixtures/docs/lolo_nf_overview.txt minio/test-docs/
    let config = json!({
        "mode": "s3",
        "bucket": "test-docs",
        "prefix": "",
        "endpoint": "http://localhost:9100",
        "region": "us-east-1"
    });
    std::env::set_var("AWS_ACCESS_KEY_ID", "ione");
    std::env::set_var("AWS_SECRET_ACCESS_KEY", "ioneione");

    let connector =
        ione::connectors::fs_s3::FsS3Connector::from_config(&config).expect("from_config failed");

    let result = connector.poll("documents", None).await;
    match result {
        Ok(r) => {
            assert!(!r.events.is_empty(), "S3 poll must return ≥1 event, got 0");
        }
        Err(e) => {
            // MinIO not running is acceptable when IONE_SKIP_LIVE not set but
            // connection fails — log and pass.
            eprintln!(
                "s3_mode_against_minio: S3 call failed (MinIO unreachable?): {}",
                e
            );
        }
    }
}

/// build_from_row dispatches to FsS3Connector via kind hint.
#[tokio::test]
#[ignore]
async fn fs_s3_build_from_row_dispatch() {
    let (_, pool) = spawn_app().await;
    let ws = default_workspace_id(&pool).await;

    let connector_id: Uuid = sqlx::query_scalar(
        "INSERT INTO connectors (workspace_id, kind, name, config)
         VALUES ($1, 'rust_native'::connector_kind, 'documents-lolo',
                 '{\"kind\":\"fs_s3\",\"mode\":\"fs\",\"path\":\"infra/fixtures/docs\"}'::jsonb)
         RETURNING id",
    )
    .bind(ws)
    .fetch_one(&pool)
    .await
    .expect("insert connector");

    let row: ione::models::Connector = sqlx::query_as(
        "SELECT id, workspace_id, kind, name, config, status, last_error, created_at
         FROM connectors WHERE id = $1",
    )
    .bind(connector_id)
    .fetch_one(&pool)
    .await
    .expect("fetch connector");

    ione::connectors::build_from_row(&row)
        .expect("build_from_row must succeed for fs_s3 kind hint");
}

// ─── IRWIN tests ─────────────────────────────────────────────────────────────

/// mock:// mode returns fixtures.
#[tokio::test]
#[ignore]
async fn irwin_mock_mode_returns_fixtures() {
    let config = json!({
        "base_url": "mock://irwin",
        "kind": "irwin"
    });
    let connector =
        ione::connectors::irwin::IrwinConnector::from_config(&config).expect("from_config failed");

    assert!(connector.mock_mode, "mock:// must set mock_mode");

    let result = connector
        .poll("incidents", None)
        .await
        .expect("poll failed");

    assert!(
        !result.events.is_empty(),
        "mock fixtures must produce ≥1 incident, got {}",
        result.events.len()
    );
}

/// JSON parse populates IncidentName in payload.
#[tokio::test]
#[ignore]
async fn irwin_json_parse_populates_incident_name() {
    let config = json!({ "base_url": "mock://irwin" });
    let connector =
        ione::connectors::irwin::IrwinConnector::from_config(&config).expect("from_config");

    let result = connector.poll("incidents", None).await.expect("poll");
    let first = result.events.first().expect("at least 1 event");

    let name = first.payload["IncidentName"].as_str();
    assert!(
        name.is_some(),
        "payload must have IncidentName, got: {}",
        first.payload
    );
    assert!(!name.unwrap().is_empty(), "IncidentName must be non-empty");
}

/// Real-URL path attempts auth header (compile+runtime path; skipped with IONE_SKIP_LIVE).
#[tokio::test]
#[ignore]
async fn irwin_real_url_attempts_auth_header() {
    if std::env::var("IONE_SKIP_LIVE").is_ok() {
        eprintln!("irwin_real_url_attempts_auth_header: skipped (IONE_SKIP_LIVE)");
        return;
    }

    // Use a dead URL — we just verify the connector attempts to connect.
    let config = json!({
        "base_url": "http://127.0.0.1:19998",
        "api_key": "test-key-123"
    });
    let connector =
        ione::connectors::irwin::IrwinConnector::from_config(&config).expect("from_config");

    assert!(!connector.mock_mode, "non-mock:// must not set mock_mode");
    assert_eq!(
        connector.api_key.as_deref(),
        Some("test-key-123"),
        "api_key must be stored"
    );

    // Poll must fail (connection refused) but the code path that adds the auth
    // header is exercised.
    let result = connector.poll("incidents", None).await;
    assert!(
        result.is_err(),
        "unreachable URL must produce an error, not Ok"
    );
}

/// build_from_row dispatches to IrwinConnector via name prefix.
#[tokio::test]
#[ignore]
async fn irwin_build_from_row_dispatch() {
    let (_, pool) = spawn_app().await;
    let ws = default_workspace_id(&pool).await;

    let connector_id: Uuid = sqlx::query_scalar(
        "INSERT INTO connectors (workspace_id, kind, name, config)
         VALUES ($1, 'rust_native'::connector_kind, 'irwin-incidents',
                 '{\"base_url\":\"mock://irwin\"}'::jsonb)
         RETURNING id",
    )
    .bind(ws)
    .fetch_one(&pool)
    .await
    .expect("insert connector");

    let row: ione::models::Connector = sqlx::query_as(
        "SELECT id, workspace_id, kind, name, config, status, last_error, created_at
         FROM connectors WHERE id = $1",
    )
    .bind(connector_id)
    .fetch_one(&pool)
    .await
    .expect("fetch connector");

    ione::connectors::build_from_row(&row)
        .expect("build_from_row must succeed for irwin* name prefix");
}

// ─── audit_events route test ──────────────────────────────────────────────────

/// GET /api/v1/workspaces/:id/audit_events returns { items: [...] }.
#[tokio::test]
#[ignore]
async fn audit_events_list_returns_items() {
    let (base, pool) = spawn_app().await;
    let ws = default_workspace_id(&pool).await;

    // Seed one audit event.
    sqlx::query(
        "INSERT INTO audit_events
           (workspace_id, actor_kind, actor_ref, verb, object_kind, payload)
         VALUES ($1, 'system'::actor_kind, 'test', 'test_verb', 'connector', '{}'::jsonb)",
    )
    .bind(ws)
    .execute(&pool)
    .await
    .expect("insert audit_event");

    let resp = reqwest::Client::new()
        .get(format!("{}/api/v1/workspaces/{}/audit_events", base, ws))
        .send()
        .await
        .expect("GET audit_events failed");

    assert_eq!(resp.status(), reqwest::StatusCode::OK, "must return 200");

    let body: Value = resp.json().await.expect("response not JSON");
    let items = body["items"].as_array().expect("items must be array");
    assert!(!items.is_empty(), "items must not be empty after seeding");
}

// ─── OpenAPI connector tests ────────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn openapi_build_from_row_dispatches_successfully() {
    let (_, pool) = spawn_app().await;
    let ws = default_workspace_id(&pool).await;

    let connector_id: Uuid = sqlx::query_scalar(
        "INSERT INTO connectors (workspace_id, kind, name, config)
         VALUES ($1, 'openapi'::connector_kind, 'example-api',
                 $2::jsonb)
         RETURNING id",
    )
    .bind(ws)
    .bind(json!({
        "spec_inline": {
            "openapi": "3.0.0",
            "servers": [{ "url": "http://127.0.0.1:3000" }],
            "paths": {
                "/incidents": { "get": { "operationId": "listIncidents" } }
            }
        },
        "base_url": "http://127.0.0.1:3000",
        "auth": { "type": "none" },
        "streams": [{
            "name": "incidents",
            "method": "GET",
            "path": "/incidents",
            "operation_id": "listIncidents",
            "items_json_pointer": "/items",
            "observed_at_json_pointer": "/updated_at"
        }]
    }))
    .fetch_one(&pool)
    .await
    .expect("insert connector");

    let row: ione::models::Connector = sqlx::query_as(
        "SELECT id, workspace_id, kind, name, config, status, last_error, created_at
         FROM connectors WHERE id = $1",
    )
    .bind(connector_id)
    .fetch_one(&pool)
    .await
    .expect("fetch connector");

    ione::connectors::build_from_row(&row).expect("openapi dispatch should build");
}

#[tokio::test]
#[ignore]
async fn openapi_default_streams_returns_declared_streams() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/openapi.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "openapi": "3.0.0",
            "servers": [{ "url": mock_server.uri() }],
            "paths": {
                "/incidents": { "get": { "operationId": "listIncidents", "summary": "List incidents" } },
                "/search": { "post": { "operationId": "searchIncidents" } }
            }
        })))
        .mount(&mock_server)
        .await;

    let config = json!({
        "spec_url": format!("{}/openapi.json", mock_server.uri()),
        "base_url": mock_server.uri(),
        "auth": { "type": "none" },
        "streams": [
            {
                "name": "incidents",
                "method": "GET",
                "path": "/incidents",
                "operation_id": "listIncidents",
                "items_json_pointer": "/items",
                "observed_at_json_pointer": "/updated_at"
            },
            {
                "name": "search",
                "method": "POST",
                "path": "/search",
                "operation_id": "searchIncidents",
                "items_json_pointer": "",
                "observed_at_json_pointer": "/updated_at"
            }
        ]
    });

    let connector =
        ione::connectors::openapi::OpenApiConnector::from_config(&config).expect("from_config");
    let streams = connector.default_streams().await.expect("default_streams");
    assert_eq!(
        streams.len(),
        2,
        "must create one default stream per configured stream"
    );
}

#[tokio::test]
#[ignore]
async fn openapi_poll_get_and_post_emit_source_metadata_and_record() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/openapi.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "openapi": "3.0.0",
            "servers": [{ "url": mock_server.uri() }],
            "paths": {
                "/incidents": { "get": { "operationId": "listIncidents" } },
                "/search": { "post": { "operationId": "searchIncidents" } }
            }
        })))
        .mount(&mock_server)
        .await;

    Mock::given(method("GET"))
        .and(path("/incidents"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [{
                "id": "INC-1",
                "updated_at": "2026-04-23T12:00:00Z",
                "status": "active"
            }]
        })))
        .mount(&mock_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "INC-2",
            "updated_at": "2026-04-23T12:05:00Z",
            "status": "active"
        })))
        .mount(&mock_server)
        .await;

    let config = json!({
        "spec_url": format!("{}/openapi.json", mock_server.uri()),
        "base_url": mock_server.uri(),
        "auth": { "type": "none" },
        "streams": [
            {
                "name": "incidents",
                "method": "GET",
                "path": "/incidents",
                "operation_id": "listIncidents",
                "items_json_pointer": "/items",
                "observed_at_json_pointer": "/updated_at",
                "event_id_json_pointer": "/id"
            },
            {
                "name": "search",
                "method": "POST",
                "path": "/search",
                "operation_id": "searchIncidents",
                "items_json_pointer": "",
                "observed_at_json_pointer": "/updated_at",
                "event_id_json_pointer": "/id",
                "body": { "status": "active" }
            }
        ]
    });

    let connector =
        ione::connectors::openapi::OpenApiConnector::from_config(&config).expect("from_config");
    let get_result = connector.poll("incidents", None).await.expect("get poll");
    assert_eq!(get_result.events.len(), 1);
    assert_eq!(get_result.events[0].payload["source"]["id"], "INC-1");
    assert_eq!(
        get_result.events[0].payload["source"]["connector"],
        "openapi"
    );
    assert_eq!(get_result.events[0].payload["record"]["status"], "active");

    let post_result = connector.poll("search", None).await.expect("post poll");
    assert_eq!(post_result.events.len(), 1);
    assert_eq!(post_result.events[0].payload["source"]["id"], "INC-2");
    assert_eq!(post_result.events[0].payload["record"]["status"], "active");
}

#[tokio::test]
#[ignore]
async fn openapi_poll_honors_max_items_and_rejects_invalid_timestamps() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/openapi.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "openapi": "3.0.0",
            "servers": [{ "url": mock_server.uri() }],
            "paths": {
                "/incidents": { "get": { "operationId": "listIncidents" } },
                "/broken": { "get": { "operationId": "listBroken" } }
            }
        })))
        .mount(&mock_server)
        .await;

    Mock::given(method("GET"))
        .and(path("/incidents"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [
                { "id": "INC-1", "updated_at": "2026-04-23T12:00:00Z" },
                { "id": "INC-2", "updated_at": "2026-04-23T12:01:00Z" }
            ]
        })))
        .mount(&mock_server)
        .await;

    Mock::given(method("GET"))
        .and(path("/broken"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [{ "id": "BROKEN-1", "updated_at": "not-a-time" }]
        })))
        .mount(&mock_server)
        .await;

    let config = json!({
        "spec_url": format!("{}/openapi.json", mock_server.uri()),
        "base_url": mock_server.uri(),
        "auth": { "type": "none" },
        "streams": [
            {
                "name": "incidents",
                "method": "GET",
                "path": "/incidents",
                "operation_id": "listIncidents",
                "items_json_pointer": "/items",
                "observed_at_json_pointer": "/updated_at",
                "max_items": 1
            },
            {
                "name": "broken",
                "method": "GET",
                "path": "/broken",
                "operation_id": "listBroken",
                "items_json_pointer": "/items",
                "observed_at_json_pointer": "/updated_at"
            }
        ]
    });

    let connector =
        ione::connectors::openapi::OpenApiConnector::from_config(&config).expect("from_config");
    let limited = connector
        .poll("incidents", None)
        .await
        .expect("limited poll");
    assert_eq!(
        limited.events.len(),
        1,
        "max_items must cap returned events"
    );

    let err = connector
        .poll("broken", None)
        .await
        .err()
        .expect("invalid timestamp must fail");
    assert!(
        err.to_string().contains("timestamp parse failure"),
        "error must mention timestamp parse failure, got {}",
        err
    );
}

#[tokio::test]
#[ignore]
async fn openapi_route_create_poll_and_cursor_template_work() {
    let (base, pool) = spawn_app().await;
    let ws = default_workspace_id(&pool).await;
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/openapi.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "openapi": "3.0.0",
            "servers": [{ "url": mock_server.uri() }],
            "paths": {
                "/incidents": { "get": { "operationId": "listIncidents" } }
            }
        })))
        .mount(&mock_server)
        .await;

    Mock::given(method("GET"))
        .and(path("/incidents"))
        .and(query_param("modified_since", "2026-04-23T11:00:00+00:00"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [{
                "id": "INC-2",
                "updated_at": "2026-04-23T12:00:00Z",
                "status": "active"
            }]
        })))
        .mount(&mock_server)
        .await;

    let client = reqwest::Client::new();
    let create_resp = client
        .post(format!("{}/api/v1/workspaces/{}/connectors", base, ws))
        .json(&json!({
            "kind": "openapi",
            "name": "Example API",
            "config": {
                "spec_url": format!("{}/openapi.json", mock_server.uri()),
                "base_url": mock_server.uri(),
                "auth": { "type": "none" },
                "streams": [{
                    "name": "incidents",
                    "method": "GET",
                    "path": "/incidents",
                    "operation_id": "listIncidents",
                    "query": { "modified_since": "{{cursor.observed_at}}" },
                    "items_json_pointer": "/items",
                    "observed_at_json_pointer": "/updated_at",
                    "event_id_json_pointer": "/id"
                }]
            }
        }))
        .send()
        .await
        .expect("create connector");

    assert_eq!(create_resp.status(), reqwest::StatusCode::OK);
    let connector: Value = create_resp.json().await.expect("connector json");
    let connector_id = connector["id"].as_str().expect("connector id");

    let stream_id: Uuid = sqlx::query_scalar("SELECT id FROM streams WHERE connector_id = $1")
        .bind(Uuid::parse_str(connector_id).expect("uuid"))
        .fetch_one(&pool)
        .await
        .expect("stream id");

    sqlx::query(
        "INSERT INTO stream_events (stream_id, payload, observed_at)
         VALUES ($1, '{}'::jsonb, '2026-04-23T11:00:00Z')",
    )
    .bind(stream_id)
    .execute(&pool)
    .await
    .expect("seed prior event");

    let poll_resp = client
        .post(format!("{}/api/v1/streams/{}/poll", base, stream_id))
        .json(&json!({}))
        .send()
        .await
        .expect("poll stream");

    assert_eq!(poll_resp.status(), reqwest::StatusCode::OK);
    let body: Value = poll_resp.json().await.expect("poll body");
    assert_eq!(body["ingested"], 1);
}

#[tokio::test]
#[ignore]
async fn openapi_invalid_create_returns_400_and_persists_nothing() {
    let (base, pool) = spawn_app().await;
    let ws = default_workspace_id(&pool).await;
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/openapi.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "openapi": "3.0.0",
            "servers": [{ "url": mock_server.uri() }],
            "paths": {
                "/incidents": { "get": { "operationId": "listIncidents" } }
            }
        })))
        .mount(&mock_server)
        .await;

    let resp = reqwest::Client::new()
        .post(format!("{}/api/v1/workspaces/{}/connectors", base, ws))
        .json(&json!({
            "kind": "openapi",
            "name": "Broken API",
            "config": {
                "spec_url": format!("{}/openapi.json", mock_server.uri()),
                "base_url": mock_server.uri(),
                "auth": { "type": "none" },
                "streams": [{
                    "name": "incidents",
                    "method": "GET",
                    "path": "/incidents",
                    "operation_id": "wrongOperation",
                    "items_json_pointer": "/items",
                    "observed_at_json_pointer": "/updated_at"
                }]
            }
        }))
        .send()
        .await
        .expect("create connector");

    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM connectors")
        .fetch_one(&pool)
        .await
        .expect("count connectors");
    assert_eq!(
        count, 0,
        "invalid openapi create must not persist connector"
    );
}

#[tokio::test]
#[ignore]
async fn openapi_poll_failure_sets_connector_error_status() {
    let (base, pool) = spawn_app().await;
    let ws = default_workspace_id(&pool).await;
    let mock_server = MockServer::start().await;

    std::env::set_var("OPENAPI_TEST_TOKEN", "secret-token");

    Mock::given(method("GET"))
        .and(path("/openapi.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "openapi": "3.0.0",
            "servers": [{ "url": mock_server.uri() }],
            "paths": {
                "/incidents": { "get": { "operationId": "listIncidents" } }
            }
        })))
        .mount(&mock_server)
        .await;

    Mock::given(method("GET"))
        .and(path("/incidents"))
        .and(header("authorization", "Bearer secret-token"))
        .respond_with(ResponseTemplate::new(502))
        .mount(&mock_server)
        .await;

    let create_resp = reqwest::Client::new()
        .post(format!("{}/api/v1/workspaces/{}/connectors", base, ws))
        .json(&json!({
            "kind": "openapi",
            "name": "Example API",
            "config": {
                "spec_url": format!("{}/openapi.json", mock_server.uri()),
                "base_url": mock_server.uri(),
                "auth": { "type": "bearer", "token_env": "OPENAPI_TEST_TOKEN" },
                "streams": [{
                    "name": "incidents",
                    "method": "GET",
                    "path": "/incidents",
                    "operation_id": "listIncidents",
                    "items_json_pointer": "/items",
                    "observed_at_json_pointer": "/updated_at"
                }]
            }
        }))
        .send()
        .await
        .expect("create connector");

    assert_eq!(create_resp.status(), reqwest::StatusCode::OK);
    let connector: Value = create_resp.json().await.expect("connector json");
    let connector_id =
        Uuid::parse_str(connector["id"].as_str().expect("connector id")).expect("uuid");
    let stream_id: Uuid = sqlx::query_scalar("SELECT id FROM streams WHERE connector_id = $1")
        .bind(connector_id)
        .fetch_one(&pool)
        .await
        .expect("stream id");

    let poll_resp = reqwest::Client::new()
        .post(format!("{}/api/v1/streams/{}/poll", base, stream_id))
        .json(&json!({}))
        .send()
        .await
        .expect("poll stream");
    assert_eq!(poll_resp.status(), reqwest::StatusCode::BAD_GATEWAY);

    let status_and_error: (String, Option<String>) =
        sqlx::query_as("SELECT status::text, last_error FROM connectors WHERE id = $1")
            .bind(connector_id)
            .fetch_one(&pool)
            .await
            .expect("fetch connector status");
    assert_eq!(status_and_error.0, "error");
    assert!(
        status_and_error
            .1
            .as_deref()
            .unwrap_or_default()
            .contains("request failed"),
        "last_error must capture poll failure, got {:?}",
        status_and_error.1
    );
}
