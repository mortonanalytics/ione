//! Peer join lifecycle, end to end — issue #13 (AC-6 / OQ-2).
//!
//! Everything else in the suite starts from a peer that is already `active`,
//! inserted straight into `peers`. This file starts from a stranger: an MCP URL
//! and nothing else. It drives the real state machine
//!
//!   `pending_oauth` → (OAuth callback) → `pending_allowlist` → (authorize) → `active`
//!
//! through IONe's public HTTP surface against a peer that runs its own OAuth 2.1
//! authorization server, then binds that peer into a workspace from its
//! `whoami://`, renders its map/chart/table refs in the shell, and accepts a
//! signed webhook from it — the "one pane of glass" path, as one flow.
//!
//! What is proven here that `stub_peer_conformance_integration.rs` does not
//! prove: the join itself. Discovery, dynamic client registration, the PKCE
//! S256 binding IONe actually transmitted, the single-use callback nonce,
//! token ciphertext at rest, and the two gates that stand between a stranger
//! and a rendered panel (`Active` required to subscribe, `tool_invoke` required
//! to invoke).
//!
//! AC-6 / OQ-2 is asserted structurally rather than asserted once: every shell
//! assertion runs twice, against two stub instances that share no resource URI,
//! tenant id, axis name or column name. An assertion that passes against both
//! cannot be reading a fixture-specific field out of IONe.
//!
//! Prerequisites:
//!   docker compose up -d postgres
//!
//! Run (serial, ignored):
//!   SQLX_OFFLINE=true IONE_SKIP_LIVE=1 \
//!     DATABASE_URL=postgres://ione:ione@localhost:5433/ione_w13 \
//!     cargo test --test peer_lifecycle_e2e_integration -- --ignored --test-threads=1

use std::collections::HashMap;
use std::net::SocketAddr;

use reqwest::{redirect::Policy, StatusCode};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;
use uuid::Uuid;

use ione::auth::AuthContext;
use ione::state::AppState;
use ione::util::token_crypto::decrypt_token;

mod support;
use support::stub_peer::{
    pkce_s256, StubPeer, StubPeerProfile, WebhookEvent, TOOL_ACKNOWLEDGE, TOOL_QUERY,
};

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione_w13";
const TEST_STATIC_BEARER: &str = "peer-lifecycle-e2e-test-bearer";
const TEST_KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

// ─── Harness ──────────────────────────────────────────────────────────────────

struct Harness {
    base: String,
    pool: PgPool,
    state: AppState,
}

/// Boot IONe on an ephemeral port with `IONE_OAUTH_ISSUER` pointing at that same
/// port, so the `redirect_uri` IONe hands the peer is a URL the peer can really
/// redirect an operator's browser to — which is what makes the callback leg of
/// this test the real one rather than a synthesized request.
async fn spawn_app() -> Harness {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr: SocketAddr = listener.local_addr().expect("local addr");
    let base = format!("http://{addr}");

    std::env::set_var("IONE_AUTH_MODE", "local");
    std::env::set_var("IONE_OAUTH_STATIC_BEARER", TEST_STATIC_BEARER);
    std::env::set_var("IONE_TOKEN_KEY", TEST_KEY);
    std::env::set_var("IONE_WEBHOOK_SECRET_KEY", TEST_KEY);
    std::env::set_var("IONE_OAUTH_ISSUER", &base);
    // Both stub instances live on loopback; §2's host-match rule then requires
    // their advertised endpoints to be loopback too.
    std::env::set_var("IONE_ALLOW_PRIVATE_PEERS", "1");
    std::env::set_var("IONE_PRIVATE_PEER_ALLOWLIST", "127.0.0.1,localhost");

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
        "TRUNCATE peer_oauth_pending, pending_peer_tool_calls, webhook_events_seen,
                  workspace_peer_credentials, workspace_peer_delegations,
                  workspace_peer_bindings, interaction_events, org_memberships,
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

    let (router, state) = ione::app_with_state(pool.clone()).await;
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("server error");
    });

    let harness = Harness { base, pool, state };
    harness.grant_operator_permissions(&["peers:manage"]).await;
    harness
}

impl Harness {
    async fn org_id(&self) -> Uuid {
        sqlx::query_scalar("SELECT id FROM organizations WHERE name = 'Default Org' LIMIT 1")
            .fetch_one(&self.pool)
            .await
            .expect("Default Org not found")
    }

    async fn workspace_id(&self) -> Uuid {
        sqlx::query_scalar("SELECT id FROM workspaces WHERE name = 'Operations' LIMIT 1")
            .fetch_one(&self.pool)
            .await
            .expect("Operations workspace not found")
    }

    async fn user_id(&self) -> Uuid {
        sqlx::query_scalar("SELECT id FROM users ORDER BY created_at ASC LIMIT 1")
            .fetch_one(&self.pool)
            .await
            .expect("default user not found")
    }

    /// Peer registration and allowlist authorization are org-scoped
    /// (`routes/peers.rs:69`, `:245`); subscribe is workspace-scoped
    /// (`routes/peers.rs:284`). The operator who joins a peer holds both, so
    /// grant the same set in both places.
    async fn grant_operator_permissions(&self, permissions: &[&str]) {
        let perms = json!(permissions);
        let user_id = self.user_id().await;
        let org_id = self.org_id().await;
        let workspace_id = self.workspace_id().await;
        sqlx::query(
            "INSERT INTO org_memberships (user_id, org_id, permissions)
             VALUES ($1, $2, $3)
             ON CONFLICT (user_id, org_id) DO UPDATE SET permissions = EXCLUDED.permissions",
        )
        .bind(user_id)
        .bind(org_id)
        .bind(&perms)
        .execute(&self.pool)
        .await
        .expect("grant org permissions");
        sqlx::query(
            "UPDATE roles SET permissions = $2 WHERE workspace_id = $1 AND name = 'member'",
        )
        .bind(workspace_id)
        .bind(&perms)
        .execute(&self.pool)
        .await
        .expect("grant workspace permissions");
    }

    fn client(&self) -> reqwest::Client {
        reqwest::Client::builder()
            .redirect(Policy::none())
            .build()
            .expect("reqwest client")
    }

    async fn get_json(&self, path: &str) -> (StatusCode, Value) {
        let response = self
            .client()
            .get(format!("{}{path}", self.base))
            .bearer_auth(TEST_STATIC_BEARER)
            .send()
            .await
            .expect("request");
        let status = response.status();
        (status, response.json().await.expect("json"))
    }

    async fn post_json(&self, path: &str, body: Value) -> (StatusCode, Value) {
        let response = self
            .client()
            .post(format!("{}{path}", self.base))
            .bearer_auth(TEST_STATIC_BEARER)
            .json(&body)
            .send()
            .await
            .expect("request");
        let status = response.status();
        (status, response.json().await.expect("json"))
    }

    async fn peer_status(&self, peer_id: Uuid) -> String {
        sqlx::query_scalar("SELECT status::text FROM peers WHERE id = $1")
            .bind(peer_id)
            .fetch_one(&self.pool)
            .await
            .expect("peer status")
    }

    fn auth_context(&self, user_id: Uuid, org_id: Uuid) -> AuthContext {
        AuthContext {
            user_id,
            org_id,
            is_oidc: false,
            is_mcp_peer: false,
            active_role_id: None,
            session_id: None,
            mfa_verified: true,
            is_service_account: false,
            service_account_token_id: None,
            permissions: Vec::new(),
        }
    }
}

// ─── The join, step by step ───────────────────────────────────────────────────

/// `POST /api/v1/peers` — the peer is a stranger; IONe discovers it, registers
/// itself as a client, and parks in `pending_oauth`.
async fn begin_join(h: &Harness, stub: &StubPeer) -> (Uuid, String) {
    let (status, body) = h
        .post_json("/api/v1/peers", json!({ "peerUrl": stub.mcp_url }))
        .await;
    assert_eq!(status, StatusCode::OK, "peer registration failed: {body}");
    assert_eq!(body["status"], "pending_oauth", "{body}");
    let peer_id: Uuid = body["id"].as_str().expect("peer id").parse().expect("uuid");
    let authorize_url = body["authorizeUrl"]
        .as_str()
        .expect("authorizeUrl")
        .to_string();
    (peer_id, authorize_url)
}

/// Drive the operator's leg of the flow: hit the peer's `/oauth/authorize`, then
/// hand the resulting `?code&state` back to IONe's callback.
async fn complete_oauth(h: &Harness, authorize_url: &str) -> String {
    let response = h
        .client()
        .get(authorize_url)
        .send()
        .await
        .expect("peer authorize");
    assert_eq!(
        response.status(),
        StatusCode::FOUND,
        "the peer must redirect back with an authorization code"
    );
    let callback_url = response
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .expect("authorize redirect Location")
        .to_string();

    let response = h
        .client()
        .get(&callback_url)
        .send()
        .await
        .expect("ione callback");
    assert_eq!(
        response.status(),
        StatusCode::SEE_OTHER,
        "the callback must complete the token exchange and redirect to the peer page"
    );
    callback_url
}

async fn authorize_allowlist(h: &Harness, peer_id: Uuid, tools: &[&str]) {
    let (status, body) = h
        .post_json(
            &format!("/api/v1/peers/{peer_id}/authorize"),
            json!({ "toolAllowlist": tools }),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "allowlist authorization failed: {body}"
    );
    assert_eq!(body["status"], "active", "{body}");
}

async fn subscribe(h: &Harness, workspace_id: Uuid, peer_id: Uuid) -> (StatusCode, Value) {
    h.post_json(
        &format!("/api/v1/workspaces/{workspace_id}/peers/{peer_id}/subscribe"),
        json!({}),
    )
    .await
}

/// The whole lifecycle in one call, for tests whose subject is what happens
/// *after* a peer is joined.
async fn join_and_subscribe(h: &Harness, stub: &StubPeer, workspace_id: Uuid) -> Uuid {
    let (peer_id, authorize_url) = begin_join(h, stub).await;
    complete_oauth(h, &authorize_url).await;
    authorize_allowlist(h, peer_id, &[TOOL_QUERY]).await;
    let (status, body) = subscribe(h, workspace_id, peer_id).await;
    assert_eq!(status, StatusCode::OK, "subscribe failed: {body}");
    assert_eq!(body["binding"]["status"], "active", "{body}");
    peer_id
}

fn query_params(url: &str) -> HashMap<String, String> {
    url::Url::parse(url)
        .expect("url")
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect()
}

fn sha256_hex(input: &str) -> String {
    Sha256::digest(input.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

// ─── 1. The full join ─────────────────────────────────────────────────────────

/// A stranger becomes an `active`, workspace-bound peer, and every intermediate
/// state is the one the state machine promises.
#[tokio::test]
#[ignore]
async fn peer_joins_from_stranger_to_active_and_binds_to_workspace() {
    let h = spawn_app().await;
    let workspace_id = h.workspace_id().await;
    let stub = StubPeer::start().await;
    let profile = stub.profile().clone();

    // ── pending_oauth ────────────────────────────────────────────────────────
    let (peer_id, authorize_url) = begin_join(&h, &stub).await;
    assert_eq!(h.peer_status(peer_id).await, "pending_oauth");

    let registrations = stub.registrations();
    assert_eq!(
        registrations.len(),
        1,
        "IONe must dynamically register itself as a client before authorizing: {registrations:?}"
    );
    assert_eq!(
        registrations[0]["redirect_uris"][0],
        format!("{}/api/v1/peers/callback", h.base),
        "the registered redirect_uri must be IONe's own callback: {}",
        registrations[0]
    );

    let client_id: String = sqlx::query_scalar("SELECT oauth_client_id FROM peers WHERE id = $1")
        .bind(peer_id)
        .fetch_one(&h.pool)
        .await
        .expect("oauth_client_id");
    assert!(
        client_id.starts_with("stub-client-"),
        "the peer-issued client id must be persisted, got {client_id:?}"
    );

    let sent = query_params(&authorize_url);
    assert_eq!(sent.get("response_type").map(String::as_str), Some("code"));
    assert_eq!(
        sent.get("code_challenge_method").map(String::as_str),
        Some("S256"),
        "PKCE downgrade to 'plain' or to no challenge is not acceptable"
    );
    assert_eq!(sent.get("client_id"), Some(&client_id));
    let challenge = sent.get("code_challenge").expect("code_challenge").clone();
    let nonce = sent.get("state").expect("state nonce").clone();
    assert!(!challenge.is_empty() && !nonce.is_empty());

    let pending_nonce: String =
        sqlx::query_scalar("SELECT nonce FROM peer_oauth_pending WHERE peer_id = $1")
            .bind(peer_id)
            .fetch_one(&h.pool)
            .await
            .expect("pending oauth row");
    assert_eq!(pending_nonce, nonce);

    // ── pending_allowlist ────────────────────────────────────────────────────
    let callback_url = complete_oauth(&h, &authorize_url).await;
    assert_eq!(
        h.peer_status(peer_id).await,
        "pending_allowlist",
        "a completed token exchange must advance the peer, and no further"
    );

    // PKCE is real: the verifier IONe sent to the token endpoint hashes to the
    // challenge it sent to the authorization endpoint.
    let authorize_seen = stub.authorize_params();
    assert_eq!(authorize_seen.len(), 1);
    assert_eq!(authorize_seen[0].get("code_challenge"), Some(&challenge));
    let token_seen = stub.token_params();
    assert_eq!(
        token_seen.len(),
        1,
        "exactly one token exchange: {token_seen:?}"
    );
    assert_eq!(
        token_seen[0].get("grant_type").map(String::as_str),
        Some("authorization_code")
    );
    let verifier = token_seen[0].get("code_verifier").expect("code_verifier");
    assert_eq!(
        pkce_s256(verifier),
        challenge,
        "the code_verifier must hash (S256) to the transmitted code_challenge"
    );
    assert_ne!(
        verifier, &challenge,
        "sending the challenge as the verifier would be a 'plain' downgrade"
    );

    // Tokens are stored as ciphertext, never as plaintext.
    let issued = stub.issued_access_tokens();
    assert_eq!(issued.len(), 1);
    let access_plaintext = issued[0].clone();
    let (access_ct, refresh_ct, access_hash, refresh_hash): (
        Vec<u8>,
        Option<Vec<u8>>,
        String,
        String,
    ) = sqlx::query_as(
        "SELECT access_token_ciphertext, refresh_token_ciphertext,
                access_token_hash, refresh_token_hash
         FROM peers WHERE id = $1",
    )
    .bind(peer_id)
    .fetch_one(&h.pool)
    .await
    .expect("stored peer tokens");
    assert!(!access_ct.is_empty(), "the access token must be persisted");
    let refresh_ct = refresh_ct.expect("the refresh token must be persisted");
    assert!(
        !contains_bytes(&access_ct, access_plaintext.as_bytes()),
        "the access token must not appear in plaintext at rest"
    );
    assert!(!contains_bytes(&refresh_ct, access_plaintext.as_bytes()));
    assert_eq!(
        decrypt_token(&access_ct).expect("decrypt access token"),
        access_plaintext,
        "the stored ciphertext must decrypt to exactly the token the peer issued"
    );
    assert_eq!(access_hash, sha256_hex(&access_plaintext));
    assert_eq!(
        refresh_hash.len(),
        64,
        "refresh token hash must be sha256 hex"
    );

    // The callback nonce is single-use: replaying it fails, and IONe stops at
    // the nonce check — the peer's token endpoint is never hit a second time.
    let pending_rows: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM peer_oauth_pending WHERE peer_id = $1")
            .bind(peer_id)
            .fetch_one(&h.pool)
            .await
            .expect("pending count");
    assert_eq!(pending_rows, 0, "the pending nonce must be consumed");

    let replay = h.client().get(&callback_url).send().await.expect("replay");
    assert_eq!(
        replay.status(),
        StatusCode::BAD_REQUEST,
        "a replayed callback must be rejected"
    );
    assert_eq!(
        stub.token_params().len(),
        1,
        "the replay must be rejected before any second token exchange"
    );

    // ── the Active gate is real ──────────────────────────────────────────────
    let (status, body) = subscribe(&h, workspace_id, peer_id).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a peer still pending allowlist must not be subscribable: {body}"
    );
    let bindings: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM workspace_peer_bindings WHERE peer_id = $1")
            .bind(peer_id)
            .fetch_one(&h.pool)
            .await
            .expect("binding count");
    assert_eq!(bindings, 0, "a refused subscribe must not create a binding");
    let connectors: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM connectors WHERE config->>'peer_id' = $1")
            .bind(peer_id.to_string())
            .fetch_one(&h.pool)
            .await
            .expect("connector count");
    assert_eq!(
        connectors, 0,
        "a refused subscribe must not create a connector"
    );

    // ── active ───────────────────────────────────────────────────────────────
    authorize_allowlist(&h, peer_id, &[TOOL_QUERY]).await;
    assert_eq!(h.peer_status(peer_id).await, "active");
    let allowlist: Value = sqlx::query_scalar("SELECT tool_allowlist FROM peers WHERE id = $1")
        .bind(peer_id)
        .fetch_one(&h.pool)
        .await
        .expect("tool allowlist");
    assert_eq!(allowlist, json!([TOOL_QUERY]));

    // ── bound, from the peer's own whoami:// ─────────────────────────────────
    let (status, body) = subscribe(&h, workspace_id, peer_id).await;
    assert_eq!(status, StatusCode::OK, "subscribe failed: {body}");
    assert_eq!(body["binding"]["status"], "active", "{body}");
    assert_eq!(
        body["binding"]["foreignTenantId"],
        profile.foreign_tenant_id
    );
    assert_eq!(
        body["binding"]["foreignTenantName"],
        profile.foreign_tenant_name
    );
    assert_eq!(
        body["binding"]["foreignWorkspaceId"],
        profile.foreign_workspace_id
    );
    assert_eq!(
        body["binding"]["foreignUserEmail"],
        profile.foreign_user_email
    );
    assert_eq!(
        body["firstPollDeferred"], false,
        "an Active binding must not defer the first poll: {body}"
    );

    let methods = stub.methods_called();
    assert!(
        methods.iter().any(|m| m == "resources/read"),
        "the binding must come from a live whoami:// read: {methods:?}"
    );
}

// ─── 2. Rendering is contract-driven, not fixture-driven ──────────────────────

/// Assert the shell renders `profile`'s three view types, reading nothing but
/// `profile`. Run against every peer in the workspace; a peer-specific code path
/// in IONe would make one of the two runs fail.
async fn assert_shell_renders(
    h: &Harness,
    workspace_id: Uuid,
    peer_id: Uuid,
    profile: &StubPeerProfile,
) {
    // §4.2 — map
    let (status, body) = h
        .get_json(&format!("/api/v1/workspaces/{workspace_id}/map-layers"))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["peersFailed"].as_array().expect("peersFailed").len(),
        0
    );
    let layer = body["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|item| item["peerId"] == peer_id.to_string())
        .unwrap_or_else(|| panic!("no map layer for peer {peer_id}: {body}"));
    assert_eq!(layer["uri"], profile.map_uri);
    assert_eq!(layer["name"], profile.map_name);
    assert_eq!(layer["meta"]["tileUrl"], profile.map_tile_url);
    assert_eq!(layer["meta"]["layerName"], profile.map_layer_name);
    assert_eq!(layer["meta"]["attribution"], profile.map_attribution);
    assert_eq!(layer["meta"]["bounds"], profile.map_bounds);
    assert_eq!(layer["meta"]["vectorUrl"], profile.map_vector_url);

    // §4.3 — chart panel, then the chart body behind it
    let (status, body) = h
        .get_json(&format!("/api/v1/workspaces/{workspace_id}/chart-panels"))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["peerErrors"].as_array().expect("peerErrors").len(), 0);
    let chart = body["peerCharts"]
        .as_array()
        .expect("peerCharts")
        .iter()
        .find(|item| item["peerId"] == peer_id.to_string())
        .unwrap_or_else(|| panic!("no chart panel for peer {peer_id}: {body}"));
    assert_eq!(chart["uri"], profile.chart_uri);
    assert_eq!(chart["name"], profile.chart_name);
    assert_eq!(chart["source"], "peer");
    assert_eq!(chart["spec"]["chartType"], profile.chart_type);
    assert_eq!(chart["spec"]["xAxis"], profile.chart_x_axis);
    assert_eq!(chart["spec"]["yAxis"], profile.chart_y_axis);
    assert_eq!(chart["spec"]["series"], json!(profile.chart_series));

    let encoded = urlencoding::encode(&profile.chart_uri);
    let (status, body) = h
        .get_json(&format!(
            "/api/v1/workspaces/{workspace_id}/chart-data?peer_id={peer_id}&uri={encoded}"
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "chart-data failed: {body}");
    assert_eq!(body["rows"], json!(profile.chart_rows));

    // §4.4 — table panel, then the table body behind it
    let (status, body) = h
        .get_json(&format!("/api/v1/workspaces/{workspace_id}/table-panels"))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["peerErrors"].as_array().expect("peerErrors").len(), 0);
    let table = body["peerTables"]
        .as_array()
        .expect("peerTables")
        .iter()
        .find(|item| item["peerId"] == peer_id.to_string())
        .unwrap_or_else(|| panic!("no table panel for peer {peer_id}: {body}"));
    assert_eq!(table["uri"], profile.table_uri);
    assert_eq!(table["name"], profile.table_name);
    assert_eq!(table["source"], "peer");

    let encoded = urlencoding::encode(&profile.table_uri);
    let (status, body) = h
        .get_json(&format!(
            "/api/v1/workspaces/{workspace_id}/table-data?peer_id={peer_id}&uri={encoded}"
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "table-data failed: {body}");
    assert_eq!(body["schema"], json!(profile.table_schema));
    assert_eq!(body["rows"], json!(profile.table_rows));
}

/// Two peers that share nothing but the contract join the same workspace and
/// both render map, chart and table in the shell. This is AC-6 / OQ-2: the
/// panels are populated from `metadata.ione_view` alone.
#[tokio::test]
#[ignore]
async fn shell_renders_map_chart_and_table_for_two_unrelated_peers() {
    let h = spawn_app().await;
    let workspace_id = h.workspace_id().await;

    let first = StubPeer::start_with(StubPeerProfile::default()).await;
    let second = StubPeer::start_with(StubPeerProfile::terrayield()).await;

    // Nothing about these two fixtures overlaps, so a rendering path that keyed
    // off either one's names could not satisfy both.
    assert_ne!(first.profile().map_uri, second.profile().map_uri);
    assert_ne!(first.profile().chart_x_axis, second.profile().chart_x_axis);
    assert_ne!(first.profile().table_schema, second.profile().table_schema);
    assert_ne!(
        first.profile().foreign_tenant_id,
        second.profile().foreign_tenant_id
    );

    let first_id = join_and_subscribe(&h, &first, workspace_id).await;
    let second_id = join_and_subscribe(&h, &second, workspace_id).await;

    assert_shell_renders(&h, workspace_id, first_id, first.profile()).await;
    assert_shell_renders(&h, workspace_id, second_id, second.profile()).await;

    // The peer-authored §5 slice reaches the context surface for both, so the
    // fan-out is per-peer rather than a single winner.
    let (status, body) = h
        .get_json(&format!("/api/v1/workspaces/{workspace_id}/context-slices"))
        .await;
    assert_eq!(status, StatusCode::OK);
    let slices = body["items"].as_array().expect("items");
    for (peer_id, profile) in [(first_id, first.profile()), (second_id, second.profile())] {
        let slice = slices
            .iter()
            .find(|entry| entry["peerId"] == peer_id.to_string())
            .unwrap_or_else(|| panic!("no context slice for peer {peer_id}: {body}"));
        assert_eq!(slice["body"]["schema_version"], "1");
        assert_eq!(slice["body"]["peer_id"], profile.self_peer_id);
    }
}

/// The claim behind AC-6 stated as a static fact, not only as behaviour: the
/// names either fixture invents appear nowhere in IONe's source.
///
/// Needs no database, but is `#[ignore]`d with the rest of the file so it runs
/// under the suite's `-- --ignored` invocation rather than being filtered out.
#[test]
#[ignore]
fn ione_source_contains_no_peer_specific_identifiers() {
    let mut offenders = Vec::new();
    let mut stack = vec![std::path::PathBuf::from("src")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("read src") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path)
                .expect("read source file")
                .to_ascii_lowercase();
            for needle in [
                "terrayield",
                "bushels_per_acre",
                "harvested_pct",
                "ty-grower",
                "stub://",
                "stub-tenant",
                "displacement_mm",
            ] {
                if text.contains(needle) {
                    offenders.push(format!("{}: {needle}", path.display()));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "IONe must not name any peer's data: {offenders:?}"
    );
}

// ─── 3. Signed webhook fan-in ─────────────────────────────────────────────────

/// A `publication.created` event signed by a joined peer becomes one signal in
/// the workspace its binding points at, at the severity the peer declared; a
/// replay of the same event id is an idempotent no-op.
#[tokio::test]
#[ignore]
async fn signed_publication_created_webhook_fans_in_once_and_replays_are_suppressed() {
    let h = spawn_app().await;
    let workspace_id = h.workspace_id().await;
    let stub = StubPeer::start().await;
    let peer_id = join_and_subscribe(&h, &stub, workspace_id).await;

    let (status, body) = h
        .post_json(
            &format!("/api/v1/peers/{peer_id}/webhook/provision"),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "provision failed: {body}");
    stub.set_webhook_config(
        peer_id,
        body["signingSecret"].as_str().expect("signingSecret"),
    );

    let mut event = WebhookEvent::new("publication.created", "flagged");
    event.data = json!({ "publication_id": "pub-4417", "title": "Q3 parcel revision" });

    let response = stub.emit_webhook_event(&h.base, &event).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "the peer's signature must verify"
    );
    let ack: Value = response.json().await.expect("json");
    assert_eq!(ack["ok"], true);
    assert_eq!(ack["duplicate"], false);
    // §3.5 spells this `signalIds`, and `WebhookAckResponse` does serialize it
    // that way (`routes/webhooks.rs` carries `rename_all = "camelCase"`).
    assert_eq!(
        ack["signalIds"].as_array().expect("signal ids").len(),
        1,
        "one envelope, one binding, one signal: {ack}"
    );

    let (title, severity, approval_required, signal_workspace, evidence_peer): (
        String,
        String,
        bool,
        Uuid,
        String,
    ) = sqlx::query_as(
        "SELECT title, severity::text, approval_required, workspace_id, evidence->>'peer_id'
         FROM signals WHERE evidence->>'event_id' = $1",
    )
    .bind(&event.id)
    .fetch_one(&h.pool)
    .await
    .expect("webhook signal");
    assert_eq!(title, "publication.created");
    assert_eq!(severity, "flagged", "declared severity must survive intact");
    assert!(
        approval_required,
        "§3.3: flagged always gates, regardless of the envelope flag"
    );
    assert_eq!(signal_workspace, workspace_id);
    assert_eq!(evidence_peer, peer_id.to_string());

    // Replay: same event id, freshly signed, inside the replay window.
    let response = stub.emit_webhook_event(&h.base, &event).await;
    assert_eq!(response.status(), StatusCode::OK);
    let ack: Value = response.json().await.expect("json");
    assert_eq!(ack["ok"], true);
    assert_eq!(ack["duplicate"], true, "a repeated event id is a duplicate");
    assert!(
        ack.get("signalIds").map(Value::is_null).unwrap_or(true),
        "a duplicate reports no new signals: {ack}"
    );

    let signals: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM signals WHERE evidence->>'event_id' = $1")
            .bind(&event.id)
            .fetch_one(&h.pool)
            .await
            .expect("signal count");
    assert_eq!(signals, 1, "the replay must not create a second signal");
}

/// A peer that has not finished the join cannot push events, even holding a
/// valid signing secret and signing correctly (§3.4).
#[tokio::test]
#[ignore]
async fn webhook_from_a_peer_that_is_not_active_is_unauthorized() {
    let h = spawn_app().await;
    let stub = StubPeer::start().await;
    let (peer_id, authorize_url) = begin_join(&h, &stub).await;
    complete_oauth(&h, &authorize_url).await;
    assert_eq!(h.peer_status(peer_id).await, "pending_allowlist");

    let (status, body) = h
        .post_json(
            &format!("/api/v1/peers/{peer_id}/webhook/provision"),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "provision failed: {body}");
    stub.set_webhook_config(
        peer_id,
        body["signingSecret"].as_str().expect("signingSecret"),
    );

    let event = WebhookEvent::new("publication.created", "flagged");
    let response = stub.emit_webhook_event(&h.base, &event).await;
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "only an active peer may push events"
    );

    let signals: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM signals")
        .fetch_one(&h.pool)
        .await
        .expect("signal count");
    assert_eq!(signals, 0);
    let seen: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM webhook_events_seen")
        .fetch_one(&h.pool)
        .await
        .expect("seen count");
    assert_eq!(
        seen, 0,
        "a rejected event must leave no dedup row, so it is safe to retry"
    );
}

// ─── 4. The invocation gate ───────────────────────────────────────────────────

/// A joined peer's tools are not automatically callable: `route_tool_call`
/// requires a matching `tool_invoke:<peer>:<tool>` grant for the workspace
/// (C-2 / DICE §2.4), and a denial never reaches the peer.
///
/// Note the gate under test is the `tool_invoke` grant, not `peers.tool_allowlist`.
/// The allowlist written by `POST /api/v1/peers/:id/authorize` is read in exactly
/// one place, `services/delivery.rs:561`, which gates IONe's *outbound*
/// `propose_artifact` delivery; it is not consulted on the inbound invocation
/// path. Asserted here as it behaves, so the divergence is visible rather than
/// assumed away.
#[tokio::test]
#[ignore]
async fn a_tool_the_caller_was_not_granted_is_not_invokable() {
    let h = spawn_app().await;
    let workspace_id = h.workspace_id().await;
    let stub = StubPeer::start().await;
    let peer_id = join_and_subscribe(&h, &stub, workspace_id).await;

    let (peer_name, prefix): (String, String) =
        sqlx::query_as("SELECT name, tool_prefix FROM peers WHERE id = $1")
            .bind(peer_id)
            .fetch_one(&h.pool)
            .await
            .expect("peer name and prefix");

    // Exactly the tool that was allowlisted at authorize time, and nothing else.
    let allowlist: Value = sqlx::query_scalar("SELECT tool_allowlist FROM peers WHERE id = $1")
        .bind(peer_id)
        .fetch_one(&h.pool)
        .await
        .expect("tool allowlist");
    assert_eq!(allowlist, json!([TOOL_QUERY]));

    h.grant_operator_permissions(&[
        "peers:manage",
        &format!("tool_invoke:{peer_name}:{TOOL_QUERY}"),
    ])
    .await;

    let auth = h.auth_context(h.user_id().await, h.org_id().await);
    let allowed = ione::services::federation::route_tool_call(
        &h.state,
        workspace_id,
        &format!("{prefix}:{TOOL_QUERY}"),
        json!({ "aoi_id": "aoi-1" }),
        &auth,
    )
    .await
    .expect("a granted tool must dispatch");
    assert_eq!(allowed["isError"], false, "{allowed}");

    let denied = ione::services::federation::route_tool_call(
        &h.state,
        workspace_id,
        &format!("{prefix}:{TOOL_ACKNOWLEDGE}"),
        json!({ "alert_id": "alert-1" }),
        &auth,
    )
    .await;
    let err = denied.expect_err("an ungranted tool must be denied");
    assert!(
        err.to_string().starts_with("FORBIDDEN:"),
        "denial must surface as FORBIDDEN (→ -32403 / 403), got: {err}"
    );

    assert_eq!(
        stub.tools_called(),
        vec![TOOL_QUERY.to_string()],
        "the denial must happen before any outbound call reaches the peer"
    );
}
