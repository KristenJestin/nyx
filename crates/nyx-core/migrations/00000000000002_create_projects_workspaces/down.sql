-- Reverse schema v2. Drop the terminal binding columns first (they reference
-- workspaces), then the workspaces table (and its indexes go with it), then
-- projects. `libsqlite3-sys` bundles a recent SQLite (>= 3.35), so
-- `ALTER TABLE ... DROP COLUMN` is available.
ALTER TABLE terminals DROP COLUMN workspace_binding_mode;
ALTER TABLE terminals DROP COLUMN workspace_id;

DROP INDEX IF EXISTS idx_workspaces_project;
DROP INDEX IF EXISTS idx_one_root_per_project;
DROP TABLE workspaces;
DROP TABLE projects;
