use std::{collections::HashSet, time::Duration};

use anyhow::Context;
use futures_util::future::join_all;
use reqwest::Url;
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use crate::{models::Peer, state::AppState, util::url_guard};

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
        crate::services::peer_panels::list_peer_resources(&state, &peer, &endpoint),
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

fn extract_map_layer(peer_id: Uuid, peer_name: &str, resource: Value) -> Option<MapLayerItem> {
    let meta = resource.get("metadata")?;
    if meta.get("ione_view")?.as_str()? != "map" {
        return None;
    }
    let uri = resource["uri"].as_str().unwrap_or("").to_string();
    let tile_url = meta.get("tile_url")?.as_str()?.to_string();
    if tile_url.is_empty() {
        return None;
    }
    // `tile_url` is handed to MapLibre verbatim — IONe never proxies tiles — so a
    // peer-supplied `javascript:` or plaintext-`http:` template would be loaded by
    // the operator's browser as-is. Validate it exactly as the document path
    // validates `download_url`, and drop just this layer (partial success) rather
    // than failing the whole workspace's fan-out.
    if let Err(err) = validate_layer_url(&tile_url, "map tile_url") {
        tracing::warn!(
            peer_id = %peer_id,
            peer_name = %peer_name,
            uri = %uri,
            error = %err,
            "dropping map layer with unsafe tile_url"
        );
        return None;
    }

    // `vector_url` is pass-through only — contract v1 §4.2 freezes it as an
    // accepted optional field that v1 does not render — so it is validated to the
    // same bar and stripped (not dropped with its layer) when it fails. Removing
    // the field outright would contradict the frozen contract; leaving it
    // unvalidated would hand a future consumer an unchecked peer-controlled URL.
    let vector_url = meta
        .get("vector_url")
        .and_then(|v| v.as_str())
        .and_then(|raw| match validate_layer_url(raw, "map vector_url") {
            Ok(()) => Some(raw.to_string()),
            Err(err) => {
                tracing::warn!(
                    peer_id = %peer_id,
                    peer_name = %peer_name,
                    uri = %uri,
                    error = %err,
                    "stripping unsafe vector_url from map layer"
                );
                None
            }
        });

    Some(MapLayerItem {
        peer_id,
        peer_name: peer_name.to_string(),
        uri,
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
            vector_url,
        },
    })
}

/// Same bar as `document_panels::validate_document_url`: https-only, then the
/// shared SSRF guard.
///
/// The raw string is what gets surfaced — an XYZ template's `{z}/{x}/{y}`
/// placeholders are percent-encoded by `Url::parse`, so the parsed form is used
/// for validation only and never round-tripped back into the response.
fn validate_layer_url(raw: &str, label: &str) -> anyhow::Result<()> {
    let url = Url::parse(raw).with_context(|| format!("invalid {label} '{raw}'"))?;
    if url.scheme() != "https" {
        anyhow::bail!("unsafe {label}: unsupported scheme '{}'", url.scheme());
    }
    url_guard::ensure_safe_url(&url, label)
}

#[cfg(test)]
mod tests {
    use super::{extract_map_layer, validate_layer_url};
    use serde_json::json;
    use uuid::Uuid;

    #[test]
    fn layer_urls_are_https_only_but_allow_on_prem_https() {
        for raw in [
            "https://tiles.example.com/{z}/{x}/{y}.png",
            "https://10.0.0.5/{z}/{x}/{y}.png",
        ] {
            assert!(
                validate_layer_url(raw, "map tile_url").is_ok(),
                "{raw} should be allowed"
            );
        }
        for raw in [
            "http://tiles.example.com/{z}/{x}/{y}.png",
            "http://localhost/{z}/{x}/{y}.png",
            "http://127.0.0.1/{z}/{x}/{y}.png",
            "http://10.0.0.5/{z}/{x}/{y}.png",
            "file:///tmp/{z}.png",
            "javascript:alert(1)",
            "data:text/html,<script>alert(1)</script>",
            "https://169.254.169.254/{z}/{x}/{y}.png",
            "not-a-url",
        ] {
            assert!(
                validate_layer_url(raw, "map tile_url").is_err(),
                "{raw} must be rejected"
            );
        }
    }

    #[test]
    fn unsafe_tile_url_drops_the_layer_and_unsafe_vector_url_only_strips_the_field() {
        let peer_id = Uuid::new_v4();
        let hostile = json!({
            "uri": "peer://layer/1",
            "name": "Hostile",
            "metadata": { "ione_view": "map", "tile_url": "javascript:alert(1)" }
        });
        assert!(extract_map_layer(peer_id, "peer", hostile).is_none());

        let mixed = json!({
            "uri": "peer://layer/2",
            "name": "Mixed",
            "metadata": {
                "ione_view": "map",
                "tile_url": "https://tiles.example.com/{z}/{x}/{y}.png",
                "vector_url": "javascript:alert(1)"
            }
        });
        let item = extract_map_layer(peer_id, "peer", mixed).expect("layer retained");
        assert_eq!(
            item.meta.tile_url,
            "https://tiles.example.com/{z}/{x}/{y}.png"
        );
        assert_eq!(item.meta.vector_url, None);
    }
}
