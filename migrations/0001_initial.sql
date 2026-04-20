CREATE EXTENSION IF NOT EXISTS vector;
CREATE EXTENSION IF NOT EXISTS pgcrypto;

CREATE TYPE message_role AS ENUM ('user', 'assistant', 'system');

CREATE TABLE organizations (
    id         UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    name       TEXT        NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE users (
    id           UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id       UUID        NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    email        TEXT        NOT NULL,
    display_name TEXT        NOT NULL,
    oidc_subject TEXT,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (org_id, email)
);

CREATE TABLE conversations (
    id           UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID,
    user_id      UUID        REFERENCES users(id) ON DELETE SET NULL,
    title        TEXT        NOT NULL DEFAULT 'Untitled',
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE messages (
    id              UUID         PRIMARY KEY DEFAULT gen_random_uuid(),
    conversation_id UUID         NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    role            message_role NOT NULL,
    content         TEXT         NOT NULL,
    model           TEXT,
    tokens_in       INT,
    tokens_out      INT,
    created_at      TIMESTAMPTZ  NOT NULL DEFAULT now()
);

CREATE INDEX messages_conversation_id_created_at ON messages(conversation_id, created_at);
