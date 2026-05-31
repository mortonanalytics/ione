# Push Event Ingress — Implementation Plan

**Design doc:** [md/design/push-ingress.md](../design/push-ingress.md)
**Shape:** Large — db + api + ui, ~18 files, 3 sequential vertical phases + Task Manifest
**Stack:** Rust/Axum + Postgres/sqlx backend; vanilla JS/CSS frontend
**Carried defaults:** replay window ±5 min (OQ-5); webhook-secret encryption via a separate `IONE_WEBHOOK_SECRET_KEY`, falling back to `IONE_TOKEN_KEY` in dev with a startup warning (OQ-1); top-level envelope `severity` (OQ-4, resolved in design).

Phases are **sequential**: Phase 2 needs the secret column + crypto from Phase 1; Phase 3 needs the receiver from Phase 2. MCP `notifications/*` is **out of scope** (v0.2).

## Dependencies

None new. Reuse: `hmac` + `sha2` (already in `Cargo.toml`, used in `src/auth.rs`), `subtle::ConstantTimeEq` (already imported, `src/middleware/mcp_bearer.rs`), `rand`/`OsRng` for secret generation, `axum::extract::DefaultBodyLimit` (axum already present).

---

## Phase 1 — Webhook secret provisioning

**Goal:** An operator provisions a per-peer HMAC signing secret; IONe stores it encrypted and returns it once for pasting into the app config.

**Decision (resolves the struct-vs-bespoke ambiguity):** `webhook_secret_ciphertext` is **NOT** added to the `Peer` struct. The secret is read only by the dedicated `get_with_webhook_secret` repo method (bespoke SELECT). This keeps every existing peer query and `Peer` `FromRow` untouched, and avoids loading the secret on normal peer fetches (security guidance).

**Files:**
- `migrations/0027_peer_webhook_secret.sql` — CREATE: add encrypted-secret column to `peers`
- `src/util/token_crypto.rs` — add `encrypt_webhook_secret(&[u8]) -> Result<Vec<u8>>` / `decrypt_webhook_secret(&[u8]) -> Result<String>` using `IONE_WEBHOOK_SECRET_KEY` (fallback `IONE_TOKEN_KEY` + `tracing::warn!` in dev)
- `src/repos/peer_repo.rs` — add `set_webhook_secret`, `get_with_webhook_secret`
- `src/routes/webhooks.rs` — CREATE: `provision_webhook` handler + `ProvisionWebhookResponse`
- `src/routes/mod.rs` — add `pub mod webhooks;`; register `POST /api/v1/peers/:id/webhook/provision` in the **protected** group (after line 199, near the other `/api/v1/peers/:id` routes)
- `static/index.html`, `static/app.js`, `static/style.css` — "Provision webhook" control on the peer/Federate-peer surface; one-time secret + URL reveal
- `tests/phase_push_ingress.rs` — CREATE: provisioning test (AC-1)
- `tests/contract_api_routes.rs` — add `route_post_peer_webhook_provision_registered`

**Code shapes:**
```sql
-- 0027_peer_webhook_secret.sql
ALTER TABLE peers ADD COLUMN webhook_secret_ciphertext BYTEA NULL;
COMMENT ON COLUMN peers.webhook_secret_ciphertext IS
  'AES-256-GCM encrypted HMAC-SHA256 inbound-webhook signing secret (IONE_WEBHOOK_SECRET_KEY). NULL = not provisioned.';
```
```rust
// peer_repo.rs
pub async fn set_webhook_secret(&self, peer_id: Uuid, ciphertext: &[u8]) -> anyhow::Result<()>;
// UPDATE peers SET webhook_secret_ciphertext = $1 WHERE id = $2
pub async fn get_with_webhook_secret(&self, peer_id: Uuid)
    -> anyhow::Result<Option<(Peer, Option<Vec<u8>>)>>;
// bespoke SELECT: existing Peer columns + webhook_secret_ciphertext, mapped manually
// (the ciphertext is the tuple's second element, NOT a Peer field)

// routes/webhooks.rs
#[derive(Serialize)] #[serde(rename_all = "camelCase")]
pub struct ProvisionWebhookResponse { pub peer_id: Uuid, pub signing_secret: String, pub webhook_url: String }

pub async fn provision_webhook(
    State(state): State<AppState>, Extension(ctx): Extension<AuthContext>, Path(peer_id): Path<Uuid>,
) -> Result<Json<ProvisionWebhookResponse>, AppError>;
// ensure_peer_in_org (reuse from routes/peers.rs — promote to pub(crate));
// 32 bytes OsRng -> hex; encrypt_webhook_secret; PeerRepo::set_webhook_secret;
// webhook_url = format!("{}/webhooks/peer/{}", base_url, peer_id)
// AUDIT: write an audit_event verb="webhook.provisioned" (object=peer) — payload MUST NOT
//   contain the secret or ciphertext (only peer_id, rotated:bool). Re-provision = rotate.
```
Notes:
- `ensure_peer_in_org` currently lives private in `src/routes/peers.rs:271` — promote to `pub(crate)` and import, do not re-implement.
- **`Config` has no `public_base_url`** (verified — only `bind`, `oauth_issuer`, `ollama_*`, `static_dir`). For the returned `webhook_url`, use `state.config.oauth_issuer` as the public base (it is already the externally-reachable issuer URL). Do **not** reference a nonexistent `public_base_url`.

**Gate:**
```
cargo check && cargo clippy --all-targets -- -D warnings && \
IONE_WEBHOOK_SECRET_KEY=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA= \
DATABASE_URL=postgres://ione:ione@localhost:5433/ione \
  cargo test --test phase_push_ingress provision -- --ignored --test-threads=1
```
**Acceptance:** AC-1 — provision returns ≥32-hex `signingSecret` + `webhookUrl`; `peers.webhook_secret_ciphertext` non-null after.

---

## Phase 2 — Signed webhook receiver + replay protection

**Goal:** A peer POSTs a signed event; IONe verifies HMAC over the raw body, rejects replays/stale/oversized, and dedups by `(id, peer_id)`.

**Files:**
- `migrations/0028_webhook_dedup.sql` — CREATE: `webhook_events_seen` table + `received_at` index
- `src/repos/webhook_event_repo.rs` — CREATE: `try_insert_seen`, `cleanup_expired`
- `src/repos/mod.rs` — add `pub mod webhook_event_repo;` + re-export
- `src/routes/webhooks.rs` — add `receive_webhook` handler + `WebhookEnvelope` + `WebhookAckResponse` + signature parse/verify helper + `validate_envelope`
- `src/error.rs` — add `WebhookRejected` variant → fixed generic 400 body `{"error":"webhook_rejected"}` and `WebhookUnauthorized` → 401 `{"error":"webhook_unauthorized"}`. Do **NOT** reuse `AppError::Unauthorized` (it returns "Sign in to access this resource" — wrong/leaky for machine auth, verified `error.rs:85`).
- `src/routes/mod.rs` — register `POST /webhooks/peer/:peer_id` in the **public** group (lines 47–67), with `DefaultBodyLimit::max(256*1024)` layered on that route
- `tests/phase_push_ingress.rs` — receiver tests (AC-3, AC-4, AC-5, AC-9, AC-11 peer-status, AC-12 validation)
- `tests/contract_api_routes.rs` — add `route_post_webhook_peer_registered`

**Code shapes:**
```sql
-- 0028_webhook_dedup.sql
CREATE TABLE webhook_events_seen (
  event_id    TEXT NOT NULL,
  peer_id     UUID NOT NULL REFERENCES peers(id) ON DELETE CASCADE,
  received_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (event_id, peer_id)
);
CREATE INDEX webhook_events_seen_received ON webhook_events_seen (received_at);
```
```rust
// webhook_event_repo.rs — the dedup insert runs INSIDE the Phase-3 ingest transaction
// (takes a transaction executor, not &self.pool), so a dedup row is committed ONLY
// alongside successful signal creation. See Phase 3 transaction contract.
pub async fn try_insert_seen_tx(tx: &mut sqlx::PgConnection, event_id: &str, peer_id: Uuid)
    -> anyhow::Result<bool>;  // INSERT ... ON CONFLICT DO NOTHING; Ok(rows_affected == 1)
pub async fn cleanup_expired(&self) -> anyhow::Result<u64>;
// DELETE FROM webhook_events_seen WHERE received_at < now() - INTERVAL '72 hours'

// routes/webhooks.rs
#[derive(Deserialize)] // snake_case wire format (NO rename_all)
pub struct WebhookEnvelope {
    pub id: String, pub r#type: String, pub occurred_at: DateTime<Utc>,
    pub peer_id: Uuid, pub foreign_tenant_id: String,
    pub severity: Option<String>, pub data: serde_json::Value, pub approval_required: bool,
}
#[derive(Serialize)] // snake_case
pub struct WebhookAckResponse { pub ok: bool, pub duplicate: bool,
    #[serde(skip_serializing_if="Option::is_none")] pub signal_ids: Option<Vec<Uuid>> }

pub async fn receive_webhook(
    State(state): State<AppState>, Path(peer_id): Path<Uuid>, headers: HeaderMap, body: Bytes,
) -> Result<Json<WebhookAckResponse>, AppError>;
```
Receiver order (must match design decision-flow):
1. Load peer by path `peer_id`. **Reject unless `status = Active`** (PeerStatus has Revoked/Paused/Error/pending_* — verified `models/peer.rs:8`) AND `webhook_secret_ciphertext IS NOT NULL` → else `WebhookUnauthorized` (generic).
2. Decrypt secret. Parse `X-IONe-Signature: t=<int>,v1=<hex>` — reject malformed/missing/duplicate fields or wrong hex length (64 chars) → `WebhookRejected`.
3. **HMAC-SHA256 over BYTES, not a UTF-8 string**: feed `t.as_bytes()`, then `b"."`, then the **raw body `&Bytes`** into the MAC (do not `String::from_utf8` the body). `ConstantTimeEq` against decoded `v1` (mismatch→`WebhookUnauthorized`).
4. `|now - t| ≤ 5min` and `|t - occurred_at| ≤ 30s` (fail→`WebhookRejected`).
5. `validate_envelope`: `id` non-empty ≤255; `type` matches `^[a-z0-9._/-]{1,255}$`; `foreign_tenant_id` non-empty ≤512; `data` is a JSON object; body `peer_id == path peer_id` (any fail→`WebhookRejected`).
6. Hand to `ingest_webhook_event` (Phase 3) which owns the **transactional** dedup+fan-in. The handler does NOT insert dedup itself.
Body-size 413 is automatic from `DefaultBodyLimit`. All rejections use fixed generic bodies; specifics logged internally only.

**Gate:**
```
cargo check && cargo clippy --all-targets -- -D warnings && \
IONE_WEBHOOK_SECRET_KEY=... DATABASE_URL=postgres://ione:ione@localhost:5433/ione \
  cargo test --test phase_push_ingress -- --ignored --test-threads=1
```
**Acceptance:** AC-3 (bad sig → 401, no rows), AC-4 (replay → `{ok:true,duplicate:true}`, one signal), AC-5 (stale `occurred_at`/`t` skew → 400), AC-9 (>256KB → 413), **AC-11 (peer status: a Revoked/Paused peer → 401 even with a valid signature)**, **AC-12 (validation negatives: invalid `type`, overlong `foreign_tenant_id`, non-object `data`, malformed/duplicate signature fields, wrong hex length, missing required field → each 400, no signal)**.

---

## Phase 3 — Fan-in to the signal chain + approval gating

**Goal:** A verified event creates a signal synchronously in every matching workspace; `approval_required` (or severity ∈ {flagged,command}) forces the draft/approval path and skips auto-exec.

**Files:**
- `migrations/0029_signals_approval_required.sql` — CREATE: add `approval_required` to `signals`
- `src/models/signal.rs` — add field `approval_required: bool`
- `src/repos/signal_repo.rs` — extend `insert` with `approval_required: bool` param; update INSERT + RETURNING + all `list`/`get` SELECTs that enumerate signal columns
- `src/services/webhook_ingress.rs` — CREATE: transactional `ingest_webhook_event`, `IngestOutcome`, `map_severity`, evidence builder
- `src/services/mod.rs` — add `pub mod webhook_ingress;`
- `src/services/router.rs` — add a shared guard `fn forced_target(approval_required: bool, severity: &str) -> Option<RoutingTarget>` (Some(Draft) when `approval_required || severity ∈ {flagged,command}`). `classify_survivor` already SELECTs `sig.title, sig.body, sig.severity` (router.rs:233) — **add `sig.approval_required`** and short-circuit via `forced_target` before the Ollama call. `classify_with_response` (router.rs:175) takes `severity: &str` and has **no SELECT** — add an `approval_required: bool` parameter and call `forced_target` likewise. Both paths use the same helper so production and the test hook agree.
- `src/services/delivery.rs` — `process_draft` (line 283) calls `auto_exec::evaluate_and_invoke` first (line 296); fetch the signal's `approval_required` and, when true, **skip auto-exec** and create the pending approval directly. Use `ApprovalRepo::create_pending_with_foreign_tenant` (exists, repo.rs:21) with the foreign-tenant from the signal's evidence so the approval row carries provenance.
- `src/services/scheduler.rs` — **(a)** add `run_delivery_for_workspace` after `run_router_for_workspace` in the tick: select routing_decisions with no downstream artifact/delivery yet and call `delivery::process_routing_decision` for each (budget-limited, mirrors the router pass). **(b)** call `WebhookEventRepo::cleanup_expired` once per `run_tick`.
- `tests/phase_push_ingress.rs` — fan-in tests (AC-2, AC-6, AC-7, AC-8, AC-10, AC-13 dedup-no-poison, AC-14 production delivery)

**Code shapes:**
```sql
-- 0029_signals_approval_required.sql
ALTER TABLE signals ADD COLUMN approval_required BOOLEAN NOT NULL DEFAULT false;
```
```rust
// signal_repo.rs — insert gains a param (existing callers rules/generator/scheduler pass false)
pub async fn insert(&self, workspace_id: Uuid, source: SignalSource, title: &str, body: &str,
    evidence: serde_json::Value, severity: Severity, generator_model: Option<&str>,
    approval_required: bool) -> anyhow::Result<Signal>;

// services/webhook_ingress.rs
pub enum IngestOutcome { Created(Vec<Uuid>), Duplicate, NoBinding }
pub async fn ingest_webhook_event(state: &AppState, peer_id: Uuid, env: &WebhookEnvelope)
    -> anyhow::Result<IngestOutcome>;
```
**Transaction contract (resolves the dedup-poisoning finding):** `ingest_webhook_event` runs everything in ONE `pool.begin()` transaction:
1. Resolve bindings: `SELECT b.workspace_id FROM workspace_peer_bindings b JOIN workspaces w ON w.id=b.workspace_id JOIN peers p ON p.id=b.peer_id WHERE b.peer_id=$1 AND b.foreign_tenant_id=$2 AND b.status='active' AND w.closed_at IS NULL AND p.org_id=w.org_id` (joins ensure live workspace + same-org, per finding #6). **If empty → rollback, return `NoBinding`** (handler → 400). No dedup row persists, so a retry after the operator fixes the binding still works.
2. `try_insert_seen_tx` on the same tx. If it reports duplicate (0 rows) → rollback, return `Duplicate` (handler → 200 `{ok:true,duplicate:true}`).
3. For each workspace: `SignalRepo::insert(... approval_required = env.approval_required || matches!(severity, Flagged|Command))` with `evidence = {peer_id, event_id, occurred_at, type, foreign_tenant_id, data}`.
4. **Commit.** Dedup row is committed only with the signals — never on no-binding or failure.

Handler maps `Created(ids)→200 {signal_ids}`, `Duplicate→200 {duplicate:true}`, `NoBinding→WebhookRejected (400)`.
`map_severity(&Option<String>) -> Severity`: command→Command, flagged→Flagged, else Routine.

**Gate:**
```
cargo check && cargo clippy --all-targets -- -D warnings && \
IONE_WEBHOOK_SECRET_KEY=... IONE_SKIP_LIVE=1 DATABASE_URL=postgres://ione:ione@localhost:5433/ione \
  cargo test --test phase_push_ingress -- --ignored --test-threads=1
```
**Acceptance:**
- AC-2 (valid event → signal in W, source=connector_event).
- AC-6 (no binding → 400, and **no `webhook_events_seen` row** — verify the dedup ledger is empty for that event id).
- AC-7 (approval_required → draft + artifact + pending approval, auto_exec did NOT fire).
- AC-8 (severity=command + approval_required=false → still draft).
- AC-10 (cleanup purges >72h rows).
- **AC-13 (dedup-no-poison, the finding-1 regression test):** POST event E with no binding → 400; create the active binding; POST the *same* event E again → 200 `{duplicate:false}` with a signal created (NOT swallowed as duplicate).
- **AC-14 (production delivery, the finding-2 regression test):** with `approval_required=true`, drive the **real scheduler tick** (`run_tick`/`run_delivery_for_workspace`), NOT `classify_with_response` — assert an `artifacts` row + pending `approvals` row (with `foreign_tenant_id` populated) exist afterward. This proves the route→delivery chain runs in production, not just via the test hook.

---

## Task Manifest

| Task | Agent | Files | Depends On | Gate |
|------|-------|-------|------------|------|
| T1: migrations 0027/0028/0029 | codex | `migrations/0027_*`, `migrations/0028_*`, `migrations/0029_*` | — | `sqlx migrate run` clean |
| T2: token_crypto webhook key | codex | `src/util/token_crypto.rs` | — | `cargo test token_crypto` |
| T3: peer repo secret methods | claude-code | `src/repos/peer_repo.rs` | T1 | `cargo check` |
| T4: provision handler + route | claude-code | `src/routes/webhooks.rs`, `src/routes/mod.rs`, `src/routes/peers.rs` (pub ensure_peer_in_org) | T2,T3 | Phase 1 gate |
| T5: provision UI | codex | `static/index.html`, `static/app.js`, `static/style.css` | T4 | `tsc`/manual (no TS; grep) |
| T6: webhook errors + dedup repo + receiver handler | claude-code | `src/error.rs`, `src/repos/webhook_event_repo.rs`, `src/repos/mod.rs`, `src/routes/webhooks.rs`, `src/routes/mod.rs` | T4 | Phase 2 gate |
| T7: signal column + repo param | claude-code | `migrations/0029_*` (with T1), `src/models/signal.rs`, `src/repos/signal_repo.rs` | T1 | `cargo check` (all insert callers updated) |
| T8: transactional fan-in service | claude-code | `src/services/webhook_ingress.rs`, `src/services/mod.rs` | T6,T7 | `cargo check` |
| T9: router guard helper + delivery auto_exec skip + foreign-tenant approval | claude-code | `src/services/router.rs`, `src/services/delivery.rs` | T7 | Phase 3 gate |
| T10: scheduler delivery pass + dedup cleanup | claude-code | `src/services/scheduler.rs` | T6,T9 | AC-14 (production delivery) |
| T11: tests | claude-code | `tests/phase_push_ingress.rs`, `tests/contract_api_routes.rs` | T4,T6,T8,T9,T10 | full Phase 1–3 gates |

`webhooks.rs` and `mod.rs` appear in T4 and T6 — those tasks must run **sequentially** (T6 after T4), not in parallel. `signal_repo.rs::insert` signature change (T7) fans out to existing callers (rules, generator, scheduler) — claude-code must update all of them to pass `false`. T10 is now claude-code (not codex): wiring the delivery pass into the tick touches existing scheduler control flow and must not double-deliver (idempotency relies on `process_routing_decision`'s existing already-processed guard at delivery.rs:75).

---

## Self-review

1. **Every design AC maps to a gate?** AC-1→P1; AC-3,4,5,9,11,12→P2; AC-2,6,7,8,10,13,14→P3. ✓
2. **Every file exists or is listed new?** New: 3 migrations, `routes/webhooks.rs`, `services/webhook_ingress.rs`, `repos/webhook_event_repo.rs`, `tests/phase_push_ingress.rs`. Edits cite verified locations (`signal_repo.rs:17`, `router.rs:175/233`, `delivery.rs:283/296`, `peers.rs:271`, `approval_repo.rs:21`, `scheduler.rs:401`, `config.rs:4`, `error.rs:85`, `mod.rs:47-67/199`). ✓
3. **Vertical slices?** Yes — provisioning / receiving / fan-in, each end-to-end. Sequential by data dependency. ✓
4. **Concrete gate commands?** Yes. ✓
5. **Parallel tasks disjoint?** T4/T6 share `webhooks.rs`/`mod.rs` → sequenced. Others dependency-ordered. ✓

## Notes / risks (incl. Codex review remediations)
- **Delivery is not wired into the tick today (pre-existing gap, finding #2).** `process_routing_decision` has no production caller — routing decisions accumulate undelivered, so no signal ever becomes an artifact/approval in production. This feature's `approval_required` gate is meaningless without it, so T10 adds a delivery pass to `run_tick`. **This fixes the whole approval chain, not just webhooks** — a scope-adjacent fix the feature depends on. AC-14 is the regression test (uses the real tick, not the `classify_with_response` hook).
- **Dedup is transactional (finding #1).** The dedup row commits only with the signals; no-binding/failure → rollback → no poisoning. AC-13 is the regression test.
- **HMAC over raw bytes, not a UTF-8 string (finding #5)** — `t.as_bytes()` + `b"."` + raw body `Bytes`. Webhook auth uses dedicated `WebhookRejected`/`WebhookUnauthorized` errors, not `AppError::Unauthorized` ("Sign in…").
- **`signal_repo.insert` signature change is a fan-out edit** — rules/generator/scheduler callers pass `approval_required: false`. `cargo check` catches misses.
- **Webhook secret stays out of the `Peer` struct** — only `get_with_webhook_secret` reads it; no peer-query churn.
- **Router guard is a shared helper** (`forced_target`) called by both `classify_survivor` (has a SELECT — add `sig.approval_required`) and `classify_with_response` (no SELECT — add an `approval_required` param), so test hook and production never diverge.
- **Foreign-tenant provenance (finding #3):** approval/audit rows for webhook drafts use `create_pending_with_foreign_tenant` (exists) + `insert_with_foreign_tenant`; provision writes `webhook.provisioned` audit with no secret in the payload.
- **`Config` has no `public_base_url`** — use `oauth_issuer` for the returned `webhook_url`.
