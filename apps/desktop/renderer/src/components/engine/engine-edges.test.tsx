import { render } from "@testing-library/react";
import { Position, ReactFlowProvider, type EdgeProps } from "@xyflow/react";
import { describe, expect, it } from "vitest";

import { describeBranchCondition, NEVER_MATCHES_LABEL } from "./build-graph";
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

  // SEVERAL branches may target the SAME child — nyx-core allows it, and `/nspec`'s `route ready`
  // shipped it (`ready_ok = yes` and `ready_ok = go` both selecting child 0) until revision 22 split
  // `go` off. Reading only the FIRST claimed the other values fell through to the `ELSE` arm, the
  // exact opposite of the routing.
  it("ORs together EVERY condition that routes to the same child, in branch order", () => {
    const shared = JSON.stringify({
      branches: [
        { cond: { var: "ready_ok", eq: "yes" }, child: 0 },
        { cond: { var: "ready_ok", eq: "go" }, child: 0 },
        { cond: { always: true }, child: 1 },
      ],
    });
    expect(describeBranchCondition(shared, 0)).toBe("ready_ok = yes OR ready_ok = go");
    expect(describeBranchCondition(shared, 1)).toBe("always");
    // NO REGRESSION: one branch per child (every other seeded selector) still reads verbatim, no `OR`.
    expect(describeBranchCondition(config, 0)).toBe("spec_status = ready");
    expect(describeBranchCondition(config, 1)).toBe("spec_status ≠ draft");
    expect(describeBranchCondition(config, 2)).toBe("always");
  });

  it("collapses a child's conditions to the DEFAULT arm when one of its branches always matches", () => {
    const withAlways = JSON.stringify({
      branches: [
        { cond: { var: "x", eq: "1" }, child: 0 },
        { cond: { always: true }, child: 0 },
      ],
    });
    expect(describeBranchCondition(withAlways, 0)).toBe("always");
    // A branch with NO `cond` is nyx-core's default arm too — same collapse, same `null` as today.
    const condless = JSON.stringify({
      branches: [{ cond: { var: "x", eq: "1" }, child: 0 }, { child: 0 }],
    });
    expect(describeBranchCondition(condless, 0)).toBeNull();
    // The same test spelled twice adds nothing to read.
    const duplicated = JSON.stringify({
      branches: [
        { cond: { var: "x", eq: "1" }, child: 0 },
        { cond: { var: "x", eq: "1" }, child: 0 },
      ],
    });
    expect(describeBranchCondition(duplicated, 0)).toBe("x = 1");
  });

  // FINDING `01M0AE30KM8MC6PD7DWRSM06PW` — the collapse used to fire on `describeCondition`'s `null`,
  // which nyx-core hands back for TWO shapes it holds strictly apart: a branch with NO `cond`
  // (`cond: None` ⇒ `pick_branch` matches it ALWAYS — the default arm, covered just above) and a
  // `cond` that is PRESENT but names neither a `var` nor `always: true` (`Condition::matches` starts
  // `let Some(var) = self.var else { return false }` ⇒ matches NEVER). Merging them painted an arm
  // that can never fire as the one that always fires.
  it("never reads a cond that tests NOTHING as the default arm, and never lets it erase a sibling", () => {
    // THE REVERSAL, verbatim from the finding: child 0 is reached iff `x == 1`. Short-circuiting on
    // the second branch's `null` threw the first away and answered "ELSE" — unconditional.
    const varless = JSON.stringify({
      branches: [
        { cond: { var: "x", eq: 1 }, child: 0 },
        { cond: {}, child: 0 },
      ],
    });
    expect(describeBranchCondition(varless, 0)).toBe("x = 1");

    // ALONE, such a branch is not the default either — it is NAMED, because a blank is exactly what
    // made it readable as `ELSE`. Every spelling nyx-core deserializes into a var-less Condition.
    for (const cond of [{}, { eq: "yes" }, { always: false }]) {
      const only = JSON.stringify({ branches: [{ cond, child: 0 }] });
      expect(describeBranchCondition(only, 0)).toBe(NEVER_MATCHES_LABEL);
    }

    // ANNEXE of the same finding (pre-existing): `always` as a blackboard VAR is a real presence
    // test, not the default arm. Its RENDERING still collides with `always: true` — untouched here —
    // but the COLLAPSE no longer does, because it reads the branch's shape: a var named `always` no
    // longer swallows the conditions its siblings contribute for the same child.
    const bothArms = JSON.stringify({
      branches: [
        { cond: { var: "always" }, child: 0 },
        { cond: { var: "y", eq: "z" }, child: 0 },
      ],
    });
    expect(describeBranchCondition(bothArms, 0)).toBe("always OR y = z");
  });
});
