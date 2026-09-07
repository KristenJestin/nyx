-- Revert 20260907154551: drop the command template's agent note. Plain drop (nullable, no index /
-- FK / CHECK) — a command without its note simply stops carrying one, and the ambient block stops
-- rendering the line.

ALTER TABLE managed_commands DROP COLUMN description;
