use std::time::Duration;

use anyhow::Context;
use serde::Serialize;
use serde_json::Value;

use crate::models::Peer;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChartDataResponse {
    pub spec: Value,
    pub rows: Vec<Value>,
}

pub async fn fetch_chart_data(
    http: &reqwest::Client,
    peer: &Peer,
    uri: &str,
) -> anyhow::Result<ChartDataResponse> {
    let token = resolve_token(peer)?;
    let endpoint = peer.mcp_url.trim_end_matches('/').to_string();
    tokio::time::timeout(
        Duration::from_secs(5),
        call_resources_read(http, &endpoint, &token, uri),
    )
    .await
    .context("timeout")?
}

async fn call_resources_read(
    http: &reqwest::Client,
    endpoint: &str,
    token: &str,
    uri: &str,
) -> anyhow::Result<ChartDataResponse> {
    let resp: Value = http
        .post(endpoint)
        .bearer_auth(token)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "resources/read",
            "params": { "uri": uri }
        }))
        .send()
        .await
        .context("HTTP send failed")?
        .error_for_status()
        .context("peer returned error status")?
        .json()
        .await
        .context("failed to parse peer response")?;

    if let Some(err) = resp.get("error").filter(|v| !v.is_null()) {
        anyhow::bail!("peer MCP error: {err}");
    }

    let text = resp["result"]["contents"]
        .as_array()
        .and_then(|contents| contents.first())
        .and_then(|content| content.get("text"))
        .and_then(Value::as_str)
        .context("resources/read response missing result.contents[0].text")?;
    let body: Value = serde_json::from_str(text).context("invalid chart resource JSON")?;
    let spec = body
        .get("spec")
        .cloned()
        .context("chart resource missing spec")?;
    let rows = body
        .get("rows")
        .and_then(Value::as_array)
        .cloned()
        .context("chart resource missing rows")?;

    Ok(ChartDataResponse { spec, rows })
}

fn resolve_token(peer: &Peer) -> anyhow::Result<String> {
    if let Some(ct) = &peer.access_token_ciphertext {
        return crate::util::token_crypto::decrypt_token(ct)
            .context("failed to decrypt peer token");
    }
    std::env::var("IONE_OAUTH_STATIC_BEARER")
        .context("peer has no token and IONE_OAUTH_STATIC_BEARER is not set")
}
