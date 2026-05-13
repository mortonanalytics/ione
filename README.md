# IONe

**Integration fabric for federated, AI-native operational apps. The on-prem MCP substrate underneath domain-specific decision-support apps.**

IONe is a single Rust binary + static UI that federates a heterogeneous portfolio of operational apps into one workspace for an operator. Each connected app stays opinionated about its own data, compute, storage, and frontend; IONe handles federation over MCP, brokered identity, signed push event ingress, a generator↔adversarial LLM loop that gates app actions through human approval with audit trail, and a thin UX shell that renders refs (maps, tables, charts, documents) the apps provide.

> Status: **pre-alpha**. v0.1.0 is a reference implementation of the federation thesis under the older chat-first framing; current architectural direction is captured in [md/design/ione-substrate.md](md/design/ione-substrate.md). Expect breakage.

## What it's for

Organizations that run operational decision-support workflows across many specialized apps, with a chain-of-command that needs information up and commands down. The reference deployments pair IONe with one or more domain apps: GroundPulse for infrastructure risk, TerraYield for crop-health intelligence, bearingLineDash for financial analytics. The same substrate serves them all — domain-specific data and compute live inside each app; IONe brokers identity, federates over MCP, gates actions through approval, and presents the unified pane of glass.

## Thesis

The canonical reference is [md/design/ione-substrate.md](md/design/ione-substrate.md) (integration-fabric framing, 2026-05-12). The earlier chat-first design [md/design/ione-v1.md](md/design/ione-v1.md) and product-completion design [md/design/ione-complete.md](md/design/ione-complete.md) are preserved as historical context for what shipped on `main`. Short version of the current thesis:

1. **IONe is integration fabric, not a hosting platform.** Apps stay independent; IONe federates over MCP + brokered identity + approval/audit gateway + UX shell.
2. **Any MCP-speaking app is a peer.** The contract apps satisfy lives at [md/design/app-integration-playbook.md](md/design/app-integration-playbook.md): MCP server + OAuth 2.1 + signed webhooks + view-hint resource metadata + foreign-tenant `whoami` + role declaration.
3. **Identity is brokered.** IONe consumes one identity (OIDC, SAML 2.0 SP, MFA) and holds delegated tokens per app per user. Apps trust IONe-issued OAuth credentials.
4. **State-changing app actions go through IONe's approval gateway.** Apps declare which tools require human-in-the-loop. IONe routes the operator's intent through a generator↔adversarial LLM survivor chain, then through approval, then invokes the app. Audit on every action.
5. **The UX shell renders refs.** Apps own their tile servers, raster stores, report generators, time-series DBs. IONe embeds tile URLs, MIME-typed documents, and view-hinted resources from MCP metadata. No data is hosted in IONe that belongs to an app.
6. **Chat is one surface, not the engine.** The engine is federation + identity broker + approval gateway. Chat, map, table, chart, document are surfaces over that engine.

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

`.env.example` sets `IONE_SEED_DEMO=1`, so after copying it to `.env` a fresh local install lands in the read-only `[Demo] IONe Ops` workspace automatically. The demo is populated and chat works offline through canned replies; switch to your real workspace when you are ready to connect live systems.

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
| `IONE_TOKEN_KEY` | required | 32-byte base64 or hex key for encrypting peer OAuth tokens |
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
