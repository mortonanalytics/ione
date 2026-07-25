/// Reference stub peer, end to end.
///
/// Two things are proven here, and they are deliberately not the same thing:
///
///   1. IONe's shell renders a contract-v1-conforming peer's refs — map layer,
///      chart panel, table panel and document panel — driven purely by
///      `metadata.ione_view`, with no stub-specific code anywhere in IONe.
///   2. The standalone conformance kit (`src/bin/ione-conformance.rs`) reports
///      PASS on all six surfaces when run against that same stub. That is what
///      makes the kit trustworthy for a candidate peer such as TerraYield, which
///      runs it without any IONe deployment in the loop.
///
/// Target: md/design/app-integration-contract-v1.md (v1, frozen 2026-07-25)
/// Kit docs: md/design/peer-conformance-kit.md
///
/// Prerequisites:
///   docker compose up -d postgres
///
/// Run:
///   SQLX_OFFLINE=true IONE_SKIP_LIVE=1 \
///     DATABASE_URL=postgres://ione:ione@localhost:5433/ione \
///     cargo test --test stub_peer_conformance_integration -- --ignored --test-threads=1
use std::net::SocketAddr;
use std::process::Command;

use reqwest::StatusCode;
use serde_json::Value;
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;
use uuid::Uuid;

mod support;
use support::stub_peer::{
    self, StubPeer, CHART_URI, DOCUMENT_DOWNLOAD_URL, DOCUMENT_URI, FOREIGN_TENANT_ID,
    FOREIGN_TENANT_NAME, FOREIGN_USER_EMAIL, MAP_TILE_URL, MAP_URI, TABLE_URI,
};

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione";
const TEST_STATIC_BEARER: &str = "stub-peer-conformance-test-bearer";
const TEST_KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

// ─── Harness ──────────────────────────────────────────────────────────────────

async fn spawn_app() -> (String, PgPool) {
    std::env::set_var("IONE_AUTH_MODE", "local");
    std::env::set_var("IONE_OAUTH_STATIC_BEARER", TEST_STATIC_BEARER);
    std::env::set_var("IONE_TOKEN_KEY", TEST_KEY);
    std::env::set_var("IONE_WEBHOOK_SECRET_KEY", TEST_KEY);

    let db_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_owned());
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("failed to connect to Postgres — is `docker compose up -d postgres` running?");

    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("migration failed");

    sqlx::query(
        "TRUNCATE pending_peer_tool_calls, webhook_events_seen, workspace_peer_bindings,
                  audit_events, approvals, artifacts,
                  peers, trust_issuers, routing_decisions, survivors, signals,
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

/// Register the stub as an active peer of the default workspace, bound to the
/// tenant its `whoami://` reports.
async fn register_stub_peer(pool: &PgPool, workspace_id: Uuid, stub: &StubPeer) -> Uuid {
    let org_id = default_org_id(pool).await;
    let issuer_id: Uuid = sqlx::query_scalar(
        "INSERT INTO trust_issuers (org_id, issuer_url, audience, jwks_uri, claim_mapping)
         VALUES ($1, 'https://stub-peer.issuer.test', 'aud', 'secret:test', '{}'::jsonb)
         RETURNING id",
    )
    .bind(org_id)
    .fetch_one(pool)
    .await
    .expect("insert trust issuer");

    let peer_id: Uuid = sqlx::query_scalar(
        "INSERT INTO peers (name, mcp_url, issuer_id, sharing_policy, tool_allowlist, status)
         VALUES ('stub-peer', $1, $2, '{}'::jsonb, '[]'::jsonb, 'active'::peer_status)
         RETURNING id",
    )
    .bind(&stub.mcp_url)
    .bind(issuer_id)
    .fetch_one(pool)
    .await
    .expect("insert peer");

    sqlx::query(
        "INSERT INTO workspace_peer_bindings (workspace_id, peer_id, foreign_tenant_id, status)
         VALUES ($1, $2, $3, 'active'::binding_status)",
    )
    .bind(workspace_id)
    .bind(peer_id)
    .bind(FOREIGN_TENANT_ID)
    .execute(pool)
    .await
    .expect("insert binding");

    peer_id
}

/// Grant the seeded 'member' role `peers:manage`. `POST .../bindings/:id/refresh`
/// is gated on it, and the operator who rebinds a peer is exactly who holds it.
async fn grant_peers_manage(pool: &PgPool, workspace_id: Uuid) {
    sqlx::query("UPDATE roles SET permissions = $2 WHERE workspace_id = $1 AND name = 'member'")
        .bind(workspace_id)
        .bind(serde_json::json!(["peers:manage"]))
        .execute(pool)
        .await
        .expect("grant peers:manage");
}

async fn get_json(base: &str, path: &str) -> (StatusCode, Value) {
    let response = reqwest::Client::new()
        .get(format!("{base}{path}"))
        .bearer_auth(TEST_STATIC_BEARER)
        .send()
        .await
        .expect("request");
    let status = response.status();
    (status, response.json().await.expect("json"))
}

// ─── Deliverable 2 — IONe renders the stub's refs end to end ──────────────────

/// All four `ione_view` panels populate from one peer's `resources/list`, using
/// only view-hint metadata (§4). The stub is never special-cased in IONe.
#[tokio::test]
#[ignore]
async fn ione_renders_all_four_stub_peer_views() {
    let (base, pool) = spawn_app().await;
    let workspace_id = default_workspace_id(&pool).await;
    let stub = StubPeer::start().await;
    let peer_id = register_stub_peer(&pool, workspace_id, &stub).await;

    // §4.2 — map layer
    let (status, body) = get_json(
        &base,
        &format!("/api/v1/workspaces/{workspace_id}/map-layers"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["items"].as_array().expect("items").len(),
        1,
        "§4.2: the stub's map resource must render: {body}"
    );
    assert_eq!(body["items"][0]["uri"], MAP_URI);
    assert_eq!(body["items"][0]["peerId"], peer_id.to_string());
    assert_eq!(body["items"][0]["meta"]["tileUrl"], MAP_TILE_URL);
    assert_eq!(body["items"][0]["meta"]["attribution"], "Stub Peer Tiles");
    assert_eq!(body["items"][0]["meta"]["layerName"], "Displacement");
    assert_eq!(
        body["peersFailed"].as_array().expect("peersFailed").len(),
        0
    );

    // §4.3 — chart panel
    let (status, body) = get_json(
        &base,
        &format!("/api/v1/workspaces/{workspace_id}/chart-panels"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["peerCharts"].as_array().expect("peerCharts").len(),
        1,
        "§4.3: the stub's chart resource must render: {body}"
    );
    assert_eq!(body["peerCharts"][0]["uri"], CHART_URI);
    assert_eq!(body["peerCharts"][0]["spec"]["chartType"], "line");
    assert_eq!(body["peerCharts"][0]["spec"]["xAxis"], "observation_time");
    assert_eq!(body["peerCharts"][0]["spec"]["yAxis"], "displacement_mm");

    // §4.4 — table panel
    let (status, body) = get_json(
        &base,
        &format!("/api/v1/workspaces/{workspace_id}/table-panels"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["peerTables"].as_array().expect("peerTables").len(),
        1,
        "§4.4: the stub's table resource must render: {body}"
    );
    assert_eq!(body["peerTables"][0]["uri"], TABLE_URI);
    assert_eq!(body["peerTables"][0]["name"], "Asset inventory");

    // §4.5 — document panel
    let (status, body) = get_json(
        &base,
        &format!("/api/v1/workspaces/{workspace_id}/document-panels"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["peerDocuments"]
            .as_array()
            .expect("peerDocuments")
            .len(),
        1,
        "§4.5: the stub's document resource must render: {body}"
    );
    assert_eq!(body["peerDocuments"][0]["uri"], DOCUMENT_URI);
    assert_eq!(
        body["peerDocuments"][0]["downloadUrl"],
        DOCUMENT_DOWNLOAD_URL
    );
    assert_eq!(body["peerDocuments"][0]["mimeType"], "application/pdf");
    assert_eq!(body["peerDocuments"][0]["fileSizeBytes"], 184_320);
}

/// The panel *bodies* fetched via `resources/read` are the ones the stub serves
/// (§4.3, §4.4) — so the refs are not merely listed, they resolve.
#[tokio::test]
#[ignore]
async fn ione_reads_stub_peer_chart_and_table_bodies() {
    let (base, pool) = spawn_app().await;
    let workspace_id = default_workspace_id(&pool).await;
    let stub = StubPeer::start().await;
    let peer_id = register_stub_peer(&pool, workspace_id, &stub).await;

    let chart_uri = urlencoding::encode(CHART_URI);
    let (status, body) = get_json(
        &base,
        &format!("/api/v1/workspaces/{workspace_id}/chart-data?peer_id={peer_id}&uri={chart_uri}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["spec"], stub_peer::chart_body()["spec"]);
    assert_eq!(body["rows"].as_array().expect("rows").len(), 3);
    assert_eq!(body["rows"][0]["observation_time"], "2026-07-01T00:00:00Z");
    assert_eq!(body["rows"][0]["mean"], 1.2);
    assert_eq!(body["rows"][0]["p95"], 3.4);

    let table_uri = urlencoding::encode(TABLE_URI);
    let (status, body) = get_json(
        &base,
        &format!("/api/v1/workspaces/{workspace_id}/table-data?peer_id={peer_id}&uri={table_uri}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["schema"], stub_peer::table_body()["schema"]);
    assert_eq!(body["rows"].as_array().expect("rows").len(), 3);
    assert_eq!(body["rows"][0]["asset_id"], "A-1");
    assert_eq!(body["rows"][0]["in_service"], true);
}

/// §6 + §5: the stub's identity binds the workspace, and its peer-authored slice
/// (`schema_version: "1"`) reaches IONe's context surface rather than the
/// synthesized `"0"` fallback of §5.2.
#[tokio::test]
#[ignore]
async fn ione_consumes_stub_peer_whoami_and_slice() {
    let (base, pool) = spawn_app().await;
    let workspace_id = default_workspace_id(&pool).await;
    let stub = StubPeer::start().await;
    let peer_id = register_stub_peer(&pool, workspace_id, &stub).await;
    grant_peers_manage(&pool, workspace_id).await;

    let binding_id: Uuid = sqlx::query_scalar(
        "SELECT id FROM workspace_peer_bindings WHERE workspace_id = $1 AND peer_id = $2",
    )
    .bind(workspace_id)
    .bind(peer_id)
    .fetch_one(&pool)
    .await
    .expect("binding id");

    let response = reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/workspaces/{workspace_id}/bindings/{binding_id}/refresh"
        ))
        .send()
        .await
        .expect("refresh");
    assert_eq!(response.status(), StatusCode::OK);
    let binding: Value = response.json().await.expect("json");
    assert_eq!(binding["foreignTenantId"], FOREIGN_TENANT_ID);
    assert_eq!(binding["foreignTenantName"], FOREIGN_TENANT_NAME);
    assert_eq!(binding["foreignUserEmail"], FOREIGN_USER_EMAIL);

    let (status, body) = get_json(
        &base,
        &format!("/api/v1/workspaces/{workspace_id}/context-slices"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let slices = body["items"].as_array().expect("context slice items");
    let slice = slices
        .iter()
        .find(|entry| entry["peerId"] == peer_id.to_string())
        .expect("stub peer slice");
    assert_eq!(
        slice["body"]["schema_version"], "1",
        "§5.2: \"1\" means peer-authored; \"0\" would mean IONe synthesized it"
    );
    assert_eq!(slice["body"]["peer_id"], stub_peer::SELF_PEER_ID);
    assert!(
        slice["body"]["summary"]
            .as_str()
            .expect("summary")
            .contains("stub peer"),
        "the peer's own summary must survive: {}",
        slice["body"]
    );
}

/// §8.1: the stub serves `tools/list` in two pages, so IONe's `nextCursor`
/// following is exercised by a peer that actually paginates.
#[tokio::test]
#[ignore]
async fn ione_follows_stub_peer_tools_list_pagination() {
    let (base, pool) = spawn_app().await;
    let workspace_id = default_workspace_id(&pool).await;
    let stub = StubPeer::start().await;
    let peer_id = register_stub_peer(&pool, workspace_id, &stub).await;

    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/peers/{peer_id}/manifest/refresh"))
        .bearer_auth(TEST_STATIC_BEARER)
        .send()
        .await
        .expect("manifest refresh");
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await.expect("json");
    let manifest = &body["manifest"];

    let tools: Vec<&str> = manifest["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert_eq!(
        tools,
        vec!["query_displacement", "acknowledge_alert"],
        "§8.1: both pages must be collected, in order"
    );
    assert_eq!(
        manifest["resources"].as_array().expect("resources").len(),
        stub_peer::resources().len()
    );
}

/// §3: a signed envelope from the stub is accepted by IONe's webhook ingress and
/// materializes a signal, closing the loop on the one surface where the peer is
/// the sender rather than the server.
#[tokio::test]
#[ignore]
async fn ione_accepts_stub_peer_signed_webhook() {
    let (base, pool) = spawn_app().await;
    let workspace_id = default_workspace_id(&pool).await;
    let stub = StubPeer::start().await;
    let peer_id = register_stub_peer(&pool, workspace_id, &stub).await;

    let provision: Value = reqwest::Client::new()
        .post(format!("{base}/api/v1/peers/{peer_id}/webhook/provision"))
        .bearer_auth(TEST_STATIC_BEARER)
        .send()
        .await
        .expect("provision")
        .json()
        .await
        .expect("provision json");
    let secret = provision["signingSecret"]
        .as_str()
        .expect("signingSecret")
        .to_string();
    stub.set_webhook_config(peer_id, secret);

    let response = stub.emit_webhook(&base).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "§3.1: the stub's signature must verify"
    );
    let body: Value = response.json().await.expect("json");
    assert_eq!(body["ok"], true);
    assert_eq!(body["duplicate"], false);
    // §3.5 documents this key as `signalIds`; `WebhookAckResponse` in
    // src/routes/webhooks.rs carries no `rename_all = "camelCase"`, so IONe
    // actually emits `signal_ids`. Accepting either keeps this test honest about
    // "an accepted event reports the signals it created" without pinning the
    // divergence in place — see the findings note in md/design/peer-conformance-kit.md.
    let signal_ids = body
        .get("signalIds")
        .or_else(|| body.get("signal_ids"))
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("§3.5: accepted webhook must report signal ids: {body}"));
    assert_eq!(
        signal_ids.len(),
        1,
        "one envelope creates one signal: {body}"
    );
}

// ─── Deliverable 3 — the conformance kit passes against the stub ──────────────

/// The kit is the artifact a candidate peer runs on its own; this asserts it
/// reports PASS on every one of the six surfaces against a conforming peer, and
/// exits 0. Without this, a green kit would prove nothing.
///
/// No IONe server is started here — that is the point of the kit.
#[tokio::test]
#[ignore]
async fn conformance_kit_passes_all_six_surfaces_against_the_stub() {
    let stub = StubPeer::start().await;
    let webhook_peer_id = Uuid::new_v4();
    stub.set_webhook_config(webhook_peer_id, "stub-conformance-signing-secret");

    let mcp_url = stub.mcp_url.clone();
    let trigger_url = stub.webhook_trigger_url();
    let output = tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_ione-conformance"))
            .args([
                "--url",
                &mcp_url,
                "--token",
                "stub-conformance-bearer",
                "--webhook-peer-id",
                &webhook_peer_id.to_string(),
                "--webhook-secret",
                "stub-conformance-signing-secret",
                "--webhook-trigger",
                &trigger_url,
                "--webhook-timeout",
                "20",
            ])
            .output()
            .expect("run ione-conformance")
    })
    .await
    .expect("join conformance run");

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let report = format!("--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}");
    // Captured by default; `-- --nocapture` shows the full six-surface report.
    println!("{report}");

    for surface in [
        "[PASS] 1. MCP server endpoint",
        "[PASS] 2. OAuth 2.1 authorization server",
        "[PASS] 3. Signed webhook sender",
        "[PASS] 4. Resource view metadata (ione_view)",
        "[PASS] 5. Context slice (slice://)",
        "[PASS] 6. whoami:// resource",
    ] {
        assert!(
            stdout.contains(surface),
            "expected '{surface}' in the conformance report:\n{report}"
        );
    }
    assert!(
        stdout.contains("6 passed, 0 failed, 0 skipped"),
        "expected a clean six-surface summary:\n{report}"
    );
    assert!(
        output.status.success(),
        "the kit must exit 0 when nothing failed:\n{report}"
    );
}

/// The kit is only useful if it fails loudly. Pointed at an endpoint that is not
/// an MCP peer at all, it must fail rather than pass vacuously, and exit 1.
#[tokio::test]
#[ignore]
async fn conformance_kit_fails_and_exits_nonzero_against_a_non_peer() {
    // Port 9 (discard) refuses connections, so every surface is unreachable.
    let output = tokio::task::spawn_blocking(|| {
        Command::new(env!("CARGO_BIN_EXE_ione-conformance"))
            .args(["--url", "http://127.0.0.1:9/mcp", "--pre-brokered"])
            .output()
            .expect("run ione-conformance")
    })
    .await
    .expect("join conformance run");

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(
        stdout.contains("[FAIL] 1. MCP server endpoint"),
        "an unreachable peer must fail surface 1:\n{stdout}"
    );
    assert!(
        stdout.contains("[FAIL] 4."),
        "an unreachable peer must fail surface 4:\n{stdout}"
    );
    assert!(
        stdout.contains("[FAIL] 5."),
        "an unreachable peer must fail surface 5:\n{stdout}"
    );
    assert!(
        stdout.contains("[FAIL] 6."),
        "an unreachable peer must fail surface 6:\n{stdout}"
    );
    assert_eq!(
        output.status.code(),
        Some(1),
        "the kit must exit 1 when any surface failed:\n{stdout}"
    );
}

/// Bad usage is distinguishable from a failing peer: exit code 2, not 1.
#[tokio::test]
#[ignore]
async fn conformance_kit_reports_usage_errors_with_exit_code_two() {
    let output = tokio::task::spawn_blocking(|| {
        Command::new(env!("CARGO_BIN_EXE_ione-conformance"))
            .args(["--nonsense"])
            .output()
            .expect("run ione-conformance")
    })
    .await
    .expect("join conformance run");

    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown option '--nonsense'"));
}
