-- Schema v45: `workflow_runs.pending_consent` — the RECEIPT of the consent question nyx put to the
-- human for this run: what was asked, when, and to which session (H6).
--
-- An `ask_human` whose answer gates an ENGAGING Spec transition (ready/done/cancel) is answered in
-- the conversation like any other. What the engine cannot read off the answer itself is whether a
-- HUMAN gave it: a session was observed answering `yes` "on the user's behalf" — applying an
-- instruction from one turn earlier — and moving a Spec to `ready` the human never validated. The
-- missing fact is that no human turn happened between the question and the answer. So this column
-- records the asking, including the session's human-turn count at that instant; the tick that
-- receives the answer compares it to the count then, refuses an answer with no turn behind it, and
-- clears the column when it accepts one. The ENGINE is the only writer.
--
-- A single nullable JSON column, not a table: a run parks on ONE leaf at a time, so it has at most
-- one open question. The consent HISTORY is not here either — it is journaled in the `domain_events`
-- outbox (`run.consent_asked` / `run.consent_answered`, the latter carrying the raw human prompt
-- that authorised it), which is where a Spec's audit trail already lives.

ALTER TABLE workflow_runs ADD COLUMN pending_consent TEXT;
