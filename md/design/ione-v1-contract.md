# IONe v1 — Layer Contract

Canonical names and types. All code (SQL migrations, Rust structs, TS/JS interfaces) conforms to this file. DB: snake_case. Rust: snake_case. JS/TS: camelCase.

## Entities

| Entity | DB table | Rust type | JS interface |
|--------|----------|-----------|--------------|
| organization | `organizations` | `Organization` | `Organization` |
| user | `users` | `User` | `User` |
| workspace | `workspaces` | `Workspace` | `Workspace` |
| membership (user⇄workspace⇄role) | `memberships` | `Membership` | `Membership` |
| role | `roles` | `Role` | `Role` |
| conversation | `conversations` | `Conversation` | `Conversation` |
| message | `messages` | `Message` | `Message` |
| connector | `connectors` | `Connector` | `Connector` |
| stream | `streams` | `Stream` | `Stream` |
| stream event | `stream_events` | `StreamEvent` | `StreamEvent` |
| signal | `signals` | `Signal` | `Signal` |
| survivor | `survivors` | `Survivor` | `Survivor` |
| routing decision | `routing_decisions` | `RoutingDecision` | `RoutingDecision` |
| artifact | `artifacts` | `Artifact` | `Artifact` |
| approval | `approvals` | `Approval` | `Approval` |
| audit event | `audit_events` | `AuditEvent` | `AuditEvent` |
| trust issuer (OIDC) | `trust_issuers` | `TrustIssuer` | `TrustIssuer` |
| peer IONe | `peers` | `Peer` | `Peer` |
| pending peer tool call | `pending_peer_tool_calls` | `PendingPeerToolCall` | `PendingPeerToolCall` |

## Fields

### organization
| Field | DB column | Rust field | JS field | Type |
|-------|-----------|------------|----------|------|
| id | `id` | `id` | `id` | UUID |
| name | `name` | `name` | `name` | TEXT |
| created_at | `created_at` | `created_at` | `createdAt` | TIMESTAMPTZ |

### user
| Field | DB column | Rust field | JS field | Type |
|-------|-----------|------------|----------|------|
| id | `id` | `id` | `id` | UUID |
| org_id | `org_id` | `org_id` | `orgId` | UUID |
| email | `email` | `email` | `email` | TEXT |
| display_name | `display_name` | `display_name` | `displayName` | TEXT |
| oidc_subject | `oidc_subject` | `oidc_subject` | `oidcSubject` | TEXT NULL |
| created_at | `created_at` | `created_at` | `createdAt` | TIMESTAMPTZ |

### workspace
| Field | DB column | Rust field | JS field | Type |
|-------|-----------|------------|----------|------|
| id | `id` | `id` | `id` | UUID |
| org_id | `org_id` | `org_id` | `orgId` | UUID |
| parent_id | `parent_id` | `parent_id` | `parentId` | UUID NULL (sub-workspace) |
| name | `name` | `name` | `name` | TEXT |
| domain | `domain` | `domain` | `domain` | TEXT (free-form tag: fire-ops, fema, enterprise, …) |
| lifecycle | `lifecycle` | `lifecycle` | `lifecycle` | ENUM `workspace_lifecycle` |
| end_condition | `end_condition` | `end_condition` | `endCondition` | JSONB NULL |
| metadata | `metadata` | `metadata` | `metadata` | JSONB |
| created_at | `created_at` | `created_at` | `createdAt` | TIMESTAMPTZ |
| closed_at | `closed_at` | `closed_at` | `closedAt` | TIMESTAMPTZ NULL |

*`GET /api/v1/workspaces/:id` additionally injects a non-persisted `panels`
object (response extension; not a stored column) that drives adaptive tab
visibility:*

| JS field | Type | Meaning |
|----------|------|---------|
| `panels.charts` | int | count of native chart-capable streams (`view_config IS NOT NULL`) |
| `panels.tables` | int | count of native table-capable streams (`view_config ? 'property_fields'`) |
| `panels.hasActivePeer` | bool | an active peer binding exists (presence proxy for the federation-only Map/Document panels) |
| `panels.approvalsPending` | int | count of pending approvals in the workspace |

*Computed with cheap COUNT/EXISTS queries — no peer fan-out. `panels` is absent
from `GET /api/v1/workspaces` list items.*

### role
| Field | DB column | Rust field | JS field | Type |
|-------|-----------|------------|----------|------|
| id | `id` | `id` | `id` | UUID |
| workspace_id | `workspace_id` | `workspace_id` | `workspaceId` | UUID |
| name | `name` | `name` | `name` | TEXT (e.g., `division_sup`, `duty_officer`) |
| coc_level | `coc_level` | `coc_level` | `cocLevel` | INT (0 = top) |
| permissions | `permissions` | `permissions` | `permissions` | JSONB |

*Unique constraint: `(workspace_id, name, coc_level)` — same role name at different CoC levels is semantically distinct (e.g., deputy vs chief at different levels) and allowed.*

### membership
| Field | DB column | Rust field | JS field | Type |
|-------|-----------|------------|----------|------|
| id | `id` | `id` | `id` | UUID |
| user_id | `user_id` | `user_id` | `userId` | UUID |
| workspace_id | `workspace_id` | `workspace_id` | `workspaceId` | UUID |
| role_id | `role_id` | `role_id` | `roleId` | UUID |
| federated_claim_ref | `federated_claim_ref` | `federated_claim_ref` | `federatedClaimRef` | TEXT NULL |
| created_at | `created_at` | `created_at` | `createdAt` | TIMESTAMPTZ |

### conversation
| Field | DB column | Rust field | JS field | Type |
|-------|-----------|------------|----------|------|
| id | `id` | `id` | `id` | UUID |
| workspace_id | `workspace_id` | `workspace_id` | `workspaceId` | UUID NULL (Phase 1 = NULL ok) |
| user_id | `user_id` | `user_id` | `userId` | UUID NULL (Phase 1 = NULL ok) |
| title | `title` | `title` | `title` | TEXT |
| created_at | `created_at` | `created_at` | `createdAt` | TIMESTAMPTZ |

### message
| Field | DB column | Rust field | JS field | Type |
|-------|-----------|------------|----------|------|
| id | `id` | `id` | `id` | UUID |
| conversation_id | `conversation_id` | `conversation_id` | `conversationId` | UUID |
| role | `role` | `role` | `role` | ENUM `message_role` (`user`, `assistant`, `system`) |
| content | `content` | `content` | `content` | TEXT |
| model | `model` | `model` | `model` | TEXT NULL |
| tokens_in | `tokens_in` | `tokens_in` | `tokensIn` | INT NULL |
| tokens_out | `tokens_out` | `tokens_out` | `tokensOut` | INT NULL |
| created_at | `created_at` | `created_at` | `createdAt` | TIMESTAMPTZ |

### connector
| Field | DB column | Rust field | JS field | Type |
|-------|-----------|------------|----------|------|
| id | `id` | `id` | `id` | UUID |
| workspace_id | `workspace_id` | `workspace_id` | `workspaceId` | UUID |
| kind | `kind` | `kind` | `kind` | ENUM `connector_kind` (`mcp`, `openapi`, `rust_native`) |
| name | `name` | `name` | `name` | TEXT |
| config | `config` | `config` | `config` | JSONB |
| status | `status` | `status` | `status` | ENUM `connector_status` (`active`, `paused`, `error`) |
| last_error | `last_error` | `last_error` | `lastError` | TEXT NULL |
| created_at | `created_at` | `created_at` | `createdAt` | TIMESTAMPTZ |

### stream
| Field | DB column | Rust field | JS field | Type |
|-------|-----------|------------|----------|------|
| id | `id` | `id` | `id` | UUID |
| connector_id | `connector_id` | `connector_id` | `connectorId` | UUID |
| name | `name` | `name` | `name` | TEXT |
| schema | `schema` | `schema` | `schema` | JSONB |
| created_at | `created_at` | `created_at` | `createdAt` | TIMESTAMPTZ |

### stream_event
| Field | DB column | Rust field | JS field | Type |
|-------|-----------|------------|----------|------|
| id | `id` | `id` | `id` | UUID |
| stream_id | `stream_id` | `stream_id` | `streamId` | UUID |
| payload | `payload` | `payload` | `payload` | JSONB |
| observed_at | `observed_at` | `observed_at` | `observedAt` | TIMESTAMPTZ |
| ingested_at | `ingested_at` | `ingested_at` | `ingestedAt` | TIMESTAMPTZ |
| embedding | `embedding` | `embedding` | `embedding` | VECTOR(768) NULL |

### signal
| Field | DB column | Rust field | JS field | Type |
|-------|-----------|------------|----------|------|
| id | `id` | `id` | `id` | UUID |
| workspace_id | `workspace_id` | `workspace_id` | `workspaceId` | UUID |
| source | `source` | `source` | `source` | ENUM `signal_source` (`rule`, `connector_event`, `generator`) |
| title | `title` | `title` | `title` | TEXT |
| body | `body` | `body` | `body` | TEXT |
| evidence | `evidence` | `evidence` | `evidence` | JSONB (array of stream_event IDs and excerpts) |
| severity | `severity` | `severity` | `severity` | ENUM `severity` (`routine`, `flagged`, `command`) |
| generator_model | `generator_model` | `generator_model` | `generatorModel` | TEXT NULL |
| created_at | `created_at` | `created_at` | `createdAt` | TIMESTAMPTZ |

### survivor
| Field | DB column | Rust field | JS field | Type |
|-------|-----------|------------|----------|------|
| id | `id` | `id` | `id` | UUID |
| signal_id | `signal_id` | `signal_id` | `signalId` | UUID |
| critic_model | `critic_model` | `critic_model` | `criticModel` | TEXT |
| verdict | `verdict` | `verdict` | `verdict` | ENUM `critic_verdict` (`survive`, `reject`, `defer`) |
| rationale | `rationale` | `rationale` | `rationale` | TEXT |
| confidence | `confidence` | `confidence` | `confidence` | REAL (0.0–1.0) |
| chain_of_reasoning | `chain_of_reasoning` | `chain_of_reasoning` | `chainOfReasoning` | JSONB |
| created_at | `created_at` | `created_at` | `createdAt` | TIMESTAMPTZ |

### routing_decision
| Field | DB column | Rust field | JS field | Type |
|-------|-----------|------------|----------|------|
| id | `id` | `id` | `id` | UUID |
| survivor_id | `survivor_id` | `survivor_id` | `survivorId` | UUID |
| target_kind | `target_kind` | `target_kind` | `targetKind` | ENUM `routing_target` (`feed`, `notification`, `draft`, `peer`) |
| target_ref | `target_ref` | `target_ref` | `targetRef` | JSONB (role_id / peer_id / connector_id etc.) |
| classifier_model | `classifier_model` | `classifier_model` | `classifierModel` | TEXT |
| rationale | `rationale` | `rationale` | `rationale` | TEXT |
| created_at | `created_at` | `created_at` | `createdAt` | TIMESTAMPTZ |

### artifact
| Field | DB column | Rust field | JS field | Type |
|-------|-----------|------------|----------|------|
| id | `id` | `id` | `id` | UUID |
| workspace_id | `workspace_id` | `workspace_id` | `workspaceId` | UUID |
| kind | `kind` | `kind` | `kind` | ENUM `artifact_kind` (`briefing`, `notification_draft`, `resource_order`, `message`, `report`, `tool_call`) |
| source_survivor_id | `source_survivor_id` | `source_survivor_id` | `sourceSurvivorId` | UUID NULL |
| content | `content` | `content` | `content` | JSONB |
| blob_ref | `blob_ref` | `blob_ref` | `blobRef` | TEXT NULL (S3 key) |
| created_at | `created_at` | `created_at` | `createdAt` | TIMESTAMPTZ |

### approval
| Field | DB column | Rust field | JS field | Type |
|-------|-----------|------------|----------|------|
| id | `id` | `id` | `id` | UUID |
| artifact_id | `artifact_id` | `artifact_id` | `artifactId` | UUID |
| approver_user_id | `approver_user_id` | `approver_user_id` | `approverUserId` | UUID NULL (null while pending) |
| status | `status` | `status` | `status` | ENUM `approval_status` (`pending`, `approved`, `rejected`) |
| comment | `comment` | `comment` | `comment` | TEXT NULL |
| decided_at | `decided_at` | `decided_at` | `decidedAt` | TIMESTAMPTZ NULL |

### audit_event
| Field | DB column | Rust field | JS field | Type |
|-------|-----------|------------|----------|------|
| id | `id` | `id` | `id` | UUID |
| workspace_id | `workspace_id` | `workspace_id` | `workspaceId` | UUID NULL |
| actor_kind | `actor_kind` | `actor_kind` | `actorKind` | ENUM `actor_kind` (`user`, `system`, `peer`) |
| actor_ref | `actor_ref` | `actor_ref` | `actorRef` | TEXT |
| verb | `verb` | `verb` | `verb` | TEXT |
| object_kind | `object_kind` | `object_kind` | `objectKind` | TEXT |
| object_id | `object_id` | `object_id` | `objectId` | UUID NULL |
| payload | `payload` | `payload` | `payload` | JSONB |
| created_at | `created_at` | `created_at` | `createdAt` | TIMESTAMPTZ |

### trust_issuer
| Field | DB column | Rust field | JS field | Type |
|-------|-----------|------------|----------|------|
| id | `id` | `id` | `id` | UUID |
| org_id | `org_id` | `org_id` | `orgId` | UUID |
| issuer_url | `issuer_url` | `issuer_url` | `issuerUrl` | TEXT |
| audience | `audience` | `audience` | `audience` | TEXT |
| jwks_uri | `jwks_uri` | `jwks_uri` | `jwksUri` | TEXT |
| claim_mapping | `claim_mapping` | `claim_mapping` | `claimMapping` | JSONB |

### peer
| Field | DB column | Rust field | JS field | Type |
|-------|-----------|------------|----------|------|
| id | `id` | `id` | `id` | UUID |
| org_id | `org_id` | `org_id` | `orgId` | UUID |
| name | `name` | `name` | `name` | TEXT |
| mcp_url | `mcp_url` | `mcp_url` | `mcpUrl` | TEXT |
| issuer_id | `issuer_id` | `issuer_id` | `issuerId` | UUID (trust_issuer) |
| status | `status` | `status` | `status` | ENUM `peer_status` (`pending_oauth`, `pending_allowlist`, `active`, `revoked`, `paused`, `error`) |
| tool_allowlist | `tool_allowlist` | `tool_allowlist` | `toolAllowlist` | JSONB (operator-approved tool names) |
| tool_prefix | `tool_prefix` | `tool_prefix` | `toolPrefix` | VARCHAR(16) NULL — federation namespace, unique per `(org_id, tool_prefix)`, immutable once set (mig 0033) |
| session_status | `session_status` | `session_status` | `sessionStatus` | TEXT (`disconnected`\|`connecting`\|`live`\|`error`; free-form, default `disconnected`) (mig 0033) |
| last_connected_at | `last_connected_at` | `last_connected_at` | `lastConnectedAt` | TIMESTAMPTZ NULL (mig 0033) |
| last_session_error | `last_session_error` | `last_session_error` | `lastSessionError` | TEXT NULL (mig 0033) |
| last_manifest_jsonb | `last_manifest_jsonb` | `last_manifest_jsonb` | `lastManifestJsonb` | JSONB NULL — last-known-good manifest cache (mig 0034) |
| oauth_client_id | `oauth_client_id` | `oauth_client_id` | `oauthClientId` | TEXT NULL |
| access_token_ciphertext | `access_token_ciphertext` | `access_token_ciphertext` | `accessTokenCiphertext` | TEXT NULL (AES-256-GCM; `serde(skip)` in API) |
| refresh_token_ciphertext | `refresh_token_ciphertext` | `refresh_token_ciphertext` | `refreshTokenCiphertext` | TEXT NULL (AES-256-GCM; mig 0032) |
| token_expires_at | `token_expires_at` | `token_expires_at` | `tokenExpiresAt` | TIMESTAMPTZ NULL |
| sharing_policy | `sharing_policy` | `sharing_policy` | `sharingPolicy` | JSONB |
| created_at | `created_at` | `created_at` | `createdAt` | TIMESTAMPTZ |

### pending_peer_tool_call
*An approval-gated, agent-initiated `tools/call` durably parked until a human approves; executes exactly once on approval. Arguments encrypted at rest; replay-protected by `(workspace_id, arguments_digest)` unique partial index over non-terminal rows. (mig 0034)*

| Field | DB column | Rust field | JS field | Type |
|-------|-----------|------------|----------|------|
| id | `id` | `id` | `id` | UUID |
| workspace_id | `workspace_id` | `workspace_id` | `workspaceId` | UUID |
| peer_id | `peer_id` | `peer_id` | `peerId` | UUID |
| artifact_id | `artifact_id` | `artifact_id` | `artifactId` | UUID (artifact of kind `tool_call`) |
| approval_id | `approval_id` | `approval_id` | `approvalId` | UUID |
| namespaced_tool | `namespaced_tool` | `namespaced_tool` | `namespacedTool` | TEXT (`‹prefix›:‹tool›`) |
| arguments_ciphertext | `arguments_ciphertext` | `arguments_ciphertext` | — | BYTEA (`serde(skip)`; AES-256-GCM) |
| arguments_digest | `arguments_digest` | `arguments_digest` | `argumentsDigest` | TEXT (SHA-256, idempotency/replay key) |
| requested_by | `requested_by` | `requested_by` | `requestedBy` | UUID (user) |
| status | `status` | `status` | `status` | ENUM `pending_peer_tool_call_status` |
| expires_at | `expires_at` | `expires_at` | `expiresAt` | TIMESTAMPTZ |
| approver_user_id | `approver_user_id` | `approver_user_id` | `approverUserId` | UUID NULL |
| created_at | `created_at` | `created_at` | `createdAt` | TIMESTAMPTZ |
| executed_at | `executed_at` | `executed_at` | `executedAt` | TIMESTAMPTZ NULL |
| result_ref | `result_ref` | `result_ref` | `resultRef` | JSONB NULL |

## Enums

| Enum | DB representation | Variants |
|------|-------------------|----------|
| `workspace_lifecycle` | Postgres enum | `continuous`, `bounded` |
| `message_role` | Postgres enum | `user`, `assistant`, `system` |
| `connector_kind` | Postgres enum | `mcp`, `openapi`, `rust_native` |
| `connector_status` | Postgres enum | `active`, `paused`, `error` |
| `signal_source` | Postgres enum | `rule`, `connector_event`, `generator` |
| `severity` | Postgres enum | `routine`, `flagged`, `command` |
| `critic_verdict` | Postgres enum | `survive`, `reject`, `defer` |
| `routing_target` | Postgres enum | `feed`, `notification`, `draft`, `peer` |
| `artifact_kind` | Postgres enum | `briefing`, `notification_draft`, `resource_order`, `message`, `report`, `tool_call` (mig 0034) |
| `approval_status` | Postgres enum | `pending`, `approved`, `rejected` |
| `actor_kind` | Postgres enum | `user`, `system`, `peer` |
| `peer_status` | Postgres enum | `active`, `paused`, `error`, `pending_oauth`, `pending_allowlist`, `revoked` (migs 0010, 0016) |
| `pending_peer_tool_call_status` | Postgres enum | `pending`, `approved`, `rejected`, `executed`, `expired` (mig 0034) |

## API operations

| Operation | Method | Path | Request body | Response body |
|-----------|--------|------|--------------|---------------|
| health check | GET | `/api/v1/health` | — | `{ status: "ok", version }` |
| chat once (Phase 1) | POST | `/api/v1/chat` | `{ model?: string, prompt: string }` | `{ reply: string, model: string }` |
| list conversations | GET | `/api/v1/conversations` | — | `{ items: Conversation[] }` |
| create conversation | POST | `/api/v1/conversations` | `{ title?: string, workspaceId?: UUID }` | `Conversation` |
| get conversation | GET | `/api/v1/conversations/:id` | — | `{ conversation, messages: Message[] }` |
| post message | POST | `/api/v1/conversations/:id/messages` | `{ content: string, model?: string }` | `Message` (assistant reply) |
| list workspaces | GET | `/api/v1/workspaces` | — | `{ items: Workspace[] }` |
| create workspace | POST | `/api/v1/workspaces` | `{ name, domain, lifecycle, parentId? }` | `Workspace` |
| get workspace | GET | `/api/v1/workspaces/:id` | — | `Workspace` (+ `panels` summary) |
| close workspace | POST | `/api/v1/workspaces/:id/close` | `{}` | `Workspace` |
| list connectors | GET | `/api/v1/workspaces/:id/connectors` | — | `{ items: Connector[] }` |
| create connector | POST | `/api/v1/workspaces/:id/connectors` | `{ kind, name, config }` | `Connector` |
| list streams | GET | `/api/v1/connectors/:id/streams` | — | `{ items: Stream[] }` |
| poll stream | POST | `/api/v1/streams/:id/poll` | `{}` | `{ ingested: n }` |
| list signals | GET | `/api/v1/workspaces/:id/signals` | — | `{ items: Signal[] }` |
| list survivors | GET | `/api/v1/workspaces/:id/survivors` | — | `{ items: Survivor[] }` |
| list artifacts | GET | `/api/v1/workspaces/:id/artifacts` | — | `{ items: Artifact[] }` |
| list approvals | GET | `/api/v1/workspaces/:id/approvals?status=pending` | — | `{ items: Approval[] }` |
| decide approval | POST | `/api/v1/approvals/:id` | `{ decision: "approved"\|"rejected", comment? }` | `Approval` |
| list peers | GET | `/api/v1/peers` | — | `{ items: Peer[] }` |
| add peer (federated) | POST | `/api/v1/peers` | `{ peerUrl }` (preferred) — begins OAuth federation | `{ id, status: "pending_oauth", authorizeUrl }` |
| add peer (legacy) | POST | `/api/v1/peers` | `{ name, mcpUrl, issuerId, sharingPolicy }` (fallback shape, SSRF-guarded) | `Peer` |
| delete peer | DELETE | `/api/v1/peers/:id` | — | `204` |
| authorize peer | POST | `/api/v1/peers/:id/authorize` | `{}` | `{ authorizeUrl }` |
| peer OAuth callback | GET | `/api/v1/peers/callback` | `?code&state` | sets peer tokens, redirects |
| provision peer webhook | POST | `/api/v1/peers/:id/webhook/provision` | `{}` | `{ signingSecret, webhookUrl }` (secret shown once) |
| list peer bindings | GET | `/api/v1/peers/:id/bindings` | — | `{ items: WorkspacePeerBinding[] }` |
| get peer session | GET | `/api/v1/peers/:id/session` | — | `{ peerId, sessionStatus, lastConnectedAt, lastSessionError }` |
| reconnect peer session | POST | `/api/v1/peers/:id/session` (or `…/session/reconnect`) | `{}` | `{ peerId, sessionStatus: "connecting" }` |
| get peer manifest | GET | `/api/v1/peers/:id/manifest` | — | `{ tools, resources, stale, fetchedAt }` |
| refresh peer manifest | POST | `/api/v1/peers/:id/manifest/refresh` | `{}` | `{ tools, resources, fetchedAt }` |
| list peer tools (workspace-scoped) | GET | `/api/v1/workspaces/:id/peers/:peerId/tools` | — | `{ items: [{ name, namespaced, description, approvalRequired }] }` |
| list peer resources | GET | `/api/v1/workspaces/:id/peers/:peerId/resources` | — | `{ items: [{ uri, name, mimeType, ioneView }] }` |
| subscribe peer | POST | `/api/v1/workspaces/:id/peers/:peerId/subscribe` | `{}` | `{ ok }` |
| list context slices | GET | `/api/v1/workspaces/:id/context-slices` | — | `{ items: [{ peerId, summary, domainTags, sampleQueries, toolIndex }] }` |
| MCP server (Streamable HTTP, 2025-11-25) | POST | `/mcp` | JSON-RPC (`initialize`, aggregated workspace-scoped `tools/list`, namespaced `tools/call`, `resources/list`, `resources/read`); requires auth + `MCP-Session-Id` after init | JSON-RPC; `MCP-Protocol-Version` echoed; Origin-validated |
| MCP SSE stream | GET | `/mcp/sse` | — | `text/event-stream` (server→client notifications) |
| MCP session teardown | DELETE | `/mcp` | header `MCP-Session-Id` | `204` |
| OIDC callback | GET | `/auth/callback` | — | sets session |

*Approval-gated peer `tools/call`: when a tool's `ione_approval.required` is set, `/mcp tools/call` does not execute — it parks a `pending_peer_tool_call` (artifact kind `tool_call` + approval) and returns `{ status: "pending_approval", pendingId }`. `POST /api/v1/approvals/:id` with `approved` then executes the peer call exactly once.*

## Relationships

- `user` belongs to `organization` via `org_id`
- `workspace` belongs to `organization` via `org_id`; optionally references parent `workspace` via `parent_id`
- `role` belongs to `workspace` via `workspace_id`
- `membership` joins `user` × `workspace` × `role`
- `conversation` optionally belongs to `workspace` and `user`
- `message` belongs to `conversation`
- `connector` belongs to `workspace`
- `stream` belongs to `connector`
- `stream_event` belongs to `stream`
- `signal` belongs to `workspace`; evidence references `stream_event` ids
- `survivor` belongs to `signal` (1:1 for v1; critic runs once per signal)
- `routing_decision` belongs to `survivor`
- `artifact` belongs to `workspace`; optionally references `survivor`
- `approval` belongs to `artifact`
- `audit_event` optionally belongs to `workspace`
- `trust_issuer` belongs to `organization`
- `peer` belongs to `organization` via `org_id` and references `trust_issuer` via `issuer_id`
- `pending_peer_tool_call` belongs to `workspace`, `peer`, an `artifact` (kind `tool_call`), and an `approval`; references the requesting `user`
