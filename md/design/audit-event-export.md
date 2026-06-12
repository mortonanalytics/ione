# Audit & T&E Event Export

**Status:** Reviewed (PM + sql-architect + security + devil's advocate + technical-writer passes complete) — ready for `/implement`
**Layers:** `db` (indexes + one write-path redaction change; no new tables), `api`, `ui`
**Demand signals:** DARPA DICE TA3 abstract §2.4/§2.7 (`rfp/darpa-dice/abstract-final.md`; full proposal due 2026-08-25) · enterprise pilot audit-compliance asks (Path 2: audit gateway is an IONe-owned layer)

---

## Problem statement

IONe writes a rich audit trail — every routed tool call, approval decision, delivery attempt, and peer notification lands in `audit_events`; pipeline lifecycle lands in `pipeline_events`. But the only read surface is a hard-capped 200-row unfiltered list per workspace. None of the metrics the DICE abstract claims this trail yields — interaction counts per agent per time window, time-to-recover after a fault, per-actor activity — can actually be computed through the API. The claim is currently unfalsifiable, and a DARPA reviewer can trivially check that.

Commercially, the same gap means a pilot client asking "what did the agents do in that session?" needs direct DB access, which defeats the purpose of an audit gateway and fails NIST 800-171 AU-12's on-demand-review expectation.

## What this feature is

A read-only query, aggregate, and bulk-export surface over the **existing** `audit_events` and `pipeline_events` tables, plus one write-path hardening change (error-string redaction), plus a thin workspace UI panel. No new event tables, no new write paths, no agent-side instrumentation.

## Honest metric scoping (what the schema supports today)

Findings from schema review that bound this design — these are stated, not hidden:

| DICE abstract claim (§2.4) | v1 deliverable | Gap |
|---|---|---|
| Tool-call logs → interaction counts | ✅ Counts per time bucket, grouped by actor/verb, from `peer_tool_executed` / `peer_tool_pending_approval` / `peer_tool_blocked` rows (verbs verified in source) | None |
| Session timestamps → time-to-recover | ✅ Recovery-gap distribution over `pipeline_events` (gap from `stall`/`error` to next `publish_started`, per connector) | Peer MCP **session duration** is not computable — no persistent session table exists (in-process registry only). Human session duration via `user_sessions` is possible but deferred. |
| Per-agent inference-step tracking | ⚠️ Per-actor **event counts** (tool calls executed per actor per window) | True inference-step tracking has no schema support. It is a DICE Phase 1 schema extension (new step-event convention), not this feature. The full proposal must say "per-agent interaction counts" for the shipped capability and keep step tracking in future tense. |

## Non-goals

- Agent-internal telemetry (token cost, inference latency) — apps own this; "compute observability of remote apps" is explicitly not an IONe layer (Path 2 rule).
- LangSmith-style trace trees or a standalone observability dashboard — prohibited standalone-product positioning until the Y3 gate. The UI is one panel in the existing workspace shell.
- CSV export — NDJSON only in v1; CSV waits for a paying ask (JSONB payloads flatten lossily).
- Export to external sinks (S3/SIEM) — deferred.
- A new MCP-session table or `identity_audit_events` exposure — out of scope; see Open questions.
- Cross-workspace/org-wide export endpoint — a DICE evaluator iterates per-workspace exports client-side; an org-level endpoint waits for RBAC.

---

## Feature slices

### Slice 1 — Filterable audit event query

Extend the existing per-workspace audit list with filters and cursor pagination, replacing the hard 200-row ceiling with a proper paging contract.

- **DB:** no schema change. Three new composite indexes on `audit_events`: `(workspace_id, actor_kind, created_at)`, `(workspace_id, verb, created_at)`, `(workspace_id, actor_ref, created_at)`.
- **API:** `GET /api/v1/workspaces/:id/audit_events` gains query params: `actor_kind`, `actor_ref`, `verb` (repeatable), `object_kind`, `object_id`, `foreign_tenant_id`, `since`/`until`, `cursor`, `limit` (clamp 1–200). Keyset cursor on (created_at, id) — offset paging is unsafe under concurrent inserts. `foreign_tenant_id` remains a member-visible response field in v1 (current posture of the existing list endpoint), stated explicitly so the serializer is not silently changed.
- **UI:** extend the **existing** `audit` tab (`tab-audit` / `panel-audit`, already wired into `switchTab(name)` and already polling this endpoint): add filter controls (actor kind, verb, time range) above the existing event table and switch its fetch to the new paged contract.
- **Wiring:** Audit panel → `GET /api/v1/workspaces/:id/audit_events` → audit-event repository list query → `audit_events` table.

### Slice 2 — Audit & pipeline aggregates

Aggregate endpoints mirroring the proven `event-aggregates` design (bucket allow-list, org-scoped, bounded windows) but over native columns instead of JSONB pointers.

- **DB:** no schema change. One new composite index on `pipeline_events` `(workspace_id, stage, occurred_at)`. Aggregate queries: counts per `date_trunc` bucket grouped by actor kind/verb (plain GROUP BY); per-actor event counts (plain GROUP BY, top-200); recovery-gap distribution (window function: per-connector gap from `stall`/`error` event to next `publish_started`).
- **API:** `GET /api/v1/workspaces/:id/audit-aggregates` (ops: `count_by_bucket`, `count_by_actor`) and `GET /api/v1/workspaces/:id/pipeline-aggregates` (op: `recovery_gap`). Window cap 90 days for both. `audit-aggregates` bucket cap 1000 (identical to the existing aggregates endpoint); `pipeline-aggregates` is not time-bucketed — its only caps are the 90-day window and at most 10,000 gap items per response.
- **UI:** interaction-count chart in the Audit panel via the existing chart panel's IONe data path (same rendering route as `event-aggregates` today); recovery-gap shown as a summary stat row, not a new chart type.
- **Wiring:** Audit panel chart → `GET /api/v1/workspaces/:id/audit-aggregates` → audit-aggregate repository → `audit_events`; recovery stat → `GET /api/v1/workspaces/:id/pipeline-aggregates` → pipeline-aggregate repository → `pipeline_events`.

### Slice 3 — Bulk NDJSON export

Streaming export of audit events for a bounded time window, admin-gated.

- **DB:** no schema change; reuses Slice 1 indexes. Row stream from the database to the response body — no full-result buffering (payload JSONB rows can be KB each).
- **API:** `GET /api/v1/workspaces/:id/audit-export` — same filters as Slice 1; **mandatory** `since`/`until` (max 90-day span); hard ceiling of 10,000 rows per request (no client-settable `limit` param on this endpoint), continuation via the `cursor` query param with the next token returned in the `X-Next-Cursor` response header; `Content-Type: application/x-ndjson`; one concurrent export per org (429 otherwise). Gated on the existing admin check (coc ≥ 80) until RBAC ships.
- **UI:** "Export" button in the Audit panel, visible only to admins, downloads the current filter view as NDJSON (paginating client-side through the `X-Next-Cursor` response header if present; there is no `next_cursor` body field on this endpoint).
- **Wiring:** Export button → `GET /api/v1/workspaces/:id/audit-export` → streaming audit-event repository query → `audit_events`.

### Slice 4 — Write-time error-string redaction (security hardening)

Today, delivery failures and connector poll errors write raw error strings (which can embed credentialed URLs and upstream response bodies) into `audit_events.payload` and `pipeline_events.detail`. Export makes these bulk-retrievable, so the leak must be closed at the source before Slices 1–3 ship.

- **DB:** no schema change (write-path behavior change only).
- **API:** none. All audit/pipeline error writes pass error text through a scrub (strip secret-pattern matches: `Authorization:`, `token=`, `key=`, userinfo-in-URL) and truncate to 256 chars.
- **UI:** none.
- **Cross-reference:** this slice gates the other three. Scrubbing is centralized at the audit/pipeline repo write layer (every persisted payload/detail, not per write site — an inventory found error writes beyond the two services first identified). Pre-existing rows are not rewritten at rest; instead the list and export responses re-apply the scrub to `error`-keyed payload fields at read time, so bulk-readable surfaces are clean regardless of row age. The export endpoint additionally omits `foreign_tenant_id` from responses for non-admin callers (admin-gated export makes this moot in v1, but the field rule is stated for when the list endpoint grows the field).

---

## API contracts

| Endpoint | Method | Request schema | Response schema | Error codes | Auth |
|---|---|---|---|---|---|
| `/api/v1/workspaces/:id/audit_events` | GET | `?actor_kind=enum(user,system,peer)&actor_ref=string&verb=string(repeatable)&object_kind=string&object_id=UUID&foreign_tenant_id=string&since=ISO8601&until=ISO8601&cursor=opaque&limit=int(1..200)` | `{ items: AuditEvent[], next_cursor: string\|null }` | 400, 401, 403, 404 | Session + workspace-in-org |
| `/api/v1/workspaces/:id/audit-aggregates` | GET | `?op=enum(count_by_bucket,count_by_actor)&bucket=enum(minute,hour,day,week)&group_by=enum(actor_kind,verb,actor_ref)&verb=string(repeatable)&actor_kind=enum&since=ISO8601&until=ISO8601` — `bucket` and `group_by` **required when** `op=count_by_bucket`, **rejected with 400 when** `op=count_by_actor`; window ≤ 90d; ≤ 1000 buckets; ≤ 200 groups for `count_by_actor` | Per-op shape, see below | 400, 401, 403, 404 | Session + workspace-in-org + admin (coc ≥ 80) |
| `/api/v1/workspaces/:id/pipeline-aggregates` | GET | `?op=enum(recovery_gap)&connector_id=UUID?&since=ISO8601&until=ISO8601` (window ≤ 90d; ≤ 10,000 items) | `{ op, items: [{ connector_id, gap_seconds, from_stage, occurred_at }], summary: { count, p50, p90, max } }` | 400, 401, 403, 404 | Session + workspace-in-org + admin (coc ≥ 80) |
| `/api/v1/workspaces/:id/audit-export` | GET | Slice-1 filters; `since`+`until` **required** (≤ 90d span); `cursor=opaque`; no `limit` param — hard ceiling 10,000 rows/request | NDJSON stream, one `AuditEvent` JSON object per line; `X-Next-Cursor` response header when truncated (no body cursor field) | 400, 401, 403, 404, 429 | Session + workspace-in-org + admin (coc ≥ 80) |

`AuditEvent` (existing shape, unchanged): id, workspace id, actor kind, actor ref, verb, object kind, object id, payload (JSONB, opaque), foreign tenant id, created-at timestamp.

**Per-op response shapes for `audit-aggregates`:**

- `count_by_bucket` → `{ op: "count_by_bucket", bucket: enum, groups: [{ key: string, bucket_start: ISO8601, count: int }] }` — `key` is the value of the requested `group_by` dimension (an `actor_kind` enum value, a `verb` string, or an `actor_ref` string).
- `count_by_actor` → `{ op: "count_by_actor", groups: [{ key: string, count: int }] }` — `key` is the `actor_ref` value; no `bucket_start` field; at most 200 groups, ordered by count descending.

**`pipeline-aggregates` field definitions:** `from_stage` is the `stage` value of the triggering fault event (`stall` or `error`) that begins the gap; `occurred_at` is that triggering event's timestamp (not the recovery event's).

**Contract rules:** `verb`/`object_kind`/`actor_ref` filter values are bound parameters only — never interpolated; no wildcard/regex filter modes. `op`, `bucket`, `group_by` are allow-listed enums (the existing aggregates endpoint's injection-guard pattern). Every UI rendering field above appears in these schemas; no agent may infer shapes from prose.

## Wiring dependency graph

```mermaid
graph LR
  Panel["Audit panel (workspace shell)"] --> ListEP["GET /api/v1/workspaces/:id/audit_events"]
  Panel --> AggEP["GET /api/v1/workspaces/:id/audit-aggregates"]
  Panel --> PipeAggEP["GET /api/v1/workspaces/:id/pipeline-aggregates"]
  Panel --> ExportBtn["Export button (admin-only)"]
  ExportBtn --> ExportEP["GET /api/v1/workspaces/:id/audit-export"]
  ListEP --> ListRepo["audit-event list query (filters + keyset cursor)"]
  AggEP --> AggRepo["audit-event aggregate queries (GROUP BY)"]
  PipeAggEP --> GapRepo["pipeline recovery-gap query (window fn)"]
  ExportEP --> StreamRepo["audit-event streaming query"]
  ListRepo --> AE[("audit_events + 3 new indexes")]
  AggRepo --> AE
  StreamRepo --> AE
  GapRepo --> PE[("pipeline_events + 1 new index")]
  Scrub["error-string scrub (write path)"] -.gates.-> AE
  Scrub -.gates.-> PE
```

## Tenancy & security model

- **Org isolation:** neither table has an org column; isolation is the workspace→org membership check. Every new endpoint enforces it at the route layer **and** the repo queries take the org id and verify it via a join to workspaces — a DB-layer backstop without RLS. RLS via `current_setting` is deliberately avoided: the one existing RLS policy in the codebase is inert because the app never sets the session variable (pre-existing defect, noted in backlog — not fixed here, but not replicated either).
- **Why no org-id denormalization migration:** v1 is workspace-scoped only, so the join suffices; it keeps the hot audit insert path untouched (only index maintenance is added, which is the basis of the <5% monitoring-overhead claim). Org-level rows (null workspace id) stay unreachable in v1 — acceptable because all tool-call interaction rows carry a workspace id (verified in source).
- **Authz tiers (pre-RBAC):** list endpoint = workspace member (matches today's exposure); aggregates + export = admin (coc ≥ 80), because bulk retrieval of every member's actions is a materially different exposure than a 200-row recent list. Revisit when RBAC lands (the planned `audit_read` permission).
- **Rate/size limits:** 90-day windows, 10k-row export pages, 1000-bucket aggregate cap, one concurrent export per org. These guard DB connection + stream DoS by authenticated users.
- **Compliance framing:** this surface directly supports NIST 800-171 AU-12 (on-demand audit review); the admin gate and scrubbing support AU-9 (audit information protection). Useful in both the DICE CMMC narrative and enterprise pilots.

## Tradeoffs

| Decision | Alternative considered | Why this wins |
|---|---|---|
| Extend list endpoint + new aggregate/export endpoints | SQL views + direct DB access for evaluators | DB access fails the gateway product claim, fails AU-12 "support on-demand review" through the system, and is a non-starter for enterprise pilots. The API is the deliverable the DICE proposal cites. |
| Parallel aggregate repos mirroring the `event-aggregates` pattern | Generalize the existing stream-event aggregate repo | Scope joins differ structurally (stream events join 3 hops; audit events 0 hops); parameterizing produces premature abstraction. Mirror the interface, not the implementation. |
| Join-based org guard | org-id column backfill + RLS | Join adds one indexed lookup per query; the migration touches the hot insert path and the RLS pattern is demonstrably inert in this codebase today. Revisit if org-level aggregation becomes a requirement. |
| NDJSON only | NDJSON + CSV | CSV of JSONB is lossy or escaping-heavy; no paying ask yet. |
| Admin gate via existing coc-level check | Wait for RBAC / new role | RBAC is a separate backlog item (DICE Phase 1); the existing check ships now and the design names the future permission. |

## Acceptance criteria

All mechanically verifiable; each maps to an integration test against a seeded workspace.

1. Given a workspace with 350 audit events of which 120 have verb `peer_tool_executed`, when `GET /api/v1/workspaces/:id/audit_events?verb=peer_tool_executed&limit=100` is called by a workspace member, then status is 200, `items.length == 100`, every item's verb is `peer_tool_executed`, and `next_cursor` is non-null; following the cursor returns the remaining 20 with `next_cursor == null`.
2. Given events spanning 3 hours from two actors, when `GET /api/v1/workspaces/:id/audit-aggregates?op=count_by_bucket&bucket=hour&group_by=actor_ref&since=…&until=…` is called by an admin, then status is 200 and the bucket counts sum to the seeded totals per actor.
3. Given 5 audit events from actor A and 3 from actor B in a 1-day window, when `GET /api/v1/workspaces/:id/audit-aggregates?op=count_by_actor&since=…&until=…` is called by an admin, then status is 200 and `groups` contains exactly two items with `key` values A and B and counts 5 and 3, ordered descending; passing `bucket=hour` with this op returns 400.
4. Given `pipeline_events` with an `error` at T and the same connector's next `publish_started` at T+90s, when `GET /api/v1/workspaces/:id/pipeline-aggregates?op=recovery_gap&since=…&until=…` is called by an admin, then status is 200 and an item with `gap_seconds == 90`, `from_stage == "error"`, and `occurred_at == T` for that connector is present, and `summary.count == 1`.
5. Given 10,500 audit events in a 7-day window, when `GET /api/v1/workspaces/:id/audit-export?since=…&until=…` is called by an admin, then the response is `application/x-ndjson` with exactly 10,000 lines each parsing as a JSON object, and the `X-Next-Cursor` header is present; the cursor-continued request returns the remaining 500.
6. Given a non-admin workspace member, when they call `audit-export`, `audit-aggregates`, or `pipeline-aggregates`, then status is 403; when a member of a *different* org calls any of the four endpoints with this workspace id, then status is 404 (existing not-found-over-forbidden convention) and zero rows from the foreign workspace are ever returned.
7. Given an export request with a 91-day span or missing `since`, then status is 400 with a machine-readable error code.
8. Given a delivery failure whose underlying error string contains `https://user:secret@host/path` and a 4KB response body, when the audit row is written, then the stored payload error contains neither the userinfo credential nor more than 256 characters of error text.
9. Given a second concurrent export request for the same org while one stream is open, then status is 429.
10. Given a workspace seeded with 100,000 audit events, EXPLAIN output for a filtered list query (verb + time range) shows an index scan on one of the three new composite indexes, not a sequential scan. (Informational, non-gating target: the same queries return in under 2 seconds on the CI integration environment.)

## Open questions

1. **Peer-session persistence.** Session-duration metrics need an MCP session open/close event written somewhere durable. Defer to the DICE Phase 1 schema work or add cheap `peer_session_opened`/`peer_session_closed` audit verbs now? (Cheap option is two write sites; would make session duration computable from this very surface. Lean: add the verbs in a follow-up, not this design.)
2. **Retention.** `audit_events` is unbounded. Export caps mask this for now, but a retention policy decision is needed before any client runs at high volume. Not blocking v1.
3. **Abstract wording.** §2.4 says "per-agent inference-step tracking supports role-coherence measurement" — for the *full proposal*, recheck that the shipped-capability sentence says interaction counts (shipped) vs step tracking (funded work). Owner: proposal editing pass, not this design.

## Commercial linkage

Sold as part of the audit-gateway layer in domain-app engagements (GroundPulse et al.): "every agent/tool action across your federated apps is queryable and exportable through one gate, no DB access." For DICE: §2.4's three metric claims become live endpoints citable in the full proposal (criteria 1–4 are exactly the evidence artifacts). Tier framing (Path 2-compatible): filtered list = all tiers; aggregates + export = team/enterprise engagements.

## Requirements impact

This project has no `md/requirements/` source-of-truth directory; design docs under `md/design/` are the contract record. This document is the source of truth for the four endpoint contracts above. The infrastructure backlog item "Structured T&E event export" (P6) should be marked in-design referencing this doc.

---

## Devil's Advocate

**1. What assumption, if wrong, invalidates the design?**
That the audit rows IONe *actually writes today* carry enough structure — stable verbs, per-actor refs, workspace scoping, timestamps — to compute the DICE interaction metrics without schema changes. If tool-call routing didn't write distinguishable per-actor rows, every aggregate endpoint here would be aggregating noise and the design would need a new event table (a much bigger feature).

**2. Verified against live state?**
`grep '"peer_tool' src/` against the working tree: verbs `peer_tool_executed` (federation service, tool-routing path), `peer_tool_pending_approval` (approval path, written with workspace id + user actor ref + tool name in payload — read directly in source), and `peer_tool_blocked` (delivery path) all exist as audit write sites with `created_at` defaulting to insert time. Result: **VERIFIED ✓**. Two adjacent claims were *refuted* during review and the design re-scoped accordingly: no MCP session table exists (session-duration metric dropped from v1) and no inference-step granularity exists (metric honestly restated as per-actor event counts).

**3. Simplest alternative that avoids the biggest risk?**
Read-only SQL views + giving evaluators/clients a Postgres role. Avoids all new API surface and its authz/DoS risks. Rejected because it forfeits exactly what is being sold and claimed: the DICE abstract cites IONe's *API* as the monitoring surface; AU-12 review through the system is the compliance story; and pilot clients getting DB credentials is the anti-pattern this product exists to remove. The added complexity is bounded: four read-only endpoints reusing a proven aggregates pattern, four indexes, one write-path scrub.

**4. Structural completeness checklist**
- [x] Every UI call (panel list, panel chart, recovery stat, export button) appears in the API contract table.
- [x] Every endpoint maps to a named repository query (list w/ cursor, aggregates, recovery gap, streaming export); none claim "no DB access."
- [x] No new data fields are introduced in any layer; existing `AuditEvent` fields appear in DB (migration 0009/0026), API schema, and UI table rendering. New query params exist at API + UI layers only (filters, not data fields).
- [x] Each acceptance criterion names its endpoint and expected response values (criteria 1–10).
- [x] Wiring graph paths are unbroken from panel/button → endpoint → query → table; the scrub slice is write-path-only and marked as gating, not rendering.
- [x] Integration scenarios exercise one full request path per slice: criterion 1 (slice 1), 2–4 (slice 2), 5/9 (slice 3), 8 (slice 4), 6–7/10 (cross-cutting authz/limits/perf).
