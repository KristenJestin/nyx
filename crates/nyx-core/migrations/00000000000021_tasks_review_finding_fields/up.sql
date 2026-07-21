-- Schema v21: PORT the review-finding fields onto `tasks` — findings = `tasks kind=review`
-- (v2.5.11 "Boucle review complète", Phase 1, task 01KXR1CWP2FMB2DYJG467WAMYQ —
-- "Findings = tasks kind=review (aligné overlay) — réconcilier/déprécier review_tasks").
--
-- ## Why (PRD Impl Decisions, PDR-0016)
--
-- The canonical review FINDING is a `tasks kind=review` row (carrying `severity`) — the SAME
-- model the overlay already reads via `list_tasks_for_spec` + `<TasksTab>`. The separate
-- `review_tasks` table (v17, PDR-0012) is DEPRECATED / merged into `tasks`: its two useful
-- columns are PORTED here, plus a new `origin`, so a finding carries everything a promoted
-- `review_tasks` row did — in ONE representation. There is no longer a competing findings model.
--
--   * `from_comment_id` — the trail back to the `review_comments` row a finding was PROMOTED
--     from (the comment↔task link, 1→N). A plain correlation value (no FK CASCADE), like
--     `review_tasks.from_comment_id`: the finding is durable coder work that survives Git churn /
--     an archive. NULL on a finding NOT promoted from a comment (e.g. one the auditor raised
--     directly via `task_add`).
--   * `grill_state` — the promotion "grill": proposed (human lâche brut) → structured (agent
--     structures via MCP) → validated (human validates; only then ready for the coder). Ported
--     verbatim from `review_tasks.grill_state`. CHECK-enforced. NULL on a task that is not a
--     grilled review finding.
--   * `origin` — WHO raised the finding: `agent` (an auditor/agent finding, closable by the
--     agent coder loop) vs `human` (a human finding, HUMAN-ONLY resolution). Drives the
--     resolution guard (task 01KXR1CX5Z4XV2M05HTTDQJSZN). CHECK-enforced. NULL on a non-finding.
--
-- All three are NULLABLE and un-back-filled: a plain `slice`/`human` task — and every existing
-- row — keeps NULL. SQLite `ALTER TABLE ADD COLUMN` is a metadata-only, in-place change; a CHECK
-- that references only the new column is permitted and is not re-validated against existing rows
-- (all NULL, which satisfies the `... IS NULL OR ...` guard). Column order here is the physical
-- order `schema.rs` must mirror (Diesel positional decoding): appended after `completed_at`.

ALTER TABLE tasks
    ADD COLUMN from_comment_id TEXT;

ALTER TABLE tasks
    ADD COLUMN grill_state TEXT
    CHECK (grill_state IS NULL OR grill_state IN ('proposed', 'structured', 'validated'));

ALTER TABLE tasks
    ADD COLUMN origin TEXT
    CHECK (origin IS NULL OR origin IN ('agent', 'human'));

-- "the finding task(s) promoted from THIS comment" — the trail from a comment to its findings
-- (1→N, the comment↔task mapping). Mirrors the deprecated `idx_review_tasks_comment`.
CREATE INDEX idx_tasks_from_comment ON tasks (from_comment_id);
