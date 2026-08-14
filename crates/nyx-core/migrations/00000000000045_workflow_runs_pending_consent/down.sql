-- Revert v45: drop the run's pending human consent (nullable, no index / FK — a plain drop).
ALTER TABLE workflow_runs DROP COLUMN pending_consent;
