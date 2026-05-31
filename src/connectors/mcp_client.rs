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
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });

        let token = self.resolve_bearer_token(false).await?;
        let mut resp = self.send_jsonrpc(&body, &token, mcp_session_id).await?;

        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            let Some(token) = self.try_refresh_bearer_token().await? else {
                return self.handle_jsonrpc_response(resp).await;
            };
            resp = self.send_jsonrpc(&body, &token, mcp_session_id).await?;
        }

        self.handle_jsonrpc_response(resp).await
    }

    async fn initialize_session(&self) -> anyhow::Result<String> {
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "protocolVersion": "2025-11-25", "capabilities": {} },
        });
        let token = self.resolve_bearer_token(false).await?;
        let resp = self.send_jsonrpc(&body, &token, None).await?;
        let header_session = resp
            .headers()
            .get("MCP-Session-Id")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let value: Value = resp.error_for_status()?.json().await?;
        if let Some(error) = value.get("error").filter(|error| !error.is_null()) {
            anyhow::bail!("peer initialize error: {}", error);
        }
        header_session
            .or_else(|| {
                value
                    .get("result")
                    .and_then(|result| result.get("sessionId"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .ok_or_else(|| anyhow::anyhow!("peer initialize did not return a session id"))
    }

    async fn handle_jsonrpc_response(&self, resp: reqwest::Response) -> anyhow::Result<Value> {
        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            anyhow::bail!("peer auth error: HTTP {}", status.as_u16());
        }
        if !status.is_success() {
            anyhow::bail!("peer returned HTTP {}", status.as_u16());
        }

        let val: Value = resp.json().await?;

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
        let mut req = self.http.post(&self.mcp_url).json(body);
        if !token.is_empty() {
            req = req.bearer_auth(token);
        }
        if let Some(session_id) = mcp_session_id {
            req = req.header("MCP-Session-Id", session_id);
        }
        Ok(req.send().await?)
    }

    async fn resolve_bearer_token(&self, force_refresh: bool) -> anyhow::Result<String> {
        if let (Some(pool), Some(peer_id)) = (&self.pool, self.peer_id) {
            if let Some(peer) = PeerRepo::new(pool.clone()).get(peer_id).await? {
                return if force_refresh {
                    crate::services::peer_tokens::refresh_access_token(pool, &self.http, &peer)
                        .await
                } else if peer.access_token_ciphertext.is_some() || self.bearer_token.is_empty() {
                    crate::services::peer_tokens::resolve_access_token(pool, &self.http, &peer)
                        .await
                } else {
                    Ok(self.bearer_token.clone())
                };
            }
        }
        Ok(self.bearer_token.clone())
    }

    async fn try_refresh_bearer_token(&self) -> anyhow::Result<Option<String>> {
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
        let workspace_ids = self.resolve_workspace_ids_with_binding().await;
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

    async fn resolve_workspace_ids_with_binding(&self) -> Vec<String> {
        if let (Some(pool), Some(workspace_id), Some(peer_id)) =
            (&self.pool, self.workspace_id, self.peer_id)
        {
            match WorkspacePeerBindingRepo::new(pool.clone())
                .get_by_workspace_peer(workspace_id, peer_id)
                .await
            {
                Ok(Some(binding)) if binding.status == BindingStatus::Active => {
                    if let Some(foreign_workspace_id) = binding.foreign_workspace_id {
                        if !foreign_workspace_id.is_empty() {
                            return vec![foreign_workspace_id];
                        }
                    }
                }
                Ok(_) => {}
                Err(e) => warn!("mcp_client: binding lookup failed during poll: {}", e),
            }
        }

        self.resolve_all_peer_workspace_ids().await
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
