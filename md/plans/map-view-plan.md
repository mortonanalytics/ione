# Map View — Implementation Plan

**Design doc:** [md/design/map-view.md](../design/map-view.md)
**Shape:** Medium — 2 layers (api, ui), 3 phases, ~10 files
**Stack:** Rust/Axum API + vanilla JS/CSS frontend (no bundler)
**OQ-5 resolved:** `futures-util = "0.3"` already in `Cargo.toml`; use `futures_util::future::join_all`

---

## Dependencies

None new. All required crates are already present:
- `futures-util = "0.3"` — `join_all` for concurrent peer fan-out
- `reqwest = "0.12"` — HTTP calls to peer MCP endpoints
- `wiremock = "0.6"` (dev) — mock peer MCP server in integration tests
- `serde_json` — resource metadata extraction

---

## Phases

### Phase 1 — Fan-out endpoint

**Goal:** `GET /api/v1/workspaces/:id/map-layers` returns aggregated `ione_view: "map"` resources from all active peers, with partial-success semantics and 5-second per-peer timeout.

**Files:**
- `src/repos/workspace_peer_binding_repo.rs` — add `list_active_peers_for_workspace`
- `src/services/map_layers.rs` — new; fan-out logic
- `src/services/mod.rs` — add `pub mod map_layers`
- `src/routes/map_layers.rs` — new; handler + request/response types
- `src/routes/mod.rs` — add `pub mod map_layers` + route registration
- `tests/phase_map_layers.rs` — new; integration tests AC-1 through AC-5, AC-13, AC-15

**Code shapes:**

`src/repos/workspace_peer_binding_repo.rs` — append after `list_by_peer`:
```rust
/// Returns all active Peer records that have an active binding to `workspace_id`
/// within `org_id`. Used by the map-layer fan-out service.
pub async fn list_active_peers_for_workspace(
    &self,
    workspace_id: Uuid,
    org_id: Uuid,
) -> anyhow::Result<Vec<crate::models::Peer>> {
    sqlx::query_as::<_, crate::models::Peer>(
        "SELECT p.id, p.org_id, p.name, p.mcp_url, p.issuer_id, p.sharing_policy,
                p.status, p.created_at, p.oauth_client_id,
                p.access_token_hash, p.refresh_token_hash,
                p.access_token_ciphertext, p.token_expires_at, p.tool_allowlist
         FROM workspace_peer_bindings b
         JOIN peers p ON p.id = b.peer_id
         WHERE b.workspace_id = $1
           AND b.status = 'active'
           AND p.status = 'active'
           AND EXISTS (
               SELECT 1 FROM workspaces w WHERE w.id = b.workspace_id AND w.org_id = $2
           )
         ORDER BY b.created_at DESC",
    )
    .bind(workspace_id)
    .bind(org_id)
    .fetch_all(&self.pool)
    .await
    .context("failed to list active peers for workspace")
}
```

`src/services/map_layers.rs` — new file:
```rust
use std::time::Duration;
use futures_util::future::join_all;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;
use crate::models::Peer;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MapLayerMeta {
    pub tile_url: String,
    pub bounds: Option<Value>,           // [west, south, east, north]
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
    http: &reqwest::Client,
    peers: Vec<Peer>,
    filter_peer_id: Option<Uuid>,
) -> MapLayersResponse {
    let peers = match filter_peer_id {
        Some(pid) => peers.into_iter().filter(|p| p.id == pid).collect(),
        None => peers,
    };

    let futures = peers.into_iter().map(|peer| fetch_from_peer(http.clone(), peer));
    let outcomes = join_all(futures).await;

    let mut items: Vec<MapLayerItem> = Vec::new();
    let mut peers_ok = Vec::new();
    let mut peers_failed = Vec::new();
    let mut seen: std::collections::HashSet<(Uuid, String)> = std::collections::HashSet::new();

    for outcome in outcomes {
        match outcome {
            Ok((peer_id, peer_name, resources)) => {
                peers_ok.push(peer_id);
                for item in resources {
                    if seen.insert((item.peer_id, item.uri.clone())) {
                        items.push(item);
                    }
                }
            }
            Err((peer_id, peer_name, error)) => {
                peers_failed.push(PeerFetchError { peer_id, peer_name, error });
            }
        }
    }

    MapLayersResponse { items, peers_ok, peers_failed }
}

type PeerResult = Result<(Uuid, String, Vec<MapLayerItem>), (Uuid, String, String)>;

async fn fetch_from_peer(http: reqwest::Client, peer: Peer) -> PeerResult {
    let token = resolve_token(&peer).map_err(|e| {
        (peer.id, peer.name.clone(), format!("token error: {e}"))
    })?;

    // `peer.mcp_url` is the canonical MCP endpoint — POST directly, do NOT append `/mcp`.
    // This matches `mcp_client.rs` (posts to `self.mcp_url`) and
    // `workspace_peer_binding::fetch_whoami` (posts to `mcp_url.trim_end_matches('/')`).
    // NOTE: `routes/peers.rs:409` is the lone outlier that appends `/mcp`; that is a
    // pre-existing inconsistency, out of scope for this feature — do not "fix" it here.
    let endpoint = peer.mcp_url.trim_end_matches('/').to_string();

    let result = tokio::time::timeout(
        Duration::from_secs(5),
        call_resources_list(&http, &endpoint, &token),
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
    http: &reqwest::Client,
    endpoint: &str,
    token: &str,
) -> anyhow::Result<Vec<Value>> {
    let resp: Value = http
        .post(endpoint)
        .bearer_auth(token)
        .json(&serde_json::json!({
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
            attribution: meta.get("attribution").and_then(|v| v.as_str()).map(str::to_string),
            layer_name: meta.get("layer_name").and_then(|v| v.as_str()).map(str::to_string),
            opacity: meta.get("opacity").and_then(|v| v.as_f64()),
            vector_url: meta.get("vector_url").and_then(|v| v.as_str()).map(str::to_string),
        },
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
```

**Token lifecycle (v0.1 scope — explicit):** `resolve_token` uses the *stored* access token as-is. It does **not** refresh. This is a deliberate v0.1 limitation, not an oversight: `peer_oauth.rs:187` stores only a SHA-256 *hash* of the refresh token (`refresh_token_hash`), not recoverable ciphertext — so refresh is impossible without a schema change (a separate hardening slice). When a peer's access token is expired, the peer returns HTTP 401, `call_resources_list` fails via `error_for_status()`, and that peer lands in `peers_failed` with a clear error string. The UI surfaces this as a per-peer failure (see Phase 2 all-failed state). **AC-15 below tests the expired-token → `peers_failed` path.** Peer token refresh is tracked as a prerequisite hardening slice — out of scope here.

`src/routes/map_layers.rs` — new file:
```rust
use axum::{
    extract::{Path, Query, State},
    response::Json,
    Extension,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    auth::{ensure_workspace_in_org, AuthContext},
    error::AppError,
    repos::WorkspacePeerBindingRepo,
    services::map_layers::{fetch_map_layers, MapLayersResponse},
    state::AppState,
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MapLayersQuery {
    pub peer_id: Option<Uuid>,
}

pub async fn list_map_layers(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(workspace_id): Path<Uuid>,
    Query(query): Query<MapLayersQuery>,
) -> Result<Json<MapLayersResponse>, AppError> {
    ensure_workspace_in_org(&state.pool, workspace_id, ctx.org_id).await?;

    let peers = WorkspacePeerBindingRepo::new(state.pool.clone())
        .list_active_peers_for_workspace(workspace_id, ctx.org_id)
        .await
        .map_err(AppError::Internal)?;

    let response = fetch_map_layers(&state.http, peers, query.peer_id).await;
    Ok(Json(response))
}
```

`src/routes/mod.rs` — two additions:
```rust
// After existing pub mod declarations (near line 40):
pub mod map_layers;

// After workspaces route block in router() (near line 99):
.route(
    "/api/v1/workspaces/:id/map-layers",
    get(map_layers::list_map_layers),
)
```

`src/services/mod.rs` — add:
```rust
pub mod map_layers;
```

`tests/phase_map_layers.rs` — new file, test module header + AC-1 through AC-5, AC-13, AC-15 using `wiremock::MockServer`. Pattern mirrors `phase14_bindings.rs`: `spawn_app()`, insert org/workspace/peer fixtures, mount mock, assert JSON response. All tests are `#[ignore]`-gated.

Additional AC for this phase:

**AC-15 — Expired/unauthorized peer token → peers_failed**
Given a workspace with one active peer binding whose mock MCP server responds to `resources/list` with HTTP 401,
when `GET /api/v1/workspaces/:id/map-layers` is called,
then the response has HTTP 200, `items` is empty, `peers_ok` is empty, and `peers_failed` contains one entry for that peer with a non-empty `error`.

**Gate:**
```
cargo check && cargo clippy --all-targets -- -D warnings && \
IONE_OAUTH_STATIC_BEARER=test-bearer DATABASE_URL=postgres://ione:ione@localhost:5433/ione \
  cargo test --test phase_map_layers -- --ignored --test-threads=1
```

**Acceptance:** AC-1: `items` length 1 and `items[0].meta.tileUrl` matches the mock resource; AC-2: org B token → 404; AC-5: no bindings → `items: []`.

---

### Phase 2 — Map panel UI

**Goal:** A Map tab in the workspace shell renders a MapLibre canvas seeded from `GET /api/v1/workspaces/:id/map-layers`, with layer control overlay, empty/loading/error states, and keyboard navigation.

**Files:**
- `static/index.html` — add MapLibre CDN `<script>`+`<link>`, `tab-map` button, `panel-map` div
- `static/app.js` — `initMapPanel()`, `updateMapLayers(workspaceId)`, `renderLayerControl(items)`, keyboard handler on map canvas, tab activation hook
- `static/style.css` — `#panel-map`, `.map-container`, `.layer-control`, `.layer-row`, `.layer-row--error`, empty/loading state styles

**Code shapes:**

`static/index.html` — in `<head>`:
```html
<link rel="stylesheet" href="https://unpkg.com/maplibre-gl@4/dist/maplibre-gl.css" />
<script src="https://unpkg.com/maplibre-gl@4/dist/maplibre-gl.js"></script>
```

In tab bar, after `tab-chat` button:
```html
<button id="tab-map" role="tab" aria-selected="false"
        aria-controls="panel-map" tabindex="-1">Map</button>
```

New panel div (after `panel-chat`):
```html
<div id="panel-map" role="tabpanel" aria-labelledby="tab-map"
     aria-label="Map view" hidden>
  <div id="map-loading" class="map-state-overlay" aria-live="polite" hidden>
    <div class="map-skeleton"></div>
    <span>Loading map layers…</span>
  </div>
  <div id="map-empty" class="map-state-overlay" hidden>
    <p>No map layers available in this workspace.</p>
    <button id="map-connect-peer">Connect a peer</button>
  </div>
  <div id="map-error" class="map-state-overlay" role="status" aria-live="polite" hidden>
    <p id="map-error-msg"></p>
    <button id="map-error-retry">Retry</button>
  </div>
  <div id="map-canvas-container" hidden>
    <div id="map-canvas" role="application" tabindex="0"
         aria-label="Interactive map"></div>
    <div id="map-layer-control" role="region" aria-label="Map layers">
      <button id="map-layer-toggle" aria-expanded="true"
              aria-controls="map-layer-list">Layers</button>
      <ul id="map-layer-list" role="list"></ul>
    </div>
  </div>
</div>
```

`static/app.js` key additions:

```javascript
// Module-level state
let mapInstance = null;
let mapLayerItems = [];

function initMapPanel() {
  if (typeof maplibregl === 'undefined') {
    showMapError('Map view requires MapLibre GL JS. Check your network connection.');
    return;
  }
  // MapLibre instance created lazily in updateMapLayers()
  document.getElementById('map-connect-peer').addEventListener('click', () => switchTab('connectors'));
  document.getElementById('map-error-retry').addEventListener('click', () => {
    const wsId = currentWorkspaceId(); // existing function
    if (wsId) updateMapLayers(wsId);
  });
  setupMapKeyboard();
}

async function updateMapLayers(workspaceId) {
  showMapState('loading');
  try {
    // apiFetch (app.js:81) already parses and RETURNS the JSON body — NOT a Response.
    // Do not call .json() on the result.
    const data = await apiFetch(`/api/v1/workspaces/${workspaceId}/map-layers`);
    mapLayerItems = data.items || [];
    const failed = data.peersFailed || [];

    if (mapLayerItems.length === 0) {
      // Distinguish "no layers" from "every peer failed". If bindings existed and all
      // failed, peers_ok is empty AND peers_failed is non-empty → show failure, not empty.
      if (failed.length > 0 && (data.peersOk || []).length === 0) {
        showMapError(`Couldn't reach any connected peer (${failed.length} failed). The data source may be temporarily unavailable.`);
      } else {
        showMapState('empty');
      }
      rehydrateTranscriptChips();  // Phase 3: re-scan chat for now-unknown URIs
      return;
    }

    showMapState('canvas');
    renderMapLayers(mapLayerItems);
    renderLayerControl(mapLayerItems);
    if (failed.length > 0) {
      markFailedPeers(failed);  // partial failure — some layers present
    }
    rehydrateTranscriptChips();  // Phase 3: backfill chips in already-rendered messages
  } catch (err) {
    showMapError('Couldn\'t load map layers. ' + err.message);
  }
}

function renderMapLayers(items) {
  const reduceMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches;

  // Attribution is PEER-CONTROLLED. MapLibre's customAttribution renders as HTML, so
  // escape every attribution string to text before passing it (AC-16). escapeHtml
  // converts < > & " ' to entities — peer markup renders as literal characters, never DOM.
  const attributions = items
    .map(i => i.meta.attribution)
    .filter(Boolean)
    .map(escapeHtml);

  if (!mapInstance) {
    mapInstance = new maplibregl.Map({
      container: 'map-canvas',
      style: { version: 8, sources: {}, layers: [] },
      keyboard: true,
      fadeDuration: reduceMotion ? 0 : 300,
      attributionControl: { customAttribution: attributions },
    });
    mapInstance.on('load', () => addLayersToMap(items, reduceMotion));
  } else {
    clearMapLayers();
    addLayersToMap(items, reduceMotion);
  }
}

function addLayersToMap(items, reduceMotion) {
  const bounds = new maplibregl.LngLatBounds();
  let hasBounds = false;

  items.forEach((item) => {
    const sourceId = `src-${item.peerId}-${btoa(item.uri).replace(/[^a-z0-9]/gi, '')}`;
    const layerId = `lyr-${sourceId}`;

    mapInstance.addSource(sourceId, { type: 'raster', tiles: [item.meta.tileUrl], tileSize: 256 });
    mapInstance.addLayer({
      id: layerId,
      type: 'raster',
      source: sourceId,
      paint: { 'raster-opacity': item.meta.opacity ?? 1.0 },
    });
    // Attribution handled via customAttribution in the Map constructor (escaped). Do NOT
    // mutate MapLibre internals (_controls / _attribHTML) — that is an XSS vector.

    if (Array.isArray(item.meta.bounds) && item.meta.bounds.length === 4) {
      const [w, s, e, n] = item.meta.bounds;
      bounds.extend([w, s]);
      bounds.extend([e, n]);
      hasBounds = true;
    }
  });

  if (hasBounds && !bounds.isEmpty()) {
    mapInstance.fitBounds(bounds, { padding: 20, animate: !reduceMotion });
  }
}

function renderLayerControl(items) {
  const list = document.getElementById('map-layer-list');
  list.innerHTML = '';
  items.forEach((item) => {
    const layerId = `lyr-src-${item.peerId}-${btoa(item.uri).replace(/[^a-z0-9]/gi, '')}`;
    const label = item.meta.layerName || item.name;
    const li = document.createElement('li');
    li.className = 'layer-row';
    li.dataset.uri = item.uri;
    li.innerHTML = `
      <label>
        <input type="checkbox" checked data-layer-id="${layerId}" />
        ${escapeHtml(label)}
      </label>
      ${item.meta.opacity != null ? `<input type="range" min="0" max="1" step="0.05"
        value="${item.meta.opacity}" data-layer-id="${layerId}"
        aria-label="Opacity for ${escapeHtml(label)}" />` : ''}
      <span class="layer-error-icon" hidden aria-label="Tiles unavailable"></span>
    `;
    li.querySelector('input[type=checkbox]').addEventListener('change', (e) => {
      if (mapInstance) {
        mapInstance.setLayoutProperty(layerId, 'visibility', e.target.checked ? 'visible' : 'none');
      }
    });
    const slider = li.querySelector('input[type=range]');
    if (slider) {
      slider.addEventListener('input', (e) => {
        if (mapInstance) mapInstance.setPaintProperty(layerId, 'raster-opacity', Number(e.target.value));
      });
    }
    list.appendChild(li);
  });
}

function setupMapKeyboard() {
  const canvas = document.getElementById('map-canvas');
  canvas.addEventListener('keydown', (e) => {
    if (!mapInstance) return;
    const PAN = 100;
    if (e.key === 'ArrowRight') mapInstance.panBy([PAN, 0]);
    else if (e.key === 'ArrowLeft') mapInstance.panBy([-PAN, 0]);
    else if (e.key === 'ArrowUp') mapInstance.panBy([0, -PAN]);
    else if (e.key === 'ArrowDown') mapInstance.panBy([0, PAN]);
    else if (e.key === '+') mapInstance.zoomIn();
    else if (e.key === '-') mapInstance.zoomOut();
    else if (e.key === 'Escape') document.getElementById('map-layer-toggle').focus();
    else return;
    e.preventDefault();
  });
}

function showMapState(state) { /* 'loading' | 'empty' | 'canvas' | 'error' */ }
function showMapError(msg) { /* sets #map-error-msg, shows error overlay */ }
function clearMapLayers() { /* removes all sources/layers added by addLayersToMap */ }
function markFailedPeers(failed) { /* shows error icon on layer rows from failed peers */ }
```

Hook `updateMapLayers` into the existing `switchTab` function: when `switchTab('map')` is called and the map panel has never loaded for the current workspace, call `updateMapLayers(currentWorkspaceId())`. Also call on workspace switch if map tab is active.

`static/style.css` additions:
```css
#panel-map { position: relative; height: 100%; overflow: hidden; }
#map-canvas-container { width: 100%; height: 100%; }
#map-canvas { width: 100%; height: 100%; }
#map-canvas:focus-visible { outline: 3px solid var(--color-primary); outline-offset: -3px; }
.map-state-overlay { display: flex; flex-direction: column; align-items: center;
                     justify-content: center; height: 100%; gap: 1rem; }
.map-skeleton { width: 100%; height: 100%; background: var(--color-bg); }
#map-layer-control { position: absolute; top: 10px; right: 10px; z-index: 10;
                     background: rgba(255,255,255,0.92); border-radius: 4px;
                     padding: 0.5rem; min-width: 180px; }
.layer-row { display: flex; align-items: center; gap: 0.5rem; padding: 0.25rem 0;
             min-height: 44px; }
.layer-row--highlight { background: var(--color-primary-light, #e8f0fa);
                        transition: background 0.2s; }
.layer-error-icon::before { content: "⚠"; color: var(--color-error); }
@media (prefers-reduced-motion: reduce) {
  .layer-row--highlight { transition: none; }
}
```

**Gate:**
```
# 1. No peer-specific strings in the render path (AC-9)
! grep -ri "groundpulse\|terrayout\|bearinglinedash\|bearingline" static/app.js
# 2. Playwright smoke against a stub peer (AC-6, 7, 8, 17)
npx playwright test tests/e2e/map-panel.spec.ts
```

The Playwright smoke (`tests/e2e/map-panel.spec.ts`, new file) boots the app with a seeded stub-peer workspace and asserts mechanically — not visually:
- AC-6: clicking `#tab-map` reveals `#map-canvas`; `.maplibregl-canvas` exists with non-zero `boundingBox()`; one `.layer-row` renders with text "World tiles".
- AC-7: a workspace with no map layers shows `#map-empty` visible, `#map-canvas-container` hidden.
- AC-8: unchecking the `.layer-row input[type=checkbox]` results in the layer's `visibility` layout property becoming `none` (assert via `page.evaluate(() => mapInstance.getLayoutProperty(id,'visibility'))`).
- AC-17: a workspace whose only peer returns 401 shows `#map-error` (not `#map-empty`).

**Acceptance:** Playwright smoke passes (AC-6, 7, 8, 17); `grep -ri` finds no peer strings (AC-9).

**AC-16 — Attribution is escaped (XSS)**
Given a stub peer returns a map resource with `metadata.attribution = "<img src=x onerror=alert(1)>"`,
when the Map tab renders, then the attribution control's DOM contains the literal escaped text and no `<img>` element is created (assert `document.querySelector('.maplibregl-ctrl-attrib img')` is null).

**AC-17 — All-peer-failure state is distinct from empty**
Given a workspace with active peer bindings where every peer returns 401/times out,
when the Map tab activates, then `#map-error` is visible and `#map-empty` is hidden (the user sees a failure message, not "No map layers available").

---

### Phase 3 — Chat → Map navigation

**Goal:** Resource URI chips in the chat transcript have a "View in Map" button; clicking it switches to the Map tab and highlights the corresponding layer row for 1500 ms.

**Files:**
- `static/app.js` — `appendMessage` stores raw text on the element; `injectResourceChips` URI detection + chip insertion; `rehydrateTranscriptChips` backfill pass; `highlightMapLayer(uri)`
- `static/style.css` — `.resource-chip`, `.chip-view-map` styles
- `tests/e2e/map-panel.spec.ts` — extend with AC-14 (chip click → highlight)

**Ordering constraint (finding #6):** Chips can only be injected for URIs present in `mapLayerItems`. The transcript may render *before* the operator opens the Map tab (so `mapLayerItems` is empty at message-render time). Two passes are therefore required:
1. **At message render** (`appendMessage`) — inject chips for any URIs already known.
2. **After layers load** (`updateMapLayers` calls `rehydrateTranscriptChips`) — re-scan all existing messages and backfill chips for URIs that became known. To re-scan safely, `appendMessage` stores the raw message text in `div.dataset.rawText` and chip injection always works from that source (never from already-chipped DOM).

**Code shapes:**

`static/app.js` additions:

```javascript
// Modify the existing appendMessage (app.js:334) to store raw text + run chip injection:
function appendMessage(role, text) {
  const div = document.createElement('div');
  div.className = 'message ' + role;
  const prefix = (role === 'user' ? 'You: ' : 'Model: ');
  div.dataset.rawText = prefix + text;   // source of truth for (re)hydration
  div.textContent = div.dataset.rawText;
  transcript.appendChild(div);
  injectResourceChips(div);              // pass 1 — inject for already-known URIs
  transcript.scrollTop = transcript.scrollHeight;
}

// Re-scan every transcript message after map layers load (pass 2 — backfill).
function rehydrateTranscriptChips() {
  transcript.querySelectorAll('.message[data-raw-text]').forEach((el) => {
    el.textContent = el.dataset.rawText;  // reset to plain text, then re-inject
    injectResourceChips(el);
  });
}

// URI pattern: any scheme://path that matches a loaded map layer URI
const RESOURCE_URI_RE = /\b([a-z][a-z0-9+\-.]*:\/\/[^\s"<>]+)/gi;

function injectResourceChips(messageEl) {
  // Walk text nodes in the message, replace matching URIs with chip HTML
  // Only injects chips for URIs present in mapLayerItems
  const walker = document.createTreeWalker(messageEl, NodeFilter.SHOW_TEXT);
  const nodes = [];
  while (walker.nextNode()) nodes.push(walker.currentNode);

  nodes.forEach((textNode) => {
    const text = textNode.textContent;
    if (!RESOURCE_URI_RE.test(text)) return;
    RESOURCE_URI_RE.lastIndex = 0;

    const frag = document.createDocumentFragment();
    let last = 0, match;
    while ((match = RESOURCE_URI_RE.exec(text)) !== null) {
      const uri = match[1];
      const item = mapLayerItems.find(i => i.uri === uri);
      if (!item) continue;

      frag.appendChild(document.createTextNode(text.slice(last, match.index)));
      const chip = document.createElement('button');
      chip.className = 'resource-chip';
      chip.dataset.uri = uri;
      chip.textContent = item.meta.layerName || item.name;
      const viewBtn = document.createElement('button');
      viewBtn.className = 'chip-view-map';
      viewBtn.dataset.uri = uri;
      viewBtn.setAttribute('aria-label', 'View in Map');
      viewBtn.textContent = '🗺';
      viewBtn.addEventListener('click', () => highlightMapLayer(uri));
      frag.appendChild(chip);
      frag.appendChild(viewBtn);
      last = match.index + match[0].length;
    }
    frag.appendChild(document.createTextNode(text.slice(last)));
    textNode.parentNode.replaceChild(frag, textNode);
  });
}

function highlightMapLayer(uri) {
  switchTab('map');
  const row = document.querySelector(`#map-layer-list .layer-row[data-uri="${CSS.escape(uri)}"]`);
  if (!row) return;
  row.scrollIntoView({ block: 'nearest' });
  row.classList.add('layer-row--highlight');
  setTimeout(() => row.classList.remove('layer-row--highlight'), 1500);
}
```

Call `injectResourceChips(messageEl)` from the existing transcript message rendering function in `app.js` after inserting each assistant message element. The `mapLayerItems` array (Phase 2) is module-level state already available.

`static/style.css` additions:
```css
.resource-chip { display: inline-flex; align-items: center; gap: 0.25rem;
                 padding: 0.1rem 0.4rem; border-radius: 3px;
                 background: var(--color-bg); border: 1px solid var(--color-border);
                 font-size: 0.85em; cursor: default; }
.chip-view-map { padding: 0.1rem 0.3rem; border: none; background: none;
                 cursor: pointer; min-width: 44px; min-height: 44px;
                 display: inline-flex; align-items: center; justify-content: center; }
.chip-view-map:hover { background: var(--color-bg); }
```

**Gate:**
```
grep -n "injectResourceChips\|rehydrateTranscriptChips\|highlightMapLayer\|chip-view-map\|layer-row--highlight" static/app.js && \
npx playwright test tests/e2e/map-panel.spec.ts -g "chip"
```

**Acceptance:** AC-14 (Playwright): with a stub map layer loaded, a transcript message containing the layer URI renders a `.resource-chip` + `.chip-view-map`; clicking `.chip-view-map[data-uri="stub://layer/1"]` activates the Map tab and adds `layer-row--highlight` to the matching row, removed after 1500 ms. Also verify the rehydration path: render the message *before* opening Map, then open Map, and assert the chip appears (finding #6).

---

## Self-review

1. **Every AC maps to a phase gate?**
   - AC-1–5, AC-13, AC-15 → Phase 1 integration tests
   - AC-6, 6b, 7, 8, 10, 11, 12, 16, 17 → Phase 2 Playwright smoke + `cargo check`
   - AC-9 → Phase 2 gate (`grep -ri`)
   - AC-14 → Phase 3 Playwright test

2. **Every file either exists or is listed as new?**
   - New: `src/routes/map_layers.rs`, `src/services/map_layers.rs`, `tests/phase_map_layers.rs`, `tests/e2e/map-panel.spec.ts`
   - Existing: all others confirmed present via `ls` and `grep`

3. **Phases are vertical slices?** Yes — Phase 1 is API-only (no DB migration). Phase 2 and 3 are UI. The API endpoint from Phase 1 is what Phase 2 consumes.

4. **Gates are concrete shell commands?** Yes — Phase 2/3 now use Playwright assertions, not manual visual checks.

5. **No parallel agents; no contract file needed** (single developer, sequential phases).

## Resolved review findings

The following runtime-breakers were caught in plan review and folded into the phases above:

1. **`apiFetch` returns parsed body, not `Response`** — `updateMapLayers` uses `const data = await apiFetch(...)` directly (verified app.js:113-114). ✓
2. **Token refresh** — explicitly scoped OUT of v0.1. Refresh token is stored as a hash only (`peer_oauth.rs:187`), so refresh is impossible without a schema change. Expired tokens surface as `peers_failed` (AC-15). Refresh is a separate prerequisite hardening slice. ✓
3. **All-peer-failure ≠ empty** — `updateMapLayers` shows the error state when `items` empty AND `peers_failed` non-empty AND `peers_ok` empty (AC-17). ✓
4. **MCP endpoint URL** — canonical decision: POST directly to `peer.mcp_url` (no `/mcp` append), matching `mcp_client.rs` and `fetch_whoami`. `routes/peers.rs:409` is a pre-existing outlier, flagged but not changed here. ✓
5. **Attribution XSS** — peer attribution is escaped via `escapeHtml` before being passed to `customAttribution`; no MapLibre internals are mutated (AC-16). ✓
6. **Chat chips before layers load** — two-pass injection: at message render and via `rehydrateTranscriptChips` after layers load; raw text stored in `dataset.rawText` (AC-14 rehydration check). ✓
7. **Manual UI gate** — replaced with Playwright smoke (`tests/e2e/map-panel.spec.ts`). ✓

## Open questions (carried from design)

- **Token refresh slice** — should peer token refresh land before this feature, or is "expired → peers_failed" acceptable for v0.1 demos? Plan assumes the latter. Confirm.
- **`vector_url` / PMTiles** (OQ-3) — pass-through only in this plan; no PMTiles rendering. If TerraYield's v0.1 scenario needs vector tiles, add `pmtiles` protocol plugin to Phase 2.
