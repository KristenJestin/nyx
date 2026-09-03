-- Revert `create_categories` (20260828180916): drop the Spec's category axis entirely.
--
-- ORDER MATTERS, and it is the reverse of `up.sql`'s. `specs.category_id` holds a REFERENCES clause
-- into `categories`, so the referencing column goes FIRST and the referenced table second;
-- dropping the table while a live FK still points at it leaves the database in the state
-- `PRAGMA foreign_key_check` reports as broken (SQLite does not refuse the DROP — it lets the
-- dangling reference stand — which is worse than an error).
--
-- `idx_specs_category` must be dropped EXPLICITLY and FIRST. SQLite's `ALTER TABLE … DROP COLUMN`
-- refuses a column an index is written on, and it discovers this AFTER rewriting the table — the
-- failure it raises reads `error in index idx_specs_category after drop column: no such column`,
-- which is a half-applied revert, not a clean refusal. `idx_categories_project_slug` needs no such
-- line: it goes away with the `DROP TABLE` that removes the table it indexes.
--
-- Reverting takes every Spec back to UNCATEGORIZED, which is the same state the column's NULL
-- default already expresses, so no Spec becomes invalid on the way down. The assignments
-- themselves are lost — there is nowhere left to keep them — and that is the accepted cost of a
-- revert, exercised by the migration tests' `revert_last_migration`.

DROP INDEX idx_specs_category;

ALTER TABLE specs DROP COLUMN category_id;

DROP TABLE categories;
