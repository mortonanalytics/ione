# Headless Provisioning — Implementation Plan

**Design doc:** `md/design/headless-provisioning.md`
**Shape:** large by file count (~20 files) but **strictly sequential** (Slice 1 auth is a hard prerequisite for 2–3) — phases as vertical slices, no task manifest, no contract file (no parallel agents).
**Stack:** Rust/Axum + Postgres (sqlx, embedded `sqlx::migrate!`) + static HTML/JS UI. Integration tests `#[ignore]`-gated, run `--ignored --test-threads=1` with `IONE_SKIP_LIVE=1`; server boot needs `IONE_TOKEN_KEY` + `IONE_WEBHOOK_SECRET_KEY`.

## Dependencies

None. `sha2` (token hashing, [mcp_bearer.rs:90](src/middleware/mcp_bearer.rs#L90)), `base64`, `uuid`, sqlx JSONB all present. Reuses RBAC's `require_permission`/`require_org_permission`, the escalation-guard helper in `src/routes/roles.rs`, `bootstrap.rs`'s `pg_advisory_xact_lock` pattern, and the existing create repos.

## Resolved-at-plan-time facts (verified against working tree)

- `AuthContext` ([auth.rs:31](src/auth.rs#L31)) has `user_id, org_id, is_oidc, is_mcp_peer, active_role_id, session_id, mfa_verified`. Built in `auth_middleware` ([auth.rs:239](src/auth.rs#L239)) from a session cookie or the default user; `enforce_auth` ([auth.rs:253](src/auth.rs#L253)) rejects non-`is_oidc` in OIDC mode; both + `mfa_gate` ([routes/mod.rs:400](src/routes/mod.rs#L400)) are `route_layer`s on the `protected` router ([mod.rs:339-341](src/routes/mod.rs#L339)).
- `OauthContext` ([mcp_bearer.rs:17](src/middleware/mcp_bearer.rs#L17)) is a separate struct (no `org_id`) — not reused; SA tokens build a real `AuthContext`. `sha256_hex` is at [mcp_bearer.rs:90](src/middleware/mcp_bearer.rs#L90).
- Unique constraints **absent** (verified): `workspaces` (no `(org_id,name)`), `connectors` (no `(workspace_id,name)`), `peers` (only `UNIQUE(mcp_url)`). The `actor_kind` enum is `user/system/peer` (migration 0009) — needs `service_account`.
- `create_connector` ([routes/connectors.rs](src/routes/connectors.rs)) calls `ensure_workspace_in_org` but not `require_permission(…, "workspace:write")` (HP-H1).
- Migration numbering: 0040 is auto_exec_policies; next free **0041** (tokens), then **0042** (constraints).
- RBAC requirements + audit-export requirements docs already updated (this design pass) with the two new permissions and three new audit verbs.
- UI shell: `tab-*`/`panel-*` + `switchTab()` + probe-and-hide (roles/policies panels are the reference).

## Phases

### Phase 1 — Service-account tokens + headless auth (design Slice 1; prerequisite for all)

**Goal:** a machine client authenticates with `Authorization: Bearer ione_sat_…` and reaches RBAC-gated endpoints with the token's org + permissions.

**Files:**
- `migrations/0041_service_account_tokens.sql` — **create.** Table + RLS + GIN per design; `ALTER TYPE actor_kind ADD VALUE 'service_account'` (own statement — `ADD VALUE` can't be used in the same txn it's added); backfill `["service_accounts:manage","provisioning:apply"]` into `org_memberships.permissions` for users holding a `permissions @> '["admin"]'` role:
```sql
CREATE TABLE service_account_tokens (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  org_id UUID NOT NULL REFERENCES organizations(id) ON DELETE RESTRICT,
  name TEXT NOT NULL,
  token_hash TEXT NOT NULL,
  permissions JSONB NOT NULL DEFAULT '[]'::jsonb CHECK (jsonb_typeof(permissions)='array'),
  provisionable_max_coc INT NOT NULL DEFAULT 0,
  created_by UUID REFERENCES users(id) ON DELETE SET NULL,
  expires_at TIMESTAMPTZ, revoked_at TIMESTAMPTZ, last_used_at TIMESTAMPTZ,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(), updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  UNIQUE (org_id, name)
);
CREATE UNIQUE INDEX sat_token_hash_uniq ON service_account_tokens (token_hash);
CREATE INDEX sat_org_active ON service_account_tokens (org_id) WHERE revoked_at IS NULL;
-- BEFORE UPDATE touch updated_at; RLS org-isolation policy per existing pattern.
```
- `src/models/service_account_token.rs` — **create.** `ServiceAccountToken` (FromRow); never serializes `token_hash`.
- `src/repos/service_account_token_repo.rs` — **create.** `issue` (insert hash, return row + plaintext separately), `list_active(org_id)`, `revoke(id, org_id)`, `verify(token_hash) -> Option<token>` (where not-revoked, not-expired), `touch_last_used(id)` (fire-and-forget).
- `src/repos/mod.rs` — export it.
- `src/models/mod.rs` — add `ServiceAccount` to the `ActorKind` enum (matches the new DB enum value).
- `src/auth.rs` — add `is_service_account: bool` + `service_account_token_id: Option<Uuid>` to `AuthContext`. In `auth_middleware`, **before** the session/default branch, check `Authorization: Bearer ione_sat_…`: hash, `verify`, and if valid build the synthetic context (`user_id = Uuid::nil()`, token's `org_id`, `is_oidc=false`, `mfa_verified=true`, `is_service_account=true`, `service_account_token_id=Some(id)`); spawn `touch_last_used`. In `enforce_auth`, accept when `is_oidc || is_service_account`. In `require_permission` and `require_org_permission`: if `ctx.is_service_account`, check `permission ∈ ctx.permissions` (carry the token's permissions on the context — add a `permissions: Vec<String>` field or resolve via the token id) and skip the membership join.
- `src/routes/mod.rs` — `mfa_gate` passes through when `ctx.is_service_account`; mount `POST/GET /api/v1/service-account-tokens` + `DELETE /:tokenId`. Update all `AuthContext { … }` literals for the two new fields.
- `src/routes/service_account_tokens.rs` — **create.** `issue/list/revoke`. `issue`: gen 32 random bytes → `ione_sat_` + base64url → sha256 hash → store; **escalation guard** (issued `permissions ⊆ issuer's`, `provisionable_max_coc ≤ issuer's max_coc`; session `admin` exempt, SA issuer never); validate each permission against the closed vocab (incl. the two new org strings) → 422; audit `service_account_token.issued`/`.revoked`. Gated `require_org_permission(ctx, "service_accounts:manage")`.
- `src/routes/roles.rs` — extend the 0039-style admin grant set in `RoleRepo::upsert`/claim-mapper carry-forward to include the two new org perms (so post-migration admin roles get them). *(verify the exact carry-forward site)*
- `static/index.html` + `static/app.js` + `static/style.css` — `tab-tokens`/`panel-tokens`: list (name, permissions, last-used, expiry), create form with a **copy-once** plaintext modal, revoke control; `loadTokens()` probe-and-hide on 403 (`tokensAdminDenied`).
- `tests/headless_provisioning_integration.rs` — **create** (`_integration` suffix; `spawn_app` per `tests/audit_export_integration.rs`). AC-1, 2, 3, 4.
- `tests/e2e/tokens-panel.spec.ts` — **create.** AC-12 (mirror `roles-panel.spec.ts`).

**Gate:** `IONE_SKIP_LIVE=1 cargo test --test headless_provisioning_integration --test phase08_auth -- --ignored --test-threads=1` + `cargo clippy --all-targets -- -D warnings` + `npx playwright test tests/e2e/tokens-panel.spec.ts`.
**Acceptance:** AC-1 (token → 200 from `/provision`… or a simpler `service_accounts:manage`-gated GET until Phase 3 exists — see note), AC-2/3/4 green; AC-12 in Playwright.
*Note:* AC-1 references `/provision` (Phase 3). For Phase-1 isolation, assert the token authenticates against `GET /service-account-tokens` (a `service_accounts:manage`-gated endpoint that exists in this phase); re-point AC-1 at `/provision` once Phase 3 lands.

---

### Phase 2 — Idempotency constraints + connector gate (design Slice 2)

**Goal:** the upsert targets exist and the connector-create authz gap is closed.

**Files:**
- `migrations/0042_provisioning_unique_constraints.sql` — **create.**
```sql
ALTER TABLE workspaces ADD CONSTRAINT workspaces_org_id_name_key UNIQUE (org_id, name);
ALTER TABLE connectors ADD CONSTRAINT connectors_workspace_id_name_key UNIQUE (workspace_id, name);
ALTER TABLE peers ADD CONSTRAINT peers_org_id_name_key UNIQUE (org_id, name);
```
  (Dev/test DBs from bootstrap have no duplicates; preflight dedup queries documented in the design for prod.)
- `src/routes/connectors.rs` — add `require_permission(&auth, &state.pool, workspace_id, "workspace:write").await?` after `ensure_workspace_in_org` in `create_connector` (HP-H1).
- `tests/phase13_connectors.rs` — add `create_connector_requires_workspace_write` (member without `workspace:write` → 403).
- `tests/headless_provisioning_integration.rs` — add AC-9 (two applies changing a connector config → one row).  *(AC-9 fully exercised once Phase 3's `/provision` exists; in this phase, assert the constraint directly via a duplicate-insert returning a unique violation.)*

**Gate:** `IONE_SKIP_LIVE=1 cargo test --test phase13_connectors -- --ignored --test-threads=1` + a one-shot `sqlx migrate run` exit-0 + clippy.
**Acceptance:** `create_connector_requires_workspace_write` passes; duplicate `(workspace_id,name)` connector insert raises a unique violation.

---

### Phase 3 — Declarative provisioning endpoint (design Slice 3)

**Goal:** one call applies a workspace spec transactionally and idempotently.

**Files:**
- `src/services/provisioning.rs` — **create.** `apply(state, ctx, spec) -> ProvisionResult`. Opens a txn, takes `pg_advisory_xact_lock(hashtext('ione_provision'), hashtext(org_id::text))`, then upserts in dependency order: workspace → roles (escalation guard: each role's `permissions ⊆ ctx.permissions`, `coc_level ≤ token.provisionable_max_coc` → 409) → creator-membership (capped role) → connectors → peers → bindings → auto_exec_policies. Each upsert `ON CONFLICT … DO UPDATE … WHERE row IS DISTINCT FROM EXCLUDED`, classifying created/updated/unchanged. On any error → return Err → txn rolls back. After commit, one `provisioning.applied` audit row (`actor_kind=service_account`, `actor_ref=token id`, diff payload, no connector secrets).
- `src/routes/provision.rs` — **create.** `POST /api/v1/provision`: `require_org_permission(ctx, "provisioning:apply")`, parse spec (`version:"v1"`), call `provisioning::apply`, map errors (422 entity-named / 409 escalation), return the diff.
- `src/routes/mod.rs` — mount `POST /api/v1/provision`.
- `src/repos/*` — add upsert methods where missing (`WorkspaceRepo::upsert_by_org_name`, `ConnectorRepo::upsert_by_workspace_name`, `PeerRepo::upsert_by_org_name`); roles/bindings/policies reuse existing keys.
- `md/requirements/active/headless-provisioning.md` — **create.** Token table + lifecycle, the two permissions, both endpoint contracts, escalation guard, merge semantics + idempotency keys.
- `tests/headless_provisioning_integration.rs` — add AC-5 (idempotent apply), AC-6 (atomic rollback on bad connector kind), AC-7 (escalation: permission + coc), AC-8 (creator-membership capped), AC-10 (audit row, no secrets), AC-11 (cross-org name isolation); re-point AC-1 and AC-9 at `/provision`.

**Gate:** `IONE_SKIP_LIVE=1 cargo test --test headless_provisioning_integration -- --ignored --test-threads=1` + clippy.
**Acceptance:** AC-5/6/7/8/10/11 green; AC-1/AC-9 re-pointed and green.

---

## Acceptance-criteria → phase map (self-review, step 7)

| Design AC | Phase | Gate test |
|---|---|---|
| 1 (token → headless auth) | 1 (→ re-point 3) | `headless_provisioning_integration::token_authenticates` |
| 2 (token never re-exposed) | 1 | `…::token_list_omits_secret` |
| 3 (revocation → 401) | 1 | `…::revoked_token_401` |
| 4 (expiry → 401) | 1 | `…::expired_token_401` |
| 12 (UI probe-and-hide) | 1 | `tokens-panel.spec.ts` |
| 9 (constraint upsert) | 2 (→ 3) | `…::connector_upsert_single_row` |
| (H-1 connector gate) | 2 | `phase13_connectors::create_connector_requires_workspace_write` |
| 5 (idempotent apply) | 3 | `…::provision_idempotent` |
| 6 (atomic rollback) | 3 | `…::provision_rolls_back` |
| 7 (escalation guard) | 3 | `…::provision_escalation_409` |
| 8 (creator-membership) | 3 | `…::provision_grants_capped_membership` |
| 10 (audit, no secrets) | 3 | `…::provision_audited_no_secrets` |
| 11 (cross-org isolation) | 3 | `…::provision_cross_org_isolated` |

Every design AC maps to a phase gate. Cited files exist except those marked **create**. Phases are vertical slices (each ships DB+API+(UI)+tests for one capability; Phase 1 carries the UI since it's the only user-facing surface). Gates are concrete commands. Sequential — no manifest/contract file.

## Notes for /code-the-plan

- **Compile at every boundary:** the `AuthContext` two-field addition (Phase 1) breaks every `AuthContext { … }` literal and every match — fix them all in Phase 1 or the tree won't compile. `mcp_bearer.rs`'s `OauthContext` is untouched.
- **The `ADD VALUE 'service_account'` enum alter** must not be used in the same transaction it's declared — keep it as the first statement in 0041, before the table/backfill that don't reference it, or split. sqlx runs each migration file in one txn by default; if Postgres rejects in-txn `ADD VALUE` use, move the enum alter to its own migration `0041a`/renumber.
- **Permissions on the context:** `require_permission`'s SA short-circuit needs the token's permission list on `AuthContext`. Add a `permissions: Vec<String>` field (empty for non-SA contexts) populated only on the SA path — simplest; avoids a per-check token re-fetch.
- **Environment preflight:** `pg_isready -h localhost -p 5433`; integration tests `IONE_SKIP_LIVE=1 … --ignored --test-threads=1`; e2e server boot needs both `IONE_TOKEN_KEY` + `IONE_WEBHOOK_SECRET_KEY`.
- **Branch hygiene:** start from clean `main` (now includes RBAC + audit-export + auto-exec).
- Mark the backlog P6 provisioning item **Partial — pending walkthrough** when code-complete.
