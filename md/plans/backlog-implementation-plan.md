# Remaining IONe Backlog Implementation Plan

## Recommendation

Start with peer delegated-token refresh. It is the highest-leverage item because every shipped peer-backed visualization path depends on valid peer access tokens, and the current playbook promises automatic refresh while the `peers` schema cannot perform it.

## Priority Order

1. Peer delegated-token refresh.
2. Small shipped-work follow-ups: Epicenter M>=6.0 rules integration test and playbook dotted-key wording.
3. MCP `notifications/*` reception.
4. P3 federation: tool namespacing, `slice://` lazy expansion, semantic catalog + pgvector search.
5. P4 identity/governance: SAML SP, auto-exec policy DSL, auto-exec bypass guard audit.
6. P5 UX and hardening: theming hooks, connector/timeline polish, app-wide CSP spike.

## Execution Rules

- Use `/design -> /implement -> /code-the-plan` per logical unit.
- Review each implementation before committing.
- Commit per logical unit and do not push unless explicitly requested.
- Keep DB tests on Postgres `localhost:5433`, ignored suites serial with `--test-threads=1`.
- Run `cargo clippy --all-targets -- -D warnings` before each commit.
- Preserve org scoping with `ensure_workspace_in_org(...)` and existing workspace-peer binding patterns.
- Keep unrelated `.claude/` local/session changes out of commits.

## Unit 1: Peer Delegated-Token Refresh

Design: `md/design/peer-token-refresh.md`.

### Technical Requirements

- Add nullable `peers.refresh_token_ciphertext`.
- Store encrypted refresh token ciphertext during peer OAuth callback.
- Add server-side helper for peer token resolution, refresh, and one retry after HTTP 401.
- Use the helper in map/chart/table/document peer panels, chart/table data reads, peer manifest fetch, workspace binding `whoami`, and MCP connector calls.
- Retain `refresh_token_hash` and existing static-bearer fallback.

### Test Contract

- Schema test proves `peers.refresh_token_ciphertext` exists and is not serialized.
- OAuth callback/unit integration proves refresh ciphertext is stored when a peer returns a refresh token.
- Peer panel integration proves an expired peer access token is refreshed and the retried resource list succeeds.
- MCP connector integration or service test proves tool calls use refreshed peer tokens when `peer_id` and pool are present.
- Regression tests keep partial-success behavior when one peer refresh fails.

### Validation

- `cargo test --test contract_schema schema_peers_extended_column_refresh_token_ciphertext -- --ignored --test-threads=1`
- targeted peer panel/data tests touched by the refactor, serial where Postgres-backed
- `cargo clippy --all-targets -- -D warnings`

## Unit 2: Shipped-Work Follow-ups

### Technical Requirements

- Add an ignored integration test proving a rules condition using `payload.properties.mag >= 6.0` matches a nested payload.
- Correct playbook wording to say rules use dotted evalexpr keys, not JSON Pointer syntax.

### Validation

- Targeted rules integration test.
- `cargo clippy --all-targets -- -D warnings`

## Unit 3: MCP `notifications/*` Reception

Design from `md/design/push-ingress.md` and the MCP surface in `md/design/app-integration-playbook.md`.

### Technical Requirements

- Accept peer-authenticated MCP notification methods through IONe's MCP HTTP endpoint.
- Normalize notification payloads into the existing push-ingress signal path.
- Enforce the same `approval_required` policy floor as signed webhooks.
- Deduplicate notifications consistently with webhook ingress where an event id is supplied.

### Validation

- MCP notification happy path creates a signal.
- Peer auth failure rejects notification.
- `approval_required` can escalate but not de-escalate flagged/command severity.
- Duplicate notification id is idempotent.

## Unit 4: P3 Federation

### Technical Requirements

- Namespaced hub tool IDs prevent collisions while preserving peer provenance.
- `slice://` resources expand lazily through IONe-side routing.
- Semantic catalog indexes peer resources/tool summaries with pgvector and org/workspace scoping.

### Validation

- Two peers can export the same tool name without collision.
- `slice://` returns compact catalog data and expands individual tools/resources on demand.
- Vector search respects org/workspace boundaries.

## Unit 5: P4 Identity and Governance

### Technical Requirements

- SAML SP support either lands natively or is explicitly delegated to Keycloak with playbook honesty.
- Auto-exec policy DSL is deny-by-default and cannot bypass app-declared or severity-derived approval gates.
- Auto-exec bypass guard audit verifies `approval_required` always forces draft/approval.

### Validation

- Policy DSL tests cover allow, deny, malformed policy, and escalation cases.
- Router tests prove `approval_required` cannot be bypassed.

## Unit 6: P5 UX and CSP Spike

### Technical Requirements

- Theming hooks define reusable CSS tokens before app-specific styling.
- CSP spike measures MapLibre, vendored myIO, table panels, document iframe, and peer frame origins.
- Only ship CSP after browser validation proves no shipped visualization path breaks.

### Validation

- Playwright pass across map/chart/table/document tabs.
- CSP report-only or local browser-console check before enforcing.
