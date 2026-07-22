-- Revert schema v29: narrow `ideas.status` back to the v26 vocabulary (drop `closed`).
--
-- The mirror of `up.sql`'s in-place column rebuild. Any `closed` row (a MANUAL fermeture in the
-- v29 model) is mapped back to `implemented` — the pre-v29 `close` semantics funnelled a
-- fermeture-par-constat into `implemented`, so this is the faithful downgrade AND keeps the
-- restore UPDATE inside the narrowed CHECK (a leftover `closed` would violate it and fail the
-- revert). The `ideas_fts` virtual table + triggers and the `idea_specs` FK are untouched (no
-- table rebuild), exactly as in `up.sql`.

DROP INDEX idx_ideas_project_status;

ALTER TABLE ideas ADD COLUMN status_v29 TEXT;
-- Fold the v29-only 'closed' back into 'implemented' before the narrowed CHECK is restored.
UPDATE ideas SET status_v29 = CASE WHEN status = 'closed' THEN 'implemented' ELSE status END;
ALTER TABLE ideas DROP COLUMN status;
ALTER TABLE ideas
    ADD COLUMN status TEXT NOT NULL DEFAULT 'open'
    CHECK (status IN ('open', 'planned', 'implemented', 'dropped'));
UPDATE ideas SET status = status_v29;
ALTER TABLE ideas DROP COLUMN status_v29;

CREATE INDEX idx_ideas_project_status ON ideas (project_id, status);
