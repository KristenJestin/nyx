import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  RouterProvider,
  useRouterState,
} from "@tanstack/react-router";
import { beforeEach, describe, expect, it } from "vitest";

import { emit, mockIPC } from "@/bridge/test-harness";
import { createAppRouter } from "@/router";
import { terminalStore } from "@/components/terminal/terminal-store";
import type { ProjectRecord, WorkspaceRecord } from "@/components/sidebar/use-projects";

import { HomePage } from "@/components/home/home-page";

import { ActiveProjectProvider } from "./active-project";
import { BaseLayout } from "./base-layout";
import { TerminalsPage } from "./terminals-page";
import { saveLastProjectId, saveLastTerminalId } from "./ui-persistence";

/**
 * ENTERING A PROJECT IS A NAVIGATION (PRD fix 2) — the SHELL + the real pages together, because
 * the landing behaviour is a collaboration: `<BaseLayout>`'s rail enters the project and routes to
 * its HOME, and `<TerminalsPage>` (the `/` route, unchanged) is what re-scopes to the entered
 * project and RESTORES its last terminal when one goes there. Mounting stubs would assert the
 * navigation and nothing about what the user actually lands on.
 *
 * The route tree mirrors the app's: the shell (rail + `<Outlet/>`) as the root, the real Terminals
 * page at `/`, the real home at `/home` (both scopes, same `?scope=` validation as the app), and a
 * project-SCOPED Spec page (`/work/$specId`) to switch AWAY from — the exact situation of the bug
 * (a page of project A still on screen after entering project B).
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
        // The home's ONE read, scoped: an ABSENT `projectId` is the global scope (every
        // project), a present one is that project's recap. Each project contributes a single
        // bare shell named after it, so "which project is this home recapping" is readable
        // from the rows themselves.
        case "home_snapshot": {
          const only = a.projectId as string | undefined;
          const ids = projects.map((p) => p.id).filter((id) => only === undefined || id === only);
          return {
            sessions: [],
            services: [],
            specs: [],
            terminals: ids.map((id) => ({
              terminal_id: `${id}-home-shell`,
              label: `${projects.find((p) => p.id === id)!.name} shell`,
              cwd: `/ws-${id}`,
              exec_state: "idle",
              exec_state_unread: false,
              workspace: { id: `ws-${id}`, name: "root", branch: null },
              project: {
                id,
                name: projects.find((p) => p.id === id)!.name,
                accent: null,
                icon: null,
                image_path: null,
              },
            })),
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

/** The real home, in the scope the url names — the app's `HomeRouteComponent`, verbatim. */
function ScopedHomePage() {
  const scope = useRouterState({ select: (s) => (s.location.search as { scope?: string }).scope });
  return <HomePage scope={scope === "all" ? "all" : "project"} />;
}

/** Mount the shell + the real Terminals and Home pages, starting on `initialEntry`. */
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
  // The REAL home, at the route the switch now lands on — with the app's own `?scope=` validation,
  // because "the `scope=all` parameter fell" is one of the things the switch has to prove.
  const homeRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/home",
    component: ScopedHomePage,
    validateSearch: (s: Record<string, unknown>): { scope?: "all" } =>
      s.scope === "all" ? { scope: "all" } : {},
  });
  const specRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/work/$specId",
    component: () => <div>SPEC PAGE</div>,
  });
  const router = createRouter({
    routeTree: rootRoute.addChildren([terminalsRoute, homeRoute, specRoute]),
    history: createMemoryHistory({ initialEntries: [initialEntry] }),
  });
  return { ...render(<RouterProvider router={router} />), router };
}

/** Go to the Terminals page through the rail — the way one leaves the landing home. */
async function goToTerminals(): Promise<void> {
  fireEvent.click(await screen.findByRole("link", { name: "Terminals" }));
}

/**
 * The pane of `terminalId`, once it exists.
 *
 * The MAIN surface is `<PaneTreeHost>` (the tiling), not the mono-visible `<TerminalDeck>` it
 * replaced, so "the terminal the user landed on" is spelled `data-focused="true"` — the ONE pane
 * that takes the keyboard — where the deck had a single `data-active`. `data-visible` says whether
 * a pane is in the tiling at all. That is the vocabulary `terminals-page.test.tsx` already reads;
 * these tests assert the SAME landing behaviour through it.
 */
function pane(container: HTMLElement, terminalId: string): Element | null {
  return container.querySelector(`[data-terminal-id="${terminalId}"]`);
}

describe("Project switch — entering a project lands on its own page", () => {
  beforeEach(() => {
    installBackend();
  });

  it("leaves a Spec page of the previous project for the entered project's HOME", async () => {
    const { router } = mountShell("/work/s1");
    await screen.findByText("SPEC PAGE");

    fireEvent.click(await screen.findByRole("button", { name: "Project Beta" }));

    await waitFor(() => expect(router.state.location.pathname).toBe("/home"));
    // Beta's home is the screen — its own shell is listed and nothing of Alpha's is.
    expect(await screen.findByText("Beta shell")).toBeTruthy();
    expect(screen.queryByText("Alpha shell")).toBeNull();
  });

  it("still restores the entered project's last terminal when one goes to Terminals", async () => {
    // Beta's last active terminal is its SECOND one. The switch lands on the home now, so the
    // restore happens when the user asks for the Terminals page — the `/` route's own behaviour,
    // untouched by the landing change.
    saveLastTerminalId("p2", "p2-term-b");
    const { container, router } = mountShell("/work/s1");
    await screen.findByText("SPEC PAGE");

    fireEvent.click(await screen.findByRole("button", { name: "Project Beta" }));
    await waitFor(() => expect(router.state.location.pathname).toBe("/home"));
    await goToTerminals();

    await waitFor(() => expect(router.state.location.pathname).toBe("/"));
    // Beta's page is on screen (its name heads the Terminals sidebar) with its remembered
    // terminal as the ACTIVE pane — not the first one, and not anything of Alpha's.
    expect(await screen.findByText("Beta")).toBeTruthy();
    await waitFor(() =>
      expect(pane(container, "p2-term-b")?.getAttribute("data-focused")).toBe("true"),
    );
    expect(pane(container, "p2-term-a")?.getAttribute("data-focused")).toBe("false");
    expect(pane(container, "p1-term")).toBeNull();
  });

  it("keeps the previous project's terminals ALIVE in the global store across the switch", async () => {
    const { container } = mountShell("/");
    // Alpha is entered: its terminal has a live xterm instance in the module-level store.
    await waitFor(() => expect(pane(container, "p1-term")).not.toBeNull());
    await waitFor(() => expect(terminalStore.state.ids).toContain("p1-term"));

    fireEvent.click(await screen.findByRole("button", { name: "Project Beta" }));
    await goToTerminals();

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
    await waitFor(() => expect(router.state.location.pathname).toBe("/home"));
    await goToTerminals();

    await waitFor(() => expect(router.state.location.pathname).toBe("/"));
    // Gamma has a workspace but no terminal → the page's own empty hint, nothing new.
    expect(await screen.findByText("No terminals to show.")).toBeTruthy();
    expect(pane(container, "p1-term")).toBeNull();
  });

  it("buries a STALE `?terminal=` rather than carrying it into the entered project", async () => {
    // The Runtime tab of a Spec opens a precise terminal by navigating to `/?terminal=<id>`, and
    // NOTHING clears that search afterwards. Entering ANOTHER project from there used to be a
    // no-op on the URL (already on `/`), so the page kept a deep-link target that belongs to the
    // project one just LEFT: it never resolves in the entered project, so the deep-link stayed
    // "outstanding" forever and SUPPRESSED the last-terminal restore — an EMPTY deck.
    //
    // Landing on the home disposes of it by leaving the route; and because a deep-linked `/` one
    // step back in history is the same trap, the navigation REPLACES it instead of stacking.
    saveLastTerminalId("p2", "p2-term-b");
    const { container, router } = mountShell("/?terminal=p1-term");
    await waitFor(() => expect(pane(container, "p1-term")).not.toBeNull());
    const before = router.history.length;

    fireEvent.click(await screen.findByRole("button", { name: "Project Beta" }));

    await waitFor(() => expect(router.state.location.pathname).toBe("/home"));
    expect(router.state.location.search).toEqual({});
    expect(router.history.length).toBe(before);
    // And the Terminals page, reached from the rail, restores Beta's remembered terminal: no
    // deep-link is outstanding to suppress it.
    await goToTerminals();
    await waitFor(() =>
      expect(pane(container, "p2-term-b")?.getAttribute("data-focused")).toBe("true"),
    );
    expect(pane(container, "p2-term-a")?.getAttribute("data-focused")).toBe("false");
    expect(router.state.location.search).toEqual({});
  });

  it("buries a STALE `?command=` the same way — the FIRST terminal is then selected", async () => {
    // The mirror case (`openCommand` → `/?command=<id>`): the command belongs to the previous
    // project (and no project here has services at all), so the command deep-link would suppress
    // the auto-select the same way. Beta has NO remembered terminal here (explicitly forgotten)
    // → the FIRST one must be selected.
    saveLastTerminalId("p2", null);
    const { container, router } = mountShell("/?command=p1-cmd");
    await waitFor(() => expect(pane(container, "p1-term")).not.toBeNull());

    fireEvent.click(await screen.findByRole("button", { name: "Project Beta" }));

    await waitFor(() => expect(router.state.location.pathname).toBe("/home"));
    expect(router.state.location.search).toEqual({});
    await goToTerminals();
    await waitFor(() =>
      expect(pane(container, "p2-term-a")?.getAttribute("data-focused")).toBe("true"),
    );
    expect(router.state.location.search).toEqual({});
  });

  it("does NOT stack history when the switch is made from a home", async () => {
    const { router } = mountShell("/home");
    const beta = await screen.findByRole("button", { name: "Project Beta" });
    await waitFor(() => expect(screen.getByText("Alpha shell")).toBeTruthy());
    const before = router.history.length;

    // Entering another project while already on the home only re-scopes the page…
    fireEvent.click(beta);
    await waitFor(() => expect(beta.getAttribute("aria-current")).toBe("true"));
    expect(router.state.location.pathname).toBe("/home");
    expect(router.history.length).toBe(before);

    // …and re-entering the project one is ALREADY in adds nothing either.
    fireEvent.click(beta);
    await waitFor(() => expect(router.state.location.pathname).toBe("/home"));
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
    await waitFor(() => expect(router.state.location.pathname).toBe("/home"));
  });
});

/**
 * SWITCHING PROJECT FROM A HOME KEEPS THE HOME (user feedback: "quand je change de projet ca
 * quitte la home du coup c'est trop bizarre").
 *
 * A home is not a page of the project one leaves — it is the same screen for every project, and it
 * answers a switch by re-reading in the new scope. So the switch keeps it and only changes WHAT it
 * recaps: from a home there is nothing to navigate at all, which is what the history assertions
 * above hold. The home IS the landing page from every other route too (the tests above), so what
 * this block adds is the two things only the real `/home` route can prove: that the page re-reads
 * in the new scope, and that `scope=all` falls.
 *
 * Mounted on the REAL router (`createAppRouter`) rather than the hand-built tree above, because
 * both halves of the claim live in the real `/home` route: the search validation that makes
 * `scope=all` fall, and the page that re-reads `home_snapshot` in the new scope.
 */
describe("Project switch — from a home, the switch STAYS on the home", () => {
  beforeEach(() => {
    installBackend();
    // The shell restores the LAST entered project on load; pin it so Alpha is the project one
    // switches AWAY from, whatever a previous test in this file left behind.
    saveLastProjectId("p1");
  });

  /** Mount the WHOLE app (real routes, real `<HomePage>`) starting on `entry`. */
  function mountApp(entry: string) {
    const router = createAppRouter([entry]);
    return { ...render(<RouterProvider router={router} />), router };
  }

  /** The home header's `<h1>` — the one thing that names the project the page recaps. */
  function heading(): HTMLElement {
    return screen.getByRole("heading", { level: 1 });
  }

  it("from the PROJECT home: stays on `/home`, re-scoped to the project just entered", async () => {
    const { router } = mountApp("/home");
    // Alpha's home is the screen: its own shell is listed and its name heads the page.
    expect(await screen.findByText("Alpha shell")).toBeTruthy();
    expect(heading().textContent).toBe("Alpha");

    fireEvent.click(await screen.findByRole("button", { name: "Project Beta" }));

    // The home did NOT give way to `/` — and it now recaps Beta, header included.
    await waitFor(() => expect(screen.getByText("Beta shell")).toBeTruthy());
    expect(router.state.location.pathname).toBe("/home");
    expect(heading().textContent).toBe("Beta");
    // Nothing of the project one left survives on the page.
    expect(screen.queryByText("Alpha shell")).toBeNull();
  });

  it("from the GLOBAL home: lands on the project home — `scope=all` falls", async () => {
    const { router } = mountApp("/home?scope=all");
    // The cross-project view: every project's shells at once, under the global heading.
    expect(await screen.findByText("Alpha shell")).toBeTruthy();
    expect(screen.getByText("Beta shell")).toBeTruthy();
    expect(heading().textContent).toBe("All projects");

    fireEvent.click(screen.getByRole("button", { name: "Project Beta" }));

    // Entering a project from the transverse view is LEAVING it: same route, project scope.
    await waitFor(() =>
      expect(screen.getByTestId("home-page").getAttribute("data-scope")).toBe("project"),
    );
    expect(router.state.location.pathname).toBe("/home");
    expect(router.state.location.search).toEqual({});
    await waitFor(() => expect(screen.queryByText("Alpha shell")).toBeNull());
    expect(screen.getByText("Beta shell")).toBeTruthy();
    expect(heading().textContent).toBe("Beta");
  });

  it("does NOT stack history when the switch keeps the home", async () => {
    const { router } = mountApp("/home");
    const beta = await screen.findByRole("button", { name: "Project Beta" });
    await waitFor(() => expect(heading().textContent).toBe("Alpha"));
    const before = router.history.length;

    fireEvent.click(beta);

    await waitFor(() => expect(heading().textContent).toBe("Beta"));
    expect(router.history.length).toBe(before);
  });
});
