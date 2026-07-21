-- Reverse schema v21: drop the finding index then the three ported columns off `tasks`.
-- SQLite has supported `ALTER TABLE ... DROP COLUMN` since 3.35 (2021); the bundled SQLite is
-- well past that. Drop the index first, then the columns (symmetric with the up migration).
DROP INDEX IF EXISTS idx_tasks_from_comment;

ALTER TABLE tasks DROP COLUMN origin;
ALTER TABLE tasks DROP COLUMN grill_state;
ALTER TABLE tasks DROP COLUMN from_comment_id;
