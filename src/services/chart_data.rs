use std::{fmt, time::Duration};

use serde::Serialize;
use serde_json::Value;

use crate::{models::Peer, state::AppState};

/// Contract v1 §4.3/§8.3: a chart `resources/read` response must stay at or
/// below 2 MiB, matching the table limit. Enforcing an already-documented limit
/// is additive under the compatibility rule.
const MAX_CHART_RESOURCE_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChartDataResponse {
    pub spec: Value,
    pub rows: Vec<Value>,
}

#[derive(Debug)]
pub enum ChartDataError {
    TooLarge(String),
    Unavailable(String),
}

impl fmt::Display for ChartDataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChartDataError::TooLarge(msg) | ChartDataError::Unavailable(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for ChartDataError {}

pub async fn fetch_chart_data(
    state: &AppState,
    peer: &Peer,
    uri: &str,
) -> Result<ChartDataResponse, ChartDataError> {
    let endpoint = peer.mcp_url.trim_end_matches('/').to_string();
    tokio::time::timeout(
        Duration::from_secs(5),
        call_resources_read(state, peer, &endpoint, uri),
    )
    .await
    .map_err(|_| ChartDataError::Unavailable("timeout".to_string()))?
}

async fn call_resources_read(
    state: &AppState,
    peer: &Peer,
    endpoint: &str,
    uri: &str,
) -> Result<ChartDataResponse, ChartDataError> {
    let request_id = Value::from(1);
    let response = crate::services::peer_tokens::send_mcp_request(
        &state.pool,
        &state.http,
        peer,
        endpoint,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "resources/read",
            "params": { "uri": uri }
        }),
    )
    .await
    .map_err(|err| ChartDataError::Unavailable(format!("HTTP send failed: {err}")))?
    .error_for_status()
    .map_err(|err| ChartDataError::Unavailable(format!("peer returned error status: {err}")))?;

    // Same shape as the table path: reject before buffering when the peer
    // declares an oversized body, and cap the payload-carrying text again below
    // for peers that answer chunked.
    if response
        .content_length()
        .is_some_and(|len| len > MAX_CHART_RESOURCE_BYTES as u64)
    {
        return Err(ChartDataError::TooLarge(
            "chart resource response is larger than 2 MiB".to_string(),
        ));
    }

    let resp = crate::services::peer_tokens::read_jsonrpc_reply(response, &request_id)
        .await
        .map_err(|err| {
            ChartDataError::Unavailable(format!("failed to parse peer response: {err}"))
        })?;

    if let Some(err) = resp.get("error").filter(|v| !v.is_null()) {
        return Err(ChartDataError::Unavailable(format!(
            "peer MCP error: {err}"
        )));
    }

    let text = resp["result"]["contents"]
        .as_array()
        .and_then(|contents| contents.first())
        .and_then(|content| content.get("text"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            ChartDataError::Unavailable(
                "resources/read response missing result.contents[0].text".to_string(),
            )
        })?;
    if text.len() > MAX_CHART_RESOURCE_BYTES {
        return Err(ChartDataError::TooLarge(
            "chart resource body is larger than 2 MiB".to_string(),
        ));
    }

    let body: Value = serde_json::from_str(text).map_err(|err| {
        ChartDataError::Unavailable(format!("invalid chart resource JSON: {err}"))
    })?;
    let spec = body
        .get("spec")
        .cloned()
        .ok_or_else(|| ChartDataError::Unavailable("chart resource missing spec".to_string()))?;
    let rows = body
        .get("rows")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| ChartDataError::Unavailable("chart resource missing rows".to_string()))?;

    Ok(ChartDataResponse { spec, rows })
}
