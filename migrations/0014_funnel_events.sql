CREATE TABLE funnel_events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NULL REFERENCES users(id) ON DELETE SET NULL,
    session_id UUID NOT NULL,
    workspace_id UUID NULL REFERENCES workspaces(id) ON DELETE SET NULL,
    event_kind TEXT NOT NULL,
    detail JSONB,
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX funnel_events_kind_time_idx
    ON funnel_events (event_kind, occurred_at DESC);
CREATE INDEX funnel_events_session_time_idx
    ON funnel_events (session_id, occurred_at);
