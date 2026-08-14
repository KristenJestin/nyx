-- Revert v44: drop the command template's persisted environment (nullable, no index / FK / CHECK —
-- a plain drop; every command falls back to the inherited environment alone).
ALTER TABLE managed_commands DROP COLUMN env;
