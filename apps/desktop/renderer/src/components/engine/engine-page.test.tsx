import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { mockIPC } from "@/bridge/test-harness";
import { ActiveProjectProvider } from "@/components/terminals/active-project";

import { EnginePage } from "./engine-page";

/** The ndev workflow + its nodes + a live run + the coder effector context, served by the mocked
 *  bridge so the page exercises the full read path (list → graph → run → effector context). */
const WORKFLOW = {
  id: "wf1",
  project_id: null,
  name: "ndev — dev loop (default)",
  scope: "ndev",
  root_node_id: "root",
  forked_from: null,
  created_at: 0,
  updated_at: 0,
};

const NODES = [
  node("root", null, 0, "sequence", "/ndev"),
  node("loop", "root", 0, "loop", "phases"),
  node("coder", "loop", 0, "spawn_subagent", "coder", { agent_id: "coder" }),
  node("gate", "loop", 1, "gate_check", "checks", { command_ids: ["typecheck"] }),
  node("judge", "loop", 2, "judge", "phase review"),
  node("transition", "root", 1, "transition", "→ review", { target_state: "in_review" }),
];

const RUN = {
  id: "run1",
  workflow_id: "wf1",
  spec_id: "spec1",
  parent_run_id: null,
  current_node_id: "coder",
  blackboard: JSON.stringify({ phase: "1/3", gate_green: false }),
  status: "running",
  started_at: 0,
  updated_at: 0,
};

function node(
  id: string,
  parentId: string | null,
  orderIndex: number,
  type: string,
  label: string,
  config: Record<string, unknown> = {},
) {
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

function installBridge(workflows: unknown[] = [WORKFLOW], runs: unknown[] = [RUN]) {
  mockIPC((cmd, args) => {
    switch (cmd) {
      case "engine_workflow_list":
        return workflows;
      case "engine_workflow_graph": {
        const wid = (args as { workflowId: string }).workflowId;
        const wf = (workflows as (typeof WORKFLOW)[]).find((w) => w.id === wid) ?? WORKFLOW;
        return { workflow: wf, nodes: NODES };
      }
      case "engine_run_list":
        return runs;
      case "engine_effector_context":
        return {
          context: `=== NYX CONTEXT — ${String((args as { role: string }).role).toUpperCase()} ===\nYou implement the tasks. {{rules}}`,
        };
      default:
        return null;
    }
  });
}

function renderPage() {
  return render(
    <ActiveProjectProvider>
      <EnginePage />
    </ActiveProjectProvider>,
  );
}

/** Render the page (on the home cards) and open a workflow's graph by clicking its card. */
async function openWorkflow(name = WORKFLOW.name) {
  const utils = renderPage();
  const card = await screen.findByRole("button", { name: `Open ${name}` });
  fireEvent.click(card);
  return utils;
}

describe("EnginePage — home (finding #36)", () => {
  it("opens on the workflow cards, not a graph — no auto-select, no discreet picker", async () => {
    installBridge();
    const { container } = renderPage();
    // The home lists the workflow as a CARD…
    await screen.findByRole("button", { name: `Open ${WORKFLOW.name}` });
    // …and NO graph is opened until an explicit pick (no canvas, no old `<select>` picker).
    expect(container.querySelector('[data-testid="engine-canvas"]')).toBeNull();
    expect(screen.queryByLabelText("Select a workflow")).toBeNull();
    expect(screen.queryByRole("combobox")).toBeNull();
  });

  it("each card describes the STRUCTURE — node count, scope, built-in badge — and no run", async () => {
    installBridge();
    renderPage();
    const card = await screen.findByTestId("workflow-card");
    const scoped = within(card);
    // node count (the 6 seeded rows), the scope + a built-in badge.
    expect(await scoped.findByText("6 nodes")).toBeTruthy();
    expect(scoped.getByText("ndev")).toBeTruthy();
    expect(scoped.getByText("built-in")).toBeTruthy();
    expect(scoped.queryByText("project fork")).toBeNull();
    // /engine is STRUCTURAL end to end: a card never reports a run either — the bridge serves a live
    // one here and it must stay invisible. The runtime belongs to the Spec's run modal.
    expect(scoped.queryByText("running")).toBeNull();
    expect(scoped.queryByText(/no runs yet/i)).toBeNull();
  });

  it("badges a project fork as such (and never as built-in)", async () => {
    const FORK = {
      ...WORKFLOW,
      id: "wf2",
      project_id: "proj1",
      name: "ndev — dev loop (fork)",
      scope: "custom",
    };
    installBridge([FORK], []); // a project fork, no runs
    renderPage();
    const card = await screen.findByTestId("workflow-card");
    const scoped = within(card);
    expect(await scoped.findByText("project fork")).toBeTruthy();
    expect(scoped.queryByText("built-in")).toBeNull();
  });


  it("follows a sub_workflow node to the TARGET FLOW's structure (no run on /engine)", async () => {
    // `/engine` has no run, so a `sub_workflow` node cannot "descend into the child run" — what it can
    // do is open the flow it points at. The target is named by SCOPE, exactly as the engine resolves it.
    const CODER = {
      ...WORKFLOW,
      id: "wf2",
      name: "coder — per-phase build loop",
      scope: "coder",
      root_node_id: "croot",
    };
    const WITH_SUB = [
      ...NODES,
      node("sub", "root", 9, "sub_workflow", "coder", { scope: "coder" }),
    ];
    mockIPC((cmd, args) => {
      switch (cmd) {
        case "engine_workflow_list":
          return [WORKFLOW, CODER];
        case "engine_workflow_graph": {
          const wid = (args as { workflowId: string }).workflowId;
          return wid === "wf2"
            ? { workflow: CODER, nodes: [node("croot", null, 0, "sequence", "coder")] }
            : { workflow: WORKFLOW, nodes: WITH_SUB };
        }
        case "engine_effector_context":
          return { context: "" };
        default:
          return null;
      }
    });
    renderPage();
    fireEvent.click(await screen.findByRole("button", { name: `Open ${WORKFLOW.name}` }));

    // Select the sub_workflow node → the inspector offers a REAL button to follow it.
    const subNode = await waitFor(() => {
      const el = document.querySelector('[data-node-type="sub_workflow"]');
      if (!el) throw new Error("sub_workflow node not rendered");
      return el as HTMLElement;
    });
    fireEvent.click(subNode);
    const follow = await screen.findByTestId("follow-sub-workflow");
    fireEvent.click(follow);

    // The page now shows the CODER flow.
    expect(await screen.findByText(/coder — per-phase build loop/)).toBeTruthy();
  });

  it("shows an empty state when there is no workflow at all", async () => {
    installBridge([]);
    renderPage();
    expect(await screen.findByText("No workflow yet")).toBeTruthy();
    expect(screen.queryByTestId("workflow-card")).toBeNull();
  });
});

describe("EnginePage — select → graph (finding #36)", () => {
  it("opens the picked workflow's graph on an explicit card selection", async () => {
    installBridge();
    const { container } = await openWorkflow();
    await waitFor(() =>
      expect(container.querySelector('[data-node-type="spawn_subagent"]')).toBeTruthy(),
    );
    expect(container.querySelector('[data-node-type="gate_check"]')).toBeTruthy();
    expect(container.querySelector('[data-group-type="loop"]')).toBeTruthy();
    // The breadcrumb now carries the workflow name as the current crumb.
    expect(screen.getByText(WORKFLOW.name)).toBeTruthy();
  });

  it("walks back to the cards from the breadcrumb 'Engine' crumb", async () => {
    installBridge();
    const { container } = await openWorkflow();
    await waitFor(() => expect(container.querySelector('[data-testid="engine-canvas"]')).toBeTruthy());
    // The breadcrumb 'Engine' crumb is a back control to the home cards.
    fireEvent.click(screen.getByRole("button", { name: "Engine" }));
    expect(await screen.findByRole("button", { name: `Open ${WORKFLOW.name}` })).toBeTruthy();
    expect(container.querySelector('[data-testid="engine-canvas"]')).toBeNull();
  });

  // The Engine page is the STRUCTURE, not a runtime. It used to overlay the workflow's LATEST run
  // (cursor + blackboard), which surfaced a long-finished run as if something were executing; the
  // runtime moved to the Spec-side run modal. The bridge serves `RUN` here on purpose — the page must
  // ignore it even when the workflow HAS a run.
  it("shows the STRUCTURE only — no blackboard, no run cursor — even when the workflow has a run", async () => {
    installBridge();
    const { container } = await openWorkflow();
    await waitFor(() =>
      expect(container.querySelector('[data-node-type="spawn_subagent"]')).toBeTruthy(),
    );
    expect(screen.queryByText("Blackboard")).toBeNull();
    expect(screen.queryByText("phase")).toBeNull();
    expect(screen.queryByText("1/3")).toBeNull();
    expect(container.querySelector('[data-current="true"]')).toBeNull();
    // The right rail idles on the pick-a-node hint instead.
    expect(screen.getByTestId("graph-inspector-hint")).toBeTruthy();
  });
});

describe("EnginePage — inspector integration", () => {
  it("opens the sub-agent inspector when a spawn node is clicked", async () => {
    installBridge();
    const { container } = await openWorkflow();
    await waitFor(() =>
      expect(container.querySelector('[data-node-type="spawn_subagent"]')).toBeTruthy(),
    );
    fireEvent.click(container.querySelector('[data-node-type="spawn_subagent"]')!);
    expect(await screen.findByText("Sub-agent · coder")).toBeTruthy();
    expect(await screen.findByText(/You implement the tasks/)).toBeTruthy();
  });

  it("opens the gate config inspector with its command_ids", async () => {
    installBridge();
    const { container } = await openWorkflow();
    await waitFor(() =>
      expect(container.querySelector('[data-node-type="gate_check"]')).toBeTruthy(),
    );
    fireEvent.click(container.querySelector('[data-node-type="gate_check"]')!);
    // The inspector header leads with the node NAME (the type sits beside it as a badge). Scoped to the
    // heading — the same name is also on the graph node itself.
    expect(await screen.findByRole("heading", { name: "checks" })).toBeTruthy();
    // The inspector now EXPLAINS the gate in natural language (finding #35), not a field dump.
    const explanation = screen.getByTestId("node-explanation");
    expect(explanation.textContent).toMatch(/Runs/);
    expect(explanation.textContent).toMatch(/blocks the run until they pass/);
    expect(explanation.textContent).toContain("typecheck");
  });

  // finding #40 — a CUSTOM workflow whose spawn targets an unknown agent_id shows the data-derived
  // "custom" label and a graceful custom state; the unknown role is never fetched.
  it("shows a graceful custom state for an unknown-role spawn on a custom workflow", async () => {
    const CUSTOM_WF = {
      ...WORKFLOW,
      id: "wf9",
      project_id: "proj1",
      name: "[TEST nyx-v2] agent-authored probe",
      scope: "custom",
      root_node_id: "croot",
    };
    const CUSTOM_NODES = [
      node("croot", null, 0, "sequence", "probe"),
      node("dragon", "croot", 0, "spawn_subagent", "slay dragon", { agent_id: "dragon-slayer" }),
    ];
    let effectorCalls = 0;
    mockIPC((cmd) => {
      switch (cmd) {
        case "engine_workflow_list":
          return [CUSTOM_WF];
        case "engine_workflow_graph":
          return { workflow: CUSTOM_WF, nodes: CUSTOM_NODES };
        case "engine_run_list":
          return [];
        case "engine_effector_context":
          effectorCalls += 1;
          return { context: null };
        default:
          return null;
      }
    });
    const { container } = await openWorkflow(CUSTOM_WF.name);
    await waitFor(() =>
      expect(container.querySelector('[data-node-type="spawn_subagent"]')).toBeTruthy(),
    );
    fireEvent.click(container.querySelector('[data-node-type="spawn_subagent"]')!);
    expect(await screen.findByText("Sub-agent · dragon-slayer")).toBeTruthy();
    expect(screen.getByText("custom")).toBeTruthy();
    expect(screen.getByText(/No built-in role template/i)).toBeTruthy();
    expect(screen.queryByText("role template")).toBeNull();
    expect(screen.queryByText(/Could not render/i)).toBeNull();
    // The unknown role is never fetched — the client short-circuits on the known-role list (#40).
    expect(effectorCalls).toBe(0);
  });
});
