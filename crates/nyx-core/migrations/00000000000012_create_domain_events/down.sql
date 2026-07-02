-- Reverse schema v12: drop the `domain_events` outbox table and its indexes.
-- Drop the indexes first, then the table (SQLite drops a table's indexes with it,
-- but the explicit order keeps the down migration symmetric with the up).
DROP INDEX IF EXISTS idx_domain_events_workspace;
DROP INDEX IF EXISTS idx_domain_events_project;
DROP TABLE IF EXISTS domain_events;
