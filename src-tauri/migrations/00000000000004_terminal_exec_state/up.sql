-- Schema v4: terminal exec-state fields (PRD-2.1 — running/success/error + badges).
--
-- PRD 2.1 makes a terminal row reflect real shell command execution: `running`
-- while a foreground command is active, `success`/`error` when it exits, plus a
-- read/unread notification flag so a background terminal can signal a settled
-- result without becoming noisy. The exec-state pipeline (OSC 133 parsing in the
-- bridge, the state machine, the frontend badge) lands in later tasks/phases;
-- THIS migration only adds the persistent columns so the DB record is the
-- authority for the sidebar badge after a restart.
--
-- Four columns, added to the existing `terminals` table (ALTER TABLE ADD COLUMN,
-- the same non-destructive extension shape v2 used for the workspace binding):
--
--   exec_state            the last exec-state, one of idle|running|success|error
--                         (CHECK-enforced like `terminals.status` and
--                         `command_instances.last_state`). DEFAULT 'idle' so OLD
--                         terminals (rows that predate this migration) load as
--                         `idle` — no false badge on upgrade.
--   exec_exit_code        the last command's exit code (NULL = none yet / a `D`
--                         with no parseable code). Nullable INTEGER.
--   exec_state_unread     0/1 notification flag (CHECK-enforced): a settled
--                         success/error that the user has not yet seen on an
--                         inactive terminal. DEFAULT 0 so old terminals load with
--                         unread = false. Kept SEPARATE from the settled state
--                         itself (mark-read clears unread but preserves the
--                         result) — this is the deliberate difference from the
--                         managed-command acknowledge model.
--   exec_state_updated_at epoch ms of the last exec-state transition. NOT NULL.
--                         SQLite FORBIDS a non-constant DEFAULT on `ALTER TABLE ADD
--                         COLUMN` once the table is NON-EMPTY ("Cannot add a column
--                         with non-constant default") — and every real upgrade adds
--                         this column to a terminals table that already has the
--                         user's persisted rows. So we ADD it with the CONSTANT
--                         DEFAULT 0 (legal on a non-empty table), then BACKFILL the
--                         existing rows with the portable julianday epoch-ms in a
--                         follow-up UPDATE. Fresh rows still get a meaningful stamp:
--                         the backend stamps it explicitly on every transition, and
--                         create_terminal goes through that path. (A raw insert that
--                         omits it lands on 0 — harmless; the next transition stamps
--                         it.)
ALTER TABLE terminals
    ADD COLUMN exec_state TEXT NOT NULL DEFAULT 'idle'
        CHECK (exec_state IN ('idle', 'running', 'success', 'error'));

ALTER TABLE terminals
    ADD COLUMN exec_exit_code INTEGER;

ALTER TABLE terminals
    ADD COLUMN exec_state_unread INTEGER NOT NULL DEFAULT 0
        CHECK (exec_state_unread IN (0, 1));

-- Constant DEFAULT (legal to ADD on a non-empty table), then backfill existing
-- rows with the real epoch-ms timestamp. SQLite rejects a non-constant DEFAULT on
-- ADD COLUMN for a non-empty table, so this two-step form is required for upgrades.
ALTER TABLE terminals
    ADD COLUMN exec_state_updated_at INTEGER NOT NULL DEFAULT 0;

UPDATE terminals
    SET exec_state_updated_at = CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)
    WHERE exec_state_updated_at = 0;
