-- Schema v40: `workflows.builtin_revision` — the version tag that lets a boot UPGRADE a seeded
-- built-in when the code ships a newer default graph (v2.8 review N11).
--
-- A built-in default (`project_id IS NULL`) is seeded ONCE and then left alone by the
-- idempotent-by-scope seeder, so any later change to a default graph was invisible on an existing DB
-- (nspec/ndev/ndoc stayed frozen at their first-seeded shape). This column records WHICH revision of
-- the code produced the stored tree; the seeder compares it to the code's current revision and, when
-- the code is newer, REPLACES the tree in place (see `seed::ensure_builtin`). NULL for every non-
-- built-in workflow (a project fork / a custom template — those are user customisation, never
-- upgraded) and for a built-in seeded before this column existed (treated as "needs upgrade").

ALTER TABLE workflows ADD COLUMN builtin_revision INTEGER;
