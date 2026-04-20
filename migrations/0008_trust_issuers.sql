CREATE TABLE trust_issuers (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id        UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    issuer_url    TEXT NOT NULL,
    audience      TEXT NOT NULL,
    jwks_uri      TEXT NOT NULL,
    claim_mapping JSONB NOT NULL,
    UNIQUE (org_id, issuer_url, audience)
);
