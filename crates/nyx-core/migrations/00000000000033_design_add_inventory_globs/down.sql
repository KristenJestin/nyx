-- Revert schema v33: drop the per-project inventory glob override from `design`. SQLite's
-- `ALTER TABLE DROP COLUMN` suffices — the column is referenced by no index, FK, or trigger,
-- and dropping it simply restores the "no persisted per-project glob-config" (Phase 1) state
-- (the inventory falls back to the detected stack default for every project).

ALTER TABLE design DROP COLUMN inventory_globs_json;
