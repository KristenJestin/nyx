-- Revert schema v35: drop the per-row LAST-SCAN state from `design_components`. SQLite's
-- `ALTER TABLE DROP COLUMN` suffices — neither column is referenced by an index, an FK or a
-- trigger. Dropping them restores the pre-v35 state: the catalogue keeps no memory of what the
-- last scan saw (the paged read falls back to the stored synthesis state alone, and an orphan
-- carrying a synthesis is again only reported per-run, never flagged).

ALTER TABLE design_components DROP COLUMN orphaned_at;
ALTER TABLE design_components DROP COLUMN scanned_input_hash;
