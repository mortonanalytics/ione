CREATE TABLE auto_exec_policies (
    id                          UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id                      UUID        NOT NULL REFERENCES organizations(id) ON DELETE RESTRICT,
    workspace_id                UUID        NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    name                        TEXT        NOT NULL,
    trigger_signal_title_prefix TEXT        NULL,
    trigger_severity_at_most    TEXT        NULL CHECK (trigger_severity_at_most IN ('routine', 'flagged')),
    connector_id                UUID        NOT NULL REFERENCES connectors(id) ON DELETE RESTRICT,
    op                          TEXT        NOT NULL,
    args_template               JSONB       NOT NULL DEFAULT '{}'::jsonb,
    rate_limit_per_min          INT         NOT NULL CHECK (rate_limit_per_min BETWEEN 1 AND 1000),
    severity_cap                TEXT        NOT NULL DEFAULT 'routine' CHECK (severity_cap IN ('routine', 'flagged')),
    authorized_by_permission    TEXT        NOT NULL,
    enabled                     BOOLEAN     NOT NULL DEFAULT true,
    created_by                  UUID        NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    created_at                  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at                  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, name)
);

-- org_id always derives from the workspace; callers never supply it.
CREATE OR REPLACE FUNCTION aep_set_org_id() RETURNS trigger AS $$
BEGIN
    SELECT org_id INTO NEW.org_id FROM workspaces WHERE id = NEW.workspace_id;
    RETURN NEW;
END $$ LANGUAGE plpgsql;

CREATE TRIGGER aep_set_org_id_trg
BEFORE INSERT ON auto_exec_policies
FOR EACH ROW EXECUTE FUNCTION aep_set_org_id();

CREATE OR REPLACE FUNCTION aep_touch_updated_at() RETURNS trigger AS $$
BEGIN NEW.updated_at := now(); RETURN NEW; END $$ LANGUAGE plpgsql;

CREATE TRIGGER aep_touch_updated_at_trg
BEFORE UPDATE ON auto_exec_policies
FOR EACH ROW EXECUTE FUNCTION aep_touch_updated_at();

CREATE INDEX aep_workspace_enabled ON auto_exec_policies (workspace_id) WHERE enabled = true;
CREATE INDEX aep_connector ON auto_exec_policies (connector_id);

ALTER TABLE auto_exec_policies ENABLE ROW LEVEL SECURITY;
CREATE POLICY aep_org_isolation ON auto_exec_policies
    USING (org_id = current_setting('app.current_org_id', true)::uuid);
