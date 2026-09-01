-- Revert 20260828182220: drop the project identity columns. Plain drops (nullable, no index /
-- FK); `image_path` goes first so the CHECK that names `icon` leaves with the column that carries
-- it.

ALTER TABLE projects DROP COLUMN image_path;
ALTER TABLE projects DROP COLUMN icon;
ALTER TABLE projects DROP COLUMN accent;
