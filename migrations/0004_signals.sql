CREATE TYPE signal_source AS ENUM ('rule', 'connector_event', 'generator');
CREATE TYPE severity AS ENUM ('routine', 'flagged', 'command');

CREATE TABLE signals (
    id               UUID          PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id     UUID          NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    source           signal_source NOT NULL,
    title            TEXT          NOT NULL,
    body             TEXT          NOT NULL,
    evidence         JSONB         NOT NULL DEFAULT '[]'::jsonb,
    severity         severity      NOT NULL DEFAULT 'routine',
    generator_model  TEXT,
    created_at       TIMESTAMPTZ   NOT NULL DEFAULT now()
);

CREATE INDEX signals_workspace_created ON signals(workspace_id, created_at DESC);
