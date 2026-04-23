-- Slice 4: pipeline event log (publish-don't-poll timeline)

CREATE TABLE pipeline_events (
    id           UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID        NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    connector_id UUID        NULL REFERENCES connectors(id) ON DELETE CASCADE,
    stream_id    UUID        NULL REFERENCES streams(id) ON DELETE SET NULL,
    stage        TEXT        NOT NULL CHECK (stage IN (
                                 'publish_started', 'first_event', 'first_signal',
                                 'first_survivor', 'first_decision', 'stall', 'error'
                             )),
    detail       JSONB,
    occurred_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX pipeline_events_workspace_time_idx
    ON pipeline_events (workspace_id, occurred_at DESC);

CREATE INDEX pipeline_events_connector_time_idx
    ON pipeline_events (connector_id, occurred_at DESC) WHERE connector_id IS NOT NULL;
