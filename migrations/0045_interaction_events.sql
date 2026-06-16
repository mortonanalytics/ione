-- Async observability data plane for federated tool interactions.
-- audit_events remains the synchronous compliance trail; this table is the
-- high-volume query/replay surface.

CREATE TABLE interaction_events (
    id                UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id            UUID        NOT NULL REFERENCES organizations(id) ON DELETE RESTRICT,
    workspace_id      UUID        NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    peer_id           UUID        NOT NULL REFERENCES peers(id) ON DELETE CASCADE,
    peer_name         TEXT        NOT NULL,
    tool_name         TEXT        NOT NULL,
    caller_kind       actor_kind  NOT NULL,
    caller_user_id    UUID        REFERENCES users(id) ON DELETE SET NULL,
    caller_peer_id    UUID        REFERENCES peers(id) ON DELETE SET NULL,
    caller_token_id   UUID        REFERENCES service_account_tokens(id) ON DELETE SET NULL,
    session_id        UUID,
    sequence_number   BIGINT,
    outcome           TEXT        NOT NULL CHECK (outcome IN ('allow', 'deny', 'pending', 'error')),
    latency_ms        INTEGER,
    detail            JSONB       NOT NULL DEFAULT '{}'::jsonb,
    recorded_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT interaction_latency_non_negative CHECK (latency_ms IS NULL OR latency_ms >= 0),
    CONSTRAINT interaction_session_sequence_pair CHECK (
        (session_id IS NULL AND sequence_number IS NULL)
        OR (session_id IS NOT NULL AND sequence_number IS NOT NULL)
    ),
    CONSTRAINT interaction_caller_present CHECK (
        caller_user_id IS NOT NULL
        OR caller_peer_id IS NOT NULL
        OR caller_token_id IS NOT NULL
    )
);

CREATE INDEX interaction_events_workspace_recorded_idx
    ON interaction_events (workspace_id, recorded_at DESC, id DESC);

CREATE INDEX interaction_events_workspace_peer_recorded_idx
    ON interaction_events (workspace_id, peer_id, recorded_at DESC);

CREATE INDEX interaction_events_workspace_outcome_recorded_idx
    ON interaction_events (workspace_id, outcome, recorded_at DESC);

CREATE INDEX interaction_events_session_sequence_idx
    ON interaction_events (session_id, sequence_number)
    WHERE session_id IS NOT NULL;

CREATE INDEX interaction_events_caller_recorded_idx
    ON interaction_events (workspace_id, caller_kind, caller_user_id, caller_peer_id, caller_token_id, recorded_at DESC);

ALTER TABLE interaction_events ENABLE ROW LEVEL SECURITY;
CREATE POLICY interaction_events_org_isolation ON interaction_events
    USING (org_id = current_setting('app.current_org_id', true)::uuid);
