-- Schema v4: separate a managed command's FACTUAL run OUTCOME from the
-- notification/ack state — PRD-4, review 01KV8TS78PNREN54CEHDAR6PR6 (finding
-- 01KV8TS8MVT5ETJ6KCR5EQAAGX, "UI ack erases the error the MCP sees").
--
-- BEFORE v4, `command_instances.last_state` carried BOTH the factual outcome
-- (idle/running/success/error) AND the "unseen result" notification: a UI
-- acknowledge collapsed `last_state` to 'idle', erasing the error + exit code any
-- observer (the MCP, another window) still needs. v4 splits the two concerns the
-- way PRD-2.1 split terminal exec-state from its unread flag:
--
--  - `last_state` STAYS the factual outcome and is NEVER collapsed by an ack.
--  - `last_exit_code` persists the LAST completed run's natural exit code (NULL
--    while never-finished / running). It survives restarts, so the MCP can tell a
--    crash (non-zero) from a clean run (zero) even after a cold rehydrate — not
--    just from the in-memory runner this session.
--  - `ended_at` is the epoch-millis timestamp the last run finished (NULL while
--    never-finished / running), so an observer can order/age outcomes.
--  - `unread` is the separate notification flag: 1 once a run finishes (an "unseen
--    result"), cleared to 0 by an acknowledge WITHOUT touching the outcome above.
--
-- All four are additive and back-fill safely for existing rows: `unread` defaults
-- to 0 (a pre-v4 row's result is treated as already seen — no spurious dot on
-- upgrade), and the nullable `last_exit_code` / `ended_at` default to NULL (no
-- completion recorded yet). SQLite ALTER TABLE ADD COLUMN is a metadata-only,
-- in-place change.
ALTER TABLE command_instances
    ADD COLUMN last_exit_code INTEGER;

ALTER TABLE command_instances
    ADD COLUMN ended_at INTEGER;

ALTER TABLE command_instances
    ADD COLUMN unread INTEGER NOT NULL DEFAULT 0 CHECK (unread IN (0, 1));
