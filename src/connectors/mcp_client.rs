/// MCP client connector — calls a remote IONe node's /mcp endpoint.
///
/// Config shape: `{ "mcp_url": "https://…", "bearer_token": "…" }`
///
/// default_streams: queries the peer's tools/list and exposes one synthetic stream
/// per readable tool (list_survivors, search_stream_events).
///
/// poll(stream_name, cursor): calls the corresponding MCP tool on the peer and maps
/// returned items into StreamEventInput rows.
///
/// invoke(op, args): calls the peer's MCP tool `op` with `args`. Used for outbound
/// peer writes (e.g. propose_artifact).
use serde_json::{json, Value};
use sqlx::PgPool;
use tracing::warn;
use uuid::Uuid;

use crate::connectors::{ConnectorImpl, PollResult, StreamDescriptor, StreamEventInput};
use crate::models::{BindingStatus, ConnectorKind};
use crate::repos::{PeerRepo, WorkspacePeerBindingRepo};
use crate::services::peer_tokens::ResolvedBearer;

pub struct McpClientConnector {
    pub mcp_url: String,
    pub bearer_token: String,
    pub http: reqwest::Client,
    pub workspace_id: Option<Uuid>,
    pub peer_id: Option<Uuid>,
    pub pool: Option<PgPool>,
}

impl McpClientConnector {
    pub fn from_config(config: &Value, pool: Option<PgPool>) -> anyhow::Result<Self> {
        let mcp_url = config["mcp_url"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("mcp_client config missing 'mcp_url'"))?
            .to_string();
        let bearer_token = config["bearer_token"].as_str().unwrap_or("").to_string();
        let workspace_id = config["workspace_id"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok());
        let peer_id = config["peer_id"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok());

        Ok(Self {
            mcp_url,
            bearer_token,
            http: crate::util::url_guard::guarded_client(15_000),
            workspace_id,
            peer_id,
            pool,
        })
    }

    /// Post a JSON-RPC 2.0 request to the peer's /mcp endpoint.
    async fn jsonrpc_call(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        match self.jsonrpc_call_once(method, params.clone(), None).await {
            Ok(value) => Ok(value),
            Err(e) if looks_like_missing_session(&e) => {
                let session_id = self.initialize_session().await?;
                self.jsonrpc_call_once(method, params, Some(&session_id))
                    .await
            }
            Err(e) => Err(e),
        }
    }

    async fn jsonrpc_call_once(
        &self,
        method: &str,
        params: Value,
        mcp_session_id: Option<&str>,
    ) -> anyhow::Result<Value> {
        let request_id = crate::services::peer_tokens::next_request_id();
        let body = json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": method,
            "params": params,
        });

        let bearer = self.resolve_bearer().await?;
        let mut resp = self
            .send_jsonrpc(&body, &bearer.token, mcp_session_id)
            .await?;

        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            let Some(token) = self.try_refresh_bearer_token(&bearer).await? else {
                return self.handle_jsonrpc_response(resp, &request_id).await;
            };
            resp = self.send_jsonrpc(&body, &token, mcp_session_id).await?;
        }

        self.handle_jsonrpc_response(resp, &request_id).await
    }

    async fn initialize_session(&self) -> anyhow::Result<String> {
        let request_id = crate::services::peer_tokens::next_request_id();
        let body = json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "initialize",
            "params": {
                "protocolVersion": crate::services::peer_tokens::MCP_PROTOCOL_VERSION,
                "capabilities": {},
            },
        });
        let bearer = self.resolve_bearer().await?;
        let resp = self.send_jsonrpc(&body, &bearer.token, None).await?;
        let header_session = resp
            .headers()
            .get("MCP-Session-Id")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let resp = resp.error_for_status()?;
        let value = crate::services::peer_tokens::read_jsonrpc_reply(resp, &request_id).await?;
        if let Some(error) = value.get("error").filter(|error| !error.is_null()) {
            anyhow::bail!("peer initialize error: {}", error);
        }
        let session_id = header_session
            .or_else(|| {
                value
                    .get("result")
                    .and_then(|result| result.get("sessionId"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .ok_or_else(|| anyhow::anyhow!("peer initialize did not return a session id"))?;
        self.send_initialized_notification(&session_id).await;
        Ok(session_id)
    }

    /// MCP lifecycle: the client announces it is initialized before issuing any
    /// other request. Best-effort — a peer that rejects the notification must
    /// not fail an otherwise-good handshake.
    async fn send_initialized_notification(&self, mcp_session_id: &str) {
        let body = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        let bearer = match self.resolve_bearer().await {
            Ok(bearer) => bearer,
            Err(e) => {
                warn!("mcp_client: notifications/initialized token resolve failed: {e}");
                return;
            }
        };
        if let Err(e) = self
            .send_jsonrpc(&body, &bearer.token, Some(mcp_session_id))
            .await
        {
            warn!("mcp_client: notifications/initialized send failed: {e}");
        }
    }

    async fn handle_jsonrpc_response(
        &self,
        resp: reqwest::Response,
        request_id: &Value,
    ) -> anyhow::Result<Value> {
        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            anyhow::bail!("peer auth error: HTTP {}", status.as_u16());
        }
        if !status.is_success() {
            anyhow::bail!("peer returned HTTP {}", status.as_u16());
        }

        let val = crate::services::peer_tokens::read_jsonrpc_reply(resp, request_id).await?;

        if let Some(err) = val.get("error") {
            if !err.is_null() {
                anyhow::bail!("peer MCP error: {}", err);
            }
        }

        Ok(val["result"].clone())
    }

    async fn send_jsonrpc(
        &self,
        body: &Value,
        token: &str,
        mcp_session_id: Option<&str>,
    ) -> anyhow::Result<reqwest::Response> {
        let mut req = self
            .http
            .post(&self.mcp_url)
            .header(
                reqwest::header::ACCEPT,
                crate::services::peer_tokens::MCP_ACCEPT,
            )
            .header(
                crate::services::peer_tokens::MCP_PROTOCOL_VERSION_HEADER,
                crate::services::peer_tokens::MCP_PROTOCOL_VERSION,
            )
            .json(body);
        if !token.is_empty() {
            req = req.bearer_auth(token);
        }
        if let Some(session_id) = mcp_session_id {
            req = req.header("MCP-Session-Id", session_id);
        }
        Ok(req.send().await?)
    }

    /// The outbound bearer for this connector, tagged with the precedence tier
    /// that produced it so the 401 path can tell which credential was rejected.
    async fn resolve_bearer(&self) -> anyhow::Result<ResolvedBearer> {
        if let (Some(pool), Some(peer_id)) = (&self.pool, self.peer_id) {
            if let Some(peer) = PeerRepo::new(pool.clone()).get(peer_id).await? {
                // This connector polls one workspace, so its outbound auth is
                // resolved in that workspace's scope (pre-broker credential, #19).
                let peer = match self.workspace_id {
                    Some(workspace_id) => peer.scoped_to(workspace_id),
                    None => peer,
                };
                if self.bearer_token.is_empty() {
                    return crate::services::peer_tokens::resolve_bearer(pool, &self.http, &peer)
                        .await;
                }
                // The literal in connector config is the LAST resort: it stands
                // in for the process-global env fallback, below the brokered
                // delegated token (#12), the peer-global OAuth token, and the
                // per-(workspace, peer) credential (#19). So rotating any of
                // those through the API takes effect without rewriting
                // connector rows.
                if let Some(bearer) =
                    crate::services::peer_tokens::resolve_bearer_above_env(pool, &self.http, &peer)
                        .await?
                {
                    return Ok(bearer);
                }
            }
        }
        Ok(ResolvedBearer::last_resort(self.bearer_token.clone()))
    }

    /// The peer-global token to retry a 401 with, or `None` when this 401 must
    /// be surfaced instead.
    ///
    /// A 401 never downgrades the tier
    /// (`md/design/pre-broker-peer-credentials.md`). This is the default poll
    /// path — `auto_create_connector_for_peer` writes no `bearer_token`, so
    /// every subscribed peer resolves through the full precedence chain — and
    /// without the tier check a rejected tier-1 delegated token would be
    /// re-presented as the peer-global grant, widening the scope the operator
    /// consented to.
    async fn try_refresh_bearer_token(
        &self,
        rejected: &ResolvedBearer,
    ) -> anyhow::Result<Option<String>> {
        if !rejected.allows_peer_global_refresh() {
            return Ok(None);
        }
        let (Some(pool), Some(peer_id)) = (&self.pool, self.peer_id) else {
            return Ok(None);
        };
        let Some(peer) = PeerRepo::new(pool.clone()).get(peer_id).await? else {
            return Ok(None);
        };
        if peer.refresh_token_ciphertext.is_none() || peer.oauth_client_id.is_none() {
            return Ok(None);
        }
        crate::services::peer_tokens::refresh_access_token(pool, &self.http, &peer)
            .await
            .map(Some)
    }
}

fn looks_like_missing_session(error: &anyhow::Error) -> bool {
    let msg = error.to_string().to_ascii_lowercase();
    msg.contains("mcp-session-id") || msg.contains("session not found")
}

// Readable tool names that map to synthetic pull streams.
const READABLE_TOOLS: &[&str] = &["list_survivors", "search_stream_events"];

#[async_trait::async_trait]
impl ConnectorImpl for McpClientConnector {
    fn kind(&self) -> ConnectorKind {
        ConnectorKind::Mcp
    }

    async fn default_streams(&self) -> anyhow::Result<Vec<StreamDescriptor>> {
        // Query the peer's tools/list and expose one stream per readable tool.
        let result = self.jsonrpc_call("tools/list", Value::Null).await?;

        let tools = result["tools"].as_array().cloned().unwrap_or_default();

        let streams = tools
            .iter()
            .filter_map(|t| t["name"].as_str())
            .filter(|name| READABLE_TOOLS.contains(name))
            .map(|name| StreamDescriptor {
                name: name.to_string(),
                schema: json!({ "type": "object", "description": format!("Results from peer tool {}", name) }),
                view_config: None,
            })
            .collect();

        Ok(streams)
    }

    async fn poll(&self, stream_name: &str, _cursor: Option<Value>) -> anyhow::Result<PollResult> {
        // Only poll readable tools.
        if !READABLE_TOOLS.contains(&stream_name) {
            anyhow::bail!("mcp_client: stream '{}' is not pollable", stream_name);
        }

        // list_survivors and search_stream_events require workspace_id.
        // Resolve all workspace ids and aggregate results.
        let workspace_ids = self.resolve_workspace_ids_with_binding().await?;
        let now = chrono::Utc::now();
        let mut all_events = Vec::new();

        for workspace_id_str in &workspace_ids {
            if workspace_id_str.is_empty() {
                continue;
            }
            let result = self
                .jsonrpc_call(
                    "tools/call",
                    json!({
                        "name": stream_name,
                        "arguments": { "workspace_id": workspace_id_str }
                    }),
                )
                .await?;

            let content_text = result["content"][0]["text"].as_str().unwrap_or("{}");
            let data: Value = serde_json::from_str(content_text).unwrap_or_else(|_| json!({}));

            let items = extract_items_from_tool_result(&data, stream_name);
            for item in items {
                all_events.push(StreamEventInput {
                    payload: item,
                    observed_at: now,
                    dedup_key: None,
                });
            }
        }

        Ok(PollResult {
            events: all_events,
            next_cursor: None,
        })
    }

    fn supports_invoke(&self) -> bool {
        true
    }

    async fn invoke(&self, op: &str, args: Value) -> anyhow::Result<Value> {
        self.jsonrpc_call(
            "tools/call",
            json!({
                "name": op,
                "arguments": args,
            }),
        )
        .await
    }
}

impl McpClientConnector {
    /// Resolve all peer workspace ids via tools/call list_workspaces.
    async fn resolve_all_peer_workspace_ids(&self) -> Vec<String> {
        match self
            .jsonrpc_call(
                "tools/call",
                json!({ "name": "list_workspaces", "arguments": {} }),
            )
            .await
        {
            Ok(result) => {
                let text = result["content"][0]["text"].as_str().unwrap_or("{}");
                let data: Value = serde_json::from_str(text).unwrap_or_else(|_| json!({}));
                data["workspaces"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|w| w["id"].as_str().map(str::to_string))
                    .collect()
            }
            Err(e) => {
                warn!("mcp_client: list_workspaces failed during poll: {}", e);
                vec![]
            }
        }
    }

    async fn resolve_workspace_ids_with_binding(&self) -> anyhow::Result<Vec<String>> {
        // When this connector is bound to a workspace+peer, the poll scope MUST come
        // from an Active binding's foreign_workspace_id. Falling back to the peer-wide
        // unscoped enumeration here would read across workspaces the binding does not
        // authorize (C-1 workspace-isolation leak), so this path fails closed.
        if let (Some(pool), Some(workspace_id), Some(peer_id)) =
            (&self.pool, self.workspace_id, self.peer_id)
        {
            return match WorkspacePeerBindingRepo::new(pool.clone())
                .get_by_workspace_peer(workspace_id, peer_id)
                .await
            {
                Ok(Some(binding)) if binding.status == BindingStatus::Active => {
                    match binding.foreign_workspace_id {
                        Some(fw) if !fw.is_empty() => Ok(vec![fw]),
                        _ => anyhow::bail!(
                            "mcp_client: active binding for peer {} has no foreign_workspace_id; poll blocked",
                            peer_id
                        ),
                    }
                }
                Ok(_) => anyhow::bail!(
                    "mcp_client: no active workspace_peer_binding for peer {}; poll blocked (fail-closed)",
                    peer_id
                ),
                Err(e) => Err(e.context("mcp_client: binding lookup failed during poll")),
            };
        }

        // Unbound connector (no workspace/peer context): legacy peer-wide resolution.
        Ok(self.resolve_all_peer_workspace_ids().await)
    }

    /// Resolve the peer's first workspace id via tools/call list_workspaces.
    pub async fn resolve_peer_workspace_id(&self) -> String {
        let ids = self.resolve_all_peer_workspace_ids().await;
        ids.into_iter().next().unwrap_or_default()
    }
}

/// Extract the array of items from a tools/call result for the given tool.
fn extract_items_from_tool_result(data: &Value, stream_name: &str) -> Vec<Value> {
    match stream_name {
        "list_survivors" => data["survivors"].as_array().cloned().unwrap_or_default(),
        "search_stream_events" => data["events"].as_array().cloned().unwrap_or_default(),
        _ => vec![],
    }
}
