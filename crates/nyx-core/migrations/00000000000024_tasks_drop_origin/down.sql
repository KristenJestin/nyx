-- Reverse schema v24: RE-ADD the `tasks.origin` column (the v21 shape) so the drop is reversible.
-- SQLite `ALTER TABLE ADD COLUMN` re-appends it (the column's original physical slot between
-- `grill_state` and `round` cannot be reconstructed in place, and does not need to be — a fully
-- reverted DB is read by the PRE-v24 `schema.rs`, and `schema_matches_migration` only exercises the
-- forward direction). NULLABLE + CHECK-guarded exactly as v21 created it; un-back-filled (every row
-- keeps NULL).
ALTER TABLE tasks
    ADD COLUMN origin TEXT
    CHECK (origin IS NULL OR origin IN ('agent', 'human'));
