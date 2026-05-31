# Peer Delegated-Token Refresh Plan

## Objective

Implement automatic peer OAuth token refresh for server-side peer calls so shipped peer panels and MCP connector paths do not fail solely because an access token expired.

## Design Summary

IONe stores peer access tokens as ciphertext today, but only stores refresh token hashes. Add recoverable refresh token ciphertext, refresh peer access tokens server-side, and route peer MCP calls through one token-aware helper that preserves existing partial-success behavior.

## Technical Requirements

- Add `peers.refresh_token_ciphertext BYTEA NULL`.
- Extend `Peer` and all peer SQL projections with `refresh_token_ciphertext`, skipped in serialization.
- On peer OAuth callback, encrypt `refresh_token` when present and persist it with the existing hash.
- Add peer-token service functions:
  - resolve current token,
  - force refresh,
  - send MCP JSON-RPC request with refresh-before-expiry and one 401 retry.
- Use the helper in peer visualization fan-out, resource reads, manifest fetch, whoami binding refresh, and MCP connector tool calls.
- Keep legacy `IONE_OAUTH_STATIC_BEARER` fallback when peer rows do not have OAuth token ciphertext.

## Constraints and Guardrails

- Do not expose raw or encrypted token fields in API JSON.
- Do not log token values or token hashes.
- Do not convert aggregate panel endpoints into all-or-nothing responses.
- Do not require a destructive backfill; old peers without refresh ciphertext can re-authorize.
- Do not push commits.

## Test Contract

- Schema test: `peers.refresh_token_ciphertext` exists.
- Storage test: callback token persistence stores encrypted refresh ciphertext when a refresh token is returned.
- Refresh test: an expired peer with refresh ciphertext calls the token endpoint, updates token fields, and retries a peer MCP request with the new bearer.
- Regression test: peers without ciphertext keep using static bearer.

## Implementation Plan

1. Add migration `0032_peer_refresh_token_ciphertext.sql`.
2. Update peer model and repository methods.
3. Store refresh ciphertext in `peer_oauth::complete_callback`.
4. Create `services::peer_tokens` with refresh and MCP request helpers.
5. Refactor peer panels, peer data reads, manifest fetch, whoami, and MCP connector calls to use the helper.
6. Add targeted schema and integration tests.
7. Run targeted ignored tests serially and clippy.
8. Review the implementation against auth, secret-handling, tenancy, migration safety, and partial-success behavior.
9. Commit the logical unit.

## Validation

Use local Postgres on `localhost:5433`.

```bash
docker compose up -d postgres
DATABASE_URL=postgres://ione:ione@localhost:5433/ione cargo test --test contract_schema schema_peers_extended_column_refresh_token_ciphertext -- --ignored --test-threads=1
DATABASE_URL=postgres://ione:ione@localhost:5433/ione IONE_TOKEN_KEY=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA= cargo test --test phase_peer_token_refresh -- --ignored --test-threads=1
cargo clippy --all-targets -- -D warnings
```

## Open Questions

- Leave repeated refresh failures request-scoped for this slice; mark-peer-unhealthy behavior should wait for a dedicated peer health model.
