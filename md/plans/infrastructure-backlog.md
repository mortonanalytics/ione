# IONe Infrastructure Backlog

What it would take to move IONe from a v0.1 integration fabric to a more complete substrate for hosting real data apps. Prioritized. Items tagged **[Epicenter]** are needed by the seismic-monitor demo (`../../epicenter`), which is the current demand signal driving this list.

Effort estimates are rough (solo-dev days). File refs point at where the work likely lands.

---

## P0 — Visualization (the biggest gap; unlocks every data app)

IONe renders MapLibre tiles and nothing else today. No chart, table, or live-feature rendering. This is the wall every data app hits.

- **[Epicenter] Chart panel — `ione_view:"chart"` rendering myIO.** _The load-bearing item._
  - Wire myIO's framework-agnostic D3 engine (the same `myIOapi.js` that backs the R/Python widgets) into the static UI — no SPA framework required.
  - Consume the published chart contract `application/vnd.ione.chart+json` (`chart_type / x_axis / y_axis / series`, see `md/design/app-integration-playbook.md`) and map it to a myIO spec `{ type, mapping, transform }`.
  - Validate specs via the myIO MCP `validate_spec` tool before render.
  - **Known dependency bug:** `validate_spec` rejects valid single-mapping charts (histogram/gauge/qq) — it iterates a scalar `required_mappings` string as characters. Fix is in myIO (`mcp/lib/validate.mjs` + schema generator); the panel must tolerate/route around it until fixed. (Found 2026-05-27 while validating Epicenter specs.)
  - Effort: ~1 wk.

- **[Epicenter] Live point/feature map layer from `stream_events`.**
  - Today the map (`src/routes/map_layers.rs`, `src/services/map_layers.rs`) only passes through peer-published tile/vector URLs; it cannot render ingested point events.
  - Add a "workspace events → GeoJSON source" endpoint + a MapLibre point layer (markers sized/colored by an event field, e.g. magnitude/depth).
  - Effort: ~1–2 d.

- **Table view — `ione_view:"table"`.** Schema negotiation, pagination, column filter/sort. Effort: ~3–4 d.

- **Document/report view — `ione_view:"document"`.** Render linked PDFs/reports in-app instead of just linking out. Effort: ~2–3 d.

---

## P1 — Ingestion

- **[Epicenter] Generic `geojson_poll` / JSON-URL connector.**
  - Connectors today are hand-wired (NWS / FIRMS / IRWIN) or the OpenAPI auto-adapter. Static GeoJSON/JSON feeds (e.g. USGS summary feeds) are neither — they need per-source hand-wiring.
  - Add a config-driven connector: poll a URL, map fields (JSON-pointer), dedup key, type filter → `stream_events`. Removes most per-source code.
  - Effort: ~1–2 d.

- **MCP `notifications/*` reception.** Webhook push is the only v0.1 ingest path for peers; accept MCP notifications too. Deferred from v0.1. Effort: ~3 d.

- **[Epicenter] Confirm/extend rules-engine nested-field reach.** Verify `src/services/rules.rs` expressions can resolve arbitrary-depth JSON pointers into connector payloads (e.g. `[/properties/mag]`), not just the documented `[/events/0/severity]` shape. Epicenter's M≥6.0 rule depends on it. Effort: verify + ~1 d if extension needed.

---

## P2 — Analytics primitives

- **[Epicenter] Windowed / grouped aggregates.** Count-per-interval, group-by-region, percentile, and rolling baselines — so charts can show trends and "is this unusual" without each app pre-aggregating. Epicenter's frequency timeline, region breakdown, and 30-day baseline all need this. Effort: ~3–5 d.

---

## P3 — Federation maturity (from `md/design/`)

- **Tool namespacing in the federation hub.** Single namespace today; two peers exporting `query_data` collide. Effort: ~2–3 d.
- **Context-slice lazy expansion (`slice://`).** Contract is published (apps ship slices) but IONe-side routing/expansion isn't built. Effort: ~3 d.
- **Cross-app semantic catalog + vector search** over peer resources/tool descriptions (pgvector already present). Effort: ~1 wk.

---

## P4 — Identity & governance

- **SAML 2.0 SP** for enterprise SSO (Keycloak bridges SAML→OIDC for now). Deferred from v0.1. Effort: ~3–5 d.
- **Auto-exec policy DSL.** Today: human-approval only. Add conditional auto-execution policies for low-risk tools. Effort: ~3–4 d.
- **Audit the auto-exec bypass guard.** Confirm the router's force-to-draft on `approval_required` (`src/services/router.rs`) is not bypassable. Effort: ~0.5 d review.

---

## P5 — UX / product polish

- **UI theming hooks.** The static HTML+JS UI is intentionally lightweight. To host product-grade demos (e.g. Epicenter's ops-console theme), define a token/theming layer or commit to a SPA upgrade path. Decide before investing in per-app CSS. Effort: ~2–4 d for a theming layer.
- **Connector setup + signal/approval timeline polish.** Incremental.

---

## Out of scope (noted, not planned)

- **Multi-tenant hosted SaaS tier.** Per the pricing strategy, gated behind 3 unsolicited asks + hire #2. IONe stays self-hosted-per-org until then.

---

_Created 2026-05-27 while scaffolding the Epicenter demo. The P0 visualization items are the difference between "IONe federates apps" and "IONe hosts apps" — and they pay off for every future app, not just this one._
