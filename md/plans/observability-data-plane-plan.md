# Observability Data Plane — Implementation Plan

**Design doc:** `md/design/observability-data-plane.md`
**Shape:** large (≈17 files) — but a sequential dependency chain (schema→sink→emit→read→push), not parallel streams. Phases are vertical slices; the Task Manifest routes them but most run in order.
**Stack:** backend-only (`db` + `api`); Rust/Axum + sqlx/Postgres. No UI in v1.

## Waves (per requested sequencing)
- **Wave 1 (one unit):** Phase 1 (OBS-1 sink + OBS-2 emit + OBS-3 schema/provenance) → Phase 2 (Slice 4 read endpoints).
- **Wave 2 (independent follow-on):** Phase 3 (OBS-4 MCP SSE push).
- **Docs:** Phase 4 (requirements seed + backlog status), landed with whichever wave merges.

## Dependencies
- `tokio` mpsc + `tokio::sync::broadcast` — already in tree (used by `PipelineBus`).
- `dashmap` — already in `AppState`. Used for the per-session sequence counter.
- No new crates.

## Verification preconditions (run once before starting)
```
docker compose up -d postgres
cargo sqlx migrate run
cargo check
```
Integration tests run gated: `IONE_SKIP_LIVE=1 DATABASE_URL=postgres://ione:ione@localhost:5433/ione cargo test --test <name> -- --ignored --test-threads=1`. Run `cargo fmt --check` before any commit (CI fmt gate).

---

## Phases

### Phase 1 — Capture path (OBS-1 + OBS-2 + OBS-3)
**Goal:** every federated tool call routed through `route_tool_call` lands exactly one provenance-tagged row in `interaction_events`, written off the hot path.

**Files:**
- `migrations/0045_interaction_events.sql` — **create.** Table + 5 indexes + RLS-parity policy.
- `src/models/interaction_event.rs` — **create.** `InteractionEvent`, `CallerKind`, `Outcome`.
- `src/models/mod.rs` — re-export the new model.
- `src/auth.rs` — **edit.** Add `Principal` resolution (pure fn from `&AuthContext`).
- `src/services/interaction_sink.rs` — **create.** `InteractionSink` (bounded mpsc + broadcast + drop counter + per-session seq) and `spawn_writer`.
- `src/services/mod.rs` — register module.
- `src/repos/interaction_event_repo.rs` — **create.** `insert_batch` (used by writer); query methods stubbed here, filled in Phase 2.
- `src/repos/mod.rs` — register + re-export repo.
- `src/state.rs` — **edit.** Add `pub interaction: Arc<InteractionSink>` to `AppState`; construct in `AppState::new`.
- `src/lib.rs` — **edit.** In `app_with_state` (real-DB boot only, **not** `app_no_db`), take the writer `Receiver` and `spawn_writer(pool, rx)`.
- `src/services/federation.rs` — **edit.** `route_tool_call`: `Instant` at entry; emit at the 4 exits (deny @122, pending @134, allow/error @137).
- `tests/observability_capture_integration.rs` — **create.** Criteria 1–5, 9.

**Code shapes:**

```sql
-- migrations/0045_interaction_events.sql
CREATE TABLE interaction_events (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id          UUID NOT NULL REFERENCES organizations(id) ON DELETE RESTRICT,
    workspace_id    UUID NOT NULL REFERENCES workspaces(id) ON DELETE RESTRICT,
    peer_id         UUID REFERENCES peers(id) ON DELETE RESTRICT,
    peer_name       TEXT,
    tool_name       TEXT NOT NULL,
    caller_kind     actor_kind NOT NULL,            -- existing enum (user/system/peer/service_account)
    caller_user_id  UUID REFERENCES users(id) ON DELETE RESTRICT,
    caller_token_id UUID REFERENCES service_account_tokens(id) ON DELETE RESTRICT,
    caller_peer_id  UUID REFERENCES peers(id) ON DELETE RESTRICT,
    session_id      UUID,
    sequence_number BIGINT,
    outcome         TEXT NOT NULL CHECK (outcome IN ('allow','deny','pending','error')),
    latency_ms      INTEGER,
    detail          JSONB NOT NULL DEFAULT '{}'::jsonb,
    recorded_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT interaction_caller_present
        CHECK (caller_user_id IS NOT NULL OR caller_token_id IS NOT NULL OR caller_peer_id IS NOT NULL),
    CONSTRAINT interaction_seq_positive CHECK (sequence_number IS NULL OR sequence_number >= 1),
    CONSTRAINT interaction_peer_name_when_id CHECK (peer_id IS NULL OR peer_name IS NOT NULL)
);
CREATE INDEX interaction_ws_time      ON interaction_events (workspace_id, recorded_at DESC);
CREATE INDEX interaction_ws_peer_time ON interaction_events (workspace_id, peer_id, recorded_at DESC);
CREATE INDEX interaction_ws_user_time ON interaction_events (workspace_id, caller_user_id, recorded_at DESC)
    WHERE caller_user_id IS NOT NULL;
CREATE INDEX interaction_session_seq  ON interaction_events (session_id, sequence_number);
CREATE INDEX interaction_org_time     ON interaction_events (org_id, recorded_at DESC);
ALTER TABLE interaction_events ENABLE ROW LEVEL SECURITY;
CREATE POLICY interaction_org_isolation ON interaction_events
    USING (org_id = current_setting('app.current_org_id', true)::uuid);  -- inert until session var set (parity w/ 0041)
ALTER TABLE interaction_events SET (autovacuum_vacuum_scale_factor = 0.01);
```

```rust
// src/models/interaction_event.rs
#[derive(sqlx::Type)] #[sqlx(type_name="actor_kind", rename_all="snake_case")]
// reuse existing ActorKind; CallerKind is an alias view. Outcome as &str constants.
pub struct InteractionEvent {
    pub id: Uuid, pub org_id: Uuid, pub workspace_id: Uuid,
    pub peer_id: Option<Uuid>, pub peer_name: Option<String>, pub tool_name: String,
    pub caller_kind: ActorKind,
    pub caller_user_id: Option<Uuid>, pub caller_token_id: Option<Uuid>, pub caller_peer_id: Option<Uuid>,
    pub session_id: Option<Uuid>, pub sequence_number: Option<i64>,
    pub outcome: String, pub latency_ms: Option<i32>,
    pub detail: serde_json::Value, pub recorded_at: DateTime<Utc>,
}
pub mod outcome { pub const ALLOW:&str="allow"; pub const DENY:&str="deny"; pub const PENDING:&str="pending"; pub const ERROR:&str="error"; }
```

```rust
// src/auth.rs — pure, no DB
pub enum Principal {
    User { user_id: Uuid }, ServiceAccount { token_id: Uuid }, Peer { peer_id: Uuid },
}
impl AuthContext {
    pub fn principal(&self) -> Principal { /* is_service_account -> SA(token_id); is_mcp_peer -> Peer(user_id-as-peer or resolved); else User(user_id) */ }
}
// Maps to columns: User->(caller_kind=user, caller_user_id); ServiceAccount->(service_account, caller_token_id); Peer->(peer, caller_peer_id).
```

```rust
// src/services/interaction_sink.rs
const WRITE_CAP: usize = 4096; const BUS_CAP: usize = 256;
const BATCH_MAX: usize = 256; const FLUSH_MS: u64 = 500;
pub struct InteractionSink {
    tx: mpsc::Sender<InteractionEvent>,
    bus: broadcast::Sender<InteractionEvent>,
    dropped: AtomicU64,
    seq: DashMap<Uuid, AtomicU64>,   // session_id -> last assigned
}
impl InteractionSink {
    pub fn new() -> (Arc<Self>, mpsc::Receiver<InteractionEvent>) { /* build channels */ }
    pub fn next_seq(&self, session: Option<Uuid>) -> Option<i64> { /* None if no session; else atomic ++ */ }
    pub fn emit(&self, ev: InteractionEvent) {            // hot path: non-blocking
        let _ = self.bus.send(ev.clone());               // fire-and-forget fanout (OBS-4)
        if self.tx.try_send(ev).is_err() { self.dropped.fetch_add(1, Relaxed); }
    }
    pub fn dropped(&self) -> u64 { self.dropped.load(Relaxed) }
    pub fn subscribe_workspace(&self, ws: Uuid) -> impl Stream<Item=InteractionEvent> { /* mirror PipelineBus */ }
}
// writer: select! over rx.recv() and tokio::time::interval(FLUSH_MS); accumulate to BATCH_MAX then
// repo.insert_batch(); on interval flush partial; on rx closed -> drain + final flush + return (graceful drain).
pub fn spawn_writer(pool: PgPool, rx: mpsc::Receiver<InteractionEvent>) -> JoinHandle<()> { /* tokio::spawn loop */ }
```

```rust
// src/services/federation.rs route_tool_call — emission (domain shape, not literal)
let started = Instant::now();
// ... after RBAC fail @122:
emit_interaction(state, &peer, raw_tool, auth, outcome::DENY, None, started, ws);   // latency None
// ... pending @134:
emit_interaction(state, &peer, raw_tool, auth, outcome::PENDING, Some(elapsed), started, ws);
// ... allow path @137: match invoke_peer_tool { Ok -> ALLOW, Err -> ERROR } then emit with elapsed.
// emit_interaction builds InteractionEvent (principal->caller cols, session_id from auth, seq via sink.next_seq) and calls state.interaction.emit(ev).
```

```rust
// src/repos/interaction_event_repo.rs
pub async fn insert_batch(&self, rows: &[InteractionEvent]) -> anyhow::Result<u64>  // QueryBuilder push_values, no RETURNING
```

**Open-question resolution (OQ1 — session correlation):** `session_id` = `auth.session_id` (login session) in v1. Threading the MCP transport session id from `mcp_server.rs:1039` is a follow-up noted in the design; v1 ships with the login session as the anchor and `session_id` null for sessionless headless calls. Does **not** change the schema.

**Gate:**
```
cargo sqlx migrate run && cargo check && cargo clippy --all-targets -- -D warnings
IONE_SKIP_LIVE=1 DATABASE_URL=postgres://ione:ione@localhost:5433/ione \
  cargo test --test observability_capture_integration -- --ignored --test-threads=1
```
**Acceptance:** test asserts (1) one `allow` row with correct peer/tool/`latency_ms>=0`; (2) one `deny` row, `latency_ms` null; (3) service-account caller → `caller_kind=service_account`, `caller_token_id` set, `caller_user_id` null; (4) 20-call session → seq 1..20 strict; (5) saturated channel → call still returns, `dropped()` advances; (9) dropping the sink flushes buffered rows.

---

### Phase 2 — Read surface (Slice 4)
**Goal:** an admin can query captured interactions: filtered list, aggregates, per-session replay.

**Files:**
- `src/repos/interaction_event_repo.rs` — **edit.** Add `list_filtered` (keyset), `count_by_bucket`, `count_by_principal`, `outcome_summary`, `list_session_steps`. Mirror `audit_event_repo.rs` QueryBuilder + `push_filtered_from_where` patterns; every query takes `org_id` and joins/filters on it.
- `src/routes/interaction_events.rs` — **create.** `list`, `aggregates`, `session_steps` handlers. Admin gate via existing coc≥80 check (copy from `audit_aggregates.rs`).
- `src/routes/mod.rs` — **edit.** Register 3 routes + `pub mod interaction_events;` (place near the `audit-aggregates`/`audit-export` block ~line 167–175).
- `tests/observability_read_integration.rs` — **create.** Criteria 6–8.

**Code shapes:**
```rust
// routes — registration (src/routes/mod.rs)
.route("/api/v1/workspaces/:id/interaction-events",   get(interaction_events::list))
.route("/api/v1/workspaces/:id/interaction-aggregates", get(interaction_events::aggregates))
.route("/api/v1/workspaces/:id/interaction-sessions/:session_id", get(interaction_events::session_steps))
```
Aggregate op routing + caps per design API table: `op∈{count_by_bucket,count_by_principal,outcome_summary}`, `bucket` required iff `count_by_bucket` (else 400), window ≤90d, ≤1000 buckets, ≤200 principals. `count_by_principal` group key = resolved principal `{caller_kind, caller_id, count, deny_count}`. `peer_id` filters rows for all ops. List: window ≤90d, `until` default now / `since` default `until−90d`, keyset cursor on `(recorded_at, id)`, `limit` 1..200.

**Gate:**
```
cargo check && cargo clippy --all-targets -- -D warnings && cargo fmt --check
IONE_SKIP_LIVE=1 DATABASE_URL=… cargo test --test observability_read_integration -- --ignored --test-threads=1
```
**Acceptance:** (6) `outcome_summary` returns `{peer,allow,50}`+`{peer,deny,10}`; (7) `count_by_bucket&bucket=hour` sums to total, `bucket` sent to `outcome_summary` → 400; (8) non-admin → 403, foreign-org caller → 404 with zero foreign rows.

---

### Phase 3 — MCP SSE push (OBS-4 / Slice 5) — independent wave
**Goal:** a subscribed MCP client receives interaction events for workspaces it can see, in real time.

**Files:**
- `src/mcp_server.rs` — **edit.** `SseQuery` gains `workspace_id: Option<Uuid>`. When present: `resolve_auth` → `ensure_workspace_in_org(org)` (403/404 on mismatch) → merge `state.interaction.subscribe_workspace(ws)` into the SSE stream, framing each event as a JSON-RPC notification `{"jsonrpc":"2.0","method":"notifications/tools/interaction","params":<InteractionEvent>}`. Stream becomes long-lived (merge keep-alive + broadcast) — model on `routes/pipeline_events.rs::stream_events`. Absent `workspace_id` → existing finite behavior unchanged.
- `tests/observability_push_integration.rs` — **create.** Criteria 10–11.

**Code shape:**
```rust
struct SseQuery { session: Option<String>, workspace_id: Option<Uuid> }
// if let Some(ws) = query.workspace_id { authz; let live = state.interaction.subscribe_workspace(ws)
//   .map(|ev| Ok(Event::default().event("message").json_data(jsonrpc_notification(ev)).unwrap()));
//   merge init_event ++ live, keep-alive } else { existing iter }
```
**Gate:**
```
cargo check && cargo clippy --all-targets -- -D warnings && cargo fmt --check
IONE_SKIP_LIVE=1 DATABASE_URL=… cargo test --test observability_push_integration -- --ignored --test-threads=1
```
**Acceptance:** (10) routed call in W → subscriber on `GET /mcp?workspace_id=W` receives matching `notifications/tools/interaction` within 200ms; cross-org subscribe → 403. (11) subscriber to workspace B never receives workspace A's event.

---

### Phase 4 — Requirements seed + backlog status
**Goal:** contract source-of-truth exists; backlog reflects status.

**Files:**
- `md/requirements/active/observability-data-plane.md` — **create.** The 4 endpoint contracts + `InteractionEvent` shape + per-op aggregate shapes + authz tiers, lifted verbatim from the design's "API contracts" section. This becomes the contract record (design stays the rationale record).
- `md/plans/infrastructure-backlog.md` — **edit.** Mark OBS-1..4 statuses (Partial — pending walkthrough on merge).

**Gate:** `test -f md/requirements/active/observability-data-plane.md` and grep confirms all 4 endpoints present. The `coverage-audit.sh` PostToolUse hook passes (integration tests named `*_integration.rs` exist for each capture/read/push slice).
**Acceptance:** requirements file lists `/interaction-events`, `/interaction-aggregates`, `/interaction-sessions/:id`, and the `/mcp?workspace_id=` SSE contract.

---

## Task Manifest

| Task | Agent | Files | Depends On | Gate |
|------|-------|-------|------------|------|
| T1: migration + model + Principal | codex | `migrations/0045_interaction_events.sql`, `src/models/interaction_event.rs`, `src/models/mod.rs`, `src/auth.rs` | — | `cargo sqlx migrate run && cargo check` |
| T2: sink + writer + state + boot wiring | claude-code | `src/services/interaction_sink.rs`, `src/services/mod.rs`, `src/state.rs`, `src/lib.rs`, `src/repos/interaction_event_repo.rs`, `src/repos/mod.rs` | T1 | `cargo check && cargo clippy -- -D warnings` |
| T3: router emission | claude-code | `src/services/federation.rs` | T2 | `cargo check` |
| T4: capture integration tests | test-writer | `tests/observability_capture_integration.rs` | T3 | Phase 1 gate |
| T5: read repo + routes | claude-code | `src/repos/interaction_event_repo.rs`, `src/routes/interaction_events.rs`, `src/routes/mod.rs` | T2 | Phase 2 gate |
| T6: read integration tests | test-writer | `tests/observability_read_integration.rs` | T5 | Phase 2 gate |
| T7: MCP SSE push | claude-code | `src/mcp_server.rs` | T2 | `cargo check` |
| T8: push integration tests | test-writer | `tests/observability_push_integration.rs` | T7 | Phase 3 gate |
| T9: requirements + backlog | codex | `md/requirements/active/observability-data-plane.md`, `md/plans/infrastructure-backlog.md` | — | Phase 4 gate |

**Parallelism note:** T5 and T7 both depend only on T2 and touch disjoint files (`routes/` + `repo` vs `mcp_server.rs`) — they may run in parallel after T2/T3 land. T2 edits `interaction_event_repo.rs` (writer path) and T5 also edits it (query methods) — **sequence T5 after T2**, not parallel, to avoid the same-file conflict. All else is dependency-ordered.

---

## Self-review
1. **Every design acceptance criterion maps to a phase gate?** Yes — 1–5,9 → Phase 1; 6–8 → Phase 2; 10–11 → Phase 3.
2. **Every file exists now or is listed as create?** Yes — `federation.rs`, `state.rs`, `lib.rs`, `auth.rs`, `routes/mod.rs`, `mcp_server.rs`, `audit_event_repo.rs` (pattern source) verified present; all new files marked **create**.
3. **Phases are vertical slices?** Yes — Phase 1 is capture end-to-end (schema→write→emit→verify), not a layer stack.
4. **Gates are concrete commands?** Yes — migrate/check/clippy/named integration test per phase.
5. **Parallel tasks disjoint?** T5/T7 disjoint; the one shared-file hazard (`interaction_event_repo.rs` across T2/T5) is called out and sequenced.
