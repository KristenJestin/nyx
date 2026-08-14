import { fireEvent, render, screen } from "@testing-library/react";
import { ReactFlow, type Edge, type Node } from "@xyflow/react";
import { describe, expect, it, vi } from "vitest";

import { engineEdgeTypes } from "./engine-edges";
import { EngineInteractionContext, engineNodeTypes, type EngineInteraction } from "./engine-nodes";

/** Render a set of nodes through a real `<ReactFlow>` so the custom node registry + Handle context
 *  are exercised exactly as in the app. */
function renderGraph(nodes: Node[], interaction: EngineInteraction = {}, edges: Edge[] = []) {
  return render(
    <EngineInteractionContext.Provider value={interaction}>
      <div style={{ width: 900, height: 640 }}>
        <ReactFlow
          nodes={nodes}
          edges={edges}
          nodeTypes={engineNodeTypes}
          edgeTypes={engineEdgeTypes}
          fitView
          proOptions={{ hideAttribution: true }}
        />
      </div>
    </EngineInteractionContext.Provider>,
  );
}

const flowNode = (id: string, nodeType: string, extra: Record<string, unknown> = {}): Node => ({
  id,
  type: "engineFlow",
  position: { x: 0, y: 0 },
  data: { nodeType, label: id, nodeId: id, current: false, selected: false, ...extra },
});

const groupNode = (
  id: string,
  nodeType: string,
  label: string,
  selected = false,
  rootLabel = "",
  loopBack: string | null = null,
): Node => ({
  id,
  type: "engineGroup",
  position: { x: 0, y: 0 },
  style: { width: 320, height: 200 },
  data: { nodeType, label, nodeId: id, rootLabel, selected, loopBack },
});

describe("engine custom nodes", () => {
  it("renders a custom node for each of the leaf types + the start/terminal pills", async () => {
    const nodes: Node[] = [
      flowNode("a", "spawn_subagent", { agentId: "coder" }),
      flowNode("b", "gate_check", { commandIds: [] }),
      flowNode("c", "judge"),
      flowNode("d", "selector"),
      flowNode("e", "transition"),
      {
        id: "s",
        type: "enginePill",
        position: { x: 0, y: 200 },
        data: { nodeType: "start", label: "/ndev" },
      },
      {
        id: "t",
        type: "enginePill",
        position: { x: 0, y: 300 },
        data: { nodeType: "terminal", label: "terminal" },
      },
    ];
    const { container } = renderGraph(nodes);
    // The start pill leads with an explicit "Start" entry-point badge (finding #30).
    await screen.findByText("Start");
    for (const type of ["spawn_subagent", "gate_check", "judge", "selector", "transition"]) {
      expect(container.querySelector(`[data-node-type="${type}"]`)).toBeTruthy();
    }
    expect(container.querySelector('[data-pill-type="start"]')).toBeTruthy();
    expect(container.querySelector('[data-pill-type="terminal"]')).toBeTruthy();
  });

  it("renders loop + sequence GROUP container nodes", async () => {
    const nodes: Node[] = [
      {
        id: "loop",
        type: "engineGroup",
        position: { x: 0, y: 0 },
        style: { width: 320, height: 220 },
        data: { nodeType: "loop", label: "phases", nodeId: "loop" },
      },
      {
        id: "seq",
        type: "engineGroup",
        position: { x: 0, y: 260 },
        style: { width: 320, height: 120 },
        data: { nodeType: "sequence", label: "body", nodeId: "seq" },
      },
    ];
    const { container } = renderGraph(nodes);
    // Each group chip leads with its composite TYPE (finding #30) and keeps a MEANINGFUL own label
    // (e.g. `phases` / `body`) as muted context — only a bare workflow-name echo is dropped.
    await screen.findByText("loop");
    expect(screen.getByText("sequence")).toBeTruthy();
    expect(screen.getByText("· phases")).toBeTruthy();
    expect(screen.getByText("· body")).toBeTruthy();
    expect(container.querySelector('[data-group-type="loop"]')).toBeTruthy();
    expect(container.querySelector('[data-group-type="sequence"]')).toBeTruthy();
  });

  // finding #39 — a LOOP group carries the reboucle indicator on its header (↺ + its until
  // condition), contained inside the box; a sequence group has none. The floating arc is gone.
  it("shows the ↺ loop-back indicator with its until condition on a loop group only", async () => {
    const { container } = renderGraph([
      groupNode("loop", "loop", "phases", false, "", "phase_status = complete"),
      groupNode("seq", "sequence", "body"),
    ]);
    await screen.findByText("loop");
    const indicator = container.querySelector("[data-loop-back]") as HTMLElement | null;
    expect(indicator).toBeTruthy();
    expect(indicator!.textContent).toContain("phase_status = complete");
    // The indicator is anchored on the loop box header (contained), and reads as a loop-back.
    expect(indicator!.getAttribute("aria-label")).toMatch(
      /loops back until phase_status = complete/,
    );
    // A sequence group carries NO loop-back indicator — only the loop reboucles.
    const seq = container.querySelector('[data-group-type="sequence"]') as HTMLElement;
    expect(seq.querySelector("[data-loop-back]")).toBeNull();
  });

  it("shows a bare ↺ loop-back on a loop whose until condition is undescribable", async () => {
    const { container } = renderGraph([groupNode("loop", "loop", "phases")]);
    await screen.findByText("loop");
    const indicator = container.querySelector("[data-loop-back]") as HTMLElement | null;
    expect(indicator).toBeTruthy();
    expect(indicator!.getAttribute("aria-label")).toBe("loops back");
  });

  it("renders a spawn node's clickable {{sub_agent:id}} marker that fires onMarker", async () => {
    const onMarker = vi.fn();
    renderGraph([flowNode("coder", "spawn_subagent", { agentId: "coder" })], { onMarker });
    const chip = await screen.findByText("sub_agent:coder");
    expect(chip.tagName).toBe("BUTTON");
    fireEvent.click(chip);
    expect(onMarker).toHaveBeenCalledWith({ kind: "sub_agent", id: "coder", nodeId: "coder" });
  });

  it("renders a gate node's clickable {{check:id}} markers", async () => {
    const onMarker = vi.fn();
    renderGraph([flowNode("gate", "gate_check", { commandIds: ["typecheck"] })], { onMarker });
    const chip = await screen.findByText("check:typecheck");
    expect(chip.tagName).toBe("BUTTON");
    fireEvent.click(chip);
    expect(onMarker).toHaveBeenCalledWith({ kind: "check", id: "typecheck", nodeId: "gate" });
  });

  it("marks the runtime current node for highlighting", async () => {
    const { container } = renderGraph([flowNode("coder", "spawn_subagent", { current: true })]);
    await screen.findByText("coder");
    expect(container.querySelector('[data-current="true"]')).toBeTruthy();
    expect(screen.getByLabelText("current node")).toBeTruthy();
  });

  // finding #30 + declutter — the root group shows ONLY its composite TYPE (sequence/loop). The
  // workflow name lives ONCE, on the start pill; the root group must NOT re-print it (no
  // "sequence · /nspec"), yet start ("Start · …") and group ("sequence") stay distinct.
  it("labels a group by its composite type, without re-printing the workflow name", async () => {
    const { container } = renderGraph([
      groupNode("root", "sequence", "/nspec", false, "/nspec"),
      {
        id: "s",
        type: "enginePill",
        position: { x: 0, y: 240 },
        data: { nodeType: "start", label: "/nspec" },
      },
    ]);
    await screen.findByText("Start");
    // The group chip shows the TYPE only — no echo of the workflow name.
    const group = container.querySelector('[data-group-type="sequence"]') as HTMLElement;
    expect(group).toBeTruthy();
    expect(group.textContent).toContain("sequence");
    expect(group.textContent).not.toContain("/nspec");
    // The start pill still owns the workflow name — start ≠ group stays true (#30).
    expect(screen.getByText("Start")).toBeTruthy();
    expect(screen.getByText("· /nspec")).toBeTruthy();
  });

  // finding #32 — EVERY node is clickable and opens its inspector via onSelectNode.
  it("fires onSelectNode when a flow node is clicked", async () => {
    const onSelectNode = vi.fn();
    const { container } = renderGraph([flowNode("gate", "gate_check")], { onSelectNode });
    await screen.findByText("gate");
    fireEvent.click(container.querySelector('[data-node-type="gate_check"]')!);
    expect(onSelectNode).toHaveBeenCalledWith("gate");
  });

  it("fires onSelectNode when a composite group's type chip is clicked", async () => {
    const onSelectNode = vi.fn();
    renderGraph([groupNode("loop", "loop", "phases")], { onSelectNode });
    fireEvent.click(await screen.findByText("loop"));
    expect(onSelectNode).toHaveBeenCalledWith("loop");
  });

  // finding #35 — the graph↔children CROSS-HIGHLIGHT: a flow node paints `data.hovered` (driven from
  // the inspector's children list) AND reports its own hover up via `onHoverNode` (driving the list).
  it("paints data.hovered and reports hover both ways", async () => {
    const onHoverNode = vi.fn();
    const { container } = renderGraph([flowNode("coder", "spawn_subagent", { hovered: true })], {
      onHoverNode,
    });
    await screen.findByText("coder");
    const node = container.querySelector('[data-node-type="spawn_subagent"]') as HTMLElement;
    expect(node.getAttribute("data-hovered")).toBe("true");
    fireEvent.mouseEnter(node);
    expect(onHoverNode).toHaveBeenCalledWith("coder");
    fireEvent.mouseLeave(node);
    expect(onHoverNode).toHaveBeenCalledWith(null);
  });

  // finding #33 — a selected node (inspector open) carries a distinct selected state.
  it("marks a selected flow node and a selected group", async () => {
    const { container } = renderGraph([
      flowNode("coder", "spawn_subagent", { selected: true }),
      groupNode("loop", "loop", "phases", true),
    ]);
    await screen.findByText("coder");
    expect(
      container.querySelector('[data-node-type="spawn_subagent"][data-selected="true"]'),
    ).toBeTruthy();
    expect(container.querySelector('[data-group-type="loop"][data-selected="true"]')).toBeTruthy();
  });
});
