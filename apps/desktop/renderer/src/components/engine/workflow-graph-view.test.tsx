import { render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it } from "vitest";

import { mockIPC } from "@/bridge/test-harness";
import type { EngineNode, EngineWorkflowRun } from "@/bridge";

import { WorkflowGraphView } from "./workflow-graph-view";

/**
 * The SHARED graph viewer's run seam. The same component backs `/engine` (NO run — the pure
 * structure) and the Spec-side run modal (a run — cursor + blackboard), so the two states are pinned
 * here rather than through either caller.
 */

const WORKFLOW = {
  id: "wf1",
  project_id: null,
  name: "ndev — dev loop (default)",
  scope: "ndev" as const,
  root_node_id: "root",
  forked_from: null,
  created_at: 0,
  updated_at: 0,
};

function node(
  id: string,
  parentId: string | null,
  orderIndex: number,
  type: string,
  label: string,
  config: Record<string, unknown> = {},
): EngineNode {
  return {
    id,
    workflow_id: "wf1",
    parent_id: parentId,
    order_index: orderIndex,
    type_: type,
    label,
    config: JSON.stringify(config),
    created_at: 0,
    updated_at: 0,
  };
}

const NODES = [
  node("root", null, 0, "sequence", "/ndev"),
  node("loop", "root", 0, "loop", "phases"),
  node("coder", "loop", 0, "spawn_subagent", "coder", { agent_id: "coder" }),
  node("gate", "loop", 1, "gate_check", "checks", { command_ids: ["typecheck"] }),
];

const RUN: EngineWorkflowRun = {
  id: "run1",
  workflow_id: "wf1",
  spec_id: "spec1",
  parent_run_id: null,
  current_node_id: "coder",
  blackboard: JSON.stringify({ phase: "1/3", gate_green: false }),
  status: "running",
  started_at: 0,
  updated_at: 0,
  pending_consent: null,
};

beforeEach(() => {
  mockIPC((cmd) => {
    if (cmd === "engine_workflow_graph") return { workflow: WORKFLOW, nodes: NODES };
    if (cmd === "engine_effector_context") return { context: "=== NYX CONTEXT — CODER ===" };
    return null;
  });
});

describe("<WorkflowGraphView> — the optional run", () => {
  it("with NO run: the pure structure — no cursor, no blackboard", async () => {
    const { container } = render(<WorkflowGraphView workflowId="wf1" />);
    await waitFor(() =>
      expect(container.querySelector('[data-node-type="spawn_subagent"]')).toBeTruthy(),
    );
    expect(container.querySelector('[data-current="true"]')).toBeNull();
    expect(screen.queryByText("Blackboard")).toBeNull();
    expect(screen.getByTestId("graph-inspector-hint")).toBeTruthy();
  });

  it("with a run + showRuntimePanel: the cursor is highlighted and the blackboard follows it", async () => {
    const { container } = render(
      <WorkflowGraphView workflowId="wf1" run={RUN} showRuntimePanel />,
    );
    await waitFor(() =>
      expect(container.querySelector('[data-node-type="spawn_subagent"]')).toBeTruthy(),
    );
    await waitFor(() => expect(container.querySelector('[data-current="true"]')).toBeTruthy());
    expect(screen.getByText("Blackboard")).toBeTruthy();
    expect(screen.getByText("phase")).toBeTruthy();
    expect(screen.getByText("1/3")).toBeTruthy();
    expect(screen.queryByTestId("graph-inspector-hint")).toBeNull();
  });

  it("with a run but no runtime panel: the cursor shows, the blackboard does not", async () => {
    const { container } = render(<WorkflowGraphView workflowId="wf1" run={RUN} />);
    await waitFor(() => expect(container.querySelector('[data-current="true"]')).toBeTruthy());
    expect(screen.queryByText("Blackboard")).toBeNull();
  });
});
