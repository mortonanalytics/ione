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

Apply `needs-human-auth` alongside `research-candidate` when the work would touch production secrets, delete or rewrite data, change auth or RLS policy, alter a federation wire contract shared with `eo_ag`, or add a dependency. That label holds the issue out of the automatic promote-to-`backlog-ready` path until Ryan clears it.

Never modify code, open a PR, bump a version, or start a release.
