DROP INDEX pending_peer_tool_calls_idempotency;

CREATE UNIQUE INDEX pending_peer_tool_calls_idempotency
    ON pending_peer_tool_calls(workspace_id, peer_id, arguments_digest)
    WHERE status IN ('pending', 'approved');
