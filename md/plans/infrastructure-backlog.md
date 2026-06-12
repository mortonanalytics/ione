# IONe Infrastructure Backlog

What it would take to move IONe from a v0.1 integration fabric to a more complete substrate for hosting real data apps. Prioritized. Items tagged **[Epicenter]** are needed by the seismic-monitor demo (`../../epicenter`), which is the current demand signal driving this list. Items tagged **[DICE]** position IONe for the DARPA DICE TA3 bid (`rfp/darpa-dice/abstract-final.md`; abstract due 2026-06-30, full proposal 2026-08-25) — they back specific claims in §2.4/§2.7 of the abstract.

Effort estimates are rough (solo-dev days). File refs point at where the work likely lands.

## Priority sequence — RFP need × commercial reuse

Ranked by dual fit: backs a DICE abstract claim *and* is product capability any enterprise deployment needs (not bid-only artifacts).

| # | Item | RFP need | Commercial reuse | Effort |
|---|------|----------|------------------|--------|
| 1 | Structured T&E event export (P6) | T&E observability surface — §2.4 monitoring claims | Audit analytics / compliance reporting / agent-ops monitoring for any org | ~1 wk |
| 2 | RBAC scaffolding (P4) | Committed as scoped Phase 1 extension | Enterprise table stakes; unblocks every multi-team deployment | ~3–5 d |
| 3 | Auto-exec policy DSL (P4) | Implicit: human-approval-only cannot scale to 500–100K-agent collectives | Removes the #1 friction for production use (approval fatigue) | ~3–4 d |
| 4 | Programmatic workspace/peer provisioning (P6) | "One workspace per mission" at scale; any scale demo | API-first onboarding, IaC, multi-env deploys | ~2–3 d |
| 5 | Cross-app semantic catalog + vector search (P3) | Backs the "surfaced upon relevance" bounded-context claim (§2.4) | Tool/resource discovery as peer count grows — core product value | ~1 wk |
| 6 | MCP routing throughput benchmark (P6) | §2.7 <5% overhead target needs measured baseline | Perf numbers for sales/marketing; cheap | ~2–3 d |
| 7 | Adaptor-contract spec (P3) | §2.3 claim de-risk | Protocol neutrality (A2A/ANP) widens market, but speculative pre-award | ~2–3 d |

Deadline-bound regardless of rank: **federation white paper** (P6, due with full proposal 2026-08-25). RFP-only, no commercial reuse — schedule it, don't trade it against the list above.

Single-axis items (commercial only — sequence after the above): SAML 2.0 SP, UI theming hooks, context-slice lazy expansion. The [Epicenter] approval-500 bug stays ad-hoc: demo-blocking and ~0.5 d, fix opportunistically.

---

## Shipped this cycle — branch `feature/event-layer-phases-2-3` (code-complete + tested; pending founder walkthrough + merge)

These travel together as one unit. Per the "shipped = founder walked through it" rule they are **Partial — pending walkthrough**, not closed.

| Item | Tier | Commits | Design |
|------|------|---------|--------|
| Live point/feature map layer from `stream_events` | P0 | `e0aa8bc` (+ event-layer phases 0–3) | `md/design/event-point-layer.md` |
| Chart panel — `ione_view:"chart"` (myIO) | P0 | `b22f0fa`, `2a40f42` | `md/design/chart-panel.md` |
| Table view — `ione_view:"table"` | P0 | `bcf01b3`, `7a235b7` | `md/design/table-view.md` |
| Generic `geojson_poll` / JSON-URL connector | P1 | `f0ff3e9`, `e239edb` | `md/design/geojson-poll-connector.md` |
| Windowed / grouped aggregates (`event-aggregates`) | P2 | `b22f0fa` | (folded into chart-panel design) |
| Rules-engine nested-field reach | P1 | verified-only (works as-is) | — see note below |
| Document/report view — `ione_view:"document"` | P0 | `bbbdf1a`, `c61c9a2` | `md/design/document-view.md` |

**P0 visualization is complete** — map ✓ chart ✓ table ✓ document ✓ (all pending founder walkthrough + merge). **P2 analytics** shipped too. Remaining work is breadth: P1 ingestion (MCP notifications), P3 federation, P4 identity, P5 UX — plus the `ux-security-audit-backlog.md` follow-ups.

---

## P0 — Visualization (the biggest gap; unlocks every data app)

IONe renders MapLibre tiles and nothing else today. No chart, table, or live-feature rendering. This is the wall every data app hits.

- ✅ **[Epicenter] Chart panel — `ione_view:"chart"` rendering myIO.** Shipped (`b22f0fa`, `2a40f42`). Dual data path (peer `vnd.ione.chart+json` resources + IONe `event-aggregates`); renders via `new window.myIOchart({config:{layers:[…]}})`. **The single-mapping `validate_spec` bug was confirmed absent in current myIO source** (`required_mappings` is an array for all 36 types) — no bypass needed; validation is a build-time node test against `../myIO/mcp/lib/validate.mjs`, not a runtime call. See `md/design/chart-panel.md`.

- ✅ **[Epicenter] Live point/feature map layer from `stream_events`.** Shipped (`e0aa8bc` + event-layer phases 0–3). `GET /workspaces/:id/event-layers` projects `stream_events` to GeoJSON via `view_config`; MapLibre circle layer. See `md/design/event-point-layer.md`.

- ✅ **Table view — `ione_view:"table"`.** Shipped (`bcf01b3`, `7a235b7`). Schema negotiation, server-side pagination/sort/filter (IONe), client-side (peer); semantic accessible `<table>`. See `md/design/table-view.md`.

- ✅ **Document/report view — `ione_view:"document"`.** Shipped (`bbbdf1a`, `c61c9a2`). Peer-resource-only; inline-embeds `application/pdf` in a sandboxed iframe (`allow-downloads allow-same-origin`, never `allow-scripts`), other MIME types link out; https-only `download_url` validation, `nosniff` middleware, no proxy. App-wide CSP deferred to `ux-security-audit-backlog.md`. See `md/design/document-view.md`.

---

## P1 — Ingestion

- ✅ **[Epicenter] Generic `geojson_poll` / JSON-URL connector.** Shipped (`f0ff3e9`, `e239edb`). Config-driven poll → JSON-pointer field map → dedup key (natural-key upsert) → type filter → `stream_events`; epoch-ms timestamp support; hardened SSRF guard (link-local blocked all schemes). See `md/design/geojson-poll-connector.md`.

- ✅ **MCP `notifications/*` reception.** Shipped with the long-lived peer sessions (Slice D/E, merged PR #10). The session SSE read loop (`peer_session.rs`) dispatches inbound notifications via `federation::dispatch_notification` → `dispatch_domain_notification` → `webhook_ingress::ingest_webhook_event` → `stream_events`, with audit logging and foreign-tenant resolution. Hardening follow-up (`feature/mcp-notifications-ingress`): wired the previously-unused per-peer notification rate limiter (`IONE_PEER_NOTIFICATIONS_PER_MIN`), breaker half-open single-probe, and `mcp_sessions` idle TTL eviction. SSRF re-validation on reconnect is covered by the url-guarded `state.http` client (per-request guard). H-2 post-LLM allowlist remains deferred (moot until Slice B wired to chat).

- ✅ **[Epicenter] Rules-engine nested-field reach — verified, no code change.** `populate_context` (`src/services/rules.rs`) already recurses objects at arbitrary depth, so a rule `payload.properties.mag >= 6.0` resolves today. Note: rules use **dotted evalexpr** keys (`payload.properties.mag`), NOT the `[/json/pointer]` syntax this item's premise assumed — array indices are not reachable (arrays unmapped), which the M≥6.0 rule doesn't need. _Small open follow-up:_ author the M≥6.0 integration test + correct the playbook's pointer-syntax wording (trivial; not yet done).

---

## P2 — Analytics primitives

- ✅ **[Epicenter] Windowed / grouped aggregates.** Shipped as `GET /workspaces/:id/event-aggregates` (`b22f0fa`): count-per-bucket, avg/min/max/sum, percentile, group-by, 30-day rolling baseline; numeric-aware JSONB extraction, bucket allow-list (injection guard), org-scoped. Backs the chart panel's IONe data path.

---

## P3 — Federation maturity (from `md/design/`)

- ✅ **Tool namespacing in the federation hub.** Shipped — per-peer `prefix:tool` namespacing with duplicate detection (`src/services/federation.rs:78`). This item was stale; the DICE abstract correctly cites it as shipped.
- **[DICE] Protocol-neutral adaptor-contract spec.** Publish the §2.3 contract as a spec doc (message envelope w/ sender, mission scope, payload, signature; agent registry with capability declarations; observation hooks; failure-injection API; scoring-event schema) and add capability declarations to the existing peer registry. DICE funds the A2A/ANP bindings; the spec + registry field pre-award de-risks the claim cheaply. Effort: ~2–3 d.
- **Context-slice lazy expansion (`slice://`).** Contract is published (apps ship slices) but IONe-side routing/expansion isn't built. Effort: ~3 d.
- **Cross-app semantic catalog + vector search** over peer resources/tool descriptions (pgvector already present). Effort: ~1 wk.

---

## P4 — Identity & governance

- **[DICE] RBAC scaffolding (Admin service seed).** The abstract commits RBAC as "a scoped DICE Phase 1 extension" and Fig 3 names an Admin/RBAC service. Build a minimal role model (roles, role→tool/workspace grants, enforcement at the router) so the claim has a demonstrable trajectory before full proposal. Effort: ~3–5 d.
- **SAML 2.0 SP** for enterprise SSO (Keycloak bridges SAML→OIDC for now). Deferred from v0.1. Effort: ~3–5 d.
- **Auto-exec policy DSL.** Today: human-approval only. Add conditional auto-execution policies for low-risk tools. Effort: ~3–4 d.
- **Audit the auto-exec bypass guard.** Confirm the router's force-to-draft on `approval_required` (`src/services/router.rs`) is not bypassable. Effort: ~0.5 d review.

- **[Epicenter] Approving a draft/notification artifact 500s.** Found in the 2026-06-10 Epicenter live-docker walkthrough (`../../epicenter/md/verification/epicenter-walkthrough.md`). `POST /api/v1/approvals/:id {decision:"approved"}` records the decision (status→`approved`, audit row `actorKind:user`) and **then returns HTTP 500**: delivery attempts to `invoke` the routed connector, but the `geojson_poll` connector (the only target for an ingest-only stream) returns `"invoke not implemented for this connector"` (audit `verb:delivery_failed`). For an alert/notification, the **decision is the outcome** — there is nothing to execute. Delivery of an approved draft artifact with no invokable action must record the decision and return 200, not 500. Demo-blocking for Epicenter: the UI's `apiFetch` throws on the 500 and surfaces `Decision failed: …` on the demo's climactic approve action even though the decision persisted. Options: (a) treat a draft/notification artifact as terminal-on-approval (no invoke); (b) make the delivery step tolerate `invoke`-less connectors as a no-op with an audit record. Effort: ~0.5–1 d.

---

## P5 — UX / product polish

- **UI theming hooks.** The static HTML+JS UI is intentionally lightweight. To host product-grade demos (e.g. Epicenter's ops-console theme), define a token/theming layer or commit to a SPA upgrade path. Decide before investing in per-app CSS. Effort: ~2–4 d for a theming layer.
- **Connector setup + signal/approval timeline polish.** Incremental.

---

## P6 — DICE T&E positioning (evidence for the full proposal, due 2026-08-25)

The abstract makes IONe the agent interface + monitoring layer for DICE-MDO (§2.4) with quantitative platform metrics (§2.7). These items convert design claims into measured evidence before full proposal.

- **[DICE] Structured T&E event export.** The abstract makes IONe's audit trail/session event stream the T&E observability surface: tool-call logs → interaction counts (scalability), session timestamps → time-to-recover (adaptability), per-agent inference-step tracking (resilience). No queryable export of these exists today. Add a structured metrics/event export API over the audit + session tables (filterable by workspace/peer/session, bulk export). Biggest claim-vs-backlog gap. Effort: ~1 wk. **Partial — pending founder walkthrough:** code-complete on `feature/audit-event-export` (design `md/design/audit-event-export.md`, contracts `md/requirements/active/audit-event-export.md`; all 10 ACs pass as integration tests). Note: design found per-agent *inference-step* tracking and peer-session duration have no schema support — v1 ships interaction counts + recovery-gap; step tracking stays DICE-funded future work.
- **[DICE] MCP routing throughput benchmark.** §2.7 targets <5% (stretch <2%) monitoring overhead at matched event rates. Measure baseline now: tool-calls/sec through the federation router, routing latency with/without audit logging enabled. Gives the full proposal hard numbers instead of design assumptions (per the tense/evidence boundary, `adcb6c2`). Effort: ~2–3 d.
- **[DICE] Programmatic workspace/peer provisioning.** "One workspace per mission" at 500–100K agents implies API-driven setup (create workspace, register peers, attach connectors headlessly), not the manual UI flow. Required before any scale demo. Effort: ~2–3 d.
- **[DICE] IONe federation-architecture white paper.** Promised in abstract §5 to accompany the full proposal. Hard deadline 2026-08-25. Effort: ~3–4 d writing.

Not here: CMMC / NIST SP 800-171 enclave stand-up (§2.6 commitment) — business-ops, tracked outside this infra backlog.

---

## Out of scope (noted, not planned)

- **Multi-tenant hosted SaaS tier.** Per the pricing strategy, gated behind 3 unsolicited asks + hire #2. IONe stays self-hosted-per-org until then.

---

_Created 2026-05-27 while scaffolding the Epicenter demo. The P0 visualization items are the difference between "IONe federates apps" and "IONe hosts apps" — and they pay off for every future app, not just this one._
