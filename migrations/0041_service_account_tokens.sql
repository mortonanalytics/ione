-- Service-account tokens: org-scoped, permission-carrying, hashed-at-rest
-- machine credentials for headless provisioning (design Slice 1).

-- (a) New audit actor variant. ADD VALUE must not be used in the same
-- transaction it is declared; nothing below references 'service_account', so
-- keeping it first in this file is safe (PG 12+ allows ADD VALUE in a txn as
-- long as the new value is not used within it).
ALTER TYPE actor_kind ADD VALUE IF NOT EXISTS 'service_account';

-- (b) Token table. token_hash is SHA-256 hex of the plaintext; plaintext is
-- shown once at issuance and never stored.
CREATE TABLE service_account_tokens (
    id                    UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id                UUID        NOT NULL REFERENCES organizations(id) ON DELETE RESTRICT,
    name                  TEXT        NOT NULL,
    token_hash            TEXT        NOT NULL,
    permissions           JSONB       NOT NULL DEFAULT '[]'::jsonb CHECK (jsonb_typeof(permissions) = 'array'),
    provisionable_max_coc INT         NOT NULL DEFAULT 0,
    created_by            UUID        REFERENCES users(id) ON DELETE SET NULL,
    expires_at            TIMESTAMPTZ,
    revoked_at            TIMESTAMPTZ,
    last_used_at          TIMESTAMPTZ,
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (org_id, name)
);

CREATE UNIQUE INDEX sat_token_hash_uniq ON service_account_tokens (token_hash);
CREATE INDEX sat_org_active ON service_account_tokens (org_id) WHERE revoked_at IS NULL;
CREATE INDEX sat_permissions_gin ON service_account_tokens USING gin (permissions);

CREATE OR REPLACE FUNCTION sat_touch_updated_at() RETURNS trigger AS $$
BEGIN NEW.updated_at := now(); RETURN NEW; END $$ LANGUAGE plpgsql;

CREATE TRIGGER sat_touch_updated_at_trg
BEFORE UPDATE ON service_account_tokens
FOR EACH ROW EXECUTE FUNCTION sat_touch_updated_at();

ALTER TABLE service_account_tokens ENABLE ROW LEVEL SECURITY;
CREATE POLICY sat_org_isolation ON service_account_tokens
    USING (org_id = current_setting('app.current_org_id', true)::uuid);

-- (c) Org backfill: anyone holding an admin role in any workspace of the org
-- gains the two new org-scoped grants, so the token endpoints are reachable on
-- day 1 (mirrors 0039's trust_issuers:manage/peers:manage backfill).
INSERT INTO org_memberships (user_id, org_id, permissions)
SELECT DISTINCT m.user_id, w.org_id, '["service_accounts:manage","provisioning:apply"]'::jsonb
FROM memberships m
     JOIN roles r ON r.id = m.role_id AND r.permissions @> '["admin"]'::jsonb
     JOIN workspaces w ON w.id = m.workspace_id
ON CONFLICT (user_id, org_id) DO UPDATE
  SET permissions = (
    SELECT COALESCE(jsonb_agg(DISTINCT p), '[]'::jsonb)
    FROM jsonb_array_elements(
      org_memberships.permissions || '["service_accounts:manage","provisioning:apply"]'::jsonb
    ) AS p
  );
