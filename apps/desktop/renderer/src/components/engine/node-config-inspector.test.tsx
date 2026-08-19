import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { EngineNode } from "@/bridge";

import { NodeConfigInspector } from "./node-config-inspector";

/** Build an `EngineNode` row (mirrors the sidecar `engine_workflow_graph` shape). */
function mk(
  id: string,
  type: string,
  label: string,
  config: Record<string, unknown> = {},
  parentId: string | null = null,
  orderIndex = 0,
): EngineNode {
  return {
    id,
    workflow_id: "wf",
    parent_id: parentId,
    order_index: orderIndex,
    type_: type,
    label,
    config: JSON.stringify(config),
    created_at: 0,
    updated_at: 0,
  };
}

function renderInspector(
  node: EngineNode,
  childNodes: EngineNode[] = [],
  props: Partial<React.ComponentProps<typeof NodeConfigInspector>> = {},
) {
  const onSelectNode = props.onSelectNode ?? vi.fn();
  const onHoverNode = props.onHoverNode ?? vi.fn();
  const result = render(
    <NodeConfigInspector
      node={node}
      childNodes={childNodes}
      onSelectNode={onSelectNode}
      onClose={props.onClose ?? (() => {})}
      hoveredNodeId={props.hoveredNodeId ?? null}
      onHoverNode={onHoverNode}
    />,
  );
  return { ...result, onSelectNode, onHoverNode };
}

describe("NodeConfigInspector — explains the node (finding #35)", () => {
  it("shows a natural-language sentence at the top, not a field dump", () => {
    renderInspector(mk("g", "gate_check", "checks", { command_ids: ["typecheck"], blocking: true }));
    // The header leads with the node NAME now (the type is the muted badge beside it).
    expect(screen.getByText("checks")).toBeTruthy();
    const explanation = screen.getByTestId("node-explanation");
    expect(explanation.textContent).toBe("Runs typecheck; blocks the run until they pass.");
    // The old typed field label is gone — the sentence replaces it.
    expect(screen.queryByText("Commands to run")).toBeNull();
  });

  it("surfaces the N26(a) empty-gate signal in the sentence", () => {
    renderInspector(mk("g", "gate_check", "gate", { command_ids: [] }));
    expect(screen.getByTestId("node-explanation").textContent).toBe(
      "No command configured: always passes green and checks nothing.",
    );
  });

  it("explains a transition, dropping the gate clause when ungated", () => {
    renderInspector(mk("t", "transition", "→ review", { target_state: "in_review" }));
    expect(screen.getByText("→ review")).toBeTruthy();
    expect(screen.getByTestId("node-explanation").textContent).toBe("Moves the Spec to in_review.");
  });

  it("renders code identifiers as <code> spans (backtick markup)", () => {
    const { container } = renderInspector(
      mk("q", "query", "status", { query: "spec_status", write: "spec_status" }),
    );
    const codes = container.querySelectorAll("[data-testid='node-explanation'] code");
    expect(codes.length).toBe(1);
    expect(codes[0]!.textContent).toBe("spec_status");
  });

  it("keeps the raw JSON in a COLLAPSED bottom panel that toggles open (opening upward)", () => {
    renderInspector(mk("t", "transition", "→ ready", { target_state: "ready", gate_on: "ready_ok" }));
    const panel = screen.getByTestId("raw-config");
    const toggle = screen.getByRole("button", { name: /raw config/i });
    // Collapsed by default — the JSON is present but the pane is closed (height-animated to 0).
    expect(toggle.getAttribute("aria-expanded")).toBe("false");
    expect(panel.getAttribute("data-open")).toBeNull();
    expect(panel.textContent).toContain("target_state");
    expect(panel.textContent).toContain("ready_ok");
    // Toggling opens it.
    fireEvent.click(toggle);
    expect(toggle.getAttribute("aria-expanded")).toBe("true");
    expect(screen.getByTestId("raw-config").getAttribute("data-open")).toBe("true");
  });

  it("falls back gracefully for an undescribed type (still shows the raw accordion)", () => {
    renderInspector(mk("w", "wait", "cooldown", { seconds: 30 }));
    expect(screen.getByText("cooldown")).toBeTruthy();
    expect(screen.getByTestId("node-explanation").textContent).toMatch(/No description/i);
    expect(screen.getByTestId("raw-config").textContent).toContain("seconds");
  });

  it("selector → explains its branches and lists its drillable children", () => {
    const onSelectNode = vi.fn();
    renderInspector(
      mk("sel", "selector", "route", {
        branches: [
          { cond: { var: "spec_status", eq: "ready" }, child: 0 },
          { cond: { always: true }, child: 1 },
        ],
      }),
      [
        mk("revise", "transition", "revise a ready spec", {}, "sel", 0),
        mk("create", "spawn_subagent", "create new", { agent_id: "spec" }, "sel", 1),
      ],
      { onSelectNode },
    );
    expect(screen.getByText("route")).toBeTruthy();
    // A SHORT framing sentence — it does NOT re-enumerate the conditions (they are the if-chain below).
    expect(screen.getByTestId("node-explanation").textContent).toBe(
      "Takes the first branch whose condition is true, in order — 2 branches:",
    );
    // A selector shows its BRANCHES, not a bare "Children (n)" header.
    expect(screen.queryByText(/^Children \(/)).toBeNull();
    expect(screen.getByTestId("selector-branches")).toBeTruthy();
    fireEvent.click(screen.getByText("revise a ready spec"));
    expect(onSelectNode).toHaveBeenCalledWith("revise");
  });

  it("renders a selector as an IF-chain: condition above, the child it routes to below", () => {
    renderInspector(
      mk("sel", "selector", "route", {
        branches: [
          { cond: { var: "spec_status", eq: "ready" }, child: 0 },
          { cond: { always: true }, child: 1 },
        ],
      }),
      [
        mk("revise", "transition", "revise a ready spec", {}, "sel", 0),
        mk("create", "spawn_subagent", "create new", { agent_id: "spec" }, "sel", 1),
      ],
    );
    const branches = screen.getByTestId("selector-branches");
    const arms = Array.from(branches.querySelectorAll("li"));
    expect(arms).toHaveLength(2);
    // Arm 1: `IF <condition>` on top, the child it routes to below.
    expect(arms[0]!.textContent).toContain("if");
    expect(arms[0]!.textContent).toContain("spec_status = ready");
    expect(arms[0]!.textContent).toContain("revise a ready spec");
    // Arm 2: the `always` default reads "else" — never the raw condition — and carries no `if`.
    expect(arms[1]!.textContent).toContain("else");
    expect(arms[1]!.textContent).not.toContain("always");
    expect(arms[1]!.textContent).toContain("create new");
    // The child stays drillable from inside its arm.
    expect(arms[1]!.querySelector("button")).toBeTruthy();
  });

  // `/nspec`'s `route ready` routes TWO conditions to the SAME child. The arm list is per CHILD, so a
  // single arm has to name both: showing `ready_ok = yes` alone told the reader `go` fell to the
  // `ELSE` and left the spec a draft, the exact opposite of the routing.
  //
  // THE FOURTH BRANCH IS HERE TO MAKE THE SECOND HALF SEPARATE THE TWO STATES (finding
  // `01M0AE3PKCFT93J7ARESHS1FSK`). With `/nspec`'s literal three branches, child 1 was reached by the
  // lone `{always: true}`, so `describeBranchCondition(config, 1)` answered `always` under the broken
  // code AND under the fixed one — byte-identical arms, an assertion that could not fail, claiming a
  // coverage it did not have (the test went red by the `OR` line above, never by that one). An
  // explicit `ready_ok = no` arm BEFORE the catch-all — the shape an author writes when they want the
  // refusal named and everything else caught — is what makes the difference visible: reading only the
  // FIRST branch that targets child 1 prints `if ready_ok = no`, while COLLAPSING onto the default arm
  // prints `else`. Both halves now fail on their own defect.
  it("names EVERY condition of an arm several branches share, never just the first", () => {
    renderInspector(
      mk("sel", "selector", "route ready", {
        branches: [
          { cond: { var: "ready_ok", eq: "yes" }, child: 0 },
          { cond: { var: "ready_ok", eq: "go" }, child: 0 },
          { cond: { var: "ready_ok", eq: "no" }, child: 1 },
          { cond: { always: true }, child: 1 },
        ],
      }),
      [
        mk("ready", "transition", "transition ready", {}, "sel", 0),
        mk("draft", "note", "left as draft", {}, "sel", 1),
      ],
    );
    const arms = Array.from(screen.getByTestId("selector-branches").querySelectorAll("li"));
    // One arm per CHILD still (two children), the shared arm carrying BOTH of its conditions.
    expect(arms).toHaveLength(2);
    expect(arms[0]!.textContent).toContain("if");
    expect(arms[0]!.textContent).toContain("ready_ok = yes OR ready_ok = go");
    expect(arms[0]!.textContent).toContain("transition ready");
    // …and the arm a branch makes UNCONDITIONAL names NONE of the conditions its other branches carry:
    // child 1 is reached whatever `ready_ok` holds, so printing `ready_ok = no` on it would tell the
    // reader that `no` — and nothing else — lands here. Asserted BEFORE the `else` below on purpose:
    // an arm reads `else` exactly when it shows no condition, so the two are one state read twice, and
    // this is the half that carries the claim. Proven by mutation: drop the collapse onto the default
    // arm (accumulate `always` like any other condition) and THIS line is the one that goes red, with
    // the `OR` assertion above still green. Break the whole loop back to first-branch-wins and the
    // `OR` line goes first — the two halves fail on their own defect, which is what the finding asked
    // for and what the three-branch fixture could not do.
    expect(arms[1]!.textContent).not.toContain("ready_ok");
    expect(arms[1]!.textContent).toContain("else");
    expect(arms[1]!.textContent).toContain("left as draft");
  });

  it("does NOT annotate non-selector composite children with conditions (only selectors)", () => {
    renderInspector(
      mk("loop", "loop", "phases", { until: { var: "phase_status", eq: "complete" } }),
      [
        mk("coder", "spawn_subagent", "coder", { agent_id: "coder" }, "loop", 0),
        mk("gate", "gate_check", "checks", { command_ids: ["typecheck"] }, "loop", 1),
      ],
    );
    expect(screen.queryAllByTestId("child-branch-condition")).toHaveLength(0);
  });

  it("closes from the close control", () => {
    const onClose = vi.fn();
    renderInspector(mk("t", "transition", "→ review", { target_state: "done" }), [], { onClose });
    fireEvent.click(screen.getByRole("button", { name: "Close inspector" }));
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});

describe("NodeConfigInspector — cross-highlight hover (finding #35)", () => {
  const LOOP = mk("loop", "loop", "phases", { until: { var: "phase_status", eq: "complete" } });
  const CHILDREN = [
    mk("coder", "spawn_subagent", "coder", { agent_id: "coder" }, "loop", 0),
    mk("gate", "gate_check", "checks", { command_ids: ["typecheck"] }, "loop", 1),
  ];

  it("reports a hovered child up to the page (list → graph)", () => {
    const { onHoverNode } = renderInspector(LOOP, CHILDREN);
    const row = screen.getByText("coder").closest("button")!;
    fireEvent.mouseEnter(row);
    expect(onHoverNode).toHaveBeenCalledWith("coder");
    fireEvent.mouseLeave(row);
    expect(onHoverNode).toHaveBeenCalledWith(null);
  });

  it("marks the child row hovered when the page reports its node (graph → list)", () => {
    renderInspector(LOOP, CHILDREN, { hoveredNodeId: "gate" });
    const gateRow = screen.getByText("checks").closest("button")!;
    const coderRow = screen.getByText("coder").closest("button")!;
    expect(gateRow.getAttribute("data-hovered")).toBe("true");
    expect(coderRow.getAttribute("data-hovered")).toBeNull();
  });
});
