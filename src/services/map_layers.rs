use std::{collections::HashSet, time::Duration};

use anyhow::Context;
use futures_util::future::join_all;
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use crate::{models::Peer, state::AppState};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MapLayerMeta {
    pub tile_url: String,
    pub bounds: Option<Value>,
    pub attribution: Option<String>,
    pub layer_name: Option<String>,
    pub opacity: Option<f64>,
    pub vector_url: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MapLayerItem {
    pub peer_id: Uuid,
    pub peer_name: String,
    pub uri: String,
    pub name: String,
    pub meta: MapLayerMeta,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerFetchError {
    pub peer_id: Uuid,
    pub peer_name: String,
    pub error: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MapLayersResponse {
    pub items: Vec<MapLayerItem>,
    pub peers_ok: Vec<Uuid>,
    pub peers_failed: Vec<PeerFetchError>,
}

pub async fn fetch_map_layers(
    state: &AppState,
    peers: Vec<Peer>,
    filter_peer_id: Option<Uuid>,
) -> MapLayersResponse {
    let peers: Vec<Peer> = match filter_peer_id {
        Some(pid) => peers.into_iter().filter(|p| p.id == pid).collect(),
        None => peers,
    };

    let futures = peers
        .into_iter()
        .map(|peer| fetch_from_peer(state.clone(), peer));
    let outcomes = join_all(futures).await;

    let mut items = Vec::new();
    let mut peers_ok = Vec::new();
    let mut peers_failed = Vec::new();
    let mut seen: HashSet<(Uuid, String)> = HashSet::new();

    for outcome in outcomes {
        match outcome {
            Ok((peer_id, _peer_name, resources)) => {
                peers_ok.push(peer_id);
                for item in resources {
                    if seen.insert((item.peer_id, item.uri.clone())) {
                        items.push(item);
                    }
                }
            }
            Err((peer_id, peer_name, error)) => {
                peers_failed.push(PeerFetchError {
                    peer_id,
                    peer_name,
                    error,
                });
            }
        }
    }

    MapLayersResponse {
        items,
        peers_ok,
        peers_failed,
    }
}

type PeerResult = Result<(Uuid, String, Vec<MapLayerItem>), (Uuid, String, String)>;

async fn fetch_from_peer(state: AppState, peer: Peer) -> PeerResult {
    let endpoint = peer.mcp_url.trim_end_matches('/').to_string();

    let result = tokio::time::timeout(
        Duration::from_secs(5),
        call_resources_list(&state, &peer, &endpoint),
    )
    .await;

    let resources_json = match result {
        Err(_) => return Err((peer.id, peer.name.clone(), "timeout".to_string())),
        Ok(Err(e)) => return Err((peer.id, peer.name.clone(), e.to_string())),
        Ok(Ok(v)) => v,
    };

    let items = resources_json
        .into_iter()
        .filter_map(|r| extract_map_layer(peer.id, &peer.name, r))
        .collect();

    Ok((peer.id, peer.name.clone(), items))
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
        &serde_json::json!({
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

fn extract_map_layer(peer_id: Uuid, peer_name: &str, resource: Value) -> Option<MapLayerItem> {
    let meta = resource.get("metadata")?;
    if meta.get("ione_view")?.as_str()? != "map" {
        return None;
    }
    let tile_url = meta.get("tile_url")?.as_str()?.to_string();
    if tile_url.is_empty() {
        return None;
    }

    Some(MapLayerItem {
        peer_id,
        peer_name: peer_name.to_string(),
        uri: resource["uri"].as_str().unwrap_or("").to_string(),
        name: resource["name"].as_str().unwrap_or("").to_string(),
        meta: MapLayerMeta {
            tile_url,
            bounds: meta.get("bounds").cloned(),
            attribution: meta
                .get("attribution")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            layer_name: meta
                .get("layer_name")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            opacity: meta.get("opacity").and_then(|v| v.as_f64()),
            vector_url: meta
                .get("vector_url")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        },
    })
}
