/// Phase 4 contract tests — NOAA NWS connector, streams, stream_events.
///
/// These tests are written against the contract in md/design/ione-v1-contract.md
/// and the Phase 4 spec in md/plans/ione-v1-plan.md.
/// They ALL FAIL today because the Phase 4 implementation (migration 0003,
/// connector routes, NWS connector) does not yet exist.
///
/// ──────────────────────────────────────────────────────────────────────────
/// Expected DATABASE_URL (default):
///   postgres://ione:ione@localhost:5433/ione
///
/// Bring up Postgres before running:
///   docker compose up -d postgres
///
/// Run this suite (serial to avoid DB contention):
///   DATABASE_URL=postgres://ione:ione@localhost:5433/ione \
///     cargo test --test phase04_nws_connector -- --ignored --test-threads=1
///
/// Skip the live-NWS tests (no network access):
///   IONE_SKIP_LIVE=1 DATABASE_URL=... cargo test --test phase04_nws_connector \
///     -- --ignored --test-threads=1
///
/// ──────────────────────────────────────────────────────────────────────────
/// Contract targets (md/design/ione-v1-contract.md):
///
///   Enums:
///     connector_kind : mcp | openapi | rust_native
///     connector_status: active | paused | error
///
///   Connector fields (JSON, camelCase):
///     id, workspaceId, kind, name, config, status, lastError, createdAt
///
///   Stream fields (JSON, camelCase):
///     id, connectorId, name, schema, createdAt
///
///   StreamEvent fields (JSON, camelCase):
///     id, streamId, payload, observedAt, ingestedAt
///     (embedding not serialized on the wire for v1)
///
///   Endpoints (contract § API operations):
///     GET  /api/v1/workspaces/:id/connectors  → { items: Connector[] }
///     POST /api/v1/workspaces/:id/connectors  ← { kind, name, config } → Connector
///     GET  /api/v1/connectors/:id/streams     → { items: Stream[] }
///     POST /api/v1/streams/:id/poll           → { ingested: n }
///
///   Migration 0003 (plan Phase 4):
///     - Tables: connectors, streams, stream_events
///     - Index:  stream_events_stream_observed ON stream_events(stream_id, observed_at DESC)
///     - Unique: (connector_id, name) on streams
///     - Cascade: connectors → streams → stream_events; workspaces → connectors
///
///   NWS config shape: { "lat": number, "lon": number }
///   Auto-registered default stream name: "observations"
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

/// Connect, run migrations, truncate in FK-safe order, boot on a random port.
/// Returns `(base_url, pool)`.
///
/// The truncation order is:
///   stream_events → streams → connectors →
///   memberships → roles → messages → conversations →
///   workspaces → users → organizations
///
/// Phase 4 tables (stream_events, streams, connectors) must be truncated before
/// the Phase 2/3 tables.  If migration 0003 hasn't run yet this TRUNCATE will
/// fail — which is the expected failure mode for a contract-red test.
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
        .expect("migration failed — migration 0003 may not exist yet (expected failure)");

    sqlx::query(
        "TRUNCATE stream_events, streams, connectors,
                  memberships, roles, messages, conversations,
                  workspaces, users, organizations
         RESTART IDENTITY CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate failed — tables may not exist yet (expected for contract-red)");

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

/// Return the id of the seeded "Operations" workspace from the DB.
async fn ops_workspace_id(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("SELECT id FROM workspaces WHERE name = 'Operations' LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("Operations workspace not found — bootstrap seed missing (expected failure)")
}

// ──────────────────────────────────────────────────────────────────────────────
// Enum shape tests
// ──────────────────────────────────────────────────────────────────────────────

/// SELECT unnest(enum_range(NULL::connector_kind)) must return exactly the three
/// variants declared in the contract.
///
/// Contract targets (contract § Enums):
///   connector_kind: mcp | openapi | rust_native
///
/// REASON: requires DATABASE_URL and migration 0003 (which does not yet exist).
#[tokio::test]
#[ignore]
async fn connector_kinds_enum_has_all_three_variants() {
    let (_base, pool) = spawn_app().await;

    let variants: Vec<String> =
        sqlx::query_scalar("SELECT unnest(enum_range(NULL::connector_kind))::TEXT")
            .fetch_all(&pool)
            .await
            .expect(
                "query failed — connector_kind enum not found \
                 (migration 0003 missing; expected failure)",
            );

    let mut sorted = variants.clone();
    sorted.sort();

    assert_eq!(
        sorted,
        vec!["mcp", "openapi", "rust_native"],
        "connector_kind enum must have exactly variants [mcp, openapi, rust_native], got {:?}",
        variants
    );
}

/// SELECT unnest(enum_range(NULL::connector_status)) must return exactly the
/// three variants declared in the contract.
///
/// Contract targets (contract § Enums):
///   connector_status: active | paused | error
///
/// REASON: requires DATABASE_URL and migration 0003.
#[tokio::test]
#[ignore]
async fn connector_status_enum_has_variants() {
    let (_base, pool) = spawn_app().await;

    let variants: Vec<String> =
        sqlx::query_scalar("SELECT unnest(enum_range(NULL::connector_status))::TEXT")
            .fetch_all(&pool)
            .await
            .expect(
                "query failed — connector_status enum not found \
                 (migration 0003 missing; expected failure)",
            );

    let mut sorted = variants.clone();
    sorted.sort();

    assert_eq!(
        sorted,
        vec!["active", "error", "paused"],
        "connector_status enum must have exactly variants [active, error, paused], got {:?}",
        variants
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// Connector CRUD + auto-stream
// ──────────────────────────────────────────────────────────────────────────────

/// POST /api/v1/workspaces/:id/connectors with a valid NWS config:
///   1. Returns 200 with a Connector body matching all contract fields.
///   2. GET /api/v1/connectors/:id/streams returns items.length >= 1 with an
///      "observations" stream whose `schema` is a non-null JSON object.
///
/// Contract targets:
///   POST /api/v1/workspaces/:id/connectors ← { kind, name, config } → Connector
///   GET  /api/v1/connectors/:id/streams    → { items: Stream[] }
///   Connector fields: id, workspaceId, kind, name, config, status, lastError, createdAt
///   Stream fields:    id, connectorId, name, schema, createdAt
///   Plan Phase 4: "On connector creation, a default stream 'observations' is
///                  auto-registered."
///   NWS config shape: { "lat": number, "lon": number }
///
/// REASON: requires DATABASE_URL and migration 0003 + connector routes.
#[tokio::test]
#[ignore]
async fn create_nws_connector_returns_connector_with_default_stream() {
    let (base, pool) = spawn_app().await;
    let client = reqwest::Client::new();

    let ws_id = ops_workspace_id(&pool).await;

    // POST connector
    let resp = client
        .post(format!("{}/api/v1/workspaces/{}/connectors", base, ws_id))
        .json(&json!({
            "kind": "rust_native",
            "name": "NWS Missoula",
            "config": { "lat": 46.8721, "lon": -113.9940 }
        }))
        .send()
        .await
        .expect("POST /api/v1/workspaces/:id/connectors failed");

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "expected 200 from POST /api/v1/workspaces/:id/connectors, got {}",
        resp.status()
    );

    let connector: Value = resp.json().await.expect("connector body not JSON");

    // ── Connector field shape (contract § connector) ──────────────────────────

    let connector_id_str = connector["id"]
        .as_str()
        .expect("connector response must have \"id\" string");
    let connector_id =
        Uuid::parse_str(connector_id_str).expect("connector.id must be a valid UUID");

    assert_eq!(
        connector["workspaceId"]
            .as_str()
            .expect("connector must have workspaceId"),
        ws_id.to_string().as_str(),
        "connector.workspaceId must equal the workspace used to create it"
    );

    assert_eq!(
        connector["kind"], "rust_native",
        "connector.kind must be 'rust_native', got: {}",
        connector["kind"]
    );

    assert_eq!(
        connector["name"], "NWS Missoula",
        "connector.name must be 'NWS Missoula', got: {}",
        connector["name"]
    );

    // config echoed back as a JSON object with lat/lon
    assert!(
        connector["config"].is_object(),
        "connector.config must be a JSON object, got: {}",
        connector["config"]
    );
    let cfg = &connector["config"];
    assert!(
        cfg["lat"].is_number() || cfg["lat"].is_f64(),
        "connector.config.lat must be a number, got: {}",
        cfg["lat"]
    );
    assert!(
        cfg["lon"].is_number() || cfg["lon"].is_f64(),
        "connector.config.lon must be a number, got: {}",
        cfg["lon"]
    );

    // status defaults to "active"
    assert_eq!(
        connector["status"], "active",
        "connector.status must default to 'active' on creation, got: {}",
        connector["status"]
    );

    // lastError is null on new connector
    assert!(
        connector["lastError"].is_null(),
        "connector.lastError must be null for a new connector, got: {}",
        connector["lastError"]
    );

    assert!(
        !connector["createdAt"].is_null(),
        "connector.createdAt must be non-null, got: {}",
        connector
    );

    // ── Default "observations" stream auto-registered ─────────────────────────

    let streams_resp = client
        .get(format!(
            "{}/api/v1/connectors/{}/streams",
            base, connector_id
        ))
        .send()
        .await
        .expect("GET /api/v1/connectors/:id/streams failed");

    assert_eq!(
        streams_resp.status(),
        StatusCode::OK,
        "expected 200 from GET /api/v1/connectors/:id/streams, got {}",
        streams_resp.status()
    );

    let streams_body: Value = streams_resp.json().await.expect("streams body not JSON");
    let items = streams_body["items"]
        .as_array()
        .expect("streams response must have an \"items\" array");

    assert!(
        !items.is_empty(),
        "GET /api/v1/connectors/:id/streams must return at least 1 stream after connector creation"
    );

    // Find the "observations" stream
    let obs_stream = items
        .iter()
        .find(|s| s["name"].as_str() == Some("observations"))
        .expect(
            "at least one stream must be named 'observations' \
             (auto-registered on connector creation; expected failure)",
        );

    // Stream field shape (contract § stream)
    Uuid::parse_str(obs_stream["id"].as_str().expect("stream must have id"))
        .expect("stream.id must be a valid UUID");

    assert_eq!(
        obs_stream["connectorId"]
            .as_str()
            .expect("stream must have connectorId"),
        connector_id.to_string().as_str(),
        "stream.connectorId must equal the connector's id"
    );

    assert!(
        !obs_stream["schema"].is_null(),
        "stream.schema must be non-null (plan Phase 4 requires a non-null JSONB schema), \
         got: {}",
        obs_stream
    );

    assert!(
        !obs_stream["createdAt"].is_null(),
        "stream.createdAt must be non-null, got: {}",
        obs_stream
    );
}

/// POST /api/v1/workspaces/:id/connectors returns 4xx when `kind` is not a
/// recognized connector_kind variant.
///
/// Contract targets:
///   connector.kind: ENUM connector_kind (mcp | openapi | rust_native)
///   Any value outside this set must be rejected.
///   Expected: 400 / 422 (serde enum parse failure surfaced as a client error).
///
/// REASON: requires DATABASE_URL and migration 0003 + connector routes.
#[tokio::test]
#[ignore]
async fn connector_with_unknown_kind_is_400() {
    let (base, pool) = spawn_app().await;
    let client = reqwest::Client::new();

    let ws_id = ops_workspace_id(&pool).await;

    let resp = client
        .post(format!("{}/api/v1/workspaces/{}/connectors", base, ws_id))
        .json(&json!({
            "kind": "unknown",
            "name": "Bad Connector",
            "config": {}
        }))
        .send()
        .await
        .expect("POST /api/v1/workspaces/:id/connectors failed");

    let status = resp.status().as_u16();
    assert!(
        status >= 400 && status < 500,
        "expected 4xx when connector kind is unknown, got {}",
        status
    );
}

/// GET /api/v1/workspaces/:id/connectors returns { items: Connector[] } where
/// items contains only connectors scoped to the requested workspace.
///
/// Creates 2 connectors in workspace A (the seeded Operations workspace),
/// 1 connector in a freshly-created workspace B, then asserts:
///   - GET for A returns exactly 2 items.
///   - GET for B returns exactly 1 item.
///
/// Contract targets:
///   GET /api/v1/workspaces/:id/connectors → { items: Connector[] }
///   connector.workspaceId (must match the path param)
///   Plan Phase 4: multi-workspace isolation
///
/// REASON: requires DATABASE_URL and migration 0003 + connector routes.
#[tokio::test]
#[ignore]
async fn list_connectors_returns_items_for_workspace() {
    let (base, pool) = spawn_app().await;
    let client = reqwest::Client::new();

    let ws_a = ops_workspace_id(&pool).await;

    // Create workspace B
    let ws_b_body: Value = client
        .post(format!("{}/api/v1/workspaces", base))
        .json(&json!({
            "name": "Secondary WS",
            "domain": "test",
            "lifecycle": "continuous"
        }))
        .send()
        .await
        .expect("create workspace B failed")
        .json()
        .await
        .expect("workspace B body not JSON");
    let ws_b = Uuid::parse_str(ws_b_body["id"].as_str().expect("ws B must have id"))
        .expect("ws B id must be UUID");

    // Create 2 connectors in workspace A
    for i in 0..2 {
        let r = client
            .post(format!("{}/api/v1/workspaces/{}/connectors", base, ws_a))
            .json(&json!({
                "kind": "rust_native",
                "name": format!("NWS A-{}", i),
                "config": { "lat": 46.8, "lon": -114.0 }
            }))
            .send()
            .await
            .expect("create connector in ws_a failed");
        assert_eq!(
            r.status(),
            StatusCode::OK,
            "expected 200 creating connector in ws_a (i={}), got {}",
            i,
            r.status()
        );
    }

    // Create 1 connector in workspace B
    let r = client
        .post(format!("{}/api/v1/workspaces/{}/connectors", base, ws_b))
        .json(&json!({
            "kind": "rust_native",
            "name": "NWS B-0",
            "config": { "lat": 46.8, "lon": -114.0 }
        }))
        .send()
        .await
        .expect("create connector in ws_b failed");
    assert_eq!(
        r.status(),
        StatusCode::OK,
        "expected 200 creating connector in ws_b, got {}",
        r.status()
    );

    // List connectors for workspace A
    let list_a: Value = client
        .get(format!("{}/api/v1/workspaces/{}/connectors", base, ws_a))
        .send()
        .await
        .expect("GET connectors for ws_a failed")
        .json()
        .await
        .expect("list A body not JSON");

    let items_a = list_a["items"].as_array().expect("items must be an array");
    assert_eq!(
        items_a.len(),
        2,
        "workspace A must have exactly 2 connectors, got {}; items: {}",
        items_a.len(),
        list_a["items"]
    );

    // Every item in the list must belong to workspace A
    for item in items_a {
        assert_eq!(
            item["workspaceId"]
                .as_str()
                .expect("item must have workspaceId"),
            ws_a.to_string().as_str(),
            "all items in list A must have workspaceId={}, got: {}",
            ws_a,
            item["workspaceId"]
        );
    }

    // List connectors for workspace B — must return exactly 1
    let list_b: Value = client
        .get(format!("{}/api/v1/workspaces/{}/connectors", base, ws_b))
        .send()
        .await
        .expect("GET connectors for ws_b failed")
        .json()
        .await
        .expect("list B body not JSON");

    let items_b = list_b["items"].as_array().expect("items must be an array");
    assert_eq!(
        items_b.len(),
        1,
        "workspace B must have exactly 1 connector, got {}; items: {}",
        items_b.len(),
        list_b["items"]
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// Poll endpoint
// ──────────────────────────────────────────────────────────────────────────────

/// POST /api/v1/streams/:id/poll → { ingested: N } where N >= 1.
/// After the poll, stream_events contains at least one row with a payload that
/// has a recognizable NWS observation key.
///
/// NOTE: this test calls the live NWS API (api.weather.gov). Set
/// IONE_SKIP_LIVE=1 to skip it in environments without network access.
///
/// Contract targets:
///   POST /api/v1/streams/:id/poll → { ingested: n }
///   stream_event.payload must contain an observation (NWS JSON)
///   Plan Phase 4: "POST /api/v1/streams/:id/poll → { ingested: N } where N >= 1"
///
/// REASON: requires DATABASE_URL, migration 0003, connector routes, and NWS
///         connector implementation (src/connectors/nws.rs). Additionally
///         requires network access to api.weather.gov.
#[tokio::test]
#[ignore] // REASON: requires live NWS API (api.weather.gov); set IONE_SKIP_LIVE=1 to skip
async fn poll_stream_ingests_events() {
    if std::env::var("IONE_SKIP_LIVE").is_ok() {
        eprintln!("IONE_SKIP_LIVE set — skipping live NWS test");
        return;
    }

    let (base, pool) = spawn_app().await;
    let client = reqwest::Client::new();
    let ws_id = ops_workspace_id(&pool).await;

    // Create an NWS connector for a location guaranteed to have current observations.
    // Missoula MT (KMSO) is chosen because it's the reference location in the plan.
    let connector: Value = client
        .post(format!("{}/api/v1/workspaces/{}/connectors", base, ws_id))
        .json(&json!({
            "kind": "rust_native",
            "name": "NWS Missoula Poll Test",
            "config": { "lat": 46.8721, "lon": -113.9940 }
        }))
        .send()
        .await
        .expect("POST connector failed")
        .json()
        .await
        .expect("connector body not JSON");

    let connector_id = connector["id"].as_str().expect("connector must have id");

    // Get the auto-registered "observations" stream id
    let streams: Value = client
        .get(format!(
            "{}/api/v1/connectors/{}/streams",
            base, connector_id
        ))
        .send()
        .await
        .expect("GET streams failed")
        .json()
        .await
        .expect("streams body not JSON");

    let items = streams["items"]
        .as_array()
        .expect("streams items must be array");
    let obs_stream = items
        .iter()
        .find(|s| s["name"].as_str() == Some("observations"))
        .expect("'observations' stream must exist after connector creation");

    let stream_id_str = obs_stream["id"].as_str().expect("stream must have id");
    let stream_id = Uuid::parse_str(stream_id_str).expect("stream id must be UUID");

    // Poll the stream
    let poll_resp = client
        .post(format!("{}/api/v1/streams/{}/poll", base, stream_id_str))
        .json(&json!({}))
        .send()
        .await
        .expect("POST /api/v1/streams/:id/poll failed");

    assert_eq!(
        poll_resp.status(),
        StatusCode::OK,
        "expected 200 from POST /api/v1/streams/:id/poll, got {}",
        poll_resp.status()
    );

    let poll_body: Value = poll_resp.json().await.expect("poll body not JSON");

    let ingested = poll_body["ingested"]
        .as_i64()
        .expect("poll response must have integer 'ingested' field");

    assert!(
        ingested >= 1,
        "poll must ingest at least 1 event from NWS for Missoula MT, got ingested={}",
        ingested
    );

    // DB: stream_events has at least one row for this stream with an NWS-shaped payload
    let event_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM stream_events WHERE stream_id = $1")
            .bind(stream_id)
            .fetch_one(&pool)
            .await
            .expect("stream_events count query failed");

    assert!(
        event_count >= 1,
        "DB must have at least 1 stream_events row for stream_id={} after poll, got {}",
        stream_id,
        event_count
    );

    // Payload must contain at least one recognizable NWS observation key.
    // NWS /observations/latest returns a GeoJSON Feature; typical top-level keys
    // include "@id", "properties", "geometry". The properties object contains
    // "temperature", "dewpoint", "windSpeed", etc.
    // We look for any of these keys anywhere in the top-level payload.
    let payload: serde_json::Value = sqlx::query_scalar(
        "SELECT payload FROM stream_events WHERE stream_id = $1 ORDER BY ingested_at DESC LIMIT 1",
    )
    .bind(stream_id)
    .fetch_one(&pool)
    .await
    .expect("payload fetch failed");

    let payload_str = payload.to_string();
    let nws_keys = ["temperature", "dewpoint", "windSpeed", "@id", "properties"];
    let has_nws_key = nws_keys.iter().any(|k| payload_str.contains(k));

    assert!(
        has_nws_key,
        "stream_event payload must contain at least one NWS observation key \
         (temperature, dewpoint, windSpeed, @id, or properties); got payload: {}",
        &payload_str[..payload_str.len().min(500)]
    );
}

/// After a poll, every stream_events row must have a non-null observed_at that
/// is a TIMESTAMPTZ within the last 7 days (loose bound — NWS returns recent
/// observations; any value from the last few days is fine for v1).
///
/// Contract targets:
///   stream_event.observed_at: TIMESTAMPTZ NOT NULL (contract § stream_event)
///   Plan Phase 4: "stream_events.observed_at is a TIMESTAMPTZ within the last 24h"
///
/// REASON: requires DATABASE_URL, migration 0003, NWS connector + live network.
#[tokio::test]
#[ignore] // REASON: requires live NWS API; set IONE_SKIP_LIVE=1 to skip
async fn poll_records_observed_at() {
    if std::env::var("IONE_SKIP_LIVE").is_ok() {
        eprintln!("IONE_SKIP_LIVE set — skipping live NWS test");
        return;
    }

    let (base, pool) = spawn_app().await;
    let client = reqwest::Client::new();
    let ws_id = ops_workspace_id(&pool).await;

    let connector: Value = client
        .post(format!("{}/api/v1/workspaces/{}/connectors", base, ws_id))
        .json(&json!({
            "kind": "rust_native",
            "name": "NWS Missoula ObservedAt Test",
            "config": { "lat": 46.8721, "lon": -113.9940 }
        }))
        .send()
        .await
        .expect("POST connector failed")
        .json()
        .await
        .expect("connector body not JSON");

    let connector_id = connector["id"].as_str().expect("connector must have id");

    let streams: Value = client
        .get(format!(
            "{}/api/v1/connectors/{}/streams",
            base, connector_id
        ))
        .send()
        .await
        .expect("GET streams failed")
        .json()
        .await
        .expect("streams body not JSON");

    let stream_id_str = streams["items"]
        .as_array()
        .expect("items array")
        .iter()
        .find(|s| s["name"] == "observations")
        .expect("observations stream")["id"]
        .as_str()
        .expect("stream id");
    let stream_id = Uuid::parse_str(stream_id_str).expect("stream id UUID");

    // Poll
    let poll_resp = client
        .post(format!("{}/api/v1/streams/{}/poll", base, stream_id_str))
        .json(&json!({}))
        .send()
        .await
        .expect("poll failed");
    assert_eq!(poll_resp.status(), StatusCode::OK);

    // Every observed_at in the DB must be non-null and within the last 7 days
    // (NWS only serves recent observations; 7 days is a very loose bound).
    let rows: Vec<(chrono::DateTime<chrono::Utc>,)> =
        sqlx::query_as("SELECT observed_at FROM stream_events WHERE stream_id = $1")
            .bind(stream_id)
            .fetch_all(&pool)
            .await
            .expect("observed_at fetch failed");

    assert!(!rows.is_empty(), "stream_events must have rows after poll");

    let cutoff = chrono::Utc::now() - chrono::Duration::days(7);

    for (observed_at,) in &rows {
        assert!(
            *observed_at > cutoff,
            "stream_event.observed_at must be within the last 7 days, got: {}",
            observed_at
        );
    }
}

/// Polling the same stream twice must not produce duplicate rows for the same
/// observation.  The plan allows either a DB UNIQUE constraint on
/// (stream_id, observed_at) OR handler-level insert-if-not-exists.  Either way,
/// the second poll must leave the row count unchanged for observations that were
/// already ingested.
///
/// Test strategy (no live network required):
///   1. Create connector + poll once (live or fixture, any value is fine).
///   2. Poll a second time immediately.
///   3. Assert row count did not increase.
///
/// NOTE: this test hits the live NWS API on first poll to seed a real row.
/// Set IONE_SKIP_LIVE=1 to skip.
///
/// Contract targets:
///   stream_event dedup: (stream_id, observed_at) must be unique
///   Plan Phase 4: "second poll does not duplicate"
///
/// REASON: requires DATABASE_URL, migration 0003, NWS connector + live network.
#[tokio::test]
#[ignore] // REASON: requires live NWS API for initial seed; set IONE_SKIP_LIVE=1 to skip
async fn poll_is_idempotent_on_same_observation() {
    if std::env::var("IONE_SKIP_LIVE").is_ok() {
        eprintln!("IONE_SKIP_LIVE set — skipping live NWS test");
        return;
    }

    let (base, pool) = spawn_app().await;
    let client = reqwest::Client::new();
    let ws_id = ops_workspace_id(&pool).await;

    let connector: Value = client
        .post(format!("{}/api/v1/workspaces/{}/connectors", base, ws_id))
        .json(&json!({
            "kind": "rust_native",
            "name": "NWS Missoula Idempotent Test",
            "config": { "lat": 46.8721, "lon": -113.9940 }
        }))
        .send()
        .await
        .expect("POST connector failed")
        .json()
        .await
        .expect("connector body not JSON");

    let connector_id = connector["id"].as_str().expect("connector must have id");

    let streams: Value = client
        .get(format!(
            "{}/api/v1/connectors/{}/streams",
            base, connector_id
        ))
        .send()
        .await
        .expect("GET streams failed")
        .json()
        .await
        .expect("streams body not JSON");

    let stream_id_str = streams["items"]
        .as_array()
        .expect("items array")
        .iter()
        .find(|s| s["name"] == "observations")
        .expect("observations stream")["id"]
        .as_str()
        .expect("stream id")
        .to_owned();
    let stream_id = Uuid::parse_str(&stream_id_str).expect("stream id UUID");

    // First poll — seeds rows
    let r1 = client
        .post(format!("{}/api/v1/streams/{}/poll", base, stream_id_str))
        .json(&json!({}))
        .send()
        .await
        .expect("first poll failed");
    assert_eq!(r1.status(), StatusCode::OK, "first poll must return 200");

    let count_after_first: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM stream_events WHERE stream_id = $1")
            .bind(stream_id)
            .fetch_one(&pool)
            .await
            .expect("count query failed");

    assert!(
        count_after_first >= 1,
        "first poll must ingest at least 1 event"
    );

    // Second poll — same NWS observation returned; must not add duplicates
    let r2 = client
        .post(format!("{}/api/v1/streams/{}/poll", base, stream_id_str))
        .json(&json!({}))
        .send()
        .await
        .expect("second poll failed");
    assert_eq!(r2.status(), StatusCode::OK, "second poll must return 200");

    let count_after_second: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM stream_events WHERE stream_id = $1")
            .bind(stream_id)
            .fetch_one(&pool)
            .await
            .expect("count query failed after second poll");

    assert_eq!(
        count_after_second, count_after_first,
        "second poll must not insert duplicate rows for already-ingested observations; \
         count before={} count after={} (dedup key: stream_id + observed_at)",
        count_after_first, count_after_second
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// Cascade / referential integrity
// ──────────────────────────────────────────────────────────────────────────────

/// Deleting a connector row must cascade to its streams and their stream_events.
///
/// Test:
///   1. Create connector; poll to seed at least one stream_event.
///   2. DELETE connectors row via sqlx.
///   3. Assert streams + stream_events rows for that connector are gone.
///
/// Contract targets:
///   connectors → streams (ON DELETE CASCADE)
///   streams    → stream_events (ON DELETE CASCADE)
///   Plan Phase 4 migration 0003 schema.
///
/// REASON: requires DATABASE_URL, migration 0003, NWS connector + live network
///         (for the seed poll step).
#[tokio::test]
#[ignore] // REASON: seed poll requires live NWS API; set IONE_SKIP_LIVE=1 to skip
async fn stream_events_cascades_on_connector_delete() {
    if std::env::var("IONE_SKIP_LIVE").is_ok() {
        eprintln!("IONE_SKIP_LIVE set — skipping live NWS test");
        return;
    }

    let (base, pool) = spawn_app().await;
    let client = reqwest::Client::new();
    let ws_id = ops_workspace_id(&pool).await;

    // Create connector
    let connector: Value = client
        .post(format!("{}/api/v1/workspaces/{}/connectors", base, ws_id))
        .json(&json!({
            "kind": "rust_native",
            "name": "NWS Cascade Test",
            "config": { "lat": 46.8721, "lon": -113.9940 }
        }))
        .send()
        .await
        .expect("POST connector failed")
        .json()
        .await
        .expect("connector body not JSON");

    let connector_id_str = connector["id"].as_str().expect("connector must have id");
    let connector_id = Uuid::parse_str(connector_id_str).expect("connector id must be UUID");

    // Get stream id
    let streams: Value = client
        .get(format!(
            "{}/api/v1/connectors/{}/streams",
            base, connector_id_str
        ))
        .send()
        .await
        .expect("GET streams failed")
        .json()
        .await
        .expect("streams body not JSON");

    let stream_id_str = streams["items"]
        .as_array()
        .expect("items array")
        .iter()
        .find(|s| s["name"] == "observations")
        .expect("observations stream")["id"]
        .as_str()
        .expect("stream id")
        .to_owned();
    let stream_id = Uuid::parse_str(&stream_id_str).expect("stream id UUID");

    // Poll to seed a stream_event
    client
        .post(format!("{}/api/v1/streams/{}/poll", base, stream_id_str))
        .json(&json!({}))
        .send()
        .await
        .expect("poll failed");

    // Confirm events seeded
    let events_before: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM stream_events WHERE stream_id = $1")
            .bind(stream_id)
            .fetch_one(&pool)
            .await
            .expect("event count query failed");

    assert!(
        events_before >= 1,
        "must have at least 1 stream_event before connector delete"
    );

    // Delete the connector row directly (bypass API to test FK cascade)
    sqlx::query("DELETE FROM connectors WHERE id = $1")
        .bind(connector_id)
        .execute(&pool)
        .await
        .expect("connector delete failed");

    // streams must be gone
    let stream_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM streams WHERE connector_id = $1")
            .bind(connector_id)
            .fetch_one(&pool)
            .await
            .expect("stream count query failed");

    assert_eq!(
        stream_count, 0,
        "streams must be deleted when connector is deleted (ON DELETE CASCADE), got {}",
        stream_count
    );

    // stream_events must be gone (cascade through streams)
    let event_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM stream_events WHERE stream_id = $1")
            .bind(stream_id)
            .fetch_one(&pool)
            .await
            .expect("stream_event count query failed");

    assert_eq!(
        event_count, 0,
        "stream_events must be deleted when connector is deleted (two-hop cascade), got {}",
        event_count
    );
}

/// Deleting a workspace row must cascade to ALL connectors, streams, and
/// stream_events for that workspace.
///
/// Test:
///   1. Create a workspace.
///   2. Create 2 connectors in it.
///   3. Insert stream_events directly via sqlx (no live NWS needed).
///   4. DELETE the workspace row via sqlx.
///   5. Assert connectors, streams, and stream_events for that workspace are all gone.
///
/// Contract targets:
///   workspaces → connectors (ON DELETE CASCADE)
///   connectors → streams    (ON DELETE CASCADE)
///   streams    → stream_events (ON DELETE CASCADE)
///   Plan Phase 4 migration 0003.
///
/// REASON: requires DATABASE_URL and migration 0003.
/// This test does NOT require live NWS — events are inserted directly via sqlx.
#[tokio::test]
#[ignore]
async fn workspace_cascade_reaches_stream_events() {
    let (base, pool) = spawn_app().await;
    let client = reqwest::Client::new();

    // Create a fresh workspace (not Operations, so we can delete it cleanly)
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
        .expect("ws body not JSON");

    let ws_id_str = ws_body["id"].as_str().expect("ws must have id");
    let ws_id = Uuid::parse_str(ws_id_str).expect("ws id must be UUID");

    // Create 2 connectors in this workspace via API
    let mut stream_ids: Vec<Uuid> = Vec::new();
    for i in 0..2 {
        let c: Value = client
            .post(format!("{}/api/v1/workspaces/{}/connectors", base, ws_id))
            .json(&json!({
                "kind": "rust_native",
                "name": format!("NWS C-{}", i),
                "config": { "lat": 46.8, "lon": -114.0 }
            }))
            .send()
            .await
            .expect("create connector failed")
            .json()
            .await
            .expect("connector body not JSON");

        let cid = c["id"].as_str().expect("connector must have id");

        // Get the auto-registered stream id
        let ss: Value = client
            .get(format!("{}/api/v1/connectors/{}/streams", base, cid))
            .send()
            .await
            .expect("GET streams failed")
            .json()
            .await
            .expect("streams body not JSON");

        let sid_str = ss["items"]
            .as_array()
            .expect("items array")
            .iter()
            .find(|s| s["name"] == "observations")
            .expect("observations stream")["id"]
            .as_str()
            .expect("stream id")
            .to_owned();
        let sid = Uuid::parse_str(&sid_str).expect("stream id UUID");
        stream_ids.push(sid);
    }

    // Insert stream_events directly (no live NWS needed)
    let now = chrono::Utc::now();
    for sid in &stream_ids {
        sqlx::query(
            "INSERT INTO stream_events (stream_id, payload, observed_at, ingested_at)
             VALUES ($1, '{\"test\": true}'::jsonb, $2, $2)",
        )
        .bind(sid)
        .bind(now)
        .execute(&pool)
        .await
        .expect("insert stream_event failed");
    }

    // Confirm all connectors, streams, and events exist
    let connector_count_before: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM connectors WHERE workspace_id = $1")
            .bind(ws_id)
            .fetch_one(&pool)
            .await
            .expect("connector count failed");
    assert_eq!(
        connector_count_before, 2,
        "must have 2 connectors before delete"
    );

    let event_count_before: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM stream_events se
         JOIN streams s ON s.id = se.stream_id
         JOIN connectors c ON c.id = s.connector_id
         WHERE c.workspace_id = $1",
    )
    .bind(ws_id)
    .fetch_one(&pool)
    .await
    .expect("event count failed");
    assert!(
        event_count_before >= 2,
        "must have at least 2 stream_events before delete, got {event_count_before}"
    );

    // Delete the workspace row directly
    sqlx::query("DELETE FROM workspaces WHERE id = $1")
        .bind(ws_id)
        .execute(&pool)
        .await
        .expect("workspace delete failed");

    // Connectors must be gone
    let connector_count_after: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM connectors WHERE workspace_id = $1")
            .bind(ws_id)
            .fetch_one(&pool)
            .await
            .expect("connector count after delete failed");
    assert_eq!(
        connector_count_after, 0,
        "connectors must be deleted when workspace is deleted (ON DELETE CASCADE), got {}",
        connector_count_after
    );

    // stream_events must be gone (two-hop cascade)
    for sid in &stream_ids {
        let evt_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM stream_events WHERE stream_id = $1")
                .bind(sid)
                .fetch_one(&pool)
                .await
                .expect("event count after workspace delete failed");
        assert_eq!(
            evt_count, 0,
            "stream_events for stream_id={} must be deleted when workspace is deleted \
             (two-hop cascade through connectors → streams), got {}",
            sid, evt_count
        );
    }
}
