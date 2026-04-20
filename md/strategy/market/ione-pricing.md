# IONe Pricing Recommendation

**Date:** 2026-04-19
**Prepared for:** Morton Analytics LLC — IONe rebuild pricing decision
**Horizon:** 12 months ahead
**Constraint:** 2 analyst-programmers, no sales team, no VC runway, CAGE 9VN85, USDA-NASS production track record

## 1. Pricing architecture: Federal FFP services + OSS-with-paid-deployment, hybrid

**Primary architecture: Firm-Fixed-Price (FFP) services and task orders with an OSS core.** Not per-seat. Not usage/consumption. Not a hosted SaaS tier — yet.

A 2-person team cannot support per-seat SaaS metering, billing, churn, SOC 2, 24/7 uptime SLAs, or a self-serve credit-card funnel. Every hour spent on Stripe integration is an hour not spent shipping the IONe wedge during a 12–24 month window. The market brief is explicit: Morton's defensible wedge is "reproducible, parameterized, chat-driven pipelines for domain SMEs in government/research," delivered on OSS distribution. That buyer does not swipe a credit card — they issue a task order, a cooperative agreement, or a SBIR award. Pricing should match how the buyer buys.

The hybrid is: **OSS core (free, Apache-2.0 or equivalent) + paid deployment/integration engagements + FFP federal task orders + monthly support retainer.** This mirrors the n8n and Prophecy distribution funnel but without the $47M–$180M bankroll — Morton uses CAGE 9VN85 and NASS proof as the distribution substitute for VC-backed marketing.

Hosted / per-seat comes *later*, and only when a specific gate is cleared (§6).

## 2. Price points (12 months ahead)

### Federal SBIR Phase I — $275K FFP, 12-month PoP
Shape: one NASS/ERS-adjacent agency, SBIR Phase I or direct-to-Phase-II if the prior NASS deployment qualifies. Statement of work is a scoped IONe-AI prototype — a chat-first pipeline over one published statistical product (e.g., Crop Progress, Cattle on Feed, or a BLS series). Fixed price, milestone-billed.

### Productization task order — $400K–$600K FFP per instance
Shape: agency-specific productization of the Phase-I prototype under an existing GSA/NASA SEWP/NITAAC vehicle or agency-specific IDIQ. Target two of these in the 12-month window. Matches the white paper's $500K production figure. Each is scoped as an end-to-end deployment: IONe-AI installed on the agency's infrastructure, connected to one production data product, with a documented hand-off and 3 months of transition support.

### OSS paid deployment engagement — $35K–$75K FFP
Shape: 4–8 week deployment of the OSS core for a land-grant university, state ag-stat office, or vertical SMB (commodity desk, co-op). Scope: installation, 1–2 pipeline conversions from the customer's current tooling, a training day, and 30 days of post-launch support. Price flexes with data-source count and on-prem vs. cloud.

### Monthly support retainer — $4,500/month ($54K/year)
Shape: post-deployment retainer for OSS-paid-deployment customers and non-federal installs. Covers up to 8 engineering hours/month, priority Slack/email response, version upgrades, and one minor feature request per quarter. Roughly one-quarter of a competent FTE rate, which is the defensible floor for a 2-person shop. Federal customers get this bundled into the task order instead.

### Hosted tier — **do not ship in the first 12 months.** See §6 for the gate.

### Per-seat / per-workspace — **do not price in the first 12 months.** If the gate clears and a commercial tier launches, anchor at **$75/editor/month** (Hex-class) with a **$25K annual workspace minimum**. This is deliberately above Julius ($20–70) and ThoughtSpot ($25–50) because IONe is not competing on per-user price; it is competing on "one surface replaces 3 tools + deploys on-prem."

## 3. Anchor logic

| Price point | Anchor(s) | Why |
|---|---|---|
| SBIR Phase I $275K | SBIR Phase I ceiling (~$295K at most agencies as of FY26); IONe white-paper prototype ROM $250K | Fits the envelope the buyer already expects. Not a negotiation — a template. |
| Productization $400–600K | White-paper production ROM $500K; typical agency IDIQ task order $250K–$1M | Direct lift from the document the team already has. |
| OSS deployment $35–75K | Prophecy floor $299/mo × 12 ≈ $3.6K (floor too low for services) → scale to Outerbounds $2,499/mo managed = $30K/yr | $35K lower bound ≈ one Outerbounds year. $75K upper bound ≈ two Outerbounds years + custom on-prem premium. Matches the SME buyer segment's WTP of $10K–$150K/project from the market brief. |
| Support retainer $4,500/mo | Outerbounds $2,499/mo managed cloud; typical 2-person shop's support SKU $3K–$6K/mo | Priced above Outerbounds because it's humans not a control plane, but below Glean's $50K annual minimum because the buyer isn't that buyer. |
| Per-seat $75 (if/when) | Hex Team $75; ThoughtSpot $50; Julius $70 | Parity with the premium end of chat-BI, not a discount play. Morton has nothing to gain by being the cheap option — it burns the on-prem premium. |
| Annual minimum $25K | Glean $50K; Fabric capacity floor ~$100K+ | Half the Glean floor because the 2-person support model can't absorb Glean-class customers, but high enough to exclude tire-kickers who will cost more than they pay. |

## 4. What NOT to price

- **Per-token / per-LLM-call metering.** The whole Ollama-native thesis is that inference is essentially free at the margin. Charging per token abandons the strategic moat and signals "we're another OpenAI wrapper." The market brief flags this explicitly ("end of per-token pricing").
- **Per-pipeline-run or per-DAG-execution.** Requires build/operate metering infrastructure, creates adversarial customer incentives (they'll batch runs to avoid charges), and the Snowflake/Databricks consumption model is what buyers are fatigued *by*. Copying it is the opposite of the "unification" pitch.
- **Per-GB ingested or per-row processed.** Same problem, plus the team has no clean way to meter it on the customer's on-prem install. Federal buyers especially hate variable-cost pricing.
- **Free self-serve SaaS trial.** Zero-touch trials demand zero-touch support, which a 2-person team cannot provide. Every free-tier user is a distraction from the $275K customer.
- **Freemium with a paid upgrade path.** Different failure mode from OSS+paid-deployment: freemium requires a billing system and a churn model. OSS+paid-deployment doesn't. Use the one that matches the team size.
- **Hourly T&M consulting.** Caps revenue at utilization × rate. FFP lets Morton keep the upside when IONe's pre-built scaffolding delivers a $75K project in 3 weeks instead of 8.
- **Enterprise custom "call us" with no published price.** Appropriate for Glean/Hebbia; inappropriate for a 2-person shop whose credibility is proportional to clarity. Published SBIR + task-order shapes are a feature.

## 5. 12-month revenue math (plan-hit case)

Assumptions: one SBIR Phase I wins, two productization task orders close (one new agency, one NASS re-up or adjacent), three OSS paid deployments in academia/state/SMB, average two retainers attached at year-end.

| Line | Count | Unit | Annual |
|---|---|---|---|
| SBIR Phase I | 1 | $275K | $275K |
| Productization task order | 2 | $500K avg | $1,000K |
| OSS paid deployment | 3 | $55K avg | $165K |
| Support retainer (partial-year avg) | 2 | $27K (6 mo avg) | $54K |
| **Total** | | | **~$1.49M** |

**Downside plan (one Phase I, one task order, two deployments, one retainer): ~$475K.** Still profitable for a 2-person shop with no VC overhead.

**Upside plan (one Phase I, three task orders, five deployments, four retainers): ~$2.2M.** Consistent with the market brief's $0.5M–$3M 3-year SOM, pulled forward to year 1 by the existing NASS relationship. Requires the NASS re-up hypothesis in the brief's open questions to resolve positively.

## 6. The gate: when to turn on the hosted tier

**Turn on hosted + per-seat when — and only when — three unsolicited OSS-deployment customers in the same 90-day window ask for "a managed version" without Morton bringing it up.**

Not two. Not a single big-name logo dangling a contract. Three, unprompted, in a quarter. That is the earliest point where demand exceeds the 2-person team's ability to service via FFP deployments, and the first point where the cost of building billing + SOC 2 + multi-tenant isolation is dominated by the revenue it unlocks.

Sub-gates that must also be true on the day the main gate trips:
- At least one productization task order is live and generating maintenance revenue (proves the product works unattended).
- The OSS repo has ≥1 outside contributor with a merged non-trivial PR (proves the funnel is self-sustaining, not pure outbound).
- Morton has either hired a third person or has an explicit plan funded by existing task-order cash to hire one within 60 days.

If any sub-gate fails, keep selling FFP deployments and revisit in 90 days. A hosted tier shipped into a 2-person team with no funnel is a strictly worse business than an FFP practice clearing $1.5M on 6 customers.
