-- Schema v43: the SUB-WORKFLOW vocabulary + `workflow_runs.parent_run_id` — the v2.8 "sub-workflow"
-- doctrine floor (lot 2). Three independent additions in one migration (all land past the live floor):
--
-- ## nodes.type += 'sub_workflow', 'transition_task'
--
-- Two new nyx-internal leaves. `sub_workflow` opens a CHILD run (a fresh sub-agent drives it) and folds
-- its VERIFIED result back onto the parent board; `transition_task` marks a task (start/done/block/skip)
-- as a nyx-own inline effect (like `transition` does for a Spec). Widening the `nodes.type` CHECK uses the
-- same in-place COLUMN-REBUILD recipe as v39/v41 (SQLite cannot ALTER a CHECK, and a whole-table rebuild
-- would fight the inbound FKs under foreign_keys=ON, which cannot be toggled inside Diesel's migration
-- transaction). `nodes."type"` has no index / inbound FK, so it is a plain stash → drop → re-add-widened →
-- restore → drop-stash. The transient DEFAULT 'note' is never used (every row's real type is restored
-- below, and the backend always supplies `type` on insert).
--
-- ## workflows.scope += 'coder', 'prototyper'
--
-- Two SUB-WORKFLOW-ONLY scopes: a `sub_workflow` node resolves its child target by scope, so the coder /
-- prototyper flows need their own scope vocabulary. Same in-place COLUMN-REBUILD recipe — but `scope` is
-- INDEXED (`idx_workflows_scope`) and SQLite refuses to DROP a column an index references, so we DROP that
-- index first and RE-CREATE it after, byte for byte (the v29 `idx_ideas_project_status` precedent). The
-- inbound FKs into `workflows` (`nodes.workflow_id`, `workflow_runs.workflow_id`, the self-FK `forked_from`,
-- and the outbound `root_node_id`) all reference `workflows(id)` — the PK, untouched by an in-place `scope`
-- rebuild — so they are preserved; only the `scope` index touches `scope`.
--
-- ## workflow_runs.parent_run_id (nullable self-FK)
--
-- The PARENT run a `sub_workflow` node opened this child run FROM (`None` = a top-level run). A nullable
-- ADD COLUMN needs no rebuild; the self-FK → `workflow_runs(id)` (ON DELETE SET NULL, so a child outlives
-- a deleted parent) is allowed on an ADD COLUMN because the column holds NULL with a NULL default.

-- --- nodes.type: add 'sub_workflow' + 'transition_task' (plain in-place rebuild) --------------
ALTER TABLE nodes ADD COLUMN type_v43 TEXT;
UPDATE nodes SET type_v43 = "type";
ALTER TABLE nodes DROP COLUMN "type";
ALTER TABLE nodes
    ADD COLUMN "type" TEXT NOT NULL DEFAULT 'note'
    CHECK ("type" IN (
        'sequence', 'loop', 'selector',
        'spawn_subagent', 'judge', 'ask_human', 'produce',
        'gate_check', 'transition', 'wait', 'note', 'query', 'fork_spec',
        'sub_workflow', 'transition_task'
    ));
UPDATE nodes SET "type" = type_v43;
ALTER TABLE nodes DROP COLUMN type_v43;

-- --- workflows.scope: add 'coder' + 'prototyper' (index-aware in-place rebuild) ---------------
-- The index on `scope` blocks DROP COLUMN — drop it, re-create it at the end.
DROP INDEX idx_workflows_scope;

ALTER TABLE workflows ADD COLUMN scope_v43 TEXT;
UPDATE workflows SET scope_v43 = scope;
ALTER TABLE workflows DROP COLUMN scope;
ALTER TABLE workflows
    ADD COLUMN scope TEXT NOT NULL DEFAULT 'custom'
    CHECK (scope IN ('ndev', 'nspec', 'ndoc', 'custom', 'coder', 'prototyper'));
UPDATE workflows SET scope = scope_v43;
ALTER TABLE workflows DROP COLUMN scope_v43;

-- Re-create the scope scan index the DROP COLUMN forced us to drop.
CREATE INDEX idx_workflows_scope ON workflows (scope);

-- --- workflow_runs.parent_run_id: the parent run a sub_workflow opened this child FROM --------
ALTER TABLE workflow_runs
    ADD COLUMN parent_run_id TEXT REFERENCES workflow_runs(id) ON DELETE SET NULL;
