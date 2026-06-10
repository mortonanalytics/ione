ALTER TYPE artifact_kind ADD VALUE IF NOT EXISTS 'tool_call';

ALTER TABLE peers ADD COLUMN last_manifest_jsonb JSONB;

CREATE TYPE pending_peer_tool_call_status AS ENUM (
    'pending',
    'approved',
    'rejected',
    'executed',
    'expired'
);

CREATE TABLE pending_peer_tool_calls (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    peer_id UUID NOT NULL REFERENCES peers(id) ON DELETE CASCADE,
    artifact_id UUID NOT NULL REFERENCES artifacts(id) ON DELETE CASCADE,
    approval_id UUID NOT NULL REFERENCES approvals(id) ON DELETE CASCADE,
    namespaced_tool TEXT NOT NULL,
    arguments_ciphertext BYTEA NOT NULL,
    arguments_digest TEXT NOT NULL,
    requested_by UUID NOT NULL REFERENCES users(id),
    status pending_peer_tool_call_status NOT NULL DEFAULT 'pending',
    expires_at TIMESTAMPTZ NOT NULL,
    approver_user_id UUID REFERENCES users(id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    executed_at TIMESTAMPTZ,
    result_ref JSONB
);

CREATE INDEX pending_peer_tool_calls_workspace_status
    ON pending_peer_tool_calls(workspace_id, status, created_at DESC);

CREATE UNIQUE INDEX pending_peer_tool_calls_idempotency
    ON pending_peer_tool_calls(workspace_id, arguments_digest)
    WHERE status IN ('pending', 'approved');
