# Requirements — RBAC Scaffolding

**Source design:** `md/design/rbac-scaffolding.md`
**Plan:** `md/plans/rbac-scaffolding-plan.md`
**Status:** in implementation on `feature/rbac-scaffolding`

## Permission vocabulary (closed, 10 strings)

Grammar: flat `namespace:verb`, or `tool_invoke:<peer_slug>:<tool_glob>` where `*` matches one `:`-segment. Matching is exact-string or per-segment glob (`permission_grants` in `src/auth.rs`). Org-scoped permissions are also carried by service-account tokens (`service_account_tokens.permissions`, headless-provisioning).

| Permission | Scope | Held in | Checked by |
|---|---|---|---|
| `admin` | workspace | `roles.permissions` | `require_permission` (short-circuits **every** workspace check; not an org grant) |
| `audit:read` | workspace | `roles.permissions` | `require_permission` |
| `roles:manage` | workspace | `roles.permissions` | `require_permission` |
| `peers:manage` | workspace **and** org | `roles.permissions` / `org_memberships.permissions` | `require_permission` (subscribe) / `require_org_permission` (create/delete/allowlist) |
| `approvals:decide` | workspace | `roles.permissions` | `require_permission` |
| `workspace:write` | workspace | `roles.permissions` | `require_permission` |
| `tool_invoke:<peer>:<tool>` | workspace | `roles.permissions` | segment-glob matcher in `route_tool_call` |
| `trust_issuers:manage` | org | `org_memberships.permissions` | `require_org_permission` |
| `service_accounts:manage` | org | `org_memberships.permissions` / token | `require_org_permission` (mint/list/revoke service-account tokens — headless-provisioning) |
| `provisioning:apply` | org | `org_memberships.permissions` / token | `require_org_permission` (consolidated grant for `POST /api/v1/provision`; provisions all spec entity types without per-resource permissions — headless-provisioning) |

**Resolution is per `(user, workspace)` at call time** — the union of `permissions` arrays across all the user's roles in that workspace (`RoleRepo::effective_permissions`), never the session-global `active_role_id`. Fail-closed: no membership, empty grants, or missing permission → 403. Org-scoped checks resolve `(user, org)` against `org_memberships`; the workspace `admin` grant does **not** short-circuit org checks.

## `org_memberships` table (migration 0039)

`id UUID PK · user_id UUID FK users ON DELETE CASCADE · org_id UUID FK organizations ON DELETE CASCADE · permissions JSONB DEFAULT '[]' · created_at TIMESTAMPTZ · UNIQUE(user_id, org_id)`

Backfill (runs once in 0039, mirrored by `RoleRepo::upsert` and the claim-mapper for post-migration role creation):
- every role with `coc_level >= 80` and never-set permissions (`'{}'`) gets the workspace admin grant set `["admin","audit:read","roles:manage","approvals:decide","workspace:write","tool_invoke:*:*"]`;
- every user holding such a role in any workspace of an org gets an `org_memberships` row granting `["trust_issuers:manage","peers:manage"]`.

`peers:manage` is intentionally absent from the workspace admin grant array (peer rows are org-scoped); workspace admins still pass the workspace-scoped subscribe gate via the `admin` short-circuit.

## Role-management API contracts

| Endpoint | Method | Request schema | Response schema | Error codes | Auth |
|---|---|---|---|---|---|
| `/api/v1/workspaces/:id/roles` | GET | — | `{ items: [{ id, workspaceId, name, cocLevel, permissions: string[], memberCount: int }] }` | 401, 403, 404 | Session + workspace-in-org + `roles:manage` |
| `/api/v1/workspaces/:id/roles/:roleId/permissions` | PUT | `{ permissions: string[], cocLevel?: int }` | `{ id, workspaceId, name, cocLevel, permissions }` | 400 `invalid_permission`, 401, 403, 404, 409 `permission_escalation` | Session + workspace-in-org + `roles:manage` + escalation guard |
| `/api/v1/workspaces/:id/memberships` | POST | `{ user_id: UUID, role_id: UUID }` | `{ id: UUID }` | 400, 401, 403, 404, 409 (`permission_escalation` or `membership_exists`) | Session + workspace-in-org + `roles:manage` + escalation guard |
| `/api/v1/workspaces/:id/memberships/:userId/:roleId` | DELETE | — | `204 No Content` | 401, 403, 404 | Session + workspace-in-org + `roles:manage` |

PUT validates every string against the closed **workspace** vocabulary (the 6 fixed strings + structurally valid 3-segment `tool_invoke:` scopes); anything else — including `trust_issuers:manage`, which is org-scoped — is 400 `invalid_permission`. Org grants are seeded only by backfill and the claim-mapper; no management UI in v1.

## Escalation guard (409 `permission_escalation`)

- **PUT permissions:** a non-`admin` actor may not add a permission they do not hold in the workspace, and may not raise `cocLevel` above their own effective `MAX(coc_level)` (lowering or keeping it is allowed).
- **POST membership:** granting a membership hands out the role's whole permission set, so the same guard applies — the target role's permissions must all be held by the actor and its `coc_level` must not exceed the actor's max.
- **Exemption:** a caller holding `admin` in the workspace is exempt from both clauses.

## Audit trail

Every management write emits an `audit_events` row, `actor_kind = user`, `actor_ref = <actor user id>`:

| Verb | object_kind / object_id | Payload |
|---|---|---|
| `role.permissions.updated` | `role` / role id | `{ actor, before, after, cocBefore, cocAfter }` |
| `membership.granted` | `membership` / membership id | `{ actor, userId, roleId }` |
| `membership.revoked` | `membership` / — | `{ actor, userId, roleId }` |

## Gate-to-permission map (pre-existing endpoints; added 403 path only)

| Surface | Gate |
|---|---|
| `GET …/audit-aggregates`, `GET …/pipeline-aggregates`, `GET …/audit-export` | workspace `audit:read` |
| `GET/POST/DELETE /api/v1/admin/trust-issuers*` | org `trust_issuers:manage` |
| `POST /api/v1/approvals/:id` | workspace `approvals:decide` (resolved from the approval's artifact workspace; missing artifact → 403) |
| `PATCH /api/v1/workspaces/:id`, `POST …/close` | workspace `workspace:write` |
| `POST /api/v1/workspaces/:id/connectors` | workspace `workspace:write` (HP-H1: closed the one composing-endpoint gap; was previously org-scoped only) |
| `POST/DELETE /api/v1/peers*`, `POST …/peers/:id/authorize` | org `peers:manage` |
| `POST …/workspaces/:id/peers/:peerId/subscribe` | workspace `peers:manage` |
| MCP `tools/call` on `peer:tool` names (`route_tool_call`) | workspace `tool_invoke:<peer_name>:<tool>` (glob), denied **before** any outbound peer request, surfaced as JSON-RPC `-32403` |

`coc_level` is no longer read by any per-request access check (display/sort + escalation ceiling only). MCP `tools/list` is **not** role-filtered in v1 — callers may see federated tools they cannot invoke; dispatch is the enforcement point.

## UI

Workspace shell `Roles` tab (`tab-roles`/`panel-roles`), hidden by default; revealed per workspace by a one-shot `GET …/roles` probe (403 → stays hidden, result cached per workspace, no further role-endpoint calls — `rolesAccessByWs` in `static/app.js`). Renders role cards (permission chips + member counts), a permission editor (PUT), and membership grant/revoke controls (POST/DELETE).

## Known gaps (named, not silently dropped)

- Creating a workspace grants the creator **no** membership in it — a fresh workspace is manageable only by `admin`/grant-seeded users. Surfaced during implementation; needs a follow-up decision (auto-grant creator an admin role?).
- Deferred security findings from the design: H-4 `list_workspaces` membership filter, M-1 `admin_funnel` gate, M-2 connector-validate SSRF gate, L-3 OAuth-client ownership filter.
- Org-grant management UI (only backfill + claim-mapper write `org_memberships` in v1).
