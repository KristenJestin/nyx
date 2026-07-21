-- Reverse schema v22: drop the per-phase round index then the `round` column off `tasks`.
DROP INDEX IF EXISTS idx_tasks_spec_phase_round;

ALTER TABLE tasks DROP COLUMN round;
