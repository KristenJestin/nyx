-- Migration 20260828205640: `nodes.type` += 'set_category' — the SEVENTEENTH node type.
--
-- ## What it is
--
-- A nyx-own inline leaf that CLASSIFIES the run's bound Spec: it reads a category LABEL from the
-- blackboard (`category_key`) and assigns it through the SAME `crate::category` service the MCP
-- surface calls, so the same-project refusal, the on-the-fly MINT of an unknown label and the
-- `spec.category_set` event stay ONE authority — never a second implementation of the assignment.
--
-- ## Why it is a node
--
-- PDR-0017 makes the category the axis the `bug` discipline hangs on. A prompt asking an agent to
-- classify can be skipped; a node of the graph cannot. So the classification is IMPOSED by the
-- engine, exactly as `transition`, `fork_spec` and `create_workspace` impose theirs (ADR-0006:
-- "le nœud `set_category` est un type moteur complet").
--
-- HORS PERIMETRE ASSUME: the orchestration engine 2.8 is `in_progress` and therefore frozen. The
-- crossing is deliberate and has an identical, documented precedent — 20260818130000 widened this
-- very CHECK with 'create_workspace', a SIXTEENTH node type added to the same frozen engine on the
-- user's explicit decision of 2026-08-18. ADR-0006 records this one.
--
-- ## The recipe
--
-- Widening the `nodes.type` CHECK uses the in-place COLUMN-REBUILD of v39/v41/v43 and of
-- 20260818130000 (SQLite cannot ALTER a CHECK, and a whole-table rebuild would fight the inbound FKs
-- under foreign_keys=ON, which cannot be toggled inside Diesel's migration transaction).
-- `nodes."type"` has no index / inbound FK, so it is a plain stash → drop → re-add-widened → restore
-- → drop-stash. The transient DEFAULT 'note' is never used (every row's real type is restored below,
-- and the backend always supplies `type` on insert).

ALTER TABLE nodes ADD COLUMN type_stash TEXT;
UPDATE nodes SET type_stash = "type";
ALTER TABLE nodes DROP COLUMN "type";
ALTER TABLE nodes
    ADD COLUMN "type" TEXT NOT NULL DEFAULT 'note'
    CHECK ("type" IN (
        'sequence', 'loop', 'selector',
        'spawn_subagent', 'judge', 'ask_human', 'produce',
        'gate_check', 'transition', 'wait', 'note', 'query', 'fork_spec',
        'sub_workflow', 'transition_task', 'create_workspace', 'set_category'
    ));
UPDATE nodes SET "type" = type_stash;
ALTER TABLE nodes DROP COLUMN type_stash;
