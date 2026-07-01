-- Reverse schema v9: drop the per-project sidebar order column. `libsqlite3-sys`
-- bundles SQLite >= 3.35, so `ALTER TABLE ... DROP COLUMN` is available (the v2/v4/v8
-- down migrations rely on the same).
ALTER TABLE projects DROP COLUMN "order";
