# Requirements — Auto-Execution Governance

**Source design:** `md/design/auto-exec-governance.md`
**Plan:** `md/plans/auto-exec-governance-plan.md`
**Status:** in implementation on `feature/auto-exec-governance`
**Permission vocabulary:** see `rbac.md` — this feature adds no new permission strings.

## Terminal-on-approval delivery (Slice 1)

`POST /api/v1/approvals/:id` keeps its contract shape (see `rbac.md` gate map); only the delivery behavior changes. In `deliver_artifact` (`src/services/delivery.rs`):

- **Connector capability is primary.** Delivery invokes only when the resolved connector's `ConnectorImpl::supports_invoke()` is true (overridden `true` by `slack`, `smtp`, `mcp_client`; default `false`). Otherwise the decision is recorded, a `delivered` audit row is written with payload `{ artifact_id, terminal: true }` (no-op, **not** `delivery_failed`), and the call returns 200.
- A genuine delivery failure on an invoke-capable connector still writes `delivery_failed` and returns its error (500).
- `artifacts.kind` is the secondary signal: `notification_draft`/`briefing`/`report`/`message` are informational (decision is the outcome); `resource_order` is the invoking kind. Any future kind defaults to terminal-unless-its-connector-can-invoke.

## `auto_exec_policies` table (migration 0040)

`id UUID PK · org_id UUID FK organizations ON DELETE RESTRICT (populated by BEFORE INSERT trigger from the workspace; never caller-supplied) · workspace_id UUID FK workspaces ON DELETE CASCADE · name TEXT (UNIQUE per workspace) · trigger_signal_title_prefix TEXT NULL · trigger_severity_at_most TEXT NULL CHECK in {routine,flagged} · connector_id UUID FK connectors ON DELETE RESTRICT · op TEXT · args_template JSONB DEFAULT '{}' · rate_limit_per_min INT CHECK BETWEEN 1 AND 1000 · severity_cap TEXT DEFAULT 'routine' CHECK in {routine,flagged} · authorized_by_permission TEXT · enabled BOOLEAN DEFAULT true · created_by UUID FK users ON DELETE RESTRICT · created_at · updated_at (touch trigger)`

Org-isolation RLS (`aep_org_isolation`) mirrors the existing pattern. Partial index on `(workspace_id) WHERE enabled`. No backfill — the metadata-JSONB path had no production rows.

**Engine read path** (`src/services/auto_exec.rs`): policies come from `SELECT … WHERE workspace_id = $1 AND enabled = true` (`AutoExecPolicyRepo::list_enabled_for_workspace`); the workspace-metadata JSONB parse path is deleted. The in-memory rate bucket is keyed `(workspace_id, policy_id)`. `MAX_RATE_LIMIT_PER_MIN = 1000` is the named ceiling constant in the engine, reused by write-path validation.

## Policy management API contracts

All endpoints: Session + workspace-in-org (cross-org → 404) + workspace `approvals:decide` (authoring an auto-exec policy *is* pre-authorizing approvals; no 9th permission string).

| Endpoint | Method | Request schema | Response schema | Error codes |
|---|---|---|---|---|
| `/api/v1/workspaces/:id/auto-exec-policies` | GET | — | `{ items: AutoExecPolicy[] }` | 401, 403, 404 |
| `/api/v1/workspaces/:id/auto-exec-policies` | POST | `{ name, trigger:{signal_title_prefix?, severity_at_most?:enum(routine,flagged)}, connector_id:UUID, op, args_template:object (default {}), rate_limit_per_min:int(1..1000), severity_cap?:enum(routine,flagged) (default routine), authorized_by_permission:string, enabled?:bool (default true) }` | `AutoExecPolicy` | 401, 403, 404, 409, 422 |
| `/api/v1/workspaces/:id/auto-exec-policies/:policyId` | PUT | same as POST (full replace) | `AutoExecPolicy` | 401, 403, 404, 409, 422 |
| `/api/v1/workspaces/:id/auto-exec-policies/:policyId` | DELETE | — | `204 No Content` | 401, 403, 404 |

`AutoExecPolicy` (response shape, snake_case): `id, workspace_id, name, trigger { signal_title_prefix, severity_at_most }, connector_id, op, args_template, rate_limit_per_min, severity_cap, authorized_by_permission, enabled, created_by, created_at, updated_at`. **`org_id` is internal and never serialized.**

## Authorship guards

- **Guard A — permission escalation (409 `permission_escalation`):** the actor must hold `authorized_by_permission` in this workspace (`effective_permissions` + `permission_grants` glob matching). Holders of `admin` are **exempt**. Same pattern as RBAC Slice 4, minus the `coc_level` ceiling.
- **Guard B — connector workspace scope (422):** `connector_id` must resolve via `ConnectorRepo::get_for_workspace(connector_id, :id)`. `admin` is **not exempt** — this is a data-tenancy constraint, not a privilege gate.

## Fail-closed write-time validation (422, no row created)

- `severity_cap` / `trigger.severity_at_most` allow-listed to `{routine,flagged}` — **`command` is rejected at write**; the router floor independently prevents command-severity auto-execution.
- `rate_limit_per_min` ∈ [1, 1000].
- `authorized_by_permission` must be a member of the closed workspace vocabulary (`is_valid_workspace_permission`).
- `name` non-empty; duplicate `(workspace, name)` → 422.
- Omitted `severity_cap` stores `'routine'` (the safe floor — never the old fail-open `flagged` default).
- All validation failures are 422 except Guard A (409). Malformed JSON / UUIDs are the framework's 4xx.

## Audit trail

Every management write emits an `audit_events` row, `actor_kind = user`, `actor_ref = <actor user id>`, `object_kind = auto_exec_policy`, `object_id = <policy id>`:

| Verb | Payload |
|---|---|
| `auto_exec_policy.created` | `{ actor, after }` |
| `auto_exec_policy.updated` | `{ actor, before, after }` (enable/disable toggles are updates) |
| `auto_exec_policy.deleted` | `{ actor, before }` |

These verbs flow through the existing audit-export surface — noted in `audit-event-export.md`.

## UI

Workspace shell `Policies` tab (`tab-policies`/`panel-policies`), hidden by default; revealed per workspace by a one-shot `GET …/auto-exec-policies` probe (403 → stays hidden, result cached per workspace, no further policy-endpoint calls — `policiesAccessByWs` in `static/app.js`). Renders policy cards (trigger, severity cap, rate limit, enabled state), a create/edit form (POST/PUT), an enable/disable toggle (PUT full-replace with flipped `enabled`), and delete (DELETE).

## Engine hardening (Slice 3)

- Connector resolution in `evaluate`/`run_auto_exec` uses the **workspace-scoped** `get_for_workspace(connector_id, workspace_id)`; a foreign or missing connector is `ConnectorMissing`, never an invocation (closes AEG-C1).
- The router's force-to-draft floor (`forced_target`) is the auto-exec bypass guard; its dead `severity_fallback("flagged"|"command")` arms are removed/asserted so a reorder cannot reopen the path. Regression: an `approval_required` + `flagged` signal routes to Draft even with the classifier unreachable.

## Known gaps (named, not silently dropped)

- **Durable rate-limit state:** the in-memory token bucket resets to full on process restart (deploy, ECS replacement). v1 bounds `rate_limit_per_min` and documents the reset; a Postgres-backed window counter is the follow-up.
- **`args_template` value sanitization:** templates substitute signal title/body/severity verbatim; connectors must treat rendered args as untrusted. No connector currently builds shell/SQL/URL strings from args.
- **MFA gate workspace-scoping** (`mfa_gate` reads session-global `active_role_id`): out of scope here; tracked as a fast-follow per the design's open question 3.
