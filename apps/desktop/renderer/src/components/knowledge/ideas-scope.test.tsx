import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { RouterProvider } from "@tanstack/react-router";
import { beforeEach, describe, expect, it } from "vitest";

import { mockIPC } from "@/bridge/test-harness";
import { createAppRouter } from "@/router";
import { saveGlobalScope } from "@/components/terminals/ui-persistence";
import type { ProjectRecord, WorkspaceRecord } from "@/components/sidebar/use-projects";

import type { IdeaListItem, IdeaThemeGroup } from "./types";

/**
 * THE IDEAS TABLE IN THE CROSS-PROJECT SCOPE — the SAME page, re-parameterised.
 *
 * `/knowledge` is one table in two scopes: the entered project's ideas, or every project's.
 * Switching changes exactly TWO observable things — the list grows, and each row wears its
 * project's `<ProjectMark>` — and nothing else: same toolbar, same theme grouping, same drawer.
 *
 * Four things this file pins:
 *
 *  1. the reads are made ONCE for the whole scope, with NO project filter — never a call per
 *     project;
 *  2. THE SEARCH FOLLOWS. `idea_search` gets the same scope as `idea_list`; a query that stayed
 *     inside the entered project while the table showed everything would drop rows for a reason
 *     nothing on screen explains;
 *  3. the `Enter a project…` empty state (which means "no project is LOADED") cannot fire there;
 *  4. an idea with no project keeps its place in the flow, unmarked;
 *  5. the mark is drawn at a size that can be SEEN — at least the homes' badge, which is the
 *     precedent the user reads without effort.
 *
 * That fifth point is a correction, and the shape of the bug matters: the mark was RENDERING all
 * along (constated in the real app, on the user's own database — every row carried one), it was
 * simply too discreet to notice (« je l'ai pas du tout vu »). So the assertion here is on the
 * mark's DRAWN SIZE, not on its presence: presence was already green while the user could not see
 * it, which is exactly the gap a test of presence alone cannot close.
 */

const ALPHA_PROJECT = {
  id: "p1",
  name: "Alpha",
  accent: null,
  icon: null,
  image_path: null,
} as const;
const BETA_PROJECT = {
  id: "p2",
  name: "Beta",
  accent: null,
  icon: null,
  image_path: null,
} as const;

function idea(overrides: Partial<IdeaListItem> & { id: string }): IdeaListItem {
  return {
    project_id: "p1",
    title: "Untitled idea",
    body: "",
    status: "open",
    theme: "perf",
    source: "manual",
    created_at: 0,
    updated_at: 0,
    closed_at: null,
    closed_reason: null,
    linked_specs: [],
    project: ALPHA_PROJECT,
    ...overrides,
  };
}

const ALPHA_IDEAS: IdeaListItem[] = [idea({ id: "i1", title: "Cache the manifest" })];
const ALL_IDEAS: IdeaListItem[] = [
  ...ALPHA_IDEAS,
  idea({ id: "i2", title: "Stream the tokens", project_id: "p2", project: BETA_PROJECT }),
  // An idea whose project row has vanished: no mark, and still in the flow.
  idea({ id: "i3", title: "Orphaned thought", project_id: "p3", project: null }),
];

const THEMES: IdeaThemeGroup[] = [
  { theme: "perf", total: 3, open: 3, planned: 0, implemented: 0, closed: 0, dropped: 0 },
];

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

/** Every argument object each idea read was called with, by command. */
let reads: { cmd: string; args: Record<string, unknown> }[] = [];

function installBackend() {
  reads = [];
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
        case "idea_themes":
          reads.push({ cmd, args: a });
          return THEMES;
        case "idea_list":
          reads.push({ cmd, args: a });
          return a.projectId === undefined
            ? ALL_IDEAS
            : ALL_IDEAS.filter((i) => i.project_id === a.projectId);
        case "idea_search": {
          reads.push({ cmd, args: a });
          const pool =
            a.projectId === undefined
              ? ALL_IDEAS
              : ALL_IDEAS.filter((i) => i.project_id === a.projectId);
          const q = String(a.query ?? "").toLowerCase();
          return pool.filter((i) => i.title.toLowerCase().includes(q)).map((i) => ({ id: i.id }));
        }
        default:
          return [];
      }
    },
    { shouldMockEvents: true },
  );
}

function mount(path = "/knowledge") {
  const router = createAppRouter([path]);
  return { ...render(<RouterProvider router={router} />), router };
}

/** The idea titles listed, in DOM order. */
function listedTitles(): string[] {
  return screen.getAllByTestId("idea-title").map((el) => el.textContent ?? "");
}

/** The project each row is marked with (`null` when the row wears no mark). */
function listedMarks(): (string | null)[] {
  return screen
    .getAllByTestId(/^idea-row-/)
    .map(
      (row) =>
        row.querySelector('[data-testid="idea-project-mark"]')?.getAttribute("title") ?? null,
    );
}

/** Type into the toolbar's search box. */
function search(value: string) {
  fireEvent.change(screen.getByLabelText("Search ideas"), { target: { value } });
}

/** Open one idea's DETAIL drawer by its row, and hand the drawer back. */
async function openDrawer(ideaId: string): Promise<HTMLElement> {
  fireEvent.click(await screen.findByTestId(`idea-row-${ideaId}`));
  return await screen.findByLabelText("Idea detail");
}

describe("The Ideas table follows the scope", () => {
  beforeEach(() => {
    saveGlobalScope(false);
    installBackend();
  });

  it("lists the entered project's ideas, unmarked, in the project scope", async () => {
    mount();
    await waitFor(() => expect(listedTitles()).toEqual(["Cache the manifest"]));
    // The mark is what tells projects apart; inside ONE project there is nothing to tell apart.
    expect(listedMarks()).toEqual([null]);
    expect(reads.filter((r) => r.cmd === "idea_list")).toContainEqual({
      cmd: "idea_list",
      args: { projectId: "p1" },
    });
  });

  it("lists EVERY project's ideas in ONE read, each row marked with its project", async () => {
    saveGlobalScope(true);
    mount();
    await waitFor(() =>
      expect(listedTitles()).toEqual([
        "Cache the manifest",
        "Stream the tokens",
        "Orphaned thought",
      ]),
    );
    // Marked by project — and the orphan keeps its place in the flow, unmarked.
    expect(listedMarks()).toEqual(["Alpha", "Beta", null]);
    // ONE read per model for the WHOLE scope, with no project filter — no per-project fan-out.
    for (const cmd of ["idea_list", "idea_themes"]) {
      const made = reads.filter((r) => r.cmd === cmd);
      expect(made.length).toBeGreaterThan(0);
      expect(made.every((r) => r.args.projectId === undefined)).toBe(true);
    }
  });

  /**
   * …AND IT IS DRAWN BIG ENOUGH TO BE SEEN. The homes' badge is `size-3.5` (14px) and reads
   * effortlessly because it is DETACHED — it rides a glyph, ringed in the surface behind it. This
   * row has no glyph to ride, so it buys the same separation with size + a hairline ring. Anything
   * back at the row's own text size is the bullet the user looked straight through.
   *
   * The class is the observable jsdom has (no stylesheet is loaded), and it is the right one: the
   * size is the CALLER's half of `<ProjectMark>`'s contract, so it is this file's to pin.
   */
  it("draws the mark large enough to be seen — the homes' badge is the floor", async () => {
    saveGlobalScope(true);
    mount();
    const marks = await screen.findAllByTestId("idea-project-mark");
    expect(marks).toHaveLength(2);
    for (const mark of marks) {
      const size = Number(/(?:^|\s)size-(\d+(?:\.\d+)?)(?:\s|$)/.exec(mark.className)?.[1]);
      // `size-3.5` is the homes' badge; this mark has no glyph to ride, so it may not go below it.
      expect(size).toBeGreaterThanOrEqual(3.5);
      expect(mark.className).toContain("ring-1");
    }
  });

  /**
   * THE DETAIL SAYS IT IN CLEAR, WHERE THE ROW ONLY MARKS IT.
   *
   * A row can get by on the mark alone: the place is missing and its neighbours give the scale to
   * read the colour against. The drawer has neither — it has the room, and it has no neighbour —
   * so a coloured square alone would ask the reader to REMEMBER which colour is which project
   * (« il faut aussi afficher le projet en clair »). The assertion is therefore on the NAME as
   * text; the mark riding beside it is welcome but is not what satisfies the criterion.
   */
  it("names the idea's project IN CLEAR in the drawer, in the cross-project scope", async () => {
    saveGlobalScope(true);
    mount();
    await waitFor(() => expect(listedTitles()).toHaveLength(3));

    const drawer = await openDrawer("i2");
    expect(within(drawer).getByTestId("idea-detail-project-name").textContent).toBe("Beta");
    // It sits IN the existing meta row (`Theme / Captured / date`), as one more of its pairs —
    // not in a banner added above it. The `<dl>` is that row.
    const meta = drawer.querySelector("dl");
    expect(meta?.contains(within(drawer).getByTestId("idea-detail-project-name"))).toBe(true);
    expect(within(meta as HTMLElement).getByText("Project")).toBeTruthy();
    // …and the mark accompanies the name, without replacing it.
    expect(meta?.contains(within(drawer).getByTestId("idea-detail-project-mark"))).toBe(true);
  });

  it("says nothing about the project in the drawer, in the project scope", async () => {
    mount();
    await waitFor(() => expect(listedTitles()).toEqual(["Cache the manifest"]));

    const drawer = await openDrawer("i1");
    // The rail already says which project this is; repeating it here would be noise.
    expect(within(drawer).queryByTestId("idea-detail-project-name")).toBeNull();
    expect(within(drawer).queryByTestId("idea-detail-project-mark")).toBeNull();
  });

  /** The orphan: no project field, no filler label — exactly as its row wears no mark. */
  it("names nothing for an orphaned idea, even in the cross-project scope", async () => {
    saveGlobalScope(true);
    mount();
    await waitFor(() => expect(listedTitles()).toHaveLength(3));

    const drawer = await openDrawer("i3");
    // The drawer IS the orphan's (its title is there) — and it carries no project mention.
    expect(within(drawer).getByRole("heading", { name: "Orphaned thought" })).toBeTruthy();
    expect(within(drawer).queryByTestId("idea-detail-project-name")).toBeNull();
    expect(within(drawer).queryByText("Project")).toBeNull();
  });

  it("SEARCHES the scope the list shows — the trap this page must not set", async () => {
    saveGlobalScope(true);
    mount();
    await waitFor(() => expect(listedTitles()).toHaveLength(3));

    search("tokens");
    // The query reaches the backend with the SAME (absent) project scope as the list…
    await waitFor(() =>
      expect(reads.some((r) => r.cmd === "idea_search" && r.args.projectId === undefined)).toBe(
        true,
      ),
    );
    // …so another project's idea is findable from here.
    await waitFor(() => expect(listedTitles()).toEqual(["Stream the tokens"]));
  });

  it("narrows the search with the scope, back inside a project", async () => {
    mount();
    await waitFor(() => expect(listedTitles()).toEqual(["Cache the manifest"]));

    search("tokens");
    await waitFor(() =>
      expect(reads.some((r) => r.cmd === "idea_search" && r.args.projectId === "p1")).toBe(true),
    );
    // Beta's idea is not in this project's backlog, so nothing matches — and the page says so.
    await waitFor(() => expect(screen.getByTestId("ideas-empty")).toBeTruthy());
  });

  it("keeps `Enter a project…` for NO PROJECT LOADED, and never in the cross-project scope", async () => {
    mockIPC(
      (cmd) => {
        switch (cmd) {
          case "home_snapshot":
            return { sessions: [], services: [], specs: [], terminals: [] };
          default:
            return [];
        }
      },
      { shouldMockEvents: true },
    );
    const { unmount } = mount();
    expect(await screen.findByText("Enter a project to see its ideas.")).toBeTruthy();
    unmount();

    saveGlobalScope(true);
    mount();
    expect(await screen.findByTestId("ideas-empty")).toBeTruthy();
    expect(screen.queryByText("Enter a project to see its ideas.")).toBeNull();
  });

  it("changes ONLY the list and the marks when the scope flips", async () => {
    saveGlobalScope(true);
    mount();
    await waitFor(() => expect(listedTitles()).toHaveLength(3));
    // The chrome the two scopes share, captured before the switch.
    const toolbar = screen.getByLabelText("Filter by status").textContent;
    const themeGroup = screen.getByRole("group", { name: "Filter by theme" }).textContent;

    fireEvent.click(screen.getByRole("button", { name: "Project Alpha" }));
    fireEvent.click(await screen.findByRole("link", { name: "Ideas" }));

    await waitFor(() => expect(listedTitles()).toEqual(["Cache the manifest"]));
    expect(listedMarks()).toEqual([null]);
    // …and NOTHING else moved.
    expect(screen.getByLabelText("Filter by status").textContent).toBe(toolbar);
    expect(screen.getByRole("group", { name: "Filter by theme" }).textContent).toBe(themeGroup);
  });
});
