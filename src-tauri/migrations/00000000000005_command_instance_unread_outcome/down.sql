-- Reverse schema v4: drop the three columns the v4 split added back off
-- `command_instances`. SQLite has supported `ALTER TABLE ... DROP COLUMN` since
-- 3.35 (2021); the bundled SQLite is well past that. Drop in reverse add order.
ALTER TABLE command_instances
    DROP COLUMN unread;

ALTER TABLE command_instances
    DROP COLUMN ended_at;

ALTER TABLE command_instances
    DROP COLUMN last_exit_code;
