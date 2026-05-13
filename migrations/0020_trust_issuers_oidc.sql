ALTER TABLE trust_issuers
    ADD COLUMN idp_type TEXT NOT NULL DEFAULT 'oidc',
    ADD COLUMN max_coc_level INTEGER NOT NULL DEFAULT 100,
    ADD COLUMN client_id TEXT,
    ADD COLUMN client_secret_ciphertext BYTEA,
    ADD COLUMN display_name TEXT;

UPDATE trust_issuers SET client_id = audience WHERE client_id IS NULL;

CREATE TABLE federated_identities (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    issuer_id       UUID NOT NULL REFERENCES trust_issuers(id) ON DELETE RESTRICT,
    subject         TEXT NOT NULL,
    user_id         UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    last_seen_email TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (issuer_id, subject),
    UNIQUE (user_id, issuer_id)
);

CREATE INDEX federated_identities_user ON federated_identities(user_id);
