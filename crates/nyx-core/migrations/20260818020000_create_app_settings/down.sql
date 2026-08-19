-- Revert v50: drop the app-level preference store. No index / FK of its own — a plain drop.
-- Reverting means the app forgets every preference the user expressed and every resolution falls
-- back to its historical default (for the shell: `$SHELL`, then the per-OS default).
DROP TABLE IF EXISTS app_settings;
