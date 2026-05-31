# Walkthrough UI Overhaul — Implementation Plan

**Design basis:** No standalone design doc. Inputs are the 2026-05-30 scope estimate + three `/decide` resolutions, recorded in memory `project_ui_overhaul_decisions.md`:
1. Theming = extend the existing CSS token layer (no SPA).
2. Demo map/document data = loopback mock-MCP peer.
3. Adaptive nav = data-presence counts on `GET /workspaces/:id`.
**Shape:** medium — vertical slices, file inventory + gate per phase. No task manifest, no contract file (sequential, single developer).
**Stack:** backend-only Rust (axum + sqlx, `src/routes`, `src/services`, `src/demo`) + static vanilla-JS/CSS frontend (`static/`, no build step, no tsc). Postgres on `:5433`, demo node via `IONE_SEED_DEMO=1`.

## Dependencies
- Vendor a small markdown renderer into `static/vendor/` (e.g. `marked` UMD, ~40KB, no CDN — CSP-friendly since the CSP spike is still pending). One file, no package.json change.
- No new crates. `wiremock` stays test-only; the demo mock peer is a built-in app route, not wiremock.

## Verification baseline (used by gates)
Boot a demo node once per gate as needed:
```
IONE_BIND=127.0.0.1:3010 DATABASE_URL=postgres://ione:ione@localhost:5433/ione \
IONE_SEED_DEMO=1 IONE_SKIP_LIVE=1 OLLAMA_BASE_URL=http://127.0.0.1:1 \
IONE_TOKEN_KEY=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA= \
IONE_WEBHOOK_SECRET_KEY=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA= \
./target/debug/ione
```
Demo workspace id: `crate::demo::DEMO_WORKSPACE_ID`. Frontend JS gate: `node --check static/app.js`.

---

## Phases

### Phase 1 — Demo-legibility batch (no architecture changes)
Three independent fixes that repair the demo front door. All ride on existing CSS tokens. Each is its own slice; ship in this order but they have no interdependency.

#### Phase 1a — Chat markdown rendering
**Goal:** Canned + live chat replies render `**bold**`/lists/inline-code as HTML, not literal asterisks, without breaking resource-chip injection.
**Files:**
- `static/vendor/marked.min.js` — new; vendored markdown renderer.
- `static/index.html` — add `<script src="/vendor/marked.min.js"></script>` near the other vendor scripts (before `app.js`).
- `static/app.js` — `appendMessage` ([app.js:334-343](../../static/app.js#L334-L343)): render the body as markdown; keep the `You:`/`Model:` prefix as a non-parsed label; keep `injectResourceChips(div)` working over the rendered nodes.
**Code shape:**
```js
function appendMessage(role, text) {
  const div = document.createElement('div');
  div.className = 'message ' + role;
  const prefix = (role === 'user' ? 'You: ' : 'Model: ');
  div.dataset.rawText = prefix + text;            // keep raw for chip regex + copy
  const label = document.createElement('span');
  label.className = 'message-prefix';
  label.textContent = prefix;
  const bodyEl = document.createElement('span');
  bodyEl.className = 'message-body';
  // marked.parse escapes HTML by default; do NOT enable raw HTML.
  bodyEl.innerHTML = marked.parse(text, { breaks: true });
  div.append(label, bodyEl);
  transcript.appendChild(div);
  injectResourceChips(div);                       // walks text nodes of rendered markdown — still finds URIs
  transcript.scrollTop = transcript.scrollHeight;
}
```
**Risk:** `injectResourceChips` ([app.js:1725](../../static/app.js#L1725)) walks text nodes; confirm a resource URI that lands inside rendered markdown (e.g. inside a `<li>`) still chips correctly. If `marked` wraps the URI in an `<a>`, the text-node walk still sees the text — verify with the canned reply that contains a resource URI.
**Gate:** `node --check static/app.js` passes AND in a booted demo node, open Chat → the canned reply shows no literal `**` (Playwright: assert `#transcript .message-body strong` exists and `#transcript` text does not contain `**`).
**Acceptance:** `marked` is loaded and `appendMessage` produces a `.message-body strong` element for a bold canned reply.

#### Phase 1b — Approvals + severity-badge CSS (pure CSS)
**Goal:** Approvals render as styled cards matching signals/survivors; the `kind`/`status` spans no longer collapse into `notification_draftpending`.
**Files:**
- `static/style.css` — add rules for the classes `renderApprovals` already emits ([app.js:3905-4032](../../static/app.js#L3905-L4032)): `.approval-item`, `.approval-header`, `.approval-kind`, `.approval-status` + `.approval-status--pending|approved|rejected`, `.approval-title`, `.approval-body`, `.approval-rationale`, `.approval-comment`, `.approval-actions`, `.approval-btn` + `--approve`/`--reject`, `.approval-decided-at`, `.approval-item--empty`. Also add the `.severity-badge` + `.severity-badge--routine|flagged|command` classes the approvals header uses ([app.js:3937](../../static/app.js#L3937)) — currently only `.severity-chip--*` exists ([style.css:2288](../../static/style.css#L2288)). **No JS change.**
**Code shape (mirror `.signal-card` at [style.css:2269](../../static/style.css#L2269)):**
```css
#approval-list { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: var(--space-3); }
.approval-item { border: 1px solid var(--color-border); border-radius: var(--radius);
  background: var(--color-surface); padding: var(--space-2) var(--space-3);
  display: flex; flex-direction: column; gap: var(--space-1); }
.approval-header { display: flex; align-items: center; gap: var(--space-2); flex-wrap: wrap; }
.approval-kind { font-family: ui-monospace, monospace; font-size: var(--font-size-xs);
  padding: 1px var(--space-2); border-radius: 99px; background: #eef1f6; color: #3949ab; }
.approval-status { font-size: var(--font-size-xs); padding: 1px var(--space-2); border-radius: 99px; margin-left: auto; }
.approval-status--pending  { background: #fff8e1; color: #b45309; }
.approval-status--approved { background: #e6f4ea; color: #1e7e34; }
.approval-status--rejected { background: #fdecea; color: var(--color-error); }
.approval-actions { display: flex; gap: var(--space-2); }
.approval-btn { min-height: 44px; padding: 0 var(--space-3); border-radius: var(--radius);
  border: 1px solid var(--color-border); cursor: pointer; }
.approval-btn--approve { background: var(--color-primary); color: #fff; border-color: transparent; }
.approval-comment { width: 100%; min-height: 44px; }
.severity-badge { font-size: var(--font-size-xs); padding: 1px var(--space-2); border-radius: 99px; font-weight: 600; }
.severity-badge--routine { background:#f0f0f0; color:#555; }
.severity-badge--flagged { background:#fff8e1; color:#b45309; }
.severity-badge--command { background:#fdecea; color: var(--color-error); }
```
**Gate:** booted demo node, Approvals tab — Playwright screenshot shows bordered cards (not `•` bullets), `.approval-kind` and `.approval-status` visually separated, Approve/Reject styled with ≥44px height.
**Acceptance:** `grep -c "\.approval-item" static/style.css` ≥ 1 and the rendered `.approval-kind` + `.approval-status` have distinct backgrounds.

#### Phase 1c — Chart + Table demo seed (IONe-native, no peer)
**Goal:** Charts and Tables tabs render real data in the demo workspace, via streams carrying `view_config` (the `fetch_ione_charts` native path — [services/chart_panels.rs](../../src/services/chart_panels.rs)). No peer required.
**Files:**
- `src/demo/fixture.rs` — extend the stream fixtures ([fixture.rs](../../src/demo/fixture.rs)) so ≥1 seeded stream has `view_config` populated with the property-field pointers the chart/table services read. Confirm exact `view_config` shape against `fetch_ione_charts` and `table_property_columns` (reference test patterns `tests/phase_chart_aggregates.rs`, `tests/phase_table.rs`, `tests/support/event_layer_seeder.rs`).
- `src/demo/seeder.rs` — `seed_streams`/`seed_stream_events` ([seeder.rs:48-49](../../src/demo/seeder.rs#L48-L49)) already run; ensure the view_config-bearing stream gets numeric events suitable for an aggregate (counts over time buckets).
**Code shape:** populate the existing stream fixture's `view_config`:
```rust
// view_config that the ione-native chart/table path consumes
json!({
  "property_fields": ["properties.acreage", "properties.structures_threatened"],
  "geometry_pointer": "geometry"
})
```
(Exact keys: match `fetch_ione_charts` / `table_property_columns` — do not guess; read the service first.)
**Gate:** `cargo check` passes; boot demo node, then
`curl -s 127.0.0.1:3010/api/v1/workspaces/<DEMO_ID>/chart-panels | python3 -c "import sys,json;d=json.load(sys.stdin);print(len(d['ioneCharts']))"` ≥ 1, and same for `/table-panels`.
**Acceptance:** Charts and Tables tabs are non-empty in the demo (Playwright: `.chart-empty`/`.table-empty` hidden).

---

### Phase 2 — Map + Document demo data via loopback mock-MCP peer
**Goal:** Map and Documents tabs render in the demo workspace by federating from a built-in mock MCP endpoint the demo seeds as a peer — exercising the real `map_layers.rs`/`document_panels.rs` peer-fetch path.
**Files:**
- `src/routes/` (new, e.g. `demo_mcp.rs`) — a route mounted **only when `IONE_SEED_DEMO=1`** that answers JSON-RPC `resources/list` with one `ione_view:"map"` resource (tile_url + bounds metadata) and one `ione_view:"document"` resource (download_url + mime_type), plus `resources/read` if the document panel fetches content. Bypass/seed the bearer-trust path for this loopback peer.
- `src/routes/mod.rs` — conditionally mount the demo MCP route.
- `src/demo/seeder.rs` — new `seed_demo_peer`: insert a `trust_issuers` row, a `peers` row whose `mcp_url` points at the node's own demo MCP route, and an **active** `workspace_peer_bindings` row for `DEMO_WORKSPACE_ID`. Add the call to `seed_demo` ([seeder.rs:38-58](../../src/demo/seeder.rs#L38-L58)) before `tx.commit`.
- `src/demo/fixture.rs` — the canned map + document resource payloads.
**Code shape (mock resources/list result):**
```json
{ "resources": [
  { "uri": "ione://demo/map/fires", "name": "Active Fire Detections",
    "metadata": { "ione_view": "map", "tile_url": "...", "bounds": [-116,45,-113,48] } },
  { "uri": "ione://demo/doc/briefing", "name": "Lolo NF Briefing",
    "metadata": { "ione_view": "document", "download_url": "https://...", "mime_type": "application/pdf" } }
] }
```
**Known risk (verify, do not assume):** `document_panels.rs` may require `download_url` to be HTTPS and pass the `safe_http`/SSRF guard, which a `http://127.0.0.1` loopback fails. If so, either (a) point the document `download_url` at a stable public HTTPS asset, (b) use a `data:` URL if accepted, or (c) add a narrow demo-mode exception. Resolve against the actual guard in `document_panels.rs` during implementation; map (tile_url) is lower-risk. If document can't be made to render cleanly in the demo, ship map and leave document as a follow-up rather than weakening the SSRF guard.
**Gate:** `cargo check` + `cargo test` for any touched service; boot demo node, then
`curl -s 127.0.0.1:3010/api/v1/workspaces/<DEMO_ID>/map-layers` returns ≥1 layer; Playwright: Map tab shows `#map-canvas-container` (not `#map-empty`).
**Acceptance:** Map tab renders a layer in the demo; document either renders or is explicitly deferred with the guard reason noted.

---

### Phase 3 — Adaptive navigation (data-presence)
**Goal:** Replace the flat always-on 9-tab bar with a registry that shows a data-viz tab only when its workspace has data; demote ops tabs to a secondary group; fix mobile overflow + sidebar.
**Files:**
- `src/routes/workspaces.rs` — `get_workspace` ([workspaces.rs:71-86](../../src/routes/workspaces.rs#L71-L86)): merge a `panels` count block into the JSON response.
- `src/repos/` (workspace repo or a new query module) — cheap COUNT/EXISTS queries.
- `static/index.html` — restructure `#tab-bar` ([index.html:76-86](../../static/index.html#L76-L86)) into a primary group (Chat + conditional viz tabs) and a secondary group / overflow for ops tabs.
- `static/app.js` — replace the 9 hardcoded `switchTab` blocks ([app.js:1034-1130](../../static/app.js#L1034-L1130)) and the hardcoded arrow-key chain ([app.js:1132+](../../static/app.js#L1132)) with a `TABS` registry; render/hide tab buttons from `workspace.panels`; rebuild arrow-key nav over the **visible** tab list.
- `static/style.css` — `#tab-bar` overflow strategy (horizontal scroll or wrap) and sidebar collapse/usable state below the 780px breakpoint ([style.css:1152](../../static/style.css#L1152)).
**Contract (the `panels` block):**
```jsonc
// GET /api/v1/workspaces/:id  →  add:
"panels": {
  "charts": 2,            // COUNT streams with view_config NOT NULL (cheap, native)
  "tables": 2,            // same source as charts
  "map": true,            // EXISTS active workspace_peer_binding (proxy — peer-only panel)
  "documents": true,      // EXISTS active binding (proxy)
  "approvalsPending": 3,  // COUNT approvals pending (already computed for the badge)
  "signals": 8, "survivors": 5
}
```
**Decision pinned here (honors `/decide` #3, avoids per-request peer fan-out):** native panels (chart/table/signals/survivors/approvals) use cheap COUNT queries = true data presence. Map/document are peer-only and counting their resources would require synchronous peer fan-out on every workspace GET; use **active-binding existence** as the presence signal so we never hide a tab that has data and never block the request. Exact peer resource counts are a later refinement (cache on tab-open fetch).
**Code shape (registry):**
```js
const TABS = [
  { id: 'chat',      label: 'Chat',      group: 'primary', always: true },
  { id: 'map',       label: 'Map',       group: 'primary', show: p => p.map },
  { id: 'chart',     label: 'Charts',    group: 'primary', show: p => p.charts > 0 },
  { id: 'table',     label: 'Tables',    group: 'primary', show: p => p.tables > 0 },
  { id: 'document',  label: 'Documents', group: 'primary', show: p => p.documents },
  { id: 'connectors',label: 'Connectors',group: 'ops',     always: true },
  { id: 'signals',   label: 'Signals',   group: 'ops',     show: p => p.signals > 0 },
  { id: 'survivors', label: 'Survivors', group: 'ops',     show: p => p.survivors > 0 },
  { id: 'approvals', label: 'Approvals', group: 'ops',     show: p => p.approvalsPending > 0, badge: p => p.approvalsPending },
];
// renderTabs(panels): build buttons for visible tabs, wire click+arrow nav over the visible set.
// switchTab(name): drive aria-selected/hidden from TABS, keep the existing per-tab load() side-effects.
```
**Risk:** the per-tab `load*()` side-effects in `switchTab` (lines 1084-1119) and polling start/stop must be preserved when the body becomes registry-driven; keep them in a `switch(name)` inside the new `switchTab`. Ensure the currently-active tab can never be hidden out from under the user (if active tab’s data drops to 0, fall back to Chat).
**Gate:** `cargo check` + `cargo test get_workspace` (assert `panels` block shape); `node --check static/app.js`; boot demo node — Playwright at 1440px asserts viz tabs visible (now seeded), and at 375px asserts no horizontal body scroll and all visible tabs reachable.
**Acceptance:** In an empty (non-demo) workspace, Map/Charts/Tables/Documents tabs are absent; in the seeded demo they appear; at 375px the tab bar does not overflow the body.

---

## Self-review
1. **AC → gate:** markdown (1a gate), approvals styling (1b gate), chart/table render (1c gate), map render (2 gate), adaptive visibility + mobile (3 gate). ✓
2. **Files exist or listed-to-create:** all cited files verified present this session; new files (`marked.min.js`, `demo_mcp.rs`, count query module) listed under their phase. ✓
3. **Vertical slices:** each phase ships a user-visible outcome end-to-end (1a/1b/1c independently; 2 and 3 each full-stack). No layer-stacked phases. ✓
4. **Concrete gates:** every gate is a shell command (`cargo check`, `node --check`, `curl … | python3`) or a named Playwright assertion. ✓
5. **Parallel disjointness:** N/A — sequential, single developer, no task manifest. ✓

## Sequencing note
Phase 1 (≈2 days) is the commercial priority and is independent of 2/3 — ship and re-walk first. Phases 2–3 follow; 3's data-presence rule makes Phase 2's seeded map/doc auto-light-up the tabs. Keep this off the critical path of the next P1 infra item (MCP `notifications/*` reception).
