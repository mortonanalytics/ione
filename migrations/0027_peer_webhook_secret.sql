ALTER TABLE peers ADD COLUMN webhook_secret_ciphertext BYTEA NULL;

COMMENT ON COLUMN peers.webhook_secret_ciphertext IS
  'AES-256-GCM encrypted HMAC-SHA256 inbound-webhook signing secret (IONE_WEBHOOK_SECRET_KEY). NULL = not provisioned.';
