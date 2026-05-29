# IONe Infrastructure Backlog

What it would take to move IONe from a v0.1 integration fabric to a more complete substrate for hosting real data apps. Prioritized. Items tagged **[Epicenter]** are needed by the seismic-monitor demo (`../../epicenter`), which is the current demand signal driving this list.

Effort estimates are rough (solo-dev days). File refs point at where the work likely lands.

---

## Shipped this cycle — branch `feature/event-layer-phases-2-3` (code-complete + tested; pending founder walkthrough + merge)

These travel together as one unit. Per the "shipped = founder walked through it" rule they are **Partial — pending walkthrough**, not closed.

| Item | Tier | Commits | Design |
|------|------|---------|--------|
| Live point/feature map layer from `stream_events` | P0 | `e0aa8bc` (+ event-layer phases 0–3) | `md/design/event-point-layer.md` |
| Chart panel — `ione_view:"chart"` (myIO) | P0 | `b22f0fa`, `2a40f42` | `md/design/chart-panel.md` |
| Table view — `ione_view:"table"` | P0 | `bcf01b3`, `7a235b7` | `md/design/table-view.md` |
| Generic `geojson_poll` / JSON-URL connector | P1 | `f0ff3e9`, `e239edb` | `md/design/geojson-poll-connector.md` |
| Windowed / grouped aggregates (`event-aggregates`) | P2 | `b22f0fa` | (folded into chart-panel design) |
| Rules-engine nested-field reach | P1 | verified-only (works as-is) | — see note below |

**Remaining P0:** Document view is the only unshipped visualization item.

---

## P0 — Visualization (the biggest gap; unlocks every data app)

IONe renders MapLibre tiles and nothing else today. No chart, table, or live-feature rendering. This is the wall every data app hits.

- ✅ **[Epicenter] Chart panel — `ione_view:"chart"` rendering myIO.** Shipped (`b22f0fa`, `2a40f42`). Dual data path (peer `vnd.ione.chart+json` resources + IONe `event-aggregates`); renders via `new window.myIOchart({config:{layers:[…]}})`. **The single-mapping `validate_spec` bug was confirmed absent in current myIO source** (`required_mappings` is an array for all 36 types) — no bypass needed; validation is a build-time node test against `../myIO/mcp/lib/validate.mjs`, not a runtime call. See `md/design/chart-panel.md`.

- ✅ **[Epicenter] Live point/feature map layer from `stream_events`.** Shipped (`e0aa8bc` + event-layer phases 0–3). `GET /workspaces/:id/event-layers` projects `stream_events` to GeoJSON via `view_config`; MapLibre circle layer. See `md/design/event-point-layer.md`.

- ✅ **Table view — `ione_view:"table"`.** Shipped (`bcf01b3`, `7a235b7`). Schema negotiation, server-side pagination/sort/filter (IONe), client-side (peer); semantic accessible `<table>`. See `md/design/table-view.md`.

- ⬜ **Document/report view — `ione_view:"document"`.** Render linked PDFs/reports in-app instead of just linking out (`metadata.download_url`). The last unshipped P0 visualization item; mostly a peer-resource render path, no aggregate side. Effort: ~2–3 d. **← next.**

---

## P1 — Ingestion

- ✅ **[Epicenter] Generic `geojson_poll` / JSON-URL connector.** Shipped (`f0ff3e9`, `e239edb`). Config-driven poll → JSON-pointer field map → dedup key (natural-key upsert) → type filter → `stream_events`; epoch-ms timestamp support; hardened SSRF guard (link-local blocked all schemes). See `md/design/geojson-poll-connector.md`.

- ⬜ **MCP `notifications/*` reception.** Webhook push is the only v0.1 ingest path for peers; accept MCP notifications too. Deferred from v0.1. Effort: ~3 d. **← next P1.**

- ✅ **[Epicenter] Rules-engine nested-field reach — verified, no code change.** `populate_context` (`src/services/rules.rs`) already recurses objects at arbitrary depth, so a rule `payload.properties.mag >= 6.0` resolves today. Note: rules use **dotted evalexpr** keys (`payload.properties.mag`), NOT the `[/json/pointer]` syntax this item's premise assumed — array indices are not reachable (arrays unmapped), which the M≥6.0 rule doesn't need. _Small open follow-up:_ author the M≥6.0 integration test + correct the playbook's pointer-syntax wording (trivial; not yet done).

---

## P2 — Analytics primitives

- ✅ **[Epicenter] Windowed / grouped aggregates.** Shipped as `GET /workspaces/:id/event-aggregates` (`b22f0fa`): count-per-bucket, avg/min/max/sum, percentile, group-by, 30-day rolling baseline; numeric-aware JSONB extraction, bucket allow-list (injection guard), org-scoped. Backs the chart panel's IONe data path.

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
