-- Revert schema v34 (the ORCHESTRATION ENGINE domain). Drop the runs first (they FK the
-- workflow + its nodes), then the nodes (they FK the workflow and self-reference), then the
-- workflows. `nodes` and `workflows` reference each other (workflows.root_node_id → nodes,
-- nodes.workflow_id → workflows); dropping in this order with the child rows already gone is
-- safe.
DROP TABLE IF EXISTS workflow_runs;
DROP TABLE IF EXISTS nodes;
DROP TABLE IF EXISTS workflows;
