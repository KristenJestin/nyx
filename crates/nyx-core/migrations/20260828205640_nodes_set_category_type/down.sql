-- Revert 20260828205640: narrow `nodes.type` back (remove 'set_category'). Same in-place
-- column-rebuild recipe as the up.
--
-- ## THIS DOWN CLEANS UP AFTER ITSELF, AND ITS PREDECESSORS DID NOT
--
-- The four earlier vocabulary-narrowing migrations (v41 `fork_spec`, v43 `sub_workflow` /
-- `transition_task`, 20260818130000 `create_workspace`) all say "clean only when no row uses the
-- removed vocabulary" and leave the deletion to whoever runs the revert. That was livable for exactly
-- one reason: every one of them sits BELOW the migrations any test peels through, so the promise was
-- never tested against a database that had actually booted. This one is the HEAD, and a nyx that has
-- booted once always has `set_category` rows — the seeded `/nspec` of revision 23 carries one — so
-- "clean only when no row speaks it" would mean "never clean", and the first test to peel past this
-- point would die on a CHECK constraint with no way to proceed. It did, immediately
-- (`a_pre_groups_database_gains_the_group_surface_on_the_real_boot_path`).
--
-- A revert that removes a vocabulary must remove what is written in it; anything else is not a
-- revert. So:
--
--   1. every BUILT-IN workflow that holds such a node is marked STALE (`builtin_revision` NULL, which
--      `seed::ensure_builtin` reads as 0 and therefore older than whatever the running binary ships).
--      The next boot REBUILDS its tree from the code, so the graph left behind is a coherent one and
--      not a sequence with a hole where its classification step used to be. Only the workflows that
--      actually carried the node are touched — marking the others stale would rebuild trees this
--      migration never affected, and destroy the live runs walking them (M4);
--   2. the nodes themselves are deleted. `set_category` is a LEAF, so nothing cascades; a workflow
--      whose ROOT was one becomes un-rooted (`root_node_id` is `ON DELETE SET NULL`), which is the
--      schema's own "empty draft" state.
--
-- ## WHAT A PROJECT WORKFLOW LOSES, SAID OUT LOUD
--
-- Step 1 is filtered `project_id IS NULL`; step 2 is NOT, and it cannot be. The two scopes are
-- deliberately different, and the difference costs a PROJECT workflow — a FORK of a built-in, or a
-- graph somebody authored by hand — its `set_category` node, WITH NO PATH BACK:
--
--   * its node is deleted like every other, because the CHECK re-added at step 3 refuses the value:
--     leaving one row behind would make this revert fail, so "delete only the built-ins' nodes" is
--     not an available option;
--   * it is NOT marked stale, and marking it would change nothing. `builtin_revision` is the stamp
--     of a row the SEEDER wrote; a fork carries `forked_from` / `forked_from_revision` and a NULL
--     `builtin_revision` (see `mcp_engine::fork_stale_warning`), and `seed::ensure_builtin` only
--     ever rebuilds `project_id IS NULL` rows. There is nothing for a boot to rebuild a project
--     graph FROM — it is the project's own graph, not a copy of anything the code still holds;
--   * so the graph stays AMPUTATED: a sequence with a hole where its classification step was, and
--     whatever branch read the category downstream now reads an unclassified Spec. Restoring it
--     means re-forking the built-in (which re-adopts nyx's current graph and drops the project's
--     edits) or re-authoring the node by hand once the vocabulary is re-applied.
--
-- That is an ASSUMED loss, not an oversight: this is a revert of a vocabulary, and a fork that
-- speaks it is speaking a vocabulary the database is about to stop accepting. It is written here so
-- whoever runs this revert on a project with forked graphs knows it before running it, rather than
-- finding a hole in a flow later.
--
-- The CHECK rebuild below is then the ordinary in-place stash → drop → re-add-narrowed → restore →
-- drop-stash of the up.

-- --- 1. the built-ins that carried one must be rebuilt on the next boot ------------------------
UPDATE workflows
SET builtin_revision = NULL
WHERE project_id IS NULL
  AND id IN (SELECT workflow_id FROM nodes WHERE "type" = 'set_category');

-- --- 2. no row may keep a vocabulary this migration removes ------------------------------------
DELETE FROM nodes WHERE "type" = 'set_category';

-- --- 3. nodes.type: narrow back (plain in-place rebuild) ---------------------------------------
ALTER TABLE nodes ADD COLUMN type_stash TEXT;
UPDATE nodes SET type_stash = "type";
ALTER TABLE nodes DROP COLUMN "type";
ALTER TABLE nodes
    ADD COLUMN "type" TEXT NOT NULL DEFAULT 'note'
    CHECK ("type" IN (
        'sequence', 'loop', 'selector',
        'spawn_subagent', 'judge', 'ask_human', 'produce',
        'gate_check', 'transition', 'wait', 'note', 'query', 'fork_spec',
        'sub_workflow', 'transition_task', 'create_workspace'
    ));
UPDATE nodes SET "type" = type_stash;
ALTER TABLE nodes DROP COLUMN type_stash;
