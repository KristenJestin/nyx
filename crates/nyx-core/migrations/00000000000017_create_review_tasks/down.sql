-- Reverse schema v17: drop the `review_tasks` + `review_reads` tables and their indexes.
-- Drop the indexes first, then the tables (SQLite drops a table's indexes with it, but the
-- explicit order keeps the down migration symmetric with the up).
DROP INDEX IF EXISTS idx_review_tasks_comment;
DROP INDEX IF EXISTS idx_review_tasks_repo;
DROP TABLE IF EXISTS review_reads;
DROP TABLE IF EXISTS review_tasks;
