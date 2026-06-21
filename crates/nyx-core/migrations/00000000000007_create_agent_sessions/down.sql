-- Reverse schema v7. Drop the indexes then the table. `terminals` is untouched:
-- this migration never added a column there (no `claude_session_id`), so there is
-- nothing to drop back off `terminals`.
DROP INDEX IF EXISTS idx_agent_sessions_workspace;
DROP INDEX IF EXISTS idx_agent_sessions_terminal;
DROP INDEX IF EXISTS idx_one_active_session_per_terminal_agent;
DROP TABLE agent_sessions;
