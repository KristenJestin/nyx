-- Revert `command_groups_layout_shape` (20260827120000): drop the group's tiling-shape label.
--
-- No index and no other table names `layout_shape`, and its CHECK is a COLUMN constraint (it goes
-- away with the column it is written on), so a plain DROP COLUMN is enough here — unlike
-- `20260819140000`'s down, which had to drop an index first. Reverting takes the disposition back
-- to the shape derived from the ordered member list alone, which is exactly what `auto` means, so
-- no group changes appearance on the way down.
ALTER TABLE managed_command_groups DROP COLUMN layout_shape;
