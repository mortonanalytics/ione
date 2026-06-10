ALTER TABLE peers ADD COLUMN tool_prefix VARCHAR(16);
ALTER TABLE peers ADD COLUMN session_status TEXT NOT NULL DEFAULT 'disconnected';
ALTER TABLE peers ADD COLUMN last_connected_at TIMESTAMPTZ;
ALTER TABLE peers ADD COLUMN last_session_error TEXT;

CREATE UNIQUE INDEX peers_org_tool_prefix_uniq
    ON peers(org_id, tool_prefix)
    WHERE tool_prefix IS NOT NULL;
