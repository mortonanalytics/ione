# RBAC Scaffolding

**Status:** Reviewed (PM + security + sql-architect + devil's advocate + technical-writer passes complete) — ready for `/implement`
**Layers:** `db`, `api`, `ui`
**Demand signals:** DARPA DICE TA3 abstract §2.4 ("role-based access control is a scoped DICE Phase 1 extension"; full proposal due 2026-08-25) · `md/design/audit-event-export.md` names `audit:read` as first consumer (ships behind `coc≥80` today as acknowledged debt) · enterprise expansion gate per Path 2 (`.claude/rules/path-2-stream-p.md`) · infrastructure backlog priority #2

---

## Problem statement

IONe's entire access model is one coarse integer gate: `require_admin` = the session's active role has `coc_level ≥ 80`. The `roles.permissions` JSONB column has existed since migration 0002 and has **never** been read or written. Three consequences:

1. **The gate enforces the wrong role.** `resolve_active_role_id` ([auth.rs:283](src/auth.rs#L283)) picks the user's *most-recently-created membership across all workspaces*, with no workspace filter, and stores it once on `AuthContext` ([auth.rs:236](src/auth.rs#L236)). A user who is admin in workspace A and a plain member in workspace B passes `require_admin` for B's actions. **Verified by reading; this is a live authorization defect, not a hypothetical.** It must be fixed as the foundation of any permission work — a finer-grained check built on the same session-global role inherits the bug exactly.

2. **No vocabulary, no management surface.** Even where gating exists, there is no way for an org admin to express "this contractor may read audit data but not modify connectors." The only lever is making everyone `coc≥80`, which voids the gate.

3. **The federation tool-call path has no role check at all.** `route_tool_call` ([federation.rs:99](src/services/federation.rs#L99)) and every MCP tool handler carry `active_role_id: None` (all three `resolve_auth` branches, [mcp_server.rs:208-209](src/mcp_server.rs#L208)) — the DICE §2.4 commitment ("agents can call only granted tools") is currently unenforceable, and the abstract claim is unfalsifiable at proposal time.

This is **scaffolding**, deliberately bounded: a closed, flat permission vocabulary (8 fixed strings — see the API contract grammar — plus narrower `tool_invoke` scopes), workspace-scoped resolution, enforcement at the routes that most need it plus the federation router, and a role-management surface. Not full IAM — see Non-goals.

## Scope decision: what gets gated

The security review inventoried many member-gated mutations that arguably need a role gate (peer registration, workspace rule installation, approval decisions). Shipping an RBAC feature while leaving "any member can approve a peer tool call or install arbitrary rules" is indefensible, so the highest-severity state-changing mutations are folded in. Read endpoints stay member-gated. Explicitly **in** scope for gating this pass:

| Surface | New permission | Why now (security finding) |
|---|---|---|
| Audit aggregates + export | `audit:read` | Already coc-gated; honors the audit-export design's named contract |
| Trust-issuer CRUD | `trust_issuers:manage` | Already coc-gated; JWKS-fetch SSRF surface (M-3) |
| Approval decision (`POST /approvals/:id`) | `approvals:decide` | H-1: any member triggers external peer tool execution |
| Workspace patch/close | `workspace:write` | H-3: any member installs rules / closes prod workspaces |
| Peer create/delete/allowlist/subscribe | `peers:manage` | H-2: any member registers malicious MCP endpoints |
| Federation tool-call dispatch | `tool_invoke:<peer>:<tool>` | C-2 / DICE §2.4 differentiator |
| Role + membership management | `roles:manage` | new surface |

Deferred to a fast-follow (named, not silently dropped): `list_workspaces` membership filter (H-4 enumeration), `admin_funnel` env-only gate (M-1), connector-validate SSRF gate (M-2), OAuth-client ownership filter (L-3). These are tracked in Open questions → "Deferred security findings"; none are introduced by this feature.

---

## Feature slices

### Slice 0 — Workspace + org permission resolution (foundation; fixes C-1)

Replace the session-global active-role with at-call-time resolvers keyed on `(user, workspace)` and `(user, org)`. This slice ships no new user-facing capability; it makes every subsequent gate correct.

- **DB:** no schema change in this slice (the `org_memberships` table lands in Slice 1). New read query: effective permissions for `(user_id, workspace_id)` = union of `permissions` arrays across all the user's roles in that workspace, plus `MAX(coc_level)`. Served by the existing `memberships(user_id, workspace_id, role_id)` UNIQUE index (leading-prefix scan).
- **API:** no new endpoint. Two helpers: `require_permission(ctx, workspace_id, permission)` (workspace-scoped, the common case) and `require_org_permission(ctx, permission)` (org-scoped, for org-level operations like trust issuers — resolves against `org_memberships`, added in Slice 1). Both call their resolver with an explicit scope id instead of reading `ctx.active_role_id`. `require_admin` is re-implemented as `require_permission(ctx, workspace_id, "admin")` — it no longer reads `coc_level` directly. **Fail-closed:** missing permission, empty `permissions`, or no membership in the scope → 403.
- **UI:** none.
- **Cross-reference:** every workspace-gated endpoint in Slices 2–4 calls `require_permission` with the path's `:id` workspace; trust-issuer endpoints call `require_org_permission(ctx, "trust_issuers:manage")`.

### Slice 1 — Permission vocabulary + org-membership table + backfill (preserves all current access)

- **DB:** one migration does three things. (a) Backfills `roles.permissions` for every role with `coc_level ≥ 80` to the full workspace grant set (`["admin","audit:read","roles:manage","peers:manage","approvals:decide","workspace:write","tool_invoke:*:*"]`), guarded by `WHERE permissions = '{}'`. (b) Creates `org_memberships` — a thin `(user_id, org_id, permissions JSONB, created_at)` table with `UNIQUE(user_id, org_id)`, holding org-scoped grants (`trust_issuers:manage` is the only one in the v1 vocabulary). (c) Backfills `org_memberships` so every user who holds a `coc_level ≥ 80` role in *any* workspace of an org gets an org membership granting `["trust_issuers:manage"]` — this exactly preserves who can manage trust issuers today (since `require_admin` passes for them now). Adds GIN index on `roles.permissions`; adds `memberships(workspace_id)` and `memberships(role_id)` indexes for management queries. `coc_level` is no longer used in per-request access checks (retained for display/sort and as the escalation-guard ceiling — Slice 4).
- **API:** the six existing `require_admin` call sites switch to the named permission: audit aggregates ([audit_aggregates.rs:71](src/routes/audit_aggregates.rs#L71), [:139](src/routes/audit_aggregates.rs#L139)) and audit export ([audit_export.rs:70](src/routes/audit_export.rs#L70)) → `require_permission(…, "audit:read")` (workspace-scoped); trust issuers ([trust_issuers.rs:49](src/routes/admin/trust_issuers.rs#L49), [:63](src/routes/admin/trust_issuers.rs#L63), [:121](src/routes/admin/trust_issuers.rs#L121)) → `require_org_permission(ctx, "trust_issuers:manage")`. The `admin` permission short-circuits workspace checks (it is not an org-level grant; org operations require an explicit org membership).
- **UI:** none in this slice.
- **Cross-reference:** role-create/upsert paths (`RoleRepo::upsert`, claim-mapper) set `permissions` inline when `coc_level ≥ 80`, so IdP-issued admin roles created after migration are also granted (the backfill runs once); the same paths grant an `org_memberships` row when they create a `coc≥80` role.

### Slice 2 — Route-handler permission gates (folds in H-1/H-2/H-3)

- **DB:** none.
- **API:** add `require_permission` to the in-scope mutation handlers: `POST /approvals/:id` → `approvals:decide`; `PATCH`/`close` workspace → `workspace:write`; peer create/delete/allowlist/subscribe → `peers:manage`. Each resolves the permission against the workspace the mutation targets.
- **UI:** none (these are API-only or already-rendered admin actions; the UI surfaces 403s through existing error handling).
- **Cross-reference:** `approvals:decide` gate sits before `execute_pending_tool_call`; `workspace:write` before `evalexpr` rule compilation.

### Slice 3 — Federation tool-call gating (DICE §2.4 differentiator; fixes C-2)

- **DB:** none (uses `tool_invoke:*` grants from Slice 1 vocabulary).
- **API:** in the MCP tool-call path, after the workspace is known, resolve the caller's effective permissions for that workspace and gate the dispatch: the requested `peer:tool` must match a held `tool_invoke:<peer_slug>:<tool_glob>` grant (app-side glob: `*` wildcard per `:`-segment). No match → 403 before any peer call. Since the MCP bearer path carries no role today (`active_role_id: None`), the tool handler performs the lookup with the validated workspace id rather than relying on `AuthContext`.
- **UI:** none.
- **Cross-reference:** replaces the `TODO(role-filter)` at [mcp_server.rs:743](src/mcp_server.rs#L743),[858](src/mcp_server.rs#L858); peer slug is `peers.name`.

### Slice 4 — Role-management API + admin UI (privilege-escalation-safe)

- **DB:** read query: roles in a workspace with their permissions + member counts (LEFT JOIN memberships, uses the new `memberships(role_id)` index). Writes: set a role's permission array; grant/revoke a membership. Every write emits an `audit_events` row (`role.permissions.updated` / `membership.granted` / `membership.revoked`, payload carries before/after + actor) — the JSONB-grant audit-trail story, reusing the 0038 verb index.
- **API:** `GET /api/v1/workspaces/:id/roles` (extends today's role list with permissions + member count), `PUT /api/v1/workspaces/:id/roles/:roleId/permissions`, `POST /api/v1/workspaces/:id/memberships`, `DELETE /api/v1/workspaces/:id/memberships/:userId/:roleId`. All gated by `roles:manage`. **Escalation guards:** the actor may only grant permissions they themselves hold in that workspace, and may not set a role's `coc_level` above their own effective `MAX(coc_level)`. **Exception:** a caller holding the `admin` permission is exempt from both guards (admin implicitly holds every permission).
- **UI:** a "Roles" section in the workspace admin surface — list roles with permission chips + member counts, a permission-toggle editor, and member grant/revoke. Visible only when the caller holds `roles:manage` (probe-and-hide on 403, matching the audit panel's pattern).
- **Cross-reference:** `RolesAdmin` component → the four endpoints above → role/membership repos → `roles`/`memberships` tables + `audit_events`.

---

## API contracts

| Endpoint | Method | Request schema | Response schema | Error codes | Auth |
|---|---|---|---|---|---|
| `/api/v1/workspaces/:id/roles` | GET | — | `{ items: [{ id: UUID, name: string, coc_level: int, permissions: string[], member_count: int }] }` | 401, 403, 404 | Session + workspace-in-org + `roles:manage` |
| `/api/v1/workspaces/:id/roles/:roleId/permissions` | PUT | `{ permissions: string[] }` | `{ id: UUID, name: string, coc_level: int, permissions: string[] }` | 400, 401, 403, 404, 409 | Session + workspace-in-org + `roles:manage` + escalation guard |
| `/api/v1/workspaces/:id/memberships` | POST | `{ user_id: UUID, role_id: UUID }` | `{ id: UUID }` | 400, 401, 403, 404, 409 | Session + workspace-in-org + `roles:manage` |
| `/api/v1/workspaces/:id/memberships/:userId/:roleId` | DELETE | — | `204 No Content` | 401, 403, 404 | Session + workspace-in-org + `roles:manage` |

**Permission-string grammar:** flat `namespace:verb` or `tool_invoke:<peer_slug>:<tool_glob>`. The closed vocabulary is exactly 8 strings. Workspace-scoped (held in `roles.permissions`, checked by `require_permission`): `admin`, `audit:read`, `roles:manage`, `peers:manage`, `approvals:decide`, `workspace:write`, `tool_invoke:*:*` (with narrower `tool_invoke` scopes permitted). Org-scoped (held in `org_memberships.permissions`, checked by `require_org_permission`): `trust_issuers:manage`. The PUT endpoint manages workspace-scoped role permissions only and rejects (400) any string outside the workspace set (so `trust_issuers:manage` is not assignable via the role editor in v1 — org-membership grants are seeded by backfill and the claim-mapper, no management UI in this pass). `admin` short-circuits every workspace check; it is not an org grant.

**Escalation guard (409 `permission_escalation`):** PUT fails if the request adds a permission the actor does not hold in this workspace, or implies a `coc_level` raise above the actor's effective max. A caller holding `admin` is exempt from both clauses.

**Gates applied to pre-existing endpoints (no contract shape change, only an added 403 path):** audit aggregates/export → `audit:read`; trust issuers → `trust_issuers:manage`; `POST /approvals/:id` → `approvals:decide`; workspace patch/close → `workspace:write`; peer mutations → `peers:manage`; MCP `tool_invoke` dispatch → matching `tool_invoke` grant.

## Wiring dependency graph

```mermaid
graph LR
  RolesAdmin["RolesAdmin component (workspace admin UI)"] --> GetRoles["GET /workspaces/:id/roles"]
  RolesAdmin --> PutPerms["PUT /workspaces/:id/roles/:roleId/permissions"]
  RolesAdmin --> PostMem["POST /workspaces/:id/memberships"]
  RolesAdmin --> DelMem["DELETE /workspaces/:id/memberships/:userId/:roleId"]
  GetRoles --> ListWithCount["roles-with-member-count query"]
  PutPerms --> SetPerms["set_permissions + escalation check"]
  PostMem --> GrantMem["membership grant"]
  DelMem --> RevokeMem["membership revoke"]
  SetPerms --> AuditW["audit_events write (role.permissions.updated)"]
  GrantMem --> AuditW2["audit_events write (membership.granted)"]
  RevokeMem --> AuditW3["audit_events write (membership.revoked)"]
  ListWithCount --> RolesT[("roles + GIN(permissions)")]
  SetPerms --> RolesT
  GrantMem --> MemT[("memberships + new (workspace_id),(role_id) idx")]
  RevokeMem --> MemT
  subgraph Enforcement (Slices 0-3)
    AnyGated["workspace-gated handler / tool dispatch"] --> ReqPerm["require_permission(ctx, workspace_id, perm)"]
    ReqPerm --> EffPerms["effective_permissions(user, workspace) query"]
    EffPerms --> MemT
    EffPerms --> RolesT
    OrgGated["trust-issuer handler"] --> ReqOrg["require_org_permission(ctx, perm)"]
    ReqOrg --> OrgPerms["org_permissions(user, org) query"]
    OrgPerms --> OrgMemT[("org_memberships (new, Slice 1)")]
  end
```

## Tradeoffs

| Decision | Alternative | Why this wins |
|---|---|---|
| JSONB array on `roles.permissions` | Normalized `role_permissions` join table | Free read (already fetching the roles row), GIN-indexable containment, natural union across multi-role membership, glob in app code. Per-grant lineage is covered by `audit_events`. Join table deferred to Phase 2 if hard lineage is required. |
| Workspace-scoped resolution at call time | Keep session-global `active_role_id`, add permissions to it | Session-global is the C-1 defect; building on it ships finer-grained-but-still-wrong authz. Non-negotiable foundation. |
| Backfill `coc≥80` → full grant set | Flip routes to permission checks with no backfill | Without backfill every existing admin is locked out on deploy (fail-closed). Backfill makes the change access-preserving. |
| Closed ~6-string vocabulary | Open-ended permission bag | The JSONB column invites ABAC/resource-conditions scope creep; a closed vocabulary validated at the PUT boundary keeps it scaffolding. |
| Union permissions across multi-role membership | First/highest role wins | UNIQUE(user,workspace,role) already permits multiple roles; union is least-surprise (additive, never silently subtractive). |
| Fold H-1/H-2/H-3 mutation gates in now | Ship only the 4 existing coc sites | Shipping "RBAC" while any member can approve external tool calls or install rules is indefensible; these are the same `require_permission` helper, cheap to add. |

## Acceptance criteria

Each maps to an integration test against a seeded org with two workspaces and a multi-workspace user.

1. **C-1 fix:** Given user U with an `admin`-granted role in workspace A and a no-permission role in workspace B, when U calls an `audit:read`-gated endpoint on workspace B, then status is 403; on workspace A, 200.
2. **Backfill preserves access:** Given a role at `coc_level=90` and `permissions='{}'` before migration, when the migration runs and U (member of that role) calls every previously-`require_admin` endpoint, then each returns the same 2xx status it returned pre-migration.
3. **Fail-closed (empty grants):** Given a role with `permissions=[]` and `coc_level=0`, when its member calls an `audit:read`-gated endpoint, then 403.
4. **Fail-closed (no membership):** Given a user with no membership in the target workspace, when they call any gated endpoint on it, then 403.
5. **Vocabulary validation:** Given a `roles:manage` holder, when they PUT `permissions:["audit:read","bogus:perm"]`, then 400 `invalid_permission` and the role is unchanged.
6. **Escalation guard (permission):** Given actor A holding `["roles:manage","audit:read"]` (not `peers:manage`, not `admin`), when A PUTs `peers:manage` onto any role in the workspace, then 409 `permission_escalation`; when A grants `audit:read` (which A holds), then 200.
7. **Escalation guard (coc_level):** Given actor A with effective `MAX(coc_level)=50` and no `admin`, when A PUTs a change raising a role's `coc_level` to 90, then 409 `permission_escalation`.
8. **Tool-call gating:** Given a caller whose effective permissions for the workspace are `["tool_invoke:weather:*"]`, when they invoke `weather:get_forecast`, then the call dispatches; when they invoke `db:run_query`, then 403 before any peer request is made (assert no outbound peer call via the test peer's request log).
9. **Approval gate (H-1):** Given a member without `approvals:decide`, when they `POST /approvals/:id {decision:"approved"}`, then 403 and no `execute_pending_tool_call` dispatch occurs.
10. **Workspace-write gate (H-3):** Given a member without `workspace:write`, when they `PATCH` or close the workspace, then 403 and the workspace row is unchanged.
11. **Peers-manage gate (H-2):** Given a member without `peers:manage`, when they create, delete, or modify the allowlist of a peer, then 403 and no peer row is created or mutated.
12a. **Org-scoped resolution (trust issuers):** Given user U who holds a `coc≥80`-backfilled role in workspace A (and thus an `org_memberships` row granting `trust_issuers:manage`), when U calls a trust-issuer endpoint, then non-403; given user V who is a member of workspace A but has no `org_memberships` row, when V calls the same endpoint, then 403.
12. **Member-count list:** Given a workspace with roles R1 (2 members) and R2 (0 members), when a `roles:manage` holder GETs `/roles`, then both appear with `member_count` 2 and 0 and their `permissions` arrays.
13. **Audit trail:** Given a `roles:manage` holder who PUTs a new permission set, when the call returns 200, then an `audit_events` row exists with verb `role.permissions.updated` and payload containing `before` and `after` arrays and the actor's user id.
14. **UI probe-and-hide:** Given a non-`roles:manage` member, when they open the workspace admin surface, then the Roles section is absent and no role endpoint is called after the first 403 probe.

## Open questions

1. **Org-scoped permissions (trust issuers) — RESOLVED.** Introduce a thin `org_memberships(user_id, org_id, permissions JSONB)` table (Slice 1) with a `require_org_permission` resolver; `trust_issuers:manage` is the only org-scoped grant in v1. Backfill grants it to anyone holding a `coc≥80` role in any workspace of the org, exactly preserving current trust-issuer access. No org-role management UI in this pass (grants come from backfill + claim-mapper); add one when a second org-scoped permission appears.
2. **Deferred security findings (named, not dropped):** H-4 `list_workspaces` membership filter, M-1 `admin_funnel` gate, M-2 connector-validate SSRF gate, L-3 OAuth-client ownership filter. Fast-follow after this lands; each is a one-line gate. Tracked here so they are not silently lost.
3. **MCP bearer workspace binding (C-2 depth).** Slice 3 resolves the role from the validated workspace id inside the tool handler. A cleaner long-term fix encodes the workspace in the JWT claim and resolves at auth time. v1 takes the handler-lookup path; flag the claim-binding option for Phase 2.
4. **Retire `coc_level` as a predicate.** This design stops using it for access but leaves the column. Confirm no other code path keys access off `coc_level` before declaring it display-only.

## Commercial linkage

Per Path 2, RBAC is expansion infrastructure for domain-app engagements: the second/third team deployment needs differentiated access (operator vs auditor vs read-only) before an org expands scope. For DICE, §2.4's "agents can call only granted tools" becomes a live, demonstrable control (criterion 6 is the evidence artifact) and the audit-gateway's `audit:read` gate strengthens the NIST 800-171 AU-9 narrative. Framed as the integration fabric's access layer, never as standalone enterprise IAM (Path 2 prohibition).

## Requirements impact

Create `md/requirements/active/rbac.md` carrying: the permission vocabulary (closed 8-string list + grammar + workspace-vs-org scoping), the `org_memberships` table shape, the four management endpoint contracts, the escalation-guard rule (incl. the `admin` exemption), and the gate-to-permission mapping for pre-existing endpoints. The audit-export requirements doc's authz-tier note is updated to reference `audit:read` (done in this design pass).

---

## Devil's Advocate

**1. What assumption, if wrong, invalidates the design?**
That `roles.permissions` can be made the source of truth for access *and* that resolving permissions per `(user, workspace)` from `memberships`+`roles` is correct and cheap. If, instead, the real authorization identity at the federation/MCP boundary is not tied to a workspace membership at all (e.g., bearer tokens that have no membership row), then the whole "resolve role for (user, workspace)" model can't gate the tool-call path — the DICE-differentiating slice — and Slice 3 would need a different identity source.

**2. Verified against live state?**
Two checks. (a) C-1, the foundation premise: `resolve_active_role_id` ([auth.rs:283](src/auth.rs#L283)) is `(pool, user_id)` only, no workspace param, resolved once at [auth.rs:236](src/auth.rs#L236) — **VERIFIED ✓** by reading; the session-global flaw is real. (b) The MCP-boundary identity premise (the riskier one): the security review found all three `resolve_auth` branches emit `active_role_id: None` ([mcp_server.rs:208](src/mcp_server.rs#L208)) and the bearer path carries `user_id`+`org_id` but no membership-resolved role — **VERIFIED ✓**. This is exactly why Slice 3 resolves permissions from the *validated workspace id inside the tool handler* rather than from `AuthContext`: the design already accommodates the refuting case. The residual risk (a bearer principal with no membership row in the target workspace) resolves to fail-closed 403 by Slice 0's "no membership → 403" rule, which is the safe outcome.

**3. Simplest alternative that avoids the biggest risk?**
Fix only C-1 (make `require_admin` workspace-scoped) and migrate the audit endpoints to a single `audit:read` check — skip vocabulary, federation gating, and the management UI. This is genuinely tempting and is the correct *first commit*. Why the fuller design is still worth it: the DICE §2.4 commitment is specifically about tool-call gating (Slice 3), which the minimal version omits entirely, and shipping audit-only RBAC while H-1/H-2/H-3 leave external-effect mutations ungated is a worse security posture than it looks. The slices are independently shippable, so the build order *is* the minimal alternative first (Slice 0 → 1) then the differentiator (3) then management (4) — the design is the minimal fix plus a committed, ordered path, not a big-bang.

**4. Structural completeness checklist**
- [x] Every UI call (RolesAdmin: list, set-permissions, grant, revoke) appears in the API contract table.
- [x] Every contract endpoint maps to a named repo query (roles-with-count, set_permissions, membership grant/revoke); enforcement uses the effective-permissions query (Slice 0).
- [x] New data surfaced (`permissions[]`, `member_count`) appears across DB (existing column / COUNT), API (contract rows), and UI (permission chips, member counts). No new persistent columns.
- [x] Each acceptance criterion names an endpoint + expected status/payload (criteria 1–14 plus 12a).
- [x] Wiring graph is unbroken UI → endpoint → query → table, including the enforcement subgraph (workspace + org paths) shared by Slices 0–3.
- [x] Integration scenarios cover one full path per slice: criterion 1 (Slice 0 workspace resolver), 2 + 12a (Slice 1 backfill + org resolver), 8 (Slice 3), 9–11 (Slice 2: approvals/workspace/peers), 12–13 (Slice 4), with 3/4/5/6/7/14 as cross-cutting authz/validation/UI.
