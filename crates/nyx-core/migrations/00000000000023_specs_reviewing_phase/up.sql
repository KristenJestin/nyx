-- Schema v23: the `specs.reviewing_phase` column — the STORED phase under review (v2.5.11 "Boucle
-- review complète", review follow-up finding 01KXZP1PY71W5P4N3VYJBHP91G — "État de phase explicite
-- (reviewing_phase stocké) + override phase optionnel").
--
-- ## Why (PRD Impl Decisions, PDR-0022)
--
-- A retour/finding rattaches to a Spec PHASE (v22 `tasks.round` made rounds visible; the finding is
-- PHASED so the rail gauge counts it). Deriving that phase live from task delivery
-- (`current_phase_for_spec` = the last fully-delivered phase, F1) is correct but IMPLICIT. Storing
-- the phase under review makes it EXPLICIT:
--
--   * a STABLE ANCHOR across a stepped review episode — the overlay badge ("phase X en review") and
--     the finding attribution key off ONE stored number instead of re-deriving it each time;
--   * the middle rung of the finding-phase PRECEDENCE (an explicit MCP `phase` param > this stored
--     `reviewing_phase` while `in_review` > the derived F1 phase).
--
-- POSED at the entry into `in_review` (= the last delivered phase, F1) and CLEARED on every OTHER
-- lifecycle transition (done = advance out of review, cancel) — both owned by
-- `transition_spec_with_workspace`. NULL = not under a per-phase review (a draft/in_progress Spec, or
-- an autopilot Spec whose final review has no per-phase anchor — the finding then falls back to the
-- derived F1 phase).
--
-- `reviewing_phase` is NULLABLE and un-back-filled (NULL for every existing Spec). SQLite `ALTER
-- TABLE ADD COLUMN` is a metadata-only, in-place change. Column order here is the physical order
-- `schema.rs` must mirror (Diesel positional decoding): appended after `archived_at`.

ALTER TABLE specs
    ADD COLUMN reviewing_phase INTEGER;
