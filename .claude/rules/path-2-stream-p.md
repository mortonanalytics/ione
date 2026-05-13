---
paths:
  - "src/**"
  - "static/**"
  - "md/design/**"
  - "md/strategy/**"
  - "migrations/**"
  - "Cargo.toml"
  - "docker-compose.yml"
---

# Path 2 — Stream P Supporting (IONe)

Work in this repo supports **Path 2 — Modular Infrastructure Risk Intelligence**, the selected growth path for Morton Analytics LLC (decided 2026-05-12). IONe's role: **integration fabric for Morton Analytics' polyglot client-app portfolio.** GroundPulse is the first instance; TerraYield (`../eo_ag/`) and bearingLineDash (`../bearingLineDash/`) are the next reference apps the substrate must accommodate. Full plan: `../morton-analytics-web/md/strategy/path-2-90day-plan.md`. Substrate spec: `md/design/ione-substrate.md`.

## Positioning inside Path 2

IONe is **not a standalone product** in the current plan (until Y3 gate). The pitch to a client is the **domain app + IONe substrate** sold as one solution: GroundPulse + IONe for a pipeline/bridge/dam operator; TerraYield + IONe for an ag cooperative; etc. This converts IONe's category-creation risk into a feature of an existing-category sale.

Commercial revenue accrues to client-app engagements until a separate IONe commercial line is validated. The IONe OSS release still ships publicly to maintain MCP-adoption-window optionality.

The **internal ROI** is the real driver: every Morton client engagement deploys IONe substrate + one or more app modules. The OSS positioning is the external face of an internal-developer-leverage investment across the portfolio.

## Current 90-day outcome

| ID | Outcome | Tier |
|---|---|---|
| P7 | IONe v0.1 OSS release date publicly locked or shipped | 3 (nice-to-have) |

P7 is Tier 3 — protected against P2 (pipelines module) but cuttable if Stream P load runs hot. The release date matters more than the ship date in the 90-day window: a locked public date keeps MCP-window optionality without forcing a rushed launch.

## Architectural priorities for Stream P work in IONe

- **Integration-fabric framing**: every architectural decision answers one question — *does this help IONe federate to three different polyglot apps owned by different teams, or does it only make IONe better as a standalone product?* If only the latter, defer.
- **MCP-native is the differentiator** vs. Palantir / Glean / LangChain. Federation primitives and on-prem deployment are the moat. Keep these first-class.
- **Substrate layers IONe owns**: MCP federation hub; identity broker (OIDC, SAML SP, brokered SaaS OAuth, MFA); approval/audit gateway; thin UX shell with pluggable view types (map first, others later); push event ingress (signed webhooks + MCP notifications); cross-app workspace context (`workspace_peer_bindings`); federated catalog/search (defer until peer count > 3).
- **Layers IONe explicitly does NOT own** (apps own these): PostGIS / TimescaleDB / app-specific DB extensions; background task queues for app workloads; tile servers and raster compute; format-aware exporters; compute observability of remote apps; schema modules / app code hosting.
- **Federal-AI procurement tailwind**: OMB M-25-21 / M-25-22 favors on-prem + MCP-native + OSS. This is the buyer profile the IONe pricing doc already targets.

## Working in this repo

- All Stream P commits should reference the outcome ID (currently P7) and the GroundPulse-substrate framing.
- Major architectural decisions go in `md/design/`. Cross-reference the Path 2 tracker (`../morton-analytics-web/md/strategy/path-2-tracker.md`).
- IONe pricing strategy at `md/strategy/market/ione-pricing.md` is canonical for any pricing discussion — do not re-derive.

## What this rule prohibits

- Positioning IONe as a standalone enterprise platform in external copy or product surfaces (until Y3 gate)
- Spec'ing connectors or UI flows that have no path to a GroundPulse customer deployment
- Marketing "built with AI" as a differentiator — AI is internal leverage; for IONe specifically, MCP federation + on-prem + OSS is the differentiator
- Pursuing a separate IONe commercial sales motion in 2026

## Cross-repo coordination

- **Reference apps**: GroundPulse (`../eo/`), TerraYield (`../eo_ag/`), bearingLineDash (`../bearingLineDash/`, future once it expands beyond QuickBooks). Each connects to IONe by satisfying the contract in `md/design/app-integration-playbook.md`.
- IONe's UI/API renders refs and brokers identity; the apps own their data, compute, and frontends. Per-app MCP-server design docs live in each app repo, not in IONe.
- Marketing site (`../morton-analytics-web/`) — when IONe v0.1 ships publicly, update copy with the integration-fabric framing (substrate for Morton's app portfolio), not standalone-product framing.

## When in doubt

Read the 90-day plan at `../morton-analytics-web/md/strategy/path-2-90day-plan.md`. The current week's Tier 1 Stream P outcomes (P1–P3) are the priority. IONe (P7) is supporting.
