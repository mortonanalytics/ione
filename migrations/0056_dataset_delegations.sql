CREATE TABLE dataset_delegations (
    id uuid PRIMARY KEY,
    org_id uuid NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    workspace_id uuid NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    owner_user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    dataset_id uuid NOT NULL,
    version_id uuid NOT NULL,
    token_hash text NOT NULL UNIQUE CHECK (length(token_hash) = 64),
    manifest jsonb NOT NULL,
    expires_at timestamptz NOT NULL,
    revoked_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK (expires_at > created_at)
);
CREATE INDEX dataset_delegations_owner ON dataset_delegations(org_id, workspace_id, owner_user_id, dataset_id, version_id, created_at DESC);
