# IONe Complete — Layer Contract

Extends [ione-v1-contract.md](ione-v1-contract.md). DB: snake_case. Rust: snake_case. JS/TS: camelCase.

## New entities

| Entity | DB table | Rust type | JS interface |
|--------|----------|-----------|--------------|
| activation progress step | `activation_progress` | `ActivationProgress` | `ActivationProgress` |
| activation dismissal | `activation_dismissals` | `ActivationDismissal` | `ActivationDismissal` |
| pipeline event | `pipeline_events` | `PipelineEvent` | `PipelineEvent` |
| funnel event | `funnel_events` | `FunnelEvent` | `FunnelEvent` |
| oauth client | `oauth_clients` | `OauthClient` | `OauthClient` |
| oauth auth code | `oauth_auth_codes` | `OauthAuthCode` | _(server-only)_ |
| oauth access token | `oauth_access_tokens` | `OauthAccessToken` | _(server-only)_ |
| oauth refresh token | `oauth_refresh_tokens` | `OauthRefreshToken` | _(server-only)_ |

## Extended entities

- `peers`: gains `oauth_client_id`, `access_token_hash`, `refresh_token_hash`, `token_expires_at`, `tool_allowlist`, `status` enum.
- Demo workspace marker: Rust const `DEMO_WORKSPACE_ID = uuid!("00000000-0000-0000-0000-000000000d30")`. No schema change on `workspaces`.

## Enums

| Enum | DB | Variants |
|------|----|----------|
| `activation_track` | TEXT CHECK IN | `demo_walkthrough`, `real_activation` |
| `activation_step_key` | TEXT CHECK IN | demo: `asked_demo_question`, `opened_demo_survivor`, `reviewed_demo_approval`, `viewed_demo_audit`; real: `added_connector`, `first_signal`, `first_approval_decided`, `first_audit_viewed` |
| `pipeline_event_stage` | TEXT CHECK IN | `publish_started`, `first_event`, `first_signal`, `first_survivor`, `first_decision`, `stall`, `error` |
| `peer_status` | TEXT CHECK IN | `pending_oauth`, `pending_allowlist`, `active`, `revoked` |
| `funnel_event_kind` | TEXT | open-ended (catalog lives in design doc §7) |
| `connector_kind` | existing ENUM | existing |

## Fields

### activation_progress
| Field | DB | Rust | JS | Type |
|-------|----|------|----|------|
| user_id | `user_id` | `user_id` | `userId` | UUID → users(id) |
| workspace_id | `workspace_id` | `workspace_id` | `workspaceId` | UUID → workspaces(id) |
| track | `track` | `track` | `track` | `activation_track` |
| step_key | `step_key` | `step_key` | `stepKey` | `activation_step_key` |
| completed_at | `completed_at` | `completed_at` | `completedAt` | TIMESTAMPTZ |

PK `(user_id, workspace_id, track, step_key)`.

### activation_dismissals
| Field | DB | Rust | JS | Type |
|-------|----|------|----|------|
| user_id, workspace_id, track | same as above | | | |
| dismissed_at | `dismissed_at` | `dismissed_at` | `dismissedAt` | TIMESTAMPTZ |

PK `(user_id, workspace_id, track)`.

### pipeline_events
| Field | DB | Rust | JS | Type |
|-------|----|------|----|------|
| id | `id` | `id` | `id` | UUID |
| workspace_id | `workspace_id` | `workspace_id` | `workspaceId` | UUID → workspaces(id) |
| connector_id | `connector_id` | `connector_id` | `connectorId` | UUID NULL → connectors(id) |
| stream_id | `stream_id` | `stream_id` | `streamId` | UUID NULL → streams(id) |
| stage | `stage` | `stage` | `stage` | `pipeline_event_stage` |
| detail | `detail` | `detail` | `detail` | JSONB NULL |
| occurred_at | `occurred_at` | `occurred_at` | `occurredAt` | TIMESTAMPTZ |

### funnel_events
| Field | DB | Rust | JS | Type |
|-------|----|------|----|------|
| id | `id` | `id` | `id` | UUID |
| user_id | `user_id` | `user_id` | `userId` | UUID NULL |
| session_id | `session_id` | `session_id` | `sessionId` | UUID |
| workspace_id | `workspace_id` | `workspace_id` | `workspaceId` | UUID NULL |
| event_kind | `event_kind` | `event_kind` | `eventKind` | TEXT |
| detail | `detail` | `detail` | `detail` | JSONB NULL |
| occurred_at | `occurred_at` | `occurred_at` | `occurredAt` | TIMESTAMPTZ |

### oauth_clients
| Field | DB | Rust | JS | Type |
|-------|----|------|----|------|
| id | `id` | `id` | `id` | UUID |
| client_id | `client_id` | `client_id` | `clientId` | TEXT UNIQUE |
| client_metadata | `client_metadata` | `client_metadata` | `clientMetadata` | JSONB |
| registered_by_user_id | `registered_by_user_id` | `registered_by_user_id` | `registeredByUserId` | UUID NULL → users |
| display_name | `display_name` | `display_name` | `displayName` | TEXT |
| created_at | `created_at` | `created_at` | `createdAt` | TIMESTAMPTZ |
| last_seen_at | `last_seen_at` | `last_seen_at` | `lastSeenAt` | TIMESTAMPTZ NULL |

### oauth_auth_codes
| Field | DB | Rust | Type |
|-------|----|------|------|
| code | `code` | `code` | TEXT PK |
| client_id | `client_id` | `client_id` | TEXT → oauth_clients(client_id) |
| user_id | `user_id` | `user_id` | UUID → users(id) |
| redirect_uri | `redirect_uri` | `redirect_uri` | TEXT |
| scope | `scope` | `scope` | TEXT |
| code_challenge | `code_challenge` | `code_challenge` | TEXT |
| code_challenge_method | `code_challenge_method` | `code_challenge_method` | TEXT (`S256`) |
| expires_at | `expires_at` | `expires_at` | TIMESTAMPTZ (10 min) |
| consumed_at | `consumed_at` | `consumed_at` | TIMESTAMPTZ NULL |

### oauth_access_tokens
| Field | DB | Rust | Type |
|-------|----|------|------|
| token_hash | `token_hash` | `token_hash` | TEXT PK (sha256 hex) |
| client_id | `client_id` | `client_id` | TEXT → oauth_clients(client_id) |
| user_id | `user_id` | `user_id` | UUID → users(id) |
| scope | `scope` | `scope` | TEXT |
| expires_at | `expires_at` | `expires_at` | TIMESTAMPTZ (1 h) |
| created_at | `created_at` | `created_at` | TIMESTAMPTZ |
| revoked_at | `revoked_at` | `revoked_at` | TIMESTAMPTZ NULL |

### oauth_refresh_tokens
Same shape, `expires_at` = 30 days, single-use (rotation sets `revoked_at` on issue of next).

### peers (extended columns)
| Field | DB | Rust | JS | Type |
|-------|----|------|----|------|
| oauth_client_id | `oauth_client_id` | `oauth_client_id` | `oauthClientId` | TEXT NULL |
| access_token_hash | `access_token_hash` | `access_token_hash` | _(server-only)_ | TEXT NULL |
| refresh_token_hash | `refresh_token_hash` | `refresh_token_hash` | _(server-only)_ | TEXT NULL |
| token_expires_at | `token_expires_at` | `token_expires_at` | `tokenExpiresAt` | TIMESTAMPTZ NULL |
| tool_allowlist | `tool_allowlist` | `tool_allowlist` | `toolAllowlist` | JSONB (string[]) |
| status | `status` | `status` | `status` | `peer_status` |

## Error envelope

All 4xx/5xx JSON responses:
```
{ "error": "kebab_snake_kind", "message": "user-facing sentence", "hint"?: "what to do", "field"?: "form field name" }
```

Known error kinds (non-exhaustive): `demo_read_only`, `ollama_unreachable`, `ollama_model_missing`, `nws_out_of_range`, `firms_auth_failed`, `s3_access_denied`, `validation_failed`, `peer_unreachable`, `manifest_timeout`, `oauth_denied`, `oauth_token_expired`.

## API operations

### Activation
| Op | Method | Path | Request | Response |
|----|--------|------|---------|----------|
| list activation | GET | `/api/v1/activation?workspace_id&track` | — | `{ track, items, dismissed }` |
| mark step | POST | `/api/v1/activation/events` | `{ track, stepKey, workspaceId }` | `{ ok: true }` |
| dismiss track | POST | `/api/v1/activation/dismiss` | `{ workspaceId, track }` | `{ ok: true }` |

### Health
| Op | Method | Path | Response |
|----|--------|------|----------|
| ollama health | GET | `/api/v1/health/ollama` | `{ ok, baseUrl, models: { required, available, missing }, error? }` |

### Connectors
| Op | Method | Path | Request | Response |
|----|--------|------|---------|----------|
| validate connector config | POST | `/api/v1/connectors/validate` | `{ kind, name, config }` | `{ ok, sample?, error?, hint?, field? }` |
| create connector (modified) | POST | `/api/v1/workspaces/:id/connectors` | (existing, but now server-side validates before insert) | existing + first pipeline_events |

### Pipeline events
| Op | Method | Path | Request | Response |
|----|--------|------|---------|----------|
| list events | GET | `/api/v1/workspaces/:id/events` | `?connector_id&stage&limit&cursor` | `{ items, nextCursor }` |
| stream events | GET | `/api/v1/workspaces/:id/events/stream` | SSE handshake | SSE `event: pipeline_event\ndata: {json}` |

### Telemetry
| Op | Method | Path | Request | Response |
|----|--------|------|---------|----------|
| track event | POST | `/api/v1/telemetry/events` | `{ eventKind, detail?, workspaceId? }` | `{ ok: true }` |
| funnel counts | GET | `/api/v1/admin/funnel?from&to` | — | `{ counts, conversions }` (404 unless `IONE_ADMIN_FUNNEL=1`) |

### OAuth + MCP
| Op | Method | Path | Request | Response |
|----|--------|------|---------|----------|
| discovery | GET | `/.well-known/oauth-authorization-server` | — | CIMD JSON |
| register | POST | `/mcp/oauth/register` | `{ clientMetadataUrl }` or raw metadata | `{ clientId }` |
| authorize | GET | `/mcp/oauth/authorize` | query params | 302 |
| token | POST | `/mcp/oauth/token` | OAuth params | `{ accessToken, refreshToken, tokenType, expiresIn, scope }` |
| revoke | POST | `/mcp/oauth/revoke` | `{ token, token_type_hint? }` | 200 |
| mcp (existing) | ALL | `/mcp/*` | MCP JSON-RPC | MCP JSON-RPC; 401 w/ WWW-Authenticate |
| list clients | GET | `/api/v1/mcp/clients` | — | `{ items }` |
| revoke client | DELETE | `/api/v1/mcp/clients/:id` | — | `{ ok: true }` |

### Peers
| Op | Method | Path | Request | Response |
|----|--------|------|---------|----------|
| begin federation | POST | `/api/v1/peers` | `{ peerUrl }` | `{ id, status: "pending_oauth", authorizeUrl }` |
| callback | GET | `/api/v1/peers/:id/callback` | `?code&state` | 302 |
| manifest | GET | `/api/v1/peers/:id/manifest` | — | `{ tools }` |
| authorize allowlist | POST | `/api/v1/peers/:id/authorize` | `{ toolAllowlist }` | `{ id, status: "active" }` |
| revoke peer | DELETE | `/api/v1/peers/:id` | — | `{ ok: true }` |

### Modified existing
- `POST /api/v1/conversations/:id/messages` — demo workspace → canned path, bypass Ollama. Real workspace Ollama failures → 503 `ollama_unreachable` or `ollama_model_missing`.
- `POST /api/v1/workspaces/:id/connectors` — runs validate internally, 422 on failure; synchronously emits `publish_started` + `first_event` before returning.
- Any non-GET/HEAD on demo workspace resources → 403 `demo_read_only`.
