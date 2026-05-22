# Push Event Ingress — Layer 5: Signed Webhooks

**Date:** 2026-05-21
**Status:** Draft
**Layers:** `db`, `api`, `ui`
**Substrate ref:** [ione-substrate.md](ione-substrate.md) §5 "Push event ingress" (substrate layer 5 of 7), v0.1 table stake #3
**Playbook ref:** [app-integration-playbook.md](app-integration-playbook.md) "Webhook receiver registration + push events" (surface 3)

---

## Problem Statement

IONe ingests app data by polling each connected peer's MCP server every `IONE_POLL_INTERVAL_SECS` (default 60s). A displacement anomaly that GroundPulse detects at 14:00:00 reaches IONe's signal chain no earlier than the next poll tick — up to 60s later — and the audit trail records the IONe-side timestamp, not the app-side one. For a geospatial operator acting on time-critical events (and for federal deployments where event timestamps are compliance artifacts), that gap is visible and, in a live demo, structurally awkward.

The app integration playbook already publishes a webhook contract (surface 3): apps POST signed events to IONe the moment they occur. IONe does not implement the receiver. Shipping v0.1 without it means the published contract is aspirational — any app developer who follows the playbook hits a dead end.

Layer 5 makes push first-class: a signed webhook receiver, replay protection, and fan-in into the existing signal → survivor → approval chain, honoring an `approval_required` flag so a connected app can demand human review before IONe acts.

**v0.1 scope: webhooks only.** MCP `notifications/*` reception is deferred to v0.2 (same events, separate channel; requires per-peer SSE session lifecycle). See Open Questions.

---

## Feature Slices

### Slice 1 — Webhook secret provisioning

The operator provisions a per-peer HMAC signing secret. IONe generates it, stores it encrypted, returns it once for the operator to paste into the app's webhook config. (Automatic registration POST to the app — playbook's `POST {app}/api/webhooks/register` — is deferred to v0.2; v0.1 is manual paste.)

**DB:** **Migration 0027 (to be created):** `ALTER TABLE peers ADD COLUMN webhook_secret_ciphertext BYTEA NULL` — AES-256-GCM encrypted signing secret, separate encryption purpose from `access_token_ciphertext` (see Security / OQ-1). NULL = webhook ingress not provisioned. (Last migration on disk is 0026; 0027–0029 are created by this feature.)

**API:** `POST /api/v1/peers/:id/webhook/provision` — generates a 32-byte secret, stores ciphertext, returns the raw secret **once**.

**UI:** A "Provision webhook" button on the peer/bindings view. On click, calls the endpoint and reveals the secret in a one-time copy field with the receiver URL. Re-provisioning rotates (invalidates the old secret).

**Cross-references:** `PeerWebhookControl` UI → `POST /api/v1/peers/:id/webhook/provision` → `PeerRepo::set_webhook_secret` → `peers.webhook_secret_ciphertext`.

---

### Slice 2 — Signed webhook receiver + replay protection

The peer POSTs events to a per-peer URL. IONe verifies the HMAC over the raw body, rejects replays and stale events, then hands off to fan-in (Slice 3).

**DB:** **Migration 0028 (to be created):** `webhook_events_seen (event_id TEXT, peer_id UUID REFERENCES peers(id) ON DELETE CASCADE, received_at TIMESTAMPTZ DEFAULT now(), PRIMARY KEY (event_id, peer_id))` + index on `received_at` — dedup ledger. Rows purged after a 72-hour window by the existing scheduler tick: `DELETE FROM webhook_events_seen WHERE received_at < now() - INTERVAL '72 hours'`, run once per scheduler cycle (`IONE_POLL_INTERVAL_SECS`).

**API:** `POST /webhooks/peer/:peer_id` — **HMAC-authenticated, no bearer/session.** The `:peer_id` path segment selects which signing secret to verify against (it is an untrusted *selector*; the HMAC is the actual authentication). Header `X-IONe-Signature: t=<unix_ts>,v1=<hmac_sha256_hex>`; signed string is `"{t}.{raw_request_body}"`.

**UI:** None (machine-to-machine).

**Cross-references:** Peer's HTTP POST → `POST /webhooks/peer/:peer_id` → `PeerRepo::get_with_webhook_secret` (load + decrypt secret) → `WebhookEventRepo::try_insert_seen` (dedup) → `webhook_events_seen` table → hands to `ingest_webhook_event` (Slice 3).

---

### Slice 3 — Fan-in to the signal chain + approval gating

A verified, non-duplicate event resolves to a workspace and creates a signal synchronously. `approval_required` (subject to IONe policy) forces the signal down the draft/approval path rather than auto-action.

**New code this slice requires (none of these exist today — stated so an implementer does not assume them):**
- **Migration 0029 (to be created):** `ALTER TABLE signals ADD COLUMN approval_required BOOLEAN NOT NULL DEFAULT false`.
- **`SignalRepo::insert` must gain an `approval_required: bool` parameter** and write the new column. (Today's signature is `(workspace_id, source, title, body, evidence, severity, generator_model)` — calling it unchanged would silently drop the flag.)
- **Router guard:** the router currently classifies via the LLM (`classify_survivor` calls Ollama on title/body/severity). It must be extended so that **when `signal.approval_required = true`, it returns a `draft` routing decision directly, bypassing the LLM** — the LLM must not be able to choose `notification`/auto-send for a gated event.
- **`auto_exec` bypass guard (security-critical):** the delivery service's `process_draft` calls `auto_exec::evaluate_and_invoke` *before* creating an approval; a matching auto-exec policy can deliver the action and skip approval entirely. When `signal.approval_required = true`, `process_draft` **must skip auto-exec evaluation** and go straight to the human-approval draft path. Without this guard, `approval_required` is bypassable.

**DB:** New column `signals.approval_required` (migration 0029). No new approval table: approvals reference artifacts (verified, see Devil's Advocate); the router's existing `draft` path creates the artifact + approval.

**API:** None new. Created signals surface through the existing feed/approvals endpoints.

**UI:** None new. Events appear in the existing workspace feed; `approval_required` events appear in the existing approvals queue.

**Workspace resolution & multi-binding fan-out:** `workspace_peer_bindings` has `UNIQUE (workspace_id, peer_id)` but **not** `(peer_id, foreign_tenant_id)`, so one peer+tenant may map to multiple workspaces in the same org. The ingest service creates a signal in **every** active binding that matches `(peer_id, foreign_tenant_id)`. Zero matches → 400 (no signal). The dedup ledger is checked once per event before fan-out, so a replay never produces signals in any workspace.

**Cross-references:** `ingest_webhook_event` → resolve workspace(s) via `workspace_peer_bindings` (active, matching `foreign_tenant_id`) → `SignalRepo::insert` (source=`connector_event`, `approval_required` set) → `signals` table → existing scheduler tick runs critic → router guard forces `draft` when `approval_required` → delivery `process_draft` skips auto-exec → artifact → approval.

---

## API Contracts

**Naming conventions (explicit, to avoid drift):**
- **Webhook surface** (`/webhooks/peer/:peer_id` request envelope *and* response ACK) uses **snake_case** to match the published playbook contract apps build against.
- **IONe's own authenticated API** (`/api/v1/peers/:id/webhook/provision`) uses **camelCase**, matching every other IONe REST response.

| Endpoint | Method | Request Schema | Response Schema | Error Codes | Auth |
|----------|--------|----------------|-----------------|-------------|------|
| `/api/v1/peers/:id/webhook/provision` | POST | (no body) | `{ peerId: UUID, signingSecret: string (hex, shown once), webhookUrl: string }` — `peerId` is `peers.id` serialized camelCase | 401, 404 | Bearer; org-scoped via `ensure_peer_in_org` (authenticated user's org must equal `peers.org_id`) |
| `/webhooks/peer/:peer_id` | POST | header `X-IONe-Signature: t=<ts>,v1=<hex>`; body = envelope (below) | `{ ok: bool, duplicate: bool, signal_ids?: UUID[] }` | 400, 401, 413 | **HMAC-SHA256 over `"{t}.{raw_body}"`** (no bearer) |

**Webhook envelope** (request body of `/webhooks/peer/:peer_id`) — snake_case, matches playbook
```
id:                string   // uuid-v7; dedup key is (id, peer_id) — two peers may reuse the same UUID without collision
type:              string   // "alert.created" etc.; ^[a-z0-9._/-]{1,255}$
occurred_at:       ISO8601  // must be within ±5 min of now AND within ±30s of header t
peer_id:           UUID     // must equal :peer_id path segment (post-verify cross-check)
foreign_tenant_id: string   // routed only to ACTIVE workspace_peer_bindings; ≤512 chars
severity:          string   // TOP-LEVEL: "routine"|"flagged"|"command" (resolves OQ-4). Maps to signals.severity. Unknown/absent → routine
data:              object   // app-specific payload; body capped at 256 KB
approval_required: bool     // peer may ESCALATE, never de-escalate (see policy)
```

**Webhook ACK response** — snake_case. `signal_ids` is present and non-empty only on `200 { duplicate: false }`; absent on `200 { duplicate: true }` and on all error responses.

**Error semantics (intentionally non-leaky):**
- `400` — malformed header/body, body `peer_id` ≠ path, timestamps disagree, or no active binding for `foreign_tenant_id`. Generic body; specific reason logged internally only (mirrors the 404-not-403 pattern).
- `401` — signature invalid or peer not provisioned. Generic.
- `413` — body exceeds 256 KB.
- `200` with `duplicate: true` — replayed `id` (idempotent ACK; never reveals replay status as an error).

---

## Wiring Dependency Graph

```mermaid
graph LR
  UI["PeerWebhookControl (UI)"] -->|"POST /api/v1/peers/:id/webhook/provision"| PROV["provision_webhook handler"]
  PROV --> SETSEC["PeerRepo::set_webhook_secret"]
  SETSEC --> PEERS["peers.webhook_secret_ciphertext (migration 0027)"]

  PEER["Peer app HTTP POST"] -->|"POST /webhooks/peer/:peer_id (X-IONe-Signature)"| RCV["receive_webhook handler"]
  RCV -->|load+decrypt secret| GETSEC["PeerRepo::get_with_webhook_secret"]
  GETSEC --> PEERS
  RCV -->|HMAC verify raw body| VERIFY["constant-time HMAC check"]
  VERIFY -->|dedup first| DEDUP["WebhookEventRepo::try_insert_seen"]
  DEDUP --> SEEN["webhook_events_seen (new migration 0028)"]
  DEDUP -->|inserted, not duplicate| ING["ingest_webhook_event service"]
  ING -->|resolve workspace(s)| BIND["workspace_peer_bindings (active, foreign_tenant_id; fan-out to all)"]
  ING -->|create signal| SIGREPO["SignalRepo::insert (NEW param approval_required; source=connector_event)"]
  SIGREPO --> SIGNALS["signals.approval_required (new migration 0029)"]
  SIGNALS -->|scheduler tick| ROUTER["router guard: approval_required → draft, bypass LLM"]
  ROUTER --> DELIV["delivery process_draft: skip auto_exec when gated"]
  DELIV --> ARTIFACT["artifacts → approvals (existing)"]

  MCP["MCP notifications/* (v0.2, deferred)"] -.->|same fan-in| ING
```

---

## Devil's Advocate

### 1. What assumption, if wrong, would invalidate this entire design?

The load-bearing premise: **`approval_required` events can be gated using the existing approval machinery, and webhook events can become signals that flow through the existing chain unchanged.** If approvals can't reference a webhook-sourced signal, the entire "honor approval_required" promise (the feature's main differentiator per the PM) collapses.

A second, security-critical premise: **the peer can be identified to select its signing key without trusting the request body.** The playbook as written (peer_id only in the body) is circular — you'd need the key to trust the body, but the key is selected by the body. If unaddressed, the endpoint is forgeable by anyone who knows a valid peer UUID.

### 2. Has that assumption been verified against live state?

**Approval model — verified 2026-05-21:**
```
migrations/0009: approvals.artifact_id UUID NOT NULL REFERENCES artifacts(id)
src/models/approval.rs: Approval { artifact_id: Uuid, ... }  // no signal_id
```
**Result: the naive path is REFUTED ✗, design corrected.** Approvals require an artifact; there is no `signal_id` on approvals. So `approval_required` does **not** create an approval directly from the signal. Instead, it sets a flag on the signal that the **router** reads, forcing routing to the existing `draft` target — which creates an artifact and its pending approval through the path that already works. This needs one new column (`signals.approval_required`), an extra `SignalRepo::insert` parameter, and a router branch, not a new approval schema. The fan-in itself is verified sound: `signal_source` enum includes `connector_event` (migration 0004) and `SignalRepo::insert` exists.

**Second refutation (auto-exec bypass), found in technical review:** `delivery::process_draft` calls `auto_exec::evaluate_and_invoke` *before* creating an approval — a matching auto-exec policy delivers the action and skips the human gate. So routing to `draft` is necessary but **not sufficient** to honor `approval_required`. The corrected design adds an explicit guard: when `signal.approval_required = true`, `process_draft` skips auto-exec entirely. This is stated as required new code in Slice 3. Without it, a connected app's `approval_required: true` is silently bypassable whenever an auto-exec policy matches — a security gap.

**Peer identification — addressed in design:** the receiver path is `/webhooks/peer/:peer_id`. The path segment selects the candidate signing secret *before* the body is parsed or trusted; the HMAC over the raw body is the authentication. The body's `peer_id` is cross-checked against the path post-verification. This closes the circular-lookup hole (security finding C-1) without a header `kid` scheme — the URL path is the key id.

### 3. What's the simplest alternative that avoids the biggest risk?

**Alternative A — drop the poll interval to ~5s instead of building push.** Rejected: 5s polling across N workspaces × M peers is a thundering herd against each app's MCP server (≥36 req/min/peer before any work), still leaves a multi-second audit-timestamp gap, and inverts the "MCP-native, not a polling tool" positioning. More load for a worse result.

**Alternative B — webhook receiver but trust the peer's `approval_required` verbatim (no IONe policy).** Rejected: a compromised peer sets `approval_required: false` on a state-changing event and bypasses the human gate the operator believes is enforced (security finding H-5). The policy rule is cheap and table-free for v0.1: `effective = peer_flag OR severity ∈ {flagged, command}` — the peer can escalate to "needs approval" but never de-escalate below what severity already demands.

The proposed design is the minimal correct one: it reuses the existing signal/critic/router/approval chain and adds exactly three columns/tables (peer secret, dedup ledger, signal flag) plus one receiver endpoint and one provisioning endpoint.

### 4. Structural completeness checklist

- [x] Every UI component that calls an API has it in the contract table — `PeerWebhookControl` → provision endpoint; present.
- [x] Every endpoint has a repository method — provision → `PeerRepo::set_webhook_secret`; receiver → `PeerRepo::get_with_webhook_secret` + `WebhookEventRepo::try_insert_seen` + `SignalRepo::insert`.
- [x] Every new data field appears across layers — `webhook_secret_ciphertext` (DB col + provision response `signingSecret`), `approval_required` (envelope field → `signals.approval_required` col → router behavior → approvals UI), dedup `event_id` (envelope `id` → `webhook_events_seen`).
- [x] Every acceptance criterion names a specific endpoint + expected response — see Acceptance Criteria.
- [x] Wiring graph has an unbroken path from each UI/peer entry to a DB table.
- [x] Integration test scenarios exercise a full path per slice — see Acceptance Criteria.

---

## Acceptance Criteria

**AC-1 — Provisioning returns a secret once**
Given an authenticated operator and a peer in their org, when `POST /api/v1/peers/:id/webhook/provision` is called, then HTTP 200 with `signingSecret` (≥32 hex chars) and `webhookUrl` ending `/webhooks/peer/:id`, and `peers.webhook_secret_ciphertext` for that peer is non-null.

**AC-2 — Valid signed event creates a signal**
Given a provisioned peer with an active binding (workspace W, `foreign_tenant_id` = T), when a correctly HMAC-signed POST to `/webhooks/peer/:peer_id` carries an envelope with `foreign_tenant_id` = T and a fresh `occurred_at`, then HTTP 200 `{ ok: true, duplicate: false, signal_ids: [<uuid>] }` and a `signals` row exists in W with `source = connector_event`.

**AC-3 — Invalid signature rejected**
Given a provisioned peer, when a POST carries `X-IONe-Signature` with a wrong `v1` hex, then HTTP 401 and no `signals` row and no `webhook_events_seen` row is created.

**AC-4 — Replay (duplicate id) is idempotent**
Given an event with `id = E` already accepted, when the identical signed event is POSTed again, then HTTP 200 `{ ok: true, duplicate: true }` and exactly one `signals` row exists for E (no second signal).

**AC-5 — Stale timestamp rejected**
Given a correctly signed event whose `occurred_at` is 10 minutes in the past, when POSTed, then HTTP 400 and no signal created. Likewise when header `t` and `occurred_at` differ by more than 30s.

**AC-6 — Unknown / inactive binding rejected without leak**
Given a provisioned peer with no active binding for `foreign_tenant_id = Z`, when a correctly signed event with `foreign_tenant_id = Z` is POSTed, then HTTP 400 with a generic body and no signal created.

**AC-7 — approval_required forces the draft/approval path (not auto-exec)**
Given a signed event with `approval_required = true` routed to workspace W, and an auto-exec policy that would otherwise match, when the signal is processed (drive the router/delivery via the test hook `classify_with_response` or a synchronous tick — do not rely on wall-clock scheduler timing), then a `routing_decisions` row with `target_kind = draft`, an `artifacts` row, and a pending `approvals` row exist for the resulting signal, and `auto_exec` did NOT deliver the action.

**AC-8 — Peer cannot de-escalate gating**
Given a signed event with `approval_required = false` but top-level `severity = command`, when processed by the router (via test hook), then it is still routed to `draft` (approval required), not auto-sent.

**AC-9 — Body size limit**
Given a POST to `/webhooks/peer/:peer_id` with a body exceeding 256 KB, then HTTP 413 and the request body is not fully buffered into a signal.

**AC-10 — Dedup ledger is purged**
Given `webhook_events_seen` rows older than 72 hours, when the scheduler cleanup runs, then those rows are deleted and rows newer than 72 hours remain.

---

## Tradeoffs

### Synchronous signal creation vs. land-in-stream_events-and-wait

Webhook events create a signal **synchronously** on receipt (`source = connector_event`), not via `stream_events` + the next tick. Rationale: `stream_events` is a poll-flush buffer keyed by cursor/`observed_at` with a `stream_id` FK and an embedding column — none of which fit a push event. Synchronous creation also lets the 200 response return `signal_ids`, and captures the event in ~1–3s instead of up to 60s. The downside — no LLM enrichment before the critic sees it — is acceptable because push events arrive pre-classified (top-level `type` and `severity`); the app is the classifier for its own events. The critic and router still run on the tick exactly as for rule/generator signals.

### Approval gating via router flag vs. direct approval creation

Because approvals are FK'd to artifacts (verified), `approval_required` is a **signal flag the router honors** (forcing the `draft` path that builds the artifact + approval), not a direct approval insert. This reuses the path that already works and keeps the approval model unchanged. Cost: the approval appears after the next tick, not synchronously — acceptable, since an approval is inherently human-paced.

### Peer identity in URL path vs. signature `kid` header

The path `/webhooks/peer/:peer_id` selects the signing key. An equivalent design puts a `kid` in the signature header. The path is simpler, RESTful, and equally safe (the selector is untrusted; the HMAC authenticates). Chosen for simplicity.

### Separate encryption key for the signing secret

The webhook signing secret is encrypted with a purpose-separated key (a distinct `IONE_WEBHOOK_SECRET_KEY`, or purpose AAD on the existing envelope) so a single key compromise does not simultaneously expose outbound OAuth tokens *and* inbound forgery capability. Decision deferred to implementation between the two mechanisms (Open Questions).

---

## Open Questions

| # | Question | Blocking |
|---|---|---|
| OQ-1 | **Signing-secret key separation mechanism.** Separate env var `IONE_WEBHOOK_SECRET_KEY` vs. purpose-AAD on the existing `encrypt_versioned`. Both satisfy H-3; pick one at implementation. | No — implementation detail |
| OQ-2 | **MCP `notifications/*` reception.** Deferred to v0.2 (per-peer SSE session lifecycle, reconnection). The fan-in service (`ingest_webhook_event`) is designed to be reused by it. Confirm v0.1 ships webhook-only. | No — scoping confirmed with user |
| OQ-3 | **Automatic registration POST** (`POST {app}/api/webhooks/register` from the playbook). v0.1 is manual paste of the secret into the app config. Confirm deferral; if a reference app needs auto-registration for the demo, add an outbound provisioning call (must reuse `safe_http` host validation, TLS-only, never log the secret). | No — deferred |
| OQ-4 | **Severity source field — RESOLVED.** Severity is a **top-level envelope field** (`"routine"|"flagged"|"command"`), not nested in app-specific `data`. This makes the approval policy floor (AC-8) implementable at ingest without parsing app-specific payloads. Playbook must add the field (see Requirements Impact). | Resolved |
| OQ-5 | **Replay window width.** Playbook says ±5 min; security suggests ±2 min for a known-peer context. Pick a default (recommend ±5 min to match the published contract) and make it configurable per org later. | No — default ±5 min |

---

## Diagrams

### Receiver decision flow

```
POST /webhooks/peer/:peer_id  (raw body, X-IONe-Signature)
        |
  [body > 256 KB?] --yes--> 413
        | no
  [peer provisioned? (webhook_secret_ciphertext != null)] --no--> 401 (generic)
        | yes
  [HMAC(secret, "{t}.{body}") == v1 ? constant-time] --no--> 401 (generic)
        | yes
  [t within ±5 min AND |t - occurred_at| ≤ 30s ?] --no--> 400 (generic)
        | yes
  [body.peer_id == path peer_id ?] --no--> 400
        | yes
  [INSERT webhook_events_seen ON CONFLICT DO NOTHING] --0 rows--> 200 {duplicate:true}
        | inserted
  [active binding(s) for (peer, foreign_tenant_id)?] --no--> 400 (generic; logged WARN)
        | yes (one or more)
  severity = map(envelope.severity)   // top-level field; unknown → routine
  effective_approval = approval_required OR severity∈{flagged,command}
  for each matching binding:
    SignalRepo::insert(workspace, connector_event, ..., severity, approval_required=effective)
        |
  200 { ok:true, duplicate:false, signal_ids:[...] }
        |
  (next scheduler tick) critic → router → if approval_required: draft→artifact→approval
```

---

## Commercial Linkage

Push closes the visible latency gap in a live demo: an app-side alert appears in the IONe operator's view in ~1–3s instead of up to 60s. For federal/enterprise buyers, two things matter beyond latency: the audit trail records the app-side `occurred_at` (not a polling artifact up to 60s late), and `approval_required` makes IONe's human-in-the-loop gate a first-class part of the integration contract — a connected app can declare which events must not be auto-acted-upon. That declared gate is exactly what a compliance reviewer or enterprise security team asks for before adopting an integration fabric that can act on their data. Without push, the published integration contract is unfulfilled, which undermines the OSS-launch credibility of the MCP-native positioning.

---

## Requirements Impact

- **[app-integration-playbook.md](app-integration-playbook.md) surface 3 + onboarding step 4** must be amended (the playbook is the source of truth apps build against; today it diverges from this design):
  - **Peer identity / signature:** specify the receiver path as `/webhooks/peer/:peer_id` and the signed string as `"{t}.{raw_body}"` with `X-IONe-Signature: t=,v1=`. The current playbook's body-only `peer_id` is insufficient to select the key safely — document the path-as-selector model.
  - **Wire naming:** the envelope is snake_case (`occurred_at`, `peer_id`, `foreign_tenant_id`, `approval_required`) — already matches the playbook; keep it.
  - **Add top-level `severity` field** (`"routine"|"flagged"|"command"`) to the envelope (resolves OQ-4).
  - **approval_required semantics:** document that IONe enforces its own policy floor — the flag may escalate but not de-escalate (severity `flagged`/`command` always gated, and auto-exec is skipped). Apps cannot disable the gate.
  - **occurred_at vs t:** document the ±30s cross-check between header `t` and envelope `occurred_at`.
  - **Onboarding step 4 (auto-registration):** the playbook currently states IONe registers its receiver URL with the app via `POST {app}/api/webhooks/register`. v0.1 does NOT do this. Amend step 4 to: "Webhook provisioning (v0.1): the operator calls `POST /api/v1/peers/:id/webhook/provision` and pastes the returned secret + URL into the app's webhook config. Automatic registration is deferred to v0.2." Without this amendment, the first GroundPulse integration will implement an endpoint IONe never calls.
- **No conflict** with the identity-broker or map-view designs; this slice is additive to the signal chain.
