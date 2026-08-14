-- Revert v39: drop the traversal column, narrow nodes.type back (remove 'query'). Same in-place
-- column-rebuild recipe as the up. (A revert is clean only when no `query` node rows remain.)

ALTER TABLE workflow_runs DROP COLUMN traversal;

ALTER TABLE nodes ADD COLUMN type_v39 TEXT;
UPDATE nodes SET type_v39 = "type";
ALTER TABLE nodes DROP COLUMN "type";
ALTER TABLE nodes
    ADD COLUMN "type" TEXT NOT NULL DEFAULT 'note'
    CHECK ("type" IN (
        'sequence', 'loop', 'selector',
        'spawn_subagent', 'judge', 'ask_human', 'produce',
        'gate_check', 'transition', 'wait', 'note'
    ));
UPDATE nodes SET "type" = type_v39;
ALTER TABLE nodes DROP COLUMN type_v39;
