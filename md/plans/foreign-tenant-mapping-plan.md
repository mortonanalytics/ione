# Foreign-Tenant Mapping — Implementation Plan

**Design doc:** [md/design/foreign-tenant-mapping.md](../design/foreign-tenant-mapping.md)
**Shape:** medium — 3 phases, ~14 files, vertical slices end-to-end
**Stack:** Rust/Axum + sqlx + Postgres 16 (pgvector image); vanilla-JS SPA in `static/`. No TypeScript build, no Node test runner. Gates are `cargo` + integration-test commands only.

## Dependencies

None new. `reqwest` (existing), `sqlx` (existing), `tokio` (existing), `serde`/`serde_json` (existing). No new crates.

## Pre-flight

- [ ] Confirm clean tree: `git status` shows nothing uncommitted before starting.
- [ ] Confirm local stack up: `docker compose ps postgres` shows healthy. (Already up per current session.)
- [ ] Confirm baseline green: `DATABASE_URL=postgres://ione:ione@localhost:5433/ione cargo test --no-fail-fast --tests` passes 20/20. `--ignored` suite is green except for the four pre-existing environmental failures documented in the session handoff (NWS network, Ollama, SMTP, none broker-related).

---

## Phase 1 — Auto-bind on subscribe

**Goal.** Subscribing a workspace to a peer also writes a `workspace_peer_bindings` row, populated by a best-effort `whoami` lookup. Subscribe response gains a `binding` field. Bundles design Slices 1, 2, 3.

**Files to create:**
- `migrations/0025_workspace_peer_bindings.sql` — new
- `src/models/workspace_peer_binding.rs` — new
- `src/repos/workspace_peer_binding_repo.rs` — new
- `src/services/workspace_peer_binding.rs` — new (holds `fetch_whoami`, `bind_on_subscribe`)
- `tests/phase14_bindings.rs` — new integration test file

**Files to modify:**
- `src/models/mod.rs` — register `workspace_peer_binding`
- `src/repos/mod.rs` — register repo
- `src/services/mod.rs` — register service module
- `src/routes/peers.rs` — modify `subscribe_peer` handler (~line 210) to call `bind_on_subscribe` after `auto_create_connector_for_peer` and return `{ connector, binding }`

### Code shapes

**Migration (`migrations/0025_workspace_peer_bindings.sql`):**
```sql
CREATE TYPE binding_status AS ENUM ('active', 'pending', 'conflict', 'inactive');

CREATE TABLE workspace_peer_bindings (
    id                    UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id                UUID        NOT NULL REFERENCES organizations(id) ON DELETE RESTRICT,
    workspace_id          UUID        NOT NULL REFERENCES workspaces(id)   ON DELETE CASCADE,
    peer_id               UUID        NOT NULL REFERENCES peers(id)        ON DELETE RESTRICT,
    foreign_tenant_id     TEXT        NOT NULL DEFAULT '',
    foreign_tenant_name   TEXT        NULL,
    foreign_workspace_id  TEXT        NULL,
    foreign_user_id       TEXT        NULL,
    foreign_user_email    TEXT        NULL,
    foreign_roles         TEXT[]      NOT NULL DEFAULT '{}',
    scope                 JSONB       NOT NULL DEFAULT '{}'::jsonb,
    status                binding_status NOT NULL DEFAULT 'pending',
    whoami_refreshed_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT wpb_unique_workspace_peer UNIQUE (workspace_id, peer_id),
    CONSTRAINT scope_is_object CHECK (jsonb_typeof(scope) = 'object')
);

CREATE OR REPLACE FUNCTION wpb_check_same_org() RETURNS trigger AS $$
DECLARE ws_org UUID; peer_org UUID;
BEGIN
    SELECT org_id INTO ws_org   FROM workspaces WHERE id = NEW.workspace_id;
    SELECT org_id INTO peer_org FROM peers      WHERE id = NEW.peer_id;
    IF ws_org IS DISTINCT FROM peer_org THEN
        RAISE EXCEPTION 'cross-org bindings are not allowed: workspace org % vs peer org %', ws_org, peer_org;
    END IF;
    NEW.org_id := ws_org;
    RETURN NEW;
END $$ LANGUAGE plpgsql;

CREATE TRIGGER wpb_check_same_org_trg
BEFORE INSERT OR UPDATE OF workspace_id, peer_id ON workspace_peer_bindings
FOR EACH ROW EXECUTE FUNCTION wpb_check_same_org();

CREATE OR REPLACE FUNCTION wpb_touch_updated_at() RETURNS trigger AS $$
BEGIN NEW.updated_at := now(); RETURN NEW; END $$ LANGUAGE plpgsql;

CREATE TRIGGER wpb_touch_updated_at_trg
BEFORE UPDATE ON workspace_peer_bindings
FOR EACH ROW EXECUTE FUNCTION wpb_touch_updated_at();

CREATE INDEX wpb_workspace_peer ON workspace_peer_bindings (workspace_id, peer_id);
CREATE INDEX wpb_peer_status    ON workspace_peer_bindings (peer_id, status);
CREATE INDEX wpb_org            ON workspace_peer_bindings (org_id);

ALTER TABLE workspace_peer_bindings ENABLE ROW LEVEL SECURITY;
CREATE POLICY wpb_org_isolation ON workspace_peer_bindings
    USING (org_id = current_setting('app.current_org_id', true)::uuid);
```

**Model (`src/models/workspace_peer_binding.rs`):**
```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::Type, Serialize, Deserialize, PartialEq, Eq)]
#[sqlx(type_name = "binding_status", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum BindingStatus { Active, Pending, Conflict, Inactive }

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspacePeerBinding {
    pub id: Uuid,
    pub org_id: Uuid,
    pub workspace_id: Uuid,
    pub peer_id: Uuid,
    pub foreign_tenant_id: String,
    pub foreign_tenant_name: Option<String>,
    pub foreign_workspace_id: Option<String>,
    pub foreign_user_id: Option<String>,
    pub foreign_user_email: Option<String>,
    pub foreign_roles: Vec<String>,
    pub scope: serde_json::Value,
    pub status: BindingStatus,
    pub whoami_refreshed_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
```

**Repo (`src/repos/workspace_peer_binding_repo.rs`):**
Methods needed in Phase 1 — defer the rest to Phase 2:
```rust
pub struct WorkspacePeerBindingRepo { pool: PgPool }

impl WorkspacePeerBindingRepo {
    pub fn new(pool: PgPool) -> Self { ... }

    /// Upsert on (workspace_id, peer_id). Preserves `scope` on conflict.
    /// `whoami` is Option — when None, row is written with status = 'pending' and empty whoami fields.
    /// When Some, row gets status = 'active' iff stored foreign_tenant_id matches (or row is new); otherwise 'conflict'.
    pub async fn upsert_from_subscribe(
        &self,
        workspace_id: Uuid,
        peer_id: Uuid,
        whoami: Option<&WhoamiResponse>,
    ) -> anyhow::Result<WorkspacePeerBinding>;

    pub async fn get_by_workspace_peer(
        &self,
        workspace_id: Uuid,
        peer_id: Uuid,
    ) -> anyhow::Result<Option<WorkspacePeerBinding>>;
}
```

**Service (`src/services/workspace_peer_binding.rs`):**
```rust
#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "snake_case")]
pub struct WhoamiResponse {
    pub peer_id: Option<String>,
    pub foreign_tenant_id: String,
    pub foreign_tenant_name: Option<String>,
    pub foreign_workspace_id: Option<String>,
    pub foreign_user_id: Option<String>,
    pub foreign_user_email: Option<String>,
    #[serde(default)]
    pub foreign_roles: Vec<String>,
}

/// POST to `{peer.mcp_url}/mcp` a JSON-RPC `resources/read { uri: "whoami://" }`
/// with the peer's brokered access token. 8s timeout. Returns Err on transport
/// failure, 4xx, JSON parse failure, or missing `result.contents[0].text`.
pub async fn fetch_whoami(peer: &Peer) -> anyhow::Result<WhoamiResponse>;

/// Best-effort: 3s timeout on whoami; on any failure or timeout the row is
/// upserted with whoami = None (status = 'pending'). Never propagates whoami
/// errors as Err — only DB errors. Called from subscribe_peer.
pub async fn bind_on_subscribe(
    pool: &PgPool,
    workspace_id: Uuid,
    peer: &Peer,
) -> anyhow::Result<WorkspacePeerBinding>;
```

**Route change (`src/routes/peers.rs`):**
```rust
// At end of subscribe_peer, after auto_create_connector_for_peer and before trigger_first_poll:
let binding = crate::services::workspace_peer_binding::bind_on_subscribe(
    &state.pool, workspace_id, &peer
).await.ok(); // log DB failure as warn; do not block subscribe

trigger_first_poll(&state, connector.id);

Ok(Json(json!({
    "connector": connector,
    "binding": binding,
})))
```

**Tests (`tests/phase14_bindings.rs`):**
Three test functions (all `#[ignore]`-gated, run with `--ignored --test-threads=1`):
1. `subscribe_creates_pending_binding_when_whoami_unavailable` — wiremock peer returns 404 for `whoami://`; assert binding row exists with `status='pending'` and `foreign_tenant_id=''`.
2. `subscribe_creates_active_binding_when_whoami_returns_tenant` — wiremock returns proper whoami JSON; assert row has `status='active'`, `foreign_tenant_id='t-acme'`.
3. `cross_org_binding_insert_raises_exception` — directly INSERT into `workspace_peer_bindings` with mismatched orgs; assert PG error contains `"cross-org bindings are not allowed"`.

Also: append `workspace_peer_bindings` to the TRUNCATE list in `tests/phase12_peer.rs:62`.

**Gate:**
```bash
cargo check && \
cargo clippy --all-targets -- -D warnings && \
DATABASE_URL=postgres://ione:ione@localhost:5433/ione \
  cargo test --tests phase14_bindings phase12_peer phase11_mcp_server \
  --no-fail-fast -- --ignored --test-threads=1
```

**Acceptance:** All three new tests pass; phase12 (12/12) and phase11 (12/12) remain green; `migrations/0025` applies cleanly to a fresh DB.

---

## Phase 2 — Manual CRUD + UI

**Goal.** Operator can list, read, create, remap, refresh, and delete bindings without going through subscribe. Peer detail page in `static/index.html` gets a "Workspace bindings" section. Bundles design Slice 4.

**Files to create:**
- `src/routes/bindings.rs` — new (7 handlers + org-scope guards)

**Files to modify:**
- `src/repos/workspace_peer_binding_repo.rs` — extend with the read/update/delete methods deferred in Phase 1
- `src/services/workspace_peer_binding.rs` — add `refresh_binding` that re-calls `fetch_whoami` and applies conflict detection
- `src/routes/mod.rs` — register `bindings` module + 5 new routes
- `src/error.rs` — add `WorkspaceBindingConflict { old, new }` (→ 409) and `WhoamiUnreachable { peer_id, message }` (→ 502)
- `static/index.html` — bindings table markup inside peer detail view (or new modal); edit dialog markup
- `static/app.js` — bindings list/refresh/remap/delete handlers; subscribe-success path renders `binding.status` toast
- `static/style.css` — bindings table + status badge styles
- `tests/phase14_bindings.rs` — extend with manual CRUD tests

### Code shapes

**New repo methods:**
```rust
pub async fn list_by_workspace(&self, workspace_id: Uuid) -> anyhow::Result<Vec<WorkspacePeerBinding>>;
pub async fn list_by_peer(&self, peer_id: Uuid) -> anyhow::Result<Vec<WorkspacePeerBinding>>;
pub async fn get_by_id(&self, id: Uuid) -> anyhow::Result<Option<WorkspacePeerBinding>>;
pub async fn create_manual(
    &self, workspace_id: Uuid, peer_id: Uuid,
    foreign_tenant_id: &str, foreign_workspace_id: Option<&str>, scope: Value,
) -> anyhow::Result<WorkspacePeerBinding>;
pub async fn patch(
    &self, id: Uuid,
    foreign_tenant_id: Option<&str>, // 422 on empty string — handled in route
    foreign_workspace_id: Option<&str>,
    scope: Option<Value>,
) -> anyhow::Result<WorkspacePeerBinding>;
pub async fn delete_by_id(&self, id: Uuid) -> anyhow::Result<()>;
pub async fn apply_whoami_refresh(
    &self, id: Uuid, whoami: &WhoamiResponse,
) -> anyhow::Result<(WorkspacePeerBinding, bool /* conflict */)>;
```

**New service:**
```rust
pub async fn refresh_binding(
    pool: &PgPool, binding_id: Uuid,
) -> Result<WorkspacePeerBinding, RefreshError>;

pub enum RefreshError {
    NotFound,
    PeerGone,
    Unreachable(String),       // → AppError::WhoamiUnreachable
    Conflict { old: String, new: String }, // → AppError::WorkspaceBindingConflict
    Db(anyhow::Error),
}
```

**Routes (`src/routes/bindings.rs`):**
```rust
pub async fn list_for_workspace(State, Extension(ctx), Path(workspace_id)) -> Result<Json<Value>, AppError>;
pub async fn list_for_peer    (State, Extension(ctx), Path(peer_id))       -> Result<Json<Value>, AppError>;
pub async fn get_binding      (State, Extension(ctx), Path((workspace_id, binding_id))) -> Result<Json<WorkspacePeerBinding>, AppError>;
pub async fn create_binding   (State, Extension(ctx), Path(workspace_id), Json(req)) -> Result<Json<WorkspacePeerBinding>, AppError>;
pub async fn patch_binding    (State, Extension(ctx), Path((workspace_id, binding_id)), Json(req)) -> Result<Json<WorkspacePeerBinding>, AppError>;
pub async fn refresh_binding  (State, Extension(ctx), Path((workspace_id, binding_id))) -> Result<Json<WorkspacePeerBinding>, AppError>;
pub async fn delete_binding   (State, Extension(ctx), Path((workspace_id, binding_id))) -> Result<Json<Value>, AppError>;
```
PATCH handler rejects `foreignTenantId == ""` with `AppError::BadRequest("foreignTenantId cannot be empty".into())` returning 422 per AC4a.

**Router wiring (`src/routes/mod.rs`, inside the `protected` builder before the final `.route_layer` chain):**
```rust
.route("/api/v1/workspaces/:id/bindings",
       get(bindings::list_for_workspace).post(bindings::create_binding))
.route("/api/v1/workspaces/:id/bindings/:bindingId",
       get(bindings::get_binding)
         .patch(bindings::patch_binding)
         .delete(bindings::delete_binding))
.route("/api/v1/workspaces/:id/bindings/:bindingId/refresh",
       post(bindings::refresh_binding))
.route("/api/v1/peers/:id/bindings", get(bindings::list_for_peer))
```

**UI additions (`static/index.html`):** inside the peer-federate dialog completion step OR a new "Peer detail" panel, add a `<section id="peer-bindings">` with a table, an "Edit binding" modal `<dialog id="binding-edit-dialog">` with inputs for `foreign_tenant_id`, `foreign_workspace_id`, `scope` (JSON textarea).

**UI handlers (`static/app.js`):** add `loadBindings(peerId)`, `editBinding(bindingId)`, `refreshBinding(bindingId)`, `deleteBinding(bindingId)`. Subscribe success path: read `binding.status` from response; toast `"Bound to tenant ${binding.foreignTenantName}"` if active, `"Binding pending — fill in manually"` if pending.

**New integration tests in `tests/phase14_bindings.rs`:**
4. `patch_with_empty_tenant_id_returns_422`
5. `patch_pending_to_active_when_tenant_set`
6. `refresh_returns_409_on_tenant_drift_and_preserves_stored_value`
7. `delete_binding_does_not_affect_peer`
8. `list_for_workspace_returns_only_that_workspaces_bindings`

**Gate:**
```bash
cargo check && \
cargo clippy --all-targets -- -D warnings && \
DATABASE_URL=postgres://ione:ione@localhost:5433/ione \
  cargo test --test phase14_bindings --no-fail-fast -- --ignored --test-threads=1
```

Plus manual UI smoke: `cargo run`, open `http://localhost:3000/`, federate a wiremock peer, confirm bindings table renders with one row.

**Acceptance:** Tests 4–8 pass; manual UI smoke shows a row in the bindings table after subscribe; PATCH with empty `foreignTenantId` returns 422; refresh on a drifted binding returns 409 and leaves the stored value intact.

---

## Phase 3 — Audit and approval enrichment

**Goal.** Every `approvals` and `audit_events` row that references a peer carries `foreign_tenant_id` resolved from the active binding. Bundles design Slice 5.

**Files to create:**
- `migrations/0026_audit_events_foreign_tenant.sql` — new

**Files to modify:**
- `src/models/approval.rs` — add `foreign_tenant_id: Option<String>` field
- `src/models/audit_event.rs` — add `foreign_tenant_id: Option<String>` field
- `src/repos/approval_repo.rs` — include new column in SELECT/INSERT
- `src/repos/audit_event_repo.rs` — include new column in SELECT/INSERT
- `src/services/peer.rs` — wherever an approval row is created for a peer-routed action, resolve binding via `WorkspacePeerBindingRepo::get_by_workspace_peer` and pass `foreign_tenant_id` through (None if no binding)
- `src/services/delivery.rs` — same treatment for any audit emission referencing a peer
- `tests/phase14_bindings.rs` — extend

### Code shapes

**Migration (`migrations/0026_audit_events_foreign_tenant.sql`):**
```sql
ALTER TABLE approvals    ADD COLUMN foreign_tenant_id TEXT NULL;
ALTER TABLE audit_events ADD COLUMN foreign_tenant_id TEXT NULL;
```

**Approval / audit emission pattern:**
```rust
let foreign_tenant_id = WorkspacePeerBindingRepo::new(pool.clone())
    .get_by_workspace_peer(workspace_id, peer_id).await
    .ok().flatten()
    .filter(|b| b.status == BindingStatus::Active)
    .map(|b| b.foreign_tenant_id);
// pass into ApprovalRepo::insert / AuditEventRepo::insert
```

Only `Active` bindings contribute a tenant; `pending`/`conflict`/`inactive` leave the column NULL — keeps audit data honest about completeness.

**New tests:**
9. `approval_for_peer_routed_action_carries_foreign_tenant_id` — set up active binding with `foreign_tenant_id='t-acme'`; route an approval-gated tool call via the peer; assert `approvals.foreign_tenant_id = 't-acme'`.
10. `approval_for_unbound_peer_has_null_foreign_tenant_id` — same flow without binding; assert NULL.
11. `approval_for_pending_binding_has_null_foreign_tenant_id` — binding exists with status=pending; assert NULL.

**Gate:**
```bash
cargo check && \
cargo clippy --all-targets -- -D warnings && \
DATABASE_URL=postgres://ione:ione@localhost:5433/ione \
  cargo test --test phase14_bindings --no-fail-fast -- --ignored --test-threads=1 && \
DATABASE_URL=postgres://ione:ione@localhost:5433/ione \
  cargo test --test phase09_delivery --no-fail-fast -- --ignored --test-threads=1
```

Phase 9 delivery has one pre-existing failure (`smtp_connector_send_is_invoked`, no SMTP infra) — that remains; verify no NEW failures appear.

**Acceptance:** Tests 9–11 pass; existing approval test suite continues to pass; `SELECT foreign_tenant_id FROM approvals` returns the expected tenant for the bound peer in test 9.

---

## Design AC ↔ phase gate mapping

| Design AC | Verifying gate / test |
|---|---|
| 1 (schema integrity) | Phase 1 migration apply + `cross_org_binding_insert_raises_exception` |
| 2 (auto-bind happy path) | Phase 1 `subscribe_creates_active_binding_when_whoami_returns_tenant` |
| 3 (auto-bind whoami missing) | Phase 1 `subscribe_creates_pending_binding_when_whoami_unavailable` |
| 4 (manual fill-in) | Phase 2 `patch_pending_to_active_when_tenant_set` |
| 4a (empty PATCH rejected) | Phase 2 `patch_with_empty_tenant_id_returns_422` |
| 5 (refresh detects drift) | Phase 2 `refresh_returns_409_on_tenant_drift_and_preserves_stored_value` |
| 6 (cross-org guard) | Phase 1 `cross_org_binding_insert_raises_exception` |
| 7 (cascade on workspace delete) | Add to Phase 1 tests as `binding_cascade_on_workspace_delete` |
| 8 (peer delete restricted) | Add to Phase 2 tests as `peer_delete_with_binding_returns_error` |
| 9 (audit enrichment) | Phase 3 `approval_for_peer_routed_action_carries_foreign_tenant_id` |
| 10 (backward compat) | Phase 1 gate includes phase11 + phase12 |

ACs 7 and 8 are not in the integration test list above — promote them to Phase 1 / Phase 2 test inventories before coding.

---

## Self-review checklist

- [x] Every design AC maps to a phase gate (table above).
- [x] Every referenced file exists or appears under "files to create" (verified: `src/services/peer.rs`, `src/services/delivery.rs`, `src/repos/approval_repo.rs`, `src/repos/audit_event_repo.rs`, `src/routes/peers.rs`, `src/routes/mod.rs`, `src/error.rs`, `src/models/{approval,audit_event}.rs`, `static/index.html`, `static/app.js`, `static/style.css` all exist).
- [x] Phases are vertical slices (each shipped phase = end-to-end working capability: Phase 1 = subscribe-creates-binding; Phase 2 = manual CRUD + UI; Phase 3 = audit enrichment).
- [x] Each gate is a concrete shell command, not "run tests."
- [n/a] Parallel task disjoint sets (no parallel orchestration — medium plan).
