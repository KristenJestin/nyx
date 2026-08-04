-- Revert schema v32 (the proto ↔ session link table). Dropping the table drops its index. It has
-- no dependents, so a plain drop fully reverses the migration.
DROP TABLE IF EXISTS agent_session_prototypes;
