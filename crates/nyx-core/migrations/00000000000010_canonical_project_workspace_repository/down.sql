-- Reverse schema v10 (canonical Project / Workspace / Repository). Drop in reverse
-- order of the up migration so every FK/index dependency unwinds cleanly.
-- `libsqlite3-sys` bundles SQLite >= 3.35, so `ALTER TABLE ... DROP COLUMN` and
-- `RENAME COLUMN` are available (the v2/v4/v8/v9 downs rely on the same).

-- repositories: drop the index then the table.
DROP INDEX IF EXISTS idx_repositories_project;
DROP TABLE IF EXISTS repositories;

-- workspaces: drop the secondary canonical index, the added columns, and rename the
-- folder column back to its pre-v10 name (`path`). The UNIQUE(project_id, root_path)
-- reverts to UNIQUE(project_id, path) automatically with the rename.
DROP INDEX IF EXISTS idx_workspaces_project_name;
ALTER TABLE workspaces DROP COLUMN archived_at;
ALTER TABLE workspaces DROP COLUMN folder_removed_at;
ALTER TABLE workspaces RENAME COLUMN root_path TO path;

-- projects: drop archived_at, primary_workspace_id, then the unique key index and the
-- key column.
ALTER TABLE projects DROP COLUMN archived_at;
ALTER TABLE projects DROP COLUMN primary_workspace_id;
DROP INDEX IF EXISTS idx_projects_key;
ALTER TABLE projects DROP COLUMN key;
