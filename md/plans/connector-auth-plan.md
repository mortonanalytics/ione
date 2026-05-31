# Connector & Data-Source Auth — Plan

**Date:** 2026-05-31
**Status:** Proposed. Prerequisite for any authenticated data source (most real federal/commercial feeds).
**Outcome ID:** P7 (IONe v0.1) — supporting.
**Related:** [identity-broker.md](../design/identity-broker.md) (S5 brokered SaaS OAuth — reuse its credential machinery), [ione-app-onramp-plan.md](ione-app-onramp-plan.md), [openapi-connectors.md](../design/openapi-connectors.md), [geojson-poll-connector.md](../design/geojson-poll-connector.md).

## Problem

Today's connectors (`openapi`, `geojson_poll`, FIRMS, IRWIN, NWS) assume public or single-static-key access. Most real data sources Morton will demo against need real auth: API keys, basic auth, OAuth 2.0 client-credentials (machine-to-machine), and OAuth 2.0 authorization-code with refresh (user-delegated — QuickBooks, Google, agency portals). Some federal endpoints require mTLS. There is no credential model for connectors and no place to store a secret encrypted at rest for a data source (as opposed to a *peer*, which already has encrypted token storage + an OAuth refresh path). Without this, authenticated artifact/connector ingest (ONR-001 and beyond) cannot reach gated sources.

**Reuse, don't rebuild:** IONe already has (a) AES-256-GCM column encryption (`IONE_TOKEN_KEY`), (b) a working OAuth 2.1 client dance and refresh scheduler for *peers* (migration 0032), and (c) the brokered-SaaS-OAuth design (`broker_credentials`, identity-broker S5). This plan generalizes those to data-source connectors rather than inventing a parallel system.

## Auth types to support (in priority order)

| Tier | Auth type | Covers | Mechanism |
|------|-----------|--------|-----------|
| 1 | `none` | public feeds (NWS, USGS open) | current behavior |
| 1 | `api_key` | header or query param key (FIRMS `MAP_KEY`, data.gov) | inject at poll time |
| 1 | `basic` / `bearer_static` | legacy portals, static tokens | inject at poll time |
| 2 | `oauth2_client_credentials` | machine-to-machine APIs (most enterprise) | client-credentials grant + cached token |
| 2 | `oauth2_auth_code` | user-delegated (QuickBooks, Google, Entra-gated APIs) | auth-code + PKCE + refresh (reuse peer dance) |
| 3 | `mtls` | some federal endpoints | client cert (defer; flag) |

## Task Manifest

| ID | Task | Layer | Depends on | Disposition |
|----|------|-------|-----------|-------------|
| CAU-001 | `connector_credentials` model + encryption (reuse `IONE_TOKEN_KEY`) | db | — | **Scheduled — Phase 1** |
| CAU-002 | Tier-1 injection (`api_key`/`basic`/`bearer_static`) in openapi + geojson_poll poll path | connector | CAU-001 | **Scheduled — Phase 1** |
| CAU-003 | Validate-time credential check (extend `POST /connectors/validate` to dry-run with creds) | api | CAU-001 | **Scheduled — Phase 1** |
| CAU-004 | `oauth2_client_credentials` grant + token cache + auto-refresh | connector + auth | CAU-001 | **Scheduled — Phase 2** |
| CAU-005 | `oauth2_auth_code` delegated dance + refresh (generalize peer OAuth + `broker_credentials`) | auth | CAU-004 | **Scheduled — Phase 2** |
| CAU-006 | One-time secret provisioning UX (enter once, encrypted, never returned) | ui + api | CAU-001 | **Scheduled — Phase 2** |
| CAU-007 | mTLS client-cert connector auth | connector | — | **Deferred — re-enter when a target source requires it** |

No silent drops: every auth type in the table above maps to a task or an explicit defer (CAU-007).

---

## Phase 1 — Static credentials (unblocks most demos)

**CAU-001 — Credential model.** New migration: `connector_credentials` keyed by `connector_id`, with `auth_type`, `secret_ciphertext` (AES-256-GCM, same key path as peer tokens), `config` (jsonb: header name, query param name, scopes, token_url, etc.), `expires_at` (nullable). RLS by `org_id`. Mirror the shape of the peer token columns so the encryption/rotation code is shared, not forked.

**CAU-002 — Tier-1 injection.** At poll time, the openapi/geojson_poll connectors load and decrypt the credential and inject it: `api_key` → configured header or query param; `basic` → `Authorization: Basic`; `bearer_static` → `Authorization: Bearer`. Never log the secret. SSRF guard already exists (IPv6-link-local fix, commit e239edb) — keep it on the authenticated path.

**CAU-003 — Validate with creds.** `POST /api/v1/connectors/validate` dry-runs the source with the supplied credential and returns auth-failure (401/403) distinctly from schema/connectivity failure, so an operator sees "bad key" vs "bad URL" before creating the connector.

**Acceptance:** create a FIRMS connector supplying `MAP_KEY` as `api_key` and a basic-auth OpenAPI source; both poll successfully; validate returns a clear auth error on a wrong key. Effort: ~2–3 days.

## Phase 2 — OAuth data sources

**CAU-004 — Client credentials.** For machine-to-machine APIs: exchange `client_id`/`client_secret` (stored via CAU-001) at the source's `token_url` for an access token, cache it with `expires_at`, auto-refresh before expiry on the poll path. This is the common enterprise case and needs no user interaction.

**CAU-005 — Authorization code (delegated).** For user-delegated sources (QuickBooks, Google, Entra-gated agency APIs): reuse the peer OAuth 2.1 dance (PKCE) and the refresh scheduler, persisting tokens in `connector_credentials` (or `broker_credentials` if per-user). This is the same machinery as peer federation and the identity-broker S5 slice — converge them rather than duplicate. Operator authorizes once; IONe refreshes thereafter.

**CAU-006 — Provisioning UX.** A connector "Credentials" panel: choose `auth_type`, enter the secret once (shown never again, like peer webhook secrets), rotate on demand. For OAuth, a "Connect" button launches the dance.

**Acceptance:** an OAuth client-credentials API and one auth-code source (e.g. QuickBooks sandbox) both ingest, survive a token expiry (auto-refresh), and the secret is never returned by any GET. Effort: ~4–5 days.

## Sequencing

```
Phase 1: CAU-001 -> CAU-002 ∥ CAU-003           (~3d)  <- unblocks API-key/basic demos
Phase 2: CAU-004 -> CAU-005, CAU-006 (parallel)  (~5d)  <- OAuth sources
mTLS (CAU-007) deferred until a real target needs it.
```

CAU-001 must land before everything. CAU-004/005 share machinery with peer OAuth — coordinate with `peer-token-refresh.md` to avoid a forked refresh scheduler.

## Relationship to the app on-ramp

- **Artifact ingest (ONR-001)** is independent of this for *public* artifacts, but any artifact fetched from a gated URL needs Tier-1 (CAU-002) at minimum. Sequence CAU-001/002 alongside ONR-001 Phase 1.
- **Federation peers** already use OAuth via the peer path; this plan is for *non-peer data sources* (connectors), the complementary case.

## Risks

- **Forked OAuth/refresh code.** Mitigation: CAU-004/005 must reuse the peer dance + scheduler (`src/oauth/`, peer refresh, migration 0032), not a copy. Converge with identity-broker S5.
- **Secret sprawl / logging leaks.** Mitigation: single encryption path (CAU-001), secrets never returned by any read endpoint, redaction in connector logs, reuse existing SSRF guard.
- **Scope creep into the full identity broker.** This plan is connector/data-source credentials only; operator/end-user identity stays in [identity-broker.md](../design/identity-broker.md).

## Definition of done

An operator can create a connector against an API-key, basic, OAuth-client-credentials, or OAuth-auth-code source; secrets are encrypted at rest and never returned; tokens auto-refresh; validate distinguishes auth from connectivity failure. mTLS (CAU-007) remains a flagged, deferred option.
