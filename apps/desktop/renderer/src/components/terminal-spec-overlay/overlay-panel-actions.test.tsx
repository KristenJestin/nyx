import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { mockIPC } from "@/bridge/test-harness";

import type { SpecStatus } from "../specs/types";
import { OverlayPanelActions } from "./overlay-panel-actions";
import { spec } from "./test-fixtures";

// `useNavigate` is stubbed to a shared spy (via `vi.hoisted`, so the factory can reference it) —
// clicking an action must call THIS with the right typed target, and never touch a real router.
const { navigate } = vi.hoisted(() => ({ navigate: vi.fn() }));
vi.mock("@tanstack/react-router", () => ({
  useNavigate: () => navigate,
}));

/** The ORCHESTRATION RUN the header's Run action opens, served by the mocked bridge. */
const ENGINE_WORKFLOW = {
  id: "wf1",
  project_id: null,
  name: "ndev",
  scope: "ndev",
  root_node_id: "eroot",
  forked_from: null,
  created_at: 0,
  updated_at: 0,
};
const ENGINE_NODES = [
  {
    id: "eroot",
    workflow_id: "wf1",
    parent_id: null,
    order_index: 0,
    type_: "sequence",
    label: "/ndev",
    config: "{}",
    created_at: 0,
    updated_at: 0,
  },
  {
    id: "ecoder",
    workflow_id: "wf1",
    parent_id: "eroot",
    order_index: 0,
    type_: "spawn_subagent",
    label: "coder",
    config: JSON.stringify({ agent_id: "coder" }),
    created_at: 0,
    updated_at: 0,
  },
];
const ENGINE_RUN = {
  id: "run1",
  workflow_id: "wf1",
  spec_id: "s1",
  parent_run_id: null,
  current_node_id: "ecoder",
  blackboard: JSON.stringify({ phase: "2/3" }),
  status: "running",
  started_at: 0,
  updated_at: 0,
};

function installEngineBridge(runs: unknown[]) {
  mockIPC((cmd) => {
    switch (cmd) {
      case "engine_spec_runs":
        return runs;
      case "engine_workflow_graph":
        return { workflow: ENGINE_WORKFLOW, nodes: ENGINE_NODES };
      case "engine_effector_context":
        return { context: "=== NYX CONTEXT — CODER ===" };
      default:
        return null;
    }
  });
}

describe("<OverlayPanelActions> — status-aware header quick actions", () => {
  beforeEach(() => {
    navigate.mockClear();
    installEngineBridge([]);
  });

  it("ALWAYS renders 'Open the Spec' and navigates to /work/$specId (any status)", () => {
    const statuses: SpecStatus[] = [
      "draft",
      "ready",
      "in_progress",
      "blocked",
      "in_review",
      "completed",
    ];
    for (const status of statuses) {
      const { unmount } = render(<OverlayPanelActions spec={spec({ id: "s9", status })} />);
      const btn = screen.getByRole("button", { name: /open the spec/i });
      expect(btn).toBeInTheDocument();
      fireEvent.click(btn);
      expect(navigate).toHaveBeenLastCalledWith({
        to: "/work/$specId",
        params: { specId: "s9" },
      });
      unmount();
    }
  });

  it("shows 'Open the review' ONLY when in_review, and navigates to /work/$specId/review", () => {
    render(<OverlayPanelActions spec={spec({ id: "s7", status: "in_review" })} />);
    const btn = screen.getByRole("button", { name: /open the review/i });
    expect(btn).toBeInTheDocument();
    fireEvent.click(btn);
    expect(navigate).toHaveBeenCalledWith({
      to: "/work/$specId/review",
      params: { specId: "s7" },
    });
  });

  it("HIDES 'Open the review' for any non-in_review status (but keeps 'Open the Spec')", () => {
    const nonReview: SpecStatus[] = ["draft", "ready", "in_progress", "blocked", "completed"];
    for (const status of nonReview) {
      const { unmount } = render(<OverlayPanelActions spec={spec({ status })} />);
      expect(screen.queryByRole("button", { name: /open the review/i })).toBeNull();
      expect(screen.getByRole("button", { name: /open the spec/i })).toBeInTheDocument();
      unmount();
    }
  });

  // The runtime lives with the Spec being worked, so the overlay header is one of the two entry
  // points onto the run modal. It does NOT navigate — leaving the terminal would defeat the overlay.
  it("ALWAYS renders the Run action and opens the orchestration-run modal in place", async () => {
    installEngineBridge([ENGINE_RUN]);
    const { baseElement } = render(<OverlayPanelActions spec={spec({ id: "s1" })} />);

    const btn = screen.getByRole("button", { name: /orchestration run/i });
    fireEvent.click(btn);

    expect(await screen.findByText("Orchestration run")).toBeTruthy();
    await waitFor(() =>
      expect(baseElement.querySelector('[data-node-type="spawn_subagent"]')).toBeTruthy(),
    );
    expect(screen.getByText("Blackboard")).toBeTruthy();
    expect(screen.getByText("2/3")).toBeTruthy();
    // It opens in place — the overlay never leaves the Terminals route for this.
    expect(navigate).not.toHaveBeenCalled();
  });

  it("shows the run modal's empty state from the overlay when the Spec has no run", async () => {
    render(<OverlayPanelActions spec={spec({ id: "s5" })} />); // beforeEach serves no run
    fireEvent.click(screen.getByRole("button", { name: /orchestration run/i }));
    expect(await screen.findByText("No run for this Spec yet")).toBeTruthy();
  });

  it("does NOT bubble an action click to a parent handler (no panel close/toggle on click)", () => {
    // The action lives inside a header/languette that may carry a toggle/close handler; the click
    // must `stopPropagation` so it NEVER fires — the action only navigates.
    const onParentClick = vi.fn();
    render(
      <div onClick={onParentClick}>
        <OverlayPanelActions spec={spec({ status: "in_review" })} />
      </div>,
    );
    fireEvent.click(screen.getByRole("button", { name: /open the spec/i }));
    fireEvent.click(screen.getByRole("button", { name: /open the review/i }));
    expect(onParentClick).not.toHaveBeenCalled();
    expect(navigate).toHaveBeenCalledTimes(2);
  });
});
