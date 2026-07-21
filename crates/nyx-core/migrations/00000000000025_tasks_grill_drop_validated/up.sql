-- Schema v25: DROP the `validated` grill state — collapse the finding grill to TWO states
-- (v2.5.11 "Boucle review complète", review follow-up finding 01KY28XXPP3JJ18R811C5VWYA1 (#31) —
-- "Grill : collapse — drop l'état validated (proposed → structured = prêt-coder)").
--
-- ## Why (review follow-up: the human-only `validated` step was redundant)
--
-- A review FINDING is agent-owned, and the agent's grill IS the capture: `proposed` (the agent
-- captured the raw retour) → `structured` (the agent cleaned it up = READY for the coder). The old
-- third state `validated` was a HUMAN-only step that duplicated the agent's ownership — with the
-- corrected model it carries no signal, so it (and its `validate_finding` + human-only bridge) go.
-- The coder now consumes a `structured` finding directly (`Task::is_finding_ready_for_coder`).
--
-- This TIGHTENS the `tasks.grill_state` CHECK from `proposed|structured|validated` (v21) to
-- `proposed|structured`. SQLite has no ALTER-COLUMN-DROP-CONSTRAINT, and rebuilding the whole
-- `tasks` table would fire the ON DELETE CASCADE of its inbound FKs (`task_dependencies`,
-- self-`parent_task_id`) under `foreign_keys=ON` (which cannot be toggled inside Diesel's migration
-- transaction). So we REBUILD the single column IN PLACE: stash it (mapping any existing `validated`
-- row to `structured` — the finding was ready, and `structured` is now the ready state), DROP it,
-- and ADD it back with the tightened CHECK. `ALTER TABLE ADD COLUMN` re-appends, so `grill_state`
-- now sits LAST (after the v22 `round`) — `schema.rs` mirrors this physical order (Diesel positional
-- decoding), exactly as v21/v22/v24 documented for their appended columns.

ALTER TABLE tasks ADD COLUMN grill_state_v25 TEXT;

UPDATE tasks
    SET grill_state_v25 = CASE
        WHEN grill_state = 'validated' THEN 'structured'
        ELSE grill_state
    END;

ALTER TABLE tasks DROP COLUMN grill_state;

ALTER TABLE tasks
    ADD COLUMN grill_state TEXT
    CHECK (grill_state IS NULL OR grill_state IN ('proposed', 'structured'));

UPDATE tasks SET grill_state = grill_state_v25;

ALTER TABLE tasks DROP COLUMN grill_state_v25;
