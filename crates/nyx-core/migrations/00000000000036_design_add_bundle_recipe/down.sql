-- Revert schema v36: drop the per-project BUNDLE RECIPE from `design`. SQLite's
-- `ALTER TABLE DROP COLUMN` suffices — the column is referenced by no index, FK or trigger, and
-- dropping it restores the pre-v36 state (the inventory falls back to the v33 glob override when
-- set, else to the detected stack's default recipe — nyx's former hard-coded behaviour).

ALTER TABLE design DROP COLUMN bundle_recipe_json;
