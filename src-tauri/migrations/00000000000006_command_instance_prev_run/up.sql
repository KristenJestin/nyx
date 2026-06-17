-- Schema v5: retain the LAST completed run across a (re)launch — PRD-4, review
-- 01KV90QCKZ8BXZ4DTYZRJK56EZ (finding 01KV90QDYP0F3PS86D1W39WB8B, "command output
-- has no inter-run history: each (re)launch resets the buffer, losing the previous
-- run").
--
-- BEFORE v5, a fresh `start`/`relaunch` reset the CURRENT run's `scrollback` to ''
-- and cleared its outcome columns, so the PREVIOUS run's output + exit_code/ended_at
-- were dropped on the spot: an agent could not compare run N-1 vs N, nor recover a
-- crash it did not capture in time. v5 retains exactly ONE prior run (bounded N=1 —
-- NOT unbounded history) by archiving the completing run's scrollback + factual
-- outcome into a parallel `prev_*` set on the NEXT (re)launch, BEFORE the current
-- run's columns are reset. The CURRENT run's columns are untouched by this, so the
-- per-run separation is preserved (the running buffer is never polluted by the prior
-- run's bytes); v5 only ADDS retained access to the immediately-prior run.
--
--  - `prev_scrollback`  — the bounded scrollback tail of the last completed run.
--  - `prev_exit_code`   — that run's natural exit code (NULL if it had no code).
--  - `prev_ended_at`    — the epoch-millis it finished (NULL while none retained).
--  - `prev_last_state`  — its factual outcome string (success|error), NULL while
--    none retained. Idle/never-run instances and the very first run keep all four
--    NULL/'' (there is no prior run yet).
--
-- All four are additive and back-fill safely for existing rows: `prev_scrollback`
-- defaults to '' (no prior output retained) and the three nullable columns default
-- to NULL. SQLite ALTER TABLE ADD COLUMN is a metadata-only, in-place change (the
-- same non-empty-safe shape as v4).
ALTER TABLE command_instances
    ADD COLUMN prev_scrollback TEXT NOT NULL DEFAULT '';

ALTER TABLE command_instances
    ADD COLUMN prev_exit_code INTEGER;

ALTER TABLE command_instances
    ADD COLUMN prev_ended_at INTEGER;

ALTER TABLE command_instances
    ADD COLUMN prev_last_state TEXT
        CHECK (prev_last_state IS NULL OR prev_last_state IN ('success', 'error'));
