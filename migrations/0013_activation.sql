CREATE TABLE activation_progress (
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    workspace_id UUID NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    track TEXT NOT NULL CHECK (track IN ('demo_walkthrough', 'real_activation')),
    step_key TEXT NOT NULL CHECK (step_key IN (
        'asked_demo_question', 'opened_demo_survivor', 'reviewed_demo_approval', 'viewed_demo_audit',
        'added_connector', 'first_signal', 'first_approval_decided', 'first_audit_viewed'
    )),
    completed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, workspace_id, track, step_key)
);

CREATE INDEX activation_progress_user_idx ON activation_progress (user_id);

CREATE TABLE activation_dismissals (
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    workspace_id UUID NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    track TEXT NOT NULL CHECK (track IN ('demo_walkthrough', 'real_activation')),
    dismissed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, workspace_id, track)
);
