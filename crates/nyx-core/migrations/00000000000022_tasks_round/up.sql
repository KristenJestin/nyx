-- Schema v22: the `tasks.round` column — REWORK ROUNDS made visible (v2.5.11 "Boucle review
-- complète", Phase 1, task 01KXR1CXPEJ6H1K8YTJNEBYF4S — "Rounds : phase + round sur tasks
-- (findings phasés → visibles dans la jauge du rail)").
--
-- ## Why (PRD Impl Decisions, PDR-0022)
--
-- `tasks.phase` (v14) already groups a task under a Spec phase; `round` adds the SECOND axis that
-- makes the rework VISIBLE as sub-phases (2.1, 2.2, 4.1…):
--
--   * round 0 (represented as NULL here) = the PLAN — the original slices of a phase. Every
--     pre-existing task and every plan slice keeps NULL = round 0 (no back-fill needed).
--   * round ≥ 1 = a REWORK pass rattached to the CURRENT phase. A review finding raised while a
--     phase is being reworked carries that phase's number + the pass's round, so it reads as
--     "2.1" (phase 2, round 1). Each coder pass opens the next round for the phase
--     ([`next_round_for_phase`] computes it) — PER-PHASE numbering (never Spec-level).
--
-- Consequence (the gauge fix): a finding is now PHASED (it carries `phase` like any task), so it
-- enters `tasksOfPhase` / the rail gauge (`overlay-status.ts`) instead of being excluded as an
-- "Unphased" task. `round` is the sub-phase axis the overlay renders on top of that.
--
-- `round` is NULLABLE and un-back-filled (NULL = the plan / round 0). SQLite `ALTER TABLE ADD
-- COLUMN` is a metadata-only, in-place change. Column order here is the physical order `schema.rs`
-- must mirror (Diesel positional decoding): appended after the v21 `origin` column.

ALTER TABLE tasks
    ADD COLUMN round INTEGER;

-- Per-phase round scans: "the findings of this Spec's phase, by round" — the rework sub-phases.
CREATE INDEX idx_tasks_spec_phase_round ON tasks (spec_id, phase, round);
