---
name: ione-backlog
description: Implement one approved IONe issue with Postgres-backed verification and an unmerged PR, then drive its CI to green. Use for the scheduled IONe backlog loop or an explicitly selected backlog-ready issue.
---

# IONe backlog

Require `backlog-ready`, non-red `main`, no open `automation-pr`, and an isolated worktree. `backlog-ready` is applied either by Ryan or by the `auto-backlog-ready` workflow promoting a `bug-candidate` or `research-candidate`; both count as approval. An issue carrying `needs-human-auth` does not, whatever else it carries. Evaluate every gate against remote state after `git fetch origin`.

Process one issue. TerraYield federation changes require a matching `eo_ag` issue with the same versioned interface, auth, error, and compatibility contract.

Add a failing contract test, implement the smallest safe change, run focused Rust tests, then migrated Postgres-backed ignored tests serially when persistence or federation is involved. Run Playwright for static UI or browser contracts.

If the runner cannot bring up Postgres, MinIO, or a browser, do not silently skip — say in the PR body exactly which gates did not run and why, and let CI be the first place they execute.

Open one PR labeled `automation-pr` with provenance, `Closes #N`, exact commands and their output, risks, and unrun gates. Then wait for CI on that PR and fix what it turns red — this is your own PR, so drive it to green rather than leaving a red branch behind. Stop before merge or deployment.
