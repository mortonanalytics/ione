# MCP Federation Layer — Implementation Plan

**Design doc:** [md/design/mcp-federation.md](../design/mcp-federation.md)
**Shape:** large (6 feature slices, ~20+ files across db/api/ui, ~11–12d). Owner elected to build all six slices including D/E (long-lived sessions) — risk accepted per the design's Owner Override.
**Stack:** Rust/Axum 0.7 + sqlx/Postgres, single binary, Tokio; static HTML+JS shell (`static/app.js`). Verification: `cargo check`, `cargo clippy`, `cargo test`, `cargo sqlx migrate run`, `npx tsc`/playwright for UI where present.

## Code-vs-design corrections (verified this session)

These override the design where it cited code loosely — implementers follow the plan, not the design, on these:
- **SSRF helpers split across TWO files (corrected after Codex review — re-verified):**
  - `guarded_client(timeout) -> reqwest::Client` (redirect::Policy::none + IP guard) lives in **`src/util/url_guard.rs:51`**. Use this for the long-lived session + the mcp_client (C-2).
  - `ensure_public_url(raw)` lives in **`src/util/safe_http.rs:26`** — use this in `validate_mcp_url` (C-2). `safe_http.rs` does **not** define `guarded_client`.
  - `url_guard.rs` **exists** (an earlier draft of this plan wrongly said it didn't). `url_guard` allows private HTTPS + localhost/private HTTP in constrained cases; `safe_http::ensure_public_url` rejects private resolutions — **these two have different policies; see the private-peer decision below before wiring.**
- **The scheduler is `src/services/scheduler.rs` (line ~99), NOT `src/connectors/scheduler.rs`** (corrected after Codex review). Phase 2 (Slice C) edits `src/services/scheduler.rs`.
- `workspace_peer_binding_repo.rs` exists at `src/repos/workspace_peer_binding_repo.rs` (used for C-1 tenant resolution + workspace-scoped discovery).
- Next migration number is **0033** (latest is `0032_peer_refresh_token_ciphertext.sql`).
- `dashmap` and `governor` are **not** in `Cargo.toml` — new deps (Phase 0).
- Fan-in signature (verified): `webhook_ingress::ingest_webhook_event(state: &AppState, peer_id: Uuid, env: &WebhookEnvelope) -> anyhow::Result<IngestOutcome>`. **`WebhookEnvelope` lives in `src/routes/webhooks.rs`** (fields: `id, type, occurred_at, peer_id, foreign_tenant_id, severity: Option<String>, data, approval_required`), NOT in `models/`. Slice D builds a `WebhookEnvelope` and passes `peer_id` from the authenticated session; per C-1 it must **override `env.foreign_tenant_id` with the value from the stored binding** (don't trust the notification's).
- `AppState` today (verified): `http, ollama, pipeline_bus, config, pool, default_user_id, default_workspace_id`. No `scheduler_handle` field. Phase 0 adds the federation fields. Note `AppState` derives `Clone` and is constructed in `AppState::new` — new `Arc` fields must be initialized there.
- `tokio-stream = "0.1"` (features `sync`) and `once_cell = "1"` already present; `reqwest 0.12` already has the `stream` feature. SSE-client line parsing reuses `reqwest::Response::bytes_stream()` + `tokio-stream`.

## Dependencies (new)

- `dashmap = "6"` — concurrent maps for the manifest/slice caches + per-peer governor/breaker state (lock-free reads on the hot path).
- `governor = "0.6"` — per-peer token-bucket rate limiting (Slice E). Tokio-compatible.
- (No new crate for SSE client: reuse `reqwest` `stream` + `tokio-stream` line accumulation.)

---

## Decisions folded from Codex round-2 review

- **`/mcp tools/list` scope = active-workspace-scoped (DECIDED).** The MCP session selects a workspace (via `MCP-Session-Id`-bound state or a workspace param at `initialize`); `tools/list` returns only tools from peers bound to that workspace (`workspace_peer_bindings`), role-filtered for the caller. Never advertises a tool the caller can't invoke. This shapes Slice A + the conformance baseline.
- **`tools/get` / `expand_uri` lazy schema is a NON-STANDARD IONe extension (DECIDED).** Standard MCP `tools/list` returns full `inputSchema` and supports pagination, not `tools/get`. Slice B's lazy expansion is an IONe optimization layered on `slice://`; for a standard peer that doesn't support it, **fall back to the `inputSchema` already in the peer's paginated `tools/list`**. Label it as an extension in the playbook.
- **Manifest fetch must paginate** `tools/list` and `resources/list` (cursor) or large peers silently truncate.
- **D/E hard prerequisite (strengthened):** the first MCP-notification-emitting peer is now a **hard gate on Phase 5**, not an open item — Phase 5 does not start until a peer is named and emitting against a real stream.

## Phases

> Phasing is risk-ascending and cache-dependency-ordered: Phase 0 (prereqs + MCP conformance) → A (cache + routing + workspace-scoped discovery) → C (freshness) → F (browse) → B (slices) → D (sessions) → E (breakers). Each phase is a vertical slice. Phase 0 is the allowed scaffolding+correctness exception.

### Phase 0 — Security/correctness prerequisites + scaffolding
**Goal:** the H-1 approval-floor bug is fixed, the C-2 SSRF holes are closed, peer tokens leave connector config (H-4), and the DB/deps/AppState scaffolding for all later slices exists.
**Files:**
- `src/services/generator.rs` — when severity is `Flagged`/`Command`, insert signal with `approval_required = true` (today hard-codes `false`). (H-1)
- `src/services/rules.rs` — same fix at rule-signal insertion. (H-1)
- `src/services/peer.rs` — `validate_mcp_url`: if `IONE_ALLOW_PRIVATE_PEERS` is off → `safe_http::ensure_public_url` (reject private); if on → permit a URL only when it matches `IONE_PRIVATE_PEER_ALLOWLIST` (CIDR/domain), else still `ensure_public_url`. Never a blanket bypass. `auto_create_connector_for_peer` stores only `peer_id` (not the decrypted bearer) in connector config. (C-2, H-4)
- `src/config.rs` — add `IONE_ALLOW_PRIVATE_PEERS: bool` + `IONE_PRIVATE_PEER_ALLOWLIST` (CIDR/domain list).
- `src/routes/peers.rs` — `create_legacy_peer` path runs the same guard (currently bypasses `begin_federation`'s metadata-fetch guard). (C-2)
- `src/connectors/mcp_client.rs` — build the client via **`url_guard::guarded_client(timeout)?`** (the correct module — redirect-blocking + IP guard) instead of `reqwest::Client::new()`. (C-2)
- `src/models/connector.rs` — mark any secret config field `#[serde(skip)]` / add redaction so connector-returning endpoints never serialize a token. (H-4/L-2)
- `Cargo.toml` — add `dashmap`, `governor`.
- `migrations/0033_peer_federation.sql` — **create**: `peers.tool_prefix VARCHAR(16)`, `peers.session_status TEXT NOT NULL DEFAULT 'disconnected'`, `peers.last_connected_at TIMESTAMPTZ`, `peers.last_session_error TEXT`; `CREATE UNIQUE INDEX peers_org_tool_prefix_uniq ON peers(org_id, tool_prefix) WHERE tool_prefix IS NOT NULL`. (H-3, Slice D state)
- `src/state.rs` — add federation fields to `AppState` (empty/default-initialized here).

**Code shapes:**
```rust
// state.rs — AppState additions
pub peer_manifest_cache: Arc<dashmap::DashMap<Uuid, PeerManifest>>,
pub peer_slice_cache:    Arc<dashmap::DashMap<Uuid, SliceEntry>>,
pub peer_sessions:       Arc<PeerSessionRegistry>,   // Slice D; empty registry in Phase 0
pub peer_governor:       Arc<dashmap::DashMap<Uuid, PeerGovernor>>, // Slice E

// migration 0033
ALTER TABLE peers ADD COLUMN tool_prefix VARCHAR(16);
ALTER TABLE peers ADD COLUMN session_status TEXT NOT NULL DEFAULT 'disconnected';
ALTER TABLE peers ADD COLUMN last_connected_at TIMESTAMPTZ;
ALTER TABLE peers ADD COLUMN last_session_error TEXT;
CREATE UNIQUE INDEX peers_org_tool_prefix_uniq ON peers(org_id, tool_prefix) WHERE tool_prefix IS NOT NULL;

// generator.rs / rules.rs — H-1
let approval_required = matches!(severity, Severity::Flagged | Severity::Command);
signal_repo.insert(/* ... */, approval_required).await?;
```
**Gate:** `cargo sqlx migrate run && cargo check && cargo clippy --all-targets -- -D warnings`
**Acceptance:** `cargo test approval_floor -- --nocapture` proves a rule/generator `flagged` signal has `approval_required=true`; `grep -n "reqwest::Client::new" src/connectors/mcp_client.rs` returns nothing; a peer registration against a `169.254.*` mcp_url returns an error in `cargo test peer_ssrf`.

---

### Phase 0b — MCP conformance baseline (Codex finding 1)
**Goal:** IONe's own `/mcp` surface is spec-current so "MCP as the app-integration layer" is honest, before federation features layer on top. Today `src/mcp_server.rs:1` is a "Hand-rolled MCP 2025-03 subset" with a *separate* `/mcp/sse` endpoint.
**Files:**
- `src/mcp_server.rs` — move to **target protocol `2025-11-25`** (Streamable HTTP): single `/mcp` endpoint handling POST (JSON-RPC) and GET (SSE upgrade) by `Accept`; honor `MCP-Protocol-Version` (negotiate at `initialize`, echo on responses), `MCP-Session-Id` (issue at `initialize`, require thereafter), `Last-Event-ID` (resumability), SSE `retry:`; support `DELETE /mcp` session teardown; **`Origin` validation** on the inbound MCP endpoint. Keep `/mcp/sse` as a deprecated alias if any client depends on it.
- `tests/mcp_conformance.rs` — **create**: conformance tests for IONe-as-server (header negotiation, session lifecycle, Origin reject) and IONe-as-client (the manifest/session paths reuse these against the mock peer).
**Code shapes:**
```rust
// negotiate at initialize; store per-session
const MCP_PROTOCOL: &str = "2025-11-25";
// inbound: validate Origin against config.allowed_origins; reject with 403 otherwise
// DELETE /mcp with MCP-Session-Id -> terminate session, 204
```
**Gate:** `cargo test mcp_conformance -- --nocapture`
**Acceptance:** an `initialize` without a later `MCP-Session-Id` is rejected; a cross-origin POST to `/mcp` returns 403; `DELETE /mcp` with a live session returns 204 and subsequent calls on that id fail; responses carry `MCP-Protocol-Version: 2025-11-25`.
**Note:** this is scaffolding-adjacent but a vertical slice (IONe-as-tool end-to-end). If it grows large, it can split from Phase 0 — but it must land before Phase 1, because workspace-scoped `tools/list` (Slice A) rides the session identity established here.

---

### Phase 1 — Slice A: dynamic tool aggregation + namespacing (workspace-scoped)
**Goal:** IONe's `/mcp` `tools/list` returns **the active workspace's** peer tools, namespaced `‹prefix›:‹tool›` (auth-gated, role-filtered); `tools/call` routes a namespaced call to the owning peer; operator can GET a peer's tools.
**Files:**
- `src/connectors/mcp_client.rs` — add `fetch_manifest(peer) -> PeerManifest` (calls `tools/list` + `resources/list`, **paginating via cursor** until exhausted — Codex finding 3); reject tool names containing `:`.
- `src/mcp_server.rs` — `handle_tools_list` filters to peers bound to the **session's active workspace** via `workspace_peer_binding_repo`, then aggregates; a tool is included only if the caller's role permits it (the decided active-workspace scope).
- `src/services/peer.rs` — derive + persist `tool_prefix` at peer creation (slugify name, dedupe `_2`/`_3` against existing prefixes in org); `tool_prefix` immutable on rename.
- `src/services/federation.rs` — **create**: manifest cache population, `aggregate_tools(state) -> Vec<NamespacedTool>` (merge IONe-native + per-peer, reject duplicate prefixes with logged error), `route_tool_call(state, "‹prefix›:‹tool›", args)` → resolve peer by prefix, validate tool in manifest, invoke via mcp_client. **If `ione_approval.required`: do NOT execute — persist a durable pending peer tool-call (Phase 1b) and return `{status:"pending_approval", pending_id}`.**
- `src/mcp_server.rs` — `handle_tools_list` becomes async, returns `aggregate_tools`; **require auth** (same as `tools/call`, M-4); `handle_tools_call` parses prefix and delegates to `route_tool_call`. (May require making sibling handlers async — keep the dispatch table consistent.)
- `src/routes/peers.rs` or `src/routes/workspaces.rs` — `GET /api/v1/workspaces/:id/peers/:peerId/tools`.
- `src/repos/peer_repo.rs` — `set_tool_prefix`; scope `get` by org (M-2) or add explicit org check at the new call site.
- `static/app.js` — peer-browser tools list (shared panel built in Phase 3/F; here just the data call if surfaced early — otherwise defer rendering to F).

**Code shapes:**
```rust
pub struct PeerManifest { pub tools: Vec<ToolDef>, pub resources: Vec<ResourceRef>, pub fetched_at: Instant, pub etag: Option<String> }
pub struct NamespacedTool { pub name: String /* "gp:list_alerts" */, pub description: String,
                            pub input_schema: Option<Value>, pub approval_required: bool, pub peer_id: Uuid }
fn derive_prefix(name: &str, taken: &HashSet<String>) -> String; // slug, then _2/_3
async fn route_tool_call(state: &AppState, namespaced: &str, args: Value) -> Result<Value, AppError>;
```
**Gate:** `cargo test slice_a_aggregation slice_a_routing slice_a_workspace_scope -- --nocapture && cargo clippy --all-targets -- -D warnings`
**Acceptance:** with two mock peers each exposing `list_alerts`, `tools/list` (authed, workspace W) contains `gp:list_alerts` and `ty:list_alerts`, no bare `list_alerts`; a peer bound to workspace X but **not** W does **not** appear in W's `tools/list` (active-workspace scope); unauthed `tools/list` → 401 (M-4); `tools/call gp:list_alerts` hits only peer `gp`; a peer naming itself `gp` when taken persists `gp_2`; a peer returning a paginated `tools/list` (cursor) has all pages aggregated, none truncated.

---

### Phase 1b — Durable pending peer tool-call (Codex finding 4)
**Goal:** an approval-gated, agent-initiated `tools/call` produces a durable record that can execute *after* approval — not a fake "pending" receipt and not a bypass. Existing approvals are artifact-centered (`artifact_kind` enum has no `tool_call`); this adds the missing object.
**Files:**
- `migrations/0034_pending_peer_tool_call.sql` — **create**: `pending_peer_tool_calls` (`id`, `workspace_id`, `peer_id`, `namespaced_tool`, `arguments_ciphertext` (encrypt args at rest — may contain sensitive values), `arguments_digest` (for replay/idempotency), `requested_by`, `status` `pending|approved|rejected|executed|expired`, `expires_at`, `approver_user_id`, `created_at`, `executed_at`, `result_ref`). RLS by org/workspace.
- `src/repos/pending_peer_tool_call_repo.rs` — **create**: insert/get/transition/expire.
- `src/services/federation.rs` — on approval, load the record, **re-validate** (status `approved`, not expired, digest matches), execute the peer `tools/call` exactly once (status→`executed`), write audit.
- `src/routes/approvals.rs` — approve/reject of a pending peer tool-call routes to the federation executor (extends the existing approvals flow rather than the artifact path).
- `src/mcp_server.rs` — the routed `tools/call` returns `{status:"pending_approval", pending_id}` (not a fabricated result).
**Code shapes:**
```rust
pub enum PendingStatus { Pending, Approved, Rejected, Executed, Expired }
// idempotency: unique (workspace_id, arguments_digest) within a TTL window prevents replay
```
**Gate:** `cargo test pending_tool_call -- --nocapture`
**Acceptance:** an approval-gated `tools/call` creates a `pending` row, does NOT hit the peer; on approve, the peer is called exactly once and status→`executed`; a second approve (replay) does not re-execute; an expired record cannot execute; reject → no peer call ever.

---

### Phase 2 — Slice C: protocol-change reception (poll-based freshness)
**Goal:** peer manifest cache stays fresh across the existing poll cycle without a session.
**Files:**
- `src/services/scheduler.rs` — (corrected path) per tick, re-hash each active peer's **paginated** `tools/list`/`resources/list`; on change, replace the `peer_manifest_cache` entry.
- `src/services/federation.rs` — `refresh_manifest_if_changed(state, peer_id)` (hash compare + swap). Slice D later calls the same invalidation on `list_changed`.
**Code shapes:**
```rust
async fn refresh_manifest_if_changed(state: &AppState, peer_id: Uuid) -> Result<bool, AppError>; // true if swapped
```
**Gate:** `cargo test slice_c_refresh -- --nocapture`
**Acceptance:** a mock peer whose tool set changes between two scheduler ticks → after the second tick, `GET /workspaces/:id/peers/:pid/tools` reflects the new set.

---

### Phase 3 — Slice F: resource & tool browsing (operator UI) + manifest cache durability (Codex finding 7)
**Goal:** an operator can see, per bound peer, its tools (namespaced + approval flag) and resources (with `ione_view`); and the manifest cache behaves like a contract surface, not a cold-start void.
**Manifest durability (finding 7):** the in-memory cache gets (1) **startup hydration** (fetch active peers' manifests on boot), (2) **read-through** on miss, (3) **stale markers** (`fetched_at` + `stale:true` past TTL in API responses), and (4) **last-known-good persistence** — a `peers.last_manifest_jsonb` column (migration 0034, alongside the pending-call table) so a restart or a peer being down serves the last good manifest rather than empty. `/mcp tools/list` is documented as **eventually-consistent** (may be briefly stale; never silently empty for a known peer).
**Files:**
- `src/routes/workspaces.rs` — `GET /api/v1/workspaces/:id/peers/:peerId/resources` (from cache; `stale` flag if older than TTL); `POST /api/v1/peers/:id/manifest/refresh` (force).
- `migrations/0034_pending_peer_tool_call.sql` — add `peers.last_manifest_jsonb JSONB` (same migration as Phase 1b).
- `src/main.rs` — startup hydration of the manifest cache.
- `src/services/federation.rs` — read-through helpers for the above.
- `static/app.js` + `static/index.html` — a "Peers" browser panel: per peer, tools list + resources list (+ session badge placeholder, wired in D).
- `static/style.css` — panel styles.
**Gate:** `cargo test slice_f_browse -- --nocapture && (cd . && npx tsc --noEmit 2>/dev/null || true)`
**Acceptance:** `GET /workspaces/:id/peers/gp/resources` → 200 with each resource's `ioneView` (or null); the Peers panel renders gp's tools+resources in a browser smoke test.

---

### Phase 4 — Slice B: context-slice ingestion + lazy expansion (+ H-2 controls)
**Goal:** chat context routes on per-peer `slice://` summaries (not full tool defs); full `inputSchema` fetched lazily on tool selection; peer slice text cannot inject prompt instructions.
**Files:**
- `src/connectors/mcp_client.rs` — `fetch_slice(peer) -> SliceEntry` (`resources/read slice://`); synthesize a minimal slice (`schema_version:"0"`) from the **paginated `tools/list`** if the peer omits `slice://`. **Lazy `inputSchema` expansion is an IONe extension:** try the slice's `expand_uri`/`tools/get`; **fall back to the `inputSchema` already present in the peer's `tools/list`** for standard peers (Codex finding 3). Never assume `tools/get` exists.
- `src/services/federation.rs` — slice cache population + invalidation on `resources/list_changed`.
- `src/services/chat.rs` (or wherever chat context is assembled) — inject aggregated slices inside escape-proof sentinel-delimited block; length-bound each <2 KB; strip delimiter-breaking chars (H-2 #1/#2).
- `src/services/router.rs` — **validate post-LLM tool selection against the operator tool allowlist** before routing/invoking (H-2 #3). No auto-invocation from a slice.
- `src/routes/workspaces.rs` — `GET /api/v1/workspaces/:id/context-slices`.
**Code shapes:**
```rust
pub struct SliceEntry { pub peer_id: Uuid, pub body: Value /* summary, domain_tags, sample_queries, tool_index */, pub fetched_at: Instant }
// chat context injection (sentinel block — peer text is DATA, never instructions)
// <<<IONE_PEER_SLICE id=gp untrusted>>> ... summary + tool_index names ... <<<END_IONE_PEER_SLICE>>>
async fn expand_tool_schema(state: &AppState, namespaced: &str) -> Result<Value, AppError>; // lazy, cached
```
**Gate:** `cargo test slice_b_slices slice_b_lazy slice_b_injection -- --nocapture`
**Acceptance:** with two slice-publishing peers, assembled chat prompt contains both summaries + tool_index names and **no** full `inputSchema`, each slice <2 KB; selecting `gp:generate_report` fetches exactly one schema; a slice whose `summary` contains fake system instructions cannot cause routing to a non-allowlisted tool or any auto-invoke.

---

### Phase 5 — Slice D: per-peer notification session → signals (C-1)
**Goal:** IONe holds a long-lived streamable-HTTP session per active peer, receives `notifications/*`, routes domain events through `ingest_webhook_event` (tenant resolved from IONe's binding, not the payload), and protocol notifications invalidate caches.
**Files:**
- `src/connectors/peer_session.rs` — **create**: `PeerSessionRegistry` (`DashMap<Uuid, PeerSessionHandle>`), one Tokio task per active peer; `initialize` to acquire `mcp-session-id`; long-lived SSE GET via `safe_http::guarded_client()`; reconnect with exp backoff (ceiling 5 min + jitter, M-1); idle-timeout staleness detection (`IONE_PEER_SSE_IDLE_SECS` default 90); OAuth refresh on reconnect **under a per-peer refresh mutex shared with the scheduler poll path** (the token-overwrite race).
- `src/services/federation.rs` — `dispatch_notification(state, peer_id, notif)`: protocol (`tools/list_changed`/`resources/list_changed`/`resources/updated`) → cache invalidation; domain → build `WebhookEnvelope` (with `foreign_tenant_id` **looked up from `workspace_peer_bindings` for the session's authenticated `peer_id`**, C-1) → `ingest_webhook_event`. `peer_id` from session, never message body.
- `src/repos/peer_repo.rs` — `set_session_status`, `set_last_connected_at`, `set_last_session_error`.
- `src/routes/peers.rs` — `GET /api/v1/peers/:id/session`, `POST /api/v1/peers/:id/session/reconnect`.
- `src/main.rs` / startup — spawn the registry; reconcile sessions on peer status changes.
- `src/services/webhook_ingress.rs` — add an audit row at notification→signal ingestion (`source: peer_notification`, L-3). (Small, additive.)
- `static/app.js` — peer-browser session badge (live/connecting/error + reason).
**Code shapes:**
```rust
pub struct PeerSessionRegistry { tasks: DashMap<Uuid, PeerSessionHandle>, sem: Arc<Semaphore> /* min(peers,20) */ }
pub struct PeerSessionHandle { state_tx: watch::Sender<SessionState>, abort: AbortHandle }
pub enum SessionState { Disconnected, Connecting, Live, Error(String) }
async fn dispatch_notification(state: &AppState, peer_id: Uuid, n: JsonRpcNotification);
// domain path: foreign_tenant_id = binding_repo.canonical_tenant(peer_id, workspace)  // NOT n.params
```
**Gate:** `cargo test slice_d_session slice_d_notify slice_d_c1 -- --nocapture --test-threads=1`
**Acceptance:** mock peer with a live session + binding for `tenant-A` emits `{id,severity:flagged,foreign_tenant_id:tenant-A}` → one signal with `approval_required=true` + an audit row; same `id` twice → one signal; a notification claiming `tenant-B` (no binding) → no signal; session drop → `session_status error→connecting→live` with a refreshed token.

---

### Phase 6 — Slice E: rate limiting + circuit breakers
**Goal:** a misbehaving peer is bounded — notifications/calls rate-limited per peer; a breaker opens after consecutive peer-side failures, stops the reconnect storm, and surfaces as `session_status=error` with one audit + one operator signal.
**Files:**
- `src/services/federation.rs` (or `src/services/peer_governor.rs` — **create**) — per-peer `governor` token bucket (`IONE_PEER_CALL_RPS` default 10, burst 20) + circuit breaker (open after 5 consecutive **peer-side** failures/timeouts; 4xx does NOT trip — M-1/security note; half-open probe after 30s).
- `src/services/peer_tokens.rs` — apply the governor + breaker at `send_mcp_request` (the single outbound chokepoint): acquire token before send, record success/failure after.
- `src/connectors/peer_session.rs` — inbound notification rate limit (shed protocol notifications past the per-peer queue cap; never drop domain events); breaker trip marks peer `error`, writes audit + one signal.
- `src/routes/peers.rs` — extend `GET /peers/:id/session` response with `breaker` state + `reason`.
- `static/app.js` — breaker-open badge reason.
**Code shapes:**
```rust
pub struct PeerGovernor { rl: governor::RateLimiter<...>, breaker: Breaker }
pub struct Breaker { state: BreakerState, consecutive_failures: u32, opened_at: Option<Instant> }
pub enum BreakerState { Closed, Open, HalfOpen }
// only peer-side failures (5xx, timeout, connect-refused, parse) increment; 4xx logged, not counted
```
**Gate:** `cargo test slice_e_ratelimit slice_e_breaker -- --nocapture`
**Acceptance:** peer over the rate shed excess (metric increments, memory bounded); peer failing 5× consecutively → `session_status=error`, reconnect paused until cooldown, exactly one audit + one signal, half-open probe after cooldown; a peer returning 4xx does NOT trip the breaker.

---

## Task Manifest

Routing: `claude-code` where existing callers/structure matter (mcp_server, mcp_client, scheduler, chat, router, peer services). `codex` for new self-contained modules, the migration, and test fixtures (the mock-peer harness). Parallel only when `Files` are disjoint.

| Task | Agent | Files | Depends On | Gate |
|------|-------|-------|------------|------|
| T0a: H-1 approval floor | claude-code | `src/services/generator.rs`, `src/services/rules.rs` | — | `cargo test approval_floor` |
| T0b: migration + deps + AppState scaffold | codex | `migrations/0033_peer_federation.sql`, `Cargo.toml`, `src/state.rs` | — | `cargo sqlx migrate run && cargo check` |
| T0c: SSRF + token-in-config fixes | claude-code | `src/services/peer.rs`, `src/routes/peers.rs`, `src/connectors/mcp_client.rs`, `src/models/connector.rs` | T0b | `cargo test peer_ssrf` |
| T1: Slice A aggregation+namespacing+routing | claude-code | `src/services/federation.rs` (new), `src/mcp_server.rs`, `src/repos/peer_repo.rs`, `src/routes/workspaces.rs` | T0a,T0b,T0c | `cargo test slice_a_aggregation slice_a_routing` |
| T1f: mock-peer test harness | codex | `tests/support/mock_peer.rs` (new), `tests/mcp_federation.rs` (new) | T0b | `cargo test --no-run` |
| T2: Slice C poll freshness | claude-code | `src/connectors/scheduler.rs`, `src/services/federation.rs` | T1 | `cargo test slice_c_refresh` |
| T3: Slice F browse + UI panel | claude-code | `src/routes/workspaces.rs`, `static/app.js`, `static/index.html`, `static/style.css` | T1 | `cargo test slice_f_browse` |
| T4: Slice B slices+lazy+injection | claude-code | `src/connectors/mcp_client.rs`, `src/services/chat.rs`, `src/services/router.rs`, `src/services/federation.rs`, `src/routes/workspaces.rs` | T1 | `cargo test slice_b_slices slice_b_injection` |
| T5: Slice D session manager + notify | claude-code | `src/connectors/peer_session.rs` (new), `src/services/federation.rs`, `src/repos/peer_repo.rs`, `src/routes/peers.rs`, `src/main.rs`, `src/services/webhook_ingress.rs`, `static/app.js` | T1,T2 | `cargo test slice_d_session slice_d_c1 -- --test-threads=1` |
| T6: Slice E rate limit + breaker | claude-code | `src/services/peer_governor.rs` (new), `src/services/peer_tokens.rs`, `src/connectors/peer_session.rs`, `src/routes/peers.rs`, `static/app.js` | T5 | `cargo test slice_e_breaker` |

**Parallelizable:** T0a ∥ T0b (disjoint). T1f ∥ T1 (harness is new files; but T1's tests consume it — land T1f first or together). T2 ∥ T3 ∥ T4 after T1 *only if* `federation.rs` edits are coordinated — they all touch it, so **sequence T2→T4→T3** through `federation.rs`, or split `federation.rs` into submodules first. T4 and T3 both touch `static/app.js` → sequence. T5 and T6 are sequential (T6 rides T5's session path).

> **Note on `src/services/federation.rs` contention:** T1–T6 all add to this new module. To avoid serializing everything through one file, create it in T1 with clear submodule seams (`aggregate`, `manifest`, `slices`, `notify`, `governor`) so later tasks append disjoint sections. If parallel agents are used, give each its own submodule file under `src/services/federation/` instead.

---

## Self-review

1. **Every design acceptance criterion maps to a phase gate?** Yes — Slice A→Phase 1, B→4, C→2, D→5, E→6, F→3; security gates C-1→Phase5, C-2→Phase0, H-1→Phase0, H-2→Phase4, H-3→Phase1, M-4→Phase1.
2. **Every file exists or is listed as create?** Yes — all cited existing files verified present this session; new files marked **create** (`migrations/0033`, `src/services/federation.rs`, `src/connectors/peer_session.rs`, `src/services/peer_governor.rs`, test harness).
3. **Vertical slices, not layer stacks?** Yes — each phase ships db+api+ui for one capability; Phase 0 is the allowed scaffolding+correctness exception (≤10 files).
4. **Gates are concrete shell commands?** Yes — named `cargo test` targets per phase.
5. **Parallel tasks disjoint?** Flagged the `federation.rs` and `static/app.js` contention explicitly with a sequencing/submodule remedy.

## Blocking decisions / open items

- **Private/on-prem peer URLs (Codex finding 6 — RESOLVED, owner, 2026-05-31): private allowed via explicit flag.** Default is public-only (`safe_http::ensure_public_url`); a config gate `IONE_ALLOW_PRIVATE_PEERS` plus a CIDR/domain allowlist permits private peer URLs for on-prem/VPN deployments. The SSRF guard stays on for everything not explicitly allowlisted — private access is opt-in per deployment, never a blanket bypass. **Phase 0 impact:** `validate_mcp_url` branches on the flag (public-only guard vs allowlist-checked private); `url_guard::guarded_client` is still used for the transport in both cases (it already permits private HTTPS); the allowlist is enforced at `validate_mcp_url`, not left to the client helper. Add `IONE_ALLOW_PRIVATE_PEERS` + `IONE_PRIVATE_PEER_ALLOWLIST` to `src/config.rs`.
- **Name the first MCP-notification-emitting peer — HARD GATE on Phase 5** (Slice D), not an open item (Codex finding 8). Phase 5 does not start until a peer is named and emitting against a real stream; it locks the transport detail and the C-1 `foreign_tenant_id`/`whoami` resolution against a real peer. Phases 0–4 + 1b do not need it.
- **`slice://` as published contract:** it is already in `app-integration-playbook.md` as a required app surface — treat it as a pushed app-builder contract, with the lazy-expansion *consumption* labeled an IONe extension (finding 3). Confirm with owner if it should stay required vs. recommended.
- Lower-severity security items to fold in as touched: M-2 (`PeerRepo::get` org scope — Phase 1), M-3 (`IONE_WEBHOOK_SECRET_KEY` dev fallback), M-5 (NULL `token_expires_at` → default TTL at store time — Phase 5), L-1 (per-instance rate limit semantics — document), L-4 (DCR POST uses unguarded `state.http` — Phase 0 if cheap).

## Migration numbering note

Two migrations now: **0033** (peer federation columns: `tool_prefix`, `session_status`, `last_connected_at`, `last_session_error`, unique index) in Phase 0, and **0034** (`pending_peer_tool_calls` table + `peers.last_manifest_jsonb`) spanning Phase 1b/3. Keep them separate so Phase 0 can land independently.
