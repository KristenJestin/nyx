-- Revert v42: drop the run's workspace anchor (nullable, no index / FK — a plain drop).
ALTER TABLE workflow_runs DROP COLUMN workspace_id;
