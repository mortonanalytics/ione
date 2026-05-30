ALTER TABLE peers
    ADD COLUMN refresh_token_ciphertext BYTEA NULL;

CREATE INDEX peers_refresh_token_expiring
    ON peers(token_expires_at)
    WHERE refresh_token_ciphertext IS NOT NULL;
