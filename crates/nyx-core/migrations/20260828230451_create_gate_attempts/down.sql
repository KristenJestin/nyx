-- Revert `create_gate_attempts` (20260828230451): drop the gate evidence journal.
--
-- The two indexes go with the table (SQLite drops a table's indexes with it), so one statement is
-- the whole revert — there is no column added to another table here, and therefore none of the
-- `ALTER TABLE … DROP COLUMN` ordering hazard `create_categories`' revert has to navigate.
--
-- Reverting DESTROYS every recorded attempt, and that is worth stating rather than glossing: this
-- table is the only durable record of the red-before-green sequences, so a revert takes the
-- `bug` Specs that relied on it back to "no evidence recorded" — which the auditor's context
-- renders as the explicit absence it is, never as a silent pass. The migration tests exercise this
-- path (`revert_last_migration`); nothing else should.

DROP TABLE gate_attempts;
