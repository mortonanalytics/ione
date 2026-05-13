# IONe App Integration Playbook

**Date:** 2026-05-12
**Status:** Contract document for client apps plugging into IONe.
**Audience:** Morton Analytics internal developers building client apps (GroundPulse, TerraYield, bearingLineDash, future apps); external OSS developers federating their own apps to an IONe deployment.
**Parent:** [md/design/ione-substrate.md](ione-substrate.md)

## What this document is

The contract a client app must satisfy to plug into IONe as a federated peer. If an app meets this contract, an IONe operator can: authenticate via IONe's identity broker; see the app's data and resources in the unified workspace UI; invoke the app's tools (gated by IONe's approval chain); receive the app's push events as signals.

The contract is intentionally minimal. Apps stay opinionated about their own DB, queue, compute, tile serving, file formats, and frontend. IONe federates over MCP + signed webhooks + brokered identity. Nothing more.

## The five surfaces an app exposes

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

### 3. Webhook receiver registration + push events

Apps that produce events (alerts, lifecycle changes, anomalies) push them to IONe rather than relying on poll. The flow:

- IONe registers a webhook endpoint with the app at federation time: `POST {app}/api/webhooks/register` with `{target_url, signing_secret, event_types[]}`.
- The app POSTs events to `target_url` with header `X-IONe-Signature: t=<unix_ts>,v1=<hmac_sha256_hex>`.
- Signature payload: `f"{t}.{request_body}"`, key = `signing_secret`.
- Body envelope:

```json
{
  "id": "uuid-v7",
  "type": "alert.created | alert.acknowledged | task.completed | resource.updated | ...",
  "occurred_at": "2026-05-12T10:30:00Z",
  "peer_id": "groundpulse-prod",
  "foreign_tenant_id": "tenant-abc",
  "data": { ... },
  "approval_required": false
}
```

- `approval_required: true` routes the event through IONe's approval gateway; the operator must approve before any downstream IONe-side action.
- Replay protection: IONe rejects events with `occurred_at` outside a ±5 minute window or duplicate `id`.

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
- `map` — `metadata.tile_url` (XYZ template), `metadata.bounds`, `metadata.attribution`. Optionally `vector_url` for PMTiles.
- `chart` (deferred to v0.2) — `chart_type`, axis hints, series list
- `table` (deferred to v0.2) — column schema
- `document` (deferred to v0.2) — `metadata.download_url`, MIME type

Apps may include resources with no `ione_view`; IONe surfaces them as opaque references with name + description only.

### 5. Foreign-tenant `whoami` resource

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

1. **App side** — bring up MCP server + OAuth 2.1 + webhook receiver registration endpoint + resource `whoami`. Generate a peer client registration.
2. **IONe side** — operator goes to Peers tab → "Federate new peer" → enters peer name + MCP URL → IONe initiates OAuth dance → operator authorizes via the app's IdP.
3. **Manifest fetch** — IONe pulls `tools/list` + `resources/list` + `roles://`. Operator reviews tool allowlist, approves.
4. **Webhook registration** — IONe registers its webhook receiver URL with the app, exchanges signing secret.
5. **Workspace binding** — IONe writes `workspace_peer_bindings` row with foreign tenant from `whoami`.
6. **Live** — app tools appear in chat / shell, push events arrive as signals, operator approvals are routed.

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
