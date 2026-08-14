-- Revert v43: drop `workflow_runs.parent_run_id`, narrow `workflows.scope` back (remove 'coder' /
-- 'prototyper') and `nodes.type` back (remove 'sub_workflow' / 'transition_task'). Same in-place
-- column-rebuild recipes as the up (index-aware for `scope`). Clean only when no row uses the
-- removed vocabulary.

-- --- workflow_runs.parent_run_id: a plain drop (nullable, no index) ---------------------------
ALTER TABLE workflow_runs DROP COLUMN parent_run_id;

-- --- workflows.scope: narrow back (index-aware in-place rebuild) ------------------------------
DROP INDEX idx_workflows_scope;

ALTER TABLE workflows ADD COLUMN scope_v43 TEXT;
UPDATE workflows SET scope_v43 = scope;
ALTER TABLE workflows DROP COLUMN scope;
ALTER TABLE workflows
    ADD COLUMN scope TEXT NOT NULL DEFAULT 'custom'
    CHECK (scope IN ('ndev', 'nspec', 'ndoc', 'custom'));
UPDATE workflows SET scope = scope_v43;
ALTER TABLE workflows DROP COLUMN scope_v43;

CREATE INDEX idx_workflows_scope ON workflows (scope);

-- --- nodes.type: narrow back (plain in-place rebuild) -----------------------------------------
ALTER TABLE nodes ADD COLUMN type_v43 TEXT;
UPDATE nodes SET type_v43 = "type";
ALTER TABLE nodes DROP COLUMN "type";
ALTER TABLE nodes
    ADD COLUMN "type" TEXT NOT NULL DEFAULT 'note'
    CHECK ("type" IN (
        'sequence', 'loop', 'selector',
        'spawn_subagent', 'judge', 'ask_human', 'produce',
        'gate_check', 'transition', 'wait', 'note', 'query', 'fork_spec'
    ));
UPDATE nodes SET "type" = type_v43;
ALTER TABLE nodes DROP COLUMN type_v43;
