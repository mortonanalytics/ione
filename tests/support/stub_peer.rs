//! Reference stub peer — a minimal, faithful implementation of all six surfaces
//! of `md/design/app-integration-contract-v1.md` (v1, frozen 2026-07-25).
//!
//! It exists so IONe's shell and the standalone conformance kit
//! (`src/bin/ione-conformance.rs`) can be exercised end-to-end without a real
//! third-party peer such as TerraYield.
//!
//! Surfaces served:
//!   1. MCP server endpoint          `POST /mcp`            (§1)
//!   2. OAuth 2.1 authorization srv  `/.well-known/...`     (§2)
//!   3. Signed webhook sender        `POST /test/emit-webhook` triggers a send (§3)
//!   4. Resource view metadata       one canned resource per `ione_view`  (§4)
//!   5. Context slice                `resources/read slice://`            (§5)
//!   6. `whoami://`                  `resources/read whoami://`           (§6)
//!
//! Deliberate fixture simplification, stated so nobody mistakes it for a rule:
//! `POST /mcp` does **not** gate on the bearer token. Contract §1 allows an
//! unauthenticated peer (the token may be the empty string, in which case IONe
//! omits the header), and leaving the endpoint open lets the same fixture be
//! driven both pre-brokered and unauthenticated. A production peer must validate
//! the token it issued via §2.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::extract::{Form, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use chrono::Utc;
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use uuid::Uuid;

// ─── Canned identity (§6) ─────────────────────────────────────────────────────

pub const SELF_PEER_ID: &str = "stub-peer";
pub const FOREIGN_TENANT_ID: &str = "stub-tenant-1";
pub const FOREIGN_TENANT_NAME: &str = "Stub Peer Tenant";
pub const FOREIGN_WORKSPACE_ID: &str = "stub-workspace-1";
pub const FOREIGN_USER_ID: &str = "stub-user-1";
pub const FOREIGN_USER_EMAIL: &str = "operator@stub-peer.test";

// ─── Canned resources (§4) ────────────────────────────────────────────────────

pub const MAP_URI: &str = "stub://map/displacement";
pub const MAP_TILE_URL: &str = "https://tiles.stub-peer.test/displacement/{z}/{x}/{y}.png";
pub const CHART_URI: &str = "stub://chart/displacement";
pub const TABLE_URI: &str = "stub://table/assets";
pub const DOCUMENT_URI: &str = "stub://document/q2-compliance";
pub const DOCUMENT_DOWNLOAD_URL: &str =
    "https://files.stub-peer.test/reports/q2-compliance.pdf?sig=stub";

/// `tools/list` is served in two pages so a consumer's `nextCursor` handling is
/// exercised (§8.1). `resources/list` is deliberately a single page, because the
/// four panel-discovery paths read page 1 only (§8.2).
const TOOLS_PAGE_1_CURSOR: &str = "stub-tools-page-2";

// ─── Handle ───────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct WebhookConfig {
    pub peer_id: Uuid,
    pub signing_secret: String,
}

pub struct StubPeer {
    pub base_url: String,
    pub mcp_url: String,
    webhook: Arc<Mutex<Option<WebhookConfig>>>,
    requests: Arc<Mutex<Vec<Value>>>,
    http: reqwest::Client,
}

impl StubPeer {
    /// Bind an ephemeral port and serve every surface. The returned handle stays
    /// alive for the life of the test; the server task is detached.
    pub async fn start() -> StubPeer {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind stub peer");
        let addr: SocketAddr = listener.local_addr().expect("stub peer addr");
        let base_url = format!("http://{addr}");

        let state = StubState {
            base_url: base_url.clone(),
            webhook: Arc::new(Mutex::new(None)),
            auth_codes: Arc::new(Mutex::new(HashMap::new())),
            refresh_tokens: Arc::new(Mutex::new(HashMap::new())),
            requests: Arc::new(Mutex::new(Vec::new())),
            http: reqwest::Client::new(),
        };

        let router = Router::new()
            .route("/mcp", post(mcp))
            .route(
                "/.well-known/oauth-authorization-server",
                get(oauth_discovery),
            )
            .route("/oauth/authorize", get(oauth_authorize))
            .route("/oauth/token", post(oauth_token))
            .route("/oauth/revoke", post(oauth_revoke))
            .route("/test/emit-webhook", post(emit_webhook_route))
            .with_state(state.clone());

        tokio::spawn(async move {
            axum::serve(listener, router)
                .await
                .expect("stub peer serve");
        });

        StubPeer {
            mcp_url: format!("{base_url}/mcp"),
            base_url,
            webhook: state.webhook,
            requests: state.requests,
            http: state.http,
        }
    }

    /// Store the `(peer_id, signingSecret)` pair IONe provisioned for this peer.
    /// Mirrors a real peer persisting its webhook credential after the operator
    /// calls `POST /api/v1/peers/:id/webhook/provision`.
    pub fn set_webhook_config(&self, peer_id: Uuid, signing_secret: impl Into<String>) {
        *self.webhook.lock().expect("webhook mutex") = Some(WebhookConfig {
            peer_id,
            signing_secret: signing_secret.into(),
        });
    }

    /// URL a conformance run POSTs to in order to make this peer emit one signed
    /// webhook. This is a fixture affordance, not part of contract v1.
    pub fn webhook_trigger_url(&self) -> String {
        format!("{}/test/emit-webhook", self.base_url)
    }

    /// Send one signed §3.3 envelope to `{ione_base_url}/webhooks/peer/{peer_id}`.
    pub async fn emit_webhook(&self, ione_base_url: &str) -> reqwest::Response {
        let config = self
            .webhook
            .lock()
            .expect("webhook mutex")
            .clone()
            .expect("set_webhook_config must be called before emit_webhook");
        send_signed_webhook(&self.http, ione_base_url, &config)
            .await
            .expect("stub peer webhook send")
    }

    /// Every JSON-RPC request body this peer has received, in order.
    pub fn requests(&self) -> Vec<Value> {
        self.requests.lock().expect("requests mutex").clone()
    }

    pub fn methods_called(&self) -> Vec<String> {
        self.requests()
            .iter()
            .filter_map(|r| r.get("method").and_then(Value::as_str).map(str::to_string))
            .collect()
    }
}

#[derive(Clone)]
struct StubState {
    base_url: String,
    webhook: Arc<Mutex<Option<WebhookConfig>>>,
    /// authorization code → PKCE `code_challenge`
    auth_codes: Arc<Mutex<HashMap<String, String>>>,
    /// refresh token → client_id
    refresh_tokens: Arc<Mutex<HashMap<String, String>>>,
    requests: Arc<Mutex<Vec<Value>>>,
    http: reqwest::Client,
}

// ─── Surface 1: MCP server endpoint (§1) ──────────────────────────────────────

async fn mcp(State(state): State<StubState>, Json(body): Json<Value>) -> Response {
    state
        .requests
        .lock()
        .expect("requests mutex")
        .push(body.clone());

    let id = body.get("id").cloned().unwrap_or(Value::Null);
    let params = body.get("params").cloned().unwrap_or(Value::Null);

    match body.get("method").and_then(Value::as_str).unwrap_or("") {
        "initialize" => {
            let mut headers = HeaderMap::new();
            headers.insert(
                "MCP-Session-Id",
                HeaderValue::from_str(&Uuid::new_v4().to_string()).expect("session id header"),
            );
            let result = json!({
                "protocolVersion": "2025-11-25",
                "capabilities": { "tools": {}, "resources": {} },
                "serverInfo": { "name": SELF_PEER_ID, "version": "1.0.0" }
            });
            (headers, Json(rpc_result(id, result))).into_response()
        }
        "tools/list" => {
            let cursor = params.get("cursor").and_then(Value::as_str);
            let result = match cursor {
                None => json!({
                    "tools": [tool_descriptor(
                        "query_displacement",
                        "Time-series displacement for an area of interest.",
                        "aoi_id"
                    )],
                    "nextCursor": TOOLS_PAGE_1_CURSOR
                }),
                Some(TOOLS_PAGE_1_CURSOR) => json!({
                    "tools": [tool_descriptor(
                        "acknowledge_alert",
                        "Mark an alert acknowledged.",
                        "alert_id"
                    )]
                }),
                Some(other) => {
                    return Json(rpc_error(
                        id,
                        -32602,
                        &format!("unknown tools/list cursor '{other}'"),
                    ))
                    .into_response()
                }
            };
            Json(rpc_result(id, result)).into_response()
        }
        "resources/list" => {
            Json(rpc_result(id, json!({ "resources": resources() }))).into_response()
        }
        "resources/read" => {
            let uri = params.get("uri").and_then(Value::as_str).unwrap_or("");
            match resource_contents(uri) {
                Some((mime_type, text)) => Json(rpc_result(
                    id,
                    json!({
                        "contents": [{ "uri": uri, "mimeType": mime_type, "text": text }]
                    }),
                ))
                .into_response(),
                // §7.3: resource-not-found is JSON-RPC -32002 carried in an HTTP
                // 200 body. Answering with an HTTP 4xx would make IONe report 502.
                None => Json(rpc_error(id, -32002, &format!("Resource not found: {uri}")))
                    .into_response(),
            }
        }
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
            match name {
                "query_displacement" => {
                    let aoi = arguments
                        .get("aoi_id")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown");
                    Json(rpc_result(
                        id,
                        json!({
                            "content": [{
                                "type": "text",
                                "text": json!({
                                    "aoi_id": aoi,
                                    "observations": [
                                        { "observation_time": "2026-07-01T00:00:00Z", "displacement_mm": 1.2 },
                                        { "observation_time": "2026-07-08T00:00:00Z", "displacement_mm": 1.9 }
                                    ]
                                }).to_string()
                            }],
                            "isError": false
                        }),
                    ))
                    .into_response()
                }
                "acknowledge_alert" => {
                    let alert = arguments
                        .get("alert_id")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown");
                    Json(rpc_result(
                        id,
                        json!({
                            "content": [{
                                "type": "text",
                                "text": json!({ "alert_id": alert, "acknowledged": true }).to_string()
                            }],
                            "isError": false
                        }),
                    ))
                    .into_response()
                }
                other => Json(rpc_result(
                    id,
                    json!({
                        "content": [{ "type": "text", "text": format!("unknown tool '{other}'") }],
                        "isError": true
                    }),
                ))
                .into_response(),
            }
        }
        other => Json(rpc_error(id, -32601, &format!("Method not found: {other}"))).into_response(),
    }
}

fn rpc_result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn tool_descriptor(name: &str, description: &str, argument: &str) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": { argument: { "type": "string" } },
            "required": [argument]
        }
    })
}

// ─── Surface 4: resource view metadata (§4) ───────────────────────────────────

/// One resource per `ione_view` value, each carrying exactly the metadata IONe's
/// extractors require, plus the two identity/summary resources.
pub fn resources() -> Vec<Value> {
    vec![
        // §4.2 — only `tile_url` is required; the rest are optional pass-through.
        json!({
            "uri": MAP_URI,
            "name": "Displacement tiles",
            "mimeType": "application/vnd.ione.map+json",
            "metadata": {
                "ione_view": "map",
                "tile_url": MAP_TILE_URL,
                "bounds": [-112.75, 45.4, -111.9, 46.1],
                "attribution": "Stub Peer Tiles",
                "layer_name": "Displacement",
                "opacity": 0.7,
                "vector_url": "https://tiles.stub-peer.test/displacement.pmtiles"
            }
        }),
        // §4.3 — nested `metadata.spec` form; chart_type/x_axis/y_axis all present,
        // because in the nested form a missing scalar drops the resource.
        json!({
            "uri": CHART_URI,
            "name": "Displacement over time",
            "mimeType": "application/vnd.ione.chart+json",
            "metadata": {
                "ione_view": "chart",
                "spec": chart_spec()
            }
        }),
        // §4.4 — `ione_view` and a non-empty `uri` are the whole requirement.
        json!({
            "uri": TABLE_URI,
            "name": "Asset inventory",
            "mimeType": "application/vnd.ione.table+json",
            "metadata": { "ione_view": "table" }
        }),
        // §4.5 — https-only `download_url`, and a resolvable mime type.
        json!({
            "uri": DOCUMENT_URI,
            "name": "Q2 2026 compliance report",
            "mimeType": "application/pdf",
            "metadata": {
                "ione_view": "document",
                "download_url": DOCUMENT_DOWNLOAD_URL,
                "mime_type": "application/pdf",
                "file_size_bytes": 184_320,
                "last_modified": "2026-07-01T12:00:00Z"
            }
        }),
        json!({
            "uri": "slice://",
            "name": "Stub peer capability slice",
            "mimeType": "application/vnd.ione.slice+json"
        }),
        json!({
            "uri": "whoami://",
            "name": "Caller identity",
            "mimeType": "application/vnd.ione.whoami+json"
        }),
    ]
}

fn chart_spec() -> Value {
    json!({
        "chart_type": "line",
        "x_axis": "observation_time",
        "y_axis": "displacement_mm",
        "series": ["mean", "p95"]
    })
}

/// `(mimeType, contents[0].text)` for every readable URI, or `None` for -32002.
fn resource_contents(uri: &str) -> Option<(&'static str, String)> {
    match uri {
        "whoami://" => Some((
            "application/vnd.ione.whoami+json",
            whoami_body().to_string(),
        )),
        "slice://" => Some(("application/vnd.ione.slice+json", slice_body().to_string())),
        CHART_URI => Some(("application/vnd.ione.chart+json", chart_body().to_string())),
        TABLE_URI => Some(("application/vnd.ione.table+json", table_body().to_string())),
        _ => None,
    }
}

/// §4.3 chart body: the spec plus rows keyed by `x_axis` and each `series` entry.
pub fn chart_body() -> Value {
    json!({
        "spec": chart_spec(),
        "rows": [
            { "observation_time": "2026-07-01T00:00:00Z", "mean": 1.2, "p95": 3.4 },
            { "observation_time": "2026-07-08T00:00:00Z", "mean": 1.9, "p95": 4.1 },
            { "observation_time": "2026-07-15T00:00:00Z", "mean": 2.4, "p95": 5.0 }
        ]
    })
}

/// §4.4 table body: an ordered `schema` and rows keyed by `column.name`.
pub fn table_body() -> Value {
    json!({
        "schema": [
            { "name": "asset_id", "type": "string" },
            { "name": "risk", "type": "number" },
            { "name": "inspected_at", "type": "datetime" },
            { "name": "in_service", "type": "boolean" }
        ],
        "rows": [
            { "asset_id": "A-1", "risk": 0.82, "inspected_at": "2026-06-02T09:00:00Z", "in_service": true },
            { "asset_id": "A-2", "risk": 0.14, "inspected_at": "2026-06-11T09:00:00Z", "in_service": true },
            { "asset_id": "A-3", "risk": 0.55, "inspected_at": "2026-06-19T09:00:00Z", "in_service": false }
        ]
    })
}

// ─── Surface 5: context slice (§5) ────────────────────────────────────────────

/// Kept well under the 2 KiB truncation limit of §5.1.
pub fn slice_body() -> Value {
    json!({
        "schema_version": "1",
        "peer_id": SELF_PEER_ID,
        "summary": "Reference stub peer for IONe app-integration contract v1. Serves one canned \
                    resource per ione_view (map, chart, table, document), a whoami identity, and \
                    this capability slice, so federation consumers can be exercised without a \
                    real third-party application.",
        "domain_tags": ["geospatial", "time-series", "tabular", "compliance"],
        "sample_queries": [
            "What displacement did the stub peer report this month?",
            "List the stub peer's asset inventory.",
            "Show the stub peer's latest compliance report."
        ],
        "tool_index": [
            { "name": "query_displacement",
              "summary": "Time-series displacement for an AOI.",
              "expand_uri": "tools://query_displacement" },
            { "name": "acknowledge_alert",
              "summary": "Mark an alert acknowledged.",
              "expand_uri": "tools://acknowledge_alert",
              "approval_required": true }
        ],
        "resource_hints": {
            "example_resources": [
                { "uri_template": "stub://chart/{metric}", "description": "Metric time-series" }
            ],
            "recent_activity_summary_uri": "stub://activity/recent"
        }
    })
}

// ─── Surface 6: whoami (§6) ───────────────────────────────────────────────────

/// All seven §6 keys, present and non-null.
pub fn whoami_body() -> Value {
    json!({
        "peer_id": SELF_PEER_ID,
        "foreign_tenant_id": FOREIGN_TENANT_ID,
        "foreign_tenant_name": FOREIGN_TENANT_NAME,
        "foreign_workspace_id": FOREIGN_WORKSPACE_ID,
        "foreign_user_id": FOREIGN_USER_ID,
        "foreign_user_email": FOREIGN_USER_EMAIL,
        "foreign_roles": ["operator", "analyst"]
    })
}

// ─── Surface 2: OAuth 2.1 authorization server (§2) ───────────────────────────

async fn oauth_discovery(State(state): State<StubState>) -> Json<Value> {
    let base = &state.base_url;
    Json(json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/oauth/authorize"),
        "token_endpoint": format!("{base}/oauth/token"),
        "revocation_endpoint": format!("{base}/oauth/revoke"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none", "client_secret_post"],
        "scopes_supported": ["ione.read", "ione.write"]
    }))
}

/// Non-interactive: a real peer authenticates the operator here. The fixture has
/// no login UI, so it issues the code immediately — the PKCE binding it records
/// is real and is verified at the token endpoint.
async fn oauth_authorize(
    State(state): State<StubState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let Some(redirect_uri) = params.get("redirect_uri") else {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "redirect_uri required",
        );
    };
    if params.get("response_type").map(String::as_str) != Some("code") {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "unsupported_response_type",
            "only response_type=code is supported",
        );
    }
    let Some(challenge) = params.get("code_challenge") else {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "PKCE code_challenge is required",
        );
    };
    if params.get("code_challenge_method").map(String::as_str) != Some("S256") {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "only code_challenge_method=S256 is supported",
        );
    }

    let code = Uuid::new_v4().to_string();
    state
        .auth_codes
        .lock()
        .expect("auth codes mutex")
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
    State(state): State<StubState>,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    match form.get("grant_type").map(String::as_str) {
        Some("authorization_code") => {
            let Some(code) = form.get("code") else {
                return oauth_error(StatusCode::BAD_REQUEST, "invalid_request", "code required");
            };
            let Some(verifier) = form.get("code_verifier") else {
                return oauth_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "code_verifier required",
                );
            };
            let challenge = state
                .auth_codes
                .lock()
                .expect("auth codes mutex")
                .remove(code);
            let Some(challenge) = challenge else {
                return oauth_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_grant",
                    "unknown or already-redeemed authorization code",
                );
            };
            if pkce_s256(verifier) != challenge {
                return oauth_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_grant",
                    "code_verifier does not match code_challenge",
                );
            }
            Json(issue_tokens(&state, form.get("client_id"))).into_response()
        }
        Some("refresh_token") => {
            let Some(token) = form.get("refresh_token") else {
                return oauth_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "refresh_token required",
                );
            };
            let client_id = state
                .refresh_tokens
                .lock()
                .expect("refresh tokens mutex")
                .remove(token);
            match client_id {
                Some(client_id) => Json(issue_tokens(&state, Some(&client_id))).into_response(),
                None => oauth_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_grant",
                    "unknown or already-rotated refresh token",
                ),
            }
        }
        Some(other) => oauth_error(
            StatusCode::BAD_REQUEST,
            "unsupported_grant_type",
            &format!("grant_type '{other}' is not supported"),
        ),
        None => oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "grant_type required",
        ),
    }
}

async fn oauth_revoke(Form(_form): Form<HashMap<String, String>>) -> StatusCode {
    // RFC 7009: revocation always answers 200, whether or not the token existed.
    StatusCode::OK
}

fn issue_tokens(state: &StubState, client_id: Option<&String>) -> Value {
    let refresh = Uuid::new_v4().to_string();
    state
        .refresh_tokens
        .lock()
        .expect("refresh tokens mutex")
        .insert(
            refresh.clone(),
            client_id
                .cloned()
                .unwrap_or_else(|| "stub-client".to_string()),
        );
    json!({
        "access_token": Uuid::new_v4().to_string(),
        "token_type": "Bearer",
        "expires_in": 3600,
        "refresh_token": refresh,
        "scope": "ione.read"
    })
}

fn oauth_error(status: StatusCode, error: &str, description: &str) -> Response {
    (
        status,
        Json(json!({ "error": error, "error_description": description })),
    )
        .into_response()
}

fn pkce_s256(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

// ─── Surface 3: signed webhook sender (§3) ────────────────────────────────────

async fn emit_webhook_route(State(state): State<StubState>, Json(body): Json<Value>) -> Response {
    let Some(ione_base_url) = body.get("ione_base_url").and_then(Value::as_str) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "ione_base_url required" })),
        )
            .into_response();
    };
    let config = state.webhook.lock().expect("webhook mutex").clone();
    let Some(config) = config else {
        return (
            StatusCode::PRECONDITION_FAILED,
            Json(json!({ "error": "no webhook credential provisioned" })),
        )
            .into_response();
    };
    match send_signed_webhook(&state.http, ione_base_url, &config).await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let text = resp.text().await.unwrap_or_default();
            Json(json!({ "delivered": true, "status": status, "body": text })).into_response()
        }
        Err(err) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "delivered": false, "error": err })),
        )
            .into_response(),
    }
}

/// Build and send one §3.3 envelope with a §3.1 `X-IONe-Signature`.
///
/// The signature covers the **raw bytes transmitted**: the body is serialized
/// once, signed, and that same string is sent. Re-serializing before signing is
/// the classic integration bug this ordering avoids.
async fn send_signed_webhook(
    http: &reqwest::Client,
    ione_base_url: &str,
    config: &WebhookConfig,
) -> Result<reqwest::Response, String> {
    let now = Utc::now();
    let envelope = json!({
        "id": format!("evt-{}", Uuid::new_v4()),
        "type": "stub.observation.created",
        "occurred_at": now.to_rfc3339(),
        "peer_id": config.peer_id,
        "foreign_tenant_id": FOREIGN_TENANT_ID,
        "severity": "routine",
        "data": { "message": "stub peer conformance event", "displacement_mm": 2.4 },
        "approval_required": false
    });
    let raw = envelope.to_string();
    let signature = sign_webhook(&config.signing_secret, raw.as_bytes(), now.timestamp());

    http.post(format!(
        "{}/webhooks/peer/{}",
        ione_base_url.trim_end_matches('/'),
        config.peer_id
    ))
    .header(header::CONTENT_TYPE, "application/json")
    .header("X-IONe-Signature", signature)
    .body(raw)
    .send()
    .await
    .map_err(|err| err.to_string())
}

/// §3.1: `t=<unix>,v1=<hex64>` over HMAC-SHA256(`t_ascii ++ "." ++ raw_body`).
pub fn sign_webhook(secret: &str, raw_body: &[u8], timestamp: i64) -> String {
    type HmacSha256 = Hmac<Sha256>;
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("hmac accepts any key size");
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(raw_body);
    format!(
        "t={timestamp},v1={}",
        hex::encode(mac.finalize().into_bytes())
    )
}
