-- Revert schema v38: drop the SCOPED CSS SLOT from `design_components`. SQLite's `ALTER TABLE DROP
-- COLUMN` suffices — the column is referenced by no index, no FK and no trigger. Dropping it
-- restores the pre-v38 state: a component can carry no exception at all, and anything Tailwind
-- utilities cannot express is lost again (which is the defect v38 exists to end — this direction is
-- for a rollback, not for a decision).

ALTER TABLE design_components DROP COLUMN css_slot_json;
