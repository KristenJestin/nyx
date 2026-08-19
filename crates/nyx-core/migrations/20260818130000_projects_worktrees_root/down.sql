-- Revert 20260818130000: drop `projects.worktrees_root` and narrow `nodes.type` back (remove
-- 'create_workspace'). Same in-place column-rebuild recipe as the up. Clean only when no row uses
-- the removed vocabulary.

-- --- projects.worktrees_root: a plain drop (nullable, no index / FK) --------------------------
ALTER TABLE projects DROP COLUMN worktrees_root;

-- --- nodes.type: narrow back (plain in-place rebuild) -----------------------------------------
ALTER TABLE nodes ADD COLUMN type_stash TEXT;
UPDATE nodes SET type_stash = "type";
ALTER TABLE nodes DROP COLUMN "type";
ALTER TABLE nodes
    ADD COLUMN "type" TEXT NOT NULL DEFAULT 'note'
    CHECK ("type" IN (
        'sequence', 'loop', 'selector',
        'spawn_subagent', 'judge', 'ask_human', 'produce',
        'gate_check', 'transition', 'wait', 'note', 'query', 'fork_spec',
        'sub_workflow', 'transition_task'
    ));
UPDATE nodes SET "type" = type_stash;
ALTER TABLE nodes DROP COLUMN type_stash;
