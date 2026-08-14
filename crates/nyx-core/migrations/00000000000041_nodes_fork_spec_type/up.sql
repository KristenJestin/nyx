-- Schema v41: the `fork_spec` node type — a nyx-internal leaf that FORKS the run's bound Spec into a
-- new editable revision (v2.8 review N13(b)). Widening the `nodes.type` CHECK uses the same in-place
-- COLUMN-REBUILD recipe as v39 (SQLite cannot ALTER a CHECK, and a whole-table rebuild would fight the
-- inbound FKs under foreign_keys=ON, which cannot be toggled inside Diesel's migration transaction).
-- `nodes."type"` has no index / inbound FK, so it is a plain stash → drop → re-add-widened → restore →
-- drop-stash. The transient DEFAULT 'note' is never used (every row's real type is restored below, and
-- the backend always supplies `type` on insert).

ALTER TABLE nodes ADD COLUMN type_v41 TEXT;
UPDATE nodes SET type_v41 = "type";
ALTER TABLE nodes DROP COLUMN "type";
ALTER TABLE nodes
    ADD COLUMN "type" TEXT NOT NULL DEFAULT 'note'
    CHECK ("type" IN (
        'sequence', 'loop', 'selector',
        'spawn_subagent', 'judge', 'ask_human', 'produce',
        'gate_check', 'transition', 'wait', 'note', 'query', 'fork_spec'
    ));
UPDATE nodes SET "type" = type_v41;
ALTER TABLE nodes DROP COLUMN type_v41;
