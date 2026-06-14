# RBAC Scaffolding — Implementation Plan

**Design doc:** `md/design/rbac-scaffolding.md`
**Shape:** medium-large (db + api + ui, ~18 files; 5 vertical-slice phases, no task manifest — phases are sequential and dependency-chained, not parallel; no contract file)
**Stack:** Rust/Axum + Postgres (sqlx, embedded `sqlx::migrate!`) + static HTML/JS UI. Integration tests spawn the app against Postgres on `localhost:5433` (`tests/phase08_auth.rs`, `tests/audit_export_integration.rs::spawn_app` pattern). Server boot needs `IONE_TOKEN_KEY` **and** `IONE_WEBHOOK_SECRET_KEY` (both `AAAA…=` in `.env.example`).

## Dependencies

None. `serde_json`, `sqlx` JSONB, `uuid` all present. `Role.permissions` is already `serde_json::Value` ([models/role.rs:11](src/models/role.rs#L11)) and `RoleRepo` already `RETURNING permissions` — the column is wired into the model, just never read for authz.

## Resolved-at-plan-time facts (verified against working tree)

- `require_admin` ([auth.rs:294](src/auth.rs#L294)) reads `ctx.active_role_id` (session-global, resolved once at [auth.rs:236](src/auth.rs#L236) via `resolve_active_role_id(pool, user_id)` — **no workspace filter**, the C-1 defect). 6 call sites total (audit_aggregates ×2, audit_export ×1, admin/trust_issuers ×3 — verified by grep).
- `route_tool_call` ([federation.rs:99](src/services/federation.rs#L99)) carries `workspace_id` + `&AuthContext` and resolves the peer at line 109 — gating hooks cleanly after line 110, before `invoke_peer_tool`/`create_pending_tool_call`.
- MCP tool handlers carry `active_role_id: None` (all `resolve_auth` branches, [mcp_server.rs:208-209](src/mcp_server.rs#L208)); the `TODO(role-filter)` markers are at [mcp_server.rs:743,858](src/mcp_server.rs#L743).
- Handlers to gate: `decide_approval` ([approvals.rs:51](src/routes/approvals.rs#L51)), `patch_workspace` ([workspaces.rs:112](src/routes/workspaces.rs#L112)), `close_workspace` ([workspaces.rs:252](src/routes/workspaces.rs#L252)), `create_peer` ([peers.rs:63](src/routes/peers.rs#L63)), `delete_peer`, `authorize_allowlist`, `subscribe_peer` ([peers.rs:272](src/routes/peers.rs#L272)).
- `list_roles` already mounted at `GET /api/v1/workspaces/:id/roles` ([mod.rs:220](src/routes/mod.rs#L220)) → extend, don't recreate.
- UI: workspace shell uses `tab-*`/`panel-*` + `switchTab()`; the audit panel (just shipped) is the reference pattern for a new admin tab with probe-and-hide.
- Migration numbering: `0038` exists (audit export). Next free is **0039**.

## Phases

### Phase 1 — Workspace + org permission resolution (design Slice 0; fixes C-1)

**Goal:** every gate resolves the caller's role per `(user, workspace)` (or `(user, org)`), not from a session-global role. No new user-facing behavior; existing `require_admin` sites become workspace-correct.

**Files:**
- `migrations/0039_rbac.sql` — **create** (full migration; used by Phases 1–2). Creates `org_memberships`, backfills both grant sets, adds indexes. (Listed here because Phase 1's resolver query reads `org_memberships`; the backfill that preserves access is the Phase 2 acceptance concern but the DDL ships once.)
- `src/repos/role_repo.rs` — add `effective_permissions(user_id, workspace_id) -> (HashSet<String>, i32)`:
```sql
SELECT jsonb_agg(r.permissions) AS perms, COALESCE(MAX(r.coc_level),0) AS max_coc
FROM memberships m JOIN roles r ON r.id = m.role_id
WHERE m.user_id = $1 AND m.workspace_id = $2
```
  Flatten the array-of-arrays into a `HashSet<String>` in Rust.
- `src/repos/org_membership_repo.rs` — **create.** `org_permissions(user_id, org_id) -> HashSet<String>` (`SELECT permissions FROM org_memberships WHERE user_id=$1 AND org_id=$2`), plus `grant(user_id, org_id, perms)` for backfill/claim-mapper use.
- `src/repos/mod.rs` — export `OrgMembershipRepo`.
- `src/auth.rs` — add `require_permission(ctx, pool, workspace_id, perm) -> Result<(), AppError>` and `require_org_permission(ctx, pool, perm)`. Both fail-closed (no membership / empty / missing → `AppError::Forbidden`). `admin` in the workspace set short-circuits `require_permission`. Re-implement `require_admin` as `require_permission(ctx, pool, workspace_id, "admin")` — **signature gains `workspace_id`**, so update its 6 call sites accordingly (audit sites pass their path workspace; trust-issuer sites move to `require_org_permission` in Phase 2, so leave them on a temporary `require_admin_legacy` shim this phase to keep them compiling — see note).
- **Permission matching:** exact string match; for `tool_invoke:a:b` use segment-glob (`*` per `:`-segment). One helper `permission_grants(held: &HashSet<String>, needed: &str) -> bool`.

**Note on the 6 call sites:** the 3 audit sites convert to `require_permission(…, "audit:read")` this phase (they're workspace-scoped, unblocked). The 3 trust-issuer sites are org-scoped (Phase 2) — keep them calling the old coc-based logic via a renamed `require_admin_legacy` until Phase 2 flips them, so the tree compiles at every phase boundary.

**Gate:** `cargo test --test phase08_auth` + `cargo clippy --all-targets -- -D warnings`.
**Acceptance (design AC-1):** new test `phase08_auth::rbac_workspace_scoped_admin` — user admin in WS-A, member in WS-B; `audit:read`-gated call on B → 403, on A → 200.

---

### Phase 2 — Vocabulary + org-membership backfill (design Slice 1; access-preserving)

**Goal:** existing `coc≥80` users keep identical access through the cutover; trust-issuer gating moves to org scope.

**Files:**
- `migrations/0039_rbac.sql` — (created Phase 1) contents:
```sql
-- (a) workspace admin grant set
UPDATE roles SET permissions =
  '["admin","audit:read","roles:manage","peers:manage","approvals:decide","workspace:write","tool_invoke:*:*"]'::jsonb
  WHERE coc_level >= 80 AND permissions = '{}'::jsonb;
-- (b) org membership table
CREATE TABLE org_memberships (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  org_id  UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
  permissions JSONB NOT NULL DEFAULT '[]'::jsonb,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  UNIQUE (user_id, org_id)
);
-- (c) org backfill: anyone with a coc>=80 role in any workspace of the org
INSERT INTO org_memberships (user_id, org_id, permissions)
SELECT DISTINCT m.user_id, w.org_id, '["trust_issuers:manage"]'::jsonb
FROM memberships m JOIN roles r ON r.id=m.role_id AND r.coc_level>=80
                   JOIN workspaces w ON w.id=m.workspace_id
ON CONFLICT (user_id, org_id) DO UPDATE
  SET permissions = org_memberships.permissions || '["trust_issuers:manage"]'::jsonb;
CREATE INDEX roles_permissions_gin ON roles USING gin (permissions);
CREATE INDEX memberships_workspace_id ON memberships (workspace_id);
CREATE INDEX memberships_role_id ON memberships (role_id);
```
- `src/routes/admin/trust_issuers.rs` — 3 sites flip from `require_admin_legacy` → `require_org_permission(ctx, &state.pool, "trust_issuers:manage")`. Delete the `require_admin_legacy` shim from `auth.rs` once these are the last consumers.
- `src/repos/role_repo.rs` — `upsert` gains a `permissions` write: when `coc_level >= 80`, set the workspace admin grant set inline (so post-migration IdP-issued admin roles are granted, not just backfilled ones).
- `src/services/` claim-mapper / bootstrap path — where a `coc≥80` role is created for a federated user, also `OrgMembershipRepo::grant(user, org, ["trust_issuers:manage"])`. (Find the create site; it's the membership-upsert path.)

**Gate:** `cargo test --test phase08_auth --test phase12_peer` + clippy + a one-shot migration apply (`sqlx migrate run` against the dev DB) that exits 0.
**Acceptance (design AC-2, AC-12a):** `phase08_auth::rbac_backfill_preserves_access` (a `coc=90`/`permissions='{}'` role, post-migration, returns the same 2xx on every previously-`require_admin` endpoint) and `rbac_org_scoped_trust_issuers` (backfilled admin → non-403 on trust issuers; workspace-only member → 403).

---

### Phase 3 — Route-handler gates for ungated mutations (design Slice 2; H-1/H-2/H-3)

**Goal:** the three external-effect mutation classes deny callers lacking the named permission.

**Files:**
- `src/routes/approvals.rs` — in `decide_approval`, after the workspace is resolved (~line 117) and before `execute_pending_tool_call`, add `require_permission(&auth, &state.pool, workspace_id, "approvals:decide").await?`.
- `src/routes/workspaces.rs` — `patch_workspace` and `close_workspace`: `require_permission(…, workspace_id, "workspace:write")` after `ensure_workspace_in_org`.
- `src/routes/peers.rs` — `create_peer`, `delete_peer`, `authorize_allowlist`, `subscribe_peer`: `require_permission(…, workspace_id, "peers:manage")`. **Caveat:** peer create/delete are org-scoped (no workspace in path) — gate `create_peer`/`delete_peer`/`authorize_allowlist` with `require_org_permission(ctx, "peers:manage")` if they lack a workspace id, and `subscribe_peer` (has workspace) with workspace-scoped `peers:manage`. Confirm per-handler signature; **add `peers:manage` to BOTH the workspace admin grant set and the org backfill set** so existing admins keep peer access (update the Phase 2 migration's two grant arrays before running it — peers can be either-scoped).
- `tests/phase14_bindings.rs` or `tests/phase12_peer.rs` — extend with gate tests.

**Decision to resolve during coding (not blocking the plan):** whether `peers:manage` is workspace- or org-scoped. Peer rows are org-scoped (`ensure_peer_in_org`), so **org-scoped is correct**; `subscribe_peer` additionally needs workspace `peers:manage` since it binds a peer to a workspace. Put `peers:manage` in the org backfill set; keep it out of the workspace admin set to avoid a split-brain grant. (This supersedes the either/or hedge above — implementer: org-scoped `peers:manage`.)

**Gate:** `cargo test --test phase12_peer --test phase09_delivery` + clippy.
**Acceptance (design AC-9, AC-10, AC-11):** named gate tests — member without each permission gets 403 and the side effect (tool execution / workspace mutation / peer row) does not occur.

---

### Phase 4 — Federation tool-call gating (design Slice 3; DICE §2.4 / C-2)

**Goal:** a caller may invoke `peer:tool` only if they hold a matching `tool_invoke:<peer_slug>:<tool_glob>` grant; denial happens before any outbound peer call.

**Files:**
- `src/services/federation.rs` — in `route_tool_call`, after `peer` is resolved (line 109) and the workspace/peer binding is checked (line 110), resolve the caller's effective permissions for `workspace_id` and require `tool_invoke:{peer.name}:{raw_tool}` via the segment-glob matcher. No match → return an error that surfaces as 403. Peer slug = `peer.name`.
- `src/mcp_server.rs` — the MCP tool handlers that reach federated tools (the `TODO(role-filter)` sites, [743,858](src/mcp_server.rs#L743)): since `active_role_id: None` on these paths, resolve permissions from the validated `workspace_id` + `user_id` directly (call `RoleRepo::effective_permissions`) rather than from `AuthContext`. Apply the same matcher before dispatch.
- `tests/phase11_mcp_server.rs` / `tests/phase12_peer.rs` — gating test with a test peer whose request log proves no outbound call on denial.

**Gate:** `cargo test --test phase11_mcp_server --test phase12_peer` + clippy.
**Acceptance (design AC-8):** caller with `["tool_invoke:weather:*"]` → `weather:get_forecast` dispatches; `db:run_query` → 403 and the test peer received zero requests.

---

### Phase 5 — Role-management API + admin UI (design Slice 4; escalation-safe)

**Goal:** a `roles:manage` holder can view roles with permissions + member counts, edit a role's permission set, and grant/revoke memberships — from the workspace shell, escalation-guarded, fully audited.

**Files:**
- `src/repos/role_repo.rs` — `list_with_member_count(workspace_id)` (LEFT JOIN memberships, `COUNT(m.id)`, GROUP BY r.id) and `set_permissions(role_id, workspace_id, perms) RETURNING …`.
- `src/repos/membership_repo.rs` — `grant(user_id, workspace_id, role_id) ON CONFLICT DO NOTHING RETURNING id` and `revoke(user_id, workspace_id, role_id)`.
- `src/routes/roles.rs` — **create.** `list_roles_detailed` (replaces/extends the mounted `list_roles`), `put_role_permissions`, `post_membership`, `delete_membership`. All `require_permission(…, "roles:manage")`. **Escalation guard:** load actor's effective perms+max_coc for the workspace; reject (409 `permission_escalation`) any added permission the actor lacks or any `coc_level` raise above actor max; **`admin` holders are exempt.** Validate every incoming permission string against the closed workspace vocabulary (400 `invalid_permission`). Each write emits an `audit_events` row (`role.permissions.updated` / `membership.granted` / `membership.revoked`, payload per design §"Audit trail convention") via `AuditEventRepo`.
- `src/routes/mod.rs` — point `GET …/roles` at the detailed handler; add `PUT …/roles/:roleId/permissions`, `POST …/memberships`, `DELETE …/memberships/:userId/:roleId`.
- `static/index.html` — new `tab-roles` + `panel-roles` (mirror `panel-audit`): role list with permission chips + member counts, a permission-toggle editor, member grant/revoke controls. `hidden` by default.
- `static/app.js` — `loadRoles()` probe-and-hide on 403 (the `auditAdminDenied` pattern → `rolesAdminDenied`), render, wire PUT/POST/DELETE.
- `static/style.css` — permission-chip + roles-panel styles.
- `md/requirements/active/rbac.md` — **create.** Vocabulary (8 strings + workspace/org scoping), `org_memberships` shape, the four management contracts, escalation guard (incl. `admin` exemption), gate-to-permission map. (Satisfies the coverage-gate requirements-doc check.)
- `tests/rbac_integration.rs` — **create** (`_integration` suffix for the pre-PR coverage gate; `spawn_app` per `tests/audit_export_integration.rs`). Covers AC-3,4,5,6,7,12,13.
- `tests/e2e/roles-panel.spec.ts` — **create.** AC-14 probe-and-hide (mirror `tests/e2e/audit-panel.spec.ts`).

**Gate:** `cargo test --test rbac_integration` + `cargo clippy --all-targets -- -D warnings` + `npx playwright test tests/e2e/roles-panel.spec.ts` (server up on :3007).
**Acceptance:** AC-3,4,5,6,7,12,13 green in `rbac_integration`; AC-14 green in the Playwright spec.

---

## Acceptance-criteria → phase map (self-review, step 7)

| Design AC | Phase | Gate test |
|---|---|---|
| 1 (C-1 workspace-scoped) | 1 | `phase08_auth::rbac_workspace_scoped_admin` |
| 2 (backfill preserves access) | 2 | `phase08_auth::rbac_backfill_preserves_access` |
| 12a (org-scoped trust issuers) | 2 | `phase08_auth::rbac_org_scoped_trust_issuers` |
| 3,4 (fail-closed empty / no-membership) | 1 | `phase08_auth::rbac_fail_closed_*` |
| 5 (vocabulary validation 400) | 5 | `rbac_integration::put_rejects_unknown_perm` |
| 6 (escalation: permission) | 5 | `rbac_integration::escalation_permission_409` |
| 7 (escalation: coc_level) | 5 | `rbac_integration::escalation_coc_409` |
| 8 (tool-call gating) | 4 | `phase12_peer::tool_invoke_gated` |
| 9 (approvals:decide H-1) | 3 | `phase12_peer::approval_gate_403` |
| 10 (workspace:write H-3) | 3 | `phase03_workspaces::workspace_write_gate_403` |
| 11 (peers:manage H-2) | 3 | `phase12_peer::peers_manage_gate_403` |
| 12 (member-count list) | 5 | `rbac_integration::roles_with_member_count` |
| 13 (audit trail) | 5 | `rbac_integration::permission_change_audited` |
| 14 (UI probe-and-hide) | 5 | `roles-panel.spec.ts` |

Every design AC maps to a phase gate. Every cited file exists except those marked **create**. Phases are vertical slices (each ships its DB+API+enforcement+test together; Phase 1 is the foundation, the one allowed pure-scaffolding-ish phase but it still ships the resolver + a behavioral test). Gates are concrete commands.

## Notes for /code-the-plan

- **Environment preflight before Phase 1:** `pg_isready -h localhost -p 5433`; server boot for e2e needs `IONE_TOKEN_KEY` **and** `IONE_WEBHOOK_SECRET_KEY` (the playwright.config comment omits the latter — it's required).
- **Compile-at-every-boundary:** `require_admin`'s signature changes in Phase 1 (gains `workspace_id`). The `require_admin_legacy` shim keeps the 3 trust-issuer sites compiling until Phase 2 deletes it. Don't leave a phase with a non-compiling tree.
- **The migration is authored in Phase 1 but its access-preservation is a Phase 2 acceptance gate** — apply it once; both phases' tests run against the migrated schema.
- **`peers:manage` is org-scoped** (peer rows are org-scoped) — final decision in Phase 3; put it in the org backfill set, not the workspace admin set.
- Branch hygiene: start from a clean `main` (the `feature/audit-event-export` branch and its RFP-tree contamination are a separate cleanup — do not branch RBAC off it).
- Mark the backlog P4 RBAC item **Partial — pending walkthrough** when code-complete; "shipped" needs the founder walkthrough.
