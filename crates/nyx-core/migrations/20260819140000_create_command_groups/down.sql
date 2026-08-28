-- Revert `create_command_groups` (20260819140000): drop the multi-pane grouping of managed commands — the membership columns on
-- `managed_commands` first, then the group table itself.
--
-- THE ORDER IS NOT FREE. SQLite refuses `ALTER TABLE ... DROP COLUMN` on a column that an index
-- references, so `idx_managed_commands_group` MUST be dropped BEFORE `group_order` and `group_id`
-- go; reordering these two lines makes the down fail. The group table's own index and the table
-- follow, in the mirror of up.sql.
DROP INDEX IF EXISTS idx_managed_commands_group;
ALTER TABLE managed_commands DROP COLUMN group_order;
ALTER TABLE managed_commands DROP COLUMN group_id;
DROP INDEX IF EXISTS idx_managed_command_groups_project;
DROP TABLE IF EXISTS managed_command_groups;
