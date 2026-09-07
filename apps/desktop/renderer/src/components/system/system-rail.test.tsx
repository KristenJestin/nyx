import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";
import { mockIPC } from "@/bridge/test-harness";
import { describe, expect, it } from "vitest";

import type { ProjectRecord, WorkspaceRecord } from "@/components/sidebar/use-projects";
import { ActiveProjectProvider } from "@/components/terminals/active-project";
import { BaseLayout } from "@/components/terminals/base-layout";

const HOME = "/home/kris";

function project(id: string, name: string): ProjectRecord {
  return {
    id,
    name,
    collapsed: false,
    created_at: 0,
    updated_at: 0,
    key: "P",

    resume_agent_sessions: false,
    kind: "user",
  };
}

function root(id: string, projectId: string): WorkspaceRecord {
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

/**
 * `list_projects` answers USER projects only — the backend contract (`db::list_projects` filters
 * `kind = 'user'`), reproduced faithfully here, because the whole point of the assertions below is
 * that the singleton reaches the rail through its OWN read and lands OUTSIDE that array.
 * `withScope=false` plays a database that has no singleton at all.
 */
function installBackend(withScope = true) {
  mockIPC((cmd, args) => {
    const a = (args ?? {}) as Record<string, unknown>;
    switch (cmd) {
      case "list_projects":
        return [project("p1", "Alpha"), project("p2", "Beta")];
      case "list_workspaces":
        return [root(`ws-${a.projectId as string}`, a.projectId as string)];
      case "system_scope_detail":
        return withScope
          ? {
              project: {
                ...project("sys-project", "System"),
                kind: "system",
              },
              workspace: {
                ...root("sys-ws", "sys-project"),
                name: "system",
                path: HOME,
              },
              home: HOME,
            }
          : null;
      case "workspace_runtime_snapshot":
        return {
          workspace: { id: a.workspaceId as string, name: "root", path: "/x" },
          terminals: [],
          agent_sessions: [],
          services: [],
        };
      case "agent_active_sessions":
      case "agent_activity_snapshot":
        return [];
      default:
        return undefined;
    }
  });
}

/** `<BaseLayout>` inside a real router with stub pages, so the nav links and `<Outlet/>` behave. */
function renderLayout(initialEntry = "/") {
  const rootRoute = createRootRoute({
    component: () => (
      <ActiveProjectProvider>
        <BaseLayout />
      </ActiveProjectProvider>
    ),
  });
  const mk = (path: string, text: string) =>
    createRoute({ getParentRoute: () => rootRoute, path, component: () => <div>{text}</div> });
  const routeTree = rootRoute.addChildren([
    mk("/", "INDEX PAGE"),
    mk("/home", "HOME PAGE"),
    mk("/system", "SYSTEM PAGE"),
    mk("/work", "WORK PAGE"),
    mk("/work/$specId", "SPEC PAGE"),
    mk("/knowledge", "KNOWLEDGE PAGE"),
    mk("/design", "CATALOGUE PAGE"),
  ]);
  const router = createRouter({
    routeTree,
    history: createMemoryHistory({ initialEntries: [initialEntry] }),
  });
  return { ...render(<RouterProvider router={router} />), router };
}

/** The rail's hairline rules, in DOM order — the `<div className="… h-px …">` separators. */
function rulesOf(rail: HTMLElement) {
  return [...rail.querySelectorAll<HTMLElement>("div.h-px")];
}

/** True when `el` sits strictly between `a` and `b` in the document. */
function between(a: HTMLElement, b: HTMLElement, el: HTMLElement) {
  return Boolean(
    a.compareDocumentPosition(el) & Node.DOCUMENT_POSITION_FOLLOWING &&
      b.compareDocumentPosition(el) & Node.DOCUMENT_POSITION_PRECEDING,
  );
}

describe("the Système tile in the rail", () => {
  it("sits in the FOOT GROUP — after every project tile and every nav entry, above Settings", async () => {
    installBackend();
    renderLayout();
    const system = await screen.findByRole("button", { name: "System" });
    const alpha = await screen.findByRole("button", { name: "Project Alpha" });
    const design = screen.getByRole("link", { name: "Design" });
    const settings = screen.getByRole("button", { name: "Settings" });
    // DOCUMENT_POSITION_PRECEDING: the Système tile comes AFTER the projects and after the last
    // section-nav entry — it is a SERVICE entry, not one more scope to choose between (finding S1).
    expect(system.compareDocumentPosition(alpha) & Node.DOCUMENT_POSITION_PRECEDING).toBeTruthy();
    expect(
      system.compareDocumentPosition(design) & Node.DOCUMENT_POSITION_PRECEDING,
    ).toBeTruthy();
    // …and above Settings, the other always-available entry point it now shares the foot with.
    expect(system.compareDocumentPosition(settings) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });

  it("keeps the rule BETWEEN itself and Settings — routes above, dialog below (X2)", async () => {
    installBackend();
    renderLayout();
    const system = await screen.findByRole("button", { name: "System" });
    const rail = system.closest("aside") as HTMLElement;
    const design = screen.getByRole("link", { name: "Design" });
    const settings = screen.getByRole("button", { name: "Settings" });
    const rules = rulesOf(rail);

    // The rule used to be rendered BEFORE the foot list, which fenced the tile off from the
    // section-nav — the one boundary U1 says does not exist, Système being a routed page like its
    // five neighbours. What the rule marks now is the seam between what NAVIGATES and what OPENS.
    // Counted, not merely located: the previous placement leaves this at 0 here and 1 below.
    expect(rules.filter((r) => between(system, settings, r))).toHaveLength(1);
    expect(rules.filter((r) => between(design, system, r))).toHaveLength(0);
  });

  it("is OUTSIDE the projects' drag-reorder — the foot list, not an item of theirs", async () => {
    installBackend();
    renderLayout();
    const system = await screen.findByRole("button", { name: "System" });
    const alpha = await screen.findByRole("button", { name: "Project Alpha" });
    const beta = await screen.findByRole("button", { name: "Project Beta" });
    // The two projects share the sortable list; the Système tile lives in the FOOT list, so no drop
    // can ever place it among them and `projects_reorder` can never receive its id.
    expect(alpha.closest("ul")).toBe(beta.closest("ul"));
    expect(system.closest("ul")).not.toBe(alpha.closest("ul"));
  });

  it("never enters the reorderable project array (`useProjects` stays user-only)", async () => {
    installBackend();
    renderLayout();
    await screen.findByRole("button", { name: "System" });
    // The rail draws one tile per `useProjects` tree, labelled "Project <name>". The singleton is
    // not one of them — it is not in the array at all.
    expect(screen.queryByRole("button", { name: "Project System" })).toBeNull();
    expect(screen.getAllByRole("button", { name: /^Project / })).toHaveLength(2);
  });

  it("has NO project actions (no rename / delete / add-workspace menu)", async () => {
    installBackend();
    renderLayout();
    const system = await screen.findByRole("button", { name: "System" });
    fireEvent.contextMenu(system);
    // A project tile right-clicks into its CRUD; the singleton's tile opens nothing, because the
    // backend refuses every one of those operations on it.
    await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());
    expect(screen.queryByRole("menuitem", { name: /Delete project System/ })).toBeNull();
  });

  it("does not exist at all when the database has no singleton — but the rule stays", async () => {
    installBackend(false);
    renderLayout();
    const alpha = await screen.findByRole("button", { name: "Project Alpha" });
    expect(screen.queryByRole("button", { name: "System" })).toBeNull();

    // THE RULE OUTLIVES THE TILE. It was conditioned on the singleton back when it bordered the
    // group holding the tile; it borders Settings now, and that boundary — routes above, dialog
    // below — is drawn by the static section entries whether or not a singleton exists.
    const rail = alpha.closest("aside") as HTMLElement;
    const design = screen.getByRole("link", { name: "Design" });
    const settings = screen.getByRole("button", { name: "Settings" });
    expect(rulesOf(rail).filter((r) => between(design, settings, r))).toHaveLength(1);
  });

  it("TAKES the scope region's mark — the project tile gives up its ring, `Terminals` its own", async () => {
    // The REVERSAL of human review 01M1EC7Y2625Q07C5386GKHZFS / U1, in one test. U1 had made
    // Système a mere PAGE, so entering it left the project tile RINGED beside a lit Système tile —
    // two marks in one region for one question — over a screen that is not that project's. It is a
    // SCOPE again: one mark. What U1 got right is kept, and is checked here too: `/system` is a
    // real route, so `Terminals` is at rest rather than lit over a foreign screen.
    installBackend();
    const { router } = renderLayout();
    const system = await screen.findByRole("button", { name: "System" });
    const alpha = await screen.findByRole("button", { name: "Project Alpha" });
    const terminals = screen.getByRole("link", { name: "Terminals" });
    await waitFor(() => expect(alpha.getAttribute("aria-current")).toBe("true"));
    // On `/`, in the project scope: `Terminals` is the active nav entry and Alpha wears the ring —
    // the reference the assertions below measure against, captured while both are legitimately on.
    expect(terminals.className).toContain("bg-muted");
    expect(alpha.className).toContain("ring-2");

    fireEvent.click(system);
    await waitFor(() => expect(router.state.location.pathname).toBe("/system"));
    await waitFor(() => expect(system.getAttribute("aria-current")).toBe("true"));
    // THE SCOPE MOVED: the ring leaves the project, and nothing takes its place on a tile.
    expect(alpha.className).not.toContain("ring-2");
    // …and the project is still ENTERED — remembered, not dropped, which is what makes the way
    // back one click (exactly what U1 was right about).
    expect(alpha.getAttribute("aria-current")).toBe("true");
    // The section-nav is empty in this scope, so nothing at all is lit in the PAGE region.
    expect(screen.queryByRole("link", { name: "Terminals" })).toBeNull();

    // Back into the project: the tile lets the marker go, the project takes its ring back. The
    // landing page is the project's HOME, not `/` — entering a project shows you that project, and
    // the screen that shows a project is its home (see `enterProject`).
    fireEvent.click(alpha);
    await waitFor(() => expect(router.state.location.pathname).toBe("/home"));
    expect(alpha.getAttribute("aria-current")).toBe("true");
    expect(alpha.className).toContain("ring-2");
    expect(system.getAttribute("aria-current")).toBe("false");
    expect(screen.getByRole("link", { name: "Home" }).className).toContain("bg-muted");
  });

  it("does not lose the entered project on the way in, nor on the way back", async () => {
    installBackend();
    const { router } = renderLayout();
    const system = await screen.findByRole("button", { name: "System" });
    // Enter the SECOND project, so the assertion cannot be satisfied by the default restore.
    fireEvent.click(await screen.findByRole("button", { name: "Project Beta" }));
    const beta = screen.getByRole("button", { name: "Project Beta" });
    await waitFor(() => expect(beta.getAttribute("aria-current")).toBe("true"));

    fireEvent.click(system);
    await waitFor(() => expect(router.state.location.pathname).toBe("/system"));
    // Still entered, still Beta. The singleton never BECOMES the entered project — it is absent
    // from `useProjects` by backend contract — so there is nothing for the rail's restore effect
    // to "recover" from, and it stays silent (the guard it carries for this scope).
    expect(beta.getAttribute("aria-current")).toBe("true");

    // ONE click back, straight onto Beta's own home — the way out of the scope, and the reason the
    // entered project has to survive it. The section-nav comes back with it.
    fireEvent.click(beta);
    await waitFor(() => expect(router.state.location.pathname).toBe("/home"));
    expect(beta.getAttribute("aria-current")).toBe("true");
    expect(system.getAttribute("aria-current")).toBe("false");
    expect(screen.getByRole("link", { name: "Work" })).toBeTruthy();
  });

  it("wears the SECTION-NAV's rule, not the project tile's skin: no background until it is active", async () => {
    // Human review 01M1E7K6D81M057JG5MVV4J7QE / T1. The tile carried `bg-muted` UNCONDITIONALLY —
    // harmless at the head among identical squares, but at the foot among bare icons the same pixel
    // reads as "selected, permanently". The rule is now the nav icons' own (`renderNavItem`): the
    // background IS the active marker, and nothing else says the state twice.
    installBackend();
    renderLayout();
    const system = await screen.findByRole("button", { name: "System" });
    const terminals = screen.getByRole("link", { name: "Terminals" });

    // AT REST: no background, muted glyph, and the hover is the nav icons' colour hover.
    expect(system.className).not.toMatch(/(^|\s|:)bg-muted(\s|$)/);
    expect(system.className).toContain("text-muted-foreground");
    expect(system.className).toContain("hover:bg-muted/60");
    expect(system.className).toContain("hover:text-foreground");
    expect(system.className).toContain("transition-colors");
    // …and NEITHER of the two project-tile treatments survives: the ring would be a third marker
    // for one state, the scale a second hover language.
    expect(system.className).not.toContain("ring-");
    expect(system.className).not.toContain("scale-105");
    expect(system.className).not.toContain("transition-transform");

    // The comparison that makes the ACTIVE half a RULE and not a coincidence, captured while
    // `Terminals` is legitimately the active entry (i.e. on `/`, before the click below): an active
    // nav icon is `bg-muted text-foreground`. It cannot be measured after the click any more — the
    // Système page has its own route now, so no nav entry is active while it is up (finding U1).
    expect(terminals.className).toContain("bg-muted");
    expect(terminals.className).toContain("text-foreground");

    fireEvent.click(system);
    await waitFor(() => expect(system.getAttribute("aria-current")).toBe("true"));
    // ACTIVE: exactly what an active nav icon wears — `bg-muted text-foreground`, no ring.
    expect(system.className).toContain("bg-muted");
    expect(system.className).toContain("text-foreground");
    expect(system.className).not.toContain("ring-");
    // The tile is still there and still clickable at rest — a background was never what made it so.
    fireEvent.click(screen.getByRole("button", { name: "Project Alpha" }));
    await waitFor(() => expect(system.getAttribute("aria-current")).toBe("false"));
    expect(system.className).not.toMatch(/(^|\s|:)bg-muted(\s|$)/);
    fireEvent.click(system);
    await waitFor(() => expect(system.getAttribute("aria-current")).toBe("true"));
  });

  it("MASKS THE FIVE section entries — every one of them reads a project", async () => {
    // This test has now said both things, and the second one is the human's ruling. Finding S2 hid
    // four of them; U1 restored all of them on the reasoning that the project is still entered, so they
    // still lead to its surfaces. True — and that is precisely the objection: they lead OUT of the
    // scope one has just entered. Système being a scope again, the rule that governs the other two
    // governs it — an entry belongs to a scope when it READS in it, and the singleton has no Spec,
    // no idea and no design system. The way back is the project tile, one click,
    // with the project still remembered (the test above).
    installBackend();
    const { router } = renderLayout();
    const system = await screen.findByRole("button", { name: "System" });

    fireEvent.click(system);
    await waitFor(() => expect(router.state.location.pathname).toBe("/system"));
    for (const label of ["Home", "Terminals", "Work", "Ideas", "Design"]) {
      expect(screen.queryByRole("link", { name: label })).toBeNull();
    }
    // The SCOPE region is untouched — it is the way out, so masking it would strand the user — and
    // so is Settings, which belongs to no scope.
    expect(screen.getByRole("link", { name: "All projects" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Project Alpha" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Settings" })).toBeTruthy();
  });

  it("does not redirect ANY project-scoped route — there is nothing left to redirect", async () => {
    // The redirect of finding S3 armed itself off every project-scoped route to force the user back
    // to `/`, because the page had no path of its own. It has one (U1's half that is kept), so a
    // route change is a route change: nothing bounces, and a navigation made from outside the rail
    // simply lands.
    installBackend();
    const { router } = renderLayout();
    fireEvent.click(await screen.findByRole("button", { name: "System" }));
    await waitFor(() => expect(router.state.location.pathname).toBe("/system"));

    for (const to of ["/knowledge", "/design", "/work"] as const) {
      await router.navigate({ to });
      await waitFor(() => expect(router.state.location.pathname).toBe(to));
    }
    // …a CHILD route too (`/work/$specId`), the case the redirect pattern was widened to cover.
    await router.navigate({ to: "/work/$specId", params: { specId: "spec-1" } });
    await waitFor(() => expect(router.state.location.pathname).toBe("/work/spec-1"));
  });

  it("opens from a project-scoped page and goes back to it, project intact", async () => {
    installBackend();
    const { router } = renderLayout("/design");
    await waitFor(() => expect(router.state.location.pathname).toBe("/design"));
    const alpha = await screen.findByRole("button", { name: "Project Alpha" });
    await waitFor(() => expect(alpha.getAttribute("aria-current")).toBe("true"));

    fireEvent.click(await screen.findByRole("button", { name: "System" }));
    // It used to be EJECTED to `/` from here, the scope refusing to sit on a project page. The
    // catalogue is simply left for the Système screen now, like any other navigation.
    await waitFor(() => expect(router.state.location.pathname).toBe("/system"));
    expect(alpha.getAttribute("aria-current")).toBe("true");

    // The way back is the project TILE, not a section entry (there is none in this scope) — and it
    // lands on the project's home, the screen that shows a project.
    fireEvent.click(alpha);
    await waitFor(() => expect(router.state.location.pathname).toBe("/home"));
    expect(alpha.getAttribute("aria-current")).toBe("true");
    expect(screen.getByRole("link", { name: "Design" })).toBeTruthy();
  });
});
