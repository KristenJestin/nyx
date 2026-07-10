-- Reverse schema v15 (the `specs.workspace_id` reverse-scan index). This migration only
-- ADDED an index — the `workspace_id` COLUMN + FK were created in v13 and are NOT touched
-- here — so the reversal is just the index drop.
DROP INDEX IF EXISTS idx_specs_workspace;
