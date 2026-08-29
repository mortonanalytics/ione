-- Relay integration: which relay connections a workspace may query, and the
-- permission that gates asking.
--
-- The mapping is IONe's half of a two-sided authorization. Relay holds its own
-- grants and rechecks them on every boundary; this table says which of those
-- connections this workspace has been given a name for. A query is authorized
-- only where the two intersect, and neither side trusts the other's answer.
--
-- Nothing here stores a relay credential. The relay URL, runtime token, and
-- signing keys are server-side configuration; a row in this table names a
-- connection by the id relay issued and nothing more.

CREATE TABLE workspace_relay_mappings (
    id           UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id       UUID        NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    workspace_id UUID        NOT NULL REFERENCES workspaces(id)   ON DELETE CASCADE,

    -- The connection id relay issued. Opaque here; relay is the authority.
    relay_connection_id UUID NOT NULL,
    -- The name shown in the UI. IONe's label, not relay's.
    display_name TEXT        NOT NULL,
    -- The alias the query is written against, e.g. `pg`. Constrained to the
    -- same shape relay accepts, so a mapping cannot be created that relay will
    -- refuse at run time.
    alias        TEXT        NOT NULL,

    -- Who in this workspace may use the mapping. NULL means every member with
    -- the `data:query` permission; a value narrows it to one principal.
    principal_id UUID        REFERENCES users(id) ON DELETE CASCADE,

    enabled      BOOLEAN     NOT NULL DEFAULT true,
    created_by   UUID        NOT NULL REFERENCES users(id),
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now(),

    -- One alias per workspace: the alias is an identifier in the generated
    -- query, and two mappings sharing one would make the query ambiguous.
    CONSTRAINT workspace_relay_mappings_alias_unique
        UNIQUE (workspace_id, alias),
    -- Relay's alias rules, mirrored. Lowercase, digits, underscore, not
    -- starting with a digit.
    CONSTRAINT workspace_relay_mappings_alias_shape
        CHECK (alias ~ '^[a-z_][a-z0-9_]{0,62}$'),
    CONSTRAINT workspace_relay_mappings_display_name_present
        CHECK (length(trim(display_name)) > 0)
);

CREATE INDEX workspace_relay_mappings_by_workspace
    ON workspace_relay_mappings (workspace_id) WHERE enabled;
CREATE INDEX workspace_relay_mappings_by_org
    ON workspace_relay_mappings (org_id);

-- Org isolation, in the same shape as the other mapping tables and activated
-- the same way: the policy reads `app.current_org_id`, which `org_scoped_tx`
-- sets per transaction.
ALTER TABLE workspace_relay_mappings ENABLE ROW LEVEL SECURITY;
ALTER TABLE workspace_relay_mappings FORCE  ROW LEVEL SECURITY;

CREATE POLICY wrm_org_isolation ON workspace_relay_mappings
    USING (org_id = current_setting('app.current_org_id', true)::uuid);

-- A record of what was asked and what came back, without the answer. Counts,
-- states, and hashes -- never a row value, never the generated SQL's literals,
-- never a credential. The full audit lives in relay; this is the link that
-- lets an IONe operator find it.
CREATE TABLE relay_run_links (
    id           UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id       UUID        NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    workspace_id UUID        NOT NULL REFERENCES workspaces(id)   ON DELETE CASCADE,
    user_id      UUID        NOT NULL REFERENCES users(id),

    relay_run_id UUID        NOT NULL,
    outcome      TEXT        NOT NULL,
    -- Which mappings the run touched, by id. Not what it read.
    mapping_ids  UUID[]      NOT NULL DEFAULT '{}',
    row_count    BIGINT,
    truncated    BOOLEAN     NOT NULL DEFAULT false,
    refusal_reason TEXT,
    plan_hash    TEXT,
    policy_hash  TEXT,
    usage        JSONB       NOT NULL DEFAULT '{}'::jsonb,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),

    CONSTRAINT relay_run_links_run_unique UNIQUE (org_id, relay_run_id)
);

CREATE INDEX relay_run_links_by_workspace
    ON relay_run_links (workspace_id, created_at DESC);

ALTER TABLE relay_run_links ENABLE ROW LEVEL SECURITY;
ALTER TABLE relay_run_links FORCE  ROW LEVEL SECURITY;

CREATE POLICY rrl_org_isolation ON relay_run_links
    USING (org_id = current_setting('app.current_org_id', true)::uuid);

-- The restricted role gets the same access to the new tables as to the rest.
-- Migration 0050 set default privileges for tables created by the `ione` role,
-- so this is belt and braces -- but a table the restricted role cannot read is
-- a table whose RLS policy is untestable, and an untestable policy is one
-- nobody finds out is wrong.
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ione_app') THEN
        GRANT SELECT, INSERT, UPDATE, DELETE
            ON workspace_relay_mappings, relay_run_links TO ione_app;
    END IF;
END $$;

-- `data:query` gates asking a question of connected data. It is a separate
-- permission from the conversation surface on purpose: the two reach different
-- systems, and a role that may talk to the assistant is not automatically a
-- role that may query the warehouse.
--
-- Granted to workspace-admin roles only, in the same shape migration 0039 used
-- for its backfill. Every other role starts without it, so enabling the surface
-- for a team is a deliberate grant rather than something that happened during
-- a migration.
UPDATE roles
SET permissions = permissions || '["data:query"]'::jsonb
WHERE coc_level >= 80
  AND NOT (permissions @> '["data:query"]'::jsonb);
