import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { mockIPC } from "@/bridge/test-harness";
import type { EngineNode } from "@/bridge";

import { SpecRunButton } from "./run-modal";

/**
 * The Spec-side RUNTIME view. `/engine` is structural now, so the run — its cursor on the graph AND
 * its blackboard — is opened from the Spec through this modal. The two entry points (the Spec detail
 * header, the dev overlay header) both render `<SpecRunButton>`, so opening is pinned once here and
 * each entry point only proves the button is present.
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
];

const RUN = {
  id: "run1",
  workflow_id: "wf1",
  spec_id: "s1",
  parent_run_id: null,
  current_node_id: "coder",
  blackboard: JSON.stringify({ phase: "1/3", gate_green: false }),
  status: "running",
  started_at: 0,
  updated_at: 0,
  pending_consent: null,
};

function installBridge(runs: unknown[]) {
  const calls: Array<{ cmd: string; args?: Record<string, unknown> }> = [];
  mockIPC((cmd, args) => {
    calls.push({ cmd, args });
    switch (cmd) {
      case "engine_spec_runs":
        return runs;
      case "engine_workflow_graph":
        return { workflow: WORKFLOW, nodes: NODES };
      case "engine_effector_context":
        return { context: "=== NYX CONTEXT — CODER ===" };
      default:
        return null;
    }
  });
  return calls;
}

describe("<SpecRunButton> / <SpecRunModal> — the Spec's orchestration run", () => {
  it("opens the modal on the run's graph + blackboard for a Spec WITH a run", async () => {
    const calls = installBridge([RUN]);
    const { baseElement } = render(<SpecRunButton specId="s1" />);

    // Closed: nothing is read and nothing is in the DOM (no leaked graph, no focus trap).
    expect(calls.some((c) => c.cmd === "engine_spec_runs")).toBe(false);
    expect(screen.queryByText("Orchestration run")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: /orchestration run/i }));

    expect(await screen.findByText("Orchestration run")).toBeTruthy();
    expect(calls.find((c) => c.cmd === "engine_spec_runs")?.args).toEqual({ specId: "s1" });

    // The SHARED graph, laid out for the run's OWN workflow…
    await waitFor(() =>
      expect(baseElement.querySelector('[data-node-type="spawn_subagent"]')).toBeTruthy(),
    );
    expect(calls.find((c) => c.cmd === "engine_workflow_graph")?.args).toEqual({
      workflowId: "wf1",
    });
    // …with the run's cursor on it…
    await waitFor(() => expect(baseElement.querySelector('[data-current="true"]')).toBeTruthy());
    // …and the blackboard following the run's variables.
    expect(screen.getByText("Blackboard")).toBeTruthy();
    expect(screen.getByText("phase")).toBeTruthy();
    expect(screen.getByText("1/3")).toBeTruthy();
    expect(screen.getByText("gate_green")).toBeTruthy();
  });

  it("shows the empty state for a Spec with NO run", async () => {
    installBridge([]);
    const { baseElement } = render(<SpecRunButton specId="s2" />);
    fireEvent.click(screen.getByRole("button", { name: /orchestration run/i }));

    expect(await screen.findByText("No run for this Spec yet")).toBeTruthy();
    // No graph and no blackboard — there is nothing to overlay.
    expect(baseElement.querySelector('[data-testid="engine-canvas"]')).toBeNull();
    expect(screen.queryByText("Blackboard")).toBeNull();
  });

  it("opens on the ROOT run, never on the sub-run its graph delegated to", async () => {
    // A Spec under build carries the flow the human launched AND the child runs it opened through a
    // `sub_workflow` node. Those children are NEWER, so "the newest run" would land the user on the
    // coder sub-flow when they asked for /ndev — a flow they never started. The ROOT run wins.
    const child = {
      ...RUN,
      id: "run2",
      workflow_id: "wf1",
      parent_run_id: RUN.id,
      blackboard: JSON.stringify({ phase: "3/3" }),
      status: "blocked",
    };
    installBridge([child, RUN]); // newest-first: the child heads the list
    render(<SpecRunButton specId="s1" />);
    fireEvent.click(screen.getByRole("button", { name: /orchestration run/i }));

    // The PARENT's blackboard is on screen, not the child's.
    expect(await screen.findByText("1/3")).toBeTruthy();
    expect(screen.queryByText("3/3")).toBeNull();

    // …but the sub-run stays REACHABLE: opening it swaps the view.
    fireEvent.click(screen.getByRole("button", { name: /sub-run/i }));
    expect(await screen.findByText("3/3")).toBeTruthy();
  });


  it("shows a BREADCRUMB of the run hierarchy once you descend, and walks back up", async () => {
    const child = {
      ...RUN,
      id: "run2",
      workflow_id: "wf1",
      parent_run_id: RUN.id,
      blackboard: JSON.stringify({ phase: "3/3" }),
      status: "running",
    };
    installBridge([child, RUN]);
    render(<SpecRunButton specId="s1" />);
    fireEvent.click(screen.getByRole("button", { name: /orchestration run/i }));
    // Opens on the root: no trail to show yet (one level).
    expect(await screen.findByText("1/3")).toBeTruthy();
    expect(screen.queryByRole("navigation", { name: /run hierarchy/i })).toBeNull();

    // Descend → the trail appears with BOTH levels, the last one being where we are.
    fireEvent.click(screen.getByRole("button", { name: /sub-run/i }));
    expect(await screen.findByText("3/3")).toBeTruthy();
    const trail = screen.getByRole("navigation", { name: /run hierarchy/i });
    expect(within(trail).getAllByRole("button")).toHaveLength(2);

    // The ancestor is clickable to go back up; where-you-are is not.
    const [ancestor, here] = within(trail).getAllByRole("button");
    expect((here as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(ancestor!);
    expect(await screen.findByText("1/3")).toBeTruthy();
  });

  it("does NOT bubble the open click to a parent handler (an overlay header must not toggle)", () => {
    installBridge([RUN]);
    const onParentClick = () => {
      throw new Error("the click bubbled to the panel header");
    };
    render(
      <div onClick={onParentClick}>
        <SpecRunButton specId="s1" />
      </div>,
    );
    fireEvent.click(screen.getByRole("button", { name: /orchestration run/i }));
  });
});
