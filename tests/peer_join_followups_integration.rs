//! Peer-join follow-ups: F2, F3, F4, F10a.
//!
//! Each test here fails against the pre-fix tree. They cover four defects that
//! all sit on the join path — the first thing a third-party app has to get
//! through — and that `tests/peer_lifecycle_e2e_integration.rs` cannot catch,
//! because its stub peer publishes everything IONe happens to want and serves
//! its discovery document at *both* the origin and the MCP-path location.
//!
//!   F2   `registration_endpoint` was non-optional, so a peer publishing exactly
//!        contract §2's endpoint table could not join. A CIMD peer must be able
//!        to join without one; a peer offering neither must be told what to
//!        publish, not handed a deserialization failure.
//!   F3   Discovery was fetched at `{mcp_url}/.well-known/…` rather than at the
//!        origin (RFC 8414 §3). Fixed origin-first, with a fallback to the
//!        legacy MCP-path location for peers built against the old behaviour.
//!   F4   The peer name was derived from the host alone, so two peers behind one
//!        hostname on different ports collided on `UNIQUE (org_id, name)`.
//!   F10a `GET /api/v1/peers/:id/manifest` read page 1 of `tools/list` only, and
//!        turned a JSON-RPC error into `200 {"tools": []}`.
//!
//! Every peer fixture in this file is local to it: `tests/support/stub_peer.rs`
//! answers discovery at both locations and always publishes
//! `registration_endpoint`, so it cannot distinguish any of the above.
//!
//! Prerequisites:
//!   docker compose up -d postgres
//!
//! Run (serial, ignored):
//!   SQLX_OFFLINE=true IONE_SKIP_LIVE=1 \
//!     DATABASE_URL=postgres://ione:ione@localhost:5433/ione_w13 \
//!     cargo test --test peer_join_followups_integration -- --ignored --test-threads=1

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::extract::{Form, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use reqwest::redirect::Policy;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;
use uuid::Uuid;

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione_w13";
const TEST_STATIC_BEARER: &str = "peer-join-followups-test-bearer";
const TEST_KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

const ORIGIN_DISCOVERY_PATH: &str = "/.well-known/oauth-authorization-server";
const LEGACY_DISCOVERY_PATH: &str = "/mcp/.well-known/oauth-authorization-server";

// ─── IONe harness ─────────────────────────────────────────────────────────────

struct Harness {
    base: String,
    pool: PgPool,
}

async fn spawn_app() -> Harness {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr: SocketAddr = listener.local_addr().expect("local addr");
    let base = format!("http://{addr}");

    std::env::set_var("IONE_AUTH_MODE", "local");
    std::env::set_var("IONE_OAUTH_STATIC_BEARER", TEST_STATIC_BEARER);
    std::env::set_var("IONE_TOKEN_KEY", TEST_KEY);
    std::env::set_var("IONE_WEBHOOK_SECRET_KEY", TEST_KEY);
    std::env::set_var("IONE_OAUTH_ISSUER", &base);
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

    let (router, _state) = ione::app_with_state(pool.clone()).await;
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("server error");
    });

    let harness = Harness { base, pool };
    harness.grant_peers_manage().await;
    harness
}

impl Harness {
    async fn grant_peers_manage(&self) {
        let user_id: Uuid =
            sqlx::query_scalar("SELECT id FROM users ORDER BY created_at ASC LIMIT 1")
                .fetch_one(&self.pool)
                .await
                .expect("default user not found");
        let org_id: Uuid =
            sqlx::query_scalar("SELECT id FROM organizations WHERE name = 'Default Org' LIMIT 1")
                .fetch_one(&self.pool)
                .await
                .expect("Default Org not found");
        sqlx::query(
            "INSERT INTO org_memberships (user_id, org_id, permissions)
             VALUES ($1, $2, $3)
             ON CONFLICT (user_id, org_id) DO UPDATE SET permissions = EXCLUDED.permissions",
        )
        .bind(user_id)
        .bind(org_id)
        .bind(json!(["peers:manage"]))
        .execute(&self.pool)
        .await
        .expect("grant org permissions");
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
        let status = StatusCode::from_u16(response.status().as_u16()).expect("status");
        (status, response.json().await.expect("json"))
    }

    async fn register_peer(&self, mcp_url: &str) -> (StatusCode, Value) {
        let response = self
            .client()
            .post(format!("{}/api/v1/peers", self.base))
            .bearer_auth(TEST_STATIC_BEARER)
            .json(&json!({ "peerUrl": mcp_url }))
            .send()
            .await
            .expect("request");
        let status = StatusCode::from_u16(response.status().as_u16()).expect("status");
        (status, response.json().await.expect("json"))
    }

    /// Drive the operator's leg: hit the peer's authorize endpoint, then hand the
    /// resulting `?code&state` back to IONe's callback.
    async fn complete_oauth(&self, authorize_url: &str) {
        let response = self
            .client()
            .get(authorize_url)
            .send()
            .await
            .expect("peer authorize");
        assert_eq!(
            response.status().as_u16(),
            302,
            "the peer must redirect back with an authorization code"
        );
        let callback_url = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .expect("authorize redirect Location")
            .to_string();
        let response = self
            .client()
            .get(&callback_url)
            .send()
            .await
            .expect("ione callback");
        assert_eq!(
            response.status().as_u16(),
            303,
            "the callback must complete the token exchange and redirect to the peer page"
        );
    }

    async fn peer_status(&self, peer_id: Uuid) -> String {
        sqlx::query_scalar("SELECT status::text FROM peers WHERE id = $1")
            .bind(peer_id)
            .fetch_one(&self.pool)
            .await
            .expect("peer status")
    }

    async fn peer_name(&self, peer_id: Uuid) -> String {
        sqlx::query_scalar("SELECT name FROM peers WHERE id = $1")
            .bind(peer_id)
            .fetch_one(&self.pool)
            .await
            .expect("peer name")
    }

    async fn peer_client_id(&self, peer_id: Uuid) -> String {
        sqlx::query_scalar("SELECT oauth_client_id FROM peers WHERE id = $1")
            .bind(peer_id)
            .fetch_one(&self.pool)
            .await
            .expect("peer oauth_client_id")
    }

    /// Register a peer and run it through OAuth to `pending_allowlist`.
    async fn join(&self, peer: &TestPeer) -> Uuid {
        let (status, body) = self.register_peer(&peer.mcp_url).await;
        assert_eq!(status, StatusCode::OK, "peer registration failed: {body}");
        let peer_id: Uuid = body["id"].as_str().expect("peer id").parse().expect("uuid");
        self.complete_oauth(body["authorizeUrl"].as_str().expect("authorizeUrl"))
            .await;
        assert_eq!(self.peer_status(peer_id).await, "pending_allowlist");
        peer_id
    }
}

// ─── Test peer ────────────────────────────────────────────────────────────────

/// What a fixture peer publishes. Everything here is a knob some test needs to
/// turn; the defaults describe a peer that conforms to contract §2 *and* to the
/// pre-fix implementation, so a test opts into exactly one deviation.
#[derive(Clone)]
struct PeerSpec {
    /// Publish `registration_endpoint` (RFC 7591) in the discovery document.
    registration_endpoint: bool,
    /// Advertise `client_id_metadata_document_supported`.
    cimd: bool,
    /// Answer discovery at the RFC 8414 origin location.
    origin_discovery: bool,
    /// Answer discovery at the legacy `{mcp_url}/.well-known/…` location.
    legacy_discovery: bool,
    /// Number of `tools/list` pages to serve; page *n* names its tool `tool_n`.
    tools_pages: usize,
    /// Answer `tools/list` with a JSON-RPC error inside an HTTP 200.
    tools_error: bool,
}

impl Default for PeerSpec {
    fn default() -> Self {
        Self {
            registration_endpoint: true,
            cimd: false,
            origin_discovery: true,
            legacy_discovery: false,
            tools_pages: 1,
            tools_error: false,
        }
    }
}

#[derive(Default)]
struct PeerLog {
    /// Path of every discovery request this peer received, in order.
    discovery_paths: Vec<String>,
    /// Bodies of every dynamic-client-registration request, in order.
    registrations: Vec<Value>,
    /// `client_id` presented at the authorization endpoint, in order.
    authorize_client_ids: Vec<String>,
    /// `params.cursor` of every `tools/list` request, in order.
    tools_cursors: Vec<Option<String>>,
}

#[derive(Clone)]
struct PeerState {
    base_url: String,
    spec: PeerSpec,
    log: Arc<Mutex<PeerLog>>,
    auth_codes: Arc<Mutex<HashMap<String, String>>>,
}

struct TestPeer {
    base_url: String,
    mcp_url: String,
    log: Arc<Mutex<PeerLog>>,
}

impl TestPeer {
    async fn start(spec: PeerSpec) -> TestPeer {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind peer");
        let addr: SocketAddr = listener.local_addr().expect("peer addr");
        let base_url = format!("http://127.0.0.1:{}", addr.port());

        let state = PeerState {
            base_url: base_url.clone(),
            spec,
            log: Arc::new(Mutex::new(PeerLog::default())),
            auth_codes: Arc::new(Mutex::new(HashMap::new())),
        };

        let router = Router::new()
            .route(ORIGIN_DISCOVERY_PATH, get(origin_discovery))
            .route(LEGACY_DISCOVERY_PATH, get(legacy_discovery))
            .route("/oauth/register", post(oauth_register))
            .route("/oauth/authorize", get(oauth_authorize))
            .route("/oauth/token", post(oauth_token))
            .route("/mcp", post(mcp))
            .with_state(state.clone());

        tokio::spawn(async move {
            axum::serve(listener, router).await.expect("peer serve");
        });

        TestPeer {
            mcp_url: format!("{base_url}/mcp"),
            base_url,
            log: state.log,
        }
    }

    fn discovery_paths(&self) -> Vec<String> {
        self.log.lock().expect("log").discovery_paths.clone()
    }

    fn registrations(&self) -> Vec<Value> {
        self.log.lock().expect("log").registrations.clone()
    }

    fn authorize_client_ids(&self) -> Vec<String> {
        self.log.lock().expect("log").authorize_client_ids.clone()
    }

    fn tools_cursors(&self) -> Vec<Option<String>> {
        self.log.lock().expect("log").tools_cursors.clone()
    }
}

fn discovery_document(state: &PeerState) -> Value {
    let base = &state.base_url;
    // Exactly contract §2's endpoint table, plus whatever the spec opts into.
    let mut doc = json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/oauth/authorize"),
        "token_endpoint": format!("{base}/oauth/token"),
        "revocation_endpoint": format!("{base}/oauth/revoke"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"]
    });
    if state.spec.registration_endpoint {
        doc["registration_endpoint"] = json!(format!("{base}/oauth/register"));
    }
    if state.spec.cimd {
        doc["client_id_metadata_document_supported"] = json!(true);
    }
    doc
}

async fn serve_discovery(state: PeerState, path: &str, enabled: bool) -> Response {
    state
        .log
        .lock()
        .expect("log")
        .discovery_paths
        .push(path.to_string());
    if !enabled {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": "not_found" }))).into_response();
    }
    Json(discovery_document(&state)).into_response()
}

async fn origin_discovery(State(state): State<PeerState>) -> Response {
    let enabled = state.spec.origin_discovery;
    serve_discovery(state, ORIGIN_DISCOVERY_PATH, enabled).await
}

async fn legacy_discovery(State(state): State<PeerState>) -> Response {
    let enabled = state.spec.legacy_discovery;
    serve_discovery(state, LEGACY_DISCOVERY_PATH, enabled).await
}

async fn oauth_register(State(state): State<PeerState>, Json(body): Json<Value>) -> Response {
    state
        .log
        .lock()
        .expect("log")
        .registrations
        .push(body.clone());
    (
        StatusCode::CREATED,
        Json(json!({ "client_id": format!("registered-client-{}", Uuid::new_v4()) })),
    )
        .into_response()
}

async fn oauth_authorize(
    State(state): State<PeerState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    state
        .log
        .lock()
        .expect("log")
        .authorize_client_ids
        .push(params.get("client_id").cloned().unwrap_or_default());

    let redirect_uri = params.get("redirect_uri").expect("redirect_uri");
    let challenge = params.get("code_challenge").expect("code_challenge");
    assert_eq!(
        params.get("code_challenge_method").map(String::as_str),
        Some("S256")
    );
    let code = Uuid::new_v4().to_string();
    state
        .auth_codes
        .lock()
        .expect("auth codes")
        .insert(code.clone(), challenge.clone());

    let mut location = format!("{redirect_uri}?code={code}");
    if let Some(csrf_state) = params.get("state") {
        location.push_str(&format!("&state={csrf_state}"));
    }
    (
        StatusCode::FOUND,
        [(header::LOCATION, location)],
        String::new(),
    )
        .into_response()
}

async fn oauth_token(
    State(state): State<PeerState>,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    let code = form.get("code").expect("code");
    let verifier = form.get("code_verifier").expect("code_verifier");
    let challenge = state
        .auth_codes
        .lock()
        .expect("auth codes")
        .remove(code)
        .expect("unknown authorization code");
    let expected = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    assert_eq!(
        expected, challenge,
        "PKCE verifier must match the challenge"
    );

    Json(json!({
        "access_token": Uuid::new_v4().to_string(),
        "token_type": "Bearer",
        "expires_in": 3600,
        "refresh_token": Uuid::new_v4().to_string(),
        "scope": "mcp"
    }))
    .into_response()
}

/// `tools/list` page cursor for the page *after* `page` (1-based).
fn tools_cursor(page: usize) -> String {
    format!("page-{}", page + 1)
}

async fn mcp(State(state): State<PeerState>, Json(body): Json<Value>) -> Response {
    let id = body.get("id").cloned().unwrap_or(Value::Null);
    if body.get("method").and_then(Value::as_str) != Some("tools/list") {
        return Json(json!({
            "jsonrpc": "2.0", "id": id,
            "error": { "code": -32601, "message": "Method not found" }
        }))
        .into_response();
    }

    let cursor = body
        .pointer("/params/cursor")
        .and_then(Value::as_str)
        .map(str::to_string);
    state
        .log
        .lock()
        .expect("log")
        .tools_cursors
        .push(cursor.clone());

    if state.spec.tools_error {
        return Json(json!({
            "jsonrpc": "2.0", "id": id,
            "error": { "code": -32000, "message": "peer refuses to enumerate tools" }
        }))
        .into_response();
    }

    let page = match cursor.as_deref() {
        None => 1,
        Some(value) => match (1..=state.spec.tools_pages).find(|n| tools_cursor(*n) == value) {
            Some(previous) => previous + 1,
            None => {
                return Json(json!({
                    "jsonrpc": "2.0", "id": id,
                    "error": { "code": -32602, "message": format!("unknown cursor '{value}'") }
                }))
                .into_response()
            }
        },
    };

    let mut result = json!({
        "tools": [{
            "name": format!("tool_{page}"),
            "description": format!("tool served on tools/list page {page}"),
            "inputSchema": { "type": "object", "properties": {} }
        }]
    });
    if page < state.spec.tools_pages {
        result["nextCursor"] = json!(tools_cursor(page));
    } else {
        // §8.1: an explicit null is a conforming terminator.
        result["nextCursor"] = Value::Null;
    }
    Json(json!({ "jsonrpc": "2.0", "id": id, "result": result })).into_response()
}

// ─── F2: registration_endpoint must be optional ───────────────────────────────

/// A peer that publishes exactly contract §2's endpoint table — no
/// `registration_endpoint` — but advertises CIMD joins, and IONe presents its own
/// published client-metadata document as the `client_id` instead of registering.
#[tokio::test]
#[ignore]
async fn a_cimd_peer_without_registration_endpoint_joins() {
    let h = spawn_app().await;
    let peer = TestPeer::start(PeerSpec {
        registration_endpoint: false,
        cimd: true,
        ..PeerSpec::default()
    })
    .await;

    let peer_id = h.join(&peer).await;

    let expected_client_id = format!("{}/.well-known/mcp-client", h.base);
    assert_eq!(
        h.peer_client_id(peer_id).await,
        expected_client_id,
        "under CIMD the client_id is IONe's own client-metadata document URL"
    );
    assert_eq!(
        peer.authorize_client_ids(),
        vec![expected_client_id],
        "that same client_id must be what reaches the peer's authorization endpoint"
    );
    assert!(
        peer.registrations().is_empty(),
        "a CIMD peer publishes no registration endpoint, so none may be called: {:?}",
        peer.registrations()
    );
}

/// A peer offering neither `registration_endpoint` nor CIMD is told, by name,
/// what it has to publish — not handed the generic "invalid peer metadata" that
/// a failed deserialization produced.
#[tokio::test]
#[ignore]
async fn a_peer_with_neither_registration_nor_cimd_is_told_what_to_publish() {
    let h = spawn_app().await;
    let peer = TestPeer::start(PeerSpec {
        registration_endpoint: false,
        cimd: false,
        ..PeerSpec::default()
    })
    .await;

    let (status, body) = h.register_peer(&peer.mcp_url).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let message = body["message"].as_str().expect("message").to_string();
    assert!(
        message.contains("registration_endpoint"),
        "the error must name the missing endpoint: {message}"
    );
    assert!(
        message.contains("client_id_metadata_document_supported"),
        "the error must name the alternative to publishing it: {message}"
    );
    assert_ne!(
        message, "invalid peer metadata",
        "a missing optional field is not a metadata parse failure"
    );
    assert!(
        peer.discovery_paths()
            .iter()
            .any(|p| p == ORIGIN_DISCOVERY_PATH),
        "the document must have been read before the join was refused: {:?}",
        peer.discovery_paths()
    );
}

// ─── F3: discovery lives at the origin ────────────────────────────────────────

/// RFC 8414 §3: the authorization-server metadata document is origin-relative.
/// A peer serving MCP at `/mcp` must not have to duplicate it under `/mcp`.
#[tokio::test]
#[ignore]
async fn discovery_is_fetched_from_the_origin_not_the_mcp_path() {
    let h = spawn_app().await;
    let peer = TestPeer::start(PeerSpec {
        origin_discovery: true,
        legacy_discovery: false,
        ..PeerSpec::default()
    })
    .await;

    h.join(&peer).await;

    let paths = peer.discovery_paths();
    assert!(
        !paths.is_empty(),
        "the peer must have been asked for its discovery document"
    );
    assert!(
        paths.iter().all(|path| path == ORIGIN_DISCOVERY_PATH),
        "every discovery request must go to the origin, got {paths:?}"
    );
}

/// Peers built against the previous behaviour serve the document under the MCP
/// path only. They must keep working: origin is tried first, the MCP path is the
/// fallback.
#[tokio::test]
#[ignore]
async fn a_peer_serving_discovery_only_under_the_mcp_path_still_joins() {
    let h = spawn_app().await;
    let peer = TestPeer::start(PeerSpec {
        origin_discovery: false,
        legacy_discovery: true,
        ..PeerSpec::default()
    })
    .await;

    h.join(&peer).await;

    let paths = peer.discovery_paths();
    assert_eq!(
        paths.first().map(String::as_str),
        Some(ORIGIN_DISCOVERY_PATH),
        "the origin must be tried before the legacy location: {paths:?}"
    );
    assert!(
        paths.iter().any(|path| path == LEGACY_DISCOVERY_PATH),
        "the legacy location must be the fallback: {paths:?}"
    );
}

// ─── F4: the derived peer name keeps the port ─────────────────────────────────

/// Two peers behind one hostname on different ports are two peers. Before the
/// fix the name was the host alone and `peers_org_id_name_key UNIQUE (org_id,
/// name)` rejected the second registration as a duplicate URL.
#[tokio::test]
#[ignore]
async fn two_peers_on_one_host_with_different_ports_both_register() {
    let h = spawn_app().await;
    let first = TestPeer::start(PeerSpec::default()).await;
    let second = TestPeer::start(PeerSpec::default()).await;
    assert_ne!(first.base_url, second.base_url);

    let (status, body) = h.register_peer(&first.mcp_url).await;
    assert_eq!(status, StatusCode::OK, "first registration failed: {body}");
    let first_id: Uuid = body["id"].as_str().expect("id").parse().expect("uuid");

    let (status, body) = h.register_peer(&second.mcp_url).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a second peer on the same host must not collide: {body}"
    );
    let second_id: Uuid = body["id"].as_str().expect("id").parse().expect("uuid");

    let first_name = h.peer_name(first_id).await;
    let second_name = h.peer_name(second_id).await;
    assert_ne!(
        first_name, second_name,
        "two peers on one host must get distinct names"
    );
    let first_port = url::Url::parse(&first.mcp_url)
        .expect("url")
        .port()
        .expect("port");
    let second_port = url::Url::parse(&second.mcp_url)
        .expect("url")
        .port()
        .expect("port");
    assert_eq!(first_name, format!("127.0.0.1:{first_port}"));
    assert_eq!(second_name, format!("127.0.0.1:{second_port}"));

    let prefixes: Vec<String> =
        sqlx::query_scalar("SELECT tool_prefix FROM peers ORDER BY created_at")
            .fetch_all(&h.pool)
            .await
            .expect("tool prefixes");
    assert_eq!(prefixes.len(), 2);
    assert_ne!(
        prefixes[0], prefixes[1],
        "distinct names must yield distinct tool prefixes: {prefixes:?}"
    );
}

/// The duplicate check still catches an actual duplicate.
#[tokio::test]
#[ignore]
async fn re_registering_the_same_peer_url_is_still_a_duplicate() {
    let h = spawn_app().await;
    let peer = TestPeer::start(PeerSpec::default()).await;

    let (status, body) = h.register_peer(&peer.mcp_url).await;
    assert_eq!(status, StatusCode::OK, "first registration failed: {body}");

    let (status, body) = h.register_peer(&peer.mcp_url).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["message"], "peer URL already registered", "{body}");

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM peers")
        .fetch_one(&h.pool)
        .await
        .expect("peer count");
    assert_eq!(count, 1, "the refused registration must not insert a row");
}

/// A URL on the scheme's default port keeps the name it had before the fix, so
/// no already-registered peer is renamed by it.
#[test]
fn a_default_port_url_keeps_the_bare_host_as_its_name() {
    // `peer_name_from_url` is private; assert the property through the same
    // `url` crate rule it uses, so the expectation is stated in one place here
    // and checked against real registrations by the test above.
    for (url, expected_port) in [
        ("https://peer.example.com/mcp", None),
        ("https://peer.example.com:443/mcp", None),
        ("http://peer.example.com:80/mcp", None),
        ("https://peer.example.com:8443/mcp", Some(8443)),
    ] {
        assert_eq!(
            url::Url::parse(url).expect("url").port(),
            expected_port,
            "{url}"
        );
    }
}

// ─── F10a: the allowlist-review manifest is complete and honest ───────────────

/// `GET /api/v1/peers/:id/manifest` is what an operator reviews before deciding
/// which tools to authorize. A peer that pages its `tools/list` must have every
/// page in it — page 1 alone silently hides tools from the review.
#[tokio::test]
#[ignore]
async fn the_review_manifest_contains_every_tools_list_page() {
    let h = spawn_app().await;
    let peer = TestPeer::start(PeerSpec {
        tools_pages: 3,
        ..PeerSpec::default()
    })
    .await;
    let peer_id = h.join(&peer).await;

    let (status, body) = h
        .get_json(&format!("/api/v1/peers/{peer_id}/manifest"))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let names: Vec<String> = body["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .map(|tool| tool["name"].as_str().expect("tool name").to_string())
        .collect();
    assert_eq!(
        names,
        vec!["tool_1", "tool_2", "tool_3"],
        "every page must reach the operator reviewing the allowlist"
    );

    let cursors = peer.tools_cursors();
    assert!(
        cursors.contains(&Some("page-2".to_string()))
            && cursors.contains(&Some("page-3".to_string())),
        "IONe must have followed nextCursor: {cursors:?}"
    );
}

/// A peer that answers `tools/list` with a JSON-RPC error must not be reported to
/// the operator as a peer that has no tools.
#[tokio::test]
#[ignore]
async fn a_jsonrpc_error_on_the_review_manifest_is_not_reported_as_an_empty_tool_list() {
    let h = spawn_app().await;
    let peer = TestPeer::start(PeerSpec {
        tools_error: true,
        ..PeerSpec::default()
    })
    .await;
    let peer_id = h.join(&peer).await;

    let (status, body) = h
        .get_json(&format!("/api/v1/peers/{peer_id}/manifest"))
        .await;
    assert_ne!(
        status,
        StatusCode::OK,
        "a JSON-RPC error must not be answered with success: {body}"
    );
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert!(
        body.get("tools").is_none(),
        "an error response must not carry a tool list at all: {body}"
    );
}
