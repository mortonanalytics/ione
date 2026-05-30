use std::{collections::HashSet, time::Duration};

use anyhow::Context;
use futures_util::future::join_all;
use reqwest::Url;
use serde::Serialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{models::Peer, state::AppState, util::url_guard};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentPanelItem {
    pub id: String,
    pub name: String,
    pub source: String,
    pub peer_id: Uuid,
    pub peer_name: String,
    pub uri: String,
    pub download_url: String,
    pub mime_type: String,
    pub file_size_bytes: Option<i64>,
    pub last_modified: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerFetchError {
    pub peer_id: Uuid,
    pub peer_name: String,
    pub error: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentPanelsResponse {
    pub peer_documents: Vec<DocumentPanelItem>,
    pub peer_errors: Vec<PeerFetchError>,
}

pub async fn fetch_document_panels(state: &AppState, peers: Vec<Peer>) -> DocumentPanelsResponse {
    let outcomes = join_all(
        peers
            .into_iter()
            .map(|peer| fetch_documents_from_peer(state.clone(), peer)),
    )
    .await;
    let mut documents = Vec::new();
    let mut errors = Vec::new();
    let mut seen = HashSet::new();

    for outcome in outcomes {
        match outcome {
            Ok(items) => {
                for item in items {
                    if seen.insert((item.peer_id, item.uri.clone())) {
                        documents.push(item);
                    }
                }
            }
            Err(err) => errors.push(err),
        }
    }

    DocumentPanelsResponse {
        peer_documents: documents,
        peer_errors: errors,
    }
}

async fn fetch_documents_from_peer(
    state: AppState,
    peer: Peer,
) -> Result<Vec<DocumentPanelItem>, PeerFetchError> {
    let endpoint = peer.mcp_url.trim_end_matches('/').to_string();
    let resources = match tokio::time::timeout(
        Duration::from_secs(5),
        call_resources_list(&state, &peer, &endpoint),
    )
    .await
    {
        Err(_) => {
            return Err(PeerFetchError {
                peer_id: peer.id,
                peer_name: peer.name,
                error: "timeout".to_string(),
            })
        }
        Ok(Err(err)) => {
            return Err(PeerFetchError {
                peer_id: peer.id,
                peer_name: peer.name,
                error: err.to_string(),
            })
        }
        Ok(Ok(resources)) => resources,
    };

    Ok(resources
        .into_iter()
        .filter_map(|resource| extract_document_panel(&peer, resource))
        .collect())
}

async fn call_resources_list(
    state: &AppState,
    peer: &Peer,
    endpoint: &str,
) -> anyhow::Result<Vec<Value>> {
    let resp: Value = crate::services::peer_tokens::send_mcp_request(
        &state.pool,
        &state.http,
        peer,
        endpoint,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "resources/list",
            "params": null
        }),
    )
    .await?
    .error_for_status()
    .context("peer returned error status")?
    .json()
    .await
    .context("failed to parse peer response")?;

    if let Some(err) = resp.get("error").filter(|v| !v.is_null()) {
        anyhow::bail!("peer MCP error: {err}");
    }

    Ok(resp["result"]["resources"]
        .as_array()
        .cloned()
        .unwrap_or_default())
}

fn extract_document_panel(peer: &Peer, resource: Value) -> Option<DocumentPanelItem> {
    let meta = resource.get("metadata")?;
    if meta.get("ione_view")?.as_str()? != "document" {
        return None;
    }

    let uri = resource.get("uri")?.as_str()?.to_string();
    if uri.is_empty() {
        return None;
    }
    let download_url = meta.get("download_url")?.as_str()?.to_string();
    if let Err(err) = validate_document_url(&download_url) {
        tracing::warn!(
            peer_id = %peer.id,
            peer_name = %peer.name,
            uri = %uri,
            error = %err,
            "dropping document panel with unsafe download_url"
        );
        return None;
    }

    let mime_type = match meta
        .get("mime_type")
        .or_else(|| meta.get("mimeType"))
        .or_else(|| resource.get("mimeType"))
        .and_then(Value::as_str)
    {
        Some(v) => v.to_string(),
        None => {
            tracing::warn!(
                peer_id = %peer.id,
                peer_name = %peer.name,
                uri = %uri,
                "dropping document panel resource: missing mime_type"
            );
            return None;
        }
    };
    let name = resource
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("Peer document")
        .to_string();

    Some(DocumentPanelItem {
        id: format!("peer:{}:{uri}", peer.id),
        name,
        source: "peer".to_string(),
        peer_id: peer.id,
        peer_name: peer.name.clone(),
        uri,
        download_url,
        mime_type,
        file_size_bytes: meta.get("file_size_bytes").and_then(Value::as_i64),
        last_modified: meta
            .get("last_modified")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

fn validate_document_url(raw: &str) -> anyhow::Result<()> {
    let url = Url::parse(raw).with_context(|| format!("invalid document URL '{raw}'"))?;
    if url.scheme() != "https" {
        anyhow::bail!("unsafe document URL: unsupported scheme '{}'", url.scheme());
    }
    url_guard::ensure_safe_url(&url, "document download_url")
}

#[cfg(test)]
mod tests {
    use super::validate_document_url;

    #[test]
    fn document_urls_are_https_only_but_allow_on_prem_https() {
        for raw in ["https://example.com/doc.pdf", "https://10.0.0.5/doc.pdf"] {
            assert!(
                validate_document_url(raw).is_ok(),
                "{raw} should be allowed"
            );
        }
        for raw in [
            "http://example.com/doc.pdf",
            "http://localhost/doc.pdf",
            "http://127.0.0.1/doc.pdf",
            "http://10.0.0.5/doc.pdf",
            "file:///tmp/doc.pdf",
            "javascript:alert(1)",
            "https://169.254.169.254/doc.pdf",
        ] {
            assert!(
                validate_document_url(raw).is_err(),
                "{raw} must be rejected"
            );
        }
    }
}
