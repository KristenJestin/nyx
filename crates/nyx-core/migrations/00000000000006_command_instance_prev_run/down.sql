-- Reverse schema v5: drop the four retained-prior-run columns back off
-- `command_instances`. SQLite has supported `ALTER TABLE ... DROP COLUMN` since
-- 3.35 (2021); the bundled SQLite is well past that. Drop in reverse add order.
ALTER TABLE command_instances
    DROP COLUMN prev_last_state;

ALTER TABLE command_instances
    DROP COLUMN prev_ended_at;

ALTER TABLE command_instances
    DROP COLUMN prev_exit_code;

ALTER TABLE command_instances
    DROP COLUMN prev_scrollback;
