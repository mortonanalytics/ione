//! Shared `resources/list` discovery for the four peer panel paths.
//!
//! Map, chart, table and document panels each fan out to every bound peer with
//! the same `resources/list` call. Contract v1 §8.1 requires IONe to follow
//! `nextCursor` on *every* list path, and §8.2 records that the panel paths did
//! not. Rather than grow a fourth copy of the cursor loop, all four call this
//! helper, so cursor semantics — including the `nextCursor: null` termination
//! case — are interpreted in exactly one place for the panels.
//!
//! Nothing here persists a peer payload: the returned resource descriptors are
//! handed to the caller, rendered, and dropped.

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::{models::Peer, state::AppState};

/// Contract v1 §8.1: stop after 50 pages per list call and log a truncation
/// warning. Same cap as `federation::paginated_list` uses for manifest refresh.
const MAX_PAGINATION_PAGES: usize = 50;

/// The JSON-RPC id the panel paths put on `resources/list`.
///
/// Fixed rather than drawn from `peer_tokens::next_request_id()` because the
/// panel wire format is what peers (and the stub peers exercised in CI) already
/// answer with `"id": 1`. It is still passed to `read_jsonrpc_reply` as the
/// expected id, so a reply carrying a *different* id is rejected instead of
/// being mis-attributed to this call.
const PANEL_LIST_REQUEST_ID: u64 = 1;

/// List every `resources/list` entry a peer exposes, following `nextCursor`.
///
/// Accepts both a plain-JSON and an SSE-framed JSON-RPC reply, and correlates
/// the reply to the request id, via `peer_tokens::read_jsonrpc_reply`.
pub async fn list_peer_resources(
    state: &AppState,
    peer: &Peer,
    endpoint: &str,
) -> Result<Vec<Value>> {
    let request_id = Value::from(PANEL_LIST_REQUEST_ID);
    let mut cursor: Option<Value> = None;
    let mut resources = Vec::new();

    for page in 0..MAX_PAGINATION_PAGES {
        let params = cursor
            .as_ref()
            .map(|cursor| json!({ "cursor": cursor }))
            .unwrap_or(Value::Null);
        let response = crate::services::peer_tokens::send_mcp_request(
            &state.pool,
            &state.http,
            peer,
            endpoint,
            &json!({
                "jsonrpc": "2.0",
                "id": request_id,
                "method": "resources/list",
                "params": params
            }),
        )
        .await?
        .error_for_status()
        .context("peer returned error status")?;

        let message = crate::services::peer_tokens::read_jsonrpc_reply(response, &request_id)
            .await
            .context("failed to parse peer response")?;
        if let Some(err) = message.get("error").filter(|value| !value.is_null()) {
            anyhow::bail!("peer MCP error: {err}");
        }
        let result = message.get("result").cloned().unwrap_or(Value::Null);
        if let Some(items) = result.get("resources").and_then(Value::as_array) {
            resources.extend(items.iter().cloned());
        }

        cursor = next_cursor(&result);
        if cursor.is_none() {
            break;
        }
        if page + 1 == MAX_PAGINATION_PAGES {
            tracing::warn!(
                peer_id = %peer.id,
                peer_name = %peer.name,
                "panel resources/list hit page cap ({MAX_PAGINATION_PAGES}); truncating results"
            );
        }
    }

    Ok(resources)
}

/// The cursor for the next page, or `None` when the peer signalled the last one.
///
/// Mirrors `federation::next_cursor` (private to that module). `nextCursor` is
/// the spec spelling and `cursor` an accepted alias; both are terminal when
/// absent, JSON `null`, or an empty string. The null case is load-bearing:
/// `Value::get` returns `Some(Value::Null)` for an explicit `"nextCursor": null`,
/// so treating "key present" as "keep paging" re-requests the final page until
/// the page cap and duplicates every item on it.
///
/// `pub(crate)` so the peer-manifest review path (`routes/peers.rs`) follows the
/// same rule rather than growing a third copy of it.
pub(crate) fn next_cursor(result: &Value) -> Option<Value> {
    result
        .get("nextCursor")
        .or_else(|| result.get("cursor"))
        .filter(|value| !value.is_null())
        .filter(|value| value.as_str() != Some(""))
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::next_cursor;
    use serde_json::json;

    #[test]
    fn cursor_is_terminal_when_absent_null_or_empty() {
        for result in [
            json!({ "resources": [] }),
            json!({ "resources": [], "nextCursor": null }),
            json!({ "resources": [], "nextCursor": "" }),
            json!({ "resources": [], "cursor": null }),
        ] {
            assert_eq!(next_cursor(&result), None, "{result} must terminate paging");
        }
    }

    #[test]
    fn cursor_is_followed_verbatim_from_either_key() {
        assert_eq!(
            next_cursor(&json!({ "nextCursor": "page-2" })),
            Some(json!("page-2"))
        );
        assert_eq!(
            next_cursor(&json!({ "cursor": "page-2" })),
            Some(json!("page-2"))
        );
        // Opaque: a non-string cursor is passed back unchanged, not coerced.
        assert_eq!(
            next_cursor(&json!({ "nextCursor": { "offset": 100 } })),
            Some(json!({ "offset": 100 }))
        );
    }
}
