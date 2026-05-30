# UX + Security Audit Backlog

Status: tracked follow-up after the P0-P2 remediation pass.

## P3 Security

- **App-wide Content-Security-Policy.** Deferred from the document-view design (`md/design/document-view.md` Slice 3 SHOULD). Add a baseline CSP HTTP header with `frame-src` limited to bound-peer origins + `frame-ancestors 'self'`. **Needs its own spike** — must not break the shipped MapLibre tiles, vendored myIO, or table panels. The per-element controls (iframe `sandbox`, https-only `download_url` validation, `nosniff`) already shipped; this is defense-in-depth on top.
- Resolve the RLS false-security signal by either wiring request-scoped `SET LOCAL app.current_org_id` for protected tables or disabling RLS where app-layer predicates remain the real isolation boundary.
- Add rate limiting to the MFA challenge endpoint.
- Review `whoami://` disclosure and either document the current email/role exposure or scope-gate it.

## Identity-broker drift

- ✅ **Peer delegated-token refresh.** Resolved by `md/design/peer-token-refresh.md` and `md/plans/peer-token-refresh-plan.md`: peer OAuth callback now stores recoverable `peers.refresh_token_ciphertext`, peer MCP calls refresh before expiry and retry once on 401, and map/chart/table/document panels, chart/table data reads, manifest fetch, `whoami`, and MCP connector calls share the refresh-aware token path. Existing peers without refresh ciphertext still need re-authorization after expiry. Surfaced by the map-view code review (2026-05-21). Shipped in PR #3 (`a0c49de`).

  Follow-ups surfaced by the post-implementation review (2026-05-29), none blocking:
  - **No single-flight lock on concurrent refresh.** The panel fan-out loads map/chart/table/document in parallel; each can independently detect an expired token and fire its own refresh (`src/services/peer_tokens.rs::resolve_access_token`). Harmless when the IdP's refresh token is reusable; with refresh-token rotation, concurrent losers get one "peer unavailable" until the winner's DB write lands. Add a per-peer single-flight guard (advisory lock or in-process mutex) if rotation-based peers appear.
  - **401-retry re-reads stale in-memory peer.** `send_mcp_request`'s refresh-on-401 path calls `refresh_access_token` with the same in-memory `Peer` struct, so it decrypts the *pre-refresh* refresh ciphertext rather than the just-rotated one written by a prior proactive refresh in the same call. Edge case; re-fetch the peer (or thread the rotated token through) before the second refresh.
  - **`discover_peer` not `safe_http`-guarded.** The refresh discovery GET + token POST use the shared `state.http` client, not `util::safe_http`. Consistent with the existing peer MCP fan-out (also unguarded against the trusted, registration-validated `mcp_url`), so no regression — but harden both together if/when the fan-out adopts `safe_http`.

## P3 Code Quality

- Rename or de-prefix `tests/phase13_demo.rs` and `tests/phase13_connectors.rs` when broader test-suite naming churn is acceptable.

## P3 UX

- Move hardcoded status badge colors into reusable CSS tokens.
- Finish auth-surface format hints and ARIA polish.
