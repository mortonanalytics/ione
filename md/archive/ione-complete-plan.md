# IONe Complete — Implementation Plan

**Design:** [md/design/ione-complete.md](../design/ione-complete.md)
**Contract:** [md/design/ione-complete-contract.md](../design/ione-complete-contract.md)
**Layers:** db, api, ui

Ten slices in dependency order. No version labels. Each slice independently shippable. Each slice complete only when failure-UX (§6) and a11y (§10) checks pass for its new surfaces.

## Dependencies

### Cargo.toml additions
```toml
base64 = "0.22"                                                  # OAuth PKCE + CIMD base64url (Slice 8)
sha2 = "0.10"                                                    # token hashing (pin explicit)
oauth2 = "5"                                                     # OAuth client for peer federation (Slice 9)
phf = { version = "0.12", features = ["macros"] }                # canned-chat static map (Slice 1)
tokio-stream = { version = "0.1", features = ["sync"] }          # SSE (Slice 4)
async-stream = "0.3"                                             # SSE (Slice 4)
```

### Env vars
- `IONE_SEED_DEMO` — default `0` prod, `1` in `.env.example` and docker-compose
- `IONE_OAUTH_ISSUER` — absolute URL, defaults derived from bind
- `IONE_OAUTH_STATIC_BEARER` — unset by default; CI/headless fallback
- `IONE_ADMIN_FUNNEL` — unset by default; gates `/admin/funnel`

### No npm deps
Vanilla JS stays vanilla JS.

---

## Slice 1 — Demo Workspace

**Files**
- `migrations/0011_activation_demo_helpers.sql` — **no demo-specific schema**; this migration adds `pipeline_events` helpers used later. Actual demo data lives in the runtime seeder. (Separate migrations defined per slice below; numbered as they land in sequence.)
- New `src/demo/mod.rs` — `DEMO_WORKSPACE_ID` constant.
- New `src/demo/fixture.rs` — typed fixture builders (4 connectors + streams + events + signals + survivors + routing + artifacts + approvals + audit + roles + 1 conversation).
- New `src/demo/seeder.rs` — `seed_demo_if_enabled(pool)`, `purge_demo(pool)`, re-entrant, tx-wrapped, env-gated.
- New `src/demo/canned_chat.rs` — `phf::phf_map!` of 4 normalized prompts → canned responses + stock fallback.
- New `src/middleware/demo_guard.rs` — 403 write-guard on any path that resolves to DEMO_WORKSPACE_ID.
- `src/main.rs` — call seeder after migrations; add `demo-purge` subcommand.
- `src/routes/mod.rs` — install `demo_guard` as a route layer on the protected router (after auth).
- `src/routes/conversations.rs` — `post_message` branches on demo workspace → canned path, writes `messages` with `model = Some("canned")`.
- `static/index.html` — `<div id="chat-chips" hidden>` with 4 buttons; lock-glyph span on workspace switcher.
- `static/app.js` — render chips when active workspace is demo; prefix connector names with `Sample — `; 403 toast handler keyed on `error: "demo_read_only"`.
- `static/style.css` — `.chip` 2×2 grid, focus ring, lock glyph.
- `.env.example` + `docker-compose.yml` — `IONE_SEED_DEMO=1`.
- `Cargo.toml` — add `phf`.

**Tests** `tests/phase15_demo_workspace.rs`
- `seed_is_reentrant`
- `demo_blocks_writes_with_demo_read_only_error`
- `canned_chat_bypasses_ollama` (mock Ollama fails if called)
- `canned_unmatched_returns_stock_reply`
- `demo_purge_removes_workspace_and_audit_events`

**Exit**
```
cargo test --test phase15_demo_workspace -- --test-threads=1
cargo clippy --all-targets -- -D warnings
curl -sf -X POST localhost:3000/api/v1/workspaces/00000000-0000-0000-0000-000000000d30/connectors \
  -H 'content-type: application/json' -d '{}' | jq -e '.error == "demo_read_only"'
```

---

## Slice 2 — Ollama preflight + chat remediation

**Files**
- New `src/routes/health.rs` extension — `health_ollama` handler.
- `src/services/ollama.rs` — add `list_models() -> Result<Vec<String>, AppError>` calling `/api/tags` with 3s timeout.
- `src/error.rs` — two new variants: `OllamaUnreachable { base_url, error }`, `OllamaModelMissing { model, pull_command }`. Both serialize to the error envelope with `hint` and `pullCommand`.
- `src/routes/conversations.rs` — translate Ollama errors in `post_message` into the new variants (503 status); demo path unchanged.
- `src/routes/mod.rs` — register `/api/v1/health/ollama` (public).
- `static/index.html` — `<button id="health-dot">` in top bar; hidden `<div id="health-panel">`.
- `static/app.js` — `pollHealth()` every 15s when tab active; `renderHealth()`; disable chat textarea in real workspace when health red; inline failure card on per-request Ollama failure with `pullCommand` + retry.
- `static/style.css` — dot green/red states; panel layout.

**Tests** `tests/phase16_ollama_health.rs`
- `health_returns_ok_when_ollama_up_and_model_present`
- `health_returns_model_missing_when_model_not_pulled`
- `health_returns_unreachable_when_ollama_down` (mock server)
- `chat_returns_503_with_remediation_on_ollama_down`

**Exit**
```
cargo test --test phase16_ollama_health -- --test-threads=1
curl -sf localhost:3000/api/v1/health/ollama | jq '.ok, .models.required'
```

---

## Slice 3 — Guided Connector Setup

**Files**
- New `src/routes/connectors.rs` — add `validate_connector` handler dispatching to per-kind dry-run.
- New `src/connectors/validate/mod.rs` — trait `Validator { async fn validate(config: &Value) -> Result<ValidateOk, ValidateErr>; }`.
- New `src/connectors/validate/{nws,firms,s3,slack,irwin,openapi}.rs` — one module per kind.
- `src/routes/connectors.rs` — `create_connector` now calls validate first; returns 422 with envelope on failure; unchanged success path chains into Slice 4 emission.
- `src/routes/mod.rs` — register `POST /api/v1/connectors/validate`.
- `static/index.html` — replace Add Connector modal content with a two-step form (step 1 provider grid, step 2 provider-specific inputs). Add `<dialog id="ac-dialog-v2">` retaining the old id for now.
- `static/app.js` — provider form renderers (`renderNwsForm`, `renderFirmsForm`, etc.), `testConnection()` helper, create button state gate.
- `static/style.css` — provider grid tiles; form layout; success/error cards next to Test button.
- `Cargo.toml` — no new deps (reqwest + serde_json already present).

**Tests** `tests/phase17_connector_validate.rs`
- `validate_nws_rejects_out_of_range_lat`
- `validate_firms_rejects_bad_key` (mock upstream returns 401)
- `validate_s3_rejects_access_denied` (mock)
- `validate_slack_passes_with_valid_webhook_url` (mock)
- `create_connector_422s_when_validate_fails`
- `validate_error_shape_has_hint_and_field`

**Exit**
```
cargo test --test phase17_connector_validate -- --test-threads=1
# Manual: open Add Connector modal, each provider's Test button returns ok or a specific hint.
```

---

## Slice 4 — Publish-Don't-Poll

**Files**
- `migrations/0012_pipeline_events.sql` — table + indexes per design §4.
- New `src/models/pipeline_event.rs` — `PipelineEvent`, `PipelineEventStage` enum (sqlx Type).
- New `src/repos/pipeline_event_repo.rs` — `append`, `list` with cursor.
- New `src/services/pipeline_bus.rs` — `tokio::sync::broadcast` wrapper; `publish(event)`, `subscribe() -> Receiver`. Channel capacity 256.
- `src/state.rs` — add `pub pipeline_bus: Arc<PipelineBus>`.
- `src/main.rs` — construct bus, inject.
- `src/services/scheduler.rs` — append + publish at every stage boundary:
  - pre-poll: `publish_started`
  - first stream event of run: `first_event`
  - signal inserted: `first_signal`
  - survivor inserted: `first_survivor`
  - routing decision inserted: `first_decision`
  - any stage error: `error` with structured detail
  - watchdog (new): if no progress for >10s since `publish_started`, emit `stall` with `detail.waiting_on`
- `src/routes/connectors.rs` `create_connector` — synchronously runs one poll + first-event emission inline before returning.
- New `src/routes/pipeline_events.rs` — `list`, `stream` (SSE via `Sse<Stream<Event>>`).
- `src/routes/mod.rs` — register `/api/v1/workspaces/:id/events` and `/events/stream`.
- `src/demo/fixture.rs` — seed one full stage sequence per demo connector.
- `static/index.html` — connector-card template gains `<div class="conn-timeline">`; Add Connector modal gets `<div id="ac-progress">` for post-submit progress view.
- `static/app.js` — per-card `renderTimeline()`; single `EventSource` for the Connectors tab, filtered per card; progress view subscribes on create; reconnect UI with `Reconnecting…` + backoff; timeline fills from `/events` on reconnect for missed events.
- `static/style.css` — timeline row, icons, reconnect indicator.
- `Cargo.toml` — add `tokio-stream`, `async-stream`.

**Tests** `tests/phase18_pipeline_events.rs`
- `create_connector_emits_publish_started_synchronously`
- `sse_delivers_new_event_under_500ms`
- `error_stage_includes_upstream_status`
- `stall_stage_fires_after_watchdog_window`
- `demo_connectors_have_full_stage_timeline`
- `sse_reconnect_replays_missed_events_from_list_endpoint`

**Exit**
```
cargo test --test phase18_pipeline_events -- --test-threads=1
curl -sf -N -H 'accept: text/event-stream' \
  localhost:3000/api/v1/workspaces/<demo>/events/stream | head -c 200
```

---

## Slice 5 — Split Activation

**Files**
- `migrations/0013_activation.sql` — `activation_progress` and `activation_dismissals` per contract.
- New `src/models/activation.rs` — `ActivationProgress`, `ActivationTrack`, `ActivationStepKey` enums.
- New `src/repos/activation_repo.rs` — `list`, `mark`, `dismissed`, `dismiss`. `mark` uses `ON CONFLICT DO NOTHING`.
- New `src/routes/activation.rs` — three handlers.
- `src/routes/mod.rs` — register.
- `src/routes/conversations.rs` `post_message` — on success, mark `asked_demo_question` (demo) or first-connector-prerequisite-unrelated. (Demo chat triggers demo track only.)
- `src/routes/approvals.rs` `decide_approval` — mark `first_approval_decided` (real track only; demo already blocked by write-guard).
- `src/services/scheduler.rs` — on `pipeline_events.stage = 'first_signal'` for a workspace with no prior mark, write `first_signal` step.
- `static/index.html` — `<div id="activation-tracker">` between workspace switcher and conversation list; renders demo or real tracker based on active workspace; CTA card template.
- `static/app.js` — `renderActivationTracker()`, track detection, CTA `Create your workspace` → `POST /workspaces` + switch; fire step events on UI actions that aren't server-marked (survivor-opened, audit-viewed).
- `static/style.css` — tracker card; CTA variant.

**Tests** `tests/phase19_activation.rs`
- `demo_track_completion_surfaces_cta_not_full_activation`
- `real_track_first_signal_fires_on_pipeline_event`
- `mark_is_idempotent`
- `dismiss_one_track_does_not_affect_other`
- `cta_create_workspace_happy_path_integration`

**Exit**
```
cargo test --test phase19_activation -- --test-threads=1
```

---

## Slice 6 — Failure UX pass

**Scope:** apply the Catalog table from design §6 to every prior and future slice. This is not a separate set of files — it's a shipping gate.

**Files (cross-cutting)**
- `src/error.rs` — enforce envelope `{ error, message, hint?, field? }` in `IntoResponse`; add a unit test that every `AppError` variant produces a non-empty `error` code and message.
- `src/routes/*.rs` — audit every error path; add `hint` where user action would resolve. Grep: `AppError::Internal` cannot be returned for 4xx reachable paths without a human-readable `message`.
- `static/app.js` — one shared `showError(kind, message, hint?, onRetry?)`. Every `fetch` wraps via `apiFetch` (exists); extend `apiFetch` to auto-route JSON errors through `showError`. No inline `alert(...)` or `setStatus('Error: ' + ...)`.
- `static/style.css` — toast variants: error (red), info (blue), retry button.

**Tests** `tests/phase20_failure_ux.rs`
- `error_envelope_shape_is_consistent_across_routes`
- `apifetch_ui_unit_test_routes_errors_to_showerror` (JS unit via Node `node --test`; add `scripts/test-js.sh`)

**Exit**
```
cargo test --test phase20_failure_ux -- --test-threads=1
bash scripts/test-js.sh
# Manual: walk the Catalog rows, confirm each shows the specified copy and retry path.
```

---

## Slice 7 — Funnel Telemetry

**Files**
- `migrations/0014_funnel_events.sql` — table + indexes per contract.
- New `src/models/funnel_event.rs` — `FunnelEvent` struct; `event_kind` is `TEXT` not enum (catalog-open).
- New `src/repos/funnel_event_repo.rs` — `append` (spawn-fire-and-forget helper), `counts(from, to)`.
- New `src/services/funnel.rs` — `track(state, ctx, kind, detail)` helper; non-blocking.
- New `src/routes/telemetry.rs` — `POST /telemetry/events`, `GET /admin/funnel` (gated on `IONE_ADMIN_FUNNEL`).
- `src/routes/mod.rs` — register.
- Session cookie: new middleware `src/middleware/session_cookie.rs` — issues `ione_session` cookie (uuid v4) on first request if missing; attaches to request extensions; read by `track()`.
- Server-side emission — add `track()` calls at: `create_connector` (validate_attempted, succeeded/failed, created), scheduler (`first_real_signal`), approvals (`first_real_approval_decided`), activation completion, `post_message` Ollama failure (`ollama_unreachable_seen`).
- Client-side emission — `track(kind, detail?)` helper in `app.js`; call at demo chip click, CTA shown/clicked, MCP tile click (Slice 8), peer federation start/activate (Slice 9).
- `src/routes/workspaces.rs` — track `real_workspace_created` on create.

**Tests** `tests/phase21_telemetry.rs`
- `connector_created_emits_single_funnel_event`
- `validate_failed_emits_event_with_error_kind`
- `admin_funnel_404_unless_env_set`
- `admin_funnel_returns_counts`
- `session_cookie_issued_on_first_request_and_persists`

**Exit**
```
cargo test --test phase21_telemetry -- --test-threads=1
IONE_ADMIN_FUNNEL=1 curl -sf localhost:3000/api/v1/admin/funnel | jq '.counts'
```

---

## Slice 8 — MCP OAuth + Front Door (with fallback path)

**Primary path (OAuth 2.1). Gate: Claude Desktop Pro round-trip must pass against a minimal stub before starting Slice 8 UI work.**

**Files (primary path)**
- `migrations/0015_oauth.sql` — `oauth_clients`, `oauth_auth_codes`, `oauth_access_tokens`, `oauth_refresh_tokens` per contract.
- New `src/models/oauth.rs` — structs + `ClientMetadata` (CIMD document).
- New `src/repos/oauth_client_repo.rs`, `src/repos/oauth_token_repo.rs`.
- New `src/routes/oauth.rs` — `discovery`, `register`, `authorize`, `token`, `revoke`. Discovery fields verified against Linear + Sentry on 2026-04-23:
  ```
  { issuer, authorization_endpoint, token_endpoint, registration_endpoint,
    response_types_supported: ["code"], response_modes_supported: ["query"],
    grant_types_supported: ["authorization_code","refresh_token"],
    token_endpoint_auth_methods_supported: ["client_secret_basic","client_secret_post","none"],
    code_challenge_methods_supported: ["S256"],
    revocation_endpoint, client_id_metadata_document_supported: true }
  ```
- New `src/middleware/mcp_bearer.rs` — extract Bearer, verify against `oauth_access_tokens`, inject `OauthContext` extension; 401 + `WWW-Authenticate: Bearer realm="ione", resource_metadata="..."` on missing/invalid. Honors `IONE_OAUTH_STATIC_BEARER` env var.
- `src/mcp_server.rs` — use `OauthContext` for per-call auth.
- New `src/routes/oauth_consent.rs` — minimal HTML consent page; POST back with `action=allow|deny`.
- `src/config.rs` — `oauth_issuer: String`.
- `src/routes/mod.rs` — register OAuth (public) and apply bearer layer to `/mcp/*`.
- New `src/routes/mcp_clients.rs` — `GET /api/v1/mcp/clients`, `DELETE /api/v1/mcp/clients/:id`.
- `static/index.html` — replace sidebar MCP widget with a `Connect to MCP` link opening `#/mcp-connect` panel.
- `static/app.js` — tile grid with Cursor deep link (`cursor://anysphere.cursor-deeplink/mcp/install?name=ione&config=<base64>`), Claude Desktop paste-URL + 3-step instructions, Claude Code CLI snippet, VS Code deep link, Other (raw JSON). Connected-clients table + 15s polling + revoke.
- `static/style.css` — tile grid; clients table.
- `Cargo.toml` — add `base64`, pin `sha2`.

**Files (fallback path, if round-trip fails)**
- Keep `oauth_access_tokens` migration (we still hash tokens).
- Omit OAuth flow routes. `/mcp/*` bearer middleware still honors `IONE_OAUTH_STATIC_BEARER` plus per-user tokens issued via a new `POST /api/v1/mcp/tokens` handler (session-auth'd, returns an opaque bearer stored as sha256 hash).
- UI page renamed `MCP access`; shows URL + bearer copy button + paragraph of copy: `IONe doesn't yet implement OAuth for Claude Desktop Pro's custom connector. Use Claude Code for the paved path: claude mcp add --transport http ione <url> --header 'Authorization: Bearer <token>'.`
- README update: accurate limitation text.

**Tests** `tests/phase22_mcp_oauth.rs`
- `discovery_fields_match_contract`
- `register_cimd_url_stores_metadata`
- `authorize_requires_session_and_issues_code`
- `token_exchange_with_valid_pkce_returns_tokens`
- `token_exchange_wrong_verifier_rejected`
- `refresh_rotates_and_revokes_old`
- `mcp_401_with_www_authenticate_without_bearer`
- `mcp_200_with_valid_bearer`
- `revoke_invalidates_subsequent_calls`

**Manual gate**
- Add IONe as Claude Desktop Pro custom connector. `tools/list` returns 200. Record outcome; if fail → fallback path.

**Exit**
```
cargo test --test phase22_mcp_oauth -- --test-threads=1
curl -sf localhost:3000/.well-known/oauth-authorization-server | jq .
curl -sI localhost:3000/mcp/tools/list | head -n1 | grep 401
# Manual: Claude Desktop Pro round-trip passes.
```

---

## Slice 9 — Peer Handshake UI

**Files**
- `migrations/0016_peers_oauth.sql` — alter `peers` per contract.
- `src/models/peer.rs` — add `PeerStatus` enum + new columns.
- `src/repos/peer_repo.rs` — `begin_oauth`, `set_tokens`, `set_allowlist`, `set_status`, `get_tool_allowlist`.
- New `src/services/peer_oauth.rs` — client-side OAuth against a peer. Uses `oauth2 = "5"`.
- New `src/routes/peer_federation.rs` — modified `POST /peers`, `GET /peers/:id/callback`, `GET /peers/:id/manifest`, `POST /peers/:id/authorize`.
- New `src/routes/well_known.rs` — serve `/.well-known/mcp-client` with IONe's CIMD client.json.
- `src/services/scheduler.rs` — peer-send path blocks any tool not in `peers.tool_allowlist`; writes `audit_events` with `kind: "peer_tool_blocked"`.
- `src/routes/peers.rs` — legacy `create_peer` retained under `{ legacy: true }` opt-in for one release; default path uses federation flow.
- `static/index.html` — Federation panel; wizard modal (3 steps).
- `static/app.js` — wizard flow: POST /peers → open authorize URL → poll /manifest → checkbox list → POST /authorize. Failure-UX catalog entries for unreachable / manifest timeout.
- `Cargo.toml` — add `oauth2`.

**Fallback variant (if Slice 8 chose fallback):** wizard becomes 2 steps (URL + bearer → allow-list). Backend uses static bearer instead of OAuth client.

**Tests** `tests/phase23_peer_federation.rs`
- `begin_federation_returns_pending_oauth_and_authorize_url` (mock peer `.well-known`)
- `callback_fetches_manifest_and_transitions_to_pending_allowlist`
- `authorize_with_allowlist_sets_active`
- `router_blocks_tool_not_in_allowlist_and_audits`
- `revoke_sets_status_revoked_without_deleting_row`
- `manifest_timeout_returns_manifest_timeout_envelope`

**Exit**
```
cargo test --test phase23_peer_federation -- --test-threads=1
```

---

## Slice 10 — A11y / responsive sweep (gate)

**Not new code. A run-list applied to every slice's new components.**

**Files**
- New `scripts/a11y-check.sh` — runs `axe-core` CLI against each route served by `cargo run` with demo data. Fails on serious/critical violations.
- New `scripts/viewport-check.md` — manual run-list for 375/768/1024/1440 viewports listing specific assertions per component.
- `static/style.css` — add `@media (hover: hover)` guards for any hover-only styling; `@media (prefers-reduced-motion: reduce)` for toast/timeline transitions.
- `static/app.js` — verify every new interactive element has `aria-label`, `role`, and focus management (modals, wizards, toasts).

**Exit (gate)**
```
bash scripts/a11y-check.sh                          # zero serious/critical axe violations
# Manual: complete scripts/viewport-check.md at 375/768/1024/1440px; no FAILs outstanding.
```

---

## Wiring verification (runs between slices and at release)

```
# Demo
grep -rn "DEMO_WORKSPACE_ID" src/                     # ≥ 4 matches
grep -rn "demo_read_only" src/ static/                # ≥ 2

# Ollama preflight
grep -rn "ollama_unreachable\|ollama_model_missing" src/ static/  # ≥ 4

# Connector validate
grep -rn "/api/v1/connectors/validate" src/ static/   # ≥ 2

# Pipeline events
grep -rn "PipelineBus\|pipeline_event" src/            # ≥ 4
grep -rn "/events/stream" src/ static/                 # ≥ 2

# Activation
grep -rn "activation_track\|activation_progress" src/  # ≥ 3
grep -rn "demo_walkthrough\|real_activation" src/ static/  # ≥ 4

# Failure UX
grep -rn "showError\|apiFetch" static/app.js           # no bare 'fetch(' outside apiFetch
grep -rn 'setStatus..Error' static/app.js              # 0 (replaced by showError)

# Telemetry
grep -rn "funnel_event\|track(" src/ static/           # ≥ 8

# OAuth (primary)
grep -rn "oauth_authorization-server\|mcp/oauth/" src/  # ≥ 4

# Peers
grep -rn "peers/:id/manifest\|peers/:id/authorize" src/  # ≥ 2
grep -rn "tool_allowlist" src/services/                  # ≥ 1
```

## Self-review

**Contract conformance:** all names match [ione-complete-contract.md](../design/ione-complete-contract.md).

**Cross-layer completeness:** every UI component has a route; every route has a repo (or explicit no-DB note); every new table has a migration and a repo.

**Security:** OAuth tokens stored as sha256 hashes; PKCE S256 only; WWW-Authenticate on 401; demo guard after auth; tool allow-list enforced at scheduler; telemetry writes are fire-and-forget and never block user actions; static bearer opt-in.

**Codex findings closed:**
- #1 (false-positive completion): Slice 5 splits demo walkthrough from real activation; demo completion surfaces CTA, not activation-complete.
- #2 (connector UX not fixed): Slice 3 replaces free-form JSON with provider forms + `test_connection` + inline hints.
- #3 (scope mislabeled): no version framing; OAuth/peers sequenced after the onboarding-core slices but still in this plan.
- #4 (no telemetry): Slice 7 adds `funnel_events` + server emission + catalog + admin read.
- #5 (failure UX underspecified): Slice 6 pass + per-slice catalog rows.
- #6 (no a11y plan): Slice 10 gate + per-slice a11y notes in design doc.
- #7 (roles CRUD was backlog): dropped entirely.

---

## Task Manifest

| Task | Agent | Files | Depends On | Gate | Status |
|------|-------|-------|------------|------|--------|
| T1.1: Demo fixture + seeder + constant | rust-api-coder | src/demo/mod.rs, src/demo/fixture.rs, src/demo/seeder.rs, src/main.rs, Cargo.toml | — | cargo check | pending |
| T1.2: Canned chat layer | rust-api-coder | src/demo/canned_chat.rs, src/routes/conversations.rs | T1.1 | cargo check | pending |
| T1.3: Demo write-guard middleware | rust-api-coder | src/middleware/demo_guard.rs, src/routes/mod.rs | T1.1 | cargo check | pending |
| T1.4: Demo UI | ui-coder | static/index.html, static/app.js, static/style.css | T1.1, T1.2, T1.3 | node --check static/app.js | pending |
| T1.5: Demo tests | test-writer | tests/phase15_demo_workspace.rs | T1.1-T1.4 | cargo test --test phase15_demo_workspace -- --test-threads=1 | pending |
| T2.1: Ollama list_models + health route | rust-api-coder | src/services/ollama.rs, src/routes/health.rs, src/error.rs, src/routes/mod.rs | — | cargo check | pending |
| T2.2: Chat 503 remediation envelope | rust-api-coder | src/routes/conversations.rs, src/error.rs | T2.1 | cargo check | pending |
| T2.3: Health-dot UI + chat remediation card | ui-coder | static/index.html, static/app.js, static/style.css | T2.1, T2.2 | node --check static/app.js | pending |
| T2.4: Ollama tests | test-writer | tests/phase16_ollama_health.rs | T2.1-T2.3 | cargo test --test phase16_ollama_health -- --test-threads=1 | pending |
| T3.1: Validate trait + per-provider impls | rust-api-coder | src/connectors/validate/*.rs | — | cargo check | pending |
| T3.2: Validate route + 422 on create | rust-api-coder | src/routes/connectors.rs, src/routes/mod.rs | T3.1 | cargo check | pending |
| T3.3: Two-step connector wizard UI | ui-coder | static/index.html, static/app.js, static/style.css | T3.2 | node --check static/app.js | pending |
| T3.4: Validate tests | test-writer | tests/phase17_connector_validate.rs | T3.1-T3.3 | cargo test --test phase17_connector_validate -- --test-threads=1 | pending |
| T4.1: pipeline_events migration + model + repo | sql-coder + rust-api-coder | migrations/0012_pipeline_events.sql, src/models/pipeline_event.rs, src/repos/pipeline_event_repo.rs | — | cargo check | pending |
| T4.2: PipelineBus + state injection | rust-api-coder | src/services/pipeline_bus.rs, src/state.rs, src/main.rs, Cargo.toml | T4.1 | cargo check | pending |
| T4.3: Scheduler stage emission + stall watchdog | rust-api-coder | src/services/scheduler.rs | T4.2 | cargo check | pending |
| T4.4: Connector create synchronous first-run | rust-api-coder | src/routes/connectors.rs | T4.2, T3.2 | cargo check | pending |
| T4.5: Events list + SSE routes | rust-api-coder | src/routes/pipeline_events.rs, src/routes/mod.rs | T4.2 | cargo check | pending |
| T4.6: Demo seeds full stage timeline | rust-api-coder | src/demo/fixture.rs | T1.1, T4.1 | cargo check | pending |
| T4.7: Connector timeline UI + progress view | ui-coder | static/index.html, static/app.js, static/style.css | T4.5 | node --check static/app.js | pending |
| T4.8: Pipeline event tests | test-writer | tests/phase18_pipeline_events.rs | T4.3-T4.7 | cargo test --test phase18_pipeline_events -- --test-threads=1 | pending |
| T5.1: activation migration + model + repo | sql-coder + rust-api-coder | migrations/0013_activation.sql, src/models/activation.rs, src/repos/activation_repo.rs | — | cargo check | pending |
| T5.2: activation routes + server-side marks | rust-api-coder | src/routes/activation.rs, src/routes/mod.rs, src/routes/conversations.rs, src/routes/approvals.rs, src/services/scheduler.rs | T5.1, T4.3 | cargo check | pending |
| T5.3: Tracker UI + CTA + track routing | ui-coder | static/index.html, static/app.js, static/style.css | T5.2 | node --check static/app.js | pending |
| T5.4: Activation tests | test-writer | tests/phase19_activation.rs | T5.1-T5.3 | cargo test --test phase19_activation -- --test-threads=1 | pending |
| T6.1: Error envelope enforcement + hints | rust-api-coder | src/error.rs, src/routes/*.rs | T1-T5 | cargo check | pending |
| T6.2: Unified showError + apiFetch routing | ui-coder | static/app.js | T1-T5 | node --check static/app.js | pending |
| T6.3: Failure UX tests | test-writer | tests/phase20_failure_ux.rs, scripts/test-js.sh | T6.1, T6.2 | cargo test --test phase20_failure_ux + bash scripts/test-js.sh | pending |
| T7.1: funnel_events migration + model + repo + session cookie | sql-coder + rust-api-coder | migrations/0014_funnel_events.sql, src/models/funnel_event.rs, src/repos/funnel_event_repo.rs, src/middleware/session_cookie.rs, src/services/funnel.rs | — | cargo check | pending |
| T7.2: Telemetry routes + admin funnel | rust-api-coder | src/routes/telemetry.rs, src/routes/mod.rs | T7.1 | cargo check | pending |
| T7.3: Server-side track() call sites | rust-api-coder | src/routes/connectors.rs, src/services/scheduler.rs, src/routes/approvals.rs, src/routes/workspaces.rs, src/routes/conversations.rs | T7.1, T3.2, T4.3 | cargo check | pending |
| T7.4: Client-side track() + call sites | ui-coder | static/app.js | T7.1 | node --check static/app.js | pending |
| T7.5: Telemetry tests | test-writer | tests/phase21_telemetry.rs | T7.1-T7.4 | cargo test --test phase21_telemetry -- --test-threads=1 | pending |
| T8.0: OAuth round-trip gate (manual) | — | — | — | Manual: Claude Desktop Pro adds IONe custom connector, tools/list returns 200 | pending |
| T8.1: OAuth migration + models + repos | sql-coder + rust-api-coder | migrations/0015_oauth.sql, src/models/oauth.rs, src/repos/oauth_client_repo.rs, src/repos/oauth_token_repo.rs | T8.0 | cargo check | pending |
| T8.2: OAuth routes (discovery/register/authorize/token/revoke) | claude-code | src/routes/oauth.rs, src/routes/oauth_consent.rs, src/routes/mod.rs, src/config.rs, Cargo.toml | T8.1 | cargo check | pending |
| T8.3: MCP bearer middleware | claude-code | src/middleware/mcp_bearer.rs, src/routes/mod.rs, src/mcp_server.rs | T8.1 | cargo check | pending |
| T8.4: mcp_clients API | rust-api-coder | src/routes/mcp_clients.rs, src/repos/oauth_client_repo.rs, src/routes/mod.rs | T8.1 | cargo check | pending |
| T8.5: Connect-to-MCP page | ui-coder | static/index.html, static/app.js, static/style.css | T8.4 | node --check static/app.js | pending |
| T8.6: OAuth tests | test-writer | tests/phase22_mcp_oauth.rs | T8.2-T8.5 | cargo test --test phase22_mcp_oauth -- --test-threads=1 | pending |
| T8.F: Fallback path (if T8.0 fails) | claude-code | src/routes/mcp_tokens.rs, static/index.html, static/app.js, README.md | T8.0 → fail | cargo check + README accurate | pending |
| T9.1: peers OAuth migration + model + repo | sql-coder + rust-api-coder | migrations/0016_peers_oauth.sql, src/models/peer.rs, src/repos/peer_repo.rs | T8.1 or T8.F | cargo check | pending |
| T9.2: peer OAuth client service + well-known | claude-code | src/services/peer_oauth.rs, src/routes/well_known.rs, Cargo.toml | T9.1 | cargo check | pending |
| T9.3: Federation routes + scheduler allowlist enforcement | rust-api-coder | src/routes/peer_federation.rs, src/routes/peers.rs, src/services/scheduler.rs, src/routes/mod.rs | T9.2 | cargo check | pending |
| T9.4: Federation wizard UI | ui-coder | static/index.html, static/app.js, static/style.css | T9.3 | node --check static/app.js | pending |
| T9.5: Federation tests | test-writer | tests/phase23_peer_federation.rs | T9.3, T9.4 | cargo test --test phase23_peer_federation -- --test-threads=1 | pending |
| T10.1: a11y + viewport scripts | claude-code | scripts/a11y-check.sh, scripts/viewport-check.md | T1-T9 | bash scripts/a11y-check.sh | pending |
| T10.2: style.css hover + reduced-motion guards | ui-coder | static/style.css | T1-T9 | bash scripts/a11y-check.sh | pending |
| T10.3: ARIA + focus audit + fixes | ui-coder | static/index.html, static/app.js | T1-T9 | bash scripts/a11y-check.sh + manual viewport-check | pending |
| TR.1: README + CHANGELOG accuracy | claude-code | README.md, CHANGELOG.md | all slices | grep claims against features | pending |
| TR.2: Full matrix + sqlx cache | claude-code | .sqlx/ | all | cargo test -- --ignored --test-threads=1 && cargo fmt --check && cargo clippy --all-targets -- -D warnings | pending |

**Parallel groups for `/co-code`:**
- T2.1, T3.1, T4.1 — independent, parallel after Slice 1.
- T4.3, T4.5 — parallel after T4.2.
- T7.3, T7.4 — parallel after T7.2.
- T8.2, T8.3, T8.4 — parallel after T8.1.
- T10.1, T10.2, T10.3 — parallel final sweep.

Integration-test tasks always run after their slice's implementation tasks.
