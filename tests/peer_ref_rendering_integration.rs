//! Issue #18 — shell ref-rendering for peer-owned storage.
//!
//! Four invariants are pinned here:
//!
//! 1. Peer-supplied render URLs (`tile_url`, `vector_url`) are validated to the
//!    same bar as `download_url`: https-only plus the SSRF guard, dropped with a
//!    warning rather than failing the whole fan-out.
//! 2. The four panel-discovery paths follow `nextCursor` (contract v1 §8.1) and
//!    terminate on an explicit `nextCursor: null` without re-requesting the last
//!    page.
//! 3. Every peer call site accepts an SSE-framed JSON-RPC reply and correlates
//!    it by request id.
//! 4. **Zero peer-payload persistence.** A full map + chart + table + document
//!    render fan-out leaves no peer payload anywhere in Postgres.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::{
    extract::State,
    http::header::CONTENT_TYPE,
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use reqwest::StatusCode;
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool, Row};
use tokio::net::TcpListener;
use uuid::Uuid;

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione";
const TEST_STATIC_BEARER: &str = "peer-ref-rendering-test-bearer";

/// Sentinels that live in peer **payload** bodies, never in a resource's name,
/// URI, or description. That split matters: peer *manifest metadata* (resource
/// names/descriptions in `catalog_entries`, the whole manifest in
/// `peers.last_manifest_jsonb`) is legitimately persisted by the manifest-refresh
/// path, so a sentinel placed in a name would prove nothing about payloads.
/// Every sentinel below is only reachable through a live render fan-out.
const SENTINEL_TILE: &str = "IONE18TILEPATH";
const SENTINEL_DOC: &str = "IONE18DOCPATH";
const SENTINEL_TABLE_CELL: &str = "IONE18ROWCELL";
const SENTINEL_CHART_VALUE: &str = "IONE18CHARTVAL";
/// Positive control: written into `peers.name` by the test itself, so a scan
/// that reports "found nowhere" for it is a broken scan, not a passing test.
const SENTINEL_CONTROL: &str = "IONE18PEERNAME";

/* ── stub peer ─────────────────────────────────────────────────────────── */

struct StubPeerState {
    /// `resources/list` pages, in order. The last page terminates with an
    /// explicit `"nextCursor": null`.
    pages: Vec<Vec<Value>>,
    /// `resources/read` bodies, keyed by resource URI.
    reads: HashMap<String, String>,
    /// Frame replies as `text/event-stream` instead of `application/json`.
    sse: bool,
    requests: Mutex<Vec<Value>>,
}

struct StubPeer {
    url: String,
    state: Arc<StubPeerState>,
}

impl StubPeer {
    fn list_calls(&self) -> usize {
        self.state
            .requests
            .lock()
            .expect("requests mutex")
            .iter()
            .filter(|body| body["method"] == json!("resources/list"))
            .count()
    }
}

async fn spawn_stub_peer(
    pages: Vec<Vec<Value>>,
    reads: HashMap<String, String>,
    sse: bool,
) -> StubPeer {
    let state = Arc::new(StubPeerState {
        pages,
        reads,
        sse,
        requests: Mutex::new(Vec::new()),
    });
    let router = Router::new()
        .route("/", post(stub_peer_handler))
        .with_state(Arc::clone(&state));

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind peer");
    let addr: SocketAddr = listener.local_addr().expect("peer addr");
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("peer server");
    });
    StubPeer {
        url: format!("http://{addr}"),
        state,
    }
}

async fn stub_peer_handler(
    State(state): State<Arc<StubPeerState>>,
    Json(body): Json<Value>,
) -> Response {
    state
        .requests
        .lock()
        .expect("requests mutex")
        .push(body.clone());
    let id = body.get("id").cloned().unwrap_or(Value::Null);

    let result = match body["method"].as_str().unwrap_or("") {
        "resources/list" => {
            let page = body["params"]["cursor"]
                .as_str()
                .and_then(|cursor| cursor.strip_prefix("page-"))
                .and_then(|index| index.parse::<usize>().ok())
                .unwrap_or(0);
            let resources = state.pages.get(page).cloned().unwrap_or_default();
            let next = if page + 1 < state.pages.len() {
                json!(format!("page-{}", page + 1))
            } else {
                // Explicit null termination — contract v1 §8.1.
                Value::Null
            };
            json!({ "resources": resources, "nextCursor": next })
        }
        "resources/read" => {
            let uri = body["params"]["uri"].as_str().unwrap_or("").to_string();
            match state.reads.get(&uri) {
                Some(text) => json!({
                    "contents": [{
                        "uri": uri,
                        "mimeType": "application/json",
                        "text": text
                    }]
                }),
                None => {
                    return reply(
                        &state,
                        json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": { "code": -32002, "message": "Resource not found" }
                        }),
                    )
                }
            }
        }
        _ => json!({}),
    };

    reply(
        &state,
        json!({ "jsonrpc": "2.0", "id": id, "result": result }),
    )
}

fn reply(state: &StubPeerState, message: Value) -> Response {
    if !state.sse {
        return Json(message).into_response();
    }
    // A spec-conforming server may interleave server-initiated traffic before the
    // reply; the unrelated notification below must not be mistaken for it.
    let body = format!(
        "event: message\ndata: {}\n\nevent: message\ndata: {}\n\n",
        json!({ "jsonrpc": "2.0", "method": "notifications/progress", "params": {} }),
        message
    );
    ([(CONTENT_TYPE, "text/event-stream")], body).into_response()
}

/* ── resource fixtures ─────────────────────────────────────────────────── */

fn map_resource(uri: &str, tile_url: &str, vector_url: Option<&str>) -> Value {
    let mut meta = json!({ "ione_view": "map", "tile_url": tile_url });
    if let Some(vector_url) = vector_url {
        meta["vector_url"] = json!(vector_url);
    }
    json!({ "uri": uri, "name": "Peer layer", "metadata": meta })
}

fn chart_resource(uri: &str) -> Value {
    json!({
        "uri": uri,
        "name": "Peer chart",
        "metadata": {
            "ione_view": "chart",
            "spec": { "chartType": "line", "xAxis": "bucket_start", "yAxis": "value", "series": ["value"] }
        }
    })
}

fn table_resource(uri: &str) -> Value {
    json!({ "uri": uri, "name": "Peer table", "metadata": { "ione_view": "table" } })
}

fn document_resource(uri: &str, download_url: &str) -> Value {
    json!({
        "uri": uri,
        "name": "Peer document",
        "metadata": {
            "ione_view": "document",
            "download_url": download_url,
            "mime_type": "application/pdf"
        }
    })
}

/* ── app + seeding ─────────────────────────────────────────────────────── */

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
    (format!("http://{addr}"), pool)
}

async fn default_org_id(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("SELECT id FROM organizations WHERE name = 'Default Org' LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("Default Org not found")
}

async fn default_workspace_id(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("SELECT id FROM workspaces WHERE name = 'Operations' LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("Operations workspace not found")
}

async fn seed_active_peer(pool: &PgPool, workspace_id: Uuid, name: &str, url: &str) -> Uuid {
    let org_id = default_org_id(pool).await;
    let issuer_id: Uuid = sqlx::query_scalar(
        "INSERT INTO trust_issuers (org_id, issuer_url, audience, jwks_uri, claim_mapping)
         VALUES ($1, $2, 'aud', 'secret:test', '{}'::jsonb)
         RETURNING id",
    )
    .bind(org_id)
    .bind(format!("https://{}.issuer.test", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert trust issuer");

    let peer_id: Uuid = sqlx::query_scalar(
        "INSERT INTO peers (name, mcp_url, issuer_id, sharing_policy, tool_allowlist, status)
         VALUES ($1, $2, $3, '{}'::jsonb, '[]'::jsonb, 'active'::peer_status)
         RETURNING id",
    )
    .bind(name)
    .bind(url)
    .bind(issuer_id)
    .fetch_one(pool)
    .await
    .expect("insert peer");

    sqlx::query(
        "INSERT INTO workspace_peer_bindings (workspace_id, peer_id, foreign_tenant_id, status)
         VALUES ($1, $2, 'tenant-1', 'active'::binding_status)",
    )
    .bind(workspace_id)
    .bind(peer_id)
    .execute(pool)
    .await
    .expect("insert binding");

    peer_id
}

async fn get(base: &str, path: &str) -> reqwest::Response {
    reqwest::Client::new()
        .get(format!("{base}{path}"))
        .bearer_auth(TEST_STATIC_BEARER)
        .send()
        .await
        .expect("request")
}

async fn get_json(base: &str, path: &str) -> Value {
    let resp = get(base, path).await;
    assert_eq!(resp.status(), StatusCode::OK, "GET {path}");
    resp.json().await.expect("json")
}

/* ── GAP 1: peer-supplied render URLs are validated ────────────────────── */

#[tokio::test]
#[ignore]
async fn unsafe_tile_urls_are_dropped_without_failing_the_peer() {
    let (base, pool) = spawn_app().await;
    let workspace_id = default_workspace_id(&pool).await;
    let peer = spawn_stub_peer(
        vec![vec![
            map_resource(
                "peer://layer/ok",
                "https://tiles.example.test/{z}/{x}/{y}.png",
                None,
            ),
            map_resource("peer://layer/js", "javascript:alert(1)", None),
            map_resource(
                "peer://layer/http",
                "http://tiles.example.test/{z}/{x}/{y}.png",
                None,
            ),
            map_resource(
                "peer://layer/metadata",
                "https://169.254.169.254/{z}/{x}/{y}.png",
                None,
            ),
            map_resource("peer://layer/garbage", "not-a-url", None),
        ]],
        HashMap::new(),
        false,
    )
    .await;
    let peer_id = seed_active_peer(&pool, workspace_id, "tile-peer", &peer.url).await;

    let body = get_json(
        &base,
        &format!("/api/v1/workspaces/{workspace_id}/map-layers"),
    )
    .await;
    let items = body["items"].as_array().expect("items");
    assert_eq!(items.len(), 1, "only the https layer survives: {items:?}");
    assert_eq!(items[0]["uri"], "peer://layer/ok");
    // Partial success, exactly like the one-peer-fails convention: the peer
    // answered, so it is reported OK and only the unsafe layers are dropped.
    assert_eq!(body["peersOk"][0], peer_id.to_string());
    assert!(body["peersFailed"]
        .as_array()
        .expect("peersFailed")
        .is_empty());
}

#[tokio::test]
#[ignore]
async fn unsafe_vector_url_is_stripped_but_keeps_its_layer() {
    let (base, pool) = spawn_app().await;
    let workspace_id = default_workspace_id(&pool).await;
    let peer = spawn_stub_peer(
        vec![vec![
            map_resource(
                "peer://layer/safe-vector",
                "https://tiles.example.test/{z}/{x}/{y}.png",
                Some("https://tiles.example.test/layer.pmtiles"),
            ),
            map_resource(
                "peer://layer/unsafe-vector",
                "https://tiles.example.test/{z}/{x}/{y}.png",
                Some("javascript:alert(1)"),
            ),
        ]],
        HashMap::new(),
        false,
    )
    .await;
    seed_active_peer(&pool, workspace_id, "vector-peer", &peer.url).await;

    let body = get_json(
        &base,
        &format!("/api/v1/workspaces/{workspace_id}/map-layers"),
    )
    .await;
    let items = body["items"].as_array().expect("items");
    assert_eq!(items.len(), 2, "both layers keep a valid tile_url");
    let by_uri: HashMap<&str, &Value> = items
        .iter()
        .map(|item| (item["uri"].as_str().unwrap_or_default(), item))
        .collect();
    assert_eq!(
        by_uri["peer://layer/safe-vector"]["meta"]["vectorUrl"],
        "https://tiles.example.test/layer.pmtiles"
    );
    assert_eq!(
        by_uri["peer://layer/unsafe-vector"]["meta"]["vectorUrl"],
        Value::Null
    );
}

/* ── GAP 3: panel paths honour the contract's pagination ───────────────── */

fn paged_peer_pages() -> Vec<Vec<Value>> {
    vec![
        vec![
            map_resource(
                "peer://layer/p1",
                "https://tiles.example.test/a/{z}/{x}/{y}.png",
                None,
            ),
            chart_resource("peer://chart/p1"),
        ],
        vec![
            table_resource("peer://table/p2"),
            document_resource("peer://doc/p2", "https://docs.example.test/p2.pdf"),
        ],
        vec![
            map_resource(
                "peer://layer/p3",
                "https://tiles.example.test/b/{z}/{x}/{y}.png",
                None,
            ),
            chart_resource("peer://chart/p3"),
            table_resource("peer://table/p3"),
            document_resource("peer://doc/p3", "https://docs.example.test/p3.pdf"),
        ],
    ]
}

fn uris(items: &Value) -> Vec<String> {
    items
        .as_array()
        .expect("array")
        .iter()
        .map(|item| item["uri"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[tokio::test]
#[ignore]
async fn all_four_panel_paths_follow_next_cursor_to_the_last_page() {
    let (base, pool) = spawn_app().await;
    let workspace_id = default_workspace_id(&pool).await;
    let peer = spawn_stub_peer(paged_peer_pages(), HashMap::new(), false).await;
    seed_active_peer(&pool, workspace_id, "paged-peer", &peer.url).await;

    let maps = get_json(
        &base,
        &format!("/api/v1/workspaces/{workspace_id}/map-layers"),
    )
    .await;
    assert_eq!(
        uris(&maps["items"]),
        vec!["peer://layer/p1", "peer://layer/p3"]
    );

    let charts = get_json(
        &base,
        &format!("/api/v1/workspaces/{workspace_id}/chart-panels"),
    )
    .await;
    assert_eq!(
        uris(&charts["peerCharts"]),
        vec!["peer://chart/p1", "peer://chart/p3"]
    );

    let tables = get_json(
        &base,
        &format!("/api/v1/workspaces/{workspace_id}/table-panels"),
    )
    .await;
    assert_eq!(
        uris(&tables["peerTables"]),
        vec!["peer://table/p2", "peer://table/p3"]
    );

    let documents = get_json(
        &base,
        &format!("/api/v1/workspaces/{workspace_id}/document-panels"),
    )
    .await;
    assert_eq!(
        uris(&documents["peerDocuments"]),
        vec!["peer://doc/p2", "peer://doc/p3"]
    );

    // 4 panels x 3 pages. An explicit `nextCursor: null` on the final page must
    // terminate paging, not re-request it until the 50-page cap.
    assert_eq!(peer.list_calls(), 12);
}

#[tokio::test]
#[ignore]
async fn chart_data_rejects_an_oversized_chart_body() {
    let (base, pool) = spawn_app().await;
    let workspace_id = default_workspace_id(&pool).await;
    let oversized = json!({
        "spec": { "chartType": "line", "xAxis": "bucket_start", "yAxis": "value", "series": ["value"] },
        "rows": "x".repeat(2 * 1024 * 1024)
    })
    .to_string();
    let peer = spawn_stub_peer(
        vec![vec![chart_resource("peer://chart/big")]],
        HashMap::from([("peer://chart/big".to_string(), oversized)]),
        false,
    )
    .await;
    let peer_id = seed_active_peer(&pool, workspace_id, "big-chart-peer", &peer.url).await;

    let resp = get(
        &base,
        &format!(
            "/api/v1/workspaces/{workspace_id}/chart-data?peer_id={peer_id}&uri=peer%3A%2F%2Fchart%2Fbig"
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

/* ── GAP 5: SSE-framed replies on every peer call site ─────────────────── */

#[tokio::test]
#[ignore]
async fn every_panel_path_reads_sse_framed_replies() {
    let (base, pool) = spawn_app().await;
    let workspace_id = default_workspace_id(&pool).await;
    let peer = spawn_stub_peer(
        vec![vec![
            map_resource("peer://layer/sse", "https://tiles.example.test/{z}/{x}/{y}.png", None),
            chart_resource("peer://chart/sse"),
            table_resource("peer://table/sse"),
            document_resource("peer://doc/sse", "https://docs.example.test/sse.pdf"),
        ]],
        HashMap::from([
            (
                "peer://chart/sse".to_string(),
                json!({
                    "spec": { "chartType": "line", "xAxis": "bucket_start", "yAxis": "value", "series": ["value"] },
                    "rows": [{ "bucket_start": "2026-07-01T00:00:00Z", "value": 7 }]
                })
                .to_string(),
            ),
            (
                "peer://table/sse".to_string(),
                json!({
                    "schema": [{ "name": "asset", "type": "string" }],
                    "rows": [{ "asset": "A-1" }]
                })
                .to_string(),
            ),
        ]),
        true,
    )
    .await;
    let peer_id = seed_active_peer(&pool, workspace_id, "sse-peer", &peer.url).await;

    let maps = get_json(
        &base,
        &format!("/api/v1/workspaces/{workspace_id}/map-layers"),
    )
    .await;
    assert_eq!(uris(&maps["items"]), vec!["peer://layer/sse"]);
    assert!(maps["peersFailed"]
        .as_array()
        .expect("peersFailed")
        .is_empty());

    let charts = get_json(
        &base,
        &format!("/api/v1/workspaces/{workspace_id}/chart-panels"),
    )
    .await;
    assert_eq!(uris(&charts["peerCharts"]), vec!["peer://chart/sse"]);

    let tables = get_json(
        &base,
        &format!("/api/v1/workspaces/{workspace_id}/table-panels"),
    )
    .await;
    assert_eq!(uris(&tables["peerTables"]), vec!["peer://table/sse"]);

    let documents = get_json(
        &base,
        &format!("/api/v1/workspaces/{workspace_id}/document-panels"),
    )
    .await;
    assert_eq!(uris(&documents["peerDocuments"]), vec!["peer://doc/sse"]);

    let chart_data = get_json(
        &base,
        &format!(
            "/api/v1/workspaces/{workspace_id}/chart-data?peer_id={peer_id}&uri=peer%3A%2F%2Fchart%2Fsse"
        ),
    )
    .await;
    assert_eq!(chart_data["rows"][0]["value"], 7);

    let table_data = get_json(
        &base,
        &format!(
            "/api/v1/workspaces/{workspace_id}/table-data?peer_id={peer_id}&uri=peer%3A%2F%2Ftable%2Fsse"
        ),
    )
    .await;
    assert_eq!(table_data["rows"][0]["asset"], "A-1");
}

/* ── GAP 4: zero peer-payload persistence ──────────────────────────────── */

/// Every base table in the `public` schema, so the scan cannot miss a table
/// nobody thought to check.
async fn public_tables(pool: &PgPool) -> Vec<String> {
    sqlx::query(
        "SELECT table_name FROM information_schema.tables
         WHERE table_schema = 'public' AND table_type = 'BASE TABLE'
         ORDER BY table_name",
    )
    .fetch_all(pool)
    .await
    .expect("list tables")
    .into_iter()
    .map(|row| row.get::<String, _>("table_name"))
    .collect()
}

/// Tables whose *entire row text* contains `needle` — every column of every row,
/// including jsonb blobs, is searched.
async fn tables_containing(pool: &PgPool, tables: &[String], needle: &str) -> Vec<String> {
    let mut hits = Vec::new();
    for table in tables {
        let sql = format!(
            "SELECT EXISTS (SELECT 1 FROM \"{table}\" AS scan_row WHERE scan_row::text LIKE $1)"
        );
        let found: bool = sqlx::query_scalar(&sql)
            .bind(format!("%{needle}%"))
            .fetch_one(pool)
            .await
            .unwrap_or_else(|err| panic!("scan of {table} failed: {err}"));
        if found {
            hits.push(table.clone());
        }
    }
    hits
}

#[tokio::test]
#[ignore]
async fn full_render_fanout_persists_no_peer_payload_in_postgres() {
    let (base, pool) = spawn_app().await;
    let workspace_id = default_workspace_id(&pool).await;

    let tile_url = format!("https://tiles.example.test/{SENTINEL_TILE}/{{z}}/{{x}}/{{y}}.png");
    let download_url = format!("https://docs.example.test/{SENTINEL_DOC}/report.pdf");
    let peer = spawn_stub_peer(
        vec![vec![
            map_resource("peer://layer/sentinel", &tile_url, None),
            chart_resource("peer://chart/sentinel"),
            table_resource("peer://table/sentinel"),
            document_resource("peer://doc/sentinel", &download_url),
        ]],
        HashMap::from([
            (
                "peer://chart/sentinel".to_string(),
                json!({
                    "spec": { "chartType": "line", "xAxis": "bucket_start", "yAxis": "value", "series": ["value"] },
                    "rows": [{ "bucket_start": "2026-07-01T00:00:00Z", "value": SENTINEL_CHART_VALUE }]
                })
                .to_string(),
            ),
            (
                "peer://table/sentinel".to_string(),
                json!({
                    "schema": [{ "name": "asset", "type": "string" }],
                    "rows": [{ "asset": SENTINEL_TABLE_CELL }]
                })
                .to_string(),
            ),
        ]),
        false,
    )
    .await;
    // The peer's own name carries the control sentinel: IONe *does* persist peer
    // registration rows, so this one must be findable.
    let peer_id = seed_active_peer(
        &pool,
        workspace_id,
        &format!("{SENTINEL_CONTROL} peer"),
        &peer.url,
    )
    .await;

    // Drive every render path that touches peer-owned storage.
    let maps = get_json(
        &base,
        &format!("/api/v1/workspaces/{workspace_id}/map-layers"),
    )
    .await;
    assert_eq!(maps["items"][0]["meta"]["tileUrl"], tile_url);
    let documents = get_json(
        &base,
        &format!("/api/v1/workspaces/{workspace_id}/document-panels"),
    )
    .await;
    assert_eq!(documents["peerDocuments"][0]["downloadUrl"], download_url);
    get_json(
        &base,
        &format!("/api/v1/workspaces/{workspace_id}/chart-panels"),
    )
    .await;
    get_json(
        &base,
        &format!("/api/v1/workspaces/{workspace_id}/table-panels"),
    )
    .await;
    let chart_data = get_json(
        &base,
        &format!(
            "/api/v1/workspaces/{workspace_id}/chart-data?peer_id={peer_id}&uri=peer%3A%2F%2Fchart%2Fsentinel"
        ),
    )
    .await;
    assert_eq!(chart_data["rows"][0]["value"], SENTINEL_CHART_VALUE);
    let table_data = get_json(
        &base,
        &format!(
            "/api/v1/workspaces/{workspace_id}/table-data?peer_id={peer_id}&uri=peer%3A%2F%2Ftable%2Fsentinel"
        ),
    )
    .await;
    assert_eq!(table_data["rows"][0]["asset"], SENTINEL_TABLE_CELL);

    // Let any write that a handler might have spawned land before scanning.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let tables = public_tables(&pool).await;
    assert!(
        tables.len() > 20,
        "expected the full schema, got {tables:?}"
    );

    // Positive control first: if this fails, the scan itself is broken and the
    // negative assertions below are worthless.
    let control_hits = tables_containing(&pool, &tables, SENTINEL_CONTROL).await;
    assert!(
        control_hits.contains(&"peers".to_string()),
        "scan cannot find a value that is definitely persisted: {control_hits:?}"
    );

    for sentinel in [
        SENTINEL_TILE,
        SENTINEL_DOC,
        SENTINEL_TABLE_CELL,
        SENTINEL_CHART_VALUE,
    ] {
        let hits = tables_containing(&pool, &tables, sentinel).await;
        assert!(
            hits.is_empty(),
            "peer payload sentinel {sentinel} was persisted in {hits:?}"
        );
    }

    // MinIO holds only what `artifacts` indexes; no peer render path writes one.
    let artifacts: i64 = sqlx::query_scalar("SELECT count(*) FROM artifacts")
        .fetch_one(&pool)
        .await
        .expect("count artifacts");
    assert_eq!(artifacts, 0);
}
