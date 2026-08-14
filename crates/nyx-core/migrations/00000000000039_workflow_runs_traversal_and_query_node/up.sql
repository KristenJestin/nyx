-- Schema v39: the persisted-resume TRAVERSAL column + the `query` node type — the v2.8-review
-- foundation (B1 + B2). Two independent additions in one migration (both land past the live floor).
--
-- ## workflow_runs.traversal (B1 — cross-session resume)
--
-- The interpreter's traversal STACK (which loop iteration / sequence child a run is on) lives only
-- in the in-memory EngineRuntime; a session close loses it, so a cold `nyx_next` on a progressed run
-- is refused and a multi-day grill / review can never reach `done`. This column persists that stack
-- (serialized by NODE ID, restored on the next boot). It is SEPARATE from `blackboard` (which is
-- returned to the dispatcher each tick and is the run's DATA, not the engine's internals). Nullable:
-- pre-B1 runs have no traversal (and stay non-resumable).
--
-- ## nodes.type += 'query' (B2 — an internal nyx-fact leaf)
--
-- A `query` node reads nyx state and writes a verifiable fact to the blackboard (spec_status, …) —
-- like gate_check/transition, a nyx-OWN effect, never surfaced to the dispatcher. Widening the CHECK
-- uses the v25/v29 in-place COLUMN-REBUILD recipe (SQLite cannot ALTER a CHECK, and a whole-table
-- rebuild would fight the inbound FKs under foreign_keys=ON, which cannot be toggled inside Diesel's
-- migration transaction). `nodes."type"` has NO index and NO inbound FK, so the rebuild is a plain
-- stash → drop → re-add-with-widened-CHECK → restore → drop-stash. ADD COLUMN NOT NULL needs a
-- default on a populated table; 'note' is a transient placeholder (every row's real type is restored
-- immediately below, and the backend always supplies `type` on insert — the default is never used).

ALTER TABLE workflow_runs ADD COLUMN traversal TEXT;

ALTER TABLE nodes ADD COLUMN type_v39 TEXT;
UPDATE nodes SET type_v39 = "type";
ALTER TABLE nodes DROP COLUMN "type";
ALTER TABLE nodes
    ADD COLUMN "type" TEXT NOT NULL DEFAULT 'note'
    CHECK ("type" IN (
        'sequence', 'loop', 'selector',
        'spawn_subagent', 'judge', 'ask_human', 'produce',
        'gate_check', 'transition', 'wait', 'note', 'query'
    ));
UPDATE nodes SET "type" = type_v39;
ALTER TABLE nodes DROP COLUMN type_v39;
