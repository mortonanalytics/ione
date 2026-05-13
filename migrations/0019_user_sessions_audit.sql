CREATE TABLE user_sessions (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id      UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    org_id       UUID NOT NULL REFERENCES organizations(id),
    idp_type     TEXT NOT NULL,
    mfa_verified BOOLEAN NOT NULL DEFAULT false,
    expires_at   TIMESTAMPTZ NOT NULL,
    revoked_at   TIMESTAMPTZ,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX user_sessions_user ON user_sessions(user_id);
CREATE INDEX user_sessions_active ON user_sessions(expires_at) WHERE revoked_at IS NULL;

CREATE TABLE identity_audit_events (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    occurred_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    event_type   TEXT NOT NULL,
    org_id       UUID NOT NULL REFERENCES organizations(id),
    user_id      UUID REFERENCES users(id) ON DELETE SET NULL,
    actor_ip     INET,
    session_id   UUID REFERENCES user_sessions(id) ON DELETE SET NULL,
    peer_id      UUID REFERENCES peers(id) ON DELETE SET NULL,
    outcome      TEXT NOT NULL,
    detail       JSONB
);

CREATE INDEX identity_audit_org_occurred ON identity_audit_events(org_id, occurred_at DESC);

ALTER TABLE identity_audit_events ENABLE ROW LEVEL SECURITY;
CREATE POLICY identity_audit_org_isolation ON identity_audit_events
    USING (org_id = current_setting('app.current_org_id', true)::uuid);
