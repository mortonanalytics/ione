---
name: ione-triage
description: Triage inbound IONe bug reports into reproducible, scoped bug-candidate issues, or close them out. Use for the scheduled IONe triage loop or a specific inbound report.
---

# IONe bug triage

Process open issues that carry `bug` and carry none of `bug-candidate`, `backlog-ready`, `automation-pr`, or `wontfix`. Oldest first. Cap a scheduled run at three issues.

For each, do the work the reporter could not:

1. **Reproduce.** Build a minimal case against current `main`. Use the cheap Rust path first, then migrated Postgres and the `#[ignore]`-gated suites serially when persistence or federation is involved, then Playwright for browser behavior. `docker compose up -d postgres minio`, `cargo sqlx migrate run`.
2. **Locate.** Cite the responsible code as `file:line`. A stack trace is not a location.
3. **Scope.** State the smallest change that fixes it and what it could break.

Then take exactly one action:

- **Reproduced** — comment with the repro command and its actual output, the `file:line` cause, the proposed fix, and a failing-test name the builder should write first. Apply `bug-candidate`. That label promotes to `backlog-ready` automatically, so do not apply `bug-candidate` to anything you did not personally reproduce.
- **Not reproducible** — comment with exactly what you ran, on what commit, and what you saw instead. Ask the reporter for the missing piece. Change no labels.
- **Already fixed** — verify against the remote (`git fetch origin`, then `git merge-base --is-ancestor <sha> origin/main` or `gh pr view <n> --json state` reporting `MERGED`), comment with that evidence, and close. A commit in bare `git log` is not evidence.
- **Not a bug** — comment with the contract or design doc that defines the current behavior and say so. Change no labels.

Anything you mark `bug-candidate` gets fixed and merged without a human reading it first. Your repro is the specification. That is why reproducing is not optional and why "looks wrong in the code" is never enough.

Never guess at a repro, never mark a report reproduced on code reading alone, never modify code, never open a PR, and never put secret values, tokens, or raw credential material in a comment. Fixing is the builder's job.
