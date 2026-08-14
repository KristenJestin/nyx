-- Schema v42: `workflow_runs.workspace_id` — the dev workspace a run was OPENED against (v2.8 review
-- N24). run_start resolves it (from an explicit workspace_id / the cwd) and stores it here; when the
-- graph's entry `transition → in_progress` fires, `apply_transition` reads it and POSES the Spec's
-- workspace anchor in the same op (`transition_spec_with_workspace`) — exactly what the manual
-- `spec_transition start` did. Without it a graph-started Spec lost its anchor and the coder spawned
-- behind it worked with no workspace. Nullable (a run opened by id alone, or an unbound run, has none);
-- no FK enforcement (a soft reference — a deleted workspace simply leaves the anchor unposed).

ALTER TABLE workflow_runs ADD COLUMN workspace_id TEXT;
