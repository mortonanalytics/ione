# IONe Codex guidance

IONe is active federated geospatial infrastructure. Read `CLAUDE.md` for current architecture and data contracts. Preserve node and tenant boundaries, audit events, auth policy, migration safety, and the static UI/API contract.

Use cheap Rust tests first. Persistence and federation claims require migrated Postgres and the ignored integration suite run serially; browser behavior requires the Playwright flow. Changes shared with TerraYield require matching issue-level interface contracts in both repositories before modifying either side.

Backlog automation accepts only Ryan-applied `backlog-ready`, skips red `main` and an existing `automation-pr`, works on one issue in an isolated worktree, opens one verified PR, and stops before merge or deployment.
