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

Work in this repo supports **Path 2 — Modular Infrastructure Risk Intelligence**, the selected growth path for Morton Analytics LLC (decided 2026-05-12). IONe's role: **on-prem workspace substrate underneath GroundPulse customer deployments**. Full plan: `../morton-analytics-web/md/strategy/path-2-90day-plan.md`.

## Positioning inside Path 2

IONe is **not a standalone product** in the current plan (until Y3 gate). The pitch to an asset-operator buyer is GroundPulse (the analytics + asset-type module) + IONe (the chat-first workspace surface), sold as one solution. This converts IONe's category-creation risk into a feature of GroundPulse's existing-category sale.

Commercial revenue accrues to GroundPulse engagements until a separate IONe commercial line is validated. The IONe OSS release still ships publicly to maintain MCP-adoption-window optionality.

## Current 90-day outcome

| ID | Outcome | Tier |
|---|---|---|
| P7 | IONe v0.1 OSS release date publicly locked or shipped | 3 (nice-to-have) |

P7 is Tier 3 — protected against P2 (pipelines module) but cuttable if Stream P load runs hot. The release date matters more than the ship date in the 90-day window: a locked public date keeps MCP-window optionality without forcing a rushed launch.

## Architectural priorities for Stream P work in IONe

- **Workspace-substrate framing**: every UI/API surface should be answerable to "would this be useful inside a GroundPulse customer deployment?" — not "would this win standalone enterprise AI ops sales?"
- **MCP-native is the differentiator** vs. Palantir / Glean / LangChain. Federation primitives and on-prem deployment are the moat. Keep these first-class.
- **Connectors that matter for Stream P**: ones that bridge to GroundPulse data (PMTiles, COG, observation streams) + the federal-adjacent ones already in v0.1 (NWS, FIRMS, IRWIN, S3/MinIO).
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

- GroundPulse (`../eo/`) is the primary Stream P deliverable. IONe's UI/API should consume GroundPulse module output cleanly.
- Marketing site (`../morton-analytics-web/`) — when IONe v0.1 ships publicly, update copy via that repo with the substrate-under-GroundPulse framing, not as a standalone product page.

## When in doubt

Read the 90-day plan at `../morton-analytics-web/md/strategy/path-2-90day-plan.md`. The current week's Tier 1 Stream P outcomes (P1–P3) are the priority. IONe (P7) is supporting.
