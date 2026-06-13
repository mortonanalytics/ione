# Requirements — Headless Provisioning

**Source design:** `md/design/headless-provisioning.md`
**Plan:** `md/plans/headless-provisioning-plan.md`
**Status:** in implementation on `feature/headless-provisioning`

Machine-client auth + atomic, idempotent declarative provisioning. Two org-scoped
permissions (`service_accounts:manage`, `provisioning:apply`) live in the closed
RBAC vocabulary (`md/requirements/active/rbac.md`).

## `service_account_tokens` table (migration 0041)

`id UUID PK · org_id UUID FK organizations ON DELETE RESTRICT · name TEXT · token_hash TEXT · permissions JSONB DEFAULT '[]' CHECK(array) · provisionable_max_coc INT DEFAULT 0 · created_by UUID FK users ON DELETE SET NULL · expires_at · revoked_at · last_used_at · created_at · updated_at · UNIQUE(org_id, name)`

- `token_hash` is the SHA-256 hex of the plaintext (`ione_sat_<base64url(32 bytes)>`). The plaintext is shown **once** at issuance and never stored.
- Indexes: `UNIQUE(token_hash)`, partial `(org_id) WHERE revoked_at IS NULL`, GIN on `permissions`. `BEFORE UPDATE` trigger touches `updated_at`. Org-isolation RLS policy (dormant — same as the rest; app-layer `WHERE org_id` is the real enforcement).
- Migration 0041 adds `actor_kind` enum value `service_account` and backfills `["service_accounts:manage","provisioning:apply"]` into `org_memberships` for any user holding a `permissions @> ["admin"]` role. `RoleRepo::ORG_ADMIN_GRANTS` carries both forward for post-migration admin roles.

### Lifecycle
- **Issue** (`POST`): returns plaintext + id once.
- **List** (`GET`): never returns `token_hash` or plaintext (`ServiceAccountToken` skips `token_hash` on serialization).
- **Revoke** (`DELETE`): soft-delete (`revoked_at = now()`).
- **Expire**: `expires_at` in the past → token rejected at auth.
- Rotation = issue-new + revoke-old (no rotation endpoint).

## Headless auth (`src/auth.rs`)

`AuthContext` gains `is_service_account: bool`, `service_account_token_id: Option<Uuid>`, `permissions: Vec<String>` (empty for non-SA contexts). `auth_middleware` checks `Authorization: Bearer ione_sat_…` **before** the session/default branch: SHA-256 the value, `ServiceAccountTokenRepo::verify` (not-revoked, not-expired), and on success build a synthetic context (`user_id = nil`, token's `org_id`, `is_oidc=false`, `mfa_verified=true`, `is_service_account=true`, `service_account_token_id=Some`, `permissions` from the token). **Fail-closed**: once an `ione_sat_` bearer is presented, an unknown/expired/revoked value returns 401 (never falls back to the default user). `last_used_at` is touched fire-and-forget.

`require_permission` / `require_org_permission` short-circuit on `ctx.is_service_account` — they check `permission ∈ ctx.permissions` (with the `admin` short-circuit + segment-glob matcher) and skip the membership join (the synthetic `user_id` has none). `mfa_gate` passes through for service accounts.

## API contracts

| Endpoint | Method | Request | Response | Errors | Auth |
|---|---|---|---|---|---|
| `/api/v1/service-account-tokens` | POST | `{ name, permissions:string[], provisionableMaxCoc:int, expiresAt?:ISO8601 }` | `201 { id, token (once), name, permissions, provisionableMaxCoc, expiresAt }` | 400, 401, 403, 409, 422 | Session/SA + `service_accounts:manage` + escalation guard |
| `/api/v1/service-account-tokens` | GET | — | `{ items:[{ id, name, permissions, provisionableMaxCoc, lastUsedAt, expiresAt, revokedAt, createdAt, createdBy }] }` (no hash/plaintext) | 401, 403 | Session/SA + `service_accounts:manage` |
| `/api/v1/service-account-tokens/:tokenId` | DELETE | — | `204` | 401, 403, 404 | Session/SA + `service_accounts:manage` |
| `/api/v1/provision` | POST | `{ version:"v1", workspace:{name,domain?,lifecycle?,metadata?}, roles?:[{name,cocLevel,permissions}], connectors?:[{name,kind,config}] }` | `{ workspaceId, created:[{kind,id,name}], updated:[{kind,id,name,changedFields}], unchangedCount }` | 400, 401, 403, 409, 422 | Session/SA + `provisioning:apply` + escalation guard |

### Issuance escalation guard
Issued `permissions ⊆ issuer's` and `provisionableMaxCoc ≤ issuer's effective MAX(coc_level)` (else 409 `permission_escalation`). A session actor holding `admin` is exempt; a service-account issuer is **never** exempt. Issuer authority = org grants ∪ effective workspace grants across the issuer's memberships in the org. Permissions are validated against the closed vocabulary (workspace strings + the four org strings) → 422 on an unknown string. Issuance and revocation each write a `service_account_token.issued` / `.revoked` audit row (`workspace_id` NULL; org id in payload).

## Provisioning (`src/services/provisioning.rs`)

`apply` runs the whole spec in **one transaction** under `pg_advisory_xact_lock(hashtext('ione_provision'), hashtext(org_id))` — concurrent re-applies of the same org's spec serialize, parallel across orgs. Any error rolls everything back; nothing persists (422/409 names the failing entity).

**Merge semantics** — never deletes unlisted resources. Each entity is read-then-written within the lock:
- match missing → **created**; present-and-different → **updated** (`changedFields` listed); present-and-identical → **unchanged** (counted).

**Idempotency keys:** workspace on `(org_id, name)`; role and connector on `(workspace_id, name)`. Roles' `coc_level` + `permissions` are updated to spec values on every apply. The response is identical for create vs already-exists and never reveals another org's name collision (HP-M5).

**Escalation guard (token-as-actor, never exempt):** every spec role's `permissions ⊆ actor.permissions` and `coc_level ≤ actor's ceiling` (a service-account token's `provisionable_max_coc`; a session actor's effective max coc) → 409.

**Creator-membership:** a session-actor provisioner is granted a membership in the workspace on a synthesized `provisioner` role capped at its own permissions/coc, so it can manage what it provisioned. A **service-account** principal has `user_id = nil` (no `users` row, so no membership is possible); its "manage what you provisioned" property holds via the carried-permission short-circuit in `require_permission` instead — the observable outcome (a `roles:manage`-gated call on the new workspace authorizes) is identical.

**Audit:** one `provisioning.applied` row per run (`actor_kind = service_account`, `actor_ref = token id`, `workspace_id` = the provisioned workspace), payload `{ org_id, spec_name, created, updated, unchanged_count }`. Connector configs are **never** written to the payload.

## Deferred — `peers`, `bindings`, `auto_exec_policies` spec sections

The spec schema accepts `peers`, `bindings`, and `auto_exec_policies` keys for forward-compatibility, but a **non-empty** section is rejected with `422 unsupported_spec_section` (not silently dropped).

**Why deferred:** `peers` require resolving a `trust_issuers` row (FK `issuer_id NOT NULL`); `auto_exec_policies.created_by` is `NOT NULL REFERENCES users(id)`, which a service-account principal (`user_id = nil`) cannot satisfy.

**Re-entry gate:** a follow-up that (a) makes `auto_exec_policies` attributable to a token (nullable `created_by` or a `created_by_token_id` column) and (b) defines peer trust-issuer resolution in the spec. Until then these entity types are provisioned via their own endpoints.

## UI

Org-scoped **Tokens** settings panel (`tab-tokens`/`panel-tokens`): list (name, permissions, last-used, expiry), issue form with a copy-once plaintext modal, revoke control. Visibility via a `service_accounts:manage` probe-and-hide (one `GET` probe; 403 → tab hidden, no further calls).
