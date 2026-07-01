-- Reverse schema v3. Drop `command_instances` first (it references
-- `managed_commands`), then `managed_commands`. Their indexes go with the tables.
DROP INDEX IF EXISTS idx_command_instances_workspace;
DROP INDEX IF EXISTS idx_command_instances_command;
DROP TABLE command_instances;

DROP INDEX IF EXISTS idx_managed_commands_project;
DROP TABLE managed_commands;
