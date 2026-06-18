-- Reverse schema v8. Drop the per-project resume-agent-sessions option column.
-- `libsqlite3-sys` bundles a recent SQLite (>= 3.35), so `ALTER TABLE ... DROP
-- COLUMN` is available (the v2/v4 down migrations rely on the same).
ALTER TABLE projects DROP COLUMN resume_agent_sessions;
