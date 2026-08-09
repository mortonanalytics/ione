---
name: ione-backlog
description: Carry one IONe issue from backlog-ready to merged — failing test, smallest safe change, verified PR, green CI, squash merge. Use for the scheduled IONe backlog loop or an explicitly selected backlog-ready issue.
---

# IONe backlog

This loop merges. No human reads the diff before it lands on `main`, so the verification below is the only thing standing between a bad change and the default branch. Treat a gate you cannot run as a reason to stop, never as a reason to proceed.

## Pick the run's work

Evaluate everything against remote state after `git fetch origin`.

1. **An open `automation-pr` already exists** — that is this run's work. Do not open a second one. Drive it to green and merge it, then stop. A prior run's stalled PR blocks the queue, and unblocking it matters more than starting something new.
2. **Otherwise** — take the oldest open issue carrying `backlog-ready` and not carrying `needs-human-auth`. `backlog-ready` counts as approval whether Ryan applied it or the `auto-backlog-ready` workflow promoted it from `bug-candidate` or `research-candidate`. `needs-human-auth` is Ryan's manual hold; nothing else applies it.
3. **`main` CI is red** — fixing `main` is the run's work. Do not layer a feature on a broken base.
4. **Nothing matches** — stop and say so. A no-op run is a fine outcome.

One issue per run. TerraYield federation changes require a matching `eo_ag` issue carrying the same versioned interface, auth, error, and compatibility contract; if it does not exist, stop and say so rather than changing one side of a wire contract alone.

## Build it

Write a failing contract test first, then the smallest safe change that passes it. Never edit an already-applied migration — a checksum break took down all 57 suites once, which is what issue #31 exists to prevent.

## Verify

Run the gates the change actually touches, and run them before the PR, not after:

- always: `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --lib --bins`, `cargo test --test phase01_chat`
- persistence or federation: `docker compose up -d postgres minio`, `cargo sqlx migrate run`, then each relevant suite as `DATABASE_URL=postgres://ione:ione@localhost:5433/ione IONE_SKIP_LIVE=1 cargo test --test <suite> -- --include-ignored --test-threads=1`
- static UI or browser contract: `npm run test:e2e`

If the runner cannot start Postgres, MinIO, or a browser, say which gates did not run and why. CI runs the full matrix on the PR — let it be the first place they execute, and treat its verdict as binding.

## Land it

Open one PR labeled `automation-pr` with `Closes #N`, provenance naming the scheduled run and date, the exact commands and their output, risks, and any gate that did not run locally.

Wait for CI. Fix what it turns red — this is your own PR. When every required check is green, `gh pr merge --squash --delete-branch`.

Do not merge on a red or pending check, do not merge past a gate you skipped, and do not use admin override to bypass a failing check. If CI stays red after a genuine attempt to fix it, leave the PR open with a comment explaining what is wrong and stop — the next run will resume it under rule 1.

Stop before any deployment, release, or version bump. Never write a secret value into a commit, PR body, or issue comment.
