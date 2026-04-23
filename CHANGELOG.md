# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased] — complete-product work

### Added

- Demo Workspace with `IONE_SEED_DEMO` seeder + canned-chat layer.
- Ollama preflight (`GET /api/v1/health/ollama`) + chat remediation card.
- Guided connector setup: 5 per-provider validators + `POST /connectors/validate`.
- Pipeline events: table, broadcast bus, scheduler stage emission, list endpoint, SSE stream (`/events/stream`).
- Split activation tracking (demo walkthrough + real activation) with CTA handoff.
- Funnel telemetry: `funnel_events` table, session cookie middleware, `POST /telemetry/events`, `GET /admin/funnel`.
- MCP OAuth 2.1: discovery, register, authorize, token, revoke; bearer middleware on `/mcp/*`; CIMD advertised.
- Peer federation: OAuth client, well-known client metadata, federation routes, allowlist enforcement.
- Connect-to-MCP UI panel with per-client tiles (Cursor, Claude Desktop, Claude Code, VS Code, raw JSON).
- Connected MCP clients panel with revoke.
- Demo UI: chat chips, lock glyph, `Sample —` connector prefix, 403 toast.
- Activation tracker UI with demo→real CTA.
- Connector timeline UI + post-create progress view via SSE.
- Guided connector wizard UI (provider grid + Test connection).

### Changed

- `AppError` envelope enforced to `{ error, message, hint?, field?, ... }`.
- Connector create now runs validate inline (422 on failure) and emits publish_started + first_event synchronously.
- Demo-workspace writes return 403 `demo_read_only`; demo-conversation messages return canned replies.

### Migrations

- 0012_pipeline_events
- 0013_activation
- 0014_funnel_events
- 0015_oauth
- 0016_peers_oauth

## [0.1.0] — 2026-04-20

First public release.

### Added

- **Phase 1** — Rust axum backend + vanilla HTML/JS chat UI + Ollama proxy at `POST /api/v1/chat`.
- **Phase 2** — Postgres + pgvector via docker-compose. Persistent conversation history; `organizations`, `users`, `conversations`, `messages`; default org + user bootstrap.
- **Phase 3** — Workspaces + roles + memberships as the generic persistent primitive. `Operations` workspace seeded at first boot with a `member` role and the default user's membership. Conversations become workspace-scoped; UI gains a workspace switcher.
- **Phase 4** — Hybrid connector fabric (MCP · OpenAPI · hand-written Rust). First connector: NOAA NWS via `api.weather.gov`. Idempotent polls via `(stream_id, observed_at)` unique index.
- **Phase 5** — Rules floor (evalexpr-based) + generator LLM pass → signals. Background scheduler polls connectors, evaluates workspace.metadata.rules, runs the generator (`qwen3:14b` default) every tick. Signals tab in the UI.
- **Phase 6** — Adversarial critic (`phi4-reasoning:14b` default) → survivors. Every signal passes through the critic; verdict + confidence + chain-of-reasoning captured as the audit trail. Survivors tab with expandable reasoning.
- **Phase 7** — Routing classifier (`qwen3:8b` default) turns survivors into routing decisions with targets `feed | notification | draft | peer`. Role-scoped feed endpoint; routing-chip UI; severity fallback when classifier is unavailable.
- **Phase 8** — OIDC / SAML auth with pluggable IdP. `IONE_AUTH_MODE=local` keeps air-gap deployments working; `oidc` mode federates via any trusted issuer (Keycloak default in docker-compose). Per-trust-issuer claim mapping builds local memberships from federated claims.
- **Phase 9** — Delivery (Slack webhook + SMTP), artifacts, approvals queue, audit log. Commands-down is now live: notifications fire immediately; drafts become approvals that a user decides from the UI; every send writes an audit row.
- **Phase 10** — Rule-authorized auto-execution. Workspace-declared policies can bypass the approval step for narrow routine commands; `severity_cap` prevents command-level signals from auto-executing regardless of policy; per-policy token-bucket rate limit.
- **Phase 11** — IONe-as-MCP-server. Hand-rolled JSON-RPC 2.0 + SSE subset (~550 LoC). Five tools: `list_workspaces`, `list_survivors`, `search_stream_events`, `propose_artifact`, `deliver_notification`. Accepts signed session cookies or bearer JWTs from trusted issuers.
- **Phase 12** — MCP client + peer federation. Register a peer IONe; subscribe a workspace to it; the peer becomes a pullable stream and a push target. Sharing-policy enforcement per peer edge. Two IONes now talk end-to-end over the same MCP protocol.
- **Phase 13** — FIRMS + filesystem/S3 + IRWIN-read connectors. Fire-ops-flavored fixture data under `infra/fixtures/` for offline demos. `scripts/demo.sh` and `tests/phase13_demo.rs` exercise the full federation loop end-to-end.
- **Phase 14** — OSS release: Apache 2.0 license, README, CONTRIBUTING, CODE_OF_CONDUCT, GitHub Actions CI.

### Test counts

139 integration tests across 13 phases (all `#[ignore]`-gated; 1 Ollama-gated in Phase 1; the rest run under `IONE_SKIP_LIVE=1` by default).

### Known limitations

- Ollama is the only LLM surface wired; OpenAI-compatible endpoints are stubbed but not fully exercised.
- Vector search (pgvector) plumbing exists on `stream_events.embedding` but isn't populated yet.
- MCP uses a hand-rolled subset; the `rmcp` crate is on the v0.4 swap list.
- Policy editor UI for auto_exec + sharing policies is deferred to v0.2; configure via SQL for now.
- Streaming responses (SSE for chat + for signals) deferred to v0.2.

[0.1.0]: https://github.com/mortonanalytics/ione/releases/tag/v0.1.0
