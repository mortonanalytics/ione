-- RBAC scaffolding: permission vocabulary backfill + org-scoped grants.

-- (a) Workspace admin grant set: every role at coc_level >= 80 whose
-- permissions were never set keeps identical access through the cutover.
-- peers:manage is org-scoped (peer rows are org-scoped) and lives in the org
-- backfill below, not here; workspace admins still pass workspace-scoped
-- peers:manage checks via the `admin` short-circuit.
UPDATE roles SET permissions =
  '["admin","audit:read","roles:manage","approvals:decide","workspace:write","tool_invoke:*:*"]'::jsonb
  WHERE coc_level >= 80 AND permissions = '{}'::jsonb;

-- (b) Org-scoped grants (trust_issuers:manage, peers:manage in v1).
CREATE TABLE org_memberships (
    id          UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id     UUID        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    org_id      UUID        NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    permissions JSONB       NOT NULL DEFAULT '[]'::jsonb,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (user_id, org_id)
);

-- (c) Org backfill: anyone holding a coc >= 80 role in any workspace of the
-- org currently passes require_admin on trust-issuer and peer mutations;
-- grant the org-scoped equivalents so that access is preserved exactly.
INSERT INTO org_memberships (user_id, org_id, permissions)
SELECT DISTINCT m.user_id, w.org_id, '["trust_issuers:manage","peers:manage"]'::jsonb
FROM memberships m JOIN roles r ON r.id = m.role_id AND r.coc_level >= 80
                   JOIN workspaces w ON w.id = m.workspace_id
ON CONFLICT (user_id, org_id) DO UPDATE
  SET permissions = org_memberships.permissions || '["trust_issuers:manage","peers:manage"]'::jsonb;

CREATE INDEX roles_permissions_gin ON roles USING gin (permissions);
CREATE INDEX memberships_workspace_id ON memberships (workspace_id);
CREATE INDEX memberships_role_id ON memberships (role_id);
