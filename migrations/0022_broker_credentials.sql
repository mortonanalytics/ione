CREATE TABLE broker_credentials (
    id                       UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id                  UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    org_id                   UUID NOT NULL REFERENCES organizations(id),
    provider                 TEXT NOT NULL,
    label                    TEXT NOT NULL DEFAULT '',
    scopes                   TEXT[] NOT NULL DEFAULT '{}',
    access_token_ciphertext  BYTEA,
    refresh_token_ciphertext BYTEA,
    token_expires_at         TIMESTAMPTZ,
    state_token              TEXT,
    code_verifier            TEXT,
    state_expires_at         TIMESTAMPTZ,
    created_at               TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (user_id, provider, label)
);

CREATE INDEX broker_credentials_expiring ON broker_credentials(token_expires_at)
    WHERE access_token_ciphertext IS NOT NULL;
CREATE UNIQUE INDEX broker_state_token_unique
    ON broker_credentials(state_token) WHERE state_token IS NOT NULL;

ALTER TABLE broker_credentials ENABLE ROW LEVEL SECURITY;
CREATE POLICY broker_credentials_org_isolation ON broker_credentials
    USING (org_id = current_setting('app.current_org_id', true)::uuid);
