-- Reverse schema v14 (the task domain). Drop in reverse order of the up migration so
-- every FK/index dependency unwinds cleanly: the edge table (`task_dependencies`, which
-- FKs into `tasks`) before the `tasks` table itself. SQLite drops a table's indexes with
-- it, but the explicit index drops keep this down migration symmetric with the up.

-- task_dependencies: drop its index then the table.
DROP INDEX IF EXISTS idx_task_dependencies_depends_on;
DROP TABLE IF EXISTS task_dependencies;

-- tasks: drop its indexes then the table.
DROP INDEX IF EXISTS idx_tasks_parent;
DROP INDEX IF EXISTS idx_tasks_project_status;
DROP INDEX IF EXISTS idx_tasks_spec_position;
DROP TABLE IF EXISTS tasks;
