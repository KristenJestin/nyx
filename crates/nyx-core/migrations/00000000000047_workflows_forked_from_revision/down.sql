-- Revert v47: drop the fork's source-revision stamp (nullable, no index / FK — a plain drop).
ALTER TABLE workflows DROP COLUMN forked_from_revision;
