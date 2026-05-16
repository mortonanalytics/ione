# Foreign-Tenant Mapping — Implementation Plan

**Design doc:** [md/design/foreign-tenant-mapping.md](../design/foreign-tenant-mapping.md)
**Shape:** medium — 4 phases, ~16 files, vertical slices end-to-end
**Stack:** Rust/Axum + sqlx + Postgres 16 (pgvector image); vanilla-JS SPA in `static/`. No TypeScript build, no Node test runner. Gates are `cargo` + integration-test commands only.

**Revision:** 2026-05-15 — folded six findings from Codex review pass. Adds binding-aware routing as Phase 3 (without it, audit enrichment in P4 would record intent but not enforcement, producing dishonest audit rows). Adds `UnprocessableEntity` error variant. Adds `whoami://` resource handler to IONe's own MCP server. Specifies cross-org guards rigorously and adds negative tests.

## Dependencies

None new. `reqwest` (existing), `sqlx` (existing), `tokio` (existing), `serde`/`serde_json` (existing). No new crates.

## Pre-flight

- [ ] Confirm clean tree: `git status` shows nothing uncommitted before starting.
- [ ] Confirm local stack up: `docker compose ps postgres` shows healthy.
- [ ] Confirm baseline green: `DATABASE_URL=postgres://ione:ione@localhost:5433/ione cargo test --no-fail-fast --tests` passes 20/20. `--ignored` suite is green except for the four pre-existing environmental failures (NWS network, Ollama, SMTP — none broker-related).

---

## Phase 1 — Auto-bind on subscribe + IONe `whoami` resource

**Goal.** Subscribing a workspace to a peer also writes a `workspace_peer_bindings` row, populated by a best-effort `whoami` lookup. IONe's own MCP server exposes `whoami://` so IONe-to-IONe federation produces active bindings, not pending ones. Bundles design Slices 1, 2, 3.

**Files to create:**
- `migrations/0025_workspace_peer_bindings.sql`
- `src/models/workspace_peer_binding.rs`
- `src/repos/workspace_peer_binding_repo.rs` — only `upsert_from_subscribe` + `get_by_workspace_peer` in this phase
- `src/services/workspace_peer_binding.rs` — `fetch_whoami`, `bind_on_subscribe`
- `tests/phase14_bindings.rs`

**Files to modify:**
- `src/models/mod.rs` — register `workspace_peer_binding`
- `src/repos/mod.rs` — register repo
- `src/services/mod.rs` — register service module
- `src/routes/peers.rs` — `subscribe_peer` calls `bind_on_subscribe`, returns `{ connector, binding }`
- `src/mcp_server.rs` — add `resources/list` and `resources/read` dispatch; `whoami://` resource returns the caller's AuthContext rendered per playbook
- `tests/phase12_peer.rs` — append `workspace_peer_bindings` to the TRUNCATE list; update any assertion on subscribe response shape (verify `subscribe_creates_mcp_connector_in_workspace` reads `connector.id` after the change)
- `static/app.js` — callers of `POST /workspaces/:id/peers/:peerId/subscribe` read `data.connector` instead of treating `data` as the connector directly. Verify all call sites.

### Code shapes

**Migration:**
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

**Model:** `WorkspacePeerBinding` struct with `sqlx::FromRow`, `Serialize(rename_all = "camelCase")`. Enum `BindingStatus` as `sqlx::Type` mapping to PG `binding_status`.

**Repo (Phase 1 methods only):**
```rust
pub struct WorkspacePeerBindingRepo { pool: PgPool }

impl WorkspacePeerBindingRepo {
    pub fn new(pool: PgPool) -> Self;

    /// Upsert on (workspace_id, peer_id). Preserves `scope` on conflict.
    /// `whoami = None` → status='pending', foreign fields empty/null.
    /// `whoami = Some` → status='active' if new row OR stored foreign_tenant_id
    /// matches; status='conflict' if stored differs (stored value preserved).
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

**Service:**
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

/// POST to {peer.mcp_url} a JSON-RPC `resources/read { uri: "whoami://" }`
/// with the peer's brokered access token. 8s transport timeout. Returns Err
/// on transport failure, non-2xx, JSON parse failure, or missing
/// `result.contents[0].text`.
pub async fn fetch_whoami(peer: &Peer) -> anyhow::Result<WhoamiResponse>;

/// Best-effort. 3s outer timeout wraps fetch_whoami. On any whoami failure
/// (timeout, transport, missing resource, parse) returns Ok(binding) with
/// status='pending'. Only DB errors propagate as Err.
pub async fn bind_on_subscribe(
    pool: &PgPool,
    workspace_id: Uuid,
    peer: &Peer,
) -> anyhow::Result<WorkspacePeerBinding>;
```

**IONe MCP server `whoami://` resource (`src/mcp_server.rs`):**

Add two dispatch arms to `dispatch_method`:
```rust
"resources/list" => handle_resources_list(req.id),
"resources/read" => {
    let auth = match resolve_auth(state, headers).await {
        Some(a) => a,
        None => return JsonRpcResponse::err(req.id, -32001,
            "unauthorized: valid session cookie or bearer JWT required", None),
    };
    handle_resources_read(req.id, req.params.unwrap_or(Value::Null), &auth, state).await
}
```

`handle_resources_list` returns:
```json
{ "resources": [{ "uri": "whoami://", "name": "Caller identity",
                  "mimeType": "application/vnd.ione.whoami+json" }] }
```

`handle_resources_read` parses `params.uri`. For `whoami://` returns:
```json
{ "contents": [{ "uri": "whoami://", "mimeType": "application/vnd.ione.whoami+json",
                 "text": "<JSON-encoded WhoamiResponse>" }] }
```
The WhoamiResponse payload uses: `peer_id` = IONe instance hostname (env `IONE_BIND` or "ione"); `foreign_tenant_id` = `auth.org_id.to_string()`; `foreign_workspace_id` left null in this phase (IONe doesn't yet have a notion of "the workspace the caller is acting in" at the MCP-server level); `foreign_user_id` = `auth.user_id.to_string()`; `foreign_user_email` looked up via `UserRepo::get`; `foreign_roles` = role names from `MembershipRepo` for active user. Unknown URIs return JSON-RPC error -32602 "resource not found".

**Route change (`src/routes/peers.rs`):**
```rust
// After auto_create_connector_for_peer, before trigger_first_poll:
let binding = match crate::services::workspace_peer_binding::bind_on_subscribe(
    &state.pool, workspace_id, &peer
).await {
    Ok(b) => Some(b),
    Err(e) => {
        tracing::warn!(error = %e, workspace_id = %workspace_id, peer_id = %peer.id,
                       "bind_on_subscribe failed; subscribe continues without binding");
        None
    }
};

trigger_first_poll(&state, connector.id);

Ok(Json(json!({
    "connector": connector,
    "binding": binding,
})))
```

**Tests in `tests/phase14_bindings.rs`:**
1. `subscribe_creates_pending_binding_when_whoami_unavailable` — wiremock peer returns -32601 for `resources/read`; assert row exists, `status='pending'`, `foreign_tenant_id=''`.
2. `subscribe_creates_active_binding_when_whoami_returns_tenant` — wiremock returns proper whoami JSON; assert `status='active'`, `foreign_tenant_id='t-acme'`.
3. `cross_org_binding_insert_raises_exception` — direct INSERT with mismatched orgs; assert PG error contains `"cross-org bindings are not allowed"`.
4. `binding_cascade_on_workspace_delete` (design AC7) — create binding; DELETE workspace; assert binding row gone.
5. `ione_whoami_resource_returns_caller_identity` — spawn app, call `POST /mcp` with `resources/read whoami://` and a session cookie; assert response.contents[0].text parses as WhoamiResponse with `foreign_tenant_id` = caller's org_id.
6. `ione_to_ione_subscribe_produces_active_binding` — spawn two apps; instance A federates to instance B; assert A's binding row for B has `status='active'` (proves dogfooding: P1 doesn't reduce to wiremock-only validation).

**Gate:**
```bash
cargo check && \
cargo clippy --all-targets -- -D warnings && \
DATABASE_URL=postgres://ione:ione@localhost:5433/ione \
  cargo test --tests phase14_bindings phase12_peer phase11_mcp_server \
  --no-fail-fast -- --ignored --test-threads=1
```

**Acceptance:** Tests 1–6 pass; phase12 (12/12) and phase11 (12/12) remain green; `migrations/0025` applies cleanly to fresh DB; manual `cargo run` + browser-side federate flow still succeeds (no JS console errors from the `{connector, binding}` shape change).

---

## Phase 2 — Manual CRUD + UI + cross-org enforcement

**Goal.** Operator can list, read, create, remap, refresh, and delete bindings without going through subscribe. Peer detail page gets a "Workspace bindings" section. Every read/update/delete enforces `ctx.org_id` at the route layer (RLS is inert in v0.1 per the broker known-limitations note; cannot rely on it). Bundles design Slice 4.

**Files to create:**
- `src/routes/bindings.rs`

**Files to modify:**
- `src/repos/workspace_peer_binding_repo.rs` — extend with read/update/delete methods, all org-scoped
- `src/services/workspace_peer_binding.rs` — add `refresh_binding` with conflict detection
- `src/routes/mod.rs` — register `bindings` module + new routes
- `src/error.rs` — add `UnprocessableEntity(String)` → 422, `WorkspaceBindingConflict { old, new }` → 409, `WhoamiUnreachable { peer_id, message }` → 502
- `static/index.html` — bindings table markup in peer detail section; edit modal
- `static/app.js` — bindings list/refresh/remap/delete handlers; subscribe-success toast based on `binding.status`
- `static/style.css` — bindings table + status badge styles
- `tests/phase14_bindings.rs` — extend

### Code shapes

**Error additions (`src/error.rs`):**
```rust
#[error("unprocessable entity: {0}")]
UnprocessableEntity(String),

#[error("workspace binding conflict: foreign_tenant_id changed from {old} to {new}")]
WorkspaceBindingConflict { old: String, new: String },

#[error("whoami unreachable for peer {peer_id}: {message}")]
WhoamiUnreachable { peer_id: Uuid, message: String },
```
Map to 422 / 409 / 502 in `IntoResponse for AppError`.

**Repo additions — every read/update/delete is org-scoped:**
```rust
// All queries JOIN workspaces (and peers where applicable) and require
// matching org_id. Caller passes ctx.org_id; repo enforces.

pub async fn list_by_workspace(
    &self, workspace_id: Uuid, org_id: Uuid,
) -> anyhow::Result<Vec<WorkspacePeerBinding>>;
// SQL: SELECT b.* FROM workspace_peer_bindings b
//      JOIN workspaces w ON w.id = b.workspace_id
//      WHERE b.workspace_id = $1 AND w.org_id = $2

pub async fn list_by_peer(
    &self, peer_id: Uuid, org_id: Uuid,
) -> anyhow::Result<Vec<WorkspacePeerBinding>>;
// SQL: similar JOIN on peers + workspaces, both must match org_id

pub async fn get_by_id_org_scoped(
    &self, id: Uuid, org_id: Uuid,
) -> anyhow::Result<Option<WorkspacePeerBinding>>;

pub async fn create_manual(
    &self, workspace_id: Uuid, peer_id: Uuid, org_id: Uuid,
    foreign_tenant_id: &str, foreign_workspace_id: Option<&str>, scope: Value,
) -> anyhow::Result<WorkspacePeerBinding>;
// Pre-validates workspace.org_id = peer.org_id = org_id BEFORE insert,
// so 404 (not the cross-org trigger) is returned to cross-org callers.

pub async fn patch(
    &self, id: Uuid, org_id: Uuid,
    foreign_tenant_id: Option<&str>,           // route rejects "" with 422 first
    foreign_workspace_id: Option<Option<&str>>,// outer Option = "not provided", inner None = clear
    scope: Option<Value>,
) -> anyhow::Result<WorkspacePeerBinding>;
// Re-fetches binding, checks org_id, then UPDATEs. Flips status to 'active'
// when transitioning from 'pending' iff foreign_tenant_id becomes non-empty.

pub async fn delete_by_id_org_scoped(
    &self, id: Uuid, org_id: Uuid,
) -> anyhow::Result<bool>;  // false if not found OR wrong org

pub async fn apply_whoami_refresh(
    &self, id: Uuid, org_id: Uuid, whoami: &WhoamiResponse,
) -> anyhow::Result<(WorkspacePeerBinding, bool /* drift detected */)>;

/// Called from peer status-change paths to cascade. Used in Phase 3 too.
pub async fn set_inactive_for_peer(&self, peer_id: Uuid) -> anyhow::Result<u64>;
```

Validation rules enforced in the route handler before repo call:
- `foreign_tenant_id` after `.trim()` must be non-empty → else 422 `"foreignTenantId cannot be empty or whitespace"`.
- `scope` if present must be `serde_json::Value::Object` → else 422 `"scope must be a JSON object"`.
- `foreignWorkspaceId == ""` is interpreted as "clear" (set to NULL); explicit `null` in JSON is also "clear"; field absent is "no change".

**Service: `refresh_binding`:**
```rust
pub async fn refresh_binding(
    pool: &PgPool, binding_id: Uuid, org_id: Uuid,
) -> Result<WorkspacePeerBinding, RefreshError>;

pub enum RefreshError {
    NotFound,                              // → AppError::NotFound
    PeerGone,                              // → AppError::NotFound
    Unreachable(String),                   // → AppError::WhoamiUnreachable
    Conflict { old: String, new: String }, // → AppError::WorkspaceBindingConflict
    Db(anyhow::Error),                     // → AppError::Internal
}
```

**Routes (`src/routes/bindings.rs`):** seven handlers; all read `Extension<AuthContext>` and pass `ctx.org_id` into every repo call. Cross-org reads return 404, not 403, so existence is not leaked. PATCH validates payload (above) before any DB call.

**Router wiring (`src/routes/mod.rs`):**
```rust
.route("/api/v1/workspaces/:id/bindings",
       get(bindings::list_for_workspace).post(bindings::create_binding))
.route("/api/v1/workspaces/:id/bindings/:bindingId",
       get(bindings::get_binding).patch(bindings::patch_binding).delete(bindings::delete_binding))
.route("/api/v1/workspaces/:id/bindings/:bindingId/refresh",
       post(bindings::refresh_binding))
.route("/api/v1/peers/:id/bindings", get(bindings::list_for_peer))
```

**UI:** new `<section id="peer-bindings">` inside the peer-federate dialog "done" step or as part of a peer detail view. Edit modal `<dialog id="binding-edit-dialog">` with text inputs for `foreignTenantId`, `foreignWorkspaceId`, and a `<textarea>` for `scope` JSON. App.js handlers: `loadBindings(peerId)`, `editBinding(bindingId)`, `refreshBinding(bindingId)`, `deleteBinding(bindingId)`, plus subscribe-success toast keyed on `binding.status`.

**New tests in `tests/phase14_bindings.rs`:**
7. `patch_with_empty_tenant_id_returns_422` (design AC4a)
8. `patch_with_whitespace_only_tenant_id_returns_422`
9. `patch_pending_to_active_when_tenant_set` (design AC4)
10. `patch_with_scope_not_object_returns_422`
11. `patch_can_clear_foreign_workspace_id_via_null`
12. `refresh_returns_409_on_tenant_drift_and_preserves_stored_value` (design AC5)
13. `refresh_returns_502_when_peer_unreachable`
14. `delete_binding_does_not_revoke_peer`
15. `list_for_workspace_returns_only_that_workspaces_bindings`
16. `cross_org_get_returns_404` — bootstrap two orgs; create binding in org A; ctx with org B's user calls GET; assert 404.
17. `cross_org_patch_returns_404` — same setup; PATCH from wrong org; assert 404 and stored row unchanged.
18. `cross_org_delete_returns_404` — same setup; DELETE from wrong org; assert 404 and row still present.
19. `cross_org_refresh_returns_404`
20. `cross_org_peer_bindings_list_returns_empty` — `GET /api/v1/peers/:id/bindings` for a peer in another org returns 200 with `items: []` (peer existence is not the focus; the list filter is).
21. `duplicate_manual_create_returns_409_or_409-via-unique-constraint` — POST create twice for same (workspace, peer); second returns 409.
22. `manual_create_with_cross_org_peer_returns_404` — operator in org A tries to bind workspace-in-A to peer-in-B; 404, no row written.

**Gate:**
```bash
cargo check && \
cargo clippy --all-targets -- -D warnings && \
DATABASE_URL=postgres://ione:ione@localhost:5433/ione \
  cargo test --test phase14_bindings --no-fail-fast -- --ignored --test-threads=1
```

Plus manual UI smoke: `cargo run`, federate a wiremock peer, confirm bindings table renders one row.

**Acceptance:** Tests 7–22 pass; manual UI smoke shows binding row with correct status badge; PATCH/DELETE from wrong org returns 404 (not 403, not 200, no row mutation).

---

## Phase 3 — Binding-aware routing

**Goal.** When an active binding exists for `(workspace_id, peer_id)`, IONe uses `binding.foreign_workspace_id` to scope peer calls. Without this, the audit enrichment in Phase 4 records intent without enforcement — failure mode the design names as the primary risk. Falls through to current heuristics when no active binding exists, preserving phase12 backward compat.

**Files to modify:**
- `src/connectors/mcp_client.rs` — `poll` (line ~104) calls a new helper to resolve the workspace id; if binding exists, use `binding.foreign_workspace_id`; else iterate all (current behavior). Add `workspace_id` and `peer_id` to `McpClientConnector` state.
- `src/services/peer.rs` — `auto_create_connector_for_peer` (line ~37) writes `workspace_id` and `peer_id` into the connector's `config` JSONB so the connector can read them at poll time without a DB lookup.
- `src/services/delivery.rs` — `peer_route_briefing` path (line ~606) consults binding before falling back to `resolve_peer_workspace_id`.
- `tests/phase14_bindings.rs` — extend.

### Code shapes

**Connector config addition (`src/services/peer.rs`):**
```rust
let config = json!({
    "mcp_url": peer.mcp_url,
    "bearer_token": bearer_token,
    "peer_id": peer.id.to_string(),
    "workspace_id": workspace_id.to_string(),  // ← new
});
```

**`McpClientConnector` workspace resolution (`src/connectors/mcp_client.rs`):**
```rust
/// Returns the foreign workspace ids to call. If an active binding exists
/// for (workspace_id, peer_id) AND binding.foreign_workspace_id is non-null,
/// returns a single-element Vec with that id. Otherwise returns the full
/// list from resolve_all_peer_workspace_ids() (current behavior).
async fn resolve_workspace_ids_with_binding(
    &self, pool: &PgPool,
) -> Vec<String> {
    if let (Some(ws), Some(peer)) = (self.workspace_id, self.peer_id) {
        if let Ok(Some(binding)) = WorkspacePeerBindingRepo::new(pool.clone())
            .get_by_workspace_peer(ws, peer).await {
            if binding.status == BindingStatus::Active {
                if let Some(fws) = binding.foreign_workspace_id {
                    return vec![fws];
                }
            }
        }
    }
    self.resolve_all_peer_workspace_ids().await
}
```

`McpClientConnector` gains optional `workspace_id: Option<Uuid>`, `peer_id: Option<Uuid>`, `pool: Option<PgPool>` fields populated from config in `build_from_row`. `pool` injection: `ConnectorImpl::build_from_row` already receives `&AppState`-equivalent context — verify and wire. If not, expose pool via a `ConnectorContext` parameter; do not insert a new singleton.

**Delivery routing (`src/services/delivery.rs`):**
```rust
// Replace the line: let peer_workspace_id = resolve_peer_workspace_id(&*impl_).await;
// with:
let peer_workspace_id = match WorkspacePeerBindingRepo::new(pool.clone())
    .get_by_workspace_peer(local_workspace_id, peer_id).await {
    Ok(Some(b)) if b.status == BindingStatus::Active && b.foreign_workspace_id.is_some()
        => b.foreign_workspace_id.unwrap(),
    _   => resolve_peer_workspace_id(&*impl_).await,
};
```

The `local_workspace_id` and `peer_id` must be in scope at this call site — confirm before coding; if not, thread them through from the caller.

**New tests (`tests/phase14_bindings.rs`):**
23. `mcp_client_poll_uses_binding_foreign_workspace_id_when_active` — set up binding with `foreign_workspace_id='fws-acme'`; mock peer; assert the `tools/call list_survivors` request body's `arguments.workspace_id == 'fws-acme'` (single call, not a loop over all peer workspaces).
24. `mcp_client_poll_falls_back_when_binding_inactive` — binding with status='pending'; assert call iterates all remote workspaces (current behavior preserved).
25. `mcp_client_poll_falls_back_when_no_binding` — no binding row; assert current behavior.
26. `delivery_peer_routing_uses_binding_when_active` — binding active; trigger peer-routed briefing; assert peer's `tools/call propose_artifact` received `arguments.workspace_id` = binding's foreign workspace.
27. `phase12_two_node_federation_remains_green_with_routing_change` — promotion of existing test as explicit backward-compat assertion; if phase12 implicitly relied on the "iterate all" behavior, this test catches the regression.

**Gate:**
```bash
cargo check && \
cargo clippy --all-targets -- -D warnings && \
DATABASE_URL=postgres://ione:ione@localhost:5433/ione \
  cargo test --tests phase14_bindings phase12_peer phase09_delivery \
  --no-fail-fast -- --ignored --test-threads=1
```

(Phase 9 delivery has one pre-existing failure: `smtp_connector_send_is_invoked`, no SMTP infra — remains; verify no NEW failures.)

**Acceptance:** Tests 23–27 pass; phase12 (12/12) remains green; manual trace: federate IONe-to-IONe, trigger peer-routed action, confirm the mock peer received exactly one `tools/call` with the bound workspace id.

---

## Phase 4 — Audit and approval enrichment

**Goal.** Now that routing is binding-aware (Phase 3), recording `foreign_tenant_id` on approval and audit rows is honest. Every approval/audit row for a peer-routed action carries the resolved tenant from the active binding. Bundles design Slice 5.

**Files to create:**
- `migrations/0026_audit_events_foreign_tenant.sql`

**Files to modify:**
- `src/models/approval.rs` — add `foreign_tenant_id: Option<String>`
- `src/models/audit_event.rs` — add `foreign_tenant_id: Option<String>`
- `src/repos/approval_repo.rs` — include new column in SELECT/INSERT
- `src/repos/audit_event_repo.rs` — include new column in SELECT/INSERT
- `src/services/peer.rs` — peer status-change paths call `WorkspacePeerBindingRepo::set_inactive_for_peer` (Phase 2 added the method) when peer status flips to revoked
- `src/services/delivery.rs` — approval-emission paths for peer-routed actions look up active binding and pass `foreign_tenant_id` through to `ApprovalRepo::insert`
- `src/services/approval.rs` (or wherever audit emission for approvals lives) — same pattern
- `tests/phase14_bindings.rs` — extend

### Code shapes

**Migration:**
```sql
ALTER TABLE approvals    ADD COLUMN foreign_tenant_id TEXT NULL;
ALTER TABLE audit_events ADD COLUMN foreign_tenant_id TEXT NULL;
```

**Emission pattern:**
```rust
let foreign_tenant_id = WorkspacePeerBindingRepo::new(pool.clone())
    .get_by_workspace_peer(workspace_id, peer_id).await
    .ok().flatten()
    .filter(|b| b.status == BindingStatus::Active)
    .map(|b| b.foreign_tenant_id);
// Pass into ApprovalRepo::insert / AuditEventRepo::insert.
```

Only `Active` bindings contribute a tenant; `pending` / `conflict` / `inactive` leave the column NULL. Keeps audit data honest about completeness.

**Peer-revoke cascade:** in whichever handler sets peer status to `revoked` (currently `delete_peer` in `routes/peers.rs`), append a call to `WorkspacePeerBindingRepo::set_inactive_for_peer(peer_id)` so downstream lookups skip those bindings.

**New tests:**
28. `approval_for_peer_routed_action_carries_foreign_tenant_id` (design AC9) — active binding `t-acme`; route approval-gated tool call via peer; assert `approvals.foreign_tenant_id = 't-acme'`.
29. `approval_for_unbound_peer_has_null_foreign_tenant_id` — no binding; assert NULL.
30. `approval_for_pending_binding_has_null_foreign_tenant_id` — binding status='pending'; assert NULL.
31. `peer_soft_revoke_cascades_bindings_to_inactive` (design AC8 rewritten) — given peer status='active' and one binding status='active', when peer DELETE (which currently soft-revokes), then peer status='revoked' AND binding status='inactive'.
32. `approval_for_inactive_binding_has_null_foreign_tenant_id` — proves cascade is end-to-end honest.

**Gate:**
```bash
cargo check && \
cargo clippy --all-targets -- -D warnings && \
DATABASE_URL=postgres://ione:ione@localhost:5433/ione \
  cargo test --tests phase14_bindings phase09_delivery \
  --no-fail-fast -- --ignored --test-threads=1
```

**Acceptance:** Tests 28–32 pass; existing approval test suite continues to pass; SELECT verification: `SELECT foreign_tenant_id FROM approvals WHERE workspace_id = ? AND peer_id = ?` returns expected value or NULL exactly per binding status.

---

## Design AC ↔ phase gate mapping (revised)

| Design AC | Verifying gate / test |
|---|---|
| 1 (schema integrity) | Phase 1 migration apply + test 3 (cross-org exception) |
| 2 (auto-bind happy path) | Phase 1 test 2 |
| 3 (auto-bind whoami missing) | Phase 1 test 1 |
| 4 (manual fill-in) | Phase 2 test 9 |
| 4a (empty PATCH rejected) | Phase 2 test 7 |
| 5 (refresh detects drift) | Phase 2 test 12 |
| 6 (cross-org guard at DB layer) | Phase 1 test 3 |
| 6+ (cross-org guard at API layer) | Phase 2 tests 16–20, 22 |
| 7 (cascade on workspace delete) | Phase 1 test 4 |
| 8 (peer delete soft-revokes + cascades binding) | Phase 4 test 31 (rewritten from FK RESTRICT interpretation, which doesn't apply here — `delete_peer` is soft-revoke) |
| 9 (audit enrichment) | Phase 4 test 28 |
| 10 (backward compat) | Phase 1 gate runs phase12 + phase11; Phase 3 test 27 explicit |

---

## Self-review checklist

- [x] Every design AC maps to a phase gate (table above).
- [x] Every referenced file exists or appears under "files to create" (verified existence: `src/error.rs`, `src/mcp_server.rs`, `src/connectors/mcp_client.rs`, `src/services/peer.rs`, `src/services/delivery.rs`, `src/repos/approval_repo.rs`, `src/repos/audit_event_repo.rs`, `src/routes/peers.rs`, `src/routes/mod.rs`, `src/models/{approval,audit_event}.rs`, `static/{index.html,app.js,style.css}`).
- [x] Phases are vertical slices (each phase = end-to-end working capability: P1=subscribe+whoami; P2=manual CRUD+UI+cross-org enforcement; P3=routing enforcement; P4=audit enrichment).
- [x] Each gate is a concrete shell command.
- [x] Phase 3 added explicitly because Phase 4 (audit enrichment) is dishonest without it — codified the dependency rather than papering over it.
- [x] AC8 rewritten — original assumed hard delete; current `delete_peer` is soft-revoke; AC reframed as cascade-to-inactive.
- [x] Error variant added (`UnprocessableEntity`) so 422 semantics are real, not implicit.
- [x] `whoami://` added to IONe's own MCP server so phase14 IONe-to-IONe test (#6) can actually exercise active bindings.
- [n/a] Parallel task disjoint sets (no parallel orchestration — medium plan).
