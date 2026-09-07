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
import { loadScopeKind, saveScopeKind } from "./ui-persistence";

/**
 * THE APP SCOPE — THREE scopes as a STATE OF THE APPLICATION, not a parameter of one page: the
 * ENTERED PROJECT, ALL PROJECTS, and SYSTÈME.
 *
 * This is the FOUNDATION every page branches onto, so what is proven here is the CONTRACT and
 * nothing about any page's content:
 *
 *   1. ONE source of truth — `useActiveProject().scope` — that any page can read without knowing
 *      the home exists, let alone parsing its url;
 *   2. it SURVIVES navigation between every page;
 *   3. it is PERSISTED and restored at relaunch, exactly as the entered project is;
 *   4. the three are EXCLUSIVE — entering one leaves the other two;
 *   5. and none of them ever gets spelled as `activeProjectId === null`, which keeps its one
 *      meaning ("no project is loaded") and the three empty states that read it.
 *
 * SYSTÈME IS THE THIRD ONE since the human decision that reverses finding U1
 * (`01M1EC7Y2625Q07C5386GKHZFS`). It had been demoted to a mere page, which left the rail marking a
 * project as entered AND selected while the host's terminals filled the screen, and left six
 * project entries in the section-nav over a scope that has none of them. What U1 gave it — a route
 * of its own — is KEPT, and that route now MIRRORS the scope the way `/home?scope=` does.
 *
 * The rail is mounted for real (`<BaseLayout>`), because the rail is the scope SELECTOR: the
 * `All projects` bar, the project tiles and the Système tile are the three ways in. The pages are a
 * PROBE (which reads the contract as a page would) plus the real `<BoardPage>`, whose
 * `activeProjectId === null` empty state is one of the three the mode must not trip.
 */

function project(id: string, name: string): ProjectRecord {
  return { id, name, key: "P", collapsed: false, created_at: 0, updated_at: 0, resume_agent_sessions: false };
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

/**
 * The backend the shell reads. `withSystem` decides whether the SINGLETON exists: `false` plays the
 * documented `null` answer of a database that never bootstrapped one (the rail then renders no
 * Système tile at all), `true` plays a real install. The cases about the project ↔ all pair keep
 * the former — the tile is irrelevant to them and its absence proves the rail survives it — and the
 * Système cases take the latter.
 */
function installBackend(withSystem = false) {
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
        case "system_scope_detail":
          // `list_projects` above answers USER projects only — the backend contract
          // (`db::list_projects` filters `kind = 'user'`), reproduced faithfully: the singleton
          // reaches the rail through THIS read and lands outside the reorderable array.
          return withSystem
            ? {
                project: { ...project("sys", "System"), kind: "system" },
                workspace: { ...workspace("sys"), name: "system", path: "/home/kris" },
                home: "/home/kris",
              }
            : null;
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
  const { scope, globalScope, systemScope, activeProjectId } = useActiveProject();
  return (
    <div
      data-testid="probe"
      data-kind={scope.kind}
      data-global={globalScope}
      data-system={systemScope}
      // What the page would hand its read model: the project id in the project scope, and NO id at
      // all in the two others. `null` here is "no project loaded", never "all" and never "system".
      data-read={
        scope.kind === "all"
          ? "ALL"
          : scope.kind === "system"
            ? "SYSTEM"
            : (scope.projectId ?? "NONE")
      }
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
      // The SYSTÈME scope's one screen. A probe rather than the real `<SystemScopePage>`: what is
      // under test here is the app's scope contract, and the page's own behaviour has its own
      // suite (`system/system-scope-page.test.tsx`).
      probe("/system"),
      probe("/knowledge"),
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

/** The rail's SYSTÈME tile — the third scope's way in. */
function systemTile(): HTMLElement {
  return screen.getByRole("button", { name: "System" });
}

/** Whether the rail draws the app as being in the Système scope. */
function railSaysSystem(): boolean {
  return systemTile().getAttribute("aria-current") === "true";
}

/** The labels the section-nav is drawing right now, in DOM order. */
function navLabels(): string[] {
  return ["Home", "Terminals", "Work", "Ideas", "Design"].filter(
    (label) => screen.queryByRole("link", { name: label }) !== null,
  );
}

describe("The app scope — one source of truth for every page", () => {
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
    expect(loadScopeKind()).toBe("all");
    first.unmount();

    // A RELAUNCH: a brand-new app over the same renderer storage, on the app's default route.
    mountApp("/");
    await entered("Alpha");
    const p = await probe();
    await waitFor(() => expect(p.getAttribute("data-kind")).toBe("all"));
    expect(railSaysGlobal()).toBe(true);
  });

  it("ENTERING A PROJECT LEAVES the global scope — the two are exclusive", async () => {
    saveScopeKind("all");
    mountApp("/work");
    await entered("Alpha");
    await waitFor(() => expect(railSaysGlobal()).toBe(true));

    fireEvent.click(screen.getByRole("button", { name: "Project Beta" }));

    const p = await probe();
    await waitFor(() => expect(p.getAttribute("data-kind")).toBe("project"));
    expect(p.getAttribute("data-read")).toBe("p2");
    expect(railSaysGlobal()).toBe(false);
    expect(loadScopeKind()).toBe("project");
  });

  it("leaves the global scope through the rail's `Home` too — the entered project's own recap", async () => {
    saveScopeKind("all");
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

/**
 * SYSTÈME — the THIRD scope, and the reversal of finding U1 (`01M1EC7Y2625Q07C5386GKHZFS`).
 *
 * These cases used to live in `system/system-rail.test.tsx` asserting the OPPOSITE: that entering
 * Système left the project tile lit and the six section entries standing, because Système was "a
 * page one visits, not a place one moves into". Seen in the real app, that model put two
 * contradictory marks in the rail's scope region and offered a nav that leads out of the scope one
 * has just entered. The human ruled the other way; this is the contract that replaces it.
 *
 * What U1 got right is UNCHANGED and still checked here: `/system` is a real route, and the entered
 * project is REMEMBERED rather than dropped — one click on its tile puts the user back.
 */
describe("The SYSTÈME scope — the third one", () => {
  beforeEach(() => {
    installBackend(true);
  });

  it("the Système tile puts the WHOLE APP in the `system` scope, and carries no project id", async () => {
    mountApp("/");
    await entered("Alpha");

    fireEvent.click(systemTile());

    const p = await probe();
    await waitFor(() => expect(p.getAttribute("data-kind")).toBe("system"));
    // No project id, by construction — the guard that keeps a page from reading the entered
    // project's runtime while claiming to show the host's.
    expect(p.getAttribute("data-read")).toBe("SYSTEM");
    expect(p.getAttribute("data-system")).toBe("true");
    expect(p.getAttribute("data-global")).toBe("false");
    expect(railSaysSystem()).toBe(true);
  });

  it("navigates to `/system`, and a deep-linked `/system` ENTERS the scope", async () => {
    // The route survives U1 — a scope with no path of its own is what made the rail light
    // `Terminals` over the host's screen. What is added is the MIRROR, both ways.
    const first = mountApp("/");
    await entered("Alpha");
    fireEvent.click(systemTile());
    await waitFor(() => expect(first.router.state.location.pathname).toBe("/system"));
    first.unmount();

    const { router } = mountApp("/system");
    await entered("Alpha");
    const p = await probe();
    await waitFor(() => expect(p.getAttribute("data-kind")).toBe("system"));
    expect(router.state.location.pathname).toBe("/system");
    expect(railSaysSystem()).toBe(true);
  });

  it("DESELECTS every project tile — one mark in the scope region, never two", async () => {
    mountApp("/");
    const alpha = await entered("Alpha");
    expect(alpha.className).toContain("ring-2");

    fireEvent.click(systemTile());
    await waitFor(() => expect(railSaysSystem()).toBe(true));

    // The ring is gone from EVERY tile — the same rule the global scope already followed.
    for (const name of ["Project Alpha", "Project Beta"]) {
      expect(screen.getByRole("button", { name }).className).not.toContain("ring-2");
    }
    expect(railSaysGlobal()).toBe(false);
    // …but the project is still ENTERED. It is remembered, not dropped: that is what makes going
    // back one click, and what U1 was right to insist on.
    expect(screen.getByRole("button", { name: "Project Alpha" }).getAttribute("aria-current")).toBe(
      "true",
    );
    expect((await probe()).getAttribute("data-entered")).toBe("p1");
  });

  it("EMPTIES the section-nav — every entry of it reads a project", async () => {
    mountApp("/");
    await entered("Alpha");
    expect(navLabels()).toEqual(["Home", "Terminals", "Work", "Ideas", "Design"]);

    fireEvent.click(systemTile());
    await waitFor(() => expect(railSaysSystem()).toBe(true));

    // NONE of the six. They read the entered project (five) or the projects as a set (three of
    // them are `crossProject`); the singleton is in neither, so all six would lead OUT of the scope
    // just entered. The `All projects` bar and the project tiles are untouched — they are the scope
    // region, which is precisely the way out.
    expect(navLabels()).toEqual([]);
    expect(allProjectsTile()).toBeTruthy();
    expect(screen.getByRole("button", { name: "Project Alpha" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Settings" })).toBeTruthy();
  });

  it("is LEFT by a project tile, which restores the project scope and its nav", async () => {
    mountApp("/");
    await entered("Alpha");
    fireEvent.click(systemTile());
    await waitFor(() => expect(railSaysSystem()).toBe(true));

    fireEvent.click(screen.getByRole("button", { name: "Project Beta" }));

    const p = await probe();
    await waitFor(() => expect(p.getAttribute("data-kind")).toBe("project"));
    expect(p.getAttribute("data-read")).toBe("p2");
    expect(railSaysSystem()).toBe(false);
    expect(navLabels()).toEqual(["Home", "Terminals", "Work", "Ideas", "Design"]);
  });

  it("is LEFT by the `All projects` bar too — the three are exclusive", async () => {
    mountApp("/");
    await entered("Alpha");
    fireEvent.click(systemTile());
    await waitFor(() => expect(railSaysSystem()).toBe(true));

    fireEvent.click(allProjectsTile());

    const p = await probe();
    await waitFor(() => expect(p.getAttribute("data-kind")).toBe("all"));
    expect(railSaysSystem()).toBe(false);
    expect(railSaysGlobal()).toBe(true);
  });

  it("is PERSISTED and restored at relaunch, like the other two", async () => {
    const first = mountApp("/");
    await entered("Alpha");
    fireEvent.click(systemTile());
    await waitFor(() => expect(railSaysSystem()).toBe(true));
    expect(loadScopeKind()).toBe("system");
    first.unmount();

    const { router } = mountApp("/system");
    await entered("Alpha");
    await waitFor(() => expect(railSaysSystem()).toBe(true));
    expect(router.state.location.pathname).toBe("/system");
  });

  it("falls back to the project scope when the singleton VANISHES under the page", async () => {
    // The tile stops being rendered on a database with no singleton, so a `/system` left standing
    // would be a screen nothing in the rail points at — and a scope nothing in the rail marks.
    installBackend(false);
    const { router } = mountApp("/system");
    await entered("Alpha");
    await waitFor(() => expect(router.state.location.pathname).toBe("/"));
    const p = await probe();
    await waitFor(() => expect(p.getAttribute("data-kind")).toBe("project"));
    expect(screen.queryByRole("button", { name: "System" })).toBeNull();
  });
});
