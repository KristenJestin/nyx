import { render } from "@testing-library/react";
import { mockIPC, emit } from "@/bridge/test-harness";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

import type { EngineNode, EngineWorkflowGraph, EngineWorkflowRun } from "@/bridge";

import * as buildGraph from "./build-graph";
import { EngineWorkflowView } from "./engine-workflow-view";

/**
 * JSDOM test of the REAL `EngineWorkflowView` data flow — finding #37 redux. The browser harness (even
 * a big nested graph) stays stable, so the flicker is NOT in EngineGraph/overlay; it is in the LIVE
 * flow: `useEngineGraph` re-pulls on `specs://changed`/`workflows://changed` (returning a fresh but
 * identical object), and if the async ELK layout is keyed on the graph OBJECT it re-lays-out on every
 * such churn (the flicker / two-layouts). This pins that a re-pull of an UNCHANGED graph does NOT
 * re-run `buildFlowGraph` — the N37a fix (layout keyed on a stable structural signature).
 */

function mk(id: string, p: string | null, o: number, t: string, l: string, c: Record<string, unknown> = {}): EngineNode {
  return { id, workflow_id: "wf", parent_id: p, order_index: o, type_: t, label: l, config: JSON.stringify(c), created_at: 0, updated_at: 0 };
}

function nodes(): EngineNode[] {
  return [
    mk("root", null, 0, "sequence", "/nspec"),
    mk("status", "root", 0, "query", "spec status"),
    mk("entry", "root", 1, "selector", "entry gate", { branches: [{ cond: { always: true }, child: 0 }] }),
    mk("createnew", "entry", 0, "note", "create new"),
    mk("finalq", "root", 2, "query", "final status"),
  ];
}

const WF = { id: "wf", project_id: null, name: "nspec", scope: "nspec" as const, root_node_id: "root", forked_from: null, created_at: 0, updated_at: 0 };
const RUN: EngineWorkflowRun = { id: "run1", workflow_id: "wf", spec_id: "spec1", parent_run_id: null, current_node_id: null, blackboard: "{}", status: "done", started_at: 0, updated_at: 0, pending_consent: null };

// The bridge deserializes a FRESH object on every invoke — mirror that so re-pulls churn identity while
// the STRUCTURE stays identical (the exact condition the fix must tolerate).
function freshGraph(): EngineWorkflowGraph {
  return { workflow: { ...WF }, nodes: nodes() };
}

let buildSpy: ReturnType<typeof vi.spyOn>;
let runListCalls = 0;

beforeEach(() => {
  runListCalls = 0;
  buildSpy = vi.spyOn(buildGraph, "buildFlowGraph");
  mockIPC((cmd) => {
    if (cmd === "engine_workflow_graph") return freshGraph();
    if (cmd === "engine_run_list") {
      runListCalls += 1;
      return [RUN];
    }
    if (cmd === "engine_effector_context") return { context: null };
    return null;
  }, { shouldMockEvents: true });
});
afterEach(() => {
  buildSpy.mockRestore();
  document.body.replaceChildren();
});

async function waitFor(pred: () => boolean, timeoutMs = 5000): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (pred()) return true;
    await new Promise((r) => setTimeout(r, 20));
  }
  return pred();
}

it("a live re-pull of an UNCHANGED graph does not re-run the ELK layout (N37a)", async () => {
  render(<EngineWorkflowView workflowId="wf" />);

  // Initial load → exactly one layout pass.
  expect(await waitFor(() => buildSpy.mock.calls.length >= 1)).toBe(true);
  await new Promise((r) => setTimeout(r, 100));
  const afterInitial = buildSpy.mock.calls.length;
  expect(afterInitial, "one layout on initial load").toBe(1);

  // Simulate the LIVE invalidations that fire on every nyx_next tick / authoring change — each makes
  // useEngineGraph re-pull a FRESH (identical) graph object.
  for (let i = 0; i < 5; i += 1) {
    await emit("specs://changed");
    await emit("workflows://changed");
    await new Promise((r) => setTimeout(r, 40));
  }
  await new Promise((r) => setTimeout(r, 150));

  // The structure never changed, so the layout must NOT have re-run — no churn-driven relayout.
  expect(
    buildSpy.mock.calls.length,
    `buildFlowGraph re-ran on identical re-pulls (churn relayout = the flicker): ${buildSpy.mock.calls.length} calls`,
  ).toBe(afterInitial);
});

/**
 * The Engine path is STRUCTURAL: the wrapper uses the shared viewer with NO run, so it never even
 * reads the workflow's runs — the bridge serves one here and it must stay untouched. (The runtime
 * moved to the Spec-side run modal, which passes the run in as a prop.)
 */
it("never reads the workflow's runs — the Engine page shows no runtime at all", async () => {
  render(<EngineWorkflowView workflowId="wf" />);
  expect(await waitFor(() => buildSpy.mock.calls.length >= 1)).toBe(true);
  await emit("specs://changed");
  await new Promise((r) => setTimeout(r, 100));
  expect(runListCalls, "engine_run_list must not be invoked from the Engine page").toBe(0);
  expect(document.querySelector('[data-current="true"]')).toBeNull();
});
