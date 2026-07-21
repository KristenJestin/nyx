-- Reverse schema v23: drop the `reviewing_phase` column off `specs`. SQLite has supported `ALTER
-- TABLE ... DROP COLUMN` since 3.35 (2021); the bundled SQLite is well past that.
ALTER TABLE specs DROP COLUMN reviewing_phase;
