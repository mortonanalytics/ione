//! Integration tests for federated catalog search
//! (md/plans/federated-catalog-search-plan.md). All three phases share this
//! file; tests are named `phase{N}_…` so each phase gate can run its slice.
//!
//! Run: IONE_SKIP_LIVE=1 cargo test --test catalog_search_integration -- --ignored --test-threads=1

use std::net::SocketAddr;

use chrono::Utc;
use ione::repos::PeerRepo;
use ione::services::federation::{reindex_peer_catalog, PeerManifest};
use ione::state::AppState;
use reqwest::header::COOKIE;
use reqwest::StatusCode;
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;
use uuid::Uuid;

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione";

async fn spawn_app() -> (String, PgPool, AppState) {
    // OIDC mode so the HTTP tests can present a session cookie and reach the
    // endpoint as an authenticated (is_oidc) caller; Phase-1 tests call the
    // service directly and are unaffected by the mode.
    std::env::set_var("IONE_AUTH_MODE", "oidc");

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
        "TRUNCATE peer_catalog_entries, webhook_events_seen, workspace_peer_bindings,
                  audit_events, pipeline_events, approvals, artifacts,
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
    let state_ret = state.clone();
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("server error");
    });
    (format!("http://{}", addr), pool, state_ret)
}

async fn default_org_id(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("SELECT id FROM organizations ORDER BY created_at LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("default org")
}

/// Insert an `active` peer with a fixed `tool_prefix` in the default org and
/// return the loaded `Peer`. `org_id` is set by the peers org-id trigger.
async fn seed_peer(pool: &PgPool, name: &str, prefix: &str) -> ione::models::Peer {
    let org_id = default_org_id(pool).await;
    let issuer_id: Uuid = sqlx::query_scalar(
        "INSERT INTO trust_issuers (org_id, issuer_url, audience, jwks_uri, claim_mapping)
         VALUES ($1, $2, 'mcp', 'local', '{}'::jsonb)
         RETURNING id",
    )
    .bind(org_id)
    .bind(format!("https://issuer-{prefix}.example.com"))
    .fetch_one(pool)
    .await
    .expect("trust issuer");

    let peer_id: Uuid = sqlx::query_scalar(
        "INSERT INTO peers (name, mcp_url, issuer_id, sharing_policy, status, tool_allowlist, tool_prefix)
         VALUES ($1, $2, $3, '{}'::jsonb, 'active'::peer_status, '[]'::jsonb, $4)
         RETURNING id",
    )
    .bind(name)
    .bind(format!("https://{prefix}.example.com"))
    .bind(issuer_id)
    .bind(prefix)
    .fetch_one(pool)
    .await
    .expect("peer");

    PeerRepo::new(pool.clone())
        .get(peer_id)
        .await
        .expect("get peer")
        .expect("peer exists")
}

fn tool(name: &str, description: &str, props: &[&str]) -> Value {
    let properties: serde_json::Map<String, Value> = props
        .iter()
        .map(|p| (p.to_string(), json!({ "type": "string" })))
        .collect();
    json!({
        "name": name,
        "description": description,
        "inputSchema": { "type": "object", "properties": properties }
    })
}

fn manifest(peer_id: Uuid, tools: Vec<Value>) -> PeerManifest {
    PeerManifest {
        peer_id,
        tools,
        resources: vec![],
        fetched_at: Utc::now(),
        etag: None,
        stale: false,
    }
}

// ── Phase 1 ────────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn phase1_index_from_manifest() {
    let (_base, pool, state) = spawn_app().await;
    let peer = seed_peer(&pool, "Weather Peer", "weatherpeer").await;
    let m = manifest(
        peer.id,
        vec![
            tool("get_forecast", "weather forecast", &["lat", "lon"]),
            tool("get_alerts", "weather alerts", &["zone"]),
            tool("get_radar", "radar imagery", &[]),
        ],
    );
    reindex_peer_catalog(&state, &peer, &m)
        .await
        .expect("reindex");

    let rows: Vec<(String, bool)> = sqlx::query_as(
        "SELECT namespaced_name, tsv IS NOT NULL AS has_tsv
         FROM peer_catalog_entries
         WHERE org_id = $1 AND peer_id = $2 AND kind = 'tool'
         ORDER BY namespaced_name",
    )
    .bind(peer.org_id)
    .bind(peer.id)
    .fetch_all(&pool)
    .await
    .expect("query rows");

    assert_eq!(rows.len(), 3, "expected 3 tool rows");
    for (namespaced_name, has_tsv) in &rows {
        assert!(has_tsv, "tsv must be populated for {namespaced_name}");
        assert!(
            namespaced_name.starts_with("weatherpeer:"),
            "namespaced_name must be <tool_prefix>:<raw>, got {namespaced_name}"
        );
    }
    // Exact match to the prefix:name route_tool_call splits on.
    assert!(rows.iter().any(|(n, _)| n == "weatherpeer:get_forecast"));
}

#[tokio::test]
#[ignore]
async fn phase1_delta_no_churn() {
    let (_base, pool, state) = spawn_app().await;
    let peer = seed_peer(&pool, "Weather Peer", "weatherpeer").await;
    let m1 = manifest(
        peer.id,
        vec![
            tool("get_forecast", "weather forecast", &["lat"]),
            tool("get_alerts", "weather alerts", &["zone"]),
        ],
    );
    reindex_peer_catalog(&state, &peer, &m1)
        .await
        .expect("reindex 1");

    let before: Vec<(String, chrono::DateTime<Utc>)> = sqlx::query_as(
        "SELECT namespaced_name, updated_at FROM peer_catalog_entries
         WHERE peer_id = $1 ORDER BY namespaced_name",
    )
    .bind(peer.id)
    .fetch_all(&pool)
    .await
    .expect("before");

    // Re-index with identical content: no row's updated_at advances.
    reindex_peer_catalog(&state, &peer, &m1)
        .await
        .expect("reindex 2");
    let after_same: Vec<(String, chrono::DateTime<Utc>)> = sqlx::query_as(
        "SELECT namespaced_name, updated_at FROM peer_catalog_entries
         WHERE peer_id = $1 ORDER BY namespaced_name",
    )
    .bind(peer.id)
    .fetch_all(&pool)
    .await
    .expect("after same");
    assert_eq!(
        before, after_same,
        "unchanged manifest must not bump updated_at"
    );

    // Change exactly one tool's description: only that row advances.
    let m2 = manifest(
        peer.id,
        vec![
            tool("get_forecast", "weather forecast UPDATED", &["lat"]),
            tool("get_alerts", "weather alerts", &["zone"]),
        ],
    );
    reindex_peer_catalog(&state, &peer, &m2)
        .await
        .expect("reindex 3");
    let after_change: Vec<(String, chrono::DateTime<Utc>)> = sqlx::query_as(
        "SELECT namespaced_name, updated_at FROM peer_catalog_entries
         WHERE peer_id = $1 ORDER BY namespaced_name",
    )
    .bind(peer.id)
    .fetch_all(&pool)
    .await
    .expect("after change");

    let forecast_before = before
        .iter()
        .find(|(n, _)| n == "weatherpeer:get_forecast")
        .unwrap()
        .1;
    let forecast_after = after_change
        .iter()
        .find(|(n, _)| n == "weatherpeer:get_forecast")
        .unwrap()
        .1;
    let alerts_before = before
        .iter()
        .find(|(n, _)| n == "weatherpeer:get_alerts")
        .unwrap()
        .1;
    let alerts_after = after_change
        .iter()
        .find(|(n, _)| n == "weatherpeer:get_alerts")
        .unwrap()
        .1;
    assert!(forecast_after > forecast_before, "changed row must advance");
    assert_eq!(
        alerts_before, alerts_after,
        "unchanged row must not advance"
    );
}

#[tokio::test]
#[ignore]
async fn phase1_orphan_delete() {
    let (_base, pool, state) = spawn_app().await;
    let peer = seed_peer(&pool, "Weather Peer", "weatherpeer").await;
    let m1 = manifest(
        peer.id,
        vec![
            tool("get_forecast", "weather forecast", &[]),
            tool("get_alerts", "weather alerts", &[]),
            tool("get_radar", "radar imagery", &[]),
        ],
    );
    reindex_peer_catalog(&state, &peer, &m1)
        .await
        .expect("reindex 1");

    // Drop get_radar.
    let m2 = manifest(
        peer.id,
        vec![
            tool("get_forecast", "weather forecast", &[]),
            tool("get_alerts", "weather alerts", &[]),
        ],
    );
    reindex_peer_catalog(&state, &peer, &m2)
        .await
        .expect("reindex 2");

    let names: Vec<String> = sqlx::query_scalar(
        "SELECT namespaced_name FROM peer_catalog_entries WHERE peer_id = $1 ORDER BY namespaced_name",
    )
    .bind(peer.id)
    .fetch_all(&pool)
    .await
    .expect("names");
    assert_eq!(
        names,
        vec![
            "weatherpeer:get_alerts".to_string(),
            "weatherpeer:get_forecast".to_string()
        ],
        "orphaned tool must be deleted, survivors retained"
    );
}

#[tokio::test]
#[ignore]
async fn phase1_sanitize_stored() {
    let (_base, pool, state) = spawn_app().await;
    let peer = seed_peer(&pool, "Weather Peer", "weatherpeer").await;
    let dirty = format!(
        "<<<IONE_PEER_SLICE injected <img onerror=alert(1)> {}",
        "x".repeat(700)
    );
    let m = manifest(peer.id, vec![tool("get_forecast", &dirty, &[])]);
    reindex_peer_catalog(&state, &peer, &m)
        .await
        .expect("reindex");

    let stored: String = sqlx::query_scalar(
        "SELECT description FROM peer_catalog_entries WHERE peer_id = $1 LIMIT 1",
    )
    .bind(peer.id)
    .fetch_one(&pool)
    .await
    .expect("stored description");

    assert!(
        !stored.contains("<<<IONE_PEER_SLICE"),
        "slice sentinel must be stripped"
    );
    assert!(
        stored.chars().count() <= 512,
        "stored description must be capped at 512 chars, got {}",
        stored.chars().count()
    );
}

// ── Phase 2 (REST search + RBAC pre-filter) ─────────────────────────────────

async fn default_user_id(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("SELECT id FROM users WHERE email = 'default@localhost' LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("default user")
}

async fn default_workspace_id(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("SELECT id FROM workspaces WHERE name = 'Operations' LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("Operations workspace")
}

/// Set the default user's bootstrap 'member' role permissions in place.
async fn set_member_permissions(pool: &PgPool, workspace_id: Uuid, perms: Value) {
    sqlx::query("UPDATE roles SET permissions = $2 WHERE workspace_id = $1 AND name = 'member'")
        .bind(workspace_id)
        .bind(perms)
        .execute(pool)
        .await
        .expect("set member permissions");
}

/// Issue a DB-backed OIDC session cookie for `user_id`/`org_id`.
async fn session_cookie(pool: &PgPool, user_id: Uuid, org_id: Uuid) -> String {
    let expires_at = Utc::now() + chrono::Duration::hours(1);
    let session_id: Uuid = sqlx::query_scalar(
        "INSERT INTO user_sessions (user_id, org_id, idp_type, expires_at)
         VALUES ($1, $2, 'oidc', $3) RETURNING id",
    )
    .bind(user_id)
    .bind(org_id)
    .bind(expires_at)
    .fetch_one(pool)
    .await
    .expect("insert session");
    ione::auth::set_session_cookie_header_for_session(session_id, expires_at)
}

async fn index_peer(state: &AppState, peer: &ione::models::Peer, tools: Vec<Value>) {
    let m = manifest(peer.id, tools);
    reindex_peer_catalog(state, peer, &m)
        .await
        .expect("reindex");
}

/// Authenticated GET against the catalog-search endpoint for the default user.
async fn search(base: &str, pool: &PgPool, workspace_id: Uuid, query: &str) -> (StatusCode, Value) {
    let org_id = default_org_id(pool).await;
    let user_id = default_user_id(pool).await;
    let cookie = session_cookie(pool, user_id, org_id).await;
    let url = format!("{base}/api/v1/workspaces/{workspace_id}/catalog-search");
    let resp = reqwest::Client::new()
        .get(&url)
        .query(&[("q", query)])
        .header(COOKIE, cookie)
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body = resp.json::<Value>().await.unwrap_or(Value::Null);
    (status, body)
}

fn names(body: &Value) -> Vec<String> {
    body.get("items")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|i| i.get("namespaced_name").and_then(Value::as_str))
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
#[ignore]
async fn phase2_flood_ranks_finance_absent() {
    let (base, pool, state) = spawn_app().await;
    let ws = default_workspace_id(&pool).await;
    let peer = seed_peer(&pool, "weatherpeer", "weatherpeer").await;
    index_peer(
        &state,
        &peer,
        vec![
            tool("get_flood", "flood risk inundation outlook", &[]),
            tool("get_finance", "financial risk exposure", &[]),
        ],
    )
    .await;
    set_member_permissions(&pool, ws, json!(["tool_invoke:weatherpeer:*"])).await;

    let (status, body) = search(&base, &pool, ws, "flood risk").await;
    assert_eq!(status, StatusCode::OK);
    let got = names(&body);
    assert!(
        got.contains(&"weatherpeer:get_flood".to_string()),
        "hydrology tool must appear: {got:?}"
    );
    assert!(
        !got.contains(&"weatherpeer:get_finance".to_string()),
        "finance tool must be absent (AND-semantics): {got:?}"
    );
}

#[tokio::test]
#[ignore]
async fn phase2_prefilter_hides_uninvokable() {
    let (base, pool, state) = spawn_app().await;
    let ws = default_workspace_id(&pool).await;
    let weather = seed_peer(&pool, "weatherpeer", "weatherpeer").await;
    let fin = seed_peer(&pool, "finpeer", "finpeer").await;
    index_peer(
        &state,
        &weather,
        vec![tool("get_storm", "storm surge flood", &[])],
    )
    .await;
    index_peer(
        &state,
        &fin,
        vec![tool("get_trade", "trade flood liquidity", &[])],
    )
    .await;
    // Caller can invoke weatherpeer tools only.
    set_member_permissions(&pool, ws, json!(["tool_invoke:weatherpeer:*"])).await;

    let (status, body) = search(&base, &pool, ws, "flood").await;
    assert_eq!(status, StatusCode::OK);
    let got = names(&body);
    assert!(
        got.contains(&"weatherpeer:get_storm".to_string()),
        "invokable tool must appear: {got:?}"
    );
    assert!(
        !got.contains(&"finpeer:get_trade".to_string()),
        "uninvokable finpeer tool must be hidden: {got:?}"
    );
}

#[tokio::test]
#[ignore]
async fn phase2_injection_string_200() {
    let (base, pool, state) = spawn_app().await;
    let ws = default_workspace_id(&pool).await;
    let peer = seed_peer(&pool, "weatherpeer", "weatherpeer").await;
    index_peer(&state, &peer, vec![tool("get_flood", "flood risk", &[])]).await;
    set_member_permissions(&pool, ws, json!(["tool_invoke:weatherpeer:*"])).await;

    let (status, _body) = search(&base, &pool, ws, "' | (select 1) &").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "websearch_to_tsquery must not parse-error on injection input"
    );
}

#[tokio::test]
#[ignore]
async fn phase2_short_query_400() {
    let (base, pool, _state) = spawn_app().await;
    let ws = default_workspace_id(&pool).await;
    set_member_permissions(&pool, ws, json!(["tool_invoke:*:*"])).await;

    let (status, _body) = search(&base, &pool, ws, "a").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
#[ignore]
async fn phase2_limit_clamp() {
    let (base, pool, state) = spawn_app().await;
    let ws = default_workspace_id(&pool).await;
    let peer = seed_peer(&pool, "weatherpeer", "weatherpeer").await;
    let tools: Vec<Value> = (0..60)
        .map(|i| tool(&format!("get_flood_{i}"), "flood watch advisory", &[]))
        .collect();
    index_peer(&state, &peer, tools).await;
    set_member_permissions(&pool, ws, json!(["tool_invoke:weatherpeer:*"])).await;

    let org_id = default_org_id(&pool).await;
    let user_id = default_user_id(&pool).await;
    let cookie = session_cookie(&pool, user_id, org_id).await;
    let url = format!("{base}/api/v1/workspaces/{ws}/catalog-search");
    let resp = reqwest::Client::new()
        .get(&url)
        .query(&[("q", "flood"), ("limit", "500")])
        .header(COOKIE, cookie)
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.json::<Value>().await.expect("json");
    let count = body.get("items").and_then(Value::as_array).unwrap().len();
    assert_eq!(count, 50, "limit must clamp to 50, got {count}");
}

// ── Phase 3 (search_catalog MCP tool) ───────────────────────────────────────

fn authed_ctx(user_id: Uuid, org_id: Uuid) -> ione::auth::AuthContext {
    ione::auth::AuthContext {
        user_id,
        org_id,
        is_oidc: true,
        is_mcp_peer: false,
        active_role_id: None,
        session_id: None,
        mfa_verified: true,
        is_service_account: false,
        service_account_token_id: None,
        permissions: vec![],
    }
}

fn unauth_ctx(user_id: Uuid, org_id: Uuid) -> ione::auth::AuthContext {
    // The default-user fallback: real user_id, but neither OIDC nor SA.
    let mut ctx = authed_ctx(user_id, org_id);
    ctx.is_oidc = false;
    ctx.mfa_verified = false;
    ctx
}

fn result_names(value: &Value) -> Vec<String> {
    value
        .get("results")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|r| r.get("namespaced_name").and_then(Value::as_str))
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
#[ignore]
async fn phase3_unauth_mcp_rejected() {
    let (_base, pool, state) = spawn_app().await;
    let ws = default_workspace_id(&pool).await;
    let org_id = default_org_id(&pool).await;
    let user_id = default_user_id(&pool).await;
    let peer = seed_peer(&pool, "weatherpeer", "weatherpeer").await;
    index_peer(&state, &peer, vec![tool("get_flood", "flood risk", &[])]).await;
    set_member_permissions(&pool, ws, json!(["tool_invoke:weatherpeer:*"])).await;

    // Default-user fallback (not OIDC, not SA) must be rejected — never served
    // the default user's permissions (FCS-C2 / AC-6).
    let result = ione::mcp_server::call_tool(
        "search_catalog",
        json!({ "workspace_id": ws, "query": "flood risk" }),
        &unauth_ctx(user_id, org_id),
        &state,
    )
    .await;
    assert!(result.is_err(), "unauthenticated MCP call must error");
}

#[tokio::test]
#[ignore]
async fn phase3_untrusted_flag() {
    let (_base, pool, state) = spawn_app().await;
    let ws = default_workspace_id(&pool).await;
    let org_id = default_org_id(&pool).await;
    let user_id = default_user_id(&pool).await;
    let peer = seed_peer(&pool, "weatherpeer", "weatherpeer").await;
    index_peer(
        &state,
        &peer,
        vec![
            tool("get_flood", "flood risk inundation", &[]),
            tool("get_storm", "storm flood risk", &[]),
        ],
    )
    .await;
    set_member_permissions(&pool, ws, json!(["tool_invoke:weatherpeer:*"])).await;

    let out = ione::mcp_server::call_tool(
        "search_catalog",
        json!({ "workspace_id": ws, "query": "flood risk" }),
        &authed_ctx(user_id, org_id),
        &state,
    )
    .await
    .expect("mcp search ok");
    let rows = out
        .get("results")
        .and_then(Value::as_array)
        .expect("results");
    assert!(!rows.is_empty(), "expected at least one result");
    for r in rows {
        assert_eq!(
            r.get("untrusted_content"),
            Some(&Value::Bool(true)),
            "each result must carry untrusted_content:true"
        );
        // MCP response omits peer_id/peer_name by design.
        assert!(r.get("peer_id").is_none(), "MCP result must omit peer_id");
        assert!(
            r.get("peer_name").is_none(),
            "MCP result must omit peer_name"
        );
    }
}

#[tokio::test]
#[ignore]
async fn phase3_rest_mcp_parity() {
    let (base, pool, state) = spawn_app().await;
    let ws = default_workspace_id(&pool).await;
    let org_id = default_org_id(&pool).await;
    let user_id = default_user_id(&pool).await;
    let peer = seed_peer(&pool, "weatherpeer", "weatherpeer").await;
    index_peer(
        &state,
        &peer,
        vec![
            tool("get_flood", "flood risk inundation", &[]),
            tool("get_storm", "storm flood risk", &[]),
            tool("get_finance", "financial risk exposure", &[]),
        ],
    )
    .await;
    set_member_permissions(&pool, ws, json!(["tool_invoke:weatherpeer:*"])).await;

    // REST (session cookie, same default user).
    let (status, body) = search(&base, &pool, ws, "flood risk").await;
    assert_eq!(status, StatusCode::OK);
    let rest = names(&body);

    // MCP (shared CatalogService, same caller identity).
    let out = ione::mcp_server::call_tool(
        "search_catalog",
        json!({ "workspace_id": ws, "query": "flood risk" }),
        &authed_ctx(user_id, org_id),
        &state,
    )
    .await
    .expect("mcp search ok");
    let mcp = result_names(&out);

    assert!(!rest.is_empty(), "expected non-empty result set");
    assert_eq!(
        rest, mcp,
        "REST and MCP must return the same ranked invokable set"
    );
}
