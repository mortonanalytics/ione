# IONe — Integration Fabric Substrate

**Date:** 2026-05-12
**Status:** North-star architecture doc. Canonical reference for all design work in this repo.
**Supersedes (in framing, not in detail):** [md/design/ione-v1.md](ione-v1.md), [md/design/ione-complete.md](ione-complete.md). Those documents capture the *application-shaped* IONe v0.1 design. This document re-frames IONe as integration fabric. Where the older docs conflict with this one, this one wins.

## Thesis

**IONe is integration fabric, not a hosting platform and not a standalone chat-first workspace product.** Its job is to be the connective tissue that makes a heterogeneous portfolio of client apps look like one workspace to an operator. The apps stay independent; IONe federates over MCP + brokered identity + approval/audit gateway + a thin UX shell.

Every architectural decision in this repo answers to one question: *does this help IONe federate to three different polyglot apps owned by different teams, or does it only make IONe better as a standalone product?* If only the latter, defer.

## Reference apps

Three Morton Analytics client apps stress-test the substrate spec. They share no implementation; they share an operator-facing surface.

| | GroundPulse (`../eo/`) | TerraYield (`../eo_ag/`) | bearingLineDash (`../bearingLineDash/`) |
|---|---|---|---|
| Language | Rust / Axum | Rust / Axum | Python / Shiny |
| DB | Postgres + PostGIS | Postgres + PostGIS + TimescaleDB | Postgres (ADBC) |
| Queue | Postgres SKIP LOCKED | Redis Streams | none |
| Storage | S3 (COG/PMTiles) | S3 (COG/PMTiles) | n/a |
| External data | OPERA STAC, NBI | Element84, Planetary Computer, NASA CMR | QuickBooks OAuth, Google Sheets |
| Identity | API keys + sessions | SAML 2.0 SP (USDA eAuth) + TOTP MFA | SaaS OAuth |
| Frontend | own UI | React/Vite/Tailwind/MapLibre | Shiny |
| Compliance | none | FedRAMP Moderate target | none |
| Domain | infrastructure risk (raster) | crop health (raster + time-series) | financial analytics (tabular) |

If a substrate decision wouldn't generalize across these three, it's wrong.

## The seven substrate layers

### 1. MCP federation hub
Already partially built. IONe is the MCP crossbar: connects to N upstream MCP servers (the client apps), aggregates their `tools/list` and `resources/list`, exposes a unified surface to chat and to UI.

Required maturation:
- Tool namespacing (`gp:query_displacement` vs `ty:fetch_ndvi`) at the federation boundary
- Conflict resolution policy for collisions
- Per-peer rate limiting + circuit breakers
- Resource browsing protocol (operator can enumerate what each peer offers)
- **Context slices for federation discovery** (see below)

Lives in: [src/connectors/mcp_client.rs](../../src/connectors/mcp_client.rs), [src/peers/](../../src/peers/), [src/mcp_server.rs](../../src/mcp_server.rs).

#### Context slices for federation discovery

**Problem.** As IONe federates to N peers with M tools each, the chat context fills with tool definitions. Realistic scale (3 apps × 20–30 tools × 200–800 tokens each) = 15k–70k tokens of tool definitions in every chat request before the user's query. Tool-definition tokens dominate the system prompt; cost and latency scale linearly with peer count.

**Approach.** Each peer publishes a compact **context slice** (~100–500 tokens) instead of forcing IONe to ship every full tool definition into the model's context up front. The slice is enough to route — the model picks a peer + intent — and IONe expands the relevant tool definitions on demand for the second turn.

A slice contains:
- `summary` — what the peer does, one paragraph (~100 tokens)
- `domain_tags` — `["geospatial", "time-series", "alerts", "ag", "financial", ...]`
- `sample_queries` — 3–5 representative natural-language queries this peer can serve
- `tool_index` — list of `{name, one_sentence_description, expand_uri}` (no full schema)
- `resource_hints` — example resource URIs, schemas, recent-activity summary

IONe injects all peer slices into the system prompt (~2–5k tokens total at peer count 5–10, vs 15–70k for full tool defs). When the model selects a tool, IONe fetches the full `inputSchema` via `tools/get` (or `resources/read expand_uri`) and continues. Tool discovery is lazy and hierarchical; token cost is bounded by `O(peers)` not `O(peers × tools)`.

**v0.1 contract scope:** apps must publish slices (in the app integration playbook). Without this, every app needs to retrofit later.

**v0.2 implementation scope:** IONe-side routing/expansion logic — slice aggregation in the federation hub, model-side prompt economy, lazy `tools/get` expansion, optional vector index over tool descriptions for retrieval-based filtering at very high peer counts.

Design doc to follow: `md/design/mcp-context-slices.md` (deferred until identity broker lands).

### 2. Identity broker
The deepest gap. IONe does not replace each app's IdP needs — it brokers. A single operator authenticates to IONe; IONe holds delegated credentials per app and presents them when invoking app tools.

Required surface:
- OIDC consumer (IONe consumes one identity from a corporate IdP)
- SAML 2.0 SP (USDA eAuth and equivalents)
- Brokered SaaS OAuth dance (QuickBooks, Google Workspace, Slack admin, etc.) with token refresh
- TOTP and WebAuthn MFA at the broker layer
- Claim mapping (`ione_user → {peer_id, foreign_user_id, foreign_role}`)

This is the highest-effort layer and the one that fundamentally cannot live in any of the apps.

### 3. Approval and audit gateway
Mostly built — the [signals → survivors → approvals → audit](../../src/) chain is IONe's differentiator. Apps emit "I want to do X" via MCP notifications or webhooks; IONe gates with human-in-the-loop and writes an audit row on every action.

Gaps to close:
- First-class push ingress (today IONe pulls via connector polling — see layer 5)
- App-declared approval requirements: an app's MCP manifest indicates which tools require approval gating, which severities, which roles can approve
- Audit retention as a substrate concern (compliance regimes vary per deployment)

### 4. Workspace UX shell with pluggable view types
IONe is the operator's pane of glass. Today only chat + opaque cards + connector-shaped lists. To embed three heterogeneous apps the shell needs generic primitives each app fills.

Required view types, in priority order:
- **Map view** — renders tile URLs from MCP resource metadata. Apps own their tile servers; IONe embeds. Critical for both EO apps.
- **Table view** — renders typed rows from MCP resources
- **Chart view** — time-series resources (line, bar, distribution)
- **Document view** — PDFs, reports, evidence packages

Each view consumes a declared MCP resource shape. **IONe does not host the tiles, the rasters, the PDFs, the underlying data.** It renders references.

### 5. Push event ingress
Today IONe pulls via connector polling on `IONE_POLL_INTERVAL_SECS`. With three apps emitting alerts, anomalies, and lifecycle events on their own schedules, push has to be first-class.

Required surface:
- Signed webhook endpoint per peer (HMAC-SHA256 over canonical envelope)
- MCP `notifications` reception
- Fan-in into `pipeline_events` → signals → survivors → approvals
- Replay protection (nonce + timestamp window)

### 6. Cross-app workspace context
Small but load-bearing. A single operator workspace in IONe maps to a tenant in each connected app. Today there is no schema for this mapping. Without it the "one pane of glass" promise collapses on the first multi-app demo.

Required schema (in IONe):
- `workspace_peer_bindings(workspace_id, peer_id, foreign_tenant_id, foreign_workspace_id, scope, created_at)`
- Convention: every peer MCP server exposes a `whoami` or `context` resource returning the foreign tenant identity for the brokered user

### 7. Federated catalog / search
Operator says "show me everything anomalous this week" → IONe broadcasts to connected peers and aggregates. Needs a thin protocol on top of MCP `tools/list`: convention for `find_anomalies(since)`, `list_alerts`, `recent_activity` across apps.

**Defer until peer count > 3.** Mentioned here so the roadmap shape is visible.

## What does NOT belong in IONe

Listed explicitly because the previous design framing implied these. Under the substrate thesis, each is an app concern, not an IONe concern.

- PostGIS, TimescaleDB, or any database extension serving a specific app's data model
- Background task queues for app workloads (Redis Streams, SKIP LOCKED, etc.)
- Tile servers, COG/PMTiles hosting, raster compute
- Format-aware exporters (GeoJSON, Shapefile, GeoPackage, PDF reports)
- Compute observability *of remote apps* (apps own their ops)
- Schema modules / hosting plugins for app code
- Long-running scientific compute orchestration

IONe ships an MCP federation hub, an identity broker, a webhook receiver, a thin generic UI shell, and the approval/audit chain. That's the substrate.

## v0.1 table stakes

Re-derived from the substrate thesis. These move from "v0.2 candidate" (under the application framing) to "required for v0.1" (under the substrate framing):

1. **Identity broker** — at minimum: OIDC consumer + SAML 2.0 SP + brokered SaaS OAuth for one external provider + TOTP MFA. Without this, no realistic client engagement can land.
2. **Cross-app workspace context** — `workspace_peer_bindings` schema + foreign-tenant `whoami` convention.
3. **Signed webhook ingress** — receiver endpoint, HMAC verification, replay protection, fan-in into `pipeline_events`.
4. **Tile-URL passthrough + generic map view** — both EO apps need this day one. MapLibre embed, reads tile URL from MCP resource metadata.
5. **Tool namespacing in the federation hub** — required at peer count > 1.
6. **Context slice contract** — apps must publish a context slice (`slice://` resource) in v0.1 so token-efficient discovery is possible without per-app retrofit. IONe-side routing logic is v0.2.
7. **App integration playbook** — the contract apps follow to plug in. Lives at [md/design/app-integration-playbook.md](app-integration-playbook.md).

What was previously in v0.1 scope (chat-first onboarding polish, demo workspace canned chat, activation funnel telemetry) is **application-layer**, not substrate. It stays in IONe's reference UI but is no longer load-bearing for the v0.1 thesis.

## What v0.1 explicitly does NOT include

- Federated catalog / search (layer 7)
- Pluggable table / chart / document view types (start with map; iterate in v0.2)
- Compliance posture profiles (FedRAMP / FIPS / SOC 2 templating)
- Cost / resource accounting per workspace
- Multi-region deployment patterns
- Streaming chat SSE
- Vector-backed semantic search across the federation

## Cross-references

- [.claude/rules/path-2-stream-p.md](../../.claude/rules/path-2-stream-p.md) — Path 2 positioning rule. Substrate framing is the portfolio-wide generalization of the original GP-specific framing.
- [md/design/app-integration-playbook.md](app-integration-playbook.md) — contract for client apps plugging into IONe.
- [md/strategy/market/ione-pricing.md](../strategy/market/ione-pricing.md) — pricing strategy (substrate framing should not require re-derivation, but sanity-check on next review).
- [md/strategy/market/ione-chat-first-data-ias.md](../strategy/market/ione-chat-first-data-ias.md) — earlier external positioning under the chat-first framing. Needs amendment or supersession (Tier 2 work).
- [md/strategy/competitive/ione-chat-first-iaas-landscape.md](../strategy/competitive/ione-chat-first-iaas-landscape.md) — competitive landscape under the chat-first framing. Comp set shifts to iPaaS (Zapier/Workato/n8n) + agent platforms + data-mesh tools.
- `../morton-analytics-web/md/strategy/path-2-90day-plan.md` — Path 2 90-day plan. Outcome P7 (v0.1 OSS release date locked) is unchanged; *what's in v0.1* changes per the table-stakes list above.

## Decision log

| Date | Decision | Rationale |
|---|---|---|
| 2026-04-19 | IONe framed as chat-first federated workspace product | Captured in [ione-v1.md](ione-v1.md). v0.1 effort followed this framing. |
| 2026-05-12 | Reframed as integration fabric for Morton's polyglot client-app portfolio | Three reference apps (GP, TerraYield, bearingLineDash) share no implementation. A hosting/module-system architecture cannot reconcile polyglot reality without becoming a kubernetes-shaped abstraction. Federation over MCP + brokered identity + approval gateway is the achievable spec. |
