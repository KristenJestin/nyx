-- Reverse schema v16: drop the `review_comments` table and its indexes. Drop the indexes
-- first, then the table (SQLite drops a table's indexes with it, but the explicit order
-- keeps the down migration symmetric with the up).
DROP INDEX IF EXISTS idx_review_comments_parent;
DROP INDEX IF EXISTS idx_review_comments_repo;
DROP TABLE IF EXISTS review_comments;
