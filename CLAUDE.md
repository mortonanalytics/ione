# CLAUDE.md — IONe

## What this is
IONe is a **pre-alpha** single Rust binary (axum + tokio + sqlx/Postgres 16 +
pgvector) with a static UI (`static/`), serving as the MCP-based federation
substrate underneath GroundPulse and TerraYield: brokered identity (OIDC/SAML),
signed webhook ingress, generator↔adversarial LLM loop with human approval +
audit, and a ref-rendering UX shell. It is **OSS engineering infrastructure,
not a standalone product** — no standalone GTM.

Canonical design: `md/design/ione-substrate.md` (integration-fabric framing).
Strategy context: `.claude/rules/path-2-stream-p.md`.
App-integration contract: `md/design/app-integration-playbook.md`.

## Quickstart
```bash
cp .env.example .env          # sets IONE_SEED_DEMO=1 (demo workspace)
docker compose up -d postgres minio
cargo sqlx database create
cargo sqlx migrate run
cargo run --release           # http://localhost:3000
```
Two-node federation demo: `./scripts/demo.sh` (nodes on :3000 and :3001).

## Tests
```bash
# Cheap unit path:
cargo test --test phase01_chat

# Integration: live Postgres, serial, #[ignore]-gated:
DATABASE_URL=postgres://ione:ione@localhost:5433/ione \
  IONE_SKIP_LIVE=1 \
  cargo test -- --ignored --test-threads=1

# Playwright e2e:
npm run test:e2e
```
Unset `IONE_SKIP_LIVE` to exercise live Ollama generator/critic/router paths.

## Automation loops
Three scheduled cloud agents, each driven by a skill in `.agents/skills/`
(symlinked into `.claude/skills/`):

| Skill | Cadence | Writes |
|---|---|---|
| `ione-scout` | Mon, Thu | `research-candidate` issues — OLAP, OLTP, data connections, federated data, edge, app support |
| `ione-triage` | Mon, Wed, Fri | reproduces inbound `bug` reports into `bug-candidate` issues |
| `ione-backlog` | Mon, Wed, Fri | one `backlog-ready` issue → `automation-pr` → green CI → squash merge |

The loop is autonomous end to end. `auto-backlog-ready.yml` promotes
`bug-candidate` and `research-candidate` to `backlog-ready`, and the builder
merges its own PR once CI is green. Nothing waits on a human.

Green CI is the only merge authority — no merging on red or pending checks, no
admin override, and no deploy, release, or version bump from any loop. The
issue body is the specification, since nobody reads it before the change lands.

Ryan steers by filing issues. `needs-human-auth` on an issue holds it out of the
loop; no agent applies that label to its own work.

## Secrets
`IONE_TOKEN_KEY` / `IONE_WEBHOOK_SECRET_KEY` live in `.env` (gitignored).
Never commit `.env` or embed key values in settings/permissions entries.
