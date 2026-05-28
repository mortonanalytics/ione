# Event Point Layer — Implementation Plan

**Design doc:** [md/design/event-point-layer.md](../design/event-point-layer.md)
**Shape:** medium (≈12 files across db/api/ui; one contract, no parallel agents)
**Stack:** Rust 1.7x · axum 0.7 · sqlx 0.8 (postgres+uuid+chrono+json+macros) · Postgres 16 (local :5433 via docker compose) · static HTML/JS (no SPA) · Playwright 1.x for e2e

## Dependencies

None new. Everything in use:
- `serde_json::Value::pointer` — RFC 6901 JSON Pointer resolver, already in tree via serde_json.
- `chrono::DateTime<Utc>` — already used by `stream_events.observed_at`.
- `axum::extract::Query` — already used by [`map_layers.rs:19-22`](../../src/routes/map_layers.rs#L19-L22).

If the validation needs a stricter Pointer parser, prefer `serde_json::Value::pointer` first (cheapest, already there). Only add a crate if a verified gap surfaces.

---

## Phases

### Phase 0 — DB scaffold + connector write path

**Goal:** `streams.view_config` exists, **and every stream-creation code path can persist it**, so real FIRMS / IRWIN / OpenAPI streams are renderable in production — not just seeded test rows. No user-visible change yet (no endpoint, no UI).

(Codex review high-3: without this, seeded tests pass while every real connector stream stays `view_config IS NULL` and invisible to `/event-layers`.)

**Files:**
- `migrations/0030_streams_view_config.sql` — **new**.
- `src/connectors/mod.rs` — **modify**. Add `pub view_config: Option<serde_json::Value>` to `StreamDescriptor` (line 16).
- `src/repos/stream_repo.rs` — **modify**. `upsert_named` (line 16) takes a `view_config: Option<&Value>` parameter and writes it on INSERT and `ON CONFLICT DO UPDATE` — connector-supplied config is authoritative each poll cycle. Manual out-of-band edits surviving a re-poll is deferred to the authoring-surface follow-up (Open Question 1).
- `src/connectors/firms.rs` — **modify**. `default_streams` (line 174) emits a hard-coded `view_config` with `/latitude`, `/longitude`, property fields for `bright_ti4` and `frp`, and a sensible default size/color encoding. Hand-wired connectors hardcode their feed shape today; the `view_config` lives in the same code, not in an external config file.
- `src/connectors/irwin.rs` — **modify**. Same pattern: `/Latitude`, `/Longitude` (Pascal-case — verified via `infra/fixtures/irwin_incidents.json` in the design's devil's-advocate pass).
- `src/connectors/openapi.rs` — **modify**. `default_streams` (line 379) reads an optional `view_config` block from the OpenAPI connector's config JSON and forwards it. No view_config in config → `None` → stream is not rendered as a point layer.
- `src/connectors/nws.rs` — **modify**. Set `view_config: None` in the descriptor. (NWS payload has no geometry today — Open Question 3 tracks the connector-side fix; out of scope here.)
- `src/connectors/fs_s3.rs`, `src/connectors/mcp_client.rs`, `src/connectors/slack.rs`, `src/connectors/smtp.rs` — **modify**. Set `view_config: None` in the descriptor. Mechanical struct-init update.
- `tests/support/mod.rs` + `tests/support/event_layer_seeder.rs` — **new**. Seeder helper (see signature below) for unit/integration tests with synthetic events that bypass the real connectors.

**Code shapes:**

```sql
-- 0030_streams_view_config.sql
ALTER TABLE streams ADD COLUMN view_config JSONB;
CREATE INDEX streams_view_config_present ON streams (id) WHERE view_config IS NOT NULL;
COMMENT ON COLUMN streams.view_config IS 'Optional per-stream geometry + style mapping for the /event-layers endpoint. See md/design/event-point-layer.md. NULL = stream is not rendered as a point layer.';
```

```rust
// src/connectors/mod.rs
pub struct StreamDescriptor {
    pub name: String,
    pub schema: serde_json::Value,
    pub view_config: Option<serde_json::Value>,   // NEW
}
```

```rust
// src/repos/stream_repo.rs — updated signature
pub async fn upsert_named(
    &self,
    connector_id: Uuid,
    name: &str,
    schema: &serde_json::Value,
    view_config: Option<&serde_json::Value>,  // NEW
) -> anyhow::Result<Uuid> /* stream_id */
```

Seeder helper:
```rust
pub async fn seed_geo_stream(
    pool: &PgPool,
    workspace_id: Uuid,
    stream_name: &str,
    view_config: serde_json::Value,
    events: Vec<(serde_json::Value, DateTime<Utc>)>, // (payload, observed_at)
) -> Uuid /* stream_id */
```

**Gate:**
```
docker compose up -d postgres && \
  sqlx migrate run --database-url postgres://ione:ione@localhost:5433/ione && \
  cargo check && \
  cargo clippy --all-targets -- -D warnings
```

**Acceptance:** `psql -c "\d streams"` shows `view_config | jsonb` (default NULL). `cargo check` compiles all 8 connector impls with the new field. A focused unit test asserts that calling `default_streams()` on FIRMS and IRWIN returns descriptors whose `view_config.lon_pointer` resolves into a sample payload from the same connector's fixture.

---

### Phase 1 — Endpoint walking skeleton (happy path + isolation + zero-event streams + event-only workspaces)

**Goal:** `GET /api/v1/workspaces/:id/event-layers` returns one layer per geo-mapped stream (with empty `collection` when there are zero events in window); the static UI calls it in parallel with `/map-layers`, **renders even when there are no raster layers**, and adds circles on the map above raster layers with an "Events" text badge in the layer-control row.

(Codex review high-1: SQL must catalog geo-mapped streams independently of events, so zero-event streams still emit an `EventLayer`. Codex review high-2: UI must handle workspaces with event layers and zero raster layers — today's `updateMapLayers` bails out and destroys the map at the first empty-rasters check. Codex review medium-1: `truncated` from `LIMIT 5000` alone is ambiguous; use `LIMIT $N + 1`.)

**Files:**
- `src/repos/stream_event_repo.rs` — **modify**. Add `fetch_geo_events`.
- `src/services/event_layers.rs` — **new**. Pure projection (`project_event_layers`), `ViewConfig` parse types, JSON Pointer resolution.
- `src/services/mod.rs` — **modify**. `pub mod event_layers;`.
- `src/routes/event_layers.rs` — **new**. Handler `list_event_layers`: extract `AuthContext`, parse `MapLayersQuery`-style `Query<EventLayersQuery>`, validate window, call `ensure_workspace_in_org`, call repo, call service, return `Json`.
- `src/routes/mod.rs` — **modify**. Register `.route("/api/v1/workspaces/:id/event-layers", get(event_layers::list_event_layers))` adjacent to the existing map-layers entry at [src/routes/mod.rs:108-109](../../src/routes/mod.rs#L108-L109). Add `pub mod event_layers;`.
- `static/app.js` — **modify**. Rewrite `updateMapLayers` (current code at line ~1134 destroys the map when rasters are empty — that branch must change). Rename current `renderMapLayers` → `renderMapWithLayers`; rename current `addLayersToMap` → `addRasterLayersToMap`; add `addEventLayersToMap` and `fitMapBounds` (extending the existing `LngLatBounds` logic to read GeoJSON point coords when no `meta.bounds` is present). Both raster and event layer additions happen inside one `mapInstance.on('load', ...)` callback.
- `static/index.html` — **modify**. No new sections yet; rely on existing `#map-layer-list` and `#map-canvas`.
- `static/style.css` — **modify**. Add `.layer-row--event` and `.layer-type-badge` rules (text badge, no color-only encoding).
- `tests/phase_event_layers.rs` — **new**. Contract + integration tests for AC-1, AC-4, AC-5, AC-6, AC-7. Pattern after [tests/phase_map_layers.rs](../../tests/phase_map_layers.rs).
- `tests/e2e/event-layers.spec.ts` — **new**. Single Playwright spec for AC-8 (raster + circles coexist, event row badged, z-order).

**Code shapes:**

Endpoint contract (already in design § API Contracts) — handler query type:
```rust
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventLayersQuery {
    #[serde(default, alias = "since")]
    pub since: Option<DateTime<Utc>>,
    #[serde(default, alias = "until")]
    pub until: Option<DateTime<Utc>>,
    #[serde(default, alias = "stream_id")]
    pub stream_id: Option<Uuid>,
    #[serde(default, alias = "limit")]
    pub limit: Option<i64>,
}
```

Defaults / bounds (handler-level, raise `AppError::BadRequest` on violation):
```
since.unwrap_or(now - 24h);  until.unwrap_or(now)
since <= until                                          // else 400
until - since <= 30d                                    // else 400
limit.unwrap_or(5000) in 1..=5000                       // else 400
```

Repo: **two queries, not one** — the catalog query establishes which layers must appear in the response (so a zero-event stream still gets an `EventLayer` with empty `collection`); the events query fetches the actual rows with `LIMIT + 1` for unambiguous truncation detection.

```sql
-- Q1: catalog of geo-mapped streams in the workspace (used to enumerate EventLayer entries
--     even when no events fall in the window — addresses AC-11 "zero events in window").
SELECT s.id, s.name, s.view_config
FROM streams s
JOIN connectors c ON c.id = s.connector_id
WHERE c.workspace_id = $1
  AND EXISTS (SELECT 1 FROM workspaces w WHERE w.id = $1 AND w.org_id = $2)
  AND s.view_config IS NOT NULL
  AND ($3::uuid IS NULL OR s.id = $3)
ORDER BY s.name;
```

```sql
-- Q2: events for those streams, with LIMIT + 1 so the service can report truncated reliably.
SELECT se.id, se.stream_id, se.payload, se.observed_at
FROM stream_events se
JOIN streams s    ON s.id = se.stream_id
JOIN connectors c ON c.id = s.connector_id
WHERE c.workspace_id = $1
  AND EXISTS (SELECT 1 FROM workspaces w WHERE w.id = $1 AND w.org_id = $2)
  AND s.view_config IS NOT NULL
  AND ($3::uuid IS NULL OR s.id = $3)
  AND se.observed_at >= $4
  AND se.observed_at <= $5
ORDER BY se.observed_at DESC
LIMIT $6 + 1;
```

Service logic (pure, no I/O):
```rust
pub fn project_event_layers(
    catalog: Vec<GeoStreamRow>,           // from Q1
    mut events: Vec<GeoEventRow>,         // from Q2
    limit: i64,                           // the user-requested limit
    queried_at: DateTime<Utc>,
) -> EventLayersResponse {
    // 1. truncated = events.len() as i64 > limit; if so, truncate to `limit` rows.
    // 2. Group events by stream_id.
    // 3. For each catalog row: try ViewConfig::parse(row.view_config).
    //      - parse fail -> push to streamsFailed.
    //      - parse ok   -> build EventLayer with collection = events for this stream
    //                       (possibly empty), incrementing featuresSkipped on
    //                       null/non-numeric coordinate pointers.
    // 4. streamsOk = streams that produced an EventLayer.
}
```

**Chatty-stream caveat (deferred):** one busy stream can consume the global `limit` budget and shrink other layers. The cheap mitigation (per-stream cap) is not in v1 — added to Open Questions as a follow-up. Workspaces with one geo-mapped stream (the Epicenter demo shape) are unaffected. Callers facing it today can pass `stream_id` to fence the query.

Response wire types match the design § Response shape tables; serde rename_all = "camelCase". Each `Feature.properties` is built by resolving each `view_config.property_fields[*].pointer` via `serde_json::Value::pointer`, then injecting `_event_id` and `_observed_at` last. **Property keys come from `property_fields[*].name`, not from the JSON Pointer.**

Static UI state machine (rewrite of `updateMapLayers`). Current code (app.js:1144) bails out and destroys the map when rasters are empty; that has to change. New shape:

```js
async function updateMapLayers(workspaceId) {
  mapLoadedWorkspaceId = workspaceId;
  showMapState('loading');
  const [rastersR, eventsR] = await Promise.allSettled([
    apiFetch(`/api/v1/workspaces/${workspaceId}/map-layers`),
    apiFetch(`/api/v1/workspaces/${workspaceId}/event-layers`),
  ]);
  if (!activeWorkspace || activeWorkspace.id !== workspaceId) return;

  const rasters     = rastersR.status === 'fulfilled' ? (rastersR.value.items || []) : [];
  const rasterFail  = rastersR.status === 'rejected';
  const eventLayers = eventsR.status === 'fulfilled' ? (eventsR.value.layers || []) : [];
  const eventFail   = eventsR.status === 'rejected';

  if (rasters.length === 0 && eventLayers.length === 0) {
    destroyMap();
    renderLayerControl([]);
    if (rasterFail && eventFail) showMapError('Could not load map.');
    else showMapState('empty');
    return;
  }

  showMapState('canvas');
  renderMapWithLayers({ rasters, eventLayers });  // creates the map and, on 'load', adds rasters then circles
  renderLayerControl([...rasters, ...eventLayers]);
  if (eventFail) renderEventLayerError();         // Phase 2 surface
  rehydrateTranscriptChips();
}
```

`renderMapWithLayers` owns the MapLibre lifecycle and replaces today's `renderMapLayers`. Both raster source/layer adds and event source/layer adds happen inside a single `mapInstance.on('load', ...)` handler so we never call `addSource` before the style is ready:

```js
function renderMapWithLayers({ rasters, eventLayers }) {
  // build attribution list from rasters + eventLayers.attribution
  destroyMap();
  mapInstance = new maplibregl.Map({
    container: 'map-canvas',
    style: { version: 8, sources: {}, layers: [] },
    keyboard: true, attributionControl: false,
    fadeDuration: prefersReducedMotion() ? 0 : 300,
  });
  attachAttributionControl(rasters, eventLayers);
  mapInstance.on('load', () => {
    addRasterLayersToMap(rasters);   // existing addLayersToMap, renamed
    addEventLayersToMap(eventLayers); // new — added AFTER rasters so circles draw on top
    fitMapBounds(rasters, eventLayers);
  });
}
```

```js
function addEventLayersToMap(eventLayers) {
  eventLayers.forEach((layer) => {
    const sourceId = `evt-src-${layer.streamId}`;
    const layerId  = `evt-lyr-${layer.streamId}`;
    mapEventSourceIds.add(sourceId);
    mapEventLayerIds.add(layerId);
    mapInstance.addSource(sourceId, { type: 'geojson', data: layer.collection });
    mapInstance.addLayer({ id: layerId, type: 'circle', source: sourceId,
      paint: {
        'circle-radius': interpolateSize(layer.style),   // literal fallback when style.sizeField is null
        'circle-color':  interpolateColor(layer.style),  // literal fallback when style.colorField is null
        'circle-stroke-color': '#fff', 'circle-stroke-width': 1,
      },
    });
  });
}
```

`fitMapBounds` extends the existing `LngLatBounds` logic so event-only workspaces (no `meta.bounds` from rasters) compute a viewport from `layer.collection.features[*].geometry.coordinates` — otherwise the map opens at MapLibre's `[0,0]` default, not a useful view.

**Gate** (Rust gates run unconditionally; Playwright gate requires a running IONe server on `127.0.0.1:3007` per [playwright.config.ts:4-10](../../playwright.config.ts#L4-L10) — bring it up with `IONE_BIND=127.0.0.1:3007 cargo run` in a separate terminal first):
```
cargo check && \
  cargo clippy --all-targets -- -D warnings && \
  cargo test --test phase_event_layers && \
  npx playwright test tests/e2e/event-layers.spec.ts -g "raster and event circles coexist"
```

**Acceptance:** AC-1, AC-4, AC-5, AC-6 (truncation now driven by `LIMIT + 1`, not row-count==limit), AC-7 (assertions in `tests/phase_event_layers.rs`), and AC-8 (Playwright spec) pass. AC-11's "geo-mapped stream with zero events in window" half is unblocked at the API level here (the catalog query returns the stream row); the UI assertion lands in Phase 3.

---

### Phase 2 — Failure handling and partial-failure UI

**Goal:** Per-feature skip, whole-stream fail (including partial-style triples), and partial-failure UI error row work end-to-end. The endpoint never 500s on bad config — every config error lands in `streamsFailed`.

(Codex review medium-2: `serde_json::Value::pointer` is a resolver, not a validator — Phase 2 must add an explicit validation pass. The OpenAPI connector at [src/connectors/openapi.rs:503](../../src/connectors/openapi.rs#L503) already does pointer-shape validation worth mirroring.)

**Files:**
- `src/services/event_layers.rs` — **modify**. Introduce a `ViewConfig::parse(value: &Value) -> Result<CompiledConfig, ViewConfigError>` step that runs BEFORE projection. Validation steps, executed in order, each producing a distinct `ViewConfigError`:
  1. **Pointer syntax** — every pointer string in `lon_pointer`, `lat_pointer`, `property_fields[*].pointer` must be empty (`""`) or start with `/`, with `~` only as part of `~0` / `~1` escapes (RFC 6901 §3). Mirror the OpenAPI connector's `validate_pointer` (or extract it to a shared `util::json_pointer` module — cheaper to share than to fork).
  2. **Required pointers present** — `lon_pointer` and `lat_pointer` both non-empty strings; otherwise `MissingLonPointer` / `MissingLatPointer`.
  3. **Property name shape** — each `property_fields[*].name` matches `^[a-zA-Z_][a-zA-Z0-9_]*$`, ≤64 chars; uniqueness across the array; not colliding with the always-injected `_event_id` / `_observed_at`.
  4. **Style triple all-or-nothing** — count of present fields in `{size_field, size_domain, size_range}` is 0 or 3; same for the color triple. Anything else → `PartialStyleTriple { which: "size" | "color" }`.
  5. **Style range/domain shape** — when present, `size_domain` and `size_range` are `[f64; 2]`; `color_domain.len() == color_range.len()` and both ≥ 2.
  6. **Style field references** — `style.size_field`, `style.color_field`, `style.label_field`, when present, must match one of `property_fields[*].name`. Otherwise `UnknownStyleFieldReference { field, name }`. This catches the common authoring error of misspelling a style field.

  All errors are flattened into the `StreamProjectionError.error` string field for the wire response.
- `static/app.js` — **modify**. On `/event-layers` non-200 response or network failure, write to `#event-layer-status` (the polite live-region wrapper from Phase 3 — gated separately, so for Phase 2 add it as a minimal element next to `#map-layer-list`); render a `layer-row--error` list item with a Retry button that re-fires the fetch.
- `static/index.html` — **modify**. Add `<div id="event-layer-status" aria-live="polite"></div>` immediately above `<ul id="map-layer-list">`. No `role="status"` on the row.
- `static/style.css` — **modify**. `.layer-row--error` styles.
- `tests/phase_event_layers.rs` — **modify**. Add AC-2, AC-3, AC-12.
- `tests/e2e/event-layers.spec.ts` — **modify**. Add AC-9 spec (raster ok, events 500, error row shown, canvas alive, no role attr on the row).

**Code shapes:**

Validation result shape (kept inside the service module — implementation detail):
```rust
enum ViewConfigOutcome {
    Ok(CompiledConfig),                          // ready to project
    Invalid { stream_id: Uuid, name: String, error: String },  // goes to streamsFailed
}
```

Per-stream all-or-nothing rule (literal check; pseudocode):
```
present(size_field) ^ present(size_domain) ^ present(size_range)  ==  true OR  false   // all-3 or none-3
present(color_field) ^ present(color_domain) ^ present(color_range)  ==  true OR  false
color_domain.len() == color_range.len()                            // when both present
```

Per-feature skip rule (in the projection loop):
```
let lon = payload.pointer(view_config.lon_pointer).and_then(Value::as_f64);
let lat = payload.pointer(view_config.lat_pointer).and_then(Value::as_f64);
if lon.is_none() || lat.is_none() { features_skipped += 1; continue; }
```

Static UI failure path inside `updateMapLayers`:
```js
const [layersResp, eventsResp] = await Promise.allSettled([
  apiFetch(`/api/v1/workspaces/${workspaceId}/map-layers`),
  apiFetch(`/api/v1/workspaces/${workspaceId}/event-layers`),
]);
// raster path: existing behavior using layersResp
// event path: on rejection or non-2xx, set #event-layer-status textContent and insert layer-row--error
```

**Gate** (server must be running per Phase 1 note):
```
cargo test --test phase_event_layers && \
  cargo clippy --all-targets -- -D warnings && \
  npx playwright test tests/e2e/event-layers.spec.ts
```

**Acceptance:** AC-2, AC-3, AC-9, AC-12 all pass. Clippy clean. The validation step in `event_layers.rs` rejects every bad-config fixture in the test file with a `StreamProjectionError.error` containing the offending field name.

---

### Phase 3 — Legend, accessible event list, popup, empty states

**Goal:** Polish + accessibility. Legend renders for visible event layers; an accessible `<details>` event list provides keyboard/SR reach to the points; popup opens with focus management; the three empty/error states are visually distinct.

(Codex review medium-3: the previous gate referenced `axe-core` and a running server, neither of which were wired. This phase explicitly adds `@axe-core/playwright` as a dev dependency; the server-startup precondition matches the existing pattern documented in [playwright.config.ts:4-10](../../playwright.config.ts#L4-L10).)

**Files:**
- `package.json` — **modify**. Add `"@axe-core/playwright": "^4.10.0"` to `devDependencies`. Re-run `npm install`.
- `static/index.html` — **modify**. Add `<section id="event-layer-legend" hidden>` inside `#map-canvas-container` and `<details id="event-list-disclosure" hidden><summary></summary><table></table></details>` below the canvas.
- `static/app.js` — **modify**. `renderLegend(layers)` (size ramp + color gradient + attribution + "Last 24 h · Updated N min ago" footer); `renderEventList(layers)` (≤100 rows, "Show on map" buttons firing `flyTo` + opening popup programmatically); `openEventPopup(feature, triggeredByKeyboard)` (move focus to close button only when keyboard-triggered); empty-state branching ("No events in last 24 h." vs silent).
- `static/style.css` — **modify**. Legend card (`var(--color-border)`, `var(--radius)`, max-width 180px, bottom-left), gradient bar, ramp circles, disclosure styles.
- `tests/e2e/event-layers.spec.ts` — **modify**. Add AC-10 (keyboard reachability + popup focus, axe-core scan using `@axe-core/playwright`'s `AxeBuilder`) and AC-11 (empty-state differentiation, including the event-only workspace case).

**Code shapes:**

Click target widening for circle layers (WCAG 2.5.5):
```js
mapInstance.on('click', layerId, (e) => {
  const r = 22;
  const bbox = [[e.point.x - r, e.point.y - r], [e.point.x + r, e.point.y + r]];
  const features = mapInstance.queryRenderedFeatures(bbox, { layers: [layerId] });
  if (features[0]) openEventPopup(features[0], false);
});
```

Color contrast guardrail (encoded in the legend renderer): if `style.colorRange[0]` resolves to a CSS color with `<3:1` contrast against `#ffffff`, log a console warning. No runtime block; the warning surfaces config errors during development.

**Gate** (server must be running; `npm install` must have been re-run after adding `@axe-core/playwright` to `package.json`):
```
cargo test --test phase_event_layers && \
  npm install --no-audit --no-fund && \
  npx playwright test tests/e2e/event-layers.spec.ts
```

**Acceptance:** AC-10 and AC-11 pass; the full Playwright suite for this feature is green; the `AxeBuilder` scan invoked inside the AC-10 spec returns zero violations on the map panel with one event layer visible. The AC-11 spec covers all three states: zero geo-mapped streams (silent), geo-mapped stream with zero events ("No events in last 24 h."), and event-only workspace (no rasters, points render alone).

---

## Self-review against the checklist

1. **Every AC mapped to a phase gate?** Yes — AC-1, AC-4, AC-5, AC-6, AC-7, AC-8 → Phase 1; AC-2, AC-3, AC-9, AC-12 → Phase 2; AC-10, AC-11 → Phase 3. AC-11's API half (catalog returns zero-event geo streams) is established in Phase 1; the UI assertions (silent vs "No events in last 24 h." vs event-only) land in Phase 3.
2. **All files exist or appear in inventory?** Verified for every "modify" target (`src/connectors/{mod,firms,irwin,openapi,nws,fs_s3,mcp_client,slack,smtp}.rs`, `src/repos/stream_repo.rs`, `src/routes/{mod,map_layers}.rs`, `static/{app.js,index.html,style.css}`, `package.json`, `playwright.config.ts`). New files listed: 1 migration, 1 service, 1 route, 1 test, 1 Playwright spec, 1 test-support module.
3. **Phases vertical, not layer-stacked?** Phase 0 is the single allowed scaffolding phase (migration + connector write path; no user-visible behavior yet). Phases 1–3 each ship db+api+ui in one slice. Phase 2 is API-validation-heavy but ships its UI failure-row counterpart at the same time; Phase 3 is UI-heavy with no API changes — natural for polish work.
4. **Gates are concrete commands?** Yes — every gate names exact `cargo test --test` and `npx playwright test -g` invocations, plus the explicit server-startup precondition.
5. **Parallel-task disjointness?** N/A (medium plan, no task manifest, no parallel agents).

## Codex review reconciliation

Reflecting the review against this plan and the design doc:

| Codex finding | Resolution | Where it landed |
|---|---|---|
| High-1: SQL can't surface zero-event geo streams (breaks AC-11) | Split into catalog query (Q1) + events query (Q2); service merges. | Phase 1 — Repo SQL section. |
| High-2: UI bails when rasters empty; ignores map.on('load') sequencing | Rewrote `updateMapLayers` state machine; both raster and event layers added inside one `map.on('load')`; `fitMapBounds` extended for event-only workspaces. | Phase 1 — Static UI state machine section. |
| High-3: No production write path for `view_config` | Added `StreamDescriptor.view_config`, plumbed through `upsert_named` and all 8 connector impls (real values for FIRMS/IRWIN/OpenAPI; `None` for the rest). | Phase 0 — file inventory + code shapes. |
| Medium-1: `truncated` ambiguous at `len == limit` | Switched to `LIMIT $N + 1` in Q2; service trims and sets `truncated`. Per-stream fairness deferred to Open Question 6. | Phase 1 SQL; design § Response shape; design Open Question 6. |
| Medium-2: Validation thinner than the contract implies | Phase 2 now spells out a 6-step validator (pointer syntax via shared util mirroring `openapi.rs:503`; required pointers; name shape + uniqueness + reserved-key collision; style triple all-or-nothing; range/domain shape + length parity; style field-name reference into `property_fields`). | Phase 2 — Files section. |
| Medium-3: axe-core gate referenced but not installed; server not running | Added `@axe-core/playwright` to `package.json` devDeps in Phase 3; every Playwright gate now states the server-startup precondition explicitly (matches `playwright.config.ts:4-10`). | Phase 3 files + all Playwright gates. |

## Environment preflight (per CLAUDE.md)

Before Phase 0:
- `command -v docker compose || echo missing`
- `command -v sqlx || cargo install sqlx-cli --no-default-features --features postgres`
- `command -v npx || echo missing` (Playwright)
- `DATABASE_URL` set or `.env` provides `postgres://ione:ione@localhost:5433/ione`
- Postgres listening on :5433 (`nc -z localhost 5433`) — last check showed it down; bring up before Phase 0.

If any of the above is missing, surface it and stop — do not attempt apt-installs or shell gymnastics.

## Carry-forward defaults (not blocking, recorded for the implementer)

From the design's Open Questions:
1. **Authoring surface** — Phase 0 ships the connector-side write path (StreamDescriptor.view_config → upsert_named) so FIRMS/IRWIN/OpenAPI streams render in production. The runtime authoring surface (e.g. `PUT /streams/:id/view-config`) is deferred to the `geojson_poll` design.
2. **`stream_events` retention** — out of scope for this plan; tracked as a separate backlog item.
3. **NWS connector lat/lon injection** — Phase 0 sets `view_config: None` for NWS; the connector-side payload fix to inject geometry is a follow-up.
4. **Color palette safe-list** — Phase 3's legend renderer logs a console warning on low contrast; a project-wide palette doc is a follow-up.
5. **Truncation copy** — Phase 3 picks "Showing N of M — narrow your window" for both `truncated: true` and event-list cap; revisit if a reviewer objects.
6. **Per-stream cap (chatty-stream fairness)** — v1 ships a single global `limit` budget with `LIMIT + 1` truncation detection. A busy stream can crowd out quieter ones; mitigated today by passing `stream_id` to fence the query. A per-stream cap query param (default e.g. 2500) is the cheap follow-up. Added to the design's Open Questions list.
