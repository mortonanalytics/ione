# Building an App on IONe — Playbook for AI Coding Agents

**Date:** 2026-05-31
**Audience:** AI coding agents (and humans) scaffolding a new app that renders inside an IONe workspace.
**Status:** Working playbook. Authoritative contract is [app-integration-playbook.md](../design/app-integration-playbook.md); lifecycle is [app-integration-state-machine.md](../design/app-integration-state-machine.md).

> Read this first, then the contract. This tells you *which* path to take and *in what order*; the contract is the exact wire format.

---

## 0. The one decision that determines everything

Classify the app before writing any code:

```
Does the app need to push live, stateful, approval-gated actions back into operations
(acknowledge alert, trigger ingest, generate a report on demand)?
        |                                            |
       YES                                          NO  (it shows precomputed / read-only results)
        v                                            v
  FEDERATION PEER                            ARTIFACT DASHBOARD
  (GroundPulse, TerraYield)                  (doi-ss-ping, doi-reclamation)
  -> Section A (heavy on-ramp)               -> Section B (fast on-ramp)
```

Most demos, RFI/Sources-Sought responses, and proofs-of-capability are **Artifact Dashboards**. Default to Section B. Only choose Section A when the app genuinely owns live state IONe must act on. **Do not implement OAuth, MCP, or webhooks for a read-only demo** — it is pure tax.

If unsure: build Section B first (you get a working IONe-rendered app in hours), then graduate to Section A later. doi-reclamation is the canonical "started B, can grow to A" example.

### A third relationship: *consume* IONe over MCP

Beyond plugging *into* IONe (peer or artifact), your agent or app can **consume IONe's federated surface as an MCP client** — connect to IONe's `/mcp` server and call a unified `tools/list` across every connected peer, `tools/call` (routed to the owning peer, approval-gated where required), `resources/read`, and the aggregated `slice://`. This makes IONe-as-tool a peer to *your* agent, the mirror of the web shell. See [mcp-federation.md](../design/mcp-federation.md) "Consumption surfaces." Caveat: until the federation slice ships, IONe's `tools/list` is a hardcoded stub that does not reflect connected peers — don't rely on it for federated discovery yet.

---

## Section A — Federation Peer (live app)

Your app stays opinionated about its own DB, compute, and frontend. IONe federates over MCP + signed webhooks + brokered identity. You expose **six surfaces** (full spec in the contract doc):

1. **MCP server** at a stable URL (`/mcp`), HTTP+SSE: `initialize`, `tools/list`, `resources/list`, `tools/call`, `resources/read`, `notifications/*`.
2. **OAuth 2.1 AS** (PKCE): `/.well-known/oauth-authorization-server`, `/oauth/authorize`, `/oauth/token` (refresh), `/oauth/revoke`. Front your existing SSO here if you have one.
3. **Signed webhook sender** (optional): POST events to `{ione}/webhooks/peer/{peer_id}` with `X-IONe-Signature: t=<ts>,v1=<hmac>`. Envelope in the contract.
4. **Resource view metadata** — every `resources/list` entry carries `metadata.ione_view` so the shell renders it (see §C below).
5. **Context slice** (`slice://`) — ~100–500 token capability summary so federation stays cheap. Hand-writable in an hour.
6. **`whoami` resource** — returns foreign tenant + user identity; populates `workspace_peer_bindings`.

**Build order for an agent:**
1. `resources/list` + `resources/read` returning view-tagged resources (§C). This alone makes data render.
2. `initialize` + capability negotiation.
3. `tools/list` (mark state-changing tools `ione_approval.required = true`).
4. OAuth 2.1 AS (or front existing SSO).
5. `slice://` + `whoami` + `roles://`.
6. Webhook sender last (only if you push events).

**Verify:** register via `POST /api/v1/peers`, run the OAuth dance, confirm IONe pulls your manifest and your resources appear as panels. Reference shapes: `src/oauth/`, `src/mcp_server.rs`, `src/connectors/mcp_client.rs`.

---

## Section B — Artifact Dashboard (read-only / demo)

Goal: get precomputed data rendering in IONe panels with **no server, no OAuth, no MCP**. You produce a data artifact + a `view_config`; the **artifact connector** (see on-ramp plan, ONR-001) loads it into `stream_events`, and the shell renders it via Machine 3.

**Steps for an agent:**
1. **Compute your artifact offline** — exactly as the demos do (Python pipeline → CSV / Parquet / GeoJSON). Keep it tidy-long: one row per observation, typed columns. **Give every row a stable `dedup_key`** (a content hash, or a natural key like `county+date+metric`) — without it, rows sharing a timestamp are silently dropped on ingest (the connector upserts on `(stream_id, dedup_key)`).
2. **Write a `view_config`** mapping artifact fields to panels using RFC 6901 JSON Pointers (§C). One artifact can drive chart + table + map.
3. **Load it**: `POST /api/v1/connectors/validate` (dry-run the schema/pointers), then the artifact connector creates a `stream` + `stream_events`.
4. **Attach to a workspace** and open it. The panels summary picks up the new charts/tables; the shell renders them.

**Authenticated sources:** if your artifact or feed comes from a gated URL (API key, basic auth, OAuth), you need connector credentials — see [connector-auth-plan.md](../plans/connector-auth-plan.md). Public sources skip this. OAuth-delegated sources (QuickBooks, Google, agency portals) reuse the same dance as peer federation; do not hand-roll token handling.

**What you inherit for free:** the chart engine (myIO), HTML tables with sort/filter/paginate, the map shell (MapLibre + event-layers), 508/WCAG-compliant chrome, adaptive navigation. This is the work doi-ss-ping and doi-reclamation each rebuilt from scratch.

> **Two current limits for the artifact path (being fixed in the on-ramp plan):**
> 1. **Maps need a panels-contract fix (ONR-001a/004).** The map tab today only appears when a live peer is bound (`panels.hasActivePeer`); the workspace summary has no native map/event-layer count. Until ONR-004 lands, an artifact-only workspace's event layers will render data but the **map tab may stay hidden**. Charts and tables are unaffected.
> 2. **Native charts are line/aggregate only (ONR-005).** The native stream path generates aggregate (line) panels from `view_config.property_fields`; it does **not** yet render arbitrary chart specs or **gauges**. For a gauge or a specific chart spec today, use the peer `resources/read` chart resource path; the artifact path gains arbitrary specs via the planned `view_config.charts[]` contract.

**When you have a thin server already** (doi-reclamation's Axum/Parquet case): you may instead wrap it as a minimal MCP server exposing only `resources/list`/`resources/read` (Section A steps 1–2, skip OAuth in dev via `IONE_OAUTH_STATIC_BEARER`). Choose this only if the data must stay live; otherwise the artifact load is less code.

---

## Section C — View metadata (shared by both paths)

A datum lands in a panel **only** if it is either (1) a `stream_event` with a `view_config`, or (2) an MCP resource with `ione_view` metadata. Same four view types either way.

**Render vs ingest (do not confuse these):** peer `resources/list`/`resources/read` **render directly** — they are fetched when the operator opens a panel and are *not* persisted, signaled, approved, or audited. Only **webhook push** and **connector polling** (including the MCP connector calling readable `tools/call`) write `stream_events` and flow through the processing pipeline (Machine 2). If you want a datum to raise a signal / approval / audit trail, push it or expose it as a pollable tool — publishing it as a resource only makes it *viewable*.

| `ione_view` | Peer resource body (`resources/read`) | Artifact `view_config` (stream_events) |
|-------------|----------------------------------------|----------------------------------------|
| `chart` | `{spec:{chart_type,x_axis,y_axis,series}, rows:[...]}` | `property_fields:[{pointer,name}]` (numeric, time-series) |
| `table` | `{schema:[{name,type}], rows:[...]}` | `property_fields:[{pointer,name}]` (columns) |
| `map` | metadata `tile_url`(XYZ),`bounds`,`attribution` | `geo_pointer`/`lon_pointer`/`lat_pointer`,`style` |
| `document` | metadata `download_url`(https),`mime_type` | n/a (peer path only) |

`chart_type ∈ {line, bar, area, scatter, histogram, gauge, qq}` — but note: the **peer `resources/read` path supports all of these today; the artifact `view_config` path natively renders line/aggregate panels only** (gauges and arbitrary specs await `view_config.charts[]`, ONR-005). Choose the peer path if you need a gauge now.

**Hard constraints (do not fight these):**
- IONe **never proxies** tiles or documents. `tile_url` and `download_url` must be **browser-reachable https**, valid ≥5 min, no IONe-injected auth. Static demos: host overlays as public/presigned URLs (or use static-overlay support, ONR-004, once landed).
- Tables are server-paginated for the stream path, client-paginated for the peer path.
- A resource with no `ione_view` renders as an opaque reference (name + description only).

---

## Section D — Local dev & verification

- `IONE_SEED_DEMO=1` boots a read-only demo workspace + the **loopback mock peer** (`POST /demo/mcp`). Study it — it is the smallest working example of feeding the shell without a remote service, and is the seed of the embedded-app SDK (ONR-002).
- `IONE_OAUTH_STATIC_BEARER` bypasses the OAuth dance in CI / local while testing MCP.
- Bring-up: `docker compose up -d postgres minio && cargo sqlx migrate run && cargo run --release`.
- **Definition of done for an app:** open its workspace in a browser, confirm every intended panel renders real data, and (Section A only) confirm one approval-gated tool round-trips through Machine 2. Grep-confirmed endpoints are not "done" — a founder walkthrough is.

---

## Section E — Anti-patterns (what NOT to do)

1. **Don't route a read-only demo through the peer contract.** OAuth + MCP for a static dashboard is days of tax for zero federation benefit.
2. **Don't ask IONe to host your data, DB, queue, tiles, or compute.** It won't. Apps own those.
3. **Don't expect a "draw this JSON" endpoint.** Produce `stream_events`+`view_config` or an `ione_view` resource. Nothing else renders.
4. **Don't try to de-escalate approvals** from the app. The human gate is non-bypassable.
5. **Don't bake app-specific rendering into IONe.** If a view need doesn't generalize across GroundPulse + TerraYield + a demo, it belongs in your app, not the shell.
6. **Don't proxy-expect tiles/documents.** Provide reachable https URLs.

---

## Quick reference — which path, which effort

| If the app is… | Path | Surfaces to build | First working render |
|----------------|------|-------------------|----------------------|
| Static site (doi-ss-ping) | B (artifact) | view_config only | hours |
| Stateless file server (doi-reclamation) | B, or thin MCP | view_config, or resources/* | hours / a day |
| Live DB-backed product (GroundPulse) | A (peer) | all six | days |
| External-token app (bearingLineDash) | A, after a **service wrapper** | build a server first, then resources/* + slice | not "thin" — it is OAuth-*client* only today and exposes no service, so it cannot start from resources metadata alone |
