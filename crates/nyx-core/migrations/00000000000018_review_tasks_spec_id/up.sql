-- Schema v18: SCOPE a promoted `review_tasks` row to the Spec its source comment belongs to
-- (v2.5 review polish, PRD 01KX0EEP5H1MJW4T3QCZEWPHFK, finding #53 — the TASKS counterpart of the
-- comment-scoping fix #41).
--
-- ## Why a `spec_id` on `review_tasks` (the leak this closes)
--
-- `review_comments` already carries an OPTIONAL `spec_id` (schema v16) so a Spec-scoped review shows
-- ONLY its own comments (fix #41). But the PROMOTED task derived from a comment did NOT carry the
-- Spec: `review_tasks` keyed only on `(workspace_id, repository_id)`, so `list_review_tasks` returned
-- EVERY promoted task of the repo regardless of the Spec — a task promoted from a comment of Spec A
-- surfaced on every OTHER Spec sharing the same `(workspace, repository)` pair (the exact leak of
-- #41, one layer up). This column lets a task INHERIT its source comment's `spec_id` at promotion,
-- and `list_review_tasks` filter by it, so a Spec's task list stays its own.
--
-- `spec_id` is NULLABLE and un-back-filled: pre-v18 rows (and any future light/un-scoped promotion)
-- keep `NULL` = the generic, un-scoped bucket — exactly like `review_comments.spec_id`. No FK CASCADE
-- (a plain correlation value, like the rest of the review artifacts) so a task survives Git churn / a
-- workspace archive. SQLite `ALTER TABLE ADD COLUMN` is a metadata-only, in-place change.
ALTER TABLE review_tasks
    ADD COLUMN spec_id TEXT;

-- BACK-FILL from the source comment: a pre-v18 task was promoted BEFORE it could inherit a Spec, but
-- its `from_comment_id` still points at a comment that DOES carry `spec_id` (fix #41). Copy it over
-- so any task promoted during earlier dogfooding stays visible on its Spec's review instead of
-- silently dropping to the un-scoped bucket. Comments with a NULL `spec_id` (or a since-deleted
-- source) leave the task NULL — the correct un-scoped default. Idempotent; a no-op on a fresh DB.
UPDATE review_tasks
SET spec_id = (SELECT spec_id FROM review_comments WHERE review_comments.id = review_tasks.from_comment_id)
WHERE spec_id IS NULL;

-- The Spec-scoped read: "the promoted tasks of this (workspace, repository) for this Spec" — the
-- review's per-Spec task list. Extends the existing `(workspace_id, repository_id)` index with the
-- Spec so the scoped scan stays indexed.
CREATE INDEX idx_review_tasks_repo_spec ON review_tasks (workspace_id, repository_id, spec_id);
