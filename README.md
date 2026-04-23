# IONe

**An input-output network of federated, AI-native operational decision-support nodes.**

IONe is a single Rust binary + static UI that turns any collection of API-reachable systems (MCP servers, OpenAPI endpoints, hand-wired adapters) into a chat-first operational workspace. Each deployment is one node. Nodes federate peer-to-peer over MCP. A built-in generator↔adversarial LLM loop surfaces insights with an audit trail; a routing classifier decides what flows where; commands-down write-backs let IONe act, not just observe.

> Status: **pre-alpha**. v0.1.0 is a reference implementation of the federated-nodes thesis. Expect breakage.

## What it's for

Organizations that run operational decision-support workflows across many data sources, with a chain-of-command that needs information up and commands down: USFS fire operations, FEMA emergency services, enterprise ops centers, and startups stitching novel cross-domain data into new products. The same substrate serves all of them — domain-specific curation lives inside each node's data; the platform doesn't care about the domain.

## Thesis

See [md/design/ione-v1.md](md/design/ione-v1.md) and [md/strategy/market/ione-chat-first-data-ias.md](md/strategy/market/ione-chat-first-data-ias.md) for the full design and market context. Short version:

1. **Workspace is one primitive.** No design-time incident/ops split — lifecycle is a declarative property.
2. **Any API-reachable system is a node.** Connectors are a hybrid: MCP primary, OpenAPI auto-adapter, hand-written Rust for top-priority integrations.
3. **Nodes federate as peers.** Each node exposes itself as an MCP server; the same protocol does internal integrations and inter-node peering.
4. **Insights are gen↔adversarial.** A generator LLM proposes; a critic model stress-tests; only survivors advance. The survivor's chain-of-reasoning is the audit trail.
5. **Routing is classified, not coded.** An LLM classifier decides where each survivor flows (feed / notification / draft / peer). Topology is runtime.
6. **Chat is the demo surface, not the engine.** The engine is connectors + workspace + gen↔adversarial + classified routing.

## Quickstart

Prerequisites: Docker, Rust (1.78+), and a local Ollama install (`ollama pull llama3.2:latest qwen3:14b phi4-reasoning:14b qwen3:8b nomic-embed-text`).

```bash
git clone <this-repo> ione && cd ione
cp .env.example .env
docker compose up -d postgres minio
cargo sqlx database create
cargo sqlx migrate run
cargo run --release
# open http://localhost:3000
```

`.env.example` and `docker-compose.yml` set `IONE_SEED_DEMO=1`, so a fresh local install lands in the read-only `[Demo] IONe Ops` workspace automatically. The demo is populated and chat works offline through canned replies; switch to your real workspace when you are ready to connect live systems.

The UI ships with Chat, Connectors, Signals, Survivors, and Approvals tabs. Create a workspace, register an NWS connector with your lat/lon, poll it, watch rule + generator signals land, watch the critic rank them, watch the classifier route them.

### Two-node federation demo

```bash
./scripts/demo.sh
# brings up two IONe processes on :3000 and :3001,
# wires a peer relationship, and prints the audit trail of
# survivors flowing from Node A to Node B.
```

## Architecture

```
 ┌─ Connector fabric ─────────────────────────────────────┐
 │  MCP servers  ·  OpenAPI adapters  ·  hand-wired Rust   │
 └────────────────┬────────────────────────────────────────┘
                  │
                  ▼
 ┌─ Workspace (generic persistent container) ─────────────┐
 │  streams → events → (rules + LLM generator) → signals   │
 │                                        │                │
 │                                        ▼                │
 │                          (adversarial critic) → survivors│
 │                                        │                │
 │                                        ▼                │
 │                        (routing classifier) → decisions │
 │                                        │                │
 │       ┌──────────────┬─────────────────┼─────────────┐  │
 │       ▼              ▼                 ▼             ▼  │
 │     feed       notification         draft          peer │
 │   (role-     (connector send —    (approval →   (federate │
 │   scoped      Slack, SMTP, ...)    deliver)     to another│
 │   inbox)                                          IONe)   │
 └─────────────────────────────────────────────────────────┘
```

## Tech

- **Rust** (axum + tokio) — single binary
- **Postgres 16** + **pgvector** — primary store + embeddings
- **S3 / MinIO** — blob store (documents, imagery)
- **Ollama** — local-first LLM (generator `qwen3:14b`, critic `phi4-reasoning:14b`, router `qwen3:8b`; all configurable)
- **MCP** — hand-rolled JSON-RPC 2.0 + SSE subset; both as server (`/mcp`) with OAuth 2.1 + PKCE + CIMD, and as client (consuming peer nodes). Connect from Claude Desktop (Pro/Max), Claude Code, Cursor, or VS Code via the in-app "Connect to MCP" panel.
- **OIDC / SAML** — per-node local CoC + federated claims layered on top; Keycloak default IdP in docker-compose; PIV/CAC-capable for federal deployments

## What's in this release

- **Demo Workspace** (`IONE_SEED_DEMO=1`) — first-run is populated, and chat works offline through the canned layer.
- **Ollama preflight + chat remediation** — health dot in the top bar; remediation card with `pullCommand` when models are missing or Ollama is unreachable.
- **Guided connector setup** — provider-specific forms, `POST /api/v1/connectors/validate` dry-runs, and inline hints before create.
- **Publish-don't-poll** — scheduler emits `pipeline_events` per stage; SSE stream at `/api/v1/workspaces/:id/events/stream`; connector cards show a live timeline.
- **Split activation** — separate demo walkthrough and real activation trackers; demo completion shows a CTA to create a real workspace.
- **Funnel telemetry** — `funnel_events` table; `POST /api/v1/telemetry/events` plus `GET /api/v1/admin/funnel` gated on `IONE_ADMIN_FUNNEL=1`.
- **MCP OAuth 2.1** — discovery, register, authorize, token, and revoke at `/mcp/oauth/*`; bearer middleware on `/mcp/*`; per-client tiles in the Connect-to-MCP panel.
- **Peer federation** — OAuth-based federation with tool allowlist; `POST /api/v1/peers`, `GET /api/v1/peers/:id/manifest`, and `POST /api/v1/peers/:id/authorize`.

## Running tests

All integration tests are `#[ignore]`-gated and run serially against a live Postgres:

```bash
# Cheap unit path:
cargo test --test phase01_chat

# Integration, live DB, Ollama-gated where applicable:
DATABASE_URL=postgres://ione:ione@localhost:5433/ione \
  IONE_SKIP_LIVE=1 \
  cargo test -- --ignored --test-threads=1
```

Unset `IONE_SKIP_LIVE` to exercise the live Ollama generator/critic/router paths against the models above.

## Key env vars

| Var | Default | Purpose |
|---|---|---|
| `IONE_BIND` | `0.0.0.0:3000` | Server address |
| `DATABASE_URL` | `postgres://ione:ione@localhost:5433/ione` | Postgres |
| `OLLAMA_BASE_URL` | `http://localhost:11434` | Ollama HTTP |
| `OLLAMA_MODEL` | `llama3.2:latest` | Chat default |
| `OLLAMA_GENERATOR_MODEL` | `qwen3:14b` | Signal generator |
| `OLLAMA_CRITIC_MODEL` | `phi4-reasoning:14b` | Adversarial critic |
| `OLLAMA_ROUTER_MODEL` | `qwen3:8b` | Routing classifier |
| `IONE_SEED_DEMO` | `0` prod, `1` in `.env.example` / docker-compose | Seeds the demo workspace |
| `IONE_POLL_INTERVAL_SECS` | `60` | Scheduler tick |
| `IONE_AUTH_MODE` | `local` | `local` or `oidc` |
| `IONE_OAUTH_ISSUER` | `http://{IONE_BIND}` | Absolute issuer URL used in the OAuth discovery document |
| `IONE_OAUTH_STATIC_BEARER` | unset | CI/headless escape hatch for `/mcp/*` |
| `IONE_ADMIN_FUNNEL` | unset | Gates `/api/v1/admin/funnel`; returns 404 when unset |
| `IONE_SKIP_LIVE` | unset | Skip external network / Ollama calls in tests |
| `IONE_HTTP_UA` | `IONe/0.1 …` | User-Agent for outbound fetches |
| `IONE_STATIC_DIR` | `./static` | Static UI assets path |
| `IONE_SESSION_SECRET` | random | HS-signed session cookie key (set in prod) |
| `IONE_SMTP_TEST_MODE` | unset | `1` short-circuits SMTP to in-memory capture (tests) |

## Roadmap (not a promise)

- **v0.2**: streaming chat (SSE), workspace-scoped conversation listing, token accounting, per-workspace RBAC enforcement beyond routing scope.
- **v0.3**: pgvector-backed semantic search on stream_events, policy-editor UI for auto_exec + sharing policies.
- **v0.4**: rmcp crate swap once its axum integration stabilizes; full MCP resources + prompts.
- **v0.5+**: hosted tier (only if three unsolicited asks arrive and there's a third hire — see [md/strategy/market/ione-pricing.md](md/strategy/market/ione-pricing.md)).

## Background

IONe descends directly from Morton Analytics' 2024 DIA NeedipeDIA submission ([docs/ione_wp.pdf](docs/ione_wp.pdf)) — originally conceived as an "input-output network of micro-services" for analyst-driven data engineering + ML + analytics. v0.1 is the AI-native rebuild: same thesis, with chat + gen↔adversarial loop + MCP federation as the three new ingredients.

## License

Apache 2.0. See [LICENSE](LICENSE).

## Contact

Morton Analytics LLC · [morton@myma.us](mailto:morton@myma.us)
