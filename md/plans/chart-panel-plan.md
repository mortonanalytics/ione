# Chart Panel — Implementation Plan

**Design doc:** `md/design/chart-panel.md`
**Shape:** large (vertical slices, **sequential** — no task manifest; backend + UI interleave within each phase, one work stream via `/code-the-plan`)
**Stack:** Rust / Axum / sqlx (Postgres) backend + static HTML + vanilla JS UI. Verification: `cargo clippy -D warnings`, `cargo test --test <name> -- --ignored --test-threads=1` (DB at `:5433`), `npx playwright test` (server must be running per `playwright.config.ts`), Node `node --test` for the adapter unit test against `../myIO/mcp/lib/validate.mjs`. No migration (sql-architect confirmed existing `stream_events` indexes suffice).

This is **P7-supporting** (Stream P substrate visualization for the Epicenter demo).

## Dependencies
- **myIO browser engine — bundle only.** Vendor into `static/vendor/myio/` from `../myIO/inst/htmlwidgets/`: `myIO/myIOapi.js` (the bundle) + `lib/{d3.min.js,d3-hexbin.js,d3-sankey.min.js}`. **Do NOT vendor `myIO/src/*.js`** — those are ES modules, not loadable as plain scripts; the bundle is self-contained (confirmed by `pymyIO/src/pymyio/static/widget.js`, which loads exactly these four files). Load order: the three d3 libs, then `myIOapi.js`. This exposes `window.myIOchart` (a constructor), invoked `new window.myIOchart({element, config:{layers:[…]}, width, height})`.
- **myIO MCP (dev-agent convenience only, not an IONe runtime dep)** in `.mcp.json`: `{"myio":{"command":"node","args":["/Users/ryanemorton/Documents/GitHub/myIO/mcp/server.mjs"]}}` (stdio; deps in `myIO/mcp/node_modules`). Used for chart-type introspection during implementation; the IONe server never calls it at runtime.
- No new Rust crates (`reqwest`, `sqlx`, `serde_json`, `chrono` present).

## Carry-forward defaults (from design Open Questions — not blocking)
1. **Wide→long pivot runs in the adapter JS**; endpoints return ione wide rows.
2. Peer chart resource body = `{spec, rows}`, returned verbatim by `chart-data`.
3. No auto-refresh in v1.

---

## Phase 0 — myIO vendoring + MCP wiring + chart tab skeleton (scaffolding, no feature)

**Goal:** the engine loads in the static shell and a Charts tab opens an empty panel — substrate for both feature slices.

**Files:**
- `.mcp.json` — **modify**. Add the `myio` stdio server alongside `playwright`.
- `static/vendor/myio/` — **create**. Vendor the myIO engine + d3 assets listed in Dependencies.
- `static/index.html` — **modify**. Add `<script src="vendor/myio/lib/d3.min.js">` + the myIO engine scripts before `app.js`. Add `<button id="tab-chart" role="tab" aria-selected="false" aria-controls="panel-chart" class="tab">Charts</button>` after `tab-map` (line 73), and a `panel-chart` `role="tabpanel"` skeleton after `panel-map` (toolbar + `#chart-list` pane + `#chart-render` pane with `#chart-myio-target` div + a `<details>` data-table shell).
- `static/app.js` — **modify**. Add `tabChart`/`panelChart` consts (mirror `tabMap`/`panelMap` at lines 999/1005), extend `switchTab` (lines 1028+) to toggle `chart`, and add a `chartPanel` module with `init(workspaceId)` stub (no fetch yet) fired on first `switchTab('chart')`.
- `static/style.css` — **modify**. Chart panel two-column layout reusing existing map-panel tokens (`--panel-bg`, list-item, error-row, skeleton tokens).

**Gate:**
```
npx playwright test tests/e2e/chart-panel.spec.ts -g "skeleton"
```
**Acceptance:** Playwright loads `/`, asserts `typeof window.myIOchart === "function"`, clicks `#tab-chart`, asserts `#panel-chart` is visible and `#tab-chart[aria-selected=true]`.

---

## Phase 0.5 — render-core spike (de-risk the myIO contract before any backend work)

**Goal:** prove the riskiest assumption end-to-end on a static fixture — that a known ione chart payload converts to a valid myIO `config.layers[]` and renders through `new window.myIOchart(...)` — before building three endpoints against it. (Per the plan review's top recommendation.)

**Files:**
- `static/js/chart_adapter.js` — **create**. Pure function `ioneToMyio(spec, rows) -> {layers:[…]}`: type-name map (`scatter→point`, etc.); `x_var ← bucket_start_ms` (numeric) for time-series, `group_key` for `group_by`; **wide→long pivot** of `series[]` into `{y_var, group}`; emit `columns` type hints. No DOM, no fetch — importable by both `app.js` and the node test.
- `static/__tests__/adapter.test.mjs` — **create**. `node --test`. Import the adapter and `validateSpec` from `../../../myIO/mcp/lib/validate.mjs` (correct relative path from `static/__tests__/` to the sibling repo). Assert: a `line`+`series:["mean","p95"]` payload → long-form layers passing `validateSpec`; a `histogram` payload passes; `x_var` is the numeric `bucket_start_ms`.
- `tests/e2e/chart-panel.spec.ts` — **modify**. Add a "spike" test: inject a hardcoded `{layers:[…]}` fixture, call `new window.myIOchart({element, config, width, height})` in-page, assert the container gets SVG/canvas child nodes and `chart.destroy()` works.

**Gate:**
```
node --test static/__tests__/adapter.test.mjs
npx playwright test tests/e2e/chart-panel.spec.ts -g "spike"
```
**Acceptance:** AC-12/AC-12b pass (adapter output validates against the real `validate.mjs`); the fixture renders through `window.myIOchart` in a real browser. If this phase fails, stop and reconsider the engine choice (uPlot fallback) before building endpoints.

---

## Phase 1 — IONe aggregate charts end-to-end (Slice 1, reuses the Phase 0.5 adapter)

**Goal:** select an IONe-backed chart (e.g. USGS magnitude frequency) → it renders via myIO with an accessible data table. The Epicenter demo gate.

**Files:**
- `src/repos/stream_event_aggregate_repo.rs` — **create**. `StreamEventAggregateRepo` with five methods, each org-scoped via the `stream_events → streams → connectors → workspaces.org_id` join (mirror `fetch_geo_events` in `stream_event_repo.rs`). Numeric extraction guarded by `jsonb_typeof(payload #> $path) = 'number'`; `bucket` is a validated literal (allow-list `hour|day|week`), never bound.
  ```
  // every time-bucketed row carries bucket_start (ISO, RFC3339) AND bucket_start_ms (i64 epoch ms);
  // bucket_start_ms is the numeric x_var the myIO adapter plots (extract(epoch from date_trunc(...))*1000).
  count_by_bucket(ws, org, stream, since, until, bucket) -> [{bucket_start, bucket_start_ms:i64, value:i64}]
  numeric_agg_by_bucket(ws, org, stream, since, until, bucket, value_path:&[&str]) -> [{bucket_start, bucket_start_ms, event_count, valid_count, avg,max,min,sum: Option<f64>}]
  percentile_by_bucket(ws, org, stream, since, until, bucket, value_path, pct:f64) -> [{bucket_start, bucket_start_ms, event_count, percentile_value: Option<f64>}]
  count_by_group(ws, org, stream, since, until, group_path) -> [{group_key: Option<String>, event_count}]  // LIMIT 200, truncated flag; no bucket
  rolling_baseline(ws, org, stream, since, until) -> [{bucket_start, bucket_start_ms, event_count, trailing_30d_avg: Option<f64>}]  // day buckets; query range extended since-30d, rows < since dropped
  ```
- `src/repos/mod.rs` — **modify**. `pub use` the new repo + its row types.
- `src/routes/event_aggregates.rs` — **create**. `GET` handler: `Query` params (`stream_id, op, bucket, value_pointer, percentile, group_by_pointer, since, until`); `ensure_workspace_in_org`; stream-in-org check (404 else); guardrails (window ≤ 90d → 400; `ceil(window/bucket) ≤ 1000` → 400; `op` requires `value_pointer` for numeric/percentile → 400; `percentile ∈ (0,1]`); `value_pointer`/`group_by_pointer` JSON-Pointer → `Vec<String>` path. Dispatch on `op` → repo method. Response `{op, bucket, rows, truncated}`.
- `src/services/chart_panels.rs` — **create**. `fetch_chart_panels` — Phase 1 builds the `ione_charts` section by **reusing the geo `view_config`** (no new config contract): for each workspace stream with a `view_config`, read its `property_fields[]`; emit a count-over-time `ChartPanelItem` per stream, plus avg/max/percentile-over-time items per **numeric** property field (descriptor `{stream_id, op, bucket, value_pointer = the field's JSON Pointer}`). `ChartPanelItem{id, name, source:"ione", spec, descriptor}`. (`peer_charts` empty until Phase 2.) Note `view_config` is geo-only (requires lon/lat) — that's fine; charts ride on the same `property_fields` the map legend uses.
- `src/routes/chart_panels.rs` — **create**. `GET` handler; `ensure_workspace_in_org`; returns `{ione_charts, peer_charts:[], peer_errors:[]}`.
- `src/routes/mod.rs` — **modify**. Register `/api/v1/workspaces/:id/event-aggregates` (get) and `/api/v1/workspaces/:id/chart-panels` (get); add `pub mod event_aggregates; pub mod chart_panels;`.
- `static/app.js` — **modify**. `chartPanel`: fetch `chart-panels`, render the list (mirror the shipped map-panel list + partial-failure rows + retry + polite live-region + the **loaded-workspace guard** so a workspace switch drops stale results and resets the panel). On select of an IONe item → fetch `event-aggregates` with the descriptor → call `ioneToMyio()` (the Phase 0.5 adapter) to build `config.layers[]` → `new window.myIOchart({element:'#chart-myio-target', config, width, height})`; listen `chart.on('error')` → **single render-error banner** (no runtime validate call — there is no browser validator). Populate the `<details>` data table from the rows. Destroy the prior `myIOchart` before re-rendering.
- `static/index.html` — **modify**. Flesh out the `panel-chart` render pane (header, banners with `role=status`/`role=alert`, `#chart-myio-target` `role=img` + `aria-describedby`, `<details><table>`).
- `tests/phase_chart_aggregates.rs` — **create**. DB-backed `#[ignore]` integration (mirror `tests/phase_event_layers.rs` `spawn_app`): AC-1 (count + `bucket_start_ms` present and numeric), AC-2 (avg/max + epoch-ms via the geojson_poll path or seeded rows), AC-3 (percentile), AC-4 (group-by), AC-5 (rolling baseline), AC-6 (non-numeric skip → valid_count<event_count, no 500), AC-7 (90d×hour → 400), AC-8 (cross-org → 404).
- `tests/e2e/chart-panel.spec.ts` — **modify**. Add AC-13: select an IONe `line` chart (stub `chart-panels` + `event-aggregates` at the network layer, like the map e2e), assert `#chart-myio-target` has child nodes (the `myIOchart` render), the `<table>` row count equals the data-point count, and the panel is axe-clean.

**Gate:**
```
cargo clippy --all-targets -- -D warnings
DATABASE_URL=postgres://ione:ione@localhost:5433/ione cargo test --test phase_chart_aggregates -- --ignored --test-threads=1
npx playwright test tests/e2e/chart-panel.spec.ts
```
**Acceptance:** AC-1..8 (Rust) + AC-13 (Playwright + axe) pass. (AC-12/12b are proven in Phase 0.5.)

---

## Phase 2 — Peer-published chart resources (Slice 2)

**Goal:** a chart a peer app publishes over MCP renders in the same panel, no IONe data computation.

**Files:**
- `src/services/chart_panels.rs` — **modify**. Add the `peer_charts` fan-out: mirror `fetch_map_layers` (`src/services/map_layers.rs`) — `WorkspacePeerBindingRepo::list_active_peers_for_workspace`, `resources/list` per peer, `extract_chart_panel` keeping `metadata.ione_view == "chart"`, dedup by `(peer_id, uri)`, collect `peer_errors`.
- `src/services/chart_data.rs` — **create**. `fetch_chart_data(http, peer, uri)` — new `call_resources_read(endpoint, token, uri)` issuing JSON-RPC `resources/read` `{params:{uri}}`; read the body from **`result.contents[0].text`** (a JSON string per the MCP resource contract — distinct from `resources/list`'s `result.resources`), parse as `{spec, rows}`, return verbatim. 5s timeout like the map fan-out.
- `src/routes/chart_data.rs` — **create**. `GET ?peer_id=&uri=` handler — **both required** (400 if either missing); `ensure_workspace_in_org`; look up that exact `peer_id` among the workspace's bound peers (404 if not bound) and call `fetch_chart_data` for `uri`; `502` on peer unreachable / `resources/read` error.
- `src/routes/mod.rs` — **modify**. Register `/api/v1/workspaces/:id/chart-data` (get); `pub mod chart_data;`.
- `static/app.js` — **modify**. `chartPanel` lists peer charts alongside IONe charts (source label); peer items carry `peer_id`+`uri`; on select → fetch `chart-data?peer_id=&uri=` → same `ioneToMyio()`/`myIOchart` render path as Phase 1.
- `tests/phase_chart_peer.rs` — **create**. DB-backed `#[ignore]` with a `wiremock` peer (mirror `tests/phase13_connectors.rs` MockServer + `tests/phase_map_layers.rs`): AC-9 (`chart-panels` lists both sections; peer items carry `peer_id`), AC-10 (`chart-data?peer_id=&uri=` parses `{spec, rows}` from a `resources/read` `contents[0].text` body; omitting `peer_id` → 400), AC-11 (one peer errors on `resources/list` → other peer's charts still return + `peer_errors` names the failure).
- `tests/e2e/chart-panel.spec.ts` — **modify**. Add a peer-chart render case (stub `chart-panels` peer section + `chart-data` at the network layer).

**Gate:**
```
cargo clippy --all-targets -- -D warnings
DATABASE_URL=postgres://ione:ione@localhost:5433/ione cargo test --test phase_chart_peer -- --ignored --test-threads=1
npx playwright test tests/e2e/chart-panel.spec.ts
```
**Acceptance:** AC-9, AC-10, AC-11 (Rust) + the peer-render e2e pass.

---

## Requirements impact (post-merge, via `update-requirements` at `/preflight`/`/pr`)
Per design § Requirements impact: (a) `app-integration-playbook.md` — already corrected `chart` to v0.1 + add the `vnd.ione.chart+json` **resource body** `{spec, rows}` contract; (b) `ione-substrate.md` — chart removed from v0.1 exclusions (done in design pass); (c) `infrastructure-backlog.md` — move chart-panel P0 to done and fold the P2 windowed-aggregates item into `event-aggregates`.

---

## Self-review
1. **Every design AC maps to a phase gate?** Yes — AC-12/12b → Phase 0.5 (`adapter.test.mjs` + spike e2e); AC-1..8 → Phase 1 (`phase_chart_aggregates`); AC-13 → Phase 1 (Playwright); AC-9/10/11 → Phase 2 (`phase_chart_peer`); skeleton smoke → Phase 0.
2. **Every file exists now or is listed to create?** Verified existing (modify targets): `.mcp.json`, `static/index.html`, `static/app.js`, `static/style.css`, `src/routes/mod.rs`, `src/repos/mod.rs`, `src/services/map_layers.rs` (precedent only). Created: the 6 new `src/` files, `static/vendor/myio/*` (bundle + d3 only), `static/js/chart_adapter.js`, `static/__tests__/adapter.test.mjs`, 2 Rust test files. `tests/e2e/chart-panel.spec.ts` created in Phase 0, extended in 0.5/1/2.
3. **Vertical slices, not layer stacks?** Phase 0 = scaffolding (≤10 files, no feature). Phase 0.5 = a thin render-core spike that de-risks the engine contract (UI-only, no backend). Phase 1 ships the IONe chart end-to-end (repo+route+UI+tests, reusing the 0.5 adapter). Phase 2 ships peer charts end-to-end (reuses the render core).
4. **Gates concrete shell commands?** Yes — named `cargo test --test …`, `node --test`, `npx playwright test -g …`.
5. **Parallel tasks disjoint?** N/A — sequential. Phase 1 and 2 both touch `src/services/chart_panels.rs`, `src/routes/mod.rs`, `static/app.js`, so they must not run in parallel.

**Implementer notes:** (a) Playwright needs a running server (`IONE_TOKEN_KEY` + `IONE_WEBHOOK_SECRET_KEY` + `IONE_BIND=127.0.0.1:3007 cargo run`) — same as the map e2e; the chart e2e stubs `chart-panels`/`event-aggregates`/`chart-data` at the network layer so no peer/DB seeding is needed for AC-13. (b) DB tests need `--test-threads=1` (shared-DB truncate races). (c) `bucket` must be allow-listed before SQL interpolation — injection guard. (d) browser render entry is **resolved**: `new window.myIOchart({element, config:{layers:[…]}, width, height})` + `chart.on('error')` + `chart.destroy()` — pattern from `../pymyIO/src/pymyio/static/widget.js`; field names per type from `../myIO/mcp/myio-schema.json`. (e) **Do Phase 0.5 before any backend work** — if `myIOchart` can't render the fixture or the adapter spec fails `validate.mjs`, stop and reconsider the engine (uPlot fallback) before building endpoints.
