# IONe App Integration Contract — v1 (FROZEN)

**Contract version:** `1`
**Freeze date:** 2026-07-25
**Status:** Frozen. Additive changes only until v2 (see [Compatibility rule](#compatibility-rule)).
**Supersedes:** [app-integration-playbook.md](app-integration-playbook.md) (v0.1, narrative)
**Audience:** developers building an app that federates to an IONe deployment as a peer.

## What this document is

The normative, versioned contract between IONe and a federated peer app. The
playbook explains *why*; this document defines *what*, with concrete schemas and
an explicit statement of which rules IONe enforces today.

Every rule below carries an **enforcement status**:

| Status | Meaning |
|---|---|
| **Enforced** | IONe's code rejects, drops, or truncates on violation today. A code citation is given. |
| **Specified** | Part of the v1 contract and relied upon by IONe's consumers, but not mechanically validated today. A peer that violates it will misrender or be dropped silently rather than get a clean error. |
| **Peer-side** | An obligation on the peer that IONe cannot observe. Stated for interoperability. |

Where the frozen contract and the v0.1 playbook disagree, this document wins and
the divergence is called out in [Appendix A](#appendix-a--code-vs-playbook-divergences).

## The six surfaces

A v1 peer exposes these six surfaces. This numbering is canonical and matches
[md/playbooks/building-on-ione.md](../playbooks/building-on-ione.md) §38-45.

| # | Surface | Required? | Section |
|---|---|---|---|
| 1 | MCP server endpoint | Required | [§1](#1-mcp-server-endpoint) |
| 2 | OAuth 2.1 authorization server | Required unless pre-brokered | [§2](#2-oauth-21-authorization-server) |
| 3 | Signed webhook sender | Optional | [§3](#3-signed-webhook-sender) |
| 4 | Resource view metadata (`ione_view`) | Required to render in the shell | [§4](#4-resource-view-metadata) |
| 5 | Context slice (`slice://`) | Recommended | [§5](#5-context-slice-slice) |
| 6 | `whoami://` resource | Required for tenant binding | [§6](#6-whoami-resource) |

---

## 1. MCP server endpoint

A v1 peer serves MCP over HTTP POST JSON-RPC 2.0 at a stable absolute URL
(stored as `peers.mcp_url`). IONe posts a single JSON-RPC object per request and
expects a single JSON-RPC object in response.

### Methods IONe calls

| Method | When | Params | IONe reads from `result` |
|---|---|---|---|
| `initialize` | On session establishment / recovery | MCP standard | `MCP-Session-Id` response header |
| `tools/list` | Manifest refresh | `null` or `{"cursor": <opaque>}` | `tools[]`, `nextCursor` |
| `resources/list` | Manifest refresh, panel discovery | `null` or `{"cursor": <opaque>}` | `resources[]`, `nextCursor` |
| `resources/read` | Panel data, slice, whoami | `{"uri": "<string>"}` | `contents[0].text` |
| `tools/call` | Operator/model tool invocation | MCP standard | MCP standard |

**Enforcement status: Enforced.** `src/services/federation.rs` (`paginated_list`,
`send_jsonrpc`), `src/services/peer_tokens.rs:238-253`.

### Authorization header — stable across the OAuth/pre-broker transition

IONe presents credentials to a peer as exactly one header on every MCP request:

```
Authorization: Bearer <token>
```

This is **identical** whether the token was obtained through OAuth 2.1 (§2) or
issued as a static per-`(workspace, peer)` pre-broker credential. The peer sees
only a bearer token and never needs to know which path produced it.

> **v1 guarantee.** A v1 peer never rebuilds its authentication when an IONe
> deployment moves between static pre-broker credentials and brokered OAuth
> tokens. The wire contract is `Authorization: Bearer` in both directions of that
> migration.

If the token is the empty string, IONe omits the header entirely (unauthenticated
peer, e.g. the loopback demo peer).

**Enforcement status: Enforced.** `src/services/peer_tokens.rs:247`
(`request.bearer_auth(token)`) is the single call site for both credential
sources.

### Session recovery

If a non-`initialize` call fails in a way that looks like a missing session, IONe
transparently calls `initialize`, stores the returned `MCP-Session-Id`, and
retries the original call **once**. A peer that does not use sessions may ignore
`MCP-Session-Id`.

**Enforcement status: Enforced.** `src/services/federation.rs` (`send_jsonrpc`).

### Timeouts

| Call path | Timeout |
|---|---|
| `resources/read` for a chart panel | 5 s |
| `resources/read` for a table panel | 5 s |
| `resources/read` for `whoami://` | 3 s on the subscribe path (`bind_on_subscribe`); **no per-call timeout** on the binding-refresh path, which falls back to the 15 s HTTP client timeout |

**Enforcement status: Enforced.** `src/services/chart_data.rs:21-27`,
`src/services/table_data.rs:44-49`, and the whoami invocation contract in
[foreign-tenant-mapping.md](foreign-tenant-mapping.md):63-67.

---

## 2. OAuth 2.1 authorization server

A v1 peer that is not pre-brokered exposes an OAuth 2.1 (PKCE-required)
authorization server:

| Endpoint | Purpose |
|---|---|
| `/.well-known/oauth-authorization-server` | Discovery metadata |
| `/oauth/authorize` | Authorization endpoint |
| `/oauth/token` | Token endpoint, supports the `refresh_token` grant |
| `/oauth/revoke` | Token revocation |

IONe stores delegated tokens per `(workspace, peer)` and refreshes them
automatically. The resulting access token is presented per §1.

**Enforcement status: Specified** for the peer's endpoint set (IONe fails the
authorization flow rather than validating the peer's discovery document against
the spec). **Enforced** for the resulting header shape.

---

## 3. Signed webhook sender

Optional. A peer that produces events POSTs them to IONe.

```
POST {ione}/webhooks/peer/{peer_id}
Content-Type: application/json
X-IONe-Signature: t=<unix_seconds>,v1=<hmac_sha256_hex>
```

### 3.1 Signature scheme

| Rule | Value | Enforcement |
|---|---|---|
| Header name | `X-IONe-Signature` | **Enforced** — `webhooks.rs:165` |
| Header grammar | comma-separated `key=value` pairs; only `t` and `v1` permitted | **Enforced** — `webhooks.rs:171-178`; any unknown key, or a repeated `t`/`v1`, is rejected |
| `t` | Unix seconds, base-10 signed integer | **Enforced** — `webhooks.rs:180-182` |
| `v1` | Hex, **exactly 64 characters** decoding to 32 bytes. Case-**insensitive**: `hex::decode` accepts `a-f` and `A-F`. Peers should emit lowercase; uppercase is accepted and will not be rejected in v1. | **Enforced** — `webhooks.rs` |
| Signed bytes | `t_ascii ++ b"." ++ raw_request_body` | **Enforced** — `webhooks.rs:200-202` |
| Algorithm | HMAC-SHA256, key = the provisioned `signingSecret` (its ASCII bytes) | **Enforced** — `webhooks.rs:199-203` |
| Comparison | Constant-time | **Enforced** — `webhooks.rs:204` (`subtle::ConstantTimeEq`) |
| Body cap | 256 KiB, rejected with **413** before the handler runs | **Enforced** — `src/routes/mod.rs:90-91` |

Sign the **raw bytes** you transmit. Re-serializing the body before signing will
produce a mismatched digest.

The `peer_id` in the URL path selects which signing secret IONe verifies against;
the HMAC is the authentication.

### 3.2 Replay window

| Rule | Value | Enforcement |
|---|---|---|
| Header freshness | `abs(now - t) <= 300` seconds | **Enforced** — `webhooks.rs:215-217` |
| Header/event skew | `abs(t - occurred_at) <= 30` seconds | **Enforced** — `webhooks.rs:216-217` |
| Duplicate suppression | `PRIMARY KEY (event_id, peer_id)` in `webhook_events_seen`, 72 h retention | **Enforced** — migration + `services/webhook_ingress.rs` |

Both windows are **hardcoded** in v1 and are not operator-configurable.

A duplicate is an idempotent success, not an error:

```json
{ "ok": true, "duplicate": true }
```

### 3.3 Envelope schema

```json
{
  "id": "evt-01J8ZK...",
  "type": "alert.created",
  "occurred_at": "2026-07-25T10:30:00Z",
  "peer_id": "3f0c1f2e-...-uuid",
  "foreign_tenant_id": "tenant-abc",
  "severity": "routine",
  "data": { "message": "..." },
  "approval_required": false
}
```

| Field | Type | Required | Constraint | Enforcement |
|---|---|---|---|---|
| `id` | string | **yes** | length 1..=255 | **Enforced** — `webhooks.rs:228-230` |
| `type` | string | **yes** | length 1..=255, bytes match `^[a-z0-9._/-]+$` | **Enforced** — `webhooks.rs:246-253` |
| `occurred_at` | RFC 3339 timestamp | **yes** | see §3.2 skew rule | **Enforced** — `webhooks.rs:211-222` |
| `peer_id` | UUID | **yes** | must equal the path `peer_id` | **Enforced** — `webhooks.rs:225-227` |
| `foreign_tenant_id` | string | **yes** | length 1..=512 | **Enforced** — `webhooks.rs:231-233` |
| `severity` | string \| absent | no | `routine` \| `flagged` \| `command`; absent or unknown ⇒ `routine` | **Enforced** — `Option<String>`, `webhooks.rs:45` |
| `data` | object | **yes** | must be a JSON **object**; serialized length ≤ 102 400 bytes | **Enforced** — `webhooks.rs:234-245` |
| `approval_required` | boolean | no | defaults `false` when absent | **Enforced** — `#[serde(default)]`, `webhooks.rs:48-55` |

> **Resolved against the v0.1 playbook.** The playbook reads as though
> `approval_required` were optional. At freeze time the code disagreed — a bare
> `bool` with no `#[serde(default)]`, so omitting it failed deserialization and
> returned a bare `400 {"error":"webhook_rejected"}` carrying no `message` (§7.2
> forbids one). v1 resolves this **in the playbook's favour: the field is
> optional, defaulting to `false`.**
>
> This is security-neutral, not a relaxation. The policy floor in §3.5 is
> escalate-only, so an absent field is exactly equivalent to an explicit
> `false` — a value a peer may always send. Requiring the field therefore bought
> no safety while costing peers an undiagnosable rejection. Amended 2026-07-25,
> the freeze date, before any peer implemented against v1; it is a pre-adoption
> correction, not a v2 break.

`approval_required: true` routes the event through IONe's approval gateway.
IONe enforces a policy floor: the flag may **escalate** but never **de-escalate**.
A `severity` of `flagged` or `command` is always gated regardless of the flag.

### 3.4 Delivery preconditions

| Precondition | Failure |
|---|---|
| Peer exists and `status = active` | 401 `webhook_unauthorized` |
| Peer has a provisioned webhook secret | 401 `webhook_unauthorized` |
| `X-IONe-Signature` **parses** per the §3.1 grammar | 400 `webhook_rejected` |
| Signature digest **verifies** against the peer secret | 401 `webhook_unauthorized` |
| Timestamps inside both replay windows (§3.2) | 400 `webhook_rejected` |
| Envelope fields valid per §3.3 | 400 `webhook_rejected` |
| An **active** `workspace_peer_bindings` row exists for `(peer_id, foreign_tenant_id)` | 400 `webhook_rejected`, and **no** dedup row is written — safe to retry once the operator adds the binding |

> **400 vs 401.** A header that does not parse never reaches signature
> verification, so *malformed grammar is 400, not 401*; only a well-formed
> header whose digest fails to verify is 401. The freeze-time draft collapsed
> these into a single "Signature valid → 401" row, which was ambiguous.
> Clarified 2026-07-25 — this documents existing behavior and changes no code.

**Enforcement status: Enforced.** Parser `webhooks.rs:174-195` (400);
`verify_signature` `webhooks.rs:205-213` (401); handler `webhooks.rs:112-155`.

### 3.5 Webhook responses

| Status | Body |
|---|---|
| 200 | `{"ok": true, "duplicate": false, "signalIds": ["<uuid>", ...]}` |
| 200 (replay) | `{"ok": true, "duplicate": true}` — `signalIds` omitted |
| 400 | `{"error": "webhook_rejected"}` |
| 401 | `{"error": "webhook_unauthorized"}` |
| 413 | body-limit rejection, before the handler |

Webhook error bodies deliberately omit `message` and `hint`. See §7.

---

## 4. Resource view metadata

Every `resources/list` entry that should render in IONe's UX shell carries a
`metadata.ione_view` discriminator.

```json
{
  "uri": "gp://aoi/12345/displacement",
  "name": "AOI 12345 displacement",
  "mimeType": "application/vnd.ione.chart+json",
  "metadata": {
    "ione_view": "chart"
  }
}
```

### 4.1 The `ione_view` enum

`ione_view` is a **string**, compared by exact value. v1 defines exactly four:

| Value | Panel | Body fetched via `resources/read`? |
|---|---|---|
| `map` | Map layers | No — metadata only |
| `chart` | Chart panels | Yes |
| `table` | Table panels | Yes |
| `document` | Document panels | No — metadata only |

Rules that hold for all four:

- A resource whose `metadata` is absent, whose `metadata.ione_view` is absent,
  or whose `ione_view` is not one of the four values above, is **silently
  dropped** from that panel. It is not an error and produces no operator-visible
  diagnostic.
- `ione_view` is matched as a raw string; there is no case-folding and no alias.

**Enforcement status: Enforced.** `src/services/map_layers.rs:156`,
`src/services/chart_panels.rs:328`, `src/services/table_panels.rs:204`,
`src/services/document_panels.rs:142`.

### 4.2 `ione_view: "map"`

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

| Field | Type | Required | Notes | Enforcement |
|---|---|---|---|---|
| `tile_url` | string | **yes** | XYZ raster template, non-empty. Scheme must be **`https`**, and it must pass the same SSRF guard as `download_url` (link-local blocked; loopback/private allowed for on-prem). Dropped with a `warn!` otherwise, leaving the rest of the peer's layers intact. IONe does **not** proxy tiles — must be browser-reachable. | **Enforced** — `map_layers.rs`, `url_guard.rs` |
| `bounds` | `[west, south, east, north]` | no | Passed through verbatim, any JSON value accepted | **Specified** (shape not validated) |
| `attribution` | string | no | Rendered as **text**; HTML is not interpreted | **Enforced** as optional |
| `layer_name` | string | no | Overrides resource `name` in layer controls | **Enforced** as optional |
| `opacity` | number | no | `0.0`–`1.0`; omitted ⇒ opaque. Range not clamped. | **Specified** |
| `vector_url` | string | no | Pass-through only; **v1 does not render it**. Validated to the same bar as `tile_url`; if unsafe it is **stripped to `null`** and the layer survives (an unrendered optional field must not cost a valid tile layer). | **Enforced** as optional |

> **Scheme tightening, 2026-07-25 (issue #18).** At freeze time `tile_url` and
> `vector_url` accepted any non-empty string — including `javascript:` and
> plaintext `http:` — and were handed straight to MapLibre. Both are now
> https-only and SSRF-guarded. Under §9 this is a **breaking** change for a peer
> that served tiles over plaintext `http:`, so it is recorded here explicitly
> rather than as an additive clarification. Loopback and private hosts over
> https keep working, so on-prem peers are unaffected.

> **Divergence.** The v0.1 playbook lists `bounds` and `attribution` alongside
> `tile_url` in a way that reads as required. In code, **only `tile_url` is
> required**; `bounds` and `attribution` are optional. v1 freezes the code
> behavior.

### 4.3 `ione_view: "chart"`

IONe accepts **two** metadata forms. The nested form wins when present.

**Nested form** (preferred — `metadata.spec`, or its alias `metadata.chart_spec`):

```json
{
  "metadata": {
    "ione_view": "chart",
    "spec": {
      "chart_type": "line",
      "x_axis": "observation_time",
      "y_axis": "displacement_mm",
      "series": ["mean", "p95"]
    }
  }
}
```

**Flat form** (legacy):

```json
{
  "metadata": {
    "ione_view": "chart",
    "chart_type": "line",
    "x_axis": "observation_time",
    "y_axis": "displacement_mm",
    "series": ["mean", "p95"]
  }
}
```

Within a spec object, each key is accepted in snake_case **or** camelCase
(`chart_type`/`chartType`, `x_axis`/`xAxis`, `y_axis`/`yAxis`).

| Field | Type | Required | Default when the **flat** form is used | Enforcement |
|---|---|---|---|---|
| `chart_type` | string | no | `"line"` | **Enforced** — `chart_panels.rs:343` |
| `x_axis` | string | no | `"bucket_start"` | **Enforced** — `chart_panels.rs:344` |
| `y_axis` | string | no | `"value"` | **Enforced** — `chart_panels.rs:345` |
| `series` | string[] | no | `["value"]`; an empty array falls back to `[y_axis]` | **Enforced** — `chart_panels.rs:346`, `380-390` |

Recommended `chart_type` values: `line`, `bar`, `area`, `scatter`, `histogram`,
`gauge`, `qq`. **Enforcement status: Peer-side** — IONe does not validate the
string; an unrecognized type renders per the shell's fallback.

> **Divergence.** The playbook presents `chart_type`, `x_axis`, `y_axis`, and
> `series` as required. In the flat form they are all **defaulted**, so a chart
> resource carrying only `ione_view: "chart"` is accepted and rendered with
> `line` / `bucket_start` / `value` / `["value"]`. In the **nested** form the
> three scalars *are* effectively required, because `parse_chart_spec` returns
> `None` — dropping the resource — when `chart_type`, `x_axis`, or `y_axis` is
> missing from the supplied spec object. v1 freezes both behaviors as described.

**Chart resource body** (`resources/read`):

```json
{
  "spec": { "chart_type": "line", "x_axis": "observation_time",
            "y_axis": "displacement_mm", "series": ["mean", "p95"] },
  "rows": [
    { "observation_time": "2026-07-01T00:00:00Z", "mean": 1.2, "p95": 3.4 }
  ]
}
```

Each row is an object keyed by `x_axis` and by each entry of `series`.

**Chart body size limit.** v1 requires a chart `resources/read` response to stay
at or below **2 MiB**, matching the table limit.

**Enforcement status: Enforced** as of 2026-07-25 (issue #18).
`src/services/chart_data.rs` caps the body at 2 MiB and maps the overflow to
**413**, matching `table_data.rs`. At freeze time this path had a 5-second
timeout but no byte cap; the gap was closed the same day. Additive under the
compatibility rule — a peer already honoring the limit sees no change.

### 4.4 `ione_view: "table"`

```json
{
  "uri": "gp://assets/inventory",
  "name": "Asset inventory",
  "mimeType": "application/vnd.ione.table+json",
  "metadata": { "ione_view": "table" }
}
```

| Field | Type | Required | Enforcement |
|---|---|---|---|
| `ione_view` | `"table"` | **yes** | **Enforced** — `table_panels.rs:204` |
| `uri` | string | **yes** | non-empty — **Enforced** — `table_panels.rs:206-209` |

No other metadata field is required. `name` defaults to `"Peer table"`.

**Table resource body** (`resources/read`):

```json
{
  "schema": [
    { "name": "asset_id", "type": "string" },
    { "name": "risk", "type": "number" }
  ],
  "rows": [ { "asset_id": "A-1", "risk": 0.82 } ]
}
```

`schema` is an **ordered** column list; each row is an object keyed by
`column.name`. A column omitting `type` normalizes to `string`. Permitted
`type` values: `string`, `number`, `boolean`, `datetime`.

**Table body limits — all Enforced** (`src/services/table_data.rs:8-10`):

| Limit | Value | On violation |
|---|---|---|
| `MAX_TABLE_RESOURCE_BYTES` | 2 MiB | 413 |
| `MAX_TABLE_ROWS` | 5 000 | 413 |
| `MAX_TABLE_COLUMNS` | 64 | 413 |

### 4.5 `ione_view: "document"`

```json
{
  "uri": "gp://reports/2026-q2-compliance",
  "name": "Q2 2026 compliance report",
  "mimeType": "application/pdf",
  "metadata": {
    "ione_view": "document",
    "download_url": "https://files.example.com/reports/q2.pdf?sig=...",
    "mime_type": "application/pdf",
    "file_size_bytes": 184320,
    "last_modified": "2026-07-01T12:00:00Z"
  }
}
```

| Field | Type | Required | Notes | Enforcement |
|---|---|---|---|---|
| `download_url` | string | **yes** | Must parse as a URL, scheme must be **`https`**, and must pass IONe's SSRF guard. The guard blocks **link-local** hosts only; loopback and RFC 1918 private hosts over https are **deliberately allowed** so on-prem peers work (`url_guard.rs:33-36`). Dropped with a `warn!` otherwise. | **Enforced** — `document_panels.rs`, `url_guard.rs` |
| `mime_type` | string | **yes** | Resolved from `metadata.mime_type`, else `metadata.mimeType`, else the resource's top-level `mimeType`. Dropped with a `warn!` if none resolve. | **Enforced** — `document_panels.rs:165-181` |
| `file_size_bytes` | integer | no | | **Enforced** as optional |
| `last_modified` | string | no | | **Enforced** as optional |

`download_url` requirements that IONe cannot verify — **Peer-side**:

- Retrievable by the operator's browser **without** IONe's delegated peer token
  (public, presigned/time-limited, or cookie-authenticated on the peer's origin).
  IONe does **not** proxy and does **not** inject auth.
- Must stay valid for **≥ 5 minutes** after `resources/list`, since the operator
  clicks later.

`application/pdf` is inline-embedded in a sandboxed iframe; other MIME types
render an "open in new tab" link. **No `resources/read` body is defined** for
documents.

> **Divergence.** The playbook documents only `metadata.mime_type`. The code
> additionally accepts `metadata.mimeType` and falls back to the resource's
> top-level `mimeType`. v1 freezes all three, in that precedence order.

---

## 5. Context slice (`slice://`)

A compact capability summary so federation stays cheap in model context.

```json
{
  "uri": "slice://",
  "name": "GroundPulse capability slice",
  "mimeType": "application/vnd.ione.slice+json"
}
```

Body, returned as `contents[0].text` (a JSON **string** containing this object):

```json
{
  "schema_version": "1",
  "peer_id": "groundpulse-prod",
  "summary": "Infrastructure risk intelligence for pipeline, bridge, and dam operators.",
  "domain_tags": ["geospatial", "time-series", "infrastructure"],
  "sample_queries": [
    "What pipeline segments showed accelerating displacement this quarter?"
  ],
  "tool_index": [
    { "name": "query_displacement", "summary": "Time-series displacement for an AOI.",
      "expand_uri": "tools://query_displacement" },
    { "name": "acknowledge_alert", "summary": "Mark an alert acknowledged.",
      "expand_uri": "tools://acknowledge_alert", "approval_required": true }
  ],
  "resource_hints": {
    "example_resources": [
      { "uri_template": "gp://aoi/{aoi_id}/displacement", "description": "AOI time-series" }
    ],
    "recent_activity_summary_uri": "gp://activity/recent"
  }
}
```

| Field | Type | Required | Notes |
|---|---|---|---|
| `schema_version` | string | **yes** | `"1"` for a v1 peer-authored slice |
| `peer_id` | string | no | Peer's own identifier, informational |
| `summary` | string | **yes** | One paragraph, target 80–120 tokens |
| `domain_tags` | string[] | no | Shared taxonomy: `geospatial`, `time-series`, `raster`, `vector`, `tabular`, `alerts`, `compliance`, `financial`, `ag`, `infrastructure`, `observability`, `identity`, `communication` |
| `sample_queries` | string[] | no | 3–5 representative NL queries |
| `tool_index` | object[] | no | `{name, summary, expand_uri?, approval_required?}` per tool. **Full `inputSchema` is not included.** |
| `resource_hints` | object | no | `example_resources[]`, `recent_activity_summary_uri` |

**Enforcement status of the field set: Peer-side.** IONe parses the slice body
opportunistically and does not reject a slice for missing fields.

### 5.1 Size limit — 2 KiB

A v1 slice targets **< 2 KiB** serialized.

**Enforcement status: Enforced as truncation, not rejection.** At prompt-assembly
time IONe truncates the serialized slice to `MAX_SLICE_BYTES = 2048` on a UTF-8
character boundary (`src/services/federation.rs:1260-1280`). An oversized slice is
**silently cut off**, not rejected — a peer that exceeds the limit loses the tail
of its own capability description.

### 5.2 Fallback when `slice://` is absent

If `resources/read {uri:"slice://"}` fails, IONe synthesizes a minimal slice from
the peer's `tools/list`:

```json
{ "schema_version": "0",
  "summary": "Peer <name> exposes <N> tool(s).",
  "tool_index": ["<tool name>", "..."] }
```

`schema_version: "0"` therefore means *IONe-synthesized*, and `"1"` means
*peer-authored*. This is why `slice://` is Recommended rather than Required.

**Enforcement status: Enforced.** `src/services/federation.rs:499-523`.

### 5.3 Slice text is untrusted

IONe inserts slice text into a model prompt inside a sentinel fence and strips
the sentinel substrings `<<<IONE_PEER_SLICE` and `<<<END_IONE_PEER_SLICE>>>`
from peer-supplied text before insertion. Peers must not rely on prompt-visible
formatting; injected instructions are a contract violation.

**Enforcement status: Enforced.** `sanitize_slice_text`,
`src/services/federation.rs:1262-1280`.

### 5.4 IONe does not re-serve an aggregated slice

**IONe's own `/mcp` server does not expose `slice://`.** It advertises exactly
one resource, `whoami://`, and `resources/read` returns a JSON-RPC `-32602`
error for any other URI (`src/mcp_server.rs:955-986`). `slice://` in v1 is a
**peer→IONe** contract only.

> **Divergence.** [mcp-federation.md](mcp-federation.md):45 and
> [building-on-ione.md](../playbooks/building-on-ione.md):32 describe an
> "aggregated `slice://`" on IONe's consumption surface. That is not implemented.
> An MCP client connecting to IONe must not depend on it in v1.

---

## 6. `whoami://` resource

Returns the foreign tenant and user identity for the authenticated session. This
populates `workspace_peer_bindings` and powers cross-app correlation.

**Invocation:** `resources/read {"uri": "whoami://"}`, `Authorization: Bearer`,
8-second timeout ([foreign-tenant-mapping.md](foreign-tenant-mapping.md):63-67).

**Response envelope:**

```json
{
  "contents": [{
    "uri": "whoami://",
    "mimeType": "application/vnd.ione.whoami+json",
    "text": "{\"peer_id\":\"...\",\"foreign_tenant_id\":\"...\"}"
  }]
}
```

`contents[0].text` is a **JSON string** whose parsed value is:

| Field | Type | Required | Notes |
|---|---|---|---|
| `peer_id` | string | **yes** | Peer's own identifier for itself |
| `foreign_tenant_id` | string | **yes** | Binding key; must match the webhook envelope's `foreign_tenant_id` for the same tenant |
| `foreign_tenant_name` | string | **yes** | Human-readable tenant name |
| `foreign_workspace_id` | string | **yes** | Peer-side workspace/project scope |
| `foreign_user_id` | string | **yes** | Peer-side user identifier |
| `foreign_user_email` | string | **yes** | Peer-side user email |
| `foreign_roles` | string[] | **yes** | Peer-side role names; may be empty |

**Enforcement status: Enforced** for the envelope and `mimeType`
(`src/mcp_server.rs:955-998`); **Peer-side** for a peer's own field completeness —
IONe tolerates a failed or partial `whoami` by materializing a `pending` binding
the operator completes manually.

**All seven keys must be present** in the object. A value may be `null` where the
peer has no such scope; a v1 consumer must tolerate `null` and must not treat a
missing key and a null value as equivalent.

IONe serves this same shape on its own `/mcp` server, since IONe is itself an MCP
peer. Its own values are derived from the authenticated session:

| Field | IONe's own value |
|---|---|
| `peer_id` | `$IONE_BIND`, defaulting to `"ione"` |
| `foreign_tenant_id` | the caller's `org_id` as a UUID string |
| `foreign_tenant_name` | the organization's `name`, or `null` |
| `foreign_workspace_id` | **always `null`** — IONe has no single foreign workspace scope for itself |
| `foreign_user_id` | the caller's `user_id` as a UUID string |
| `foreign_user_email` | the user's email, or `null` |
| `foreign_roles` | distinct role names across the caller's memberships in the org; may be `[]` |

Its `resources/list` advertises exactly:

```json
{ "resources": [{ "uri": "whoami://",
                  "name": "Caller identity",
                  "mimeType": "application/vnd.ione.whoami+json" }] }
```

---

## 7. Error conventions

### 7.1 Global envelope

Every IONe 4xx/5xx JSON response uses:

```json
{
  "error": "snake_case_kind",
  "message": "User-facing sentence.",
  "hint": "What to do about it.",
  "field": "form_field_name"
}
```

| Field | Type | Required |
|---|---|---|
| `error` | string, `snake_case` | **yes** — the stable machine-readable discriminator |
| `message` | string | yes for non-webhook endpoints |
| `hint` | string | optional |
| `field` | string | optional, present on validation failures |

Clients must branch on `error`, never on `message` text. Kinds a v1 peer may
actually observe, all verified present in `src/`: `unauthorized`, `forbidden`,
`demo_read_only`, `validation_failed`, `ollama_unreachable`,
`nws_out_of_range`, `connector_error`, `broker_upstream`, `webhook_rejected`,
`webhook_unauthorized`.

> The freeze-time draft also listed `peer_unreachable`, `manifest_timeout`,
> `oauth_denied`, and `oauth_token_expired`. **None of these are emitted
> anywhere in `src/`** — they were aspirational. They are removed rather than
> frozen: §9 clause 8 promises not to change existing discriminators, and
> freezing four that do not exist would have committed IONe to inventing them.
> A peer must not branch on them.

**Enforcement status: Enforced.** `src/error.rs`;
[ione-complete-contract.md](ione-complete-contract.md):125-132.

### 7.2 Webhook exception — deliberately non-leaky

The two webhook error responses carry **only** `error`:

```json
{ "error": "webhook_rejected" }
{ "error": "webhook_unauthorized" }
```

No `message`, no `hint`, no `field`.

**Why:** `POST /webhooks/peer/:peer_id` is an **unauthenticated** endpoint — the
signature *is* the authentication, so anyone on the internet can reach it. A
descriptive error would let an unauthenticated caller distinguish "no such peer"
from "peer revoked" from "bad signature" from "timestamp stale" from "no binding
for this tenant", turning the endpoint into an oracle for peer existence, peer
status, and tenant enumeration. Collapsing every failure into two opaque codes
removes that oracle.

Peers debugging integration should rely on the rules in §3, not on response text.

**Enforcement status: Enforced.** `src/error.rs:122-135`.

### 7.3 Peer-side JSON-RPC error mapping

When a peer returns a JSON-RPC error, IONe maps on the **code**, not the message:

| Peer JSON-RPC code | IONe HTTP status |
|---|---|
| `-32002` (resource not found) | 404 |
| any other code | 502 |

A peer that does not implement `resources/read` returns `-32601` and is reported
as 502 (peer fault), not 404.

**Enforcement status: Enforced on the table path only.**
`src/services/table_data.rs` maps `-32002` to 404 as described. The chart path
(`src/services/chart_data.rs`) collapses **every** peer JSON-RPC error —
including `-32002` — to `Unavailable` → **502**, so a not-found chart resource
reports 502 rather than 404. A v1 peer must not depend on receiving 404 for a
missing chart resource. Aligning the chart path is additive under §9 and is
tracked as a follow-up.

---

## 8. Pagination and large-resource conventions

### 8.1 Cursor semantics

v1 uses MCP-standard opaque cursors.

- A peer returning a partial page sets `nextCursor` to an **opaque string** in
  the same `result` object as the items array.
- IONe re-issues the same method with `{"cursor": <the value verbatim>}`.
- IONe treats the cursor as opaque: no parsing, no construction, no assumption of
  ordering or stability beyond "passing it back yields the next page."
- Termination: the peer **omits** `nextCursor` (or sets it to `null`) on the
  final page.
- IONe also accepts a `cursor` key as an alias for `nextCursor` in the response,
  ignoring it when null.

**Page cap: `MAX_PAGINATION_PAGES = 50`.** IONe stops after 50 pages per call and
logs a truncation warning. A peer whose full listing exceeds 50 pages will be
**silently truncated** from IONe's view. Size pages accordingly.

**Enforcement status: Enforced** on `tools/list` and `resources/list` during
manifest refresh (`src/services/federation.rs`, `paginated_list`) and, as of
2026-07-25, on the four panel-discovery paths (§8.2).

### 8.2 Panel-discovery pagination

The four panel-discovery paths follow `nextCursor` via
`src/services/peer_panels.rs`:

| Path | Source |
|---|---|
| Map layers | `src/services/map_layers.rs` |
| Chart panels | `src/services/chart_panels.rs` |
| Table panels | `src/services/table_panels.rs` |
| Document panels | `src/services/document_panels.rs` |

**Every `resources/list` call site paginates.** The peer-resource browser routes
through `workspace_peer_manifest` → `fetch_manifest` → `paginated_list`, so it
follows `nextCursor` too.

Two **`tools/list`** call sites remain un-cursored and read only the first page:
`fetch_manifest_over_mcp` (`src/routes/peers.rs`, serving
`GET /api/v1/peers/:id/manifest`, which is gated on `status = pending_allowlist`
and used for allowlist review) and `default_streams`
(`src/connectors/mcp_client.rs`, which derives one synthetic stream per readable
tool). A peer exposing more than one page of **tools** will have only its first
page reflected in those two places. The manifest path used for tool invocation
(`federation.rs::fetch_manifest`) does paginate, so this affects allowlist
review and stream derivation, not tool routing.

**v1 requirement (Enforced** as of 2026-07-25, issue #18**):** a v1 peer **must**
support cursor-based pagination on `resources/list`, and IONe follows `nextCursor`
on all four panel paths via `src/services/peer_panels.rs`, subject to the same
50-page ceiling as the manifest path (§8.1).

At freeze time these four paths sent `params: null` and rendered only the
**first page**, while the manifest saw up to 50. That was closed the same day, so
a peer may now paginate `resources/list` freely and have every page rendered.
Additive under the compatibility rule — a peer already returning a single page is
unaffected.

A `nextCursor` that is **absent, `null`, or the empty string** all terminate
pagination identically. This is load-bearing: treating a present-but-`null`
cursor as a continuation caused a 50× request-amplification loop against a
conforming peer, fixed the same day (§8.1).

### 8.3 Size limits summary

| Surface | Limit | Enforcement |
|---|---|---|
| Webhook request body | 256 KiB → 413 | **Enforced** (`routes/mod.rs:90-91`) |
| Webhook `data` field | 102 400 bytes serialized → 400 | **Enforced** (`webhooks.rs:240-245`) |
| Table `resources/read` body | 2 MiB → 413 | **Enforced** (`table_data.rs:8`) |
| Table rows | 5 000 → 413 | **Enforced** (`table_data.rs:9`) |
| Table columns | 64 → 413 | **Enforced** (`table_data.rs:10`) |
| Chart `resources/read` body | 2 MiB → 413 | **Enforced** (`chart_data.rs`, §4.3) |
| Context slice | 2 KiB | **Enforced as truncation** (§5.1) |
| Catalog `description` | 512 chars | **Enforced as truncation** (`federation.rs:1282`) |
| Pagination pages per list call | 50 | **Enforced** on manifest and panel paths (§8.1, §8.2) |

---

## Compatibility rule

### What a v1 peer can rely on across IONe releases

For as long as IONe advertises support for contract v1, a peer that conforms to
this document will keep working without modification. Specifically, IONe will
not, within v1:

1. **Change the wire auth header.** Credentials are presented as
   `Authorization: Bearer <token>`. Migrating a deployment between static
   pre-broker credentials and brokered OAuth tokens does not change this.
2. **Rename or remove** any field marked **required** in §3.3, §4, §5, or §6.
3. **Change the meaning** of the four `ione_view` values, or remove one.
4. **Change the webhook signature scheme** — header name, `t=`/`v1=` grammar,
   the `t ++ "." ++ raw_body` signing input, or HMAC-SHA256.
5. **Tighten** the webhook replay windows below ±300 s / ±30 s.
6. **Lower** any enforced numeric limit in §8.3.
7. **Change** the two webhook error codes or add fields to their bodies.
8. **Change** the `error` discriminator string for an existing documented kind.
9. **Remove** `whoami://` from IONe's own `resources/list`, or change its
   `mimeType`.

### What IONe may do within v1 (additive, non-breaking)

- Add new **optional** fields to any schema. Peers must ignore unknown fields.
- Add new `error` kinds. Clients must treat an unrecognized `error` as a generic
  failure of its HTTP status class.
- Add new `ione_view` values. Peers must expect that resources using an unknown
  view are dropped by older IONe versions.
- **Begin enforcing a limit already documented as Specified** — e.g. the 2 MiB
  chart cap (§4.3) or `nextCursor` on panel paths (§8.2). These are additive by
  construction: a peer already honoring the documented contract sees no change.
  A peer relying on the *absence* of enforcement is already out of contract.
- Relax a limit, lengthen a timeout, or widen a replay window.
- Add new MCP methods to IONe's own server, or new optional methods it calls on
  peers (a peer may answer `-32601`).

### What requires v2

Any of the following is a breaking change and ships only as contract v2, with
both versions supported through a deprecation window:

- Removing or renaming a required field, or making an optional field required.
- Changing the auth header form (e.g. mTLS, DPoP, a non-`Bearer` scheme).
- A second webhook signature version (`v2=`) becoming **mandatory**. Adding
  `v2=` alongside `v1=` while `v1=` still verifies is additive.
- Redefining `ione_view` semantics, or changing `slice://`'s `schema_version`
  contract such that `"1"` means something else.
- Changing an enforced limit in the tightening direction beyond its documented
  value.
- Adding a required field to the webhook error bodies (which would reintroduce
  the §7.2 oracle).

### Version signalling

v1 is signalled by the resource-level constants a peer already emits:
`slice://`'s `schema_version: "1"` and the `application/vnd.ione.*+json` MIME
types. There is no separate contract-version handshake in v1; introducing one
would itself be additive.

---

## Appendix A — code vs. playbook divergences

Recorded at freeze time. **In all but one case the code behavior is what v1
freezes** and the v0.1 playbook is superseded. The exception is divergence 2,
where v1 instead adopts the playbook's reading and the code was amended to
match — see the note in §3.3 for why.

Divergences 2, 6 and 7 were **resolved in code on the freeze date** rather than
left standing; each row records what changed. All three are additive under §9,
so a peer written against the freeze-time text stays conformant.

| # | Topic | Playbook (v0.1) says | Code actually does | Ref |
|---|---|---|---|---|
| 1 | `slice://` on IONe's own server | IONe exposes an "aggregated `slice://`" to MCP clients | `resources/list` advertises only `whoami://`; `resources/read` returns `-32602` for every other URI. `slice://` is peer→IONe only. | `src/mcp_server.rs:955-986` |
| 2 | Webhook `approval_required` | Reads as optional | **Was** a bare `bool` (omitting it ⇒ 400). **Resolved in the playbook's favour**: now `#[serde(default)]`, optional, defaults `false`. Security-neutral under the escalate-only floor. | `src/routes/webhooks.rs:48-55` |
| 3 | Map metadata | `tile_url`, `bounds`, `attribution` read as co-required | Only non-empty `tile_url` is required; the rest are optional | `src/services/map_layers.rs:158-161` |
| 4 | Chart metadata | `chart_type`, `x_axis`, `y_axis`, `series` required | In the flat form all four are **defaulted** (`line`/`bucket_start`/`value`/`["value"]`); a nested `metadata.spec`/`chart_spec` form also exists and accepts camelCase | `src/services/chart_panels.rs:326-400` |
| 5 | Document `mime_type` | Only `metadata.mime_type` documented | Falls back `metadata.mime_type` → `metadata.mimeType` → resource `mimeType` | `src/services/document_panels.rs:165-181` |
| 6 | Chart body size | Not addressed | **Resolved 2026-07-25 (#18).** Was uncapped; now 2 MiB → 413, matching the table path | `src/services/chart_data.rs` |
| 7 | `resources/list` pagination | Presented as uniform | **Resolved 2026-07-25 (#18).** Was manifest-only; all four panel paths now follow `nextCursor` via `peer_panels.rs`. A present-but-`null` cursor was also looping to the 50-page cap — fixed | `federation.rs`, `src/services/peer_panels.rs` |

## Appendix B — executable conformance

The frozen rules that IONe can assert against its own surfaces are exercised by
`tests/contract_v1_integration.rs`:

- `whoami://` `mimeType` and the full §6 field set, on IONe's own `/mcp`
- IONe's `resources/list` advertising exactly `whoami://` (§5.4 divergence #1)
- `resources/read` of `slice://` against IONe returning `-32602` (§5.4)
- The §3.1 signature scheme: a correctly-signed webhook is accepted, and a
  request with a tampered `v1=` digest is rejected 401
- The §7.2 non-leaky webhook error envelope on a representative 4xx
- The §7.1 global envelope shape on a representative non-webhook 4xx
