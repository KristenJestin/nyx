-- Revert schema v26 (the IDEAS domain). Drop the FTS triggers + index first (they depend
-- on `ideas`), then the link table, then `ideas` itself.
DROP TRIGGER IF EXISTS ideas_fts_au;
DROP TRIGGER IF EXISTS ideas_fts_ad;
DROP TRIGGER IF EXISTS ideas_fts_ai;
DROP TABLE IF EXISTS ideas_fts;
DROP TABLE IF EXISTS idea_specs;
DROP TABLE IF EXISTS ideas;
