CREATE TYPE connector_kind AS ENUM ('mcp', 'openapi', 'rust_native');
CREATE TYPE connector_status AS ENUM ('active', 'paused', 'error');

CREATE TABLE connectors (
    id           UUID             PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID             NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    kind         connector_kind   NOT NULL,
    name         TEXT             NOT NULL,
    config       JSONB            NOT NULL,
    status       connector_status NOT NULL DEFAULT 'active',
    last_error   TEXT,
    created_at   TIMESTAMPTZ      NOT NULL DEFAULT now()
);

CREATE TABLE streams (
    id           UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    connector_id UUID        NOT NULL REFERENCES connectors(id) ON DELETE CASCADE,
    name         TEXT        NOT NULL,
    schema       JSONB       NOT NULL DEFAULT '{}'::jsonb,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (connector_id, name)
);

CREATE TABLE stream_events (
    id          UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    stream_id   UUID        NOT NULL REFERENCES streams(id) ON DELETE CASCADE,
    payload     JSONB       NOT NULL,
    observed_at TIMESTAMPTZ NOT NULL,
    ingested_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    embedding   vector(768)
);

CREATE UNIQUE INDEX stream_events_stream_observed_unique ON stream_events(stream_id, observed_at);
CREATE INDEX stream_events_stream_observed ON stream_events(stream_id, observed_at DESC);
