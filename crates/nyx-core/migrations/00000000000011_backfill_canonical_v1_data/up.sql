-- Schema v11: DATA migration — bring EXISTING v1 rows up to the canonical form
-- WITHOUT loss (v2.1, task 01KWDCNDJX87BWJ6JMTRSETJYT). The v10 migration created the
-- canonical COLUMNS/indexes/tables and seeded the NOT-NULL `projects.key`; this
-- migration backfills the canonical POINTERS/derived data for rows that predate v10.
--
-- Design guarantees (PRD Testing Decisions + non-goals):
--   * IDEMPOTENT: every statement is guarded so a REPLAY (re-run) neither duplicates
--     nor loses a row. Diesel runs a migration once per DB, but the statements are
--     written to be safe even if re-executed (each targets only the not-yet-backfilled
--     rows), so a snapshot that is migrated twice converges to the same state.
--   * NO destructive delete: nothing is deleted. The workspace row is permanent
--     (`folder_removed_at` marks cleanup, never a DELETE). This migration is
--     backfill-only.
--   * Runtime preserved: terminals / managed_commands / command_instances /
--     agent_sessions keep their `workspace_id` (v10 RENAMED the workspaces.path column,
--     it never moved a runtime row), so the runtime stays attached to the SAME
--     workspace. This migration does not touch runtime rows.

-- --- projects.primary_workspace_id: point at the durable base (root) workspace ------
--
-- The canonical primary role is `projects.primary_workspace_id` (v10). For a project
-- created BEFORE v10 the pointer is NULL; the durable base is its single root workspace
-- (the pre-v10 `is_root = 1` row, one per project by the partial unique index
-- `idx_one_root_per_project`). Backfill the pointer from that root.
--
-- Idempotent by the `primary_workspace_id IS NULL` guard: a project whose pointer is
-- already set (e.g. one created post-v10, or a re-run of this backfill) is skipped, so a
-- replay is a no-op and never repoints an already-anchored project.
UPDATE projects
SET primary_workspace_id = (
    SELECT w.id
    FROM workspaces AS w
    WHERE w.project_id = projects.id
      AND w.is_root = 1
    LIMIT 1
)
WHERE primary_workspace_id IS NULL
  AND EXISTS (
    SELECT 1
    FROM workspaces AS w
    WHERE w.project_id = projects.id
      AND w.is_root = 1
  );

-- Note on `projects.key`: v10 already seeded a structurally-valid, UNIQUE, NOT-NULL key
-- (`P<rank>`) for every pre-v10 project, so the canonical `key` constraint is already
-- satisfied here — no backfill is needed at the migration level, and deriving a
-- friendlier name-based key (with collision disambiguation) is a curation concern the
-- Rust use-case layer owns (later phases). v11 deliberately does NOT touch `key`, to
-- avoid risking a UNIQUE collision in pure SQL.
