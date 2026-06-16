# Requirements — Observability Data Plane

**Source design:** `md/design/observability-data-plane.md` (§ API contracts)
**Plan:** `md/plans/observability-data-plane-plan.md`
**Status:** implemented; validation passed locally against Postgres

## Event Contract

`interaction_events` is the async, queryable, subscribable data plane for federated tool interactions. `audit_events` remains the synchronous compliance trail and is not written on the per-tool-call hot path.

`InteractionEvent`: `id`, `org_id`, `workspace_id`, `peer_id`, `peer_name`, `tool_name`, `caller_kind`, `caller_user_id`, `caller_peer_id`, `caller_token_id`, `session_id`, `sequence_number`, `outcome`, `latency_ms`, `detail`, `recorded_at`.

`caller_kind` uses the existing `actor_kind` enum. `outcome` is one of `allow`, `deny`, `pending`, or `error`. `detail` is capped and redacted before insert and must not carry raw upstream error strings.

Session ordering uses the MCP transport session id when provided. If it is absent, capture falls back to `AuthContext.session_id`. Sessionless calls may have null `session_id` and null `sequence_number`.

Trusted-peer JWT callers remain represented by their resolved user principal unless the auth context already carries a stable peer id. The capture path must not add a hot-path lookup solely to infer peer provenance.

## API Contracts

| Endpoint | Method | Request schema | Response schema | Error codes | Auth |
|---|---|---|---|---|---|
| `/api/v1/workspaces/:id/interaction-events` | GET | `?peer_id=UUID&caller_user_id=UUID&caller_peer_id=UUID&caller_token_id=UUID&outcome=enum(allow,deny,pending,error)&session_id=UUID&since=ISO8601&until=ISO8601&cursor=opaque&limit=int(1..200)` | `{ items: InteractionEvent[], next_cursor: string\|null }` | 400, 401, 403, 404 | Session + workspace-in-org + workspace `audit:read` |
| `/api/v1/workspaces/:id/interaction-aggregates` | GET | `?op=enum(outcome_summary,count_by_bucket,count_by_principal)&bucket=enum(minute,hour,day,week)&peer_id=UUID&caller_user_id=UUID&caller_peer_id=UUID&caller_token_id=UUID&outcome=enum&session_id=UUID&since=ISO8601&until=ISO8601` | Per-op shape, see below | 400, 401, 403, 404 | Session + workspace-in-org + workspace `audit:read` |
| `/api/v1/workspaces/:id/interaction-sessions/:session_id` | GET | `?limit=int(1..1000)` | `{ session_id: UUID, items: InteractionEvent[] }` | 400, 401, 403, 404 | Session + workspace-in-org + workspace `audit:read` |
| `/mcp?workspace_id=:workspace_id` and `/mcp/sse?workspace_id=:workspace_id` | GET | `workspace_id=UUID`; cannot be combined with `session` inline request mode | SSE stream of JSON-RPC notifications | 400, 401, 403, 404 | MCP/session auth + workspace-in-org + workspace `audit:read` |

### Aggregate Shapes

- `outcome_summary` -> `{ op: "outcome_summary", outcomes: [{ outcome, count }] }`
- `count_by_bucket` -> `{ op: "count_by_bucket", bucket, groups: [{ bucket, peerId, peerName, outcome, count }] }`
- `count_by_principal` -> `{ op: "count_by_principal", groups: [{ callerKind, callerId, count, denyCount, errorCount }] }`

`bucket` is required for `count_by_bucket` and rejected for the other aggregate ops. Aggregate windows default to the trailing 30 days, reject `since > until`, and are capped at 90 days. `count_by_bucket` rejects requests that would produce more than 1000 buckets.

## Authz And Isolation

Every read endpoint enforces `ensure_workspace_in_org` and `require_permission(..., "audit:read")`. Repository queries also join through `workspaces` and bind `org_id` as a database backstop. RLS exists for parity with org-scoped tables but is not the application isolation mechanism.

The federated tool invocation gate uses `require_permission`, not a direct user-role lookup, so service-account `tool_invoke:*:*` grants work.

## SSE Notifications

Workspace-scoped MCP SSE emits JSON-RPC notifications:

```json
{
  "jsonrpc": "2.0",
  "method": "notifications/tools/interaction",
  "params": {}
}
```

`params` is the `InteractionEvent` object. When `workspace_id` is absent, existing finite SSE behavior is preserved.
