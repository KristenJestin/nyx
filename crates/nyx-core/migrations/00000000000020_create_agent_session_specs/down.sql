-- Reverse schema v20 (the `agent_session_specs` link table). Drop the reverse-lookup index
-- then the table (SQLite drops a table's indexes with it, but the explicit index drop keeps
-- this down migration symmetric with the up).
DROP INDEX IF EXISTS idx_agent_session_specs_spec;
DROP TABLE IF EXISTS agent_session_specs;
