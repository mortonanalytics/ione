# Requirements — Audit & T&E Event Export

**Source design:** `md/design/audit-event-export.md` (§ API contracts)
**Plan:** `md/plans/audit-event-export-plan.md`
**Status:** in implementation on `feature/audit-event-export`

| Endpoint | Ships in |
|---|---|
| `GET /api/v1/workspaces/:id/audit_events` (extended) | Phase 2 |
| `GET /api/v1/workspaces/:id/audit-aggregates` | Phase 3 |
| `GET /api/v1/workspaces/:id/pipeline-aggregates` | Phase 3 |
| `GET /api/v1/workspaces/:id/audit-export` | Phase 4 |

Write-time error-string scrub (no API surface) ships in Phase 1 and applies to every `audit_events.payload` / `pipeline_events.detail` INSERT; the list and export responses re-apply the scrub at read time for legacy rows.

## API contracts

| Endpoint | Method | Request schema | Response schema | Error codes | Auth |
|---|---|---|---|---|---|
| `/api/v1/workspaces/:id/audit_events` | GET | `?actor_kind=enum(user,system,peer)&actor_ref=string&verb=string(repeatable)&object_kind=string&object_id=UUID&foreign_tenant_id=string&since=ISO8601&until=ISO8601&cursor=opaque&limit=int(1..200)` | `{ items: AuditEvent[], next_cursor: string\|null }` | 400, 401, 403, 404 | Session + workspace-in-org |
| `/api/v1/workspaces/:id/audit-aggregates` | GET | `?op=enum(count_by_bucket,count_by_actor)&bucket=enum(minute,hour,day,week)&group_by=enum(actor_kind,verb,actor_ref)&actor_kind=enum&actor_ref=string&verb=string(repeatable)&object_kind=string&object_id=UUID&foreign_tenant_id=string&since=ISO8601&until=ISO8601` — the full Slice-1 filter set applies to both ops; `bucket` and `group_by` **required when** `op=count_by_bucket`, **rejected with 400 when** `op=count_by_actor`; window ≤ 90d; ≤ 1000 buckets; ≤ 200 groups for `count_by_actor` | Per-op shape, see below | 400, 401, 403, 404 | Session + workspace-in-org + workspace `audit:read` (RBAC) |
| `/api/v1/workspaces/:id/pipeline-aggregates` | GET | `?op=enum(recovery_gap)&connector_id=UUID?&since=ISO8601&until=ISO8601` (window ≤ 90d; ≤ 10,000 items) | `{ op, items: [{ connector_id, gap_seconds, from_stage, occurred_at }], summary: { count, p50, p90, max } }` | 400, 401, 403, 404 | Session + workspace-in-org + workspace `audit:read` (RBAC) |
| `/api/v1/workspaces/:id/audit-export` | GET | Slice-1 filters; `since`+`until` **required** (≤ 90d span); `cursor=opaque`; no `limit` param — hard ceiling 10,000 rows/request | NDJSON stream, one `AuditEvent` JSON object per line; `X-Next-Cursor` response header when truncated (no body cursor field) | 400, 401, 403, 404, 429 | Session + workspace-in-org + workspace `audit:read` (RBAC) |

`AuditEvent` (existing shape, unchanged): id, workspace id, actor kind, actor ref, verb, object kind, object id, payload (JSONB, opaque), foreign tenant id, created-at timestamp.

Auto-exec governance (`md/requirements/active/auto-exec-governance.md`) adds the verbs `auto_exec_policy.created` / `auto_exec_policy.updated` / `auto_exec_policy.deleted` (object_kind `auto_exec_policy`) and the `terminal: true` payload variant of `delivered`; they flow through these endpoints with no contract change.

Headless provisioning (`md/requirements/active/headless-provisioning.md`) adds `service_account_token.issued` / `service_account_token.revoked` (object_kind `service_account_token`) and `provisioning.applied` (object_kind `org`, `workspace_id = NULL`, payload carries the created/updated/unchanged diff and never connector secrets), all with `actor_kind = service_account` and `actor_ref` = the token id. Because `provisioning.applied` is org-level with a null `workspace_id`, it does **not** appear in the workspace-scoped list/aggregate endpoints above — an `org_id` column on `audit_events` is the named follow-up for org-level audit filtering.

### Per-op response shapes for `audit-aggregates`

- `count_by_bucket` → `{ op: "count_by_bucket", bucket: enum, groups: [{ key: string, bucket_start: ISO8601, count: int }] }` — `key` is the value of the requested `group_by` dimension (an `actor_kind` enum value, a `verb` string, or an `actor_ref` string).
- `count_by_actor` → `{ op: "count_by_actor", groups: [{ key: string, count: int }] }` — `key` is the `actor_ref` value; no `bucket_start` field; at most 200 groups, ordered by count descending.

**`pipeline-aggregates` field definitions:** `from_stage` is the `stage` value of the triggering fault event (`stall` or `error`) that begins the gap; `occurred_at` is that triggering event's timestamp (not the recovery event's).

**Contract rules:** `verb`/`object_kind`/`actor_ref` filter values are bound parameters only — never interpolated; no wildcard/regex filter modes. `op`, `bucket`, `group_by` are allow-listed enums (the existing aggregates endpoint's injection-guard pattern). Every UI rendering field above appears in these schemas; no agent may infer shapes from prose.

## Authz tiers

- **List endpoint** = workspace member (matches today's exposure).
- **Aggregates + export** = workspace `audit:read` permission (RBAC, `md/requirements/active/rbac.md`), because bulk retrieval of every member's actions is a materially different exposure than a 200-row recent list. The `admin` permission short-circuits the check; pre-RBAC coc ≥ 80 admins keep access via the migration-0039 backfill.
- **Org isolation:** every endpoint enforces workspace→org membership at the route layer (`ensure_workspace_in_org`, cross-org → 404) **and** the repo queries verify org id via a join to `workspaces` — a DB-layer backstop without RLS.
- **Rate/size limits:** 90-day windows, 10k-row export pages, 1000-bucket aggregate cap, one concurrent export per org (429).

## Implementation notes (behavior the contract table leaves open)

- `limit` values outside 1..200 on the list endpoint are clamped, not rejected (plan-specified).
- Cursor token: `base64url(created_at_rfc3339_micros + "|" + id)`; malformed cursor → 400.
- `next_cursor` is non-null whenever the page is full (`items.length == limit`), including when the next page would be empty.
- On `audit-aggregates` and `pipeline-aggregates`, `since`/`until` are optional and default to a trailing 30-day window (`until = now`, `since = until − 30d`); `since > until` → 400 (also on `audit-export`).
- `audit-aggregates` parses but ignores `cursor`/`limit` (a malformed `cursor` still returns 400, since parsing is shared with the list endpoint).
- `pipeline-aggregates` `summary.p50/p90/max` are `null` when `count == 0`; percentiles use linear interpolation (`percentile_cont` semantics).
