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

import { mockIPC } from "@/bridge/test-harness";
import { HomePage } from "@/components/home/home-page";
import { BoardPage } from "@/components/specs/board-page";
import type { ProjectRecord, WorkspaceRecord } from "@/components/sidebar/use-projects";

import { ActiveProjectProvider, useActiveProject } from "./active-project";
import { BaseLayout } from "./base-layout";
import { loadGlobalScope, saveGlobalScope } from "./ui-persistence";

/**
 * THE APP SCOPE — "all projects" as a STATE OF THE APPLICATION, not a parameter of one page.
 *
 * This is the FOUNDATION the five cross-project pages (Terminals, Work, Knowledge, Engine, Design)
 * will branch onto, so what is proven here is the CONTRACT and nothing about any page's content:
 *
 *   1. ONE source of truth — `useActiveProject().scope` — that any page can read without knowing
 *      the home exists, let alone parsing its url;
 *   2. it SURVIVES navigation between every page;
 *   3. it is PERSISTED and restored at relaunch, exactly as the entered project is;
 *   4. entering a project LEAVES it — the two are exclusive;
 *   5. and it never gets spelled as `activeProjectId === null`, which keeps its one meaning
 *      ("no project is loaded") and the three empty states that read it.
 *
 * The rail is mounted for real (`<BaseLayout>`), because the rail is the scope SELECTOR: the
 * `All projects` tile enters the mode and a project tile leaves it. The pages are a PROBE (which
 * reads the contract as a page would) plus the real `<BoardPage>`, whose `activeProjectId === null`
 * empty state is one of the three the mode must not trip.
 */

function project(id: string, name: string): ProjectRecord {
  return { id, name, collapsed: false, created_at: 0, updated_at: 0, resume_agent_sessions: false };
}

function workspace(projectId: string): WorkspaceRecord {
  return {
    id: `ws-${projectId}`,
    project_id: projectId,
    name: "root",
    path: `/${projectId}`,
    branch: null,
    is_root: true,
    collapsed: false,
    created_at: 0,
    updated_at: 0,
  };
}

function installBackend() {
  mockIPC(
    (cmd, args) => {
      const a = (args ?? {}) as Record<string, unknown>;
      switch (cmd) {
        case "list_projects":
          return [project("p1", "Alpha"), project("p2", "Beta")];
        case "list_workspaces":
          return [workspace((a.projectId as string) ?? "p1")];
        case "workspace_runtime_snapshot":
          return {
            workspace: { id: (a.workspaceId as string) ?? "ws-p1", name: "root", path: "/" },
            terminals: [],
            agent_sessions: [],
            services: [],
          };
        case "home_snapshot":
          return { sessions: [], services: [], specs: [], terminals: [] };
        case "spec_list":
          return [];
        default:
          return [];
      }
    },
    { shouldMockEvents: true },
  );
}

/**
 * A PAGE, reduced to what it reads of the scope. Exactly the shape the five real pages will take:
 * one `useActiveProject()` call, one branch on `scope.kind`, and NOTHING about the url.
 */
function ScopeProbe() {
  const { scope, globalScope, activeProjectId } = useActiveProject();
  return (
    <div
      data-testid="probe"
      data-kind={scope.kind}
      data-global={globalScope}
      // What the page would hand its read model: the project id in the project scope, and the
      // absence of any id in the global one. `null` here is "no project loaded", never "all".
      data-read={scope.kind === "all" ? "ALL" : (scope.projectId ?? "NONE")}
      data-entered={activeProjectId ?? "NONE"}
    />
  );
}

/** The real home in the scope its url names, with the probe beside it. */
function ScopedHomePage() {
  const scope = useRouterState({ select: (s) => (s.location.search as { scope?: string }).scope });
  return (
    <>
      <HomePage scope={scope === "all" ? "all" : "project"} />
      <ScopeProbe />
    </>
  );
}

/** Mount the real shell over the probe + the real Specs board, starting on `entry`. */
function mountApp(entry = "/") {
  const rootRoute = createRootRoute({
    component: () => (
      <ActiveProjectProvider>
        <BaseLayout />
      </ActiveProjectProvider>
    ),
  });
  const probe = (path: string) =>
    createRoute({ getParentRoute: () => rootRoute, path, component: ScopeProbe });
  const homeRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/home",
    component: ScopedHomePage,
    validateSearch: (s: Record<string, unknown>): { scope?: "all" } =>
      s.scope === "all" ? { scope: "all" } : {},
  });
  const workRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/work",
    component: () => (
      <>
        <BoardPage />
        <ScopeProbe />
      </>
    ),
  });
  const router = createRouter({
    routeTree: rootRoute.addChildren([
      probe("/"),
      homeRoute,
      workRoute,
      probe("/knowledge"),
      probe("/engine"),
      probe("/design"),
    ]),
    history: createMemoryHistory({ initialEntries: [entry] }),
  });
  return { ...render(<RouterProvider router={router} />), router };
}

/** The probe, once the shell has settled. */
async function probe(): Promise<HTMLElement> {
  return await screen.findByTestId("probe");
}

/** Wait until a project is entered — the rail resolves it one IPC hop after mount. */
async function entered(name: string): Promise<HTMLElement> {
  const tile = await screen.findByRole("button", { name: `Project ${name}` });
  await waitFor(() => expect(tile.getAttribute("aria-current")).toBe("true"));
  return tile;
}

/** The rail's `All projects` tile — the scope selector. */
function allProjectsTile(): HTMLElement {
  return screen.getByRole("link", { name: "All projects" });
}

/** Whether the rail draws the app as being in the global scope. */
function railSaysGlobal(): boolean {
  return allProjectsTile().closest("li")!.getAttribute("data-active") === "true";
}

describe("The app scope — one source of truth for the six pages", () => {
  beforeEach(() => {
    installBackend();
  });

  it("defaults to the ENTERED project, and hands a page its id — never a bare null", async () => {
    mountApp("/");
    await entered("Alpha");
    const p = await probe();
    await waitFor(() => expect(p.getAttribute("data-read")).toBe("p1"));
    expect(p.getAttribute("data-kind")).toBe("project");
    expect(p.getAttribute("data-global")).toBe("false");
    expect(railSaysGlobal()).toBe(false);
  });

  it("the `All projects` tile puts the WHOLE APP in the global scope", async () => {
    mountApp("/");
    await entered("Alpha");

    fireEvent.click(allProjectsTile());

    const p = await probe();
    await waitFor(() => expect(p.getAttribute("data-kind")).toBe("all"));
    // The global scope carries NO project id — that is the guard: a page cannot accidentally read
    // the entered project's while claiming to show every project's.
    expect(p.getAttribute("data-read")).toBe("ALL");
    expect(railSaysGlobal()).toBe(true);
  });

  it("SURVIVES navigation across every page — the mode is the app's, not the home's", async () => {
    mountApp("/");
    await entered("Alpha");
    fireEvent.click(allProjectsTile());
    await waitFor(() => expect(railSaysGlobal()).toBe(true));

    // Walk the section-nav AS IT IS IN THE GLOBAL SCOPE — `Work` and `Ideas`, the two pages with a
    // cross-project reading (`Home` is the way OUT of the mode, so it is not part of the walk).
    // Neither touches the scope: they are the same screens in either scope, which is the point of
    // the mode.
    for (const label of ["Work", "Ideas"]) {
      fireEvent.click(screen.getByRole("link", { name: label }));
      const p = await probe();
      await waitFor(() => expect(p.getAttribute("data-kind")).toBe("all"));
      expect(railSaysGlobal()).toBe(true);
    }
  });

  it("is PERSISTED and restored at relaunch, like the entered project", async () => {
    const first = mountApp("/");
    await entered("Alpha");
    fireEvent.click(allProjectsTile());
    await waitFor(() => expect(railSaysGlobal()).toBe(true));
    expect(loadGlobalScope()).toBe(true);
    first.unmount();

    // A RELAUNCH: a brand-new app over the same renderer storage, on the app's default route.
    mountApp("/");
    await entered("Alpha");
    const p = await probe();
    await waitFor(() => expect(p.getAttribute("data-kind")).toBe("all"));
    expect(railSaysGlobal()).toBe(true);
  });

  it("ENTERING A PROJECT LEAVES the global scope — the two are exclusive", async () => {
    saveGlobalScope(true);
    mountApp("/work");
    await entered("Alpha");
    await waitFor(() => expect(railSaysGlobal()).toBe(true));

    fireEvent.click(screen.getByRole("button", { name: "Project Beta" }));

    const p = await probe();
    await waitFor(() => expect(p.getAttribute("data-kind")).toBe("project"));
    expect(p.getAttribute("data-read")).toBe("p2");
    expect(railSaysGlobal()).toBe(false);
    expect(loadGlobalScope()).toBe(false);
  });

  it("leaves the global scope through the rail's `Home` too — the entered project's own recap", async () => {
    saveGlobalScope(true);
    mountApp("/work");
    await entered("Alpha");
    await waitFor(() => expect(railSaysGlobal()).toBe(true));

    fireEvent.click(screen.getByRole("link", { name: "Home" }));

    const p = await probe();
    await waitFor(() => expect(p.getAttribute("data-kind")).toBe("project"));
    expect(railSaysGlobal()).toBe(false);
  });

  it("keeps the entered project REMEMBERED while global — the rail knows where to put you back", async () => {
    mountApp("/");
    await entered("Alpha");
    fireEvent.click(allProjectsTile());
    const p = await probe();
    await waitFor(() => expect(p.getAttribute("data-kind")).toBe("all"));

    // `activeProjectId` is untouched: the global scope is not "no project", it is "every project".
    expect(p.getAttribute("data-entered")).toBe("p1");
    expect(screen.getByRole("button", { name: "Project Alpha" }).getAttribute("aria-current")).toBe(
      "true",
    );
  });

  /**
   * THE TRAP THIS FOUNDATION EXISTS TO AVOID. `activeProjectId === null` means "no project is
   * loaded", and `board-page.tsx`, `knowledge-page.tsx` and `catalogue-page.tsx` turn it into an
   * empty state. Had the global scope been spelled by nulling that field, those three would show
   * their empty screen instead of the cross-project content. The real `<BoardPage>` is mounted at
   * `/work` here to hold that line for all three.
   */
  it("never trips the `no project loaded` empty state — `activeProjectId` keeps its one meaning", async () => {
    mountApp("/");
    await entered("Alpha");
    fireEvent.click(allProjectsTile());
    await waitFor(() => expect(railSaysGlobal()).toBe(true));

    fireEvent.click(screen.getByRole("link", { name: "Work" }));

    const p = await probe();
    await waitFor(() => expect(p.getAttribute("data-kind")).toBe("all"));
    expect(screen.queryByText("Enter a project to see its Specs.")).toBeNull();
  });

  /**
   * THE RAIL SAYS WHICH SCOPE, ON EVERY PAGE. The entered project gives up its ring in the global
   * scope for the same reason it gives it up on a home: a ring would claim the screen is scoped to
   * that project, which is exactly what it is not. It keeps `aria-current` — it is still entered.
   */
  it("draws the scope on the rail on EVERY page, not only on the home", async () => {
    mountApp("/");
    const alpha = await entered("Alpha");
    expect(alpha.className).toContain("ring-2");

    fireEvent.click(allProjectsTile());
    fireEvent.click(screen.getByRole("link", { name: "Ideas" }));

    await waitFor(() => expect(railSaysGlobal()).toBe(true));
    expect(screen.getByRole("button", { name: "Project Alpha" }).className).not.toContain("ring-2");
    expect(screen.getByRole("button", { name: "Project Alpha" }).getAttribute("aria-current")).toBe(
      "true",
    );
  });

  /**
   * THE HOME'S `?scope=` IS THE MODE WRITTEN DOWN, and the only url in the app that says it. It
   * still ENTERS the mode — a deep-linked / restored `/home?scope=all` puts the app in the global
   * scope — which is what keeps that one page's url and the app from ever disagreeing.
   */
  it("enters the mode from a deep-linked `/home?scope=all`", async () => {
    mountApp("/home?scope=all");
    await entered("Alpha");
    const p = await probe();
    await waitFor(() => expect(p.getAttribute("data-kind")).toBe("all"));
    expect(railSaysGlobal()).toBe(true);

    // …and leaving it for the plain project home puts the app back in the project scope.
    fireEvent.click(screen.getByRole("link", { name: "Home" }));
    await waitFor(() => expect(p.getAttribute("data-kind")).toBe("project"));
    expect(railSaysGlobal()).toBe(false);
  });
});
