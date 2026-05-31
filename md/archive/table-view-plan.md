# Table View — Implementation Plan

**Design doc:** `md/design/table-view.md`
**Shape:** large (vertical slices, **sequential** — no task manifest; Phases 1 & 2 share `table_panels.rs`/`app.js`/`mod.rs`, so they can't run in parallel)
**Stack:** Rust / Axum / sqlx (Postgres) backend + static HTML + vanilla JS UI. Verification: `cargo clippy -D warnings`, `cargo test --test <name> -- --ignored --test-threads=1` (DB at `:5433`), `npx playwright test` (server must be running). **No migration; no vendored assets; no render engine** — simpler than the chart panel.

This is **P7-supporting** (Stream P substrate visualization; completes map ✓ / chart ✓ / table).

## Dependencies
None. No new crates, no vendored JS (a `<table>` is plain HTML). Reuses the shipped chart-panel plumbing as the precedent throughout.

## Carry-forward defaults (from design Open Questions — not blocking)
1. Peer column missing `type` → normalize to `string` (in the `table-data` gateway).
2. `datetime` columns render locale-formatted with `title`=raw ISO.
3. IONe tables support a single `filter_col`; multi-column filter is client-side only (peer tables).
4. Streams without `view_config.property_fields` are not IONe tables (peer path serves non-geo apps).

---

## Phase 0 — table tab skeleton (scaffolding, no feature)

**Goal:** a Charts-style Tables tab opens an empty panel.

**Files:**
- `static/index.html` — **modify**. Add `<button id="tab-table" role="tab" aria-selected="false" aria-controls="panel-table" class="tab">Tables</button>` after `tab-chart` (line 79), and a `panel-table` `role="tabpanel"` skeleton after `panel-chart` (toolbar + `#table-list` pane + `#table-render` region + `#table-render-live` aria-live).
- `static/app.js` — **modify**. Add `tabTable`/`panelTable` consts (mirror `tabChart`/`panelChart` ~line 1005); add a `tabTable` click handler (~line 1094, after `tab-chart`); **include `table` in the arrow-key tab-navigation order** (the roving-tablist handler ~line 1100 — Chart ↔ Table ↔ Connectors); extend `switchTab` to toggle `table`; add a `tablePanel` module with `init(workspaceId)` stub fired on first `switchTab('table')`; add `resetTablePanel()` and **call it in the workspace-switch path next to `resetChartPanel()` (~line 577)**.
- `static/style.css` — **modify**. Table panel two-column layout + 4 new table tokens (`--table-border`, `--table-header-bg`, `--table-row-hover-bg`, `--table-row-stripe-bg`) + `--table-row-height`; reuse existing list/skeleton/error tokens.

**Gate:**
```
npx playwright test tests/e2e/table-panel.spec.ts -g "skeleton"
```
**Acceptance:** Playwright loads `/`, clicks `#tab-table`, asserts `#panel-table` visible and `#tab-table[aria-selected=true]`; arrow-key from `#tab-chart` reaches `#tab-table` (roving tablist intact).

---

## Phase 1 — IONe stream_events table (Slice 1 + render core)

**Goal:** select an IONe table (e.g. USGS events) → a paginated, sortable, filterable semantic `<table>` renders. The load-bearing slice.

**Files:**
- `src/services/event_layers.rs` — **modify**. Expose a **table-specific** property-fields parser that does NOT reuse `CompiledConfig::parse` (which hard-requires `lon_pointer`/`lat_pointer` and validates `style` — a geo-style error would otherwise hide a valid table, #4):
  ```rust
  // ordered (name, pointer_path) columns from view_config.property_fields ONLY;
  // ignores lon/lat/style; Ok(vec![]) if no property_fields. Reuses the existing
  // property-name + pointer validation helpers (validate_property_names, validate_pointer_field).
  pub fn table_property_columns(view_config: &serde_json::Value) -> Result<Vec<(String, Vec<String>)>, String>
  ```
  (`pointer_path` is the RFC6901 pointer split into segments for binding as `text[]`.) The `_observed_at` reserved key is already enforced by `validate_property_names`, so no property column can collide with the injected timestamp column (#2).
- `src/repos/stream_event_repo.rs` — **modify**. Add the projection query — **no dynamic SQL aliases** (#3): select the raw `payload` + `observed_at` (and `count(*) OVER()`), then project into named columns in Rust:
  ```rust
  pub enum SortTarget { ObservedAt, Field(Vec<String>) }   // resolved in the route, never from raw input
  pub struct TableQuery { page:i64, per_page:i64, sort:SortTarget, sort_desc:bool,
                          filter:Option<(SortTarget,String)>, since:DateTime<Utc>, until:DateTime<Utc> }
  // SELECT se.payload, se.observed_at, count(*) OVER() AS total
  //   FROM stream_events se JOIN streams JOIN connectors JOIN workspaces
  //   WHERE c.workspace_id=$1 AND w.org_id=$2 AND se.stream_id=$3
  //         AND se.observed_at >= $since AND se.observed_at <= $until
  //         [filter: AND se.payload #>> $fpath::text[] ILIKE '%'||$fval||'%'   (Field)
  //                | AND se.observed_at = $fval::timestamptz                    (_observed_at)]
  //   ORDER BY <Field: CASE jsonb_typeof(payload #> $spath::text[])='number' THEN (..#>>..)::float8 ELSE NULL END,
  //                    payload #>> $spath::text[]   |  ObservedAt: se.observed_at>  [DESC] NULLS LAST
  //   LIMIT $per_page OFFSET (($page-1)*$per_page)
  // returns (Vec<(payload: Value, observed_at: DateTime<Utc>)>, total_count i64)
  pub async fn fetch_table_rows(&self, ws:Uuid, org:Uuid, stream:Uuid, q:&TableQuery)
      -> anyhow::Result<(Vec<(serde_json::Value, DateTime<Utc>)>, i64)>
  ```
  All JSON pointers bound as `text[]` (mirror `numeric_agg_by_bucket`); `sort`/`filter` columns are a `SortTarget` enum, never interpolated. Default sort `observed_at DESC` + the bounded window use the existing `(stream_id, observed_at DESC)` index.
- `src/repos/mod.rs` — **modify**. `pub use` the new types.
- `src/routes/event_table.rs` — **create** (mirror `event_aggregates.rs`, incl. its window guard at lines ~60). `GET` handler: parse params; `ensure_workspace_in_org`; load the stream (404 if not in org) + its `table_property_columns`; resolve `sort_by`/`filter_col` against `{_observed_at} ∪ property column names` → `SortTarget` (400 on unknown); **window**: default `until=now`, `since=now−30d`, reject `since>until` and `until−since>90d` (400, mirror `event_aggregates.rs`); other guardrails: `per_page ∈ 1..=200` (400), `(page-1)*per_page > 10000` → 400, `filter_col` xor `filter_val` → 400, `sort_dir ∈ {asc,desc}`; call `fetch_table_rows`; **project each (payload, observed_at) into a row object in Rust** keyed by column name (`_observed_at` = ISO string; each property column = `payload #>> path` as text or null); respond `{stream_id, columns:[{name,label,type,pointer}], rows, totalCount, page, perPage, truncated:(page*per_page)<total}`. The `_observed_at` column is `{name:"_observed_at", label:"Observed At", type:"datetime", pointer:null}`; property columns default `type:"string"`.
- `src/services/table_panels.rs` — **create** (mirror `chart_panels.rs`). Phase 1 builds `ione_tables`: list workspace streams whose `view_config.property_fields` has ≥1 entry → `TablePanelItem{id, name, source:"ione", stream_id}`. (`peer_tables` empty until Phase 2.)
- `src/routes/table_panels.rs` — **create** (mirror `chart_panels.rs` route). `GET`; `ensure_workspace_in_org`; `{ione_tables, peer_tables:[], peer_errors:[]}`.
- `src/services/mod.rs`, `src/routes/mod.rs` — **modify**. `pub mod`; register `/api/v1/workspaces/:id/table-panels` + `/api/v1/workspaces/:id/event-table` (both get).
- `static/app.js` — **modify**. `tablePanel`: fetch `table-panels`, render the list (mirror chart-panel list + partial-failure + retry + live-region + loaded-workspace guard + AbortController). On select IONe item → fetch `event-table` → render semantic `<table>` (caption with row count, `<th scope=col>` sort buttons w/ `aria-sort`, per-column filter row, striped tbody). Sort/filter/page controls (per_page default 25) → refetch with params, page resets to 1 on sort/filter change; loading overlay retains prior table.
- `static/index.html` — **modify**. Flesh out `panel-table` render region (toolbar: page nav, per-page select, clear-filters; `#table-render-region`; `role=alert` error banner).
- `tests/phase_table.rs` — **create**. DB-backed `#[ignore]` (mirror `tests/phase_chart_aggregates.rs` `spawn_app`): AC-1 (projection: columns==`_observed_at`+property_fields, rows keyed accordingly, totalCount), **AC-1b (a property field named `observed_at` does NOT collide with `_observed_at` — both present, distinct values)**, AC-2 (missing pointer → null cell, 200), AC-3 (pagination + truncated), AC-4 (numeric-aware sort: first row==10), AC-5 (ILIKE substring filter + filtered totalCount), AC-6 (seven guardrail 400s incl. `since>until` and window>90d; + default-window excludes a >30d-old row), AC-7 (cross-org → 404).
- `tests/e2e/table-panel.spec.ts` — **modify**. AC-11: select an IONe table (stub `table-panels` + `event-table` at the network layer), assert `<table>`/`<caption>`/`<th scope=col>` count==columns, header click re-sorts (refetch), tbody rows == response rows, axe-clean.

**Gate:**
```
cargo clippy --all-targets -- -D warnings
DATABASE_URL=postgres://ione:ione@localhost:5433/ione cargo test --test phase_table -- --ignored --test-threads=1
npx playwright test tests/e2e/table-panel.spec.ts
```
**Acceptance:** AC-1, AC-1b, AC-2..7 (Rust) + AC-11 (Playwright + axe) pass. **IONe projection is geo-`view_config`-only by design** (a stream needs `property_fields`; non-geo apps use the peer path) — do NOT add payload-key inference.

---

## Phase 2 — Peer-published table resources (Slice 2)

**Goal:** a table a peer publishes over MCP renders in the same panel; sort/filter/paginate client-side.

**Files:**
- `src/services/table_panels.rs` — **modify**. Add the `peer_tables` fan-out: mirror `chart_panels.rs::fetch_charts_from_peer` — `WorkspacePeerBindingRepo::list_active_peers_for_workspace`, `resources/list` per peer, keep `metadata.ione_view == "table"`, items carry `peer_id`+`uri`, dedup `(peer_id, uri)`, collect `peer_errors`.
- `src/services/table_data.rs` — **create** (near-clone of `chart_data.rs`, but with explicit error mapping #6 and caps #5). `fetch_table_data(http, peer, uri)` → `resources/read` `{params:{uri}}`; enforce **caps before parsing**: response body ≤ 2 MiB (else a `TooLarge` error → 413), then parse `{schema, rows}` with rows ≤ 5000 and columns ≤ 64 (else `TooLarge`), normalize any column missing `type` → `"string"`. **Distinguish errors** so the route maps them: a JSON-RPC `error` whose message indicates unknown/not-found URI → `NotFound`; transport/timeout/other MCP error → `Unavailable`. 5s timeout. (Improves on `chart_data.rs`, which collapses all read failures to `ConnectorError`.)
- `src/routes/table_data.rs` — **create** (mirror `chart_data.rs` route). `GET ?peer_id=&uri=` — both required (400 if either missing); `ensure_workspace_in_org`; look up that `peer_id` among bound peers (404 "peer not bound" if absent); map `fetch_table_data` errors: `NotFound` → 404, `TooLarge` → 413, `Unavailable`/other → 502.
- `src/services/mod.rs`, `src/routes/mod.rs` — **modify**. `pub mod table_data`; register `/api/v1/workspaces/:id/table-data` (get).
- `static/app.js` — **modify**. `tablePanel` lists peer tables alongside IONe (source label); peer items carry `peer_id`+`uri`; on select → fetch `table-data` → cache `{schema, rows}` → render; sort/filter/paginate **re-slice the cache client-side** (no refetch) with a type-aware comparator + case-insensitive substring filter.
- `tests/phase_table_peer.rs` — **create** with a `wiremock` peer (mirror `tests/phase_chart_peer.rs`): AC-8 (`table-panels` lists both; peer items carry `peer_id`+`uri`), AC-9 (`table-data?peer_id=&uri=` parses `{schema, rows}` from `contents[0].text`; omitting `peer_id` → 400), AC-10 (one peer errors on `resources/list` → other returns + `peer_errors`), **AC-10b (over-cap body → 413)**, **AC-10c (error mapping: unbound peer → 404, JSON-RPC unknown-URI → 404, peer timeout/transport → 502)**.
- `tests/e2e/table-panel.spec.ts` — **modify**. AC-12: select a peer table (stub `table-panels` peer section + `table-data`); assert sort/filter happen with **no** `table-data` refetch + correct order/subset.

**Gate:**
```
cargo clippy --all-targets -- -D warnings
DATABASE_URL=postgres://ione:ione@localhost:5433/ione cargo test --test phase_table_peer -- --ignored --test-threads=1
npx playwright test tests/e2e/table-panel.spec.ts
```
**Acceptance:** AC-8, AC-9, AC-10, AC-10b, AC-10c (Rust) + AC-12 (Playwright) pass.

---

## Requirements impact (post-merge, via `update-requirements` at `/preflight`/`/pr`)
Per design § Requirements impact — `app-integration-playbook.md` §4 already corrected (table → v0.1 + `{schema, rows}` body contract, done in the design pass); `ione-substrate.md` remove `table` from v0.1 exclusions; `infrastructure-backlog.md` move the P0 table-view item to done.

---

## Self-review
1. **Every design AC maps to a phase gate?** Yes — AC-1, AC-1b, AC-2..7 + AC-11 → Phase 1; AC-8/9/10/10b/10c + AC-12 → Phase 2; skeleton smoke + arrow-key nav → Phase 0.
2. **Every file exists now or is listed to create?** Verified existing (modify targets): `static/index.html`, `static/app.js`, `static/style.css`, `src/services/event_layers.rs`, `src/repos/stream_event_repo.rs`, `src/repos/mod.rs`, `src/services/mod.rs`, `src/routes/mod.rs`; precedents read (`chart_panels.rs`, `chart_data.rs`, `event_aggregates.rs`, `stream_event_aggregate_repo.rs`). Created: `src/routes/{event_table,table_panels,table_data}.rs`, `src/services/{table_panels,table_data}.rs`, `tests/phase_table.rs`, `tests/phase_table_peer.rs`. `tests/e2e/table-panel.spec.ts` created in Phase 0, extended after.
3. **Vertical slices, not layer stacks?** Phase 0 = scaffolding (≤4 files, no feature). Phase 1 ships the IONe table end-to-end (repo+route+UI+tests incl. the `<table>` render core). Phase 2 ships peer tables end-to-end (reuses the render core).
4. **Gates concrete shell commands?** Yes — named `cargo test --test …`, `npx playwright test -g …`.
5. **Parallel tasks disjoint?** N/A — sequential. Phase 1 & 2 share `table_panels.rs`, `app.js`, `routes/mod.rs`, `services/mod.rs`.

**Implementer notes:** (a) Playwright needs a running server (`IONE_TOKEN_KEY`+`IONE_WEBHOOK_SECRET_KEY`+`IONE_BIND=127.0.0.1:3007 cargo run`); the table e2e stubs `table-panels`/`event-table`/`table-data` at the network layer (no DB/peer seeding for AC-11/12). (b) DB tests need `--test-threads=1`. (c) `sort_by`/`filter_col` must be resolved to a `SortTarget` enum against the known column allow-list **before** SQL build; pointer paths bound as `text[]` — never interpolate column/pointer strings (the sole injection surface, same discipline as `numeric_agg_by_bucket`). (d) filter is ILIKE-substring for property fields (avoids JSONB cast failure); sort is numeric-aware via `jsonb_typeof='number'` CASE.
