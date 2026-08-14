-- Revert v46: drop the per-project command-slot bindings. The UNIQUE index goes with the table
-- (SQLite drops a table's indexes with it); it is named here for the reader, not for SQLite.
DROP INDEX IF EXISTS idx_command_slots_project_slot;
DROP TABLE IF EXISTS command_slots;
