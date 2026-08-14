import { render } from "@testing-library/react";
import { page } from "vitest/browser";
import { afterEach, expect, it } from "vitest";

// The REAL Tailwind utilities (the config compiles them for a spec that imports globals.css), so this
// renders with the app's true typography/spacing — the only way a weight/layout regression is visible.
import "@/globals.css";
import type { EngineNode } from "@/bridge";

import { NodeConfigInspector } from "./node-config-inspector";

/**
 * VISUAL baseline of the node inspector (real Chromium). The engine's UI findings kept shipping unseen
 * because unit tests assert text, never appearance: this spec PAINTS the inspector so a human (or a
 * screenshot diff) can check what it actually looks like — the selector's IF-chain and the bold
 * dynamic tokens in the explanation.
 */

function mk(
  id: string,
  type: string,
  label: string,
  config: Record<string, unknown> = {},
  parentId: string | null = null,
  order = 0,
): EngineNode {
  return {
    id,
    workflow_id: "wf",
    parent_id: parentId,
    order_index: order,
    type_: type,
    label,
    config: JSON.stringify(config),
    created_at: 0,
    updated_at: 0,
  };
}

const SELECTOR = mk("sel", "selector", "entry gate", {
  branches: [
    { cond: { var: "spec_status", eq: "ready" }, child: 0 },
    { cond: { var: "spec_status", eq: "draft" }, child: 1 },
    { cond: { always: true }, child: 2 },
  ],
});
const SELECTOR_CHILDREN = [
  mk("revise", "sequence", "revise a ready spec", {}, "sel", 0),
  mk("resume", "sequence", "resume a draft", {}, "sel", 1),
  mk("create", "note", "create new", {}, "sel", 2),
];

const ASK = mk("ask", "ask_human", "continue or finalize", {
  prompt: "This draft is ready to finalize. Keep grilling to refine it, or finalize it now?",
  output_key: "entry_choice",
  options: ["grill", "ready"],
});

/** Mount a panel-sized host (the inspector lives in a ~340px side panel). */
function mount(node: EngineNode, children: readonly EngineNode[], testId: string): HTMLElement {
  const host = document.createElement("div");
  host.setAttribute("data-testid", testId);
  host.style.cssText =
    "position:fixed;top:0;left:0;width:340px;height:420px;overflow:hidden;background:var(--card,#0b0b0d)";
  host.className = "dark";
  document.body.appendChild(host);
  render(
    <NodeConfigInspector
      node={node}
      childNodes={children}
      onSelectNode={() => {}}
      onClose={() => {}}
    />,
    { container: host },
  );
  return host;
}

afterEach(() => {
  document.body.replaceChildren();
});

it("paints the SELECTOR inspector as an if-chain (condition above, child below)", async () => {
  mount(SELECTOR, SELECTOR_CHILDREN, "sel-host");
  await document.fonts.ready;
  await new Promise((r) => setTimeout(r, 120));
  await expect(page.getByTestId("sel-host")).toMatchScreenshot("selector-inspector");
});

it("paints the ASK_HUMAN inspector (checks the dynamic tokens' weight in the sentence)", async () => {
  mount(ASK, [], "ask-host");
  await document.fonts.ready;
  await new Promise((r) => setTimeout(r, 120));
  await expect(page.getByTestId("ask-host")).toMatchScreenshot("ask-human-inspector");
});

// --- the GRAPH itself ------------------------------------------------------------------------------
// The N38 review: branch-condition labels detached from their edges and piled up in a corner. This
// paints a real nested graph so the label placement (each label ON its own edge) is actually visible.

import { applyEngineOverlay, buildFlowGraph } from "./build-graph";
import { EngineGraph } from "./engine-graph";

function nspecShaped(): EngineNode[] {
  // The REAL nspec shape: nested sequences under a selector, a nested selector, and the grill LOOP —
  // i.e. every containment case the review flagged (boxes + edges routed across them).
  return [
    mk("root", "sequence", "/nspec"),
    mk("status", "query", "spec status", { query: "spec_status", write: "spec_status" }, "root", 0),
    mk(
      "entry",
      "selector",
      "entry gate",
      {
        branches: [
          { cond: { var: "spec_status", eq: "ready" }, child: 0 },
          { cond: { var: "spec_status", eq: "draft" }, child: 1 },
          { cond: { always: true }, child: 2 },
        ],
      },
      "root",
      1,
    ),
    mk("revise", "sequence", "revise a ready spec", {}, "entry", 0),
    mk("confirm", "ask_human", "confirm revision", { options: ["yes", "no"] }, "revise", 0),
    mk("fork", "fork_spec", "fork", {}, "revise", 1),
    mk("resume", "sequence", "resume a draft", {}, "entry", 1),
    mk("canready", "query", "can finalize?", { query: "spec_can_transition" }, "resume", 0),
    mk(
      "fog",
      "selector",
      "finalize or grill",
      {
        branches: [
          { cond: { var: "can_ready", eq: true }, child: 0 },
          { cond: { always: true }, child: 1 },
        ],
      },
      "resume",
      1,
    ),
    mk("cont", "ask_human", "continue or finalize", { options: ["grill", "ready"] }, "fog", 0),
    mk("incomplete", "note", "draft incomplete", {}, "fog", 1),
    mk("createnew", "note", "create new", {}, "entry", 2),
    // The reported case: a selector routing EITHER into the grill LOOP or to a note sitting BESIDE it —
    // the branch heading for the note used to cut straight across the loop's box.
    mk(
      "grillsel",
      "selector",
      "grill?",
      {
        branches: [
          { cond: { var: "entry_choice", eq: "ready" }, child: 0 },
          { cond: { always: true }, child: 1 },
        ],
      },
      "root",
      2,
    ),
    mk("grillskipped", "note", "grill skipped", {}, "grillsel", 0),
    mk("grill", "loop", "grill", { until: { var: "grill_status", eq: "complete" } }, "grillsel", 1),
    mk("spawnspec", "spawn_subagent", "author spec", { agent_id: "spec" }, "grill", 0),
    mk("steer", "ask_human", "steer", {}, "grill", 1),
    mk("judge", "judge", "spec complete?", {}, "grill", 2),
    mk("finalq", "query", "final status", { query: "spec_status", write: "final_status" }, "root", 3),
  ];
}

it("paints the GRAPH — each branch label sits on ITS OWN edge (N38 revert)", async () => {
  // The default browser viewport is narrower than the canvas we want to see — widen it so the whole
  // graph is captured (an element screenshot is clipped to the viewport).
  await page.viewport(1400, 1100);
  const graph = await buildFlowGraph(nspecShaped(), "root");
  const host = document.createElement("div");
  host.setAttribute("data-testid", "graph-host");
  host.className = "dark";
  host.style.cssText =
    "position:fixed;top:0;left:0;width:1360px;height:1040px;overflow:hidden;background:#0b0b0d";
  document.body.appendChild(host);
  render(
    <EngineGraph
      nodes={applyEngineOverlay(graph.nodes, null, null, null)}
      edges={graph.edges}
      onMarker={() => {}}
      onSelectNode={() => {}}
    />,
    { container: host },
  );
  await document.fonts.ready;
  await new Promise((r) => setTimeout(r, 500)); // let the initial fitView settle
  await expect(page.getByTestId("graph-host")).toMatchScreenshot("engine-graph");
});

// --- the RUN MODAL --------------------------------------------------------------------------------
// The runtime moved OFF /engine onto the Spec (its detail page + the dev overlay open this modal). It
// is a new surface, so it gets a visual baseline too: the graph with the run's cursor + the blackboard.

import { mockIPC } from "@/bridge/test-harness";
import { SpecRunModal } from "./run-modal";

// NOTE (harness): the bridge double (`mockIPC`) is only wired in the JSDOM project, so a browser spec
// cannot feed the modal a run — the POPULATED modal is covered by the jsdom tests in `run-modal.test.tsx`.
// What a real browser adds here is the SHELL and the EMPTY state (portal, sizing, theme, wording).
// The `dark` class lives on <html> in the app (index.html); the test page has none, so set it here or
// the portalled popup paints light and the baseline lies about the theme.
it("paints the SPEC RUN modal shell + its empty state", async () => {
  document.documentElement.classList.add("dark");
  await page.viewport(1400, 1000);
  const nodes = nspecShaped();
  mockIPC(
    (cmd) => {
      if (cmd === "engine_spec_runs")
        return [
          {
            id: "run1",
            workflow_id: "wf",
            spec_id: "spec1",
            parent_run_id: null,
            current_node_id: "steer",
            blackboard: JSON.stringify({
              spec_status: "draft",
              grill_status: "grilling",
              loop_iter: 2,
              answers: ["needs a clearer problem statement"],
            }),
            status: "running",
            started_at: 0,
            updated_at: 0,
          },
        ];
      if (cmd === "engine_workflow_graph")
        return {
          workflow: {
            id: "wf",
            project_id: null,
            name: "nspec",
            scope: "nspec",
            root_node_id: "root",
            forked_from: null,
            created_at: 0,
            updated_at: 0,
          },
          nodes,
        };
      if (cmd === "engine_effector_context") return { context: null };
      return null;
    },
    { shouldMockEvents: true },
  );

  const host = document.createElement("div");
  host.setAttribute("data-testid", "modal-host");
  host.className = "dark";
  host.style.cssText = "position:fixed;inset:0;background:#0b0b0d";
  document.body.appendChild(host);
  render(<SpecRunModal specId="spec1" open onClose={() => {}} />, { container: host });
  await document.fonts.ready;
  await new Promise((r) => setTimeout(r, 900)); // graph load + ELK + fitView
  await expect(page.getByTestId("modal-host")).toMatchScreenshot("run-modal-empty");
});
