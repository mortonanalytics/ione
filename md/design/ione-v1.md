# IONe v1 — Design Doc

**Date:** 2026-04-19
**Status:** Working doc — captures decisions from product-definition session. Lives until v0.1 ships; then becomes historical reference.
**Lineage:** Directly descended from the 2024 IONe whitepaper (`docs/ione_wp.pdf`). Same thesis — "input-output network" of analyst-driven services — now reconstituted as an AI-native, federated, chat-first product.

## Problem

Organizations that run operational decision-support workflows (incident response, continuous ops, multi-source situational awareness) stitch together ingest, transformation, ML, BI, messaging, and task-dispatch across 5–15 tools. Morton Analytics already deploys a services-shaped version of this integrated stack for multiple clients (USDA-NASS production, USFS fire-ops prospective, FEMA / enterprise / startup patterns). The gap: every deployment is a rebuild. The two new ingredients needed to productize without rebuilding are (1) an **AI/chat interface** that unifies user interaction and (2) **inter-network communication** that lets client-specific deployments federate cleanly.

## Users

One buyer archetype, several instances:

- Federal operational orgs with CoC doctrine (USFS fire ops, FEMA emergency services, adjacent statistical agencies)
- Enterprise operations with many concurrent data streams
- Startups stitching novel cross-domain data to produce new products

Common shape: multi-source ingest, CoC-structured routing (info up, commands down), decision-support artifacts, domain experts curating their own data.

## Product thesis

1. **Workspace is one primitive.** No design-time distinction between "incident" and "ongoing ops" — the lifecycle is a declarative property. Fire season is a workspace; each fire is a (sub-)workspace. Same model.
2. **Any API-reachable system is a node.** Bidirectional. The connector fabric is hybrid — **MCP** primary (2025 standard), **OpenAPI/Swagger** auto-adapter for non-MCP systems, **hand-written Rust** adapters for top-priority integrations where control matters.
3. **IONes federate as peers.** Each IONe is domain-specific and curated by domain experts. **IONe-as-an-MCP-server is the federation wire protocol** — the same protocol does internal connectors and inter-IONe peering.
4. **Signals are detected via a hybrid loop**: deterministic rules as the floor (auditable must-surface), connector-emitted events as opportunistic middle, and a **gen↔adversarial LLM loop** as the overlay. Generator proposes insights from the stream; critic/red-team (a separate reasoning model) stress-tests each; only survivors advance. The survivor's chain-of-reasoning is the audit trail.
5. **Routing is classified, not coded.** An LLM classifier inspects each survivor and decides *how and where* it travels: local CoC role scope, peer IONe, severity class, redaction, sharing policy. Topology is a runtime property, not a design-time pick — one architecture serves same-domain neighbors, cross-level hierarchies, and cross-domain federations without reshaping.
6. **Output has three modes, severity-gated**: routine → role-scoped feed (pull); flagged → directed notification (push, over whichever delivery connectors are configured); command-level → human-approved draft (gated). The commands-down direction is in v1 — IONe writes back through connectors, both human-approved and rule-authorized auto-execution are supported.
7. **Chat is the user surface, not the engine.** Chat is for triggers, refinement, and conversational retrieval. The engine is the connector + workspace + gen↔adversarial loop + router.

## Architectural primitives

Named first-class concepts inside a single IONe:

| Primitive | Role |
|---|---|
| **Workspace** | Persistent container for an operational context. Generic lifecycle (optional end condition, sub-workspaces). Holds streams, facts, roles, artifacts, comms, decisions. |
| **Connector** | Adapter to an external system. One of: MCP server reference, OpenAPI spec, hand-written Rust adapter. Connectors both **read** (streams) and **write** (commands-down). |
| **Stream** | Time-ordered data feed from a connector into a workspace. |
| **Signal** | A candidate insight produced by generator, rule, or connector event. |
| **Survivor** | A signal that survived the adversarial critic pass. Carries its chain-of-reasoning. |
| **Role / CoC assignment** | A person's position in the workspace's chain of command. Local-to-IONe plus optional federated OIDC claim. |
| **Routing classification** | LLM-produced decision about where a survivor flows (local role-scope, peer IONe, severity, redaction). |
| **Artifact** | Generated output: briefing, map, report, draft notification, draft resource order. |
| **Audit record** | Immutable record of every signal → survivor → routing → output → write-back step. |

## Signal detection loop

```
Connector streams ──┬──▶ Rule-engine evaluator ──────────▶ Deterministic signals (guaranteed surface)
                    ├──▶ Connector-declared events ──────▶ Opportunistic signals
                    └──▶ Generator LLM ──▶ Candidate signals ──▶ Critic LLM (adversarial) ──▶ Survivors
                                                                        │
                                                                        ▼
                                                           Routing classifier LLM
                                                                        │
                                       ┌────────────────────────────────┼────────────────────────────────┐
                                       ▼                                ▼                                ▼
                                  Role-scoped feed           Directed notification          Human-approved draft
                                  (pull, local CoC)          (push, via connectors)         (commands-down gate)
```

### Model roles

- **Generator**: `qwen3:14b` or `gemma3:12b` (fast, broad recall). Configurable.
- **Critic**: `phi4-reasoning:14b` or `deepseek-r1:14b` (reasoning models, built for adversarial evaluation). Configurable.
- **Routing classifier**: smaller fast model (`qwen3:8b` or `llama3.2:3b`) — deterministic output schema, low latency.
- **Embeddings**: `nomic-embed-text` → pgvector for retrieval and dedup.
- All local via Ollama by default. Swappable to hosted inference via config.

## Federation

- Each IONe exposes an **MCP server** that peer IONes connect to — same protocol as for external integrations.
- **Identity**: per-IONe local CoC (each node stands alone) + federated **OIDC/SAML** claims layered on top. Keycloak as default IdP; pluggable for PIV/CAC (federal), enterprise SSO (SAML), or standalone (startups).
- **Trust anchor**: OIDC/SAML. Each IONe declares which issuers it trusts. CoC claims (role, level, workspace scope) are custom claims in the token; federated reads authorize against them.
- **Cross-network behavior**: routing classifier decides whether a survivor traverses; the sharing policy on the originating IONe decides whether it *may* traverse; the receiving IONe's CoC claims decide who sees it.

## Tech stack

- **Backend**: Rust (axum, tokio, tower, reqwest). Single binary + embedded static UI.
- **DB**: Postgres 16 + pgvector.
- **Blob**: S3 / MinIO (MinIO as default for self-host and air-gapped).
- **LLM**: Ollama (local-first), pluggable to any OpenAI-API-compatible endpoint.
- **MCP**: Rust MCP client + server — the server exposes IONe as a peer node.
- **Chat UI**: vanilla HTML + JS served by the same binary. Upgrade path to SvelteKit if the UI grows beyond the chat surface.
- **Auth**: OIDC client library (`openidconnect` crate). Keycloak in docker compose for local dev.
- **Observability**: `tracing` + JSON logs; optional OpenTelemetry.

## Packaging

- **Single binary** `ione` with the Rust control plane.
- **Static chat UI** served from the same binary under `/`.
- **`docker compose up`** clone-and-run install: IONe + Postgres + MinIO + Ollama + Keycloak.
- **Apache 2.0** license (chosen for federal adoption per the market brief).

## v0.1 scope

- **Two IONe nodes**, deliberately unconstrained relationship, federated via OIDC. Routing classifier picks where each signal flows.
- **Full gen↔adversarial loop live** on real connector streams.
- **Commands-down** supports both human-approved drafts and rule-authorized auto-execution.
- **Starter connectors (6)**:
  1. NOAA NWS weather (public API, via OpenAPI auto-adapter)
  2. NASA FIRMS hotspots (public, OpenAPI auto-adapter)
  3. Inter-IONe MCP (hand-written, core to the product)
  4. Outbound notifications — Slack webhook + SMTP (hand-written, minimum-viable commands-down)
  5. Filesystem + S3/MinIO document/imagery (hand-written, core to the product)
  6. IRWIN read (hand-written Rust — fire-ops credibility piece, custom auth)
- **Demo scenario**: USFS fire ops, two-forest federation (concrete but interchangeable — the architecture isn't shaped around it).
- **Distribution**: **first OSS release** to GitHub under Apache 2.0.

## Non-goals for v0.1

- Replacing any agency system of record (IRWIN / WFDSS / ROSS / IQCS). IONe reads; writes where APIs allow; never claims to be the authoritative record.
- Multi-tenant hosted SaaS. Each deployment is self-hosted. Hosted tier deferred per the pricing brief's gate (three unsolicited asks + third hire).
- ICS 209 / 215 / IAP generation as formal doctrinal outputs. v1 drafts; v2+ produces doctrinally-compliant forms.
- Visual node builder (n8n-style). Connectors are code-defined or spec-defined only.
- Structure-fire / urban IC workflow specifics (Tablet Command territory).

## Open questions (tracked, not blocking v0.1 start)

- Concrete rule-authorized auto-execution policy schema — how does an operator declare "auto-file spot weather when RH trend crosses X"?
- Sharing policy language for cross-IONe redaction — DSL, config schema, or GUI-first?
- First warm USFS champion — named contact still TBD. Without this, v0.1 demo is credible but not yet sold-through.
- Hosted Ollama vs customer-provided inference endpoint for small-client deployments without GPUs.

## Build sequencing (next)

The first shipping slice resumes the paused build: Rust axum backend + chat UI + Ollama proxy as the thinnest possible end-to-end loop. Subsequent slices add Postgres, workspace model, first connector (NWS), generator pass, then critic pass, then routing classifier, then federation/MCP, then commands-down write-backs.
