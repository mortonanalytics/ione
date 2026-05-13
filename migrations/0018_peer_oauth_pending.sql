CREATE TABLE peer_oauth_pending (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    peer_id       UUID NOT NULL REFERENCES peers(id) ON DELETE CASCADE,
    nonce         TEXT NOT NULL UNIQUE,
    code_verifier TEXT NOT NULL,
    expires_at    TIMESTAMPTZ NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX peer_oauth_pending_expires ON peer_oauth_pending(expires_at);
