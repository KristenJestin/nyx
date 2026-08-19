-- Migration 20260818130000: the WHERE of a dev workspace — `projects.worktrees_root` — plus the
-- node type that lands one from a graph.
--
-- Both halves serve ONE gesture: /nspec's final consent gains a third answer ("go" — a second shape
-- of yes), which marks the Spec ready AND opens the dev workspace anchored to it. That answer needs a
-- place to put the folder, and a node that can put it there.
--
-- HORS PERIMETRE ASSUME of PRD 01KYA475AX5JBGSS8WK5J8NM1S, on the user's explicit decision of
-- 2026-08-18. THIS MIGRATION CARRIES TWO OF THE THREE CROSSINGS MATERIALLY, so all three are named
-- here as they are on `build_nspec` in `seed.rs`:
--
--   * 01M02VRMPMRK1P8536ZYBAS4DS ("Consentement final et choix du workflow nspec") — the third
--     answer "go" (a yes that ALSO opens a workspace) this migration exists for. Not carried by the
--     SQL itself.
--   * 01KYA49F5T2H02YY4VT7SCBGSX ("Internes du moteur d'orchestration 2.8 […] 2.8 est
--     in_progress/fige") — CARRIED HERE by the widening of the `nodes.type` CHECK with
--     'create_workspace', a SIXTEENTH node type added to a frozen engine.
--   * 01M02VRGBAY6HV0A97A938DQ33 ("[…] le choix du root_path […] necessite un verbe configurable
--     par projet dans un PRD separe") — CARRIED HERE by `projects.worktrees_root`, which IS that
--     per-project verb. Bounded to the WHERE; the WHAT stays with idea 01M04W3RZ333NC01R216MNXAFK.
--
-- ## projects.worktrees_root
--
-- The project-level ROOT FOLDER under which every dev workspace of that project lands. NULLABLE and
-- NULL by default: a project that never sets it is asked for a path instead, so nothing about the
-- existing behaviour changes for anyone who ignores this column. It sits on the PROJECT — the same
-- level as `resume_agent_sessions` (v8) and for the same reason: where worktrees live depends on the
-- project's layout far more than on the machine, and the project settings already have a mirrored
-- write path (`set_project_settings` on the MCP side, the settings modal on the UI side).
--
-- A plain nullable ADD COLUMN: no index, no FK, no rebuild.
--
-- ## nodes.type += 'create_workspace'
--
-- A nyx-own inline leaf that creates the dev workspace of the run's Spec through the SAME
-- `mcp_runtime::create_workspace` the MCP tool calls — folder creation, registration and Spec anchor in
-- one guarded write, never a second implementation of the anchor. Widening the `nodes.type` CHECK uses
-- the in-place COLUMN-REBUILD recipe of v39/v41/v43 (SQLite cannot ALTER a CHECK, and a whole-table
-- rebuild would fight the inbound FKs under foreign_keys=ON, which cannot be toggled inside Diesel's
-- migration transaction). `nodes."type"` has no index / inbound FK, so it is a plain stash → drop →
-- re-add-widened → restore → drop-stash. The transient DEFAULT 'note' is never used (every row's real
-- type is restored below, and the backend always supplies `type` on insert).

-- --- projects.worktrees_root: the root folder dev workspaces land under -----------------------
ALTER TABLE projects ADD COLUMN worktrees_root TEXT;

-- --- nodes.type: add 'create_workspace' (plain in-place rebuild) ------------------------------
ALTER TABLE nodes ADD COLUMN type_stash TEXT;
UPDATE nodes SET type_stash = "type";
ALTER TABLE nodes DROP COLUMN "type";
ALTER TABLE nodes
    ADD COLUMN "type" TEXT NOT NULL DEFAULT 'note'
    CHECK ("type" IN (
        'sequence', 'loop', 'selector',
        'spawn_subagent', 'judge', 'ask_human', 'produce',
        'gate_check', 'transition', 'wait', 'note', 'query', 'fork_spec',
        'sub_workflow', 'transition_task', 'create_workspace'
    ));
UPDATE nodes SET "type" = type_stash;
ALTER TABLE nodes DROP COLUMN type_stash;
