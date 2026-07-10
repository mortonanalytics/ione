---
name: ione-backlog
description: Implement one human-approved IONe federation or reliability issue with Postgres-backed verification and an unmerged PR. Use for the scheduled IONe backlog loop or an explicitly selected backlog-ready issue.
---

# IONe backlog

Require `backlog-ready`, non-red `main`, no open `automation-pr`, and an isolated worktree. Process one issue. TerraYield federation changes require a matching `eo_ag` issue with the same versioned interface, auth, error, and compatibility contract.

Add a failing contract test, implement the smallest safe change, run focused Rust tests, then migrated Postgres-backed ignored tests serially when persistence or federation is involved. Run Playwright for static UI or browser contracts. Open a PR labeled `automation-pr` with provenance, `Closes #N`, exact verification, risks, and unrun external gates. Stop before merge or deployment.
