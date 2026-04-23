# IONe Review Fixes — Plan

**Date:** 2026-04-23
**Source:** Code review of the 42-commit complete-product session ([diff f98728f..HEAD](../..)). 3 security blockers, 9 warnings, 3 nits.
**Design:** [md/design/ione-complete.md](../design/ione-complete.md) (unchanged — these are fidelity fixes)
**Layers:** `api`, some `db` (no new migrations), no `ui`

## Why the review caught things the session didn't

The session's success gate was the **58 contract tests**, and the contract tests encode structural facts (table exists, column nullable, route returns 2xx/4xx instead of 404, error envelope has an `error` field). They do not encode:

1. **RFC-level OAuth semantics** — `redirect_uri` registration binding, refresh-token rotation revoking access, revoke endpoint requiring client auth. These aren't contract assertions; they're conformance-to-spec invariants. A route-existence test passes if the endpoint returns any 2xx; it does not detect that the endpoint accepts a malicious `redirect_uri`.
2. **SSRF** — server-side fetch of a user-supplied URL is a functional feature (CIMD, peer discovery). The contract tests assert "register returns 200 with a clientId"; they don't probe "what if the URL is `http://169.254.169.254/`". Security review is required to see it.
3. **Telemetry semantics vs. emission** — contract tests assert the `funnel_events` table exists and `POST /telemetry/events` routes. They don't assert that `first_real_signal` is emitted when the scheduler persists a signal, or that `first_real_approval_decided` only fires on the first approval decision, because those are behavioral properties that require a scheduler-level integration test.
4. **Dangling state on partial failure** — the canned-chat user-row-but-no-assistant-row bug is invisible to a contract test that only checks the happy path.
5. **Stub vs. real** — `fetch_manifest_over_mcp` returns `{tools: []}` as a deliberate stub in T9.3 (flagged in the prompt as `// TODO`). The UI's allow-list screen renders against whatever the endpoint returns, which is empty. The contract test for "route exists" passes; the feature's behavior is broken.

The takeaway: contract tests are a necessary floor, not a ceiling. Security review + semantic integration tests sit above them. This plan adds the integration tests that would have caught each issue, so future implementation passes don't have the same gap.

## Scope

16 findings across 3 slices:
- **Slice S1 — Security hardening** (8 findings: B1, B2, B3, W4, W5, W6, W7, W8, N14)
- **Slice S2 — Telemetry fidelity** (4 findings: W9, W11, W12, N16)
- **Slice S3 — Peer manifest** (1 finding: W10)
- **Slice S4 — Misc correctness** (3 findings: W13, N15, plus one new integration test)

Total: ~1.5 days work. Each slice independently shippable.

---

## Slice S1 — Security hardening

Finishes OAuth 2.1 conformance and closes SSRF on two fetch-URL endpoints. Refs: B1, B2, B3, W4, W5, W6, W7, W8, N14.

### S1.1 — `redirect_uri` registration binding (B1)

**File:** `src/routes/oauth.rs`

**Change:**
- In `authorize` (GET and POST), after loading the `OauthClient` row, parse `client.client_metadata.redirect_uris` as `Vec<String>`. If `q.redirect_uri` is not exact-string-equal to any registered entry, return `AppError::BadRequest("redirect_uri not registered")`. No substring matching, no prefix matching, no scheme-normalizing — exact string per RFC 6749 §3.1.2.3.
- In `token` (authorization_code grant), if the request body includes a `redirect_uri`, compare to `row.redirect_uri` (already done). Extend: also confirm `row.redirect_uri` is still a registered URI for that `client_id` at token time (defense against out-of-band registration rollback).

**Gate:** new integration test `oauth_rejects_unregistered_redirect_uri` in `tests/contract_errors.rs` — authorize with `redirect_uri=https://evil.example.com/x` returns 400 with `error: bad_request` and a message referencing redirect_uri.

### S1.2 — CIMD fetch SSRF guard (B2)

**File:** `src/routes/oauth.rs` `register()` + new helper module `src/util/safe_http.rs`

**Change:**
- Create `src/util/safe_http.rs` with `pub async fn fetch_public_metadata(url: &str, max_bytes: usize, timeout: Duration) -> Result<Value, AppError>`:
  - Parse as `url::Url`; require scheme `https` (allow `http` only if `std::env::var("IONE_SSRF_DEV").is_ok()`)
  - Resolve host via `tokio::net::lookup_host`; reject if ANY resolved address is in RFC1918, loopback (`127.0.0.0/8`, `::1`), link-local (`169.254.0.0/16`, `fe80::/10`), ULA (`fc00::/7`), carrier-NAT (`100.64.0.0/10`), or `0.0.0.0`/`0::`
  - Build `reqwest::Client` with `redirect::Policy::none()`, `timeout(timeout)`, `connect_timeout(Duration::from_secs(3))`
  - Send, cap body at `max_bytes` via `.bytes_stream().take_while(len <= max_bytes)`, parse JSON
- Replace `reqwest::Client::new().get(client_metadata_url).send()` in `register` with `fetch_public_metadata(url, 64_000, Duration::from_secs(5))`
- Sanitize the error message: on any fetch/parse failure return `AppError::BadRequest("invalid client metadata")` — do NOT echo upstream bytes or error text.

**Cargo.toml:** add `url = "2"` if not already a dep.

**Gate:** new integration test `register_rejects_loopback_url` in `tests/contract_errors.rs` — POST `/mcp/oauth/register` with `client_metadata_url: "http://127.0.0.1:65535/"` returns 400 with `error: bad_request` and does not include `127.0.0.1` in the response message.

### S1.3 — Peer discovery SSRF guard (B3)

**File:** `src/services/peer_oauth.rs::begin_federation`

**Change:**
- Use the `safe_http::fetch_public_metadata` helper from S1.2 for the discovery GET.
- Additionally: after parsing `PeerDiscovery`, verify `authorization_endpoint`, `token_endpoint`, `registration_endpoint` all have the same host as `peer_url`. If any differs, return `AppError::BadRequest("peer endpoints must match peer host")`.
- Same error sanitization as S1.2.

**Gate:** new integration test `peer_federation_rejects_private_peer_url` in `tests/contract_errors.rs` — POST `/api/v1/peers {"peerUrl": "http://10.0.0.1"}` returns 400.

### S1.4 — Constant-time bearer comparison (W4)

**File:** `src/middleware/mcp_bearer.rs`

**Change:**
- Add `subtle = "2"` to Cargo.toml.
- Replace `token == expected` with `subtle::ConstantTimeEq::ct_eq(token.as_bytes(), expected.as_bytes()).into()`.
- Keep the `!expected.is_empty()` check BEFORE the ct_eq call so an unset env var never matches.

**Gate:** unit test in `mcp_bearer.rs` asserting both correct and incorrect tokens return the expected auth decision; no timing assertion (hard to test reliably), just correctness preserved.

### S1.5 — Refresh rotation revokes paired access token (W5)

**File:** `src/routes/oauth.rs::token()` refresh branch

**Change:**
- Before issuing new access + refresh tokens in the `RefreshToken` match arm, call `token_repo.revoke_client_tokens(&client_id, user_id)`.
- Then insert new access + refresh tokens. Order matters: revoke first so concurrent reads of the old access token see `revoked_at IS NOT NULL` immediately.

**Gate:** new integration test `refresh_revokes_old_access_token` in `tests/contract_errors.rs` — exchange auth code for tokens, use refresh to get new tokens, assert old access token fails `/mcp/tools/list` with 401.

### S1.6 — Session cookie HttpOnly + Secure (W6)

**File:** `src/middleware/session_cookie.rs`

**Change:**
- Update cookie format to: `ione_session={id}; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=31536000`
- Gate `Secure` on `IONE_COOKIE_INSECURE` env var for local dev: if set, omit `Secure`. Default production behavior is Secure.

**Gate:** curl test in integration: `curl -I http://127.0.0.1:3000/` (with IONE_COOKIE_INSECURE=1) must include `Set-Cookie: ione_session=...; HttpOnly;` in the response headers.

### S1.7 — Demo guard covers non-`/workspaces/:id` paths (W7)

**File:** `src/middleware/demo_guard.rs`

**Change:**
Rewrite `extract_workspace_id_from_path` as `resolve_workspace_id` that:
- For `/api/v1/workspaces/<uuid>/*` paths: extract directly (existing behavior).
- For `/api/v1/streams/<uuid>/*`: SELECT `workspace_id FROM streams JOIN connectors ON ... WHERE streams.id = $1`.
- For `/api/v1/approvals/<uuid>`: SELECT `workspace_id FROM artifacts JOIN approvals ON ... WHERE approvals.id = $1`.
- For `/api/v1/peers/<uuid>*` and `/api/v1/peers/<uuid>/subscribe`: SELECT `workspace_id FROM peers WHERE peers.id = $1` (if peers has a workspace_id column; if not, let the per-handler auth layer handle it and skip here).
- For `/api/v1/conversations/<uuid>/messages`: DO NOT guard — Slice 1 intentionally allows demo conversations to answer via canned layer.

Return `Option<Uuid>`. If it resolves to `DEMO_WORKSPACE_ID` and the method is mutating (non-GET/HEAD/OPTIONS), 403 `demo_read_only`.

Middleware runs after `auth_middleware`, so DB access via `state.pool` is valid. One SELECT per guarded mutating request — acceptable overhead.

**Gate:** new test `demo_guard_blocks_stream_poll_on_demo` in `tests/contract_errors.rs` — POST `/api/v1/streams/<demo-stream-uuid>/poll` returns 403 `demo_read_only`.

### S1.8 — Revoke endpoint requires client_id (W8)

**File:** `src/routes/oauth.rs::revoke()`

**Change:**
- Add `client_id: String` as required field in `RevokeBody`.
- After hashing the token, load the matching token row (access or refresh) and compare `row.client_id == body.client_id`. If no match, return 200 anyway (per RFC 7009 §2.1 — don't leak token existence) but do NOT revoke.
- Log the mismatch case at `tracing::warn!` for audit.

**Gate:** new test `revoke_rejects_wrong_client_id` in `tests/contract_errors.rs` — revoke with the correct token but wrong `client_id` returns 200 but the token still works on `/mcp/tools/list`.

### S1.9 — Validator endpoint SSRF (N14)

**File:** `src/connectors/validate/irwin.rs`, `src/connectors/validate/s3.rs`

**Change:** Apply `safe_http::fetch_public_metadata` (S1.2) to any outbound HTTP from these validators when the host is user-supplied. For S3, if the existing impl uses `aws_sdk_s3`, leave it — SDK-level endpoint validation is acceptable. For IRWIN, validate the `endpoint` URL passes SSRF screening before any GET/HEAD.

**Gate:** new test `irwin_validate_rejects_private_endpoint` in `tests/contract_errors.rs`.

### S1.10 — Integration test: full OAuth round-trip

**File:** new `tests/integration_oauth_roundtrip.rs`

**Change:** One end-to-end test that:
1. Registers a client via `/mcp/oauth/register` with a valid `client_metadata_url` (use a locally-bound test server hosting a well-known JSON doc).
2. Starts an authorize → expects 200 with consent HTML.
3. Posts the consent form with `action=allow`.
4. Parses the 302 Location header for the `code`.
5. Exchanges code + verifier at `/mcp/oauth/token`.
6. Calls `/mcp/tools/list` with the access token — expects 200.
7. Calls `/mcp/oauth/token` again with the refresh token — expects new access + refresh.
8. Verifies old access token returns 401 on `/mcp/tools/list` (closes #5).

This test is the gate that would have caught B1, B2, W5, W8 in one run.

**Exit criteria for Slice S1:**
```
cargo test --test contract_errors oauth -- --ignored --test-threads=1
cargo test --test integration_oauth_roundtrip -- --ignored --test-threads=1
cargo clippy --all-targets -- -D warnings
```

---

## Slice S2 — Telemetry fidelity

Finishes the Slice 7 event catalog and fixes session-cookie robustness. Refs: W9, W11, W12, N16.

### S2.1 — Scheduler emits `first_real_signal` (W9)

**File:** `src/services/scheduler.rs`

**Change:**
- At the point where the first `PipelineEventStage::FirstSignal` fires for a workspace (T4.3 code), call `funnel::track(...)` with event_kind `first_real_signal`.
- Attribution: the scheduler has no session_id or user_id. Use a synthetic session from the workspace UUID (`Uuid::new_v5(&NAMESPACE_OID, workspace_id.as_bytes())`) so funnel joins on session work, and pass `user_id = None` because scheduler runs as system.
- Only emit once per workspace per day: before emitting, query `SELECT EXISTS(...) FROM funnel_events WHERE event_kind = 'first_real_signal' AND workspace_id = $1 AND occurred_at > now() - interval '1 day'`. If exists, skip.

**Gate:** new integration test `scheduler_emits_first_real_signal_once_per_workspace` in `tests/phase05_signals.rs` or a new `tests/integration_telemetry.rs`.

### S2.2 — `first_real_approval_decided` only on first (W11)

**File:** `src/routes/approvals.rs::decide_approval`

**Change:**
- After the approval is persisted and before emitting the funnel event, call `ActivationRepo::is_step_complete(ctx.user_id, workspace_id, RealActivation, FirstApprovalDecided)`. If already complete, skip the funnel emit.
- The activation mark (which also fires here via T5.2) is already idempotent; the ordering is: activation mark first → check → emit funnel on miss.

**Gate:** integration test that decides 3 approvals and asserts exactly one `first_real_approval_decided` funnel row.

### S2.3 — Session cookie malformed input (W12)

**File:** `src/middleware/session_cookie.rs`

**Change:**
Fix `parse_session_cookie` to return `Option<Uuid>` where `None` covers both "cookie absent" and "cookie present but malformed". Update the middleware logic:
```rust
let existing: Option<Uuid> = req
    .headers()
    .get(header::COOKIE)
    .and_then(|h| h.to_str().ok())
    .and_then(parse_session_cookie);

let (session_id, is_new) = match existing {
    Some(id) => (id, false),
    None => (Uuid::new_v4(), true),  // was correct structurally but depended on parse_session_cookie returning None on malformed
};
```
If `parse_session_cookie` currently returns `Some(None)` on malformed, flatten to `Option<Uuid>`.

**Gate:** unit test with `Cookie: ione_session=not-a-uuid` header — assert response has `Set-Cookie: ione_session=<new uuid>`.

### S2.4 — `activation_completed` funnel event (N16)

**File:** `src/repos/activation_repo.rs` + call sites

**Change:**
- Add a method `ActivationRepo::is_track_complete(user_id, workspace_id, track) -> Result<bool>`. Returns true iff all expected steps for that track are in `activation_progress`.
- In every handler that calls `activation_repo.mark(...)` (conversations.rs, approvals.rs, connectors.rs, scheduler first_signal path if it marks), after the mark succeeds, call `is_track_complete`. If newly complete (compare to pre-mark state — OR just: emit `activation_completed` with `detail: { track }` and rely on UI-side dedup since this fires at most once per track anyway in practice), emit `funnel::track("activation_completed", ...)`.
- Simpler: just emit every time the final step is marked. Idempotent on the DB side.

**Gate:** integration test completing all 4 demo steps in one session → exactly one `activation_completed` row with `detail.track = "demo_walkthrough"`.

**Exit criteria for Slice S2:**
```
cargo test --test integration_telemetry -- --ignored --test-threads=1
cargo clippy --all-targets -- -D warnings
```

---

## Slice S3 — Peer manifest (stop lying in README)

Finishes Slice 9's allow-list UX or retreats to honest docs. Ref: W10.

### S3.1 — Option A: Real manifest fetch (preferred)

**File:** `src/routes/peers.rs::fetch_manifest_over_mcp` (currently stub returning `{tools: []}`)

**Change:**
- Load the peer row; if `status != 'pending_allowlist'` or `access_token_hash IS NULL`, return 409.
- Look up the peer's stored access token. Problem: we store the sha256 hash, not the plaintext. To make `tools/list` calls, we need either:
  - (a) Store the plaintext access token encrypted with a server-side key, decrypt on use. Add `oauth_access_tokens.encrypted_token` column and a `IONE_TOKEN_KEY` env var for the key.
  - (b) Store plaintext in `peers.access_token_plaintext` (server-only). Simpler, but widens the blast radius if the DB leaks.

Pick (a). Add migration `0017_oauth_token_plaintext.sql`:
```sql
ALTER TABLE peers ADD COLUMN access_token_ciphertext BYTEA NULL;
```

Use `aes-gcm = "0.10"` with a 256-bit key from `IONE_TOKEN_KEY` (fail startup if not set). Encrypt on `set_tokens`, decrypt here.

- MCP call: `POST {peer.peer_url}/mcp` with `Authorization: Bearer <decrypted>` and body `{"jsonrpc":"2.0","id":1,"method":"tools/list"}`. Parse `result.tools`. Return `{ tools: [...] }`.

**Gate:** new integration test `peer_manifest_returns_real_tool_list` — spin up a mock MCP server, complete federation, call `/peers/:id/manifest`, assert the tool names match the mock server's.

### S3.2 — Option B: Mark as stub in README and remove UI allow-list step

**File:** `README.md`, `static/app.js::renderPeerTools`

**Change:**
- README: add a note under Peer federation: "v0.2: tool discovery is a stub — the allow-list form accepts tool names as free text. Real manifest fetch ships in v0.3 alongside token encryption at rest."
- `renderPeerTools`: if the response is empty, render a textarea labeled `Enter tool names, one per line` instead of an empty fieldset. Parse on submit and POST as `toolAllowlist`.

**Recommendation:** if you want the advertised Slice 9 UX, do S3.1. If you want to ship the fixes quickly and defer, do S3.2 and schedule S3.1.

**Exit criteria:** new integration test passes (S3.1) OR README + UI reflect the stub honestly (S3.2).

---

## Slice S4 — Misc correctness

Small fixes that didn't warrant their own slice. Refs: W13, N15.

### S4.1 — Canned chat is transactional (W13)

**File:** `src/routes/conversations.rs::post_message` demo branch

**Change:**
Wrap the two `msg_repo.append` calls (user message, canned assistant reply) in a single `pool.begin()` transaction. If the assistant append fails, the transaction rolls back and no user row persists.

Existing pattern for non-demo path: Ollama error bubbles up *before* the assistant append, so that path is already correct.

**Gate:** unit-level mock test in `conversations.rs` — force the assistant append to fail via an invalid model string; assert the user row is not present in the DB.

### S4.2 — Drop `ok` field from `ValidateErr` (N15)

**File:** `src/connectors/validate/mod.rs`

**Change:**
- Remove the `ok: bool` field from `ValidateErr` and `ValidateOk`. The 200/422 status code already communicates success/failure; the body shape `{error, message, hint?, field?}` matches Slice 6's envelope.
- Update every callsite that constructs a `ValidateErr` to drop the `ok` argument.

**Gate:** existing contract test `error_nws_out_of_range_on_connector_validate` passes (it already asserts the envelope shape).

**Exit criteria for Slice S4:**
```
cargo test --test contract_errors -- --ignored --test-threads=1
cargo test --lib conversations -- --test-threads=1
```

---

## Sequencing

1. **S1** (security hardening) — ship first. Includes S1.10 integration test which also regression-tests the later changes.
2. **S2** (telemetry fidelity) — independent of S1; parallel-safe.
3. **S4** (misc correctness) — small; can ship alongside S2.
4. **S3** (peer manifest) — ship last: either the implementation (S3.1) or the README retreat (S3.2). Bigger decision.

No migration ordering conflict: S3.1 adds `0017_oauth_token_plaintext.sql` — only run if S3.1 is chosen. Other slices add no migrations.

## Task Manifest

| Task | Agent | Files | Depends On | Gate | Status |
|------|-------|-------|------------|------|--------|
| S1.1: redirect_uri validation | rust-api-coder | src/routes/oauth.rs | — | cargo test contract_errors oauth_rejects_unregistered_redirect_uri | pending |
| S1.2: safe_http module + register SSRF | claude-code | src/util/safe_http.rs, src/routes/oauth.rs, Cargo.toml | S1.1 | cargo test contract_errors register_rejects_loopback_url | pending |
| S1.3: peer discovery SSRF | rust-api-coder | src/services/peer_oauth.rs | S1.2 | cargo test contract_errors peer_federation_rejects_private_peer_url | pending |
| S1.4: constant-time bearer | rust-api-coder | src/middleware/mcp_bearer.rs, Cargo.toml | — | cargo test --lib mcp_bearer | pending |
| S1.5: refresh revokes access | rust-api-coder | src/routes/oauth.rs | S1.1 | cargo test refresh_revokes_old_access_token | pending |
| S1.6: cookie HttpOnly+Secure | rust-api-coder | src/middleware/session_cookie.rs | — | curl -I | grep Set-Cookie | pending |
| S1.7: demo guard full path coverage | claude-code | src/middleware/demo_guard.rs | — | cargo test demo_guard_blocks_stream_poll_on_demo | pending |
| S1.8: revoke requires client_id | rust-api-coder | src/routes/oauth.rs | — | cargo test revoke_rejects_wrong_client_id | pending |
| S1.9: validator SSRF | rust-api-coder | src/connectors/validate/irwin.rs, src/connectors/validate/s3.rs | S1.2 | cargo test irwin_validate_rejects_private_endpoint | pending |
| S1.10: OAuth round-trip integration test | test-writer | tests/integration_oauth_roundtrip.rs | S1.1, S1.5, S1.8 | cargo test --test integration_oauth_roundtrip | pending |
| S2.1: scheduler first_real_signal | rust-api-coder | src/services/scheduler.rs | — | cargo test scheduler_emits_first_real_signal_once_per_workspace | pending |
| S2.2: first_real_approval_decided once | rust-api-coder | src/routes/approvals.rs, src/repos/activation_repo.rs | — | cargo test first_approval_funnel_once | pending |
| S2.3: session cookie malformed input | rust-api-coder | src/middleware/session_cookie.rs | — | cargo test --lib session_cookie | pending |
| S2.4: activation_completed event | rust-api-coder | src/repos/activation_repo.rs, src/routes/* | S2.2 | cargo test activation_completed_fires_once | pending |
| S3 decision | human | — | S1, S2 | choose S3.1 (impl) or S3.2 (docs) | pending |
| S3.1 (if chosen): real manifest + token encryption | claude-code | migrations/0017_oauth_token_plaintext.sql, src/services/peer_oauth.rs, src/routes/peers.rs, src/repos/peer_repo.rs, Cargo.toml | S3 decision = A | cargo test peer_manifest_returns_real_tool_list | pending |
| S3.2 (if chosen): stub honesty | ui-coder | README.md, static/app.js | S3 decision = B | grep "tool discovery is a stub" README.md | pending |
| S4.1: canned chat transactional | rust-api-coder | src/routes/conversations.rs | — | cargo test --lib conversations | pending |
| S4.2: drop ok field from ValidateErr | rust-api-coder | src/connectors/validate/mod.rs | — | cargo test contract_errors error_nws_out_of_range_on_connector_validate | pending |

Parallel groups:
- S1.1, S1.4, S1.6, S1.7, S1.8 — independent, parallel
- S1.2 unblocks S1.3 and S1.9
- S1.10 is the gate after all S1 tasks
- S2.1, S2.2, S2.3 — independent, parallel
- S4.1, S4.2 — independent, parallel, can run alongside S2

## Rollback plan

Each slice lands as its own commit (or tight sequence). If any slice breaks something, revert the slice's commits; contract tests stay green at every intermediate state.

## What this plan does NOT cover

- Full security audit (penetration testing). This plan closes the known review findings; a real audit would find more.
- Claude Desktop Pro OAuth round-trip (T8.0 from the original plan) — still outstanding as a manual gate.
- v0.3 scope items from Slice 3 / Slice 9 roadmap notes.
