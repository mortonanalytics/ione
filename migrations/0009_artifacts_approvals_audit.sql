-- Phase 9: artifacts, approvals, audit_events

CREATE TYPE artifact_kind AS ENUM (
    'briefing',
    'notification_draft',
    'resource_order',
    'message',
    'report'
);

CREATE TYPE approval_status AS ENUM (
    'pending',
    'approved',
    'rejected'
);

CREATE TYPE actor_kind AS ENUM (
    'user',
    'system',
    'peer'
);

CREATE TABLE artifacts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    kind artifact_kind NOT NULL,
    source_survivor_id UUID REFERENCES survivors(id) ON DELETE SET NULL,
    content JSONB NOT NULL,
    blob_ref TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE approvals (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    artifact_id UUID NOT NULL REFERENCES artifacts(id) ON DELETE CASCADE,
    approver_user_id UUID REFERENCES users(id),
    status approval_status NOT NULL DEFAULT 'pending',
    comment TEXT,
    decided_at TIMESTAMPTZ
);

CREATE INDEX approvals_pending ON approvals(status) WHERE status = 'pending';

CREATE TABLE audit_events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID REFERENCES workspaces(id) ON DELETE SET NULL,
    actor_kind actor_kind NOT NULL,
    actor_ref TEXT NOT NULL,
    verb TEXT NOT NULL,
    object_kind TEXT NOT NULL,
    object_id UUID,
    payload JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX audit_events_workspace_created ON audit_events(workspace_id, created_at DESC);
