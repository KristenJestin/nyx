-- Revert the GLOBAL SEARCH full-text substrate. The triggers go first (they reference the
-- virtual tables), then the virtual tables themselves — which is also what drops the FTS5 shadow
-- tables. `IF EXISTS` throughout so the revert is replayable on a database where the chain was
-- already rolled back.

DROP TRIGGER IF EXISTS tasks_fts_au;
DROP TRIGGER IF EXISTS tasks_fts_ad;
DROP TRIGGER IF EXISTS tasks_fts_ai;
DROP TABLE IF EXISTS tasks_fts;

DROP TRIGGER IF EXISTS spec_revisions_fts_au;
DROP TRIGGER IF EXISTS spec_revisions_fts_ad;
DROP TRIGGER IF EXISTS spec_revisions_fts_ai;
DROP TABLE IF EXISTS spec_revisions_fts;

DROP TRIGGER IF EXISTS specs_fts_au;
DROP TRIGGER IF EXISTS specs_fts_ad;
DROP TRIGGER IF EXISTS specs_fts_ai;
DROP TABLE IF EXISTS specs_fts;
