# Intake — Tri-Thrust Advance (2026-06-21)

Canonical work-item ledger from the `ione-tri-thrust` dynamic workflow
(4 rounds, 45 survivors → deduped below) plus direct code verification.
Every item has an explicit disposition: **Shipped / Scheduled / Deferred / Out-of-scope**.

Thrusts: **A** = federation data infrastructure · **B** = UI/UX generalization · **C** = AI-augmented ingestion.

## Shipped this session (landed on working tree, `cargo check`+`clippy`+`fmt` green)

| ID | Item | Disposition | Evidence |
|----|------|-------------|----------|
| TT-A01 | SQL injection in critic evidence query → parameterized `= ANY($1)` over parsed `Vec<Uuid>` | **Shipped** | `src/services/critic.rs:175` |
| TT-A02 | Reject bindings/subscriptions to non-active peers (route guard + repo `p.status='active'` backstop) | **Shipped** | `src/routes/peers.rs:291`, `src/repos/workspace_peer_binding_repo.rs:233` |
| TT-A03 | Re-validate peer + binding status at approval **execution** time | **Shipped** (core guard) | `src/services/federation.rs:357` |
| TT-A04 | `peers:manage` RBAC gate on create/patch/delete/refresh binding routes | **Shipped** (backend) | `src/routes/bindings.rs` ×4 |
| TT-A05 | TOCTOU race in MCP session init → atomic DashMap `entry()` | **Shipped** | `src/connectors/peer_session.rs:57` |
| TT-C09 | 100 KB domain-agnostic cap on webhook `data` (flows into critic LLM prompt) | **Shipped** | `src/routes/webhooks.rs:236` |

## Scheduled (verified-real, queued for the next landing wave)

| ID | Item | Pairing | Source survivors |
|----|------|---------|------------------|
| TT-A06 | `mcp_client::resolve_workspace_ids_with_binding` falls back to **unscoped** `resolve_all_peer_workspace_ids()` when binding inactive/absent → cross-workspace data leak. **Fail closed.** | UI: binding-required chip (`app.js` buildConnectorCard) + `style.css` | #3, #22, #42 |
| TT-A07 | `subscribe_peer` triggers first-poll even when `bind_on_subscribe` failed → unscoped polling. Guard poll on binding `Active`. | UI: pending callout copy (`app.js`) | #5 |
| TT-A03b | Add observable **DENY interaction_event** trail + UI toast to the execution-time revalidation (TT-A03 shipped the guard; this adds audit + UI) | UI: approve-button catch branch (`app.js:4514`) | #6, #15 |
| TT-A04b | UI probe-and-hide for binding Refresh/Edit/Delete buttons (pairs TT-A04 backend gate) | backend already enforced | #1 |
| TT-B01 | Backend-driven event-detail field schema — `event_layers.rs` `PropertyField{label,format}`+`PropertyFormat`; `app.js` renders from `layer.propertyFields` (kills magnitude/depth/PAGER hardcode) | new req doc `event-view-schema.md` | #2,#12,#13,#14,#20 |
| TT-C01 | `stream_event` payload size cap (64 KiB) at repo choke point, mirroring `interaction_events` 4096 cap; `InsertOutcome::Rejected` + skipped counter | UI: skipped-events note (`app.js`) | #4, #27 |
| TT-C02 | Unescaped user data (signal titles, stream names) inlined into LLM prompts → prompt-injection vector; delimit/escape | backend-only | #8 |
| TT-C08 | `bucket_expr` panics on unvalidated bucket in aggregate repos → return `Result`; single-source bucket allowlist; add `minute` arm | UI: align bucket selector options | design #7 residual |

## Deferred (real but need design / intent / migration before code)

| ID | Item | Re-entry gate | Source |
|----|------|---------------|--------|
| TT-A08 | Pending-tool-call dedup excludes `executed` rows | **Owner intent**: is repeat-execution with identical args intended? If dedup desired → new migration + DB verification | #16, #29 |
| TT-A09 | Advisory lock in approval execution doesn't cover subsequent DB ops | Design: widen to a transaction or extend lock scope | #17 |
| TT-A10 | Peer-session task survives peer deletion (orphan) | Design: tie session lifecycle to peer delete | #28 |
| TT-B02 | Connector provider tiles/config backend-driven (kills Fire/Weather/Pipeline hardcode) | Design: backend config + discovery endpoint | #18,#23,#30,#36,#40 |
| TT-B03 | Panel-visibility contract decoupled from geospatial fields (data-presence nav) | Design: generalize `GET /workspaces/:id` presence counts | #32, #43 |
| TT-B04 | Generalize `eventLayers` naming geospatial→generic | Design: API rename, coordinate w/ TT-B01 | #37, #38 |
| TT-C03 | LLM stream-schema inference / validation-rule generation [L] | Design: validate against Postgres + MCP surface (no Iceberg/Git) | #10 |
| TT-C04 | LLM evalexpr rule authoring from rule diagnostics | Design | #24 |
| TT-C05 | Interaction-event intent/context classification at batch ingest | Design | #33 |
| TT-C06 | Generator stream selection deterministic first-only | Design: selection strategy | #39 |
| TT-C07 | stream-event field-presence/cardinality inference (feeds TT-C03) | Design | #44 |

## Out-of-scope

| ID | Item | Justification |
|----|------|---------------|
| TT-B05 | Demo chat prompts hardcoded fire/weather | Demo fixture, not product surface; lowest leverage (1.67–2.0); regenerate when demo content is revisited | 
| TT-X01 | Webhook `data` **schema-conformance** validation | A domain-agnostic substrate cannot assume a domain schema; the defensible part (size cap) shipped as TT-C09. Per-peer schema is a peer-app concern, not IONe's. |
| TT-X02 | Stream-events **external** ingestion size guard | No external POST path; stream events are internal-only. Residual malicious-peer-poll risk covered by TT-C01 (repo-level cap). |

## Coverage audit

All 45 workflow survivors map to an ID above (deduped). No silent drops:
- Thrust A (20 survivors) → TT-A01..A10 + TT-C09 (webhook). Shipped 5, Scheduled 4, Deferred 3.
- Thrust B (15 survivors) → TT-B01..B05. Scheduled 1, Deferred 3, Out-of-scope 1.
- Thrust C (10 survivors) → TT-C01..C08. Shipped 1, Scheduled 3, Deferred 5, Out-of-scope 1.
- `Unvalidated evidence query pattern in critic` (#9,#25) resolved by TT-A01; residual `bucket_expr` panic → TT-C08.

Every backend item has a paired UI task or an explicit "backend-only" / "Out-of-scope" note.
</content>
</invoke>
