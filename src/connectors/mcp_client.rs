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
use tracing::warn;

use crate::connectors::{ConnectorImpl, PollResult, StreamDescriptor, StreamEventInput};
use crate::models::ConnectorKind;

pub struct McpClientConnector {
    pub mcp_url: String,
    pub bearer_token: String,
    pub http: reqwest::Client,
}

impl McpClientConnector {
    pub fn from_config(config: &Value) -> anyhow::Result<Self> {
        let mcp_url = config["mcp_url"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("mcp_client config missing 'mcp_url'"))?
            .to_string();
        let bearer_token = config["bearer_token"].as_str().unwrap_or("").to_string();

        Ok(Self {
            mcp_url,
            bearer_token,
            http: reqwest::Client::new(),
        })
    }

    /// Post a JSON-RPC 2.0 request to the peer's /mcp endpoint.
    async fn jsonrpc_call(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });

        let mut req = self.http.post(&self.mcp_url).json(&body);
        if !self.bearer_token.is_empty() {
            req = req.header("Authorization", format!("Bearer {}", self.bearer_token));
        }

        let resp = req.send().await?;

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
        let workspace_ids = self.resolve_all_peer_workspace_ids().await;
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
