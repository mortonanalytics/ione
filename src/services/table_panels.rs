use std::{collections::HashSet, time::Duration};

use anyhow::Context;
use futures_util::future::join_all;
use serde::Serialize;
use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::{models::Peer, services::event_layers::table_property_columns};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TablePanelItem {
    pub id: String,
    pub name: String,
    pub source: String,
    pub stream_id: Option<Uuid>,
    pub peer_id: Option<Uuid>,
    pub peer_name: Option<String>,
    pub uri: Option<String>,
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
pub struct TablePanelsResponse {
    pub ione_tables: Vec<TablePanelItem>,
    pub peer_tables: Vec<TablePanelItem>,
    pub peer_errors: Vec<PeerFetchError>,
}

pub async fn fetch_table_panels(
    pool: &PgPool,
    http: &reqwest::Client,
    workspace_id: Uuid,
    org_id: Uuid,
    peers: Vec<Peer>,
) -> anyhow::Result<TablePanelsResponse> {
    let ione_tables = fetch_ione_tables(pool, workspace_id, org_id).await?;
    let (peer_tables, peer_errors) = fetch_peer_tables(http, peers).await;
    Ok(TablePanelsResponse {
        ione_tables,
        peer_tables,
        peer_errors,
    })
}

async fn fetch_ione_tables(
    pool: &PgPool,
    workspace_id: Uuid,
    org_id: Uuid,
) -> anyhow::Result<Vec<TablePanelItem>> {
    let rows = sqlx::query(
        "SELECT s.id AS stream_id, s.name AS stream_name, s.view_config
         FROM streams s
         JOIN connectors c ON c.id = s.connector_id
         JOIN workspaces w ON w.id = c.workspace_id
         WHERE c.workspace_id = $1
           AND w.org_id = $2
           AND s.view_config IS NOT NULL
         ORDER BY s.name",
    )
    .bind(workspace_id)
    .bind(org_id)
    .fetch_all(pool)
    .await
    .context("failed to fetch table stream catalog")?;

    let mut tables = Vec::new();
    for row in rows {
        let stream_id: Uuid = row.get("stream_id");
        let stream_name: String = row.get("stream_name");
        let view_config: Value = row.get("view_config");
        let Ok(columns) = table_property_columns(&view_config) else {
            continue;
        };
        if columns.is_empty() {
            continue;
        }
        tables.push(TablePanelItem {
            id: format!("ione:{stream_id}"),
            name: stream_name,
            source: "ione".to_string(),
            stream_id: Some(stream_id),
            peer_id: None,
            peer_name: None,
            uri: None,
        });
    }

    Ok(tables)
}

async fn fetch_peer_tables(
    http: &reqwest::Client,
    peers: Vec<Peer>,
) -> (Vec<TablePanelItem>, Vec<PeerFetchError>) {
    let outcomes = join_all(
        peers
            .into_iter()
            .map(|peer| fetch_tables_from_peer(http.clone(), peer)),
    )
    .await;
    let mut tables = Vec::new();
    let mut errors = Vec::new();
    let mut seen = HashSet::new();

    for outcome in outcomes {
        match outcome {
            Ok(items) => {
                for item in items {
                    if let (Some(peer_id), Some(uri)) = (item.peer_id, item.uri.clone()) {
                        if seen.insert((peer_id, uri)) {
                            tables.push(item);
                        }
                    }
                }
            }
            Err(err) => errors.push(err),
        }
    }

    (tables, errors)
}

async fn fetch_tables_from_peer(
    http: reqwest::Client,
    peer: Peer,
) -> Result<Vec<TablePanelItem>, PeerFetchError> {
    let token = resolve_token(&peer).map_err(|err| PeerFetchError {
        peer_id: peer.id,
        peer_name: peer.name.clone(),
        error: format!("token error: {err}"),
    })?;
    let endpoint = peer.mcp_url.trim_end_matches('/').to_string();
    let resources = match tokio::time::timeout(
        Duration::from_secs(5),
        call_resources_list(&http, &endpoint, &token),
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
        .filter_map(|resource| extract_table_panel(&peer, resource))
        .collect())
}

async fn call_resources_list(
    http: &reqwest::Client,
    endpoint: &str,
    token: &str,
) -> anyhow::Result<Vec<Value>> {
    let resp: Value = http
        .post(endpoint)
        .bearer_auth(token)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "resources/list",
            "params": null
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

    Ok(resp["result"]["resources"]
        .as_array()
        .cloned()
        .unwrap_or_default())
}

fn extract_table_panel(peer: &Peer, resource: Value) -> Option<TablePanelItem> {
    let meta = resource.get("metadata")?;
    if meta.get("ione_view")?.as_str()? != "table" {
        return None;
    }
    let uri = resource.get("uri")?.as_str()?.to_string();
    if uri.is_empty() {
        return None;
    }
    let name = resource
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("Peer table")
        .to_string();

    Some(TablePanelItem {
        id: format!("peer:{}:{uri}", peer.id),
        name,
        source: "peer".to_string(),
        stream_id: None,
        peer_id: Some(peer.id),
        peer_name: Some(peer.name.clone()),
        uri: Some(uri),
    })
}

fn resolve_token(peer: &Peer) -> anyhow::Result<String> {
    if let Some(ct) = &peer.access_token_ciphertext {
        return crate::util::token_crypto::decrypt_token(ct)
            .context("failed to decrypt peer token");
    }
    std::env::var("IONE_OAUTH_STATIC_BEARER")
        .context("peer has no token and IONE_OAUTH_STATIC_BEARER is not set")
}
