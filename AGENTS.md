# IONe Codex guidance

IONe is active federated geospatial infrastructure. Read `CLAUDE.md` for current architecture and data contracts. Preserve node and tenant boundaries, audit events, auth policy, migration safety, and the static UI/API contract.

Use cheap Rust tests first. Persistence and federation claims require migrated Postgres and the ignored integration suite run serially; browser behavior requires the Playwright flow. Changes shared with TerraYield require matching issue-level interface contracts in both repositories before modifying either side.

Backlog automation is autonomous through merge. It accepts `backlog-ready` from Ryan or from the `auto-backlog-ready` workflow, never from an issue carrying `needs-human-auth` (Ryan's manual hold). It resumes an existing `automation-pr` before starting new work, repairs a red `main` before layering on it, takes one issue per run in an isolated worktree, opens one verified PR, drives CI green, and squash-merges. Green CI is the only merge authority: never merge on a red or pending check and never admin-override one. Stop before any deployment, release, or version bump.

Scheduled scouting and triage are read-only on code. They file `research-candidate` and `bug-candidate` issues, which promote to `backlog-ready` automatically — so the issue body is the specification the builder ships against, and a bug is never marked a candidate without a real reproduction. No agent applies `needs-human-auth` to its own work. No agent writes a secret value into an issue, comment, commit, or PR.
