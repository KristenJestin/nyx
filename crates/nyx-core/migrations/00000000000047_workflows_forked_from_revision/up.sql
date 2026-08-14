-- Schema v47: `workflows.forked_from_revision` — the SOURCE's revision at the instant of the fork,
-- which is what lets nyx tell a project its fork has gone stale.
--
-- `forked_from` already records WHICH workflow a fork was copied from, and `builtin_revision` records
-- which code revision produced a built-in's stored tree. Neither says WHEN the copy was taken, so a
-- fork of the `coder` built-in made at revision 11 was indistinguishable from one made yesterday: the
-- boot seeder upgrades only the rows IT seeded, so the fork silently kept missing every later fix
-- (H9, H12, J2 …) with nothing anywhere saying so.
--
-- Stamped ONCE, by `workflow_fork`, from the source's `builtin_revision`. NULL means the comparison
-- cannot be made: a fork of a fork / of a `custom` template (the source carries no revision — nothing
-- to be stale against), or a fork taken BEFORE this column existed. The two are distinguished at read
-- time by the SOURCE: a NULL stamp whose source IS a revisioned built-in is a fork of unknown age, and
-- `run_start` says exactly that rather than inventing a number for it.

ALTER TABLE workflows ADD COLUMN forked_from_revision INTEGER;
