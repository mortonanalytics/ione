# Market Research: Onboarding UX for Data Infrastructure as a Service
## Date: 2026-04-23

## Executive summary

Onboarding in data IaaS has converged on a small number of patterns that work. The winners (Hex, Hightouch, Linear, Estuary, Retool) all ship pre-seeded demo data inside the real workspace, skippable wizards over forced ones, templates on the "new" button, and activation defined as *resolving something real* rather than finishing a tour. IONe currently ships none of these. A UI redesign should steal three specific, validated patterns rather than invent new ones. This brief recommends three concrete concepts for IONe and scores them.

## Scope

UI/UX patterns only — not TAM/SAM/pricing. Evidence drawn from documented first-run flows across Fivetran, Airbyte, Estuary, Hightouch, Census, Hex, Julius, Glean, Retool, Notion AI, Dust, PagerDuty, Rootly, Grafana, Supabase, and Linear.

## What's working in 2026 (consensus patterns)

| Pattern | Example | Why it wins |
|---------|---------|-------------|
| Pre-seeded demo connection *inside* the real workspace | Hex `[Demo] Hex Public Data` Snowflake; Hightouch "B2B SaaS Sample" / "B2C eCommerce Sample" at bottom of source picker; Retool DB ships populated | First query / first sync happens before any auth or config |
| Skippable linear wizard, each step one decision | PagerDuty 6-step trial; Linear 15-screen setup | No nag modals, clear recovery path via a single post-onboarding doc |
| Checklist *inside the product*, tied to real artifacts | Linear's "create → assign → close an issue" activation | Completion side-effects are real work, not tutorial cruft |
| Template-first "new" button | Hex templates; Retool "Employee Onboarding Dashboard"; Supabase "User Management Starter" SQL | Blank-canvas is the exception, not the default |
| Explicit publish + completion notification | Estuary Save-and-Publish; Fivetran "Start initial sync" + toast | Closes the silent gap between setup and data |
| Command-menu / primary-surface taught pre-content | Linear Cmd+K tutorial before any issues exist | Teaches the product's thesis directly |

## What's failing / outdated

- Separate "demo mode" behind a toggle — superseded by in-workspace sample data
- Video-as-onboarding as the primary path (always secondary now)
- Forced warehouse/destination pre-req before first visible value
- Long silent background indexing (Notion AI's 36–72 hour window is a known friction point)
- Docs-as-onboarding (Fivetran's "pre-read the ERD" recommendation)
- Catalogs with 300+ connectors and no curation / no "most common" default

## IONe current state (baseline we're designing against)

- Blank chat panel on first load; no welcome copy, no sample prompt, no empty-state help
- Workspace/connector creation modals are free-form JSON with no per-connector templates
- `rust_native / openapi / mcp` dropdown — openapi errors on submit, user has to read source to know which `name` routes to which connector
- Poll loop is a silent 60s scheduler tick with no user-facing state
- Roles have no UI (SQL-only)
- 5 tabs (Chat, Connectors, Signals, Survivors, Approvals) all show empty lists with zero guidance

## Three concepts, ranked

### Concept 1 — "Demo Workspace" (Hex / Hightouch pattern)

**What:** Every first-run install ships with a pre-populated read-only "IONe Demo Ops" workspace containing fake stream events from each connector IONe supports: NWS alerts, FIRMS fire detections, Slack threads, IRWIN incidents, a handful of pre-computed signals, survivors, routing decisions, and approvals. The chat bar works against it immediately — no Ollama required for the demo data, just canned responses to 5-6 suggested prompts. Switching to a real workspace is one click in the workspace switcher (which already exists).

**Evidence:**
- Hex ships `[Demo] Hex Public Data` Snowflake connection in every workspace — lives in the same Data browser as real sources, prefixed `[Demo]`
- Hightouch puts "B2B SaaS Sample" and "B2C eCommerce Sample" at the bottom of the source picker — zero-credential, one-click, UI-identical to a real source
- Retool DB ships pre-populated on all plans

**Why this beats alternatives:**
- Solves the Ollama/model/network fragility in one move — demo works offline, with no pulls
- Makes the 5 tabs (Signals, Survivors, Approvals) light up with real-looking content on first load, which they otherwise never do
- No new screens to design; just seed data + a workspace-switcher label

**Cost:** Small. Seed migration + static JSON fixtures + a feature flag to prevent the demo workspace from being written to. ~2-3 days.

---

### Concept 2 — "Triage One Alert, End-to-End" (Linear pattern)

**What:** A single pinned checklist in the workspace chrome with 4 items, each of which completes only when the user does the real thing (not a tour step):
1. Ask the workspace a question ("What's my highest-severity alert?")
2. Open one survivor in the feed
3. Approve one action in the Approvals tab
4. See the audit trail entry

Completion = user has walked the full generator → critic → routing → approval → audit loop *once*. This is IONe's entire thesis reduced to one traversal. The checklist is skippable per item and collapses after completion.

**Evidence:**
- Linear's activation metric is "resolve an issue," not "create an issue" — the checklist lives inside the product chrome and runs against the user's own first issue, not fake tutorial content
- PagerDuty's 6-step sequence: each step one decision, one screen, Skip visible, post-onboarding recovery doc for anything skipped

**Why this beats alternatives:**
- Teaches IONe's differentiation (the gen↔critic↔routing loop) by making the user live it once, rather than reading about it
- Works in the Demo Workspace (Concept 1 is a dependency), so no prereqs, no waiting on connectors
- Replaces all the scattered empty-state copy / tooltips / help text with one coherent path

**Cost:** Medium. Checklist component, progress persistence per user, wired to specific events (message_sent, survivor_opened, approval_decided, audit_viewed). ~1 week.

---

### Concept 3 — "Publish, Don't Poll" (Estuary pattern)

**What:** Kill the silent 60-second scheduler tick as the first-run experience. When a user adds a connector, immediately show a live progress cascade: "Publishing connector..." → "First stream event received" → "First signal generated" → "First survivor passed critic" → "First routing decision." Each state transition is a toast + a timeline entry on the connector card. If any stage stalls, the toast says what's next and why ("waiting for Ollama to return — normally takes 2-10s").

Default behavior also changes: on connector creation, trigger one immediate poll + one immediate generator pass, then hand off to the scheduler. User never sees a silent gap.

**Evidence:**
- Estuary Flow's Save-and-Publish: draft capture in UI, explicit Publish triggers sync + a notification when successful
- Fivetran's "Start initial sync" button after setup + completion notification, with free BigQuery destination so users without infrastructure can still finish the loop

**Why this beats alternatives:**
- Addresses the failure mode the user actually hit when they tried chat: silent failure with no remediation
- Makes IONe's pipeline (which is real, and a differentiator) visible instead of hidden — users currently have no way to know the gen↔critic loop is even running
- Converts every pipeline stage into a teaching moment

**Cost:** Medium-high. Requires a per-connector event bus / SSE channel surfaced in the UI, and refactoring the scheduler to emit structured progress events. ~1.5-2 weeks. Highest leverage long-term — every connector benefits, including v0.2+ features.

## Recommendation

**Invest now.** All three concepts are validated in shipping products and directly address IONe's first-run failures. Sequence:

1. **Concept 1 (Demo Workspace) first** — unblocks everything else, smallest cost, biggest perceived quality lift, and lets Concepts 2+3 be designed/tested without requiring live Ollama + connectors.
2. **Concept 2 (Triage checklist)** second — depends on Concept 1.
3. **Concept 3 (Publish pattern)** third — larger engineering cost but pays off on every future connector.

Do **not** try to invent a Morton-original onboarding metaphor. The patterns above are what the market has converged on; differentiation is in the product (federated nodes + gen↔adversarial loop), not in the onboarding surface.

## Anti-recommendations (things to not design)

- A welcome modal with a "Take the tour" button (superseded everywhere)
- A separate `/demo` route or demo toggle (Hex-style in-workspace demo beats it)
- A 17-step configuration wizard covering every env var (PagerDuty's skippable 6-step is the ceiling)
- A "click Poll Now to trigger a poll" button as the primary remediation for silent scheduler gaps (that's the disease, not the cure — Concept 3 fixes the disease)
- An empty-state design tour on every tab (replaced by the Demo Workspace lighting the tabs up with real content)

## Open questions

- Demo Workspace needs authoritative seed fixtures — who generates them? (Probably: one of us, once, offline, committed to the repo)
- Is the Triage checklist scoped to the demo workspace only, or does it re-trigger on the first real workspace too?
- Concept 3 requires surfacing pipeline events the scheduler currently only logs — is that worth the refactor in v0.2, or pushed to v0.3?

## Sources

Primary: dossier compiled from documentation pages for Fivetran, Airbyte, Estuary, Hightouch, Census, Hex, Julius, Glean, Retool, Notion AI, Dust, PagerDuty, Rootly, Grafana, Supabase, Linear; teardowns on pageflows.com, supademo.com, candu.ai, appcues.com. Full citation list available on request (30+ URLs verified during research).

Limits: chat-first AI workspace onboarding (Claude Projects, ChatGPT connectors, Cursor, Dust agents, Pinecone Assistant) was requested as a second axis but the researcher tasked with it lacked web-fetch tools; patterns inferred from the data-IaaS side only. MCP-specific onboarding researched in a second pass (addendum below).

---

## Addendum: MCP-specific onboarding (April 2026)

### State of the category

Maturing, not mature. Post the Nov 2025 authorization spec, MCP onboarding has split two-speed:

- **Paved path**: popular remote servers (Linear, Sentry, Figma, Stripe) into paved clients (Cursor, Claude Code, Claude Desktop Pro/Max) — click a per-client install, finish an OAuth consent, tools appear.
- **Unpaved**: everything else — custom/private servers, Claude Desktop free tier, peer-to-peer — is still JSON-paste, `claude mcp add` CLI, or `.mcpb` double-click.

Perplexity's CTO publicly moving away from MCP in March 2026 citing schema bloat + auth friction is the clearest signal that onboarding is still a real problem, not a solved one.

### Patterns that matter for IONe

1. **Per-client connect tiles** (Figma is the exemplar — https://developers.figma.com/docs/figma-mcp-server/remote-server-installation/). The provider's docs page is a grid of client tiles: Cursor gets a deep-link button, Claude Code gets a CLI command, VS Code gets a deep link, Claude Desktop gets paste-URL instructions. One surface, per-client paths.
2. **Cursor deep-link install** (`cursor://anysphere.cursor-deeplink/mcp/install?name=…&config=base64…`) — canonical one-click, shipped by Stripe, Figma, Linear, Sourcegraph. Proofpoint has documented phishing risk (`CursorJack`) so the UI must present trust signals.
3. **Claude Desktop custom connector URL paste** — Pro/Max only, Settings → Customize → Connectors → "+ Add custom connector" → URL → OAuth. Not one-click but gets a non-developer through.
4. **OAuth 2.1 + PKCE with CIMD (Client ID Metadata Document)** — Nov 2025 spec. Servers return `401 + WWW-Authenticate`, clients discover via `.well-known/oauth-authorization-server`. Static API keys exist only as headless/CI fallback. Claude's clients reject static bearer for remote MCP.
5. **Desktop Extensions (.mcpb)** — Anthropic's answer to the JSON problem for *local* servers. Irrelevant to IONe (hosted remote).
6. **Registry-driven install** (Smithery, mcp.so, PulseMCP, official registry) — fragmented across 4+ registries with ~40k total servers; no unified search; no install button on the official registry itself.

### What's still broken across the ecosystem

- No cross-client "Add to AI" button — every provider ships N per-client buttons.
- No pre-connection tool preview in any client — users don't know what they installed until it runs.
- Registry fragmentation — 4 registries, no canonical discovery.
- Tool schema bloat — Perplexity measured up to 72% context consumed by tool metadata before user input.
- Deep-link spoofing — no trust UI on `cursor://` install links.

### Concept 4 — "The MCP Front Door" (Figma + OAuth 2.1 pattern)

**What:** Replace IONe's current sidebar "copy URL" widget with a proper **Connect to MCP** page. Two tabs:

*Tab 1 — Connect a client to this IONe node.* A grid of client tiles copied directly from Figma's layout:
- **Cursor** — one-click `cursor://` deep link
- **Claude Desktop (Pro/Max)** — copy URL + show the 3-step Settings path inline
- **Claude Code** — one-line `claude mcp add --transport http ione <url>` with copy button
- **VS Code** — deep-link install
- **Other** — raw JSON config for manual paste

*Tab 2 — Connected clients.* Live list of every client that's completed OAuth against this node: client name (from OAuth consent), granted tools, first connected, last seen, revoke button. This is the "I'm a server now" disclosure done right — currently no MCP server shows this inside its own UI.

**Server side:** implement OAuth 2.1 + PKCE on `/mcp` with `.well-known/oauth-authorization-server`. Static bearer tokens remain for CI / headless use, documented but not default.

**Evidence:**
- Figma's per-client tile layout (https://developers.figma.com/docs/figma-mcp-server/remote-server-installation/)
- Stripe, Linear, Sentry all ship the same tile pattern
- OAuth 2.1 + PKCE is the Nov 2025 MCP auth spec — Claude clients reject anything else for remote MCP

**Why it wins:**
- Solves IONe's self-MCP-server onboarding in the same way every paved-path provider solves theirs
- Ships required OAuth support for v0.2+ consumer use (anyone using Claude Desktop Pro can connect with no JSON editing)
- The "Connected clients" panel is a moat — nobody else shows per-client session state inside the server's own UI, and it matters a lot in an ops product where auditability is a feature, not a nice-to-have

**Cost:** High. OAuth 2.1 + PKCE + CIMD on the server is ~1-2 weeks of correct-the-first-time work. Per-client tile UI is ~2-3 days on top.

### Concept 5 — "Peer Handshake" (OAuth-federation pattern)

**What:** IONe admin federates with another IONe node by pasting its URL. IONe-as-client performs the same OAuth 2.1 discovery a Claude Desktop would — but *before* completing the handshake, shows the peer's tool manifest (names + descriptions) for explicit allow-list approval. The admin decides which peer tools are callable before the subscription commits. Subscription stored as a first-class object (peer URL, granted scopes, token expiry, last-seen, tool allow-list, revoke).

**Optional high-leverage addition:** signed QR-code pairing for in-person federation at ops events. Peer A shows a QR with their `/mcp` URL + short-lived pairing token; peer B scans and completes OAuth. Nobody else ships this and it fits the fire-ops / emergency-services use case directly.

**Evidence:**
- Standard OAuth 2.1 + PKCE + CIMD pattern (aaronparecki.com/2025/11/25/1/mcp-authorization-spec-update)
- "No pre-connection tool preview" is explicitly identified as broken in every shipping MCP client today — IONe can be the first to fix it on the federation side

**Why it wins:**
- IONe's differentiator is federation; the federation UX should be first-class, not an afterthought of the generic MCP-client pattern
- Tool allow-list at subscription time is a security feature that compliance-sensitive buyers (federal, emergency services) will require before pilot
- QR handshake is a trade-show / demo / in-person-drill moment that's memorable and cheap to build once OAuth is in place

**Cost:** Medium. Reuses the OAuth client code from Concept 4. Allow-list + peer-subscription object ~1 week. QR pairing is 1-2 days on top.

### Revised sequence

1. **Concept 1 (Demo Workspace)** — unblocks everything else, smallest cost
2. **Concept 2 (Triage checklist)** — depends on Concept 1
3. **Concept 4 (MCP Front Door)** — enables real third-party clients, forces the OAuth 2.1 investment that v0.2 needs anyway
4. **Concept 5 (Peer Handshake)** — reuses Concept 4's OAuth plumbing, lights up the federation differentiator
5. **Concept 3 (Publish, don't poll)** — highest engineering lift, deferable until after 1/2/4/5 are shipped

### Updated anti-recommendations

- Do not ship `.mcpb` bundles — wrong shape for a hosted remote server
- Do not publish to the official MCP registry as the primary discovery path until there are >100 nodes — registry metadata without an install button is advertising, not onboarding
- Do not ship static-bearer as the default remote-auth story — Claude clients reject it and the ecosystem has moved past it

### MCP addendum sources (verified URLs)

- https://modelcontextprotocol.io/quickstart/user
- https://support.claude.com/en/articles/11175166-get-started-with-custom-connectors-using-remote-mcp
- https://www.anthropic.com/engineering/desktop-extensions
- https://cursor.com/docs/context/mcp/install-links
- https://code.claude.com/docs/en/mcp
- https://developers.figma.com/docs/figma-mcp-server/remote-server-installation/
- https://linear.app/docs/mcp
- https://docs.sentry.io/product/sentry-mcp/
- https://docs.stripe.com/mcp
- https://aaronparecki.com/2025/11/25/1/mcp-authorization-spec-update
- https://stackoverflow.blog/2026/01/21/is-that-allowed-authentication-and-authorization-in-model-context-protocol/
- https://registry.modelcontextprotocol.io/
