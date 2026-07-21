-- Reverse schema v25: WIDEN the `tasks.grill_state` CHECK back to `proposed|structured|validated`
-- (the v21 vocabulary) so the collapse is reversible. Same in-place column rebuild (stash → drop →
-- re-add with the WIDER CHECK → restore); no data mapping is needed widening a constraint (every
-- `proposed|structured|NULL` value already satisfies it). `ALTER TABLE ADD COLUMN` re-appends
-- `grill_state`: a fully-reverted DB is read by the PRE-v25 `schema.rs`, and `schema_matches_migration`
-- exercises only the forward direction (as v24's down documents), so the physical slot need not be
-- reconstructed here.

ALTER TABLE tasks ADD COLUMN grill_state_v25 TEXT;

UPDATE tasks SET grill_state_v25 = grill_state;

ALTER TABLE tasks DROP COLUMN grill_state;

ALTER TABLE tasks
    ADD COLUMN grill_state TEXT
    CHECK (grill_state IS NULL OR grill_state IN ('proposed', 'structured', 'validated'));

UPDATE tasks SET grill_state = grill_state_v25;

ALTER TABLE tasks DROP COLUMN grill_state_v25;
