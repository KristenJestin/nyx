-- Schema v44: `managed_commands.env` — the PERSISTED environment of a command TEMPLATE (v2.8 review
-- N31). Holds a JSON object of KEY→VALUE strings (`{"DATABASE_URL":"postgres://…"}`), merged onto the
-- inherited environment on EVERY path that spawns the command: an agent-initiated start/relaunch, the
-- boot restore, and — the point of this migration — a `gate_check` nyx runs itself over its own PTY.
-- Before it, a gate spawned with a HARDCODED empty env while `start_command` honoured its per-run
-- `env` argument, so the SAME command saw two different environments depending on WHO started it
-- ("green by hand, red in the gate"). The env belongs to the COMMAND, not to the gate_check node:
-- a test suite needs its DATABASE_URL whoever launches it, and hanging it off the node would
-- duplicate it per graph and drift.
--
-- Nullable (a command with no environment of its own — the overwhelming majority — stores NULL); no
-- index / FK / CHECK: a malformed value decodes to an EMPTY env rather than making the command
-- unresolvable, so the column can never wedge a run. The per-run `env` argument of start_command /
-- relaunch_command stays a TRANSIENT override MERGED ON TOP of this base for that one start.

ALTER TABLE managed_commands ADD COLUMN env TEXT;
