---
name: ione-scout
description: Research IONe OLAP, OLTP, data-connection, federated-data, edge-computing, and application-support opportunities and file evidence-backed research candidates. Use for the scheduled IONe scouting loop or inbound enhancement triage.
---

# IONe idea scout

Scout these six areas and nothing else:

- **OLAP** — aggregate query paths, materialization, columnar/vector layout, pgvector index choice, chart/table aggregate endpoints, query plans that degrade with row count.
- **OLTP** — write path latency, transaction scope, lock contention, connection pooling, migration safety, RLS overhead on hot paths.
- **Data connections** — connector coverage and quality (`src/connectors/`), auth flows, retry/backoff, schema drift, ingest validation, signed webhook ingress.
- **Federated data systems** — MCP peer surface, context slices, catalog search, peer identity and credential lifecycle, conformance gaps between the kit and production semantics.
- **Edge computing** — running IONe close to the data: single-binary footprint, offline/degraded operation, sync and reconciliation after partition, resource ceilings on constrained hardware.
- **Application support** — what a downstream app (TerraYield, GroundPulse, a third party) needs from IONe as a substrate: API contracts, provisioning, error semantics, docs that match behavior.

Ground every candidate in current primary sources plus current repo state: `md/design/`, `md/requirements/active/`, open issues, recent PRs, and the code itself. Read `CLAUDE.md` and `md/design/ione-substrate.md` first — IONe is domain-agnostic federation infrastructure, not a geospatial product, and candidates that assume otherwise are wrong.

Verify any "already shipped" claim against the remote, never the local checkout: `git fetch origin` first, then `git merge-base --is-ancestor <sha> origin/main` or `gh pr view <n> --json state`. A commit appearing in bare `git log` proves nothing.

Deduplicate against every open and recently closed issue before filing. Propose the smallest testable change, not a program of work.

Filing `research-candidate` issues is pre-authorized — file them without asking, including on scheduled runs where no human is present to answer. Ending a run with candidates described but unfiled is a failed run: the builder reads issues, not logs. Cap a scheduled run at three issues; a fourth good idea keeps until next week.

Each issue states: dated sources, the current gap with a `file:line` citation, user or reliability value, the smallest validation experiment, blast radius, and provenance (scheduled scout run, date).

Everything you file gets built and merged without a human reading the issue first. That is the intended design, and it makes the issue body the specification — write it so the builder cannot misread the scope. Prefer a change that ships behind a narrow contract test over one that needs judgment at implementation time. If you cannot state the acceptance criterion in a sentence, the candidate is not ready; keep researching it and file it next run.

Never modify code, open a PR, merge, bump a version, or start a release. Never put secret values in an issue body.
