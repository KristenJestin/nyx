-- Revert v49: drop the human turn's consumption mark (nullable, no index / FK — a plain drop).
ALTER TABLE human_turns DROP COLUMN consumed_by;
