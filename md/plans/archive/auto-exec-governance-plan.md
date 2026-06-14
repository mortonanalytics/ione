# Auto-Execution Governance — Implementation Plan

**Design doc:** `md/design/auto-exec-governance.md`
**Shape:** medium-large (db + api + ui, ~17 files; 3 vertical-slice phases, sequential & dependency-chained, no task manifest, no contract file)
**Stack:** Rust/Axum + Postgres (sqlx, embedded `sqlx::migrate!`) + static HTML/JS UI. Integration tests are `#[ignore]`-gated, run with `--ignored --test-threads=1`; CI sets `IONE_SKIP_LIVE=1`. Server boot needs `IONE_TOKEN_KEY` + `IONE_WEBHOOK_SECRET_KEY`.

## Dependencies

None. Reuses RBAC's `require_permission` ([auth.rs](src/auth.rs)), the `audit_events` verb convention, the connector trait, and the existing auto_exec engine.

## Resolved-at-plan-time facts (verified against working tree)

- `deliver_artifact` ([delivery.rs:395](src/services/delivery.rs#L395)) resolves a connector, calls `impl_.invoke("send", …)` at line 434; on `Err` it writes `delivery_failed` and returns `Err` → HTTP 500. The connector trait's default `invoke` bails "invoke not implemented" ([connectors/mod.rs:57](src/connectors/mod.rs#L57)). `slack`/`smtp`/`mcp_client` override `invoke`; `geojson_poll` does not (uses the bail).
- The engine resolves connectors with the **unscoped** `ConnectorRepo::get` ([auto_exec.rs:363](src/services/auto_exec.rs#L363), and in `run_auto_exec`) — AEG-C1. Scoped `get_for_workspace(id, workspace_id)` exists ([connector_repo.rs:62](src/repos/connector_repo.rs#L62)).
- Policies parse from `workspaces.metadata.auto_exec_policies` ([auto_exec.rs:150-197](src/services/auto_exec.rs#L150)); `fetch_survivor_context` reads `w.metadata`. The only writer is the test helper `set_auto_exec_policies` ([phase10_auto_exec.rs:188](src/tests)). No production rows — no backfill.
- Router floor `forced_target` is verified non-bypassable (design Devil's Advocate); the `severity_fallback("flagged"|"command")` arms ([router.rs:43-45](src/services/router.rs#L43)) are dead code.
- RBAC merged: `require_permission(ctx, pool, workspace_id, perm)`, escalation-guard pattern in `src/routes/roles.rs`, `AuditEventRepo` verb+payload convention, migration numbering next free = **0040**.
- UI panel pattern: `tab-*`/`panel-*` + `switchTab()` + probe-and-hide (the audit & roles panels are the reference).

## Phases

### Phase 1 — Terminal-on-approval delivery (design Slice 1; fixes the Epicenter 500, ship first)

**Goal:** approving an artifact whose connector can't `invoke` records the decision and returns 200, not 500.

**Files:**
- `src/connectors/mod.rs` — add a capability method to the `ConnectorImpl` trait: `fn supports_invoke(&self) -> bool { false }` (default false; the trait's `invoke` default still bails). Override `true` in the connectors that implement `invoke`.
- `src/connectors/slack.rs`, `src/connectors/smtp.rs`, `src/connectors/mcp_client.rs` — add `fn supports_invoke(&self) -> bool { true }`.
- `src/services/delivery.rs` — in `deliver_artifact` (line ~429), before invoking: if `!impl_.supports_invoke()`, write a `delivered` audit row with `{ "artifact_id": …, "terminal": true }`, mark the artifact delivered, and return `Ok(())` (no invoke). Invoke-capable connectors keep the existing send+audit path.
- `tests/phase09_delivery.rs` — add `terminal_approval_on_noninvokable_connector_returns_ok` (AC-1) and confirm `resource_order`+invoke-capable still invokes once (AC-2; an existing slack/smtp test likely covers this — extend if not).

**Code shapes:**
```rust
// connectors/mod.rs — trait addition
fn supports_invoke(&self) -> bool { false }
// delivery.rs deliver_artifact, before the invoke match:
if !impl_.supports_invoke() {
    audit_repo.insert(Some(workspace_id), actor.kind(), &actor.actor_ref(),
        "delivered", "connector", Some(connector_id),
        json!({ "artifact_id": artifact_id, "terminal": true })).await?;
    return Ok(());
}
```

**Gate:** `IONE_SKIP_LIVE=1 cargo test --test phase09_delivery -- --ignored --test-threads=1` + `cargo clippy --all-targets -- -D warnings`.
**Acceptance:** AC-1 (`notification_draft` on geojson_poll → 200 + `delivered` terminal audit, no `delivery_failed`) and AC-2 (`resource_order` on slack → invoke once) pass as named tests.

---

### Phase 2 — Policy table + RBAC-scoped CRUD + panel (design Slice 2; the governance core)

**Goal:** policies are a first-class, audited, permission-scoped resource with a CRUD API and a workspace panel; the engine reads the table.

**Files:**
- `migrations/0040_auto_exec_policies.sql` — **create.** Table per design §Slice-2 DB:
```sql
CREATE TABLE auto_exec_policies (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  org_id UUID NOT NULL REFERENCES organizations(id) ON DELETE RESTRICT,
  workspace_id UUID NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  trigger_signal_title_prefix TEXT,
  trigger_severity_at_most TEXT CHECK (trigger_severity_at_most IN ('routine','flagged')),
  connector_id UUID NOT NULL REFERENCES connectors(id) ON DELETE RESTRICT,
  op TEXT NOT NULL,
  args_template JSONB NOT NULL DEFAULT '{}'::jsonb,
  rate_limit_per_min INT NOT NULL CHECK (rate_limit_per_min BETWEEN 1 AND 1000),
  severity_cap TEXT NOT NULL DEFAULT 'routine' CHECK (severity_cap IN ('routine','flagged')),
  authorized_by_permission TEXT NOT NULL,
  enabled BOOLEAN NOT NULL DEFAULT true,
  created_by UUID NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  UNIQUE (workspace_id, name)
);
-- BEFORE INSERT trigger populates org_id from workspaces; BEFORE UPDATE touches updated_at.
CREATE INDEX aep_workspace_enabled ON auto_exec_policies (workspace_id) WHERE enabled = true;
CREATE INDEX aep_connector ON auto_exec_policies (connector_id);
ALTER TABLE auto_exec_policies ENABLE ROW LEVEL SECURITY; -- org-isolation policy per existing pattern
```
- `src/models/auto_exec_policy.rs` — **create.** `AutoExecPolicy` struct (sqlx FromRow) matching the table; `org_id` not serialized to API.
- `src/repos/auto_exec_policy_repo.rs` — **create.** `list_for_workspace`, `get`, `create`, `update` (full replace), `delete`. `MAX_RATE_LIMIT_PER_MIN = 1000` constant lives in the engine (`auto_exec.rs`) and is reused for validation.
- `src/repos/mod.rs` — export `AutoExecPolicyRepo`.
- `src/routes/auto_exec_policies.rs` — **create.** `list/create/update/delete` handlers. All `require_permission(ctx, pool, workspace_id, "approvals:decide")`. **Guard A:** actor must hold `authorized_by_permission` in workspace (else 409 `permission_escalation`; `admin` exempt) — reuse the escalation helper from `roles.rs`. **Guard B:** `connector_id` must resolve via `get_for_workspace(connector_id, workspace_id)` (else 422; `admin` not exempt). Validate `severity_cap`/`severity_at_most` ∈ {routine,flagged} (422 on `command`/unknown), `rate_limit_per_min` ∈ [1,1000] (422). Each write emits `audit_events` `auto_exec_policy.created/updated/deleted` (before/after payload) via `AuditEventRepo`.
- `src/routes/mod.rs` — mount the four routes under `/api/v1/workspaces/:id/auto-exec-policies`.
- `src/services/auto_exec.rs` — cut `fetch_survivor_context`/`parse_policies` over to read the table (`WHERE workspace_id = $1 AND enabled = true`), mapping rows directly to `AutoExecPolicy`; delete the metadata-JSONB parse path. Rate-bucket key becomes `(workspace_id, policy_id)`.
- `static/index.html` — `tab-policies` + `panel-policies`: policy list (trigger, connector, cap, rate limit, enabled), create/edit form, enable/disable + delete controls. `hidden` by default.
- `static/app.js` — `loadPolicies()` probe-and-hide on 403 (`policiesAdminDenied` flag, mirroring `auditAdminDenied`); wire POST/PUT/DELETE.
- `static/style.css` — policy-panel styles.
- `md/requirements/active/auto-exec-governance.md` — **create.** Table shape, four CRUD contracts + terminal-on-approval behavior, management permission + Guards A/B, fail-closed validation; reference `rbac.md`.
- `tests/phase10_auto_exec.rs` — replace `set_auto_exec_policies` (JSONB writer) with direct `INSERT INTO auto_exec_policies`; existing engine tests keep their assertions.
- `tests/auto_exec_governance_integration.rs` — **create** (`_integration` suffix for the coverage gate; `spawn_app` per `tests/audit_export_integration.rs`). AC-3a/3b/3c, 4, 5, 7, 8, 10.
- `tests/e2e/policies-panel.spec.ts` — **create.** AC-11 probe-and-hide (mirror `tests/e2e/roles-panel.spec.ts`).

**Gate:** `IONE_SKIP_LIVE=1 cargo test --test auto_exec_governance_integration --test phase10_auto_exec -- --ignored --test-threads=1` + clippy + `npx playwright test tests/e2e/policies-panel.spec.ts`.
**Acceptance:** AC-3a/3b/3c/4/5/7/8/10 green in the integration suite; AC-11 green in Playwright.

---

### Phase 3 — Engine hardening + bypass-guard lock (design Slice 3; security)

**Goal:** close AEG-C1, enforce the rate ceiling, and lock the router floor against regression.

**Files:**
- `src/services/auto_exec.rs` — replace both `connector_repo.get(policy.connector_id)` call sites (evaluate + run_auto_exec) with `get_for_workspace(policy.connector_id, ctx.workspace_id)`; a `None` is `ConnectorMissing` (closes AEG-C1). Clamp/validate `rate_limit_per_min` against `MAX_RATE_LIMIT_PER_MIN` (defense-in-depth; the table CHECK is primary).
- `src/services/router.rs` — remove or `unreachable!`-assert the dead `severity_fallback("flagged"|"command")` arms (lines 43-45) so a future reorder can't open the floor; the `_ => Feed` routine path stays.
- `tests/phase10_auto_exec.rs` — add `foreign_connector_not_invoked` (AC-6: a policy with a foreign `connector_id` inserted via fixture → `ConnectorMissing`, no invoke).
- `tests/phase07_routing.rs` (or wherever router tests live — verify) — add `approval_required_flagged_routes_draft_on_classifier_outage` (AC-9): with an unreachable classifier, an `approval_required`+`flagged` survivor routes to `Draft`.

**Gate:** `IONE_SKIP_LIVE=1 cargo test --test phase10_auto_exec --test phase07_routing -- --ignored --test-threads=1` + clippy.
**Acceptance:** AC-6 and AC-9 pass as named tests; the foreign-connector path returns `ConnectorMissing`, never invokes.

---

## Acceptance-criteria → phase map (self-review, step 7)

| Design AC | Phase | Gate test |
|---|---|---|
| 1 (terminal-on-approval 200) | 1 | `phase09_delivery::terminal_approval_on_noninvokable_connector_returns_ok` |
| 2 (invoke path intact) | 1 | `phase09_delivery::*resource_order*invoke` |
| 3a/3b/3c (CRUD round-trip) | 2 | `auto_exec_governance_integration::policy_{create,update,delete}` |
| 4 (management gate 403/404) | 2 | `auto_exec_governance_integration::policy_gate` |
| 5 (Guard A escalation 409 / admin 200) | 2 | `auto_exec_governance_integration::authorship_escalation` |
| 7 (write-time validation 422) | 2 | `auto_exec_governance_integration::validation_fail_closed` |
| 8 (severity_cap default routine) | 2 | `auto_exec_governance_integration::severity_cap_default` |
| 10 (policy audit trail) | 2 | `auto_exec_governance_integration::policy_change_audited` |
| 11 (UI probe-and-hide) | 2 | `policies-panel.spec.ts` |
| 6 (Guard B / AEG-C1 no foreign invoke) | 3 | `phase10_auto_exec::foreign_connector_not_invoked` |
| 9 (router floor under outage) | 3 | `phase07_routing::approval_required_flagged_routes_draft_on_classifier_outage` |

Every design AC maps to a phase gate. Cited files exist except those marked **create**. Phases are vertical slices (each ships DB+API/engine+UI+tests for one capability; Phase 1 is delivery-path-only by design). Gates are concrete commands. No parallel agents → no manifest/contract file.

## Notes for /code-the-plan

- **Environment preflight before Phase 1:** `pg_isready -h localhost -p 5433`; run integration tests with `IONE_SKIP_LIVE=1 … -- --ignored --test-threads=1`; e2e server boot needs `IONE_TOKEN_KEY` + `IONE_WEBHOOK_SECRET_KEY`.
- **Compile at every boundary:** Phase 2's engine read-path cutover (metadata→table) and the test-helper rewrite must land together — don't leave `set_auto_exec_policies` writing JSONB the engine no longer reads.
- **Order matters for risk:** Phase 1 (demo-unblock, low risk) → Phase 3 (close AEG-C1) → Phase 2 (management surface) is the design's minimal-first order; this plan keeps 1 first but sequences 2 before 3 because Phase 3's AC-6 needs the table from Phase 2. If shipping incrementally, Phase 1 can PR alone.
- **Branch hygiene:** start from clean `main` (now includes RBAC + audit-export).
- Mark the backlog P4 auto-exec items **Partial — pending walkthrough** when code-complete.
