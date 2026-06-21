-- Reverse schema v4. Drop the four terminal exec-state columns added by up.sql.
-- `libsqlite3-sys` bundles a recent SQLite (>= 3.35), so `ALTER TABLE ... DROP
-- COLUMN` is available (the v2 down migration relies on the same).
ALTER TABLE terminals DROP COLUMN exec_state_updated_at;
ALTER TABLE terminals DROP COLUMN exec_state_unread;
ALTER TABLE terminals DROP COLUMN exec_exit_code;
ALTER TABLE terminals DROP COLUMN exec_state;
