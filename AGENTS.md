# IONe Codex guidance

IONe is active federated geospatial infrastructure. Read `CLAUDE.md` for current architecture and data contracts. Preserve node and tenant boundaries, audit events, auth policy, migration safety, and the static UI/API contract.

Use cheap Rust tests first. Persistence and federation claims require migrated Postgres and the ignored integration suite run serially; browser behavior requires the Playwright flow. Changes shared with TerraYield require matching issue-level interface contracts in both repositories before modifying either side.

Backlog automation accepts `backlog-ready` from Ryan or from the `auto-backlog-ready` workflow, never from an issue carrying `needs-human-auth`. It skips red `main` and an existing `automation-pr`, works on one issue in an isolated worktree, opens one verified PR, drives that PR's CI to green, and stops before merge or deployment.

Scheduled scouting and triage are read-only on code. They may file `research-candidate` and `bug-candidate` issues, and must apply `needs-human-auth` to anything touching production secrets, data deletion or rewrite, auth or RLS policy, a federation wire contract shared with `eo_ag`, or a new dependency.
