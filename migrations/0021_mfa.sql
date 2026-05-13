CREATE TABLE mfa_enrollments (
    id                         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id                    UUID NOT NULL UNIQUE REFERENCES users(id) ON DELETE CASCADE,
    org_id                     UUID NOT NULL REFERENCES organizations(id),
    totp_secret_ciphertext     BYTEA NOT NULL,
    activated_at               TIMESTAMPTZ,
    recovery_codes_viewed_at   TIMESTAMPTZ,
    created_at                 TIMESTAMPTZ NOT NULL DEFAULT now()
);

ALTER TABLE mfa_enrollments ENABLE ROW LEVEL SECURITY;
CREATE POLICY mfa_enrollments_org_isolation ON mfa_enrollments
    USING (org_id = current_setting('app.current_org_id', true)::uuid);

CREATE TABLE mfa_recovery_codes (
    id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id    UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    org_id     UUID NOT NULL REFERENCES organizations(id),
    code_hash  TEXT NOT NULL,
    used_at    TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX mfa_recovery_unused ON mfa_recovery_codes(user_id) WHERE used_at IS NULL;

ALTER TABLE mfa_recovery_codes ENABLE ROW LEVEL SECURITY;
CREATE POLICY mfa_recovery_org_isolation ON mfa_recovery_codes
    USING (org_id = current_setting('app.current_org_id', true)::uuid);

ALTER TABLE organizations ADD COLUMN mfa_required_for_admins BOOLEAN NOT NULL DEFAULT false;
