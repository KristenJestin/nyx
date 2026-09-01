import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { RouterProvider } from "@tanstack/react-router";
import { beforeEach, describe, expect, it } from "vitest";

import { mockIPC } from "@/bridge/test-harness";
import { createAppRouter } from "@/router";
import type { ProjectRecord, WorkspaceRecord } from "@/components/sidebar/use-projects";

/**
 * THE HOME'S TWO ENTRY POINTS, and they are both in the RAIL — the only place the app navigates
 * from.
 *
 *   * `Home`, at the HEAD of the section-nav: the ENTERED project's recap, above the surfaces one
 *     works in;
 *   * `All projects`, in its OWN pinned region at the top of the rail, above the project list and
 *     ruled off from it: the same page across every project. It belongs over the projects and not
 *     in the section-nav, because its scope is the projects themselves — and it is out of their
 *     list because it is not one of them.
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
        case "pty_spawn":
          return 100;
        default:
          return [];
      }
    },
    { shouldMockEvents: true },
  );
}

function mount(path = "/") {
  const router = createAppRouter([path]);
  return { ...render(<RouterProvider router={router} />), router };
}

/** The `<li>` index of an element inside its own list — how "first" is asserted. */
function positionInList(el: HTMLElement): number {
  const item = el.closest("li")!;
  return Array.from(item.parentElement!.children).indexOf(item);
}

/** Every LIT sliding indicator of the rail, by the `layoutId` it names itself with. */
function litIndicators(container: HTMLElement): string[] {
  return [...container.querySelectorAll<HTMLElement>("[data-rail]")].map(
    (el) => el.dataset.rail ?? "",
  );
}

/** The entered project's tile, once the project tree has loaded. */
async function enteredTile(): Promise<HTMLElement> {
  const tile = await screen.findByRole("button", { name: "Project Alpha" });
  await waitFor(() => expect(tile.getAttribute("aria-current")).toBe("true"));
  return tile;
}

describe("The home's rail entries", () => {
  beforeEach(() => {
    installBackend();
  });

  it("puts Home FIRST in the section-nav, above Terminals", async () => {
    mount();
    const home = await screen.findByRole("link", { name: "Home" });
    expect(positionInList(home)).toBe(0);
    // The same list holds Terminals, after it.
    const terminals = screen.getByRole("link", { name: "Terminals" });
    expect(terminals.closest("ul")).toBe(home.closest("ul"));
    expect(positionInList(terminals)).toBe(1);
  });

  it("routes to the project home from the rail", async () => {
    const { router } = mount();
    fireEvent.click(await screen.findByRole("link", { name: "Home" }));
    await waitFor(() => expect(router.state.location.pathname).toBe("/home"));
    expect(router.state.location.search).toEqual({});
    expect(await screen.findByTestId("home-page")).toBeTruthy();
  });

  /**
   * ITS OWN REGION — above the projects, in neither of the rail's two lists (review
   * `01M1EJK7VF9WG86V85AJ5K8JFC`). It used to be the first `<li>` of the project list, and it is
   * out of it now: a project-identity PRD is coming, so anything the user could imitate with an
   * icon or an accent is not a distinction. Its CATEGORY is what differs.
   */
  it("puts All projects in its OWN region — in neither the project list nor the section-nav", async () => {
    mount();
    const all = await screen.findByRole("link", { name: "All projects" });
    const alpha = screen.getByRole("button", { name: "Project Alpha" });
    // Not with the PROJECTS…
    expect(alpha.closest("ul")).not.toBe(all.closest("ul"));
    // …not with the SECTIONS…
    expect(screen.getByRole("link", { name: "Home" }).closest("ul")).not.toBe(all.closest("ul"));
    // …and alone in a region of its own, which is what "pinned" means here.
    expect(all.closest("ul")!.children).toHaveLength(1);
    // It still comes FIRST in the rail: the region is ABOVE the project list in document order.
    expect(all.compareDocumentPosition(alpha) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });

  /**
   * A GEOMETRY NO PROJECT CAN CLAIM — a bar, not a square. The user chooses what goes INSIDE a
   * project tile (icon, image, accent); the SHAPE of the tile is not on offer, which is the whole
   * reason the distinction was moved there.
   */
  it("wears a BAR, where every project tile is a square", async () => {
    mount();
    const all = await screen.findByRole("link", { name: "All projects" });
    const alpha = screen.getByRole("button", { name: "Project Alpha" });
    // The project tiles are the rail's square: one `size-*`, width === height.
    expect(alpha.className).toContain("size-9");
    // The bar is wider than that square and shorter than it — two independent axes, so no
    // `size-*` utility can ever produce it.
    expect(all.className).toContain("w-11");
    expect(all.className).toContain("h-7");
    expect(all.className).not.toMatch(/\bsize-\d/);
  });

  /**
   * …and changing its region and its shape changed NOTHING of what it is: a real link, named the
   * same way, reachable by the keyboard like every other entry of the rail.
   */
  it("keeps its name and stays a focusable link after leaving the list", async () => {
    mount();
    const all = await screen.findByRole("link", { name: "All projects" });
    expect(all.tagName).toBe("A");
    expect(all.getAttribute("title")).toBe("All projects");
    expect(all.getAttribute("aria-label")).toBe("All projects");
    // An anchor WITH an href is in the tab order; a stray `tabindex="-1"` would take it out.
    expect(all.hasAttribute("href")).toBe(true);
    expect(all.getAttribute("tabindex")).toBeNull();
  });

  it("routes to the cross-project home WITHOUT entering or leaving a project", async () => {
    const { router } = mount();
    // Alpha is entered before the click…
    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "Project Alpha" }).getAttribute("aria-current"),
      ).toBe("true"),
    );
    fireEvent.click(screen.getByRole("link", { name: "All projects" }));
    await waitFor(() => expect(router.state.location.pathname).toBe("/home"));
    expect(router.state.location.search).toEqual({ scope: "all" });
    expect((await screen.findByTestId("home-page")).getAttribute("data-scope")).toBe("all");
    // …and still is after: the global home is a SCOPE, not a project switch, so leaving it puts
    // the user back where they were.
    expect(screen.getByRole("button", { name: "Project Alpha" }).getAttribute("aria-current")).toBe(
      "true",
    );
  });

  /**
   * TWO REGIONS, TWO QUESTIONS — the rail's project list says WHICH SCOPE, its section-nav says
   * WHICH PAGE, and on the cross-project home both have the same answer to give at once (user
   * feedback: « la route home est pas selectionne quand je suis dans "all project" »).
   *
   * The section-nav's `Home` used to be forced dark there, to avoid "two selections". That was an
   * over-application: the double selection the user had removed was two marks in the SAME region
   * contradicting each other (the `All projects` tile lit WHILE a project wore its entered ring),
   * not two axes each stating their own value.
   */
  it("lights BOTH axes on the cross-project home — the scope AND the page", async () => {
    const { unmount } = mount("/home?scope=all");
    const all = await screen.findByRole("link", { name: "All projects" });
    // `data-active` is the rail's own answer, per region.
    expect(all.closest("li")!.getAttribute("data-active")).toBe("true");
    expect(
      screen.getByRole("link", { name: "Home" }).closest("li")!.getAttribute("data-active"),
    ).toBe("true");
    unmount();

    // …and on the project scope only the page axis has something to say: the `All projects` tile
    // is NOT the scope one is in.
    mount("/home");
    const home = await screen.findByRole("link", { name: "Home" });
    expect(home.closest("li")!.getAttribute("data-active")).toBe("true");
    expect(
      screen.getByRole("link", { name: "All projects" }).closest("li")!.getAttribute("data-active"),
    ).toBe("false");
  });

  /**
   * …and being LIT must not make the entry inert: it is the only one that can already match the
   * path while the screen is the other home, so its click is still the way OUT of the scope.
   */
  it("still leaves the global scope when Home is clicked while lit", async () => {
    const { router } = mount("/home?scope=all");
    const home = await screen.findByRole("link", { name: "Home" });
    expect(home.closest("li")!.getAttribute("data-active")).toBe("true");

    fireEvent.click(home);

    await waitFor(() => expect(router.state.location.search).toEqual({}));
    expect((await screen.findByTestId("home-page")).getAttribute("data-scope")).toBe("project");
    expect(
      screen.getByRole("link", { name: "All projects" }).closest("li")!.getAttribute("data-active"),
    ).toBe("false");
  });

  /**
   * THE RING FOLLOWS THE SCOPE, NEVER THE PAGE — and that is the whole rule ("la double selection
   * c'est hyper chelou" — review `01M1C8AHV63Y0D54BJCSN46737`, corrected by review
   * `01M1EJK7VF9WG86V85AJ5K8JFC`).
   *
   * In the GLOBAL scope the entered project gives up its ring: the `All projects` tile is the mark
   * of that region, and a ring beside it would be a second, contradicting answer to "which scope".
   * The project stays entered behind it — it keeps `aria-current`, and nothing attenuated takes
   * the ring's place.
   *
   * On the PROJECT home the ring STAYS. It briefly did not: the global-scope rule had been
   * extended to `!onHome`, which made Home the only page in the app that de-selects the project
   * while the page itself displayed that project's name ("ca deselectionne le projet, je ne
   * comprend pas pourquoi c'est la deuxieme fois que c'est different pour cette page"). There is
   * no contradiction to resolve inside a project: both regions simply answer their own question —
   * the ring says WHICH SCOPE, the lit `Home` entry says WHICH PAGE.
   */
  it("gives up the ring in the GLOBAL scope only — the project home keeps it", async () => {
    const { container, unmount } = mount("/home?scope=all");
    const alphaGlobal = await enteredTile();
    // Over the PROJECTS the scope's tile is the only lit mark — no project ring beside it. The
    // section-nav's own indicator sits in the other region and is expected here (two axes).
    expect(litIndicators(container as HTMLElement)).toEqual(["rail-all-projects", "rail-nav"]);
    expect(alphaGlobal.className).not.toContain("ring-2");
    // …and the project is still the entered one behind it.
    expect(alphaGlobal.getAttribute("aria-current")).toBe("true");
    unmount();

    // The PROJECT home: BOTH axes speak — the entered project wears its ring AND `Home` is lit.
    const project = mount("/home");
    const alphaProject = await enteredTile();
    expect(litIndicators(project.container as HTMLElement)).toEqual(["rail-project", "rail-nav"]);
    expect(alphaProject.className).toContain("ring-2");
    expect(alphaProject.getAttribute("aria-current")).toBe("true");
    expect(
      screen.getByRole("link", { name: "Home" }).closest("li")!.getAttribute("data-active"),
    ).toBe("true");
  });

  it("leaves the entered project's ring exactly as it is on every project-scope route, `/home` included", async () => {
    for (const path of ["/", "/home", "/work", "/knowledge", "/engine", "/design"]) {
      const { container, unmount } = mount(path);
      const alpha = await enteredTile();
      expect(alpha.className).toContain("ring-2");
      expect(litIndicators(container as HTMLElement)).toContain("rail-project");
      unmount();
    }
  });

  /**
   * …and the ring FOLLOWS the scope live, without leaving the page: entering the global scope from
   * the project home takes it away, coming back puts it back. The route never moves (`/home`
   * serves both), so this is the rail reacting to the SCOPE and to nothing else.
   */
  it("takes the ring away and gives it back as the scope flips, without leaving Home", async () => {
    mount("/home");
    const alpha = await enteredTile();
    expect(alpha.className).toContain("ring-2");

    fireEvent.click(screen.getByRole("link", { name: "All projects" }));
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Project Alpha" }).className).not.toContain(
        "ring-2",
      ),
    );

    fireEvent.click(screen.getByRole("link", { name: "Home" }));
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Project Alpha" }).className).toContain("ring-2"),
    );
  });
});
