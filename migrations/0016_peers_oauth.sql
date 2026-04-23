ALTER TYPE peer_status ADD VALUE IF NOT EXISTS 'pending_oauth';
ALTER TYPE peer_status ADD VALUE IF NOT EXISTS 'pending_allowlist';
ALTER TYPE peer_status ADD VALUE IF NOT EXISTS 'revoked';

ALTER TABLE peers
    ADD COLUMN oauth_client_id TEXT NULL,
    ADD COLUMN access_token_hash TEXT NULL,
    ADD COLUMN refresh_token_hash TEXT NULL,
    ADD COLUMN token_expires_at TIMESTAMPTZ NULL,
    ADD COLUMN tool_allowlist JSONB NOT NULL DEFAULT '[]'::jsonb;
