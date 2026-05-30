use std::{fmt, time::Duration};

use serde::Serialize;
use serde_json::Value;

use crate::{models::Peer, state::AppState};

const MAX_TABLE_RESOURCE_BYTES: usize = 2 * 1024 * 1024;
const MAX_TABLE_ROWS: usize = 5_000;
const MAX_TABLE_COLUMNS: usize = 64;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TableDataResponse {
    pub schema: Vec<Value>,
    pub rows: Vec<Value>,
}

#[derive(Debug)]
pub enum TableDataError {
    NotFound(String),
    TooLarge(String),
    Unavailable(String),
}

impl fmt::Display for TableDataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TableDataError::NotFound(msg)
            | TableDataError::TooLarge(msg)
            | TableDataError::Unavailable(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for TableDataError {}

pub async fn fetch_table_data(
    state: &AppState,
    peer: &Peer,
    uri: &str,
) -> Result<TableDataResponse, TableDataError> {
    let endpoint = peer.mcp_url.trim_end_matches('/').to_string();
    tokio::time::timeout(
        Duration::from_secs(5),
        call_resources_read(state, peer, &endpoint, uri),
    )
    .await
    .map_err(|_| TableDataError::Unavailable("timeout".to_string()))?
}

async fn call_resources_read(
    state: &AppState,
    peer: &Peer,
    endpoint: &str,
    uri: &str,
) -> Result<TableDataResponse, TableDataError> {
    let response = crate::services::peer_tokens::send_mcp_request(
        &state.pool,
        &state.http,
        peer,
        endpoint,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "resources/read",
            "params": { "uri": uri }
        }),
    )
    .await
    .map_err(|err| TableDataError::Unavailable(format!("HTTP send failed: {err}")))?
    .error_for_status()
    .map_err(|err| TableDataError::Unavailable(format!("peer returned error status: {err}")))?;

    let body = response.bytes().await.map_err(|err| {
        TableDataError::Unavailable(format!("failed to read peer response: {err}"))
    })?;
    if body.len() > MAX_TABLE_RESOURCE_BYTES {
        return Err(TableDataError::TooLarge(
            "table resource response is larger than 2 MiB".to_string(),
        ));
    }

    let resp: Value = serde_json::from_slice(&body).map_err(|err| {
        TableDataError::Unavailable(format!("failed to parse peer response: {err}"))
    })?;
    if let Some(err) = resp.get("error").filter(|v| !v.is_null()) {
        let message = rpc_error_message(err);
        // Map on the JSON-RPC error CODE, not the message: MCP "Resource not found"
        // is -32002 → 404. Everything else (incl. -32601 "Method not found" = the peer
        // doesn't implement resources/read) is a peer failure → 502. Matching the word
        // "not found" in the message would mis-map -32601 to 404 and hide the real fault.
        if err.get("code").and_then(Value::as_i64) == Some(-32002) {
            return Err(TableDataError::NotFound(message));
        }
        return Err(TableDataError::Unavailable(format!(
            "peer MCP error: {message}"
        )));
    }

    let text = resp["result"]["contents"]
        .as_array()
        .and_then(|contents| contents.first())
        .and_then(|content| content.get("text"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            TableDataError::Unavailable(
                "resources/read response missing result.contents[0].text".to_string(),
            )
        })?;
    if text.len() > MAX_TABLE_RESOURCE_BYTES {
        return Err(TableDataError::TooLarge(
            "table resource body is larger than 2 MiB".to_string(),
        ));
    }

    let body: Value = serde_json::from_str(text).map_err(|err| {
        TableDataError::Unavailable(format!("invalid table resource JSON: {err}"))
    })?;
    let mut schema = body
        .get("schema")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| TableDataError::Unavailable("table resource missing schema".to_string()))?;
    let rows = body
        .get("rows")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| TableDataError::Unavailable("table resource missing rows".to_string()))?;

    if schema.len() > MAX_TABLE_COLUMNS {
        return Err(TableDataError::TooLarge(
            "table resource has too many columns".to_string(),
        ));
    }
    if rows.len() > MAX_TABLE_ROWS {
        return Err(TableDataError::TooLarge(
            "table resource has too many rows".to_string(),
        ));
    }
    for column in &mut schema {
        let Some(obj) = column.as_object_mut() else {
            return Err(TableDataError::Unavailable(
                "table schema columns must be objects".to_string(),
            ));
        };
        let Some(name) = obj.get("name").and_then(Value::as_str) else {
            return Err(TableDataError::Unavailable(
                "table schema columns must include name".to_string(),
            ));
        };
        if name.trim().is_empty() {
            return Err(TableDataError::Unavailable(
                "table schema column names must be non-empty".to_string(),
            ));
        }
        if !obj.contains_key("type") {
            obj.insert("type".to_string(), Value::String("string".to_string()));
        }
        let Some(column_type) = obj.get("type").and_then(Value::as_str) else {
            return Err(TableDataError::Unavailable(
                "table schema column type must be a string".to_string(),
            ));
        };
        if !matches!(column_type, "string" | "number" | "boolean" | "datetime") {
            return Err(TableDataError::Unavailable(format!(
                "unsupported table schema column type '{column_type}'"
            )));
        }
    }
    if !rows.iter().all(Value::is_object) {
        return Err(TableDataError::Unavailable(
            "table rows must be objects".to_string(),
        ));
    }

    Ok(TableDataResponse { schema, rows })
}

fn rpc_error_message(value: &Value) -> String {
    value
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| value.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| value.to_string())
}
