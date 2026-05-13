# Session Handoff - 2026-04-25

## Summary

Made the local IONe demo workspace usable out of the box and committed the fix batch as `0f25ff8 Make demo workspace usable out of the box`. The branch is `main`, clean, and ahead of `origin/main` by one commit. No pull request is currently associated with the branch.

No previous `.codex` or legacy handoff file was found, so this starts the handoff chain.

## Completed

- Documented and enabled demo-first local defaults in `.env.example` and `README.md`, including `IONE_SEED_DEMO=1` and `IONE_TOKEN_KEY`.
- Fixed activation fetch query naming in the static UI from `workspace_id` to `workspaceId`.
- Fixed hidden tab panel behavior and mobile layout issues in `static/style.css`.
- Fixed demo connector fixtures and stream names for Slack, IRWIN, NWS, FIRMS, and IRWIN seeded streams.
- Tightened Rust-native connector validation dispatch so prefixed/provider-hinted names resolve correctly.
- Added `IONE_SKIP_LIVE=1` syntax-only behavior for NWS validation.
- Adjusted connector creation so guided Rust-native validation does not incorrectly block non-Rust-native/OpenAPI connector creation.
- Updated regression tests for admin funnel gating, stream-event cascade expectations, MCP bearer middleware behavior, and phase 13 peer-demo auth/tool allowlist setup.
- Ran a live browser visual/usability pass on desktop and mobile, including tabs, dialogs, canned chat, and demo read-only write guard.
- Committed all source/test/doc changes.

## In Progress

- Nothing is actively in progress.
- Local `main` has not been pushed.
- No PR has been opened.

## Verification

Actually run and passed before commit:

```bash
cargo fmt -- --check
cargo clippy --all-targets -- -D warnings
cargo test
DATABASE_URL=postgres://ione:ione@localhost:5433/ione IONE_SKIP_LIVE=1 cargo test -- --ignored --test-threads=1
```

Live app verification also passed on `http://127.0.0.1:3002` before the server was stopped:

- Desktop `1280x900`: Chat, Connectors, Signals, Survivors, and Approvals each showed exactly one visible tab panel.
- Mobile `375x812`: no horizontal overflow and each tab showed exactly one visible panel.
- Add Connector, MCP setup, and peer federation dialogs were usable on mobile.
- Demo canned chat quick prompt returned the expected canned response.
- Demo connector creation attempt returned the expected read-only guard message: `Switch to your workspace to make changes.`

Not run after commit because the commit only captured the already-verified working tree.

## Open Questions

- Should `main` be pushed directly to `origin/main`, or should this become a PR despite being on `main`?
- Should `.codex/session-handoff.md` and `.codex/handoffs/2026-04-25.md` be committed so future checkouts can consume the handoff chain?

## Next Actions

1. Decide whether to push `0f25ff8` directly or open a PR.
2. Decide whether to commit the new `.codex` handoff files.
3. If preparing a release, run a fresh post-push smoke test from a clean checkout or CI job.
4. Continue with the next product-readiness pass: authentication/session persistence defaults, first-run setup messaging, and deployment documentation.

## References

- Repo: `/Users/ryanemorton/Documents/GitHub/ione`
- Branch: `main`
- HEAD: `0f25ff8 Make demo workspace usable out of the box`
- Upstream: `origin/main` at `62a6130 Add OAuth and peer regression coverage`
- PR state: no associated PR; no open PRs reported by `gh pr status`
- Key files: `.env.example`, `README.md`, `static/app.js`, `static/style.css`, `src/connectors/validate/mod.rs`, `src/connectors/validate/nws.rs`, `src/demo/fixture.rs`, `src/demo/seeder.rs`, `src/routes/connectors.rs`, `tests/phase11_mcp_server.rs`, `tests/phase13_demo.rs`
