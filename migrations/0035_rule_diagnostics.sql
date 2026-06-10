CREATE TABLE rule_diagnostics (
    workspace_id UUID PRIMARY KEY REFERENCES workspaces(id) ON DELETE CASCADE,
    evaluated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    items        JSONB       NOT NULL DEFAULT '[]'::jsonb
);
