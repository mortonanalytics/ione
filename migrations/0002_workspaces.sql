CREATE TYPE workspace_lifecycle AS ENUM ('continuous', 'bounded');

CREATE TABLE workspaces (
    id           UUID                NOT NULL DEFAULT gen_random_uuid(),
    org_id       UUID                NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    parent_id    UUID                REFERENCES workspaces(id) ON DELETE CASCADE,
    name         TEXT                NOT NULL,
    domain       TEXT                NOT NULL DEFAULT 'generic',
    lifecycle    workspace_lifecycle NOT NULL DEFAULT 'continuous',
    end_condition JSONB,
    metadata     JSONB               NOT NULL DEFAULT '{}'::jsonb,
    created_at   TIMESTAMPTZ         NOT NULL DEFAULT now(),
    closed_at    TIMESTAMPTZ,
    PRIMARY KEY (id)
);

CREATE TABLE roles (
    id           UUID  PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID  NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    name         TEXT  NOT NULL,
    coc_level    INT   NOT NULL DEFAULT 0,
    permissions  JSONB NOT NULL DEFAULT '{}'::jsonb,
    UNIQUE (workspace_id, name)
);

CREATE TABLE memberships (
    id                  UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id             UUID        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    workspace_id        UUID        NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    role_id             UUID        NOT NULL REFERENCES roles(id) ON DELETE RESTRICT,
    federated_claim_ref TEXT,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (user_id, workspace_id, role_id)
);

-- Backfill: create an "Operations" workspace in the default org (the oldest org),
-- then point any orphan conversations (workspace_id IS NULL) at it.
-- If no org exists yet (fresh DB before app boot), the WHERE EXISTS guard
-- means zero rows are inserted and zero rows are updated, so the migration
-- still succeeds. Bootstrap will create the Operations workspace idempotently
-- at app startup.
WITH default_org AS (
    SELECT id FROM organizations ORDER BY created_at ASC LIMIT 1
),
inserted_ws AS (
    INSERT INTO workspaces (org_id, name, domain, lifecycle)
    SELECT id, 'Operations', 'generic', 'continuous'
    FROM default_org
    WHERE EXISTS (SELECT 1 FROM default_org)
    RETURNING id
)
UPDATE conversations
SET workspace_id = (SELECT id FROM inserted_ws)
WHERE workspace_id IS NULL
  AND EXISTS (SELECT 1 FROM inserted_ws);

ALTER TABLE conversations
    ALTER COLUMN workspace_id SET NOT NULL,
    ADD CONSTRAINT conversations_workspace_fk
        FOREIGN KEY (workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE;
