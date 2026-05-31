# Document View — Implementation Plan

**Design doc:** `md/design/document-view.md`
**Shape:** medium (vertical slices, **sequential** — ~9 files, api + ui; **no new DB schema / no migration / no document-read path** — the route still queries existing tables for `ensure_workspace_in_org` + active peer bindings, like `table_panels`; this is NOT an `app_no_db`-style endpoint). The smallest of the four view panels.
**Stack:** Rust / Axum backend + static HTML + vanilla JS UI. Verification: `cargo clippy -D warnings`, `cargo test --test phase_document_panels -- --ignored --test-threads=1` (DB at `:5433` + `wiremock` peer), `npx playwright test` (server running). No render engine, no vendored assets.

This is **P7-supporting** (Stream P — the last P0 visualization view type).

## Dependencies
None. Reuses the shipped `table_panels`/`map_layers` peer fan-out, `url_guard`, and the table/chart panel UI patterns.

## Carry-forward defaults (from design Open Questions — not blocking)
1. **Chromium sandbox rung** — implementation spike selected `sandbox="allow-downloads allow-same-origin"`; Chromium did not expose a usable native PDF document at `allow-downloads` only. `allow-scripts` remains prohibited.
2. Embed-failure detection — post-`load` / ~3s `contentDocument` check catches failed/aborted frame loads → link-card fallback. Pure `X-Frame-Options` denial can be browser-error/blank-frame shaped under the no-proxy model, so the visible toolbar link remains mandatory even when inline preview is attempted.
3. `file_size_bytes` / `last_modified` — optional nullable, surfaced when the peer provides them.
4. App-wide CSP — deferred (SHOULD, its own spike); this plan ships the per-element security MUSTs only.

---

## Phase 0 — document tab skeleton (scaffolding, no feature)

**Goal:** a Documents tab opens an empty panel.

**Files:**
- `static/index.html` — **modify**. Add `<button id="tab-document" role="tab" aria-selected="false" aria-controls="panel-document" class="tab">Documents</button>` after `tab-table`; add a `panel-document` `role="tabpanel"` skeleton after `panel-table` (toolbar + `#document-list` pane + `#document-render` region + `#document-render-live` aria-live).
- `static/app.js` — **modify**. Add `tabDocument`/`panelDocument` consts (mirror `tabTable`/`panelTable` ~lines 1011/1019); add a `tab-document` click handler; **add `document` to the arrow-key roving-tablist order** (Table ↔ Document ↔ Connectors); extend `switchTab` to toggle `document`; add a `documentPanel` module with `init(workspaceId)` stub fired on first `switchTab('document')`; add `resetDocumentPanel()` and **call it in the workspace-switch path next to `resetTablePanel()` (~line 582)**.
- `static/style.css` — **modify**. Document panel two-column layout + a MIME-badge token; reuse existing list/skeleton/error/card tokens.
- `tests/e2e/document-panel.spec.ts` — **create** (#4). Skeleton test (`-g "skeleton"`) + the shared network-stub helpers used by `table-panel.spec.ts`/`chart-panel.spec.ts`; extended in Phase 1/2.

**Gate:**
```
npx playwright test tests/e2e/document-panel.spec.ts -g "skeleton"
```
**Acceptance:** Playwright loads `/`, clicks `#tab-document`, asserts `#panel-document` visible + `#tab-document[aria-selected=true]`; arrow-key from `#tab-table` reaches `#tab-document`.

---

## Phase 1 — Document discovery (Slice 1: endpoint + list)

**Goal:** the panel lists documents the workspace's peers publish; unsafe `download_url`s are dropped.

**Files:**
- `src/services/document_panels.rs` — **create** (mirror `src/services/table_panels.rs` peer fan-out; reuse its `PeerFetchError` + `resolve_token` patterns — or extract a shared helper if the third copy crosses the rule-of-three). `fetch_document_panels(http, peers)` → for each bound peer, MCP `resources/list`, keep resources with `metadata.ione_view == "document"`, extract the item fields, **validate `download_url` with a document-specific helper** — drop + `warn!` any item that fails; collect `peer_errors`.
  - **Validation (#1) — https-only, not bare `url_guard`.** `url_guard::ensure_safe_url` alone permits `http` to localhost/private (verified `src/util/url_guard.rs:37`), which violates the document contract. The validator MUST: parse the URL, **require `scheme == "https"`** (reject `http`/`javascript:`/`data:`/`file:`/all else), then call `url_guard::ensure_safe_url` for the link-local block. Private/loopback **https** stays allowed (on-prem). Implement as a small `fn validate_document_url(&str) -> Result<()>` in this module wrapping `url_guard`.
  - **`download_url` auth/lifetime (#2)** is a *peer* obligation (browser fetches it without IONe's token → must be public/presigned/peer-cookie, valid ≥5 min) — documented in the playbook contract; IONe enforces only scheme/host here, never proxies.
  - **JSON casing (#5):** all structs derive `#[serde(rename_all = "camelCase")]` (matching `table_panels.rs:13`). Wire keys: `peerDocuments`, `peerErrors`, `peerId`, `downloadUrl`, `mimeType`, `fileSizeBytes`, `lastModified`.
  ```rust
  #[derive(Serialize)] #[serde(rename_all = "camelCase")]
  pub struct DocumentPanelItem { id, name, source: String /* "peer" */, peer_id: Uuid, uri: String,
                                 download_url: String, mime_type: String,
                                 file_size_bytes: Option<i64>, last_modified: Option<String> }
  #[derive(Serialize)] #[serde(rename_all = "camelCase")]
  pub struct DocumentPanelsResponse { peer_documents: Vec<DocumentPanelItem>, peer_errors: Vec<PeerFetchError> }
  pub async fn fetch_document_panels(http: &Client, peers: Vec<Peer>) -> DocumentPanelsResponse
  ```
- `src/routes/document_panels.rs` — **create** (mirror `src/routes/table_panels.rs`). `GET` handler; `ensure_workspace_in_org`; `WorkspacePeerBindingRepo::list_active_peers_for_workspace`; `fetch_document_panels`; return `{peer_documents, peer_errors}`.
- `src/services/mod.rs`, `src/routes/mod.rs` — **modify**. `pub mod`; register `/api/v1/workspaces/:id/document-panels` (get).
- `static/app.js` — **modify**. `documentPanel`: fetch `document-panels`, render the list (mirror the table panel list + partial-failure rows + retry + live-region + loaded-workspace guard + AbortController); each row shows name + source label + MIME-type badge. Selecting a row stores the item for the render pane (Phase 2).
- `static/index.html` — **modify**. Flesh out the `#document-list` pane (toolbar refresh button, list `<ul role=list>`).
- `tests/phase_document_panels.rs` — **create**. DB-backed `#[ignore]` + `wiremock` peer (mirror `tests/phase_table_peer.rs`): AC-1 (discovery: item carries `peerId`/`uri`/`downloadUrl`/`mimeType` — assert **camelCase** keys), AC-2 (https-only: `javascript:`, `file:`, `http://example.com`, `http://127.0.0.1`, `http://10.0.0.5`, `https://169.254.169.254` each **omitted**; `https://example.com` **and** `https://10.0.0.5` both **present**), AC-3 (partial peer failure → reachable docs + `peerErrors`), AC-4 (cross-org → 404).

**Gate:**
```
cargo clippy --all-targets -- -D warnings
DATABASE_URL=postgres://ione:ione@localhost:5433/ione cargo test --test phase_document_panels -- --ignored --test-threads=1
```
**Acceptance:** AC-1..4 pass.

---

## Phase 2 — Document render + security (Slice 2 + Slice 3 MUSTs)

**Goal:** selecting a PDF renders inline in a sandboxed iframe; non-PDF → link card; blocked PDF → fallback card; security MUSTs enforced.

**Files:**
- `static/app.js` — **modify**. `documentPanel` render pane: on select, branch on `mime_type`:
  - `application/pdf` → create one `<iframe sandbox="allow-downloads allow-same-origin" referrerpolicy="no-referrer" title="<name> — PDF document" src=download_url>` filling the pane, with a toolbar ("Open in new tab" + "Download", both `<a target=_blank rel="noopener noreferrer">` / `download`). Post-`load` / timeout `contentDocument` check → on failed/aborted frame load, destroy iframe and render the link card + a polite "could not be displayed inline" notice. Do not add `allow-scripts`.
  - non-PDF → link card (name + MIME label + prominent "Open in new tab" `<a target=_blank rel="noopener noreferrer">`); **no iframe**.
  - Always render a programmatic fallback `<a>` even inside the iframe element. Destroy the iframe on re-select / workspace switch.
- `static/index.html` — **modify**. Flesh out `#document-render` (idle/loading/error states; iframe + toolbar container; link-card container).
- `Cargo.toml` + `src/routes/mod.rs` (or the `ione::app` router assembly) — **modify (#3).** Add `X-Content-Type-Options: nosniff` to IONe's own responses. `tower-http` currently enables only `["fs","trace","cors"]` (verified Cargo.toml:26) — so either **add the `set-header` feature** and use `SetResponseHeaderLayer::overriding`, or write a ~5-line custom map-response middleware (no new dep). Place the layer on the **outer** router so it covers both static files and API responses (around the router assembly at `src/routes/mod.rs:269`). (App-wide CSP is **deferred** per OQ-4.)
- `tests/e2e/document-panel.spec.ts` — **modify**. **First, the sandbox spike** (`-g "sandbox-spike"`): in-page, set a sandboxed iframe src to a fixture PDF and confirm which rung (`allow-downloads`, else `+allow-same-origin`) renders in Chromium; pin the chosen value as a constant the panel uses. Then: AC-5 (PDF inline — iframe `src==download_url`, sandbox contains **no `allow-scripts`** and never `allow-scripts`+`allow-same-origin`, no IONe-origin proxy fetch), AC-6 (non-PDF `text/csv` → `<a target=_blank rel=noopener noreferrer>`, **no iframe**), AC-7 (blocked/failed PDF request fixture → fallback card + working open-in-new-tab link), **AC-8 (#7 — assert ALL per-element MUSTs:** axe-clean; iframe `title` descriptive **and `referrerpolicy="no-referrer"`**; every open/download link has **`rel="noopener noreferrer"` + `target="_blank"`**; fallback link always present; open-in-new-tab announces new tab). Note the cross-origin `download` attribute is advisory (only forces save with peer download headers) — don't assert a forced save. Stub `document-panels` at the network layer; serve fixtures (a real small PDF + a blocked/failed PDF request) from the test.

**Gate:**
```
cargo clippy --all-targets -- -D warnings
npx playwright test tests/e2e/document-panel.spec.ts
```
**Acceptance:** AC-5..8 pass; the sandbox-spike test records the rung used (`allow-downloads allow-same-origin` in current Chromium). (`X-Content-Type-Options: nosniff` present on an IONe response — assert in a Rust route test or the e2e network log.)

---

## Requirements impact (post-merge, via `update-requirements` at `/preflight`/`/pr`)
Per design § Requirements impact — `app-integration-playbook.md` §4 already corrected (`document` → v0.1 + metadata contract, done in the design pass); `ione-substrate.md` remove `document` from v0.1 exclusions; `infrastructure-backlog.md` mark the P0 document-view item done (completes P0 visualization); `ux-security-audit-backlog.md` record the deferred app-wide CSP as a tracked hardening item.

---

## Self-review
1. **Every design AC maps to a phase gate?** Yes — AC-1..4 → Phase 1 (`phase_document_panels`); AC-5..8 → Phase 2 (Playwright + axe); skeleton + arrow-nav → Phase 0.
2. **Every file exists now or is listed to create?** Verified existing (modify targets): `static/index.html`, `static/app.js`, `static/style.css`, `src/services/mod.rs`, `src/routes/mod.rs`; precedents read (`table_panels.rs`, `map_layers.rs`, `url_guard.rs`, `tests/phase_table_peer.rs`). Created: `src/services/document_panels.rs`, `src/routes/document_panels.rs`, `tests/phase_document_panels.rs`, `tests/e2e/document-panel.spec.ts` (Phase 0, extended after).
3. **Vertical slices, not layer stacks?** Phase 0 = scaffolding (3 files, no feature). Phase 1 ships discovery end-to-end (service+route+list+tests). Phase 2 ships render + security end-to-end (UI render + nosniff + tests).
4. **Gates concrete shell commands?** Yes.
5. **Parallel tasks disjoint?** N/A — sequential. Phase 1 & 2 both touch `static/app.js`, `static/index.html`.

**Implementer notes:** (a) Playwright needs a running server (`IONE_TOKEN_KEY`+`IONE_WEBHOOK_SECRET_KEY`+`IONE_BIND=127.0.0.1:3007 cargo run`); the document e2e stubs `document-panels` at the network layer and serves its own PDF / blocked-request fixture. (b) DB test needs `--test-threads=1`. (c) **Security MUST invariant:** the iframe `sandbox` must never contain `allow-scripts`, and never `allow-scripts`+`allow-same-origin` together — treat the sandbox value as a security constant, not a tunable. Current Chromium uses `allow-downloads allow-same-origin`. (d) `download_url` validation is server-side and **https-only** (the document validator wraps `url_guard` + requires `scheme==https`; bare `url_guard` permits http-to-localhost/private — too loose here). The browser only ever embeds a validated https URL — never construct an href/src from unvalidated peer metadata. The URL's browser-fetchability (auth/lifetime) is a peer obligation per the playbook, not enforced by IONe. (e) no proxying: the browser fetches `download_url` from the peer origin directly.
