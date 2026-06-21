-- Schema v8: the per-project "resume agent sessions" option (PRD-5, Phase 3).
--
-- nyx can, at relaunch, RESUME an active agent session (e.g. inject
-- `claude --resume <id>` into the respawned shell) instead of starting a bare
-- shell. Per the PRD that behaviour is OPT-IN PER PROJECT and defaults OFF:
--   * default OFF  — a fresh / pre-v8 project does NOT auto-resume; the user must
--                    turn it on. This is the safe default (no surprise re-spawn of
--                    an agent) and the basis of the close-warning (#6): a terminal
--                    with an active session whose project does NOT auto-resume is
--                    exactly what the close-warning must flag.
--   * configurable per project — the option lives on `projects`, so two projects
--                    can differ. A terminal with NO project (no workspace anchor)
--                    is therefore OFF by construction (there is no project row to
--                    carry an ON flag) — matching the PRD ("terminal sans projet =
--                    OFF").
--
-- One column, added to the existing `projects` table (ALTER TABLE ADD COLUMN — the
-- same non-destructive extension shape v2/v4 used). A SQLite INTEGER 0/1 boolean
-- (mapped to a Rust `bool`), CHECK-enforced, DEFAULT 0 so OLD projects (rows that
-- predate this migration) load with the option OFF — no surprise resume on upgrade.
ALTER TABLE projects
    ADD COLUMN resume_agent_sessions INTEGER NOT NULL DEFAULT 0
        CHECK (resume_agent_sessions IN (0, 1));
