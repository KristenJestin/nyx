-- Reverse schema v18: drop the Spec-scoped index and the `spec_id` column off `review_tasks`.
-- SQLite has supported `ALTER TABLE ... DROP COLUMN` since 3.35 (2021); the bundled SQLite is well
-- past that. Drop the index first, then the column (symmetric with the up migration).
DROP INDEX IF EXISTS idx_review_tasks_repo_spec;

ALTER TABLE review_tasks
    DROP COLUMN spec_id;
