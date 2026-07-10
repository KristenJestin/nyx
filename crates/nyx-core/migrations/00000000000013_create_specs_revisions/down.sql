-- Reverse schema v13 (the Spec domain). Drop in reverse order of the up migration so
-- every FK/index dependency unwinds cleanly: the child (`spec_revisions`) before the
-- parent (`specs`). SQLite drops a table's indexes with it, but the explicit index
-- drops keep this down migration symmetric with the up.

-- spec_revisions: drop its indexes then the table.
DROP INDEX IF EXISTS idx_spec_revisions_spec;
DROP INDEX IF EXISTS idx_one_active_revision_per_spec;
DROP INDEX IF EXISTS idx_spec_revisions_spec_number;
DROP TABLE IF EXISTS spec_revisions;

-- specs: drop its indexes then the table.
DROP INDEX IF EXISTS idx_specs_project_status;
DROP INDEX IF EXISTS idx_specs_project_slug;
DROP INDEX IF EXISTS idx_specs_project_number;
DROP TABLE IF EXISTS specs;
