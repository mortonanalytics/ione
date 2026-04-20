-- Phase 12: peer federation

CREATE TYPE peer_status AS ENUM ('active', 'paused', 'error');

CREATE TABLE peers (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL,
    mcp_url TEXT NOT NULL,
    issuer_id UUID NOT NULL REFERENCES trust_issuers(id) ON DELETE RESTRICT,
    sharing_policy JSONB NOT NULL DEFAULT '{}'::jsonb,
    status peer_status NOT NULL DEFAULT 'active',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (mcp_url)
);

CREATE INDEX peers_status ON peers(status);
