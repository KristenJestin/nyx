-- Schema v48: `human_turns` — the LAST prompt a HUMAN submitted on a terminal, persisted.
--
-- ## The defect this closes
--
-- The consent check (v45) compares two facts that did not share a lifetime. The human-turn side
-- was a COUNTER held in memory by the sidecar and dropped at `SessionEnd`; the reference it is
-- compared against (`workflow_runs.pending_consent`) is a row. Every `claude -p` invocation is a
-- whole session — SessionStart, one prompt, SessionEnd — so the counter restarted at zero on each
-- one while the receipt still held a number counted in a previous session's lifetime. The same
-- run, the same question, the same human: accepted from an interactive session, refused from
-- `claude -p --resume` on two authentic answers in a row. And the comparison is arbitrary in BOTH
-- directions — numbers from two lifetimes can also compare "greater", accepting an answer nobody
-- gave, which is the silent half.
--
-- So the ledger stops COUNTING and starts NAMING: this table records WHICH human turn was the last
-- one nyx saw on a terminal. The receipt records the id of the turn that stood when the question
-- was posed, and the answer is accepted only if the last known turn is a DIFFERENT one. Two rows
-- compared, one lifetime, and it survives a sidecar restart — where a counter could only restart.
--
-- ## Shape
--
-- ONE row per terminal (the PRIMARY KEY): only the LAST turn is a fact anyone reads — the history
-- of what a human typed is not nyx's to keep, and the one prompt that ever matters (the words that
-- authorised a consent) is journaled into `domain_events` by `run.consent_answered` when it is
-- spent. The TERMINAL is the key, not the session, because that is the identity that survives the
-- `claude -p` session boundary: the person sitting at that terminal is the same one across the
-- invocations of one exchange.
--
-- NO foreign key to `terminals`. This row is written from the `UserPromptSubmit` hook, the one
-- best-effort path that must never fail and never blocks the prompt; a terminal nyx has no row for
-- (a hook from a shell nyx did not open) must record its turn rather than raise a constraint. The
-- cost is bounded by construction — one row per terminal id ever seen, holding one prompt.

CREATE TABLE human_turns (
    -- The terminal the `UserPromptSubmit` hook reported (`NYX_TERMINAL_ID` / the
    -- `X-Nyx-Terminal-Id` header an MCP call carries). PRIMARY KEY: one last-turn per terminal.
    terminal_id TEXT    NOT NULL PRIMARY KEY,

    -- The IDENTITY of that turn: Claude Code's own `prompt_id` when the hook carries one, else an
    -- id nyx generates. What is required of it is only that a NEW prompt gets a NEW value — that
    -- is the whole comparison — and re-delivering the SAME prompt keeps the same one, so a
    -- duplicated hook POST never manufactures a turn nobody took.
    turn_id     TEXT    NOT NULL,

    -- The RAW text of that prompt, kept so an accepted consent can be journaled with the very
    -- words that authorised it — a dispute is then settled from the record, not from memory.
    prompt      TEXT    NOT NULL,

    -- When the prompt was submitted (epoch ms). Read by nobody as a verdict — a clock cannot tell
    -- two turns apart when they tie, which is why the identity above exists — but it dates the
    -- ledger for anyone reading the row.
    submitted_at INTEGER NOT NULL DEFAULT (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER))
);
