-- Revert v41: narrow nodes.type back (remove 'fork_spec'). Same in-place column-rebuild recipe.
-- (Clean only when no `fork_spec` node rows remain.)

ALTER TABLE nodes ADD COLUMN type_v41 TEXT;
UPDATE nodes SET type_v41 = "type";
ALTER TABLE nodes DROP COLUMN "type";
ALTER TABLE nodes
    ADD COLUMN "type" TEXT NOT NULL DEFAULT 'note'
    CHECK ("type" IN (
        'sequence', 'loop', 'selector',
        'spawn_subagent', 'judge', 'ask_human', 'produce',
        'gate_check', 'transition', 'wait', 'note', 'query'
    ));
UPDATE nodes SET "type" = type_v41;
ALTER TABLE nodes DROP COLUMN type_v41;
