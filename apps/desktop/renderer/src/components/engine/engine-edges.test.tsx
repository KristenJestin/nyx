import { render } from "@testing-library/react";
import { Position, ReactFlowProvider, type EdgeProps } from "@xyflow/react";
import { describe, expect, it } from "vitest";

import { describeBranchCondition } from "./build-graph";
import { EngineEdge } from "./engine-edges";

/**
 * The engine graph draws ONLY the neutral sequential FLOW edge. The old lateral `reboucle` arc that
 * bowed 200px out of the loop box (label clipped) is gone — the loop-back now lives on the loop's BOX
 * header (finding #39). These tests pin that the edge is a contained, neutral flow path with no
 * overflowing reboucle variant / label.
 */
function renderEdge(data: { variant: "flow" }) {
  const props = {
    id: "e",
    source: "a",
    target: "b",
    sourceX: 100,
    sourceY: 0,
    targetX: 100,
    targetY: 200,
    sourcePosition: Position.Bottom,
    targetPosition: Position.Top,
    data,
  } as unknown as EdgeProps;
  return render(
    <ReactFlowProvider>
      <svg>
        <EngineEdge {...props} />
      </svg>
    </ReactFlowProvider>,
  );
}

describe("EngineEdge", () => {
  it("renders the sequential flow as a neutral, contained edge (no overflowing reboucle arc — #39)", () => {
    const { container } = renderEdge({ variant: "flow" });
    const path = container.querySelector('path[data-variant="flow"]');
    expect(path).toBeTruthy();
    // Neutral border stroke (never the destructive reboucle red), and a real routed path.
    expect(path!.getAttribute("class")).toContain("!stroke-border");
    expect(path!.getAttribute("d")).toBeTruthy();
    // The old floating reboucle variant / its clipped label are gone.
    expect(container.querySelector('[data-variant="reboucle"]')).toBeNull();
    expect(container.querySelector("[data-edge-label]")).toBeNull();
  });
});

describe("describeBranchCondition (N18(b) — the label on a selector-branch edge)", () => {
  const config = JSON.stringify({
    branches: [
      { cond: { var: "spec_status", eq: "ready" }, child: 0 },
      { cond: { var: "spec_status", ne: "draft" }, child: 1 },
      { cond: { always: true }, child: 2 },
    ],
  });

  it("reads the condition of the branch that targets a given child index", () => {
    expect(describeBranchCondition(config, 0)).toBe("spec_status = ready");
    expect(describeBranchCondition(config, 1)).toBe("spec_status ≠ draft");
    expect(describeBranchCondition(config, 2)).toBe("always");
  });

  it("defaults a branch with no explicit `child` to its own position, and returns null for none/garbage", () => {
    const positional = JSON.stringify({ branches: [{ cond: { var: "ok", truthy: true } }] });
    expect(describeBranchCondition(positional, 0)).toBe("ok is set");
    expect(describeBranchCondition(config, 9)).toBeNull();
    expect(describeBranchCondition("not json", 0)).toBeNull();
  });
});
