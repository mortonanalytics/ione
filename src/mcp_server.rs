// Hand-rolled MCP 2025-03 subset over HTTP+SSE (JSON-RPC 2.0).
// Choice rationale: `rmcp` 1.5.0 has unstable axum integration and a large
// dependency surface. The MCP protocol surface needed for Phase 11 is small
// (initialize, tools/list, tools/call) and fits in ~500 lines. This implementation
// is isolated behind `pub fn router()` so swapping to rmcp later is a single-file
// change.

use std::convert::Infallible;

use axum::{
    extract::{Query, State},
    http::{header, HeaderMap},
    response::{
        sse::{Event, KeepAlive, Sse},
        Json,
    },
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    auth::{
        extract_session_id_from_headers, mode_from_env, session_key_from_env, AuthContext, AuthMode,
    },
    connectors::build_from_row,
    models::{ActorKind, ArtifactKind},
    repos::{
        ApprovalRepo, ArtifactRepo, AuditEventRepo, ConnectorRepo, SurvivorRepo, WorkspaceRepo,
    },
    state::AppState,
};

// ─── JSON-RPC 2.0 types ───────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    pub params: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: &'static str,
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl JsonRpcResponse {
    fn ok(id: Option<Value>, result: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    fn err(id: Option<Value>, code: i32, message: impl Into<String>, data: Option<Value>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data,
            }),
        }
    }
}

// ─── Tool schemas ─────────────────────────────────────────────────────────────

fn tool_list() -> Value {
    json!([
        {
            "name": "list_workspaces",
            "description": "List workspaces the caller has membership in (or all in local mode).",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "required": []
            }
        },
        {
            "name": "list_survivors",
            "description": "List survivor rows for a workspace, with optional verdict filter.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "workspace_id": { "type": "string", "format": "uuid" },
                    "verdict": { "type": "string", "enum": ["survive", "reject", "defer"] },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 500, "default": 50 }
                },
                "required": ["workspace_id"]
            }
        },
        {
            "name": "search_stream_events",
            "description": "Return recent stream_events for a workspace (client-side filtering). No vector search in Phase 11.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "workspace_id": { "type": "string", "format": "uuid" },
                    "query": { "type": "string" },
                    "stream_id": { "type": "string", "format": "uuid" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 500, "default": 50 }
                },
                "required": ["workspace_id"]
            }
        },
        {
            "name": "propose_artifact",
            "description": "Create an artifact with a pending approval. kind must be briefing, message, or report.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "workspace_id": { "type": "string", "format": "uuid" },
                    "kind": { "type": "string", "enum": ["briefing", "message", "report"] },
                    "content": { "type": "object" },
                    "source_survivor_id": { "type": "string", "format": "uuid" }
                },
                "required": ["workspace_id", "kind", "content"]
            }
        },
        {
            "name": "deliver_notification",
            "description": "Invoke an outbound connector directly and write a delivered audit event.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "workspace_id": { "type": "string", "format": "uuid" },
                    "connector_id": { "type": "string", "format": "uuid" },
                    "text": { "type": "string", "minLength": 1, "maxLength": 4096 }
                },
                "required": ["workspace_id", "connector_id", "text"]
            }
        }
    ])
}

// ─── Auth extraction ──────────────────────────────────────────────────────────

/// Resolve an `AuthContext` from:
/// 1. `Authorization: Bearer <jwt>` — verified against `trust_issuers` in DB.
/// 2. `Cookie: ione_session=…` — same cookie logic as existing middleware.
/// 3. Local mode fallback (no auth required).
///
/// Returns `None` when auth is required but absent/invalid.
pub async fn resolve_auth(state: &AppState, headers: &HeaderMap) -> Option<AuthContext> {
    let mode = mode_from_env();

    // 1. Bearer JWT path
    if let Some(bearer_ctx) = try_bearer_auth(state, headers).await {
        return Some(bearer_ctx);
    }

    // 2. Session cookie path
    let key = session_key_from_env();
    if let Some(session_id) = extract_session_id_from_headers(&key, headers) {
        let session = crate::repos::UserSessionRepo::new(state.pool.clone())
            .find_active(session_id)
            .await
            .ok()
            .flatten()?;
        return Some(AuthContext {
            user_id: session.user_id,
            org_id: session.org_id,
            is_oidc: true,
            is_mcp_peer: false,
            active_role_id: None,
            session_id: Some(session.id),
            mfa_verified: session.mfa_verified,
        });
    }

    // 3. Local mode fallback
    if mode == AuthMode::Local {
        let org_id = resolve_org_id(&state.pool, state.default_user_id)
            .await
            .unwrap_or(Uuid::nil());
        return Some(AuthContext {
            user_id: state.default_user_id,
            org_id,
            is_oidc: false,
            is_mcp_peer: false,
            active_role_id: None,
            session_id: None,
            mfa_verified: false,
        });
    }

    None
}

async fn resolve_org_id(pool: &sqlx::PgPool, user_id: Uuid) -> Option<Uuid> {
    sqlx::query_scalar::<_, Uuid>("SELECT org_id FROM users WHERE id = $1 LIMIT 1")
        .bind(user_id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
}

/// Try to authenticate via `Authorization: Bearer <jwt>`.
/// Verifies the JWT against trust_issuers in the DB for the default org.
/// Returns `None` if no bearer header, or if verification fails.
async fn try_bearer_auth(state: &AppState, headers: &HeaderMap) -> Option<AuthContext> {
    let auth_header = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let token = auth_header.strip_prefix("Bearer ")?;

    // Decode header to get issuer without verification first.
    let header = jsonwebtoken::decode_header(token).ok()?;
    let _ = header; // kid not used yet — we iterate all trust_issuers

    // Find matching trust_issuer by verifying the token.
    let org_id = resolve_default_org_id(&state.pool).await?;
    let issuers = crate::repos::TrustIssuerRepo::new(state.pool.clone())
        .list(org_id)
        .await
        .ok()?;

    for issuer in &issuers {
        if let Some(ctx) = verify_jwt_against_issuer(token, issuer, org_id, state).await {
            return Some(ctx);
        }
    }

    None
}

async fn resolve_default_org_id(pool: &sqlx::PgPool) -> Option<Uuid> {
    sqlx::query_scalar::<_, Uuid>("SELECT id FROM organizations WHERE name = 'Default Org' LIMIT 1")
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
}

async fn verify_jwt_against_issuer(
    token: &str,
    issuer: &crate::models::TrustIssuer,
    org_id: Uuid,
    state: &AppState,
) -> Option<AuthContext> {
    use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};

    // For local/test issuers we expect an HMAC-SHA256 secret in jwks_uri as
    // "secret:<base64url>". For real JWKS, Phase 12 can add RSA key fetching.
    let secret = issuer.jwks_uri.strip_prefix("secret:")?;
    let key_bytes = base64::Engine::decode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        secret.trim(),
    )
    .ok()?;
    let key = DecodingKey::from_secret(&key_bytes);

    let mut validation = Validation::new(Algorithm::HS256);
    validation.set_audience(&[&issuer.audience]);
    validation.set_issuer(&[&issuer.issuer_url]);

    let claims: jsonwebtoken::TokenData<Value> = decode(token, &key, &validation).ok()?;

    let sub = claims.claims["sub"].as_str()?;

    // Look up or create the user from the JWT subject.
    let user: Option<crate::models::User> =
        sqlx::query_as("SELECT id, org_id, email, display_name, oidc_subject, created_at FROM users WHERE org_id = $1 AND oidc_subject = $2 LIMIT 1")
            .bind(org_id)
            .bind(sub)
            .fetch_optional(&state.pool)
            .await
            .ok()
            .flatten();

    let user_id = user.map(|u| u.id).unwrap_or(state.default_user_id);

    Some(AuthContext {
        user_id,
        org_id,
        is_oidc: true,
        is_mcp_peer: true,
        active_role_id: None,
        session_id: None,
        mfa_verified: false,
    })
}

// ─── Tool dispatch ────────────────────────────────────────────────────────────

pub async fn call_tool(
    tool_name: &str,
    args: Value,
    auth: &AuthContext,
    state: &AppState,
) -> anyhow::Result<Value> {
    match tool_name {
        "list_workspaces" => tool_list_workspaces(auth, state).await,
        "list_survivors" => tool_list_survivors(args, auth, state).await,
        "search_stream_events" => tool_search_stream_events(args, auth, state).await,
        "propose_artifact" => tool_propose_artifact(args, auth, state).await,
        "deliver_notification" => tool_deliver_notification(args, auth, state).await,
        other => anyhow::bail!("unknown tool: {}", other),
    }
}

// ── list_workspaces ───────────────────────────────────────────────────────────

async fn tool_list_workspaces(auth: &AuthContext, state: &AppState) -> anyhow::Result<Value> {
    let repo = WorkspaceRepo::new(state.pool.clone());
    let workspaces = repo.list(auth.org_id).await?;
    let items: Vec<Value> = workspaces
        .iter()
        .map(|w| {
            json!({
                "id": w.id,
                "name": w.name,
                "domain": w.domain,
                "lifecycle": w.lifecycle,
            })
        })
        .collect();
    Ok(json!({ "workspaces": items }))
}

// ── list_survivors ────────────────────────────────────────────────────────────

async fn tool_list_survivors(
    args: Value,
    _auth: &AuthContext,
    state: &AppState,
) -> anyhow::Result<Value> {
    let workspace_id = parse_uuid(&args, "workspace_id")?;
    let verdict = parse_optional_verdict(&args)?;
    let limit = args["limit"].as_i64().unwrap_or(50).clamp(1, 500);

    let repo = SurvivorRepo::new(state.pool.clone());
    let rows = repo.list(workspace_id, verdict, limit).await?;
    let items: Vec<Value> = rows
        .iter()
        .map(|r| serde_json::to_value(r).unwrap_or(Value::Null))
        .collect();
    Ok(json!({ "survivors": items }))
}

fn parse_optional_verdict(args: &Value) -> anyhow::Result<Option<crate::models::CriticVerdict>> {
    use crate::models::CriticVerdict;
    let v = match args["verdict"].as_str() {
        None | Some("") => return Ok(None),
        Some(s) => s,
    };
    match v {
        "survive" => Ok(Some(CriticVerdict::Survive)),
        "reject" => Ok(Some(CriticVerdict::Reject)),
        "defer" => Ok(Some(CriticVerdict::Defer)),
        other => anyhow::bail!("invalid verdict: {}", other),
    }
}

// ── search_stream_events ──────────────────────────────────────────────────────

async fn tool_search_stream_events(
    args: Value,
    _auth: &AuthContext,
    state: &AppState,
) -> anyhow::Result<Value> {
    let workspace_id = parse_uuid(&args, "workspace_id")?;
    let stream_id_filter = parse_optional_uuid(&args, "stream_id")?;
    let query_filter = args["query"].as_str().map(str::to_lowercase);
    let limit = args["limit"].as_i64().unwrap_or(50).clamp(1, 500);

    let events =
        fetch_stream_events_for_workspace(&state.pool, workspace_id, stream_id_filter, limit)
            .await?;

    let filtered: Vec<Value> = events
        .into_iter()
        .filter(|e| {
            if let Some(ref q) = query_filter {
                let payload_str = e["payload"].to_string().to_lowercase();
                payload_str.contains(q.as_str())
            } else {
                true
            }
        })
        .collect();

    Ok(json!({ "events": filtered }))
}

type StreamEventRow = (
    Uuid,
    Uuid,
    Value,
    chrono::DateTime<chrono::Utc>,
    chrono::DateTime<chrono::Utc>,
);

fn stream_event_row_to_value(
    (id, sid, payload, observed_at, ingested_at): StreamEventRow,
) -> Value {
    json!({
        "id": id,
        "stream_id": sid,
        "payload": payload,
        "observed_at": observed_at,
        "ingested_at": ingested_at,
    })
}

async fn fetch_stream_events_for_workspace(
    pool: &sqlx::PgPool,
    workspace_id: Uuid,
    stream_id_filter: Option<Uuid>,
    limit: i64,
) -> anyhow::Result<Vec<Value>> {
    if let Some(stream_id) = stream_id_filter {
        let rows: Vec<StreamEventRow> = sqlx::query_as(
            "SELECT se.id, se.stream_id, se.payload, se.observed_at, se.ingested_at
             FROM stream_events se
             JOIN streams s ON s.id = se.stream_id
             JOIN connectors c ON c.id = s.connector_id
             WHERE c.workspace_id = $1
               AND se.stream_id = $2
             ORDER BY se.observed_at DESC
             LIMIT $3",
        )
        .bind(workspace_id)
        .bind(stream_id)
        .bind(limit)
        .fetch_all(pool)
        .await?;
        Ok(rows.into_iter().map(stream_event_row_to_value).collect())
    } else {
        let rows: Vec<StreamEventRow> = sqlx::query_as(
            "SELECT se.id, se.stream_id, se.payload, se.observed_at, se.ingested_at
             FROM stream_events se
             JOIN streams s ON s.id = se.stream_id
             JOIN connectors c ON c.id = s.connector_id
             WHERE c.workspace_id = $1
             ORDER BY se.observed_at DESC
             LIMIT $2",
        )
        .bind(workspace_id)
        .bind(limit)
        .fetch_all(pool)
        .await?;
        Ok(rows.into_iter().map(stream_event_row_to_value).collect())
    }
}

// ── propose_artifact ──────────────────────────────────────────────────────────

/// Forbidden kinds: only IONe's internal delivery path may create these.
const FORBIDDEN_PROPOSE_KINDS: &[&str] = &["notification_draft", "resource_order"];

async fn tool_propose_artifact(
    args: Value,
    _auth: &AuthContext,
    state: &AppState,
) -> anyhow::Result<Value> {
    let workspace_id = parse_uuid(&args, "workspace_id")?;

    let kind_str = args["kind"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing required field: kind"))?;

    if FORBIDDEN_PROPOSE_KINDS.contains(&kind_str) {
        anyhow::bail!("FORBIDDEN: kind '{}' may not be proposed via MCP; only IONe's internal delivery path may create this kind", kind_str);
    }

    let kind = parse_artifact_kind(kind_str)?;

    let content = args
        .get("content")
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("missing required field: content"))?;

    let source_survivor_id = parse_optional_uuid(&args, "source_survivor_id")?;

    let artifact_repo = ArtifactRepo::new(state.pool.clone());
    let approval_repo = ApprovalRepo::new(state.pool.clone());

    let artifact = artifact_repo
        .insert(workspace_id, kind, source_survivor_id, content, None)
        .await?;

    let approval = approval_repo.create_pending(artifact.id).await?;

    Ok(json!({
        "artifact_id": artifact.id,
        "approval_id": approval.id,
    }))
}

fn parse_artifact_kind(s: &str) -> anyhow::Result<ArtifactKind> {
    match s {
        "briefing" => Ok(ArtifactKind::Briefing),
        "notification_draft" => Ok(ArtifactKind::NotificationDraft),
        "resource_order" => Ok(ArtifactKind::ResourceOrder),
        "message" => Ok(ArtifactKind::Message),
        "report" => Ok(ArtifactKind::Report),
        other => anyhow::bail!("unknown artifact kind: {}", other),
    }
}

// ── deliver_notification ──────────────────────────────────────────────────────

async fn tool_deliver_notification(
    args: Value,
    auth: &AuthContext,
    state: &AppState,
) -> anyhow::Result<Value> {
    let workspace_id = parse_uuid(&args, "workspace_id")?;
    let connector_id = parse_uuid(&args, "connector_id")?;
    let text = args["text"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("missing required field: text"))?;

    if text.len() > 4096 {
        anyhow::bail!("text exceeds 4096 character limit");
    }

    let connector_repo = ConnectorRepo::new(state.pool.clone());
    let connector = connector_repo
        .get(connector_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("connector {} not found", connector_id))?;

    let impl_ = build_from_row(&connector)?;

    impl_
        .invoke("send", json!({ "text": text }))
        .await
        .map_err(|e| anyhow::anyhow!("delivery failed: {}", e))?;

    // Actor kind: peer if authenticated via bearer JWT from a trusted peer issuer, user otherwise.
    let actor_kind = if auth.is_mcp_peer {
        ActorKind::Peer
    } else {
        ActorKind::User
    };

    let audit_repo = AuditEventRepo::new(state.pool.clone());
    audit_repo
        .insert(
            Some(workspace_id),
            actor_kind,
            &auth.user_id.to_string(),
            "delivered",
            "connector",
            Some(connector_id),
            json!({ "source": "mcp", "text_len": text.len() }),
        )
        .await?;

    Ok(json!({ "delivered": true, "connector_id": connector_id }))
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn parse_uuid(args: &Value, field: &str) -> anyhow::Result<Uuid> {
    let s = args[field]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing required field: {}", field))?;
    Uuid::parse_str(s).map_err(|e| anyhow::anyhow!("invalid UUID for field '{}': {}", field, e))
}

fn parse_optional_uuid(args: &Value, field: &str) -> anyhow::Result<Option<Uuid>> {
    match args[field].as_str() {
        None | Some("") => Ok(None),
        Some(s) => Ok(Some(Uuid::parse_str(s).map_err(|e| {
            anyhow::anyhow!("invalid UUID for field '{}': {}", field, e)
        })?)),
    }
}

// ─── HTTP handlers ────────────────────────────────────────────────────────────

/// POST /mcp — JSON-RPC 2.0 request/response over plain HTTP.
/// Used for `initialize`, `tools/list`, `tools/call`.
pub async fn jsonrpc_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<JsonRpcRequest>,
) -> Json<JsonRpcResponse> {
    if req.jsonrpc != "2.0" {
        return Json(JsonRpcResponse::err(
            req.id,
            -32600,
            "invalid JSON-RPC version; expected 2.0",
            None,
        ));
    }

    let resp = dispatch_method(&state, &headers, req).await;
    Json(resp)
}

async fn dispatch_method(
    state: &AppState,
    headers: &HeaderMap,
    req: JsonRpcRequest,
) -> JsonRpcResponse {
    match req.method.as_str() {
        "initialize" => handle_initialize(req.id),
        "tools/list" => handle_tools_list(req.id),
        "tools/call" => {
            let auth = match resolve_auth(state, headers).await {
                Some(a) => a,
                None => {
                    return JsonRpcResponse::err(
                        req.id,
                        -32001,
                        "unauthorized: valid session cookie or bearer JWT required",
                        None,
                    )
                }
            };
            handle_tools_call(req.id, req.params.unwrap_or(Value::Null), &auth, state).await
        }
        other => JsonRpcResponse::err(req.id, -32601, format!("method not found: {}", other), None),
    }
}

fn handle_initialize(id: Option<Value>) -> JsonRpcResponse {
    JsonRpcResponse::ok(
        id,
        json!({
            "protocolVersion": "2025-03",
            "serverInfo": {
                "name": "ione",
                "version": env!("CARGO_PKG_VERSION")
            },
            "capabilities": {
                "tools": {}
            }
        }),
    )
}

fn handle_tools_list(id: Option<Value>) -> JsonRpcResponse {
    JsonRpcResponse::ok(id, json!({ "tools": tool_list() }))
}

async fn handle_tools_call(
    id: Option<Value>,
    params: Value,
    auth: &AuthContext,
    state: &AppState,
) -> JsonRpcResponse {
    let tool_name = match params["name"].as_str() {
        Some(n) if !n.is_empty() => n,
        _ => return JsonRpcResponse::err(id, -32602, "params.name is required", None),
    };

    let args = params["arguments"].clone();

    // Schema-level validation: workspace_id required for multi-workspace tools.
    if needs_workspace_id(tool_name) && args["workspace_id"].as_str().is_none() {
        return JsonRpcResponse::err(
            id,
            -32602,
            format!(
                "schema validation: workspace_id is required for tool '{}'",
                tool_name
            ),
            Some(json!({ "field": "workspace_id", "issue": "required" })),
        );
    }

    match call_tool(tool_name, args, auth, state).await {
        Ok(result) => JsonRpcResponse::ok(
            id,
            json!({ "content": [{ "type": "text", "text": result.to_string() }], "isError": false }),
        ),
        Err(e) => {
            let msg = e.to_string();
            // Forbidden kinds get a distinct error code clients can detect.
            let code = if msg.starts_with("FORBIDDEN:") {
                -32403
            } else {
                -32000
            };
            JsonRpcResponse::err(id, code, msg, None)
        }
    }
}

fn needs_workspace_id(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "list_survivors" | "search_stream_events" | "propose_artifact" | "deliver_notification"
    )
}

/// GET /mcp/sse — SSE endpoint for MCP clients that prefer the SSE transport.
/// Accepts an optional `?session=<jsonrpc_base64>` parameter for inline requests.
/// Without a session parameter, returns the server capabilities as the first event.
#[derive(Deserialize)]
pub struct SseQuery {
    pub session: Option<String>,
}

pub async fn sse_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<SseQuery>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let init_event = build_init_event();

    let stream = if let Some(encoded) = query.session {
        // Inline request: decode and dispatch.
        let response_event = handle_inline_sse_request(&state, &headers, &encoded).await;
        tokio_stream::iter(vec![Ok(init_event), Ok(response_event)])
    } else {
        tokio_stream::iter(vec![Ok(init_event)])
    };

    Sse::new(stream).keep_alive(KeepAlive::default())
}

fn build_init_event() -> Event {
    let payload = json!({
        "protocolVersion": "2025-03",
        "serverInfo": { "name": "ione", "version": env!("CARGO_PKG_VERSION") },
        "capabilities": { "tools": {} }
    });
    Event::default()
        .event("initialize")
        .data(payload.to_string())
}

async fn handle_inline_sse_request(state: &AppState, headers: &HeaderMap, encoded: &str) -> Event {
    use base64::Engine as _;
    let decoded = match base64::engine::general_purpose::STANDARD.decode(encoded) {
        Ok(b) => b,
        Err(_) => {
            return sse_error_event("invalid base64 in session parameter");
        }
    };
    let req: JsonRpcRequest = match serde_json::from_slice(&decoded) {
        Ok(r) => r,
        Err(_) => {
            return sse_error_event("invalid JSON-RPC in session parameter");
        }
    };

    let resp = dispatch_method(state, headers, req).await;
    let payload = serde_json::to_string(&resp).unwrap_or_else(|_| "{}".to_string());
    Event::default().event("message").data(payload)
}

fn sse_error_event(msg: &str) -> Event {
    let payload = json!({ "error": msg });
    Event::default().event("error").data(payload.to_string())
}

// ─── Router ───────────────────────────────────────────────────────────────────

/// Mount the MCP server router. No auth_middleware — MCP has its own auth
/// at the tool-call level.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/mcp", post(jsonrpc_handler))
        .route("/mcp/sse", get(sse_handler))
}
