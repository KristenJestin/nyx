import { render, screen, waitFor } from "@testing-library/react";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  Outlet,
  RouterProvider,
} from "@tanstack/react-router";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { mockIPC } from "@/bridge/test-harness";
import { ActiveProjectProvider } from "@/components/terminals/active-project";

import type { SpecDetail } from "../types";
import { RuntimeTab } from "./runtime-tab";

/** A `spec_detail` fixture anchored on a workspace (so the Runtime tab has a runtime to show). */
function detailFixture(): SpecDetail {
  return {
    spec: {
      id: "s1",
      project_id: "p1",
      workspace_id: "w1",
      title: "Spec",
      slug: null,
      number: 1,
      priority: "med",
      status: "in_progress",
      mode: "stepped",
      phases: "[]",
      current_revision_number: 1,
      position: 0,
      created_at: 0,
      updated_at: 0,
      completed_at: null,
      archived_at: null,
    },
    revision: {
      id: "r1",
      spec_id: "s1",
      revision_number: 1,
      revision_status: "active",
      title: "Spec",
      problem: "",
      solution: "",
      implementation_decisions: "",
      testing_decisions: "",
      user_stories: "",
      out_of_scope: "",
      change_reason: null,
      created_at: 0,
    },
    tasks: [],
    dependencies: [],
    workspace_context: {
      spec_id: "s1",
      workspace: { id: "w1", project_id: "p1", name: "wt", path: "/tmp/wt" },
      project: { id: "p1", name: "Nyx", key: "NYX" },
      repos: [],
    },
  } as unknown as SpecDetail;
}

const SNAPSHOT = {
  workspace: { id: "w1", name: "wt", path: "/tmp/wt" },
  agent_sessions: [
    {
      id: "a1",
      terminal_id: "t1",
      workspace_id: "w1",
      agent_kind: "claude",
      external_session_id: "x",
      state: "active",
    },
  ],
  terminals: [
    {
      id: "t1",
      cwd: "/tmp/wt",
      label: "dev",
      workspace_id: "w1",
      status: "alive",
      exec_state: "idle",
      exec_state_unread: false,
      sessions: [],
    },
  ],
  services: [
    {
      id: "svc1",
      workspace_id: "w1",
      name: "dev server",
      command: "npm run dev",
      run_state: "running",
      exit_code: null,
      unread: false,
    },
  ],
};

/** Mount the RuntimeTab under a memory router (so `useNavigate` works) + the active-project
 *  provider, with a landing `/` route so a navigation there is observable. Returns the router. */
function renderRuntimeTab() {
  const rootRoute = createRootRoute({
    component: () => (
      <ActiveProjectProvider>
        <Outlet />
      </ActiveProjectProvider>
    ),
  });
  const detailRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/work/$specId",
    component: () => <RuntimeTab detail={detailFixture()} />,
  });
  const terminalsRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/",
    component: () => <div>terminals-page</div>,
    validateSearch: (s: Record<string, unknown>): { terminal?: string; command?: string } => {
      const out: { terminal?: string; command?: string } = {};
      if (typeof s.terminal === "string") out.terminal = s.terminal;
      if (typeof s.command === "string") out.command = s.command;
      return out;
    },
  });
  const router = createRouter({
    routeTree: rootRoute.addChildren([detailRoute, terminalsRoute]),
    history: createMemoryHistory({ initialEntries: ["/work/s1"] }),
  });
  render(<RouterProvider router={router} />);
  return router;
}

describe("RuntimeTab", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mockIPC((cmd) => {
      if (cmd === "workspace_runtime_snapshot") return SNAPSHOT;
      return undefined;
    });
  });

  it("recentres on terminals + managed commands and drops the workspace dump", async () => {
    renderRuntimeTab();
    // The two actionable sections are present.
    expect(await screen.findByText("Terminals")).toBeTruthy();
    expect(screen.getByText("Managed commands")).toBeTruthy();
    // The whole-workspace dump is gone: no workspace-path header, no agent-sessions section.
    expect(screen.queryByText("Agent sessions")).toBeNull();
    expect(screen.queryByText("/tmp/wt")).toBeNull();
  });

  it("navigates to the clicked terminal (`/?terminal=<id>`) and enters its project", async () => {
    const router = renderRuntimeTab();
    // Exact "dev" (the terminal label) — the managed-command row is also a role=button
    // ("dev server"), so match the terminal precisely.
    const row = await screen.findByRole("button", { name: "dev" });
    row.click();
    await waitFor(() => expect(screen.getByText("terminals-page")).toBeTruthy());
    expect(router.state.location.pathname).toBe("/");
    expect(router.state.location.search).toMatchObject({ terminal: "t1" });
  });

  it("exposes start/stop/relaunch controls on managed commands", async () => {
    renderRuntimeTab();
    // The reused <CommandControls> renders the three lifecycle buttons for the instance.
    expect(await screen.findByRole("button", { name: "Start command" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Stop command" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Relaunch command" })).toBeTruthy();
  });

  it("navigates to the clicked managed command's detail (`/?command=<id>`) and enters its project", async () => {
    const router = renderRuntimeTab();
    // The row is a `div role="button"` around the command name; click its body (not a control).
    const row = (await screen.findByText("dev server")).closest('[role="button"]');
    expect(row).not.toBeNull();
    (row as HTMLElement).click();
    await waitFor(() => expect(screen.getByText("terminals-page")).toBeTruthy());
    expect(router.state.location.pathname).toBe("/");
    expect(router.state.location.search).toMatchObject({ command: "svc1" });
  });

  it("does NOT navigate when a lifecycle control is clicked (stopPropagation on the controls)", async () => {
    const router = renderRuntimeTab();
    const startBtn = await screen.findByRole("button", { name: "Start command" });
    startBtn.click();
    // The control acted on the command but never triggered the row navigation — still on detail.
    expect(router.state.location.pathname).toBe("/work/s1");
    expect(screen.queryByText("terminals-page")).toBeNull();
  });
});
