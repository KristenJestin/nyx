-- Schema v9: per-project sidebar ORDER, so projects can be reordered (FEEDBACK #11).
--
-- `terminals` and `managed_commands` already carry a `"order"` column for sidebar
-- ordering; `projects` did not, so the project order was FIXED (created_at asc). This
-- adds the SAME shape to `projects` — a double-quoted `"order"` (ORDER is a SQL
-- keyword), Diesel-mapped to `order_index`. ALTER TABLE ADD COLUMN, the non-destructive
-- extension shape v2/v4/v8 already use.
ALTER TABLE projects
    ADD COLUMN "order" INTEGER NOT NULL DEFAULT 0;

-- DEFAULT 0 alone would collapse every pre-v9 project to the same order. To PRESERVE the
-- current display order (`list_projects` ordered by created_at asc, id asc) the backfill
-- assigns each project a DENSE RANK by (created_at, id): each row's order = the number of
-- projects that sort before it. So after the upgrade the sidebar order is unchanged, and
-- the user can then reorder freely.
UPDATE projects
SET "order" = (
    SELECT COUNT(*)
    FROM projects AS p2
    WHERE p2.created_at < projects.created_at
       OR (p2.created_at = projects.created_at AND p2.id < projects.id)
);
