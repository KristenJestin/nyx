-- Schema v29: add the `closed` status to `ideas.status` — the DISTINCT MANUAL closure state
-- (v2.6 — Idées entretenues, review 01KY4S0F20BBEPKCWMEVWQ3FYE, finding
-- 01KY4S0H9BYRC6SK7RJHQ5KBE6 — "statut 'closed' distinct (fermeture manuelle) vs
-- 'implemented' (auto via Spec done)").
--
-- ## Why (user decision #4: split MANUAL closure from AUTOMATIC implementation)
--
-- v26 gave `ideas.status` four values (`open`/`planned`/`implemented`/`dropped`) and let
-- `idea_close` funnel a fermeture-par-constat into `implemented`. Real MCP testing surfaced the
-- conflation: `close` = a HUMAN closing an idea by constat ("already covered / no longer
-- relevant"), while `implemented` should mean a linked Spec REACHED completed (automatic). The
-- user decision splits them: `close` → a NEW terminal status `closed`; `implement` / the auto
-- lifecycle → `implemented`. This migration widens the CHECK vocabulary to admit `closed`.
--
-- ## How (in-place COLUMN rebuild, NOT a whole-table rebuild — same pattern as v25)
--
-- SQLite has no ALTER-COLUMN-DROP/RETIGHTEN-CONSTRAINT, and rebuilding the WHOLE `ideas` table
-- would fire the ON DELETE CASCADE of its inbound FK (`idea_specs.idea_id → ideas.id`) and force
-- us to tear down + re-create the external-content `ideas_fts` virtual table and its AI/AD/AU
-- triggers — all under `foreign_keys=ON`, which cannot be toggled inside Diesel's migration
-- transaction. So we REBUILD THE SINGLE `status` COLUMN IN PLACE (the v25 `grill_state` recipe):
-- stash the values, drop the column, re-add it with the WIDENED CHECK, restore, drop the stash.
-- The `ideas_fts` virtual table + its triggers reference only (rowid, title, body) — never
-- `status` — so they are UNTOUCHED (the re-sync UPDATEs below merely re-index the same terms,
-- idempotently); the `idea_specs` FK is preserved (no table rebuild); every default is preserved.
--
-- The one wrinkle vs v25: `status` is INDEXED (`idx_ideas_project_status`), and SQLite refuses to
-- DROP a column an index references — so we DROP that index first and RE-CREATE it after, byte
-- for byte. `idx_ideas_project_theme` (project_id, theme) does not touch `status` and is left as-is.
--
-- `ALTER TABLE ADD COLUMN` re-appends, so `status` now sits physically LAST — harmless here: every
-- `Idea` read/write goes through Diesel `as_select()` / `as_returning()` (columns emitted BY NAME
-- in `schema.rs` order), never a positional `SELECT *`, so the physical order is immaterial.

-- The index on `status` blocks DROP COLUMN — drop it, re-create it at the end.
DROP INDEX idx_ideas_project_status;

-- Stash → drop → re-add with the widened CHECK (+ 'closed') → restore → drop stash.
ALTER TABLE ideas ADD COLUMN status_v29 TEXT;
UPDATE ideas SET status_v29 = status;
ALTER TABLE ideas DROP COLUMN status;
ALTER TABLE ideas
    ADD COLUMN status TEXT NOT NULL DEFAULT 'open'
    CHECK (status IN ('open', 'planned', 'implemented', 'closed', 'dropped'));
UPDATE ideas SET status = status_v29;
ALTER TABLE ideas DROP COLUMN status_v29;

-- Re-create the scan index the DROP COLUMN forced us to drop.
CREATE INDEX idx_ideas_project_status ON ideas (project_id, status);
