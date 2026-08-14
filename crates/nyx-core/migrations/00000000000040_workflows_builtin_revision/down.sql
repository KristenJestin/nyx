-- Revert v40: drop the built-in revision tag (nullable, no index / FK / CHECK — a plain drop).
ALTER TABLE workflows DROP COLUMN builtin_revision;
