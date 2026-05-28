# IONe App Integration Playbook

**Date:** 2026-05-12
**Status:** Contract document for client apps plugging into IONe.
**Audience:** Morton Analytics internal developers building client apps (GroundPulse, TerraYield, bearingLineDash, future apps); external OSS developers federating their own apps to an IONe deployment.
**Parent:** [md/design/ione-substrate.md](ione-substrate.md)

## What this document is

The contract a client app must satisfy to plug into IONe as a federated peer. If an app meets this contract, an IONe operator can: authenticate via IONe's identity broker; see the app's data and resources in the unified workspace UI; invoke the app's tools (gated by IONe's approval chain); receive the app's push events as signals.

The contract is intentionally minimal. Apps stay opinionated about their own DB, queue, compute, tile serving, file formats, and frontend. IONe federates over MCP + signed webhooks + brokered identity. Nothing more.

## The six surfaces an app exposes

### 1. MCP server endpoint

An app exposes an MCP server at a stable URL (typically `/mcp` on the app's API). The server speaks MCP over HTTP+SSE per the 2025-06 spec.

Required methods:
- `initialize` — capability negotiation
- `tools/list` — enumerate callable tools
- `resources/list` — enumerate browsable resources (with view-hint metadata; see surface 4)
- `tools/call` — invoke a tool
- `resources/read` — fetch a resource
- `notifications/*` — emit lifecycle notifications (see surface 3)

Optional but encouraged:
- `prompts/list` and `prompts/get` for shipping starter prompts to operators
- `completion/complete` for tool-argument suggestions

### 2. OAuth 2.1 authorization server

The app's MCP server is gated by OAuth 2.1 (PKCE-required). IONe holds delegated tokens per `(workspace, peer)` and refreshes them automatically. The app exposes:

- `/.well-known/oauth-authorization-server` — discovery metadata
- `/oauth/authorize` — authorization endpoint
- `/oauth/token` — token endpoint (supports `refresh_token` grant)
- `/oauth/revoke` — token revocation

For apps already using a corporate SSO (USDA eAuth via SAML, etc.), the OAuth surface fronts the SSO. The app handles the SAML/OIDC details internally; IONe consumes the OAuth 2.1 surface.

Reference: IONe's own implementation at [src/oauth/](../../src/oauth/) — apps can mirror its shape.

### 3. Signed webhook push events

Apps that produce events (alerts, lifecycle changes, anomalies) push them to IONe rather than relying on poll. The flow:

**Provisioning (v0.1 — manual).** The IONe operator calls `POST /api/v1/peers/:id/webhook/provision` and receives a one-time `signingSecret` plus the per-peer `webhookUrl` (`{ione}/webhooks/peer/{peer_id}`). The operator pastes both into the app's webhook config. The secret is shown once; re-provisioning rotates it. *Automatic registration (IONe POSTing `{app}/api/webhooks/register`) is deferred to v0.2 — do not implement an app-side registration endpoint for v0.1.*

**Sending events.** The app POSTs each event to its `webhookUrl` with header:

```
X-IONe-Signature: t=<unix_ts>,v1=<hmac_sha256_hex>
```

- The peer is identified by the `peer_id` in the **URL path** (it selects which signing secret IONe verifies against); the HMAC is the authentication. The body's `peer_id` must equal the path `peer_id`.
- Signature: `HMAC-SHA256` over the **bytes** `t.as_bytes() ++ b"." ++ raw_body_bytes`, key = `signingSecret`. Sign the raw bytes, not a re-serialized body.
- Body envelope (snake_case):

```json
{
  "id": "uuid-v7",
  "type": "alert.created | alert.acknowledged | task.completed | resource.updated | ...",
  "occurred_at": "2026-05-12T10:30:00Z",
  "peer_id": "<uuid matching the URL path>",
  "foreign_tenant_id": "tenant-abc",
  "severity": "routine | flagged | command",
  "data": { ... },
  "approval_required": false
}
```

- `severity` is a **top-level** field mapping to IONe's signal severity. Omitted/unknown ⇒ `routine`.
- `approval_required: true` routes the event through IONe's approval gateway before any downstream IONe-side action. **IONe enforces its own policy floor: the flag may *escalate* but never *de-escalate*.** A `severity` of `flagged` or `command` is always gated (and bypasses auto-exec) regardless of the flag — apps cannot disable the human gate.
- Constraints: body ≤ 256 KB (else 413); `type` matches `^[a-z0-9._/-]{1,255}$`; `foreign_tenant_id` ≤ 512 chars; `data` must be a JSON object. Events route only to **active** `workspace_peer_bindings` for `(peer_id, foreign_tenant_id)`; no matching binding ⇒ 400 (no event recorded — safe to retry once the operator adds the binding).
- The peer must be `active`; a revoked/paused peer's events are rejected even with a valid signature.
- Replay protection: reject `occurred_at` outside ±5 min of now; reject if header `t` and `occurred_at` differ by more than 30s; reject a duplicate `(id, peer_id)`. A duplicate returns `200 {"ok":true,"duplicate":true}` (idempotent ACK, not an error).

Apps that don't produce push events skip this surface.

### 4. Resource metadata conventions for the UX shell

Each resource returned by `resources/list` carries view-hint metadata so IONe's UI shell can render it without per-app code:

```json
{
  "uri": "groundpulse://aoi/12345/displacement",
  "name": "AOI 12345 displacement time-series",
  "mimeType": "application/vnd.ione.chart+json",
  "metadata": {
    "ione_view": "chart",
    "chart_type": "line",
    "x_axis": "observation_time",
    "y_axis": "displacement_mm",
    "series": ["mean", "p95"]
  }
}
```

Supported `ione_view` values (v0.1):
- `map` — `metadata.tile_url` (XYZ template), `metadata.bounds`, `metadata.attribution`. Optional fields: `metadata.layer_name`, `metadata.opacity`, `metadata.vector_url`.
- `chart` — `chart_type` (line\|bar\|area\|scatter\|histogram\|gauge\|qq), `x_axis`, `y_axis`, `series[]`. The resource body returned by `resources/read` must have shape `{ spec: { chart_type, x_axis, y_axis, series }, rows: [{ <x_axis>: value, <series[0]>: number, ... }] }`. (Promoted to v0.1 scope per [md/design/chart-panel.md](chart-panel.md).)
- `table` (deferred to v0.2) — column schema
- `document` (deferred to v0.2) — `metadata.download_url`, MIME type

Map resource metadata:

```json
{
  "uri": "gp://aoi/12345/displacement-map",
  "name": "AOI 12345 displacement map",
  "mimeType": "application/vnd.ione.map+json",
  "metadata": {
    "ione_view": "map",
    "tile_url": "https://tiles.example.com/aoi/12345/{z}/{x}/{y}.png",
    "bounds": [-112.75, 45.4, -111.9, 46.1],
    "attribution": "Example Tiles",
    "layer_name": "Displacement",
    "opacity": 0.7,
    "vector_url": "https://tiles.example.com/aoi/12345/displacement.pmtiles"
  }
}
```

Map field requirements:
- `tile_url` is a browser-reachable XYZ raster tile template. IONe does not proxy tile requests in v0.1.
- `bounds` is a flat GeoJSON bbox array: `[west, south, east, north]`.
- `attribution` is displayed as text by the IONe shell; apps must not rely on HTML rendering.
- `layer_name` overrides the resource `name` in map layer controls.
- `opacity` is a float from `0.0` to `1.0`; omitted means fully opaque.
- `vector_url` is pass-through metadata for PMTiles/vector layers; IONe v0.1 does not render it.

Apps may include resources with no `ione_view`; IONe surfaces them as opaque references with name + description only.

### 4b. IONe-ingested event layers (no peer-side contract)

Beyond peer-published map resources, IONe's map shell also renders **events it ingested directly** via its own connectors (FIRMS, IRWIN, NWS, the planned generic `geojson_poll`) into the `stream_events` table. These public feeds have no peer app to publish them as MCP resources, so they are rendered by an IONe-side projection — declarative per-stream config in `streams.view_config` (RFC 6901 JSON Pointers) maps payload fields to geometry and style, served as GeoJSON Point FeatureCollections from `GET /api/v1/workspaces/:id/event-layers`.

This surface adds **no contract obligation on peer apps**. Peer apps remain free to publish vector resources via `resources/list` (`vector_url` is reserved for that future v2 capability); the event-layer surface and the peer-published-vector surface are complementary, not competing.

Full design: [`event-point-layer.md`](event-point-layer.md).

### 5. Context slice resource (`slice://`)

To keep token usage bounded as IONe federates to many peers, every app publishes a single compact **context slice** instead of forcing IONe to ship full `tools/list` output into the chat model's system prompt. The slice describes the peer's capabilities in ~100–500 tokens; IONe expands tool schemas on demand only after the model selects a tool.

The slice is exposed as a well-known resource:

```json
{
  "uri": "slice://",
  "name": "GroundPulse capability slice",
  "mimeType": "application/vnd.ione.slice+json"
}
```

Slice body (returned via `resources/read`):

```json
{
  "schema_version": "1",
  "peer_id": "groundpulse-prod",
  "summary": "Infrastructure risk intelligence for pipeline, bridge, and dam operators. Detects ground displacement via OPERA InSAR satellite data, ranks asset risk, generates compliance reports (API RP 1187, NBIS, FERC).",
  "domain_tags": ["geospatial", "time-series", "infrastructure", "alerts", "compliance", "raster"],
  "sample_queries": [
    "What pipeline segments showed accelerating displacement this quarter?",
    "Show me bridges with critical alerts in Region 4.",
    "Generate an API RP 1187 report for the Permian corridor."
  ],
  "tool_index": [
    {"name": "query_displacement", "summary": "Time-series displacement for a given asset or AOI.", "expand_uri": "tools://query_displacement"},
    {"name": "list_alerts", "summary": "Active alerts filtered by tier, asset, AOI.", "expand_uri": "tools://list_alerts"},
    {"name": "acknowledge_alert", "summary": "Mark an alert as acknowledged. Requires approval.", "expand_uri": "tools://acknowledge_alert", "approval_required": true},
    {"name": "generate_report", "summary": "Generate a compliance or evidence report. Requires approval.", "expand_uri": "tools://generate_report", "approval_required": true}
  ],
  "resource_hints": {
    "example_resources": [
      {"uri_template": "gp://aoi/{aoi_id}/displacement", "description": "Time-series chart for an AOI"},
      {"uri_template": "gp://bridges/{bridge_id}", "description": "Bridge asset detail"}
    ],
    "recent_activity_summary_uri": "gp://activity/recent"
  }
}
```

Field requirements:
- `summary` — one paragraph, target 80–120 tokens
- `domain_tags` — choose from a shared taxonomy (geospatial, time-series, raster, vector, tabular, alerts, compliance, financial, ag, infrastructure, observability, identity, communication). Apps may propose new tags; the federation hub aggregates them.
- `sample_queries` — 3–5 representative natural-language queries the peer can serve. These help the model match user intent to peer.
- `tool_index` — every tool the peer exposes via `tools/list`, with name + 1-sentence summary + `expand_uri` for fetching the full `inputSchema` lazily. The full schema is NOT included in the slice. `approval_required` mirrors the tool's metadata flag.
- `resource_hints` — example resource URI templates and a pointer to a recent-activity summary if the peer publishes one.

Total slice payload should target < 2 KB.

**Why the contract is in v0.1 even though the IONe-side routing logic is v0.2:** apps publish slices once; IONe optimizes when needed. Without the contract in v0.1, every app needs to retrofit later. The slice format is intentionally cheap to produce — apps can hand-write one in an hour.

Every app exposes a `whoami` resource that returns, for the OAuth-authenticated session, the foreign tenant and user identity:

```json
{
  "peer_id": "groundpulse-prod",
  "foreign_tenant_id": "tenant-abc",
  "foreign_tenant_name": "Acme Pipeline Operators",
  "foreign_user_id": "user-xyz",
  "foreign_user_email": "ops@acme.example",
  "foreign_roles": ["member", "alert_acknowledger"]
}
```

This populates `workspace_peer_bindings` on the IONe side and powers cross-app correlation in the operator UI.

## Approval-gated tool declarations

Tools that change state in the app (acknowledge alert, generate report, trigger ingest, etc.) declare an approval requirement in their `tools/list` entry:

```json
{
  "name": "acknowledge_alert",
  "description": "Mark a displacement alert as acknowledged.",
  "inputSchema": { ... },
  "metadata": {
    "ione_approval": {
      "required": true,
      "severity": "info",
      "required_role": "alert_acknowledger"
    }
  }
}
```

When IONe sees `ione_approval.required = true`, it routes any operator invocation through the survivor → approval pipeline. The app receives the `tools/call` only after the operator approves. The app must not perform a state-changing action on first invocation; it should treat the call as authoritative (approval already gated upstream).

Tools without `ione_approval` are treated as read-only and called directly.

## Role mapping declaration

Apps declare their role taxonomy via a static resource at `roles://`:

```json
{
  "roles": [
    {"id": "owner", "label": "Owner", "level": 100},
    {"id": "admin", "label": "Administrator", "level": 80},
    {"id": "member", "label": "Member", "level": 50},
    {"id": "viewer", "label": "Viewer", "level": 10},
    {"id": "api_service", "label": "Service Account", "level": 0}
  ]
}
```

IONe maps its own workspace roles to foreign roles via operator-curated mapping at federation time. The `level` field provides a default ordering for unmapped roles.

## What apps do NOT need to provide

Listed to keep the contract surface bounded:

- A frontend (IONe renders via view hints; the app's own UI is optional and out of scope for the IONe operator surface)
- Tile rendering libraries or map widgets (IONe ships MapLibre; apps expose tile URLs)
- A queue or scheduler IONe can read (IONe doesn't read app queues; apps push via webhooks)
- A database IONe can query directly (IONe never reads an app's DB directly; all access is via MCP)
- An audit log (IONe writes its own audit on every operator action; apps may keep their own audit but IONe doesn't consume it)
- Compliance attestations specific to IONe (apps' compliance posture is their own; IONe inherits the lowest common denominator at deployment time)

## Onboarding a new app to an IONe deployment

The operator-facing flow, end-to-end:

1. **App side** — bring up MCP server + OAuth 2.1 + resource `whoami`, and (if producing events) a webhook **sender** that signs per surface 3. Generate a peer client registration. *(No app-side webhook-registration endpoint is needed in v0.1 — provisioning is manual; see step 4.)*
2. **IONe side** — operator goes to Peers tab → "Federate new peer" → enters peer name + MCP URL → IONe initiates OAuth dance → operator authorizes via the app's IdP.
3. **Manifest fetch** — IONe pulls `tools/list` + `resources/list` + `roles://`. Operator reviews tool allowlist, approves.
4. **Webhook provisioning (v0.1, manual)** — operator calls `POST /api/v1/peers/:id/webhook/provision`, then pastes the returned `signingSecret` + `webhookUrl` into the app's webhook config. (Automatic registration is a v0.2 enhancement.)
5. **Workspace binding** — IONe writes `workspace_peer_bindings` row with foreign tenant from `whoami`.
6. **Live** — app tools appear in chat / shell, push events arrive as signals (gated per `approval_required` + severity), operator approvals are routed back to the peer via outbound MCP tool calls.

## Reference implementations

Pointers, written as stubs until each app actually ships its MCP server:

- **GroundPulse** — `../eo/md/design/ione-mcp-server.md` (stub; create on first MCP-server slice)
- **TerraYield** — `../eo_ag/md/design/ione-mcp-server.md` (stub)
- **bearingLineDash** — deferred until app expands beyond QuickBooks API for data

Each per-app design doc references this playbook as the canonical contract; it specifies *only* what is app-specific (the tool list, resource shapes, role taxonomy, approval-gated actions).

## Versioning

This playbook is versioned in lockstep with IONe's MCP federation surface. The current version is **v0.1-draft (2026-05-12)**. Breaking changes will bump major version; the federation hub will negotiate version compatibility at `initialize` time.

## Cross-references

- [md/design/ione-substrate.md](ione-substrate.md) — the substrate thesis this playbook implements
- [.claude/rules/path-2-stream-p.md](../../.claude/rules/path-2-stream-p.md) — Path 2 positioning
- IONe federation source: [src/peers/](../../src/peers/), [src/oauth/](../../src/oauth/), [src/mcp_server.rs](../../src/mcp_server.rs), [src/connectors/mcp_client.rs](../../src/connectors/mcp_client.rs)
