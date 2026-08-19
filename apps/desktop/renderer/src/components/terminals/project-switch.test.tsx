import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";
import { beforeEach, describe, expect, it } from "vitest";

import { emit, mockIPC } from "@/bridge/test-harness";
import { terminalStore } from "@/components/terminal/terminal-store";
import type { ProjectRecord, WorkspaceRecord } from "@/components/sidebar/use-projects";

import { ActiveProjectProvider } from "./active-project";
import { BaseLayout } from "./base-layout";
import { TerminalsPage } from "./terminals-page";
import { saveLastTerminalId } from "./ui-persistence";

/**
 * ENTERING A PROJECT IS A NAVIGATION (PRD fix 2) — the SHELL + the real Terminals page together,
 * because the landing behaviour is a collaboration: `<BaseLayout>`'s rail enters the project and
 * routes to `/`, and `<TerminalsPage>` (the `/` route, unchanged) is what re-scopes to the newly
 * entered project and RESTORES its last terminal. Mounting a stub at `/` would assert the
 * navigation and nothing about what the user actually lands on.
 *
 * The route tree mirrors the app's: the shell (rail + `<Outlet/>`) as the root, the real Terminals
 * page at `/`, and a project-SCOPED Spec page (`/work/$specId`) to switch AWAY from — the exact
 * situation of the bug (a page of project A still on screen after entering project B).
 */

function project(id: string, name: string): ProjectRecord {
  return { id, name, collapsed: false, created_at: 0, updated_at: 0, resume_agent_sessions: false };
}

function workspace(id: string, projectId: string): WorkspaceRecord {
  return {
    id,
    project_id: projectId,
    name: "root",
    path: `/${id}`,
    branch: null,
    is_root: true,
    collapsed: false,
    created_at: 0,
    updated_at: 0,
  };
}

/** Which terminals each project's (single) workspace hosts — `p3` deliberately has NONE. */
const TERMINALS: Record<string, string[]> = {
  "ws-p1": ["p1-term"],
  "ws-p2": ["p2-term-a", "p2-term-b"],
  "ws-p3": [],
};

/**
 * The backend: three projects (Alpha / Beta / Gamma), one root workspace each, and a runtime
 * snapshot per workspace. `list_terminals` serves the GLOBAL record set (every project's
 * terminals, all alive) — the set the shell's `useReconcileTerminals` reconciles the live xterm
 * store against, so a switch must never make it drop the project one just left.
 */
function installBackend(projects: ProjectRecord[] = allProjects()) {
  mockIPC(
    (cmd, args) => {
      const a = (args ?? {}) as Record<string, unknown>;
      switch (cmd) {
        case "list_projects":
          return projects;
        case "list_workspaces": {
          const projectId = a.projectId as string;
          return [workspace(`ws-${projectId}`, projectId)];
        }
        case "workspace_runtime_snapshot": {
          const wsId = a.workspaceId as string;
          return {
            workspace: { id: wsId, name: "root", path: `/${wsId}` },
            terminals: (TERMINALS[wsId] ?? []).map((id) => ({
              id,
              cwd: `/${wsId}`,
              label: id,
              workspace_id: wsId,
              status: "alive",
              exec_state: "idle",
              exec_state_unread: false,
              sessions: [],
            })),
            agent_sessions: [],
            services: [],
          };
        }
        case "list_terminals":
          return Object.entries(TERMINALS).flatMap(([wsId, ids]) =>
            ids.map((id) => ({
              id,
              cwd: `/${wsId}`,
              label: id,
              status: "alive",
              order_index: 0,
              created_at: 0,
              updated_at: 0,
              closed_at: null,
              workspace_id: wsId,
            })),
          );
        case "pty_spawn":
          return 100;
        case "command_list":
        case "command_instance_list":
        case "command_import_scripts":
        case "agent_active_sessions":
        case "agent_activity_snapshot":
          return [];
        default:
          return undefined;
      }
    },
    { shouldMockEvents: true },
  );
}

function allProjects(): ProjectRecord[] {
  return [project("p1", "Alpha"), project("p2", "Beta"), project("p3", "Gamma")];
}

/** Mount the shell + the real Terminals page at `/`, starting on `initialEntry`. */
function mountShell(initialEntry: string, initialProjectId: string | null = "p1") {
  const rootRoute = createRootRoute({
    component: () => (
      <ActiveProjectProvider initialProjectId={initialProjectId}>
        <BaseLayout />
      </ActiveProjectProvider>
    ),
  });
  const terminalsRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/",
    component: TerminalsPage,
    validateSearch: (s: Record<string, unknown>): { terminal?: string; command?: string } => {
      const out: { terminal?: string; command?: string } = {};
      if (typeof s.terminal === "string") out.terminal = s.terminal;
      if (typeof s.command === "string") out.command = s.command;
      return out;
    },
  });
  const specRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/work/$specId",
    component: () => <div>SPEC PAGE</div>,
  });
  const router = createRouter({
    routeTree: rootRoute.addChildren([terminalsRoute, specRoute]),
    history: createMemoryHistory({ initialEntries: [initialEntry] }),
  });
  return { ...render(<RouterProvider router={router} />), router };
}

/** The deck pane of `terminalId`, once it exists. */
function pane(container: HTMLElement, terminalId: string): Element | null {
  return container.querySelector(`[data-terminal-id="${terminalId}"]`);
}

describe("Project switch — entering a project lands on its own page", () => {
  beforeEach(() => {
    installBackend();
  });

  it("leaves a Spec page of the previous project for `/` and restores the entered project's last terminal", async () => {
    // Beta's last active terminal is its SECOND one — entering Beta must land on `/` AND reopen
    // that terminal (the `/` route's existing restore, untouched by this fix).
    saveLastTerminalId("p2", "p2-term-b");
    const { container, router } = mountShell("/work/s1");
    await screen.findByText("SPEC PAGE");

    fireEvent.click(await screen.findByRole("button", { name: "Project Beta" }));

    await waitFor(() => expect(router.state.location.pathname).toBe("/"));
    // Beta's page is on screen (its name heads the Terminals sidebar) with its remembered
    // terminal as the ACTIVE pane — not the first one, and not anything of Alpha's.
    expect(await screen.findByText("Beta")).toBeTruthy();
    await waitFor(() =>
      expect(pane(container, "p2-term-b")?.getAttribute("data-active")).toBe("true"),
    );
    expect(pane(container, "p2-term-a")?.getAttribute("data-active")).toBe("false");
    expect(pane(container, "p1-term")).toBeNull();
  });

  it("keeps the previous project's terminals ALIVE in the global store across the switch", async () => {
    const { container } = mountShell("/");
    // Alpha is entered: its terminal has a live xterm instance in the module-level store.
    await waitFor(() => expect(pane(container, "p1-term")).not.toBeNull());
    await waitFor(() => expect(terminalStore.state.ids).toContain("p1-term"));

    fireEvent.click(await screen.findByRole("button", { name: "Project Beta" }));

    // Beta's runtime is on screen…
    await waitFor(() => expect(pane(container, "p2-term-a")).not.toBeNull());
    // …and Alpha's terminal is still LIVE (only hidden with its unmounted deck): a project switch
    // hides panes, it never disposes another project's PTYs (nor their agents).
    expect(terminalStore.state.ids).toContain("p1-term");
  });

  it("shows the EXISTING empty state when the entered project has no terminal", async () => {
    const { container, router } = mountShell("/work/s1");
    await screen.findByText("SPEC PAGE");

    fireEvent.click(await screen.findByRole("button", { name: "Project Gamma" }));

    await waitFor(() => expect(router.state.location.pathname).toBe("/"));
    // Gamma has a workspace but no terminal → the page's own empty hint, nothing new.
    expect(await screen.findByText("No terminals to show.")).toBeTruthy();
    expect(pane(container, "p1-term")).toBeNull();
  });

  it("restores the entered project's last terminal even when `/` still carries a STALE `?terminal=`", async () => {
    // The Runtime tab of a Spec opens a precise terminal by navigating to `/?terminal=<id>`, and
    // NOTHING clears that search afterwards. Entering ANOTHER project from there used to be a
    // no-op on the URL (already on `/`), so the page kept a deep-link target that belongs to the
    // project one just LEFT: it never resolves in the entered project, so the deep-link stays
    // "outstanding" forever and SUPPRESSES the last-terminal restore — landing on an EMPTY deck.
    saveLastTerminalId("p2", "p2-term-b");
    const { container, router } = mountShell("/?terminal=p1-term");
    await waitFor(() => expect(pane(container, "p1-term")).not.toBeNull());

    fireEvent.click(await screen.findByRole("button", { name: "Project Beta" }));

    await waitFor(() => expect(router.state.location.pathname).toBe("/"));
    // Beta's remembered terminal is ACTIVE — the deck is not empty…
    await waitFor(() =>
      expect(pane(container, "p2-term-b")?.getAttribute("data-active")).toBe("true"),
    );
    expect(pane(container, "p2-term-a")?.getAttribute("data-active")).toBe("false");
    // …and the stale deep-link of the project one left is gone from the URL.
    expect(router.state.location.search).toEqual({});
  });

  it("restores the entered project's FIRST terminal when `/` still carries a STALE `?command=`", async () => {
    // The mirror case (`openCommand` → `/?command=<id>`): the command belongs to the previous
    // project (and no project here has services at all), so the command deep-link would stay
    // outstanding forever and suppress the auto-select the same way. Beta has NO remembered
    // terminal here (explicitly forgotten) → the FIRST one must be selected.
    saveLastTerminalId("p2", null);
    const { container, router } = mountShell("/?command=p1-cmd");
    await waitFor(() => expect(pane(container, "p1-term")).not.toBeNull());

    fireEvent.click(await screen.findByRole("button", { name: "Project Beta" }));

    await waitFor(() => expect(router.state.location.pathname).toBe("/"));
    await waitFor(() =>
      expect(pane(container, "p2-term-a")?.getAttribute("data-active")).toBe("true"),
    );
    expect(router.state.location.search).toEqual({});
  });

  it("does NOT stack history when the route is already `/`", async () => {
    const { router } = mountShell("/");
    const beta = await screen.findByRole("button", { name: "Project Beta" });
    await waitFor(() => expect(screen.getByText("Alpha")).toBeTruthy());
    const before = router.history.length;

    // Entering another project while already on `/` only re-scopes the page…
    fireEvent.click(beta);
    await waitFor(() => expect(beta.getAttribute("aria-current")).toBe("true"));
    expect(router.state.location.pathname).toBe("/");
    expect(router.history.length).toBe(before);

    // …and re-entering the project one is ALREADY in adds nothing either.
    fireEvent.click(beta);
    await waitFor(() => expect(router.state.location.pathname).toBe("/"));
    expect(router.history.length).toBe(before);
  });

  it("recovers from a DELETED project without leaving its route behind", async () => {
    // Alpha is entered and the user is on one of ITS pages; Alpha is then deleted elsewhere.
    const projects = allProjects();
    installBackend(projects);
    const { router } = mountShell("/work/s1");
    await screen.findByText("SPEC PAGE");
    await screen.findByRole("button", { name: "Project Alpha" });

    projects.splice(0, 1);
    await emit("workspaces://changed");

    // The rail falls back to a surviving project AND the view leaves the dead project's page.
    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "Project Beta" }).getAttribute("aria-current"),
      ).toBe("true"),
    );
    await waitFor(() => expect(router.state.location.pathname).toBe("/"));
  });
});
