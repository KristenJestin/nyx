import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { RouterProvider } from "@tanstack/react-router";
import { beforeEach, describe, expect, it } from "vitest";

import { mockIPC } from "@/bridge/test-harness";
import { createAppRouter } from "@/router";

import type { IdeaListItem, IdeaThemeGroup } from "./types";

/** An `idea_list` row fixture (enriched with linked Specs). */
function idea(overrides: Partial<IdeaListItem>): IdeaListItem {
  return {
    id: "i0",
    project_id: "p1",
    title: "Untitled idea",
    body: "",
    status: "open",
    theme: "RBAC",
    source: "manual",
    created_at: 0,
    updated_at: 0,
    closed_at: null,
    closed_reason: null,
    linked_specs: [],
    ...overrides,
  };
}

// The idea_themes read model spans EVERY status (the renderer paints the full mini-dots +
// filters client-side), so the fixture carries the `closed` bucket too.
const THEMES: IdeaThemeGroup[] = [
  { theme: "RBAC", total: 3, open: 1, planned: 1, implemented: 1, closed: 0, dropped: 0 },
  { theme: "Mail", total: 2, open: 1, planned: 0, implemented: 0, closed: 1, dropped: 0 },
];

const IDEAS: IdeaListItem[] = [
  idea({
    id: "i1",
    title: "RBAC public decorator",
    body: "unify the public whitelist so a decorator suffices",
    status: "open",
    theme: "RBAC",
    source: "review",
    linked_specs: [
      { spec_id: "s1", kind: "addresses", title: "RBAC ph2", status: "in_review", number: 2 },
    ],
  }),
  idea({ id: "i2", title: "Narrow OT writes", status: "planned", theme: "RBAC" }),
  idea({
    id: "i4",
    title: "Theme sync cross-tabs",
    status: "implemented",
    theme: "RBAC",
    closed_reason: "implemented via ThemeService",
  }),
  idea({ id: "i3", title: "Brevo to M365", status: "open", theme: "Mail" }),
  // A MANUALLY closed idea (fermeture par constat) — terminal, hidden under the Active default.
  idea({
    id: "i5",
    title: "Legacy SMTP relay retired",
    status: "closed",
    theme: "Mail",
    closed_reason: "already handled by the Brevo migration",
  }),
];

function installBackend(themes = THEMES, ideas = IDEAS) {
  mockIPC((cmd, args) => {
    switch (cmd) {
      case "list_projects":
        return [
          {
            id: "p1",
            name: "Project",
            key: "NYX",
            collapsed: false,
            created_at: 0,
            updated_at: 0,
            resume_agent_sessions: false,
          },
        ];
      case "list_workspaces":
        return [];
      case "idea_themes":
        return themes;
      case "idea_list":
        return ideas;
      case "idea_search": {
        // The backend FTS search: return the ids whose title contains the query (case-insensitive).
        const q = String((args as { query?: string })?.query ?? "").toLowerCase();
        return ideas.filter((i) => i.title.toLowerCase().includes(q)).map((i) => ({ id: i.id }));
      }
      case "spec_detail":
        // The linked-spec Link navigates to `/work/$specId`; the target detail is out of scope here,
        // so reject → the detail page renders its graceful "not found" branch (no boundary trip).
        throw new Error("spec_detail not mocked in the Knowledge test");
      default:
        return undefined;
    }
  });
}

function renderPage() {
  const router = createAppRouter(["/knowledge"]);
  return { router, ...render(<RouterProvider router={router} />) };
}

describe("KnowledgePage (Idées view)", () => {
  beforeEach(() => {
    // The grouped↔flat layout persists to localStorage — clear it so each test starts from the
    // default (grouped) rather than inheriting a prior test's toggle.
    localStorage.clear();
    installBackend();
  });

  it("renders the header with the backlog counts", async () => {
    renderPage();
    expect(await screen.findByRole("heading", { name: "Ideas" })).toBeTruthy();
    // 5 ideas, open+planned = 3 tracked, 2 themes.
    expect(await screen.findByText(/5 ideas · 3 open\/tracked · 2 themes/)).toBeTruthy();
  });

  it("groups ideas by theme with collapsible group headers", async () => {
    renderPage();
    // The `expanded` filter picks the GROUP header buttons (they carry aria-expanded), not the
    // theme facet chips (which carry aria-pressed) that also spell the theme name.
    const rbac = await screen.findByRole("button", { name: /RBAC/, expanded: true });
    expect(screen.getByRole("button", { name: /Mail/, expanded: true })).toBeTruthy();
    // Rows are visible (default open) and the group is expanded.
    expect(await screen.findByTestId("idea-row-i1")).toBeTruthy();
    // Collapsing the group flips aria-expanded.
    fireEvent.click(rbac);
    await waitFor(() => expect(rbac.getAttribute("aria-expanded")).toBe("false"));
  });

  it("filters by status (only the matching ideas remain)", async () => {
    renderPage();
    await screen.findByTestId("idea-row-i1");
    // Filter to Done (implemented) — only the implemented idea remains, other groups drop out.
    fireEvent.click(screen.getByRole("button", { name: "Done" }));
    await waitFor(() => expect(screen.queryByTestId("idea-row-i1")).toBeNull());
    expect(screen.getByTestId("idea-row-i4")).toBeTruthy();
    expect(screen.queryByTestId("idea-row-i3")).toBeNull();
  });

  it("searches ideas through the backend (idea_search)", async () => {
    renderPage();
    await screen.findByTestId("idea-row-i1");
    fireEvent.change(screen.getByRole("textbox", { name: "Search ideas" }), {
      target: { value: "Brevo" },
    });
    // The debounced backend search returns only i3; the non-matching ideas drop out (wait for the
    // debounce + re-pull to settle by watching a non-matching row disappear).
    await waitFor(() => expect(screen.queryByTestId("idea-row-i1")).toBeNull());
    expect(screen.getByTestId("idea-row-i3")).toBeTruthy();
  });

  it("narrows to a single theme via the facet filter", async () => {
    renderPage();
    await screen.findByTestId("idea-row-i1");
    const facet = screen.getByRole("group", { name: "Filter by theme" });
    fireEvent.click(within(facet).getByRole("button", { name: /Mail/ }));
    // Only the Mail group's row stays; the RBAC rows drop out.
    await waitFor(() => expect(screen.queryByTestId("idea-row-i1")).toBeNull());
    expect(screen.getByTestId("idea-row-i3")).toBeTruthy();
  });

  it("opens the idea detail drawer (body + linked specs)", async () => {
    renderPage();
    fireEvent.click(await screen.findByTestId("idea-row-i1"));
    const drawer = await screen.findByRole("complementary", { name: "Idea detail" });
    expect(within(drawer).getByRole("heading", { name: /RBAC public decorator/ })).toBeTruthy();
    // The body ("corps") + the linked Spec with its status ("Specs liées").
    expect(within(drawer).getByText(/unify the public whitelist/)).toBeTruthy();
    expect(within(drawer).getByText("RBAC ph2")).toBeTruthy();
  });

  it("shows the closure reason for a closed idea", async () => {
    renderPage();
    // i4 is `implemented` → hidden by the default Active filter; reveal it via All first.
    await screen.findByTestId("idea-row-i1");
    fireEvent.click(screen.getByRole("button", { name: "All" }));
    fireEvent.click(await screen.findByTestId("idea-row-i4"));
    const drawer = await screen.findByRole("complementary", { name: "Idea detail" });
    expect(within(drawer).getByText(/Closure reason/i)).toBeTruthy();
    expect(within(drawer).getByText(/implemented via ThemeService/)).toBeTruthy();
  });

  it("defaults the status filter to the active backlog (open + planned only)", async () => {
    renderPage();
    // Open (i1, i3) + planned (i2) are visible on load; implemented (i4) + closed (i5) are hidden.
    await screen.findByTestId("idea-row-i1");
    expect(screen.getByTestId("idea-row-i2")).toBeTruthy();
    expect(screen.getByTestId("idea-row-i3")).toBeTruthy();
    expect(screen.queryByTestId("idea-row-i4")).toBeNull();
    expect(screen.queryByTestId("idea-row-i5")).toBeNull();
    // Done/Closed/Dropped/All stay reachable — All reveals the terminal ideas.
    fireEvent.click(screen.getByRole("button", { name: "All" }));
    expect(await screen.findByTestId("idea-row-i4")).toBeTruthy();
    expect(screen.getByTestId("idea-row-i5")).toBeTruthy();
  });

  it("has a Closed filter segment that reveals manually-closed ideas with a Closed badge", async () => {
    renderPage();
    await screen.findByTestId("idea-row-i1");
    // `closed` is terminal → hidden under the default Active filter.
    expect(screen.queryByTestId("idea-row-i5")).toBeNull();
    // The dedicated Closed segment reveals it, badged "Closed" (distinct from Done/implemented).
    fireEvent.click(screen.getByRole("button", { name: "Closed" }));
    const row = await screen.findByTestId("idea-row-i5");
    expect(within(row).getByText("Closed")).toBeTruthy();
    // The implemented idea (i4) is NOT shown under the Closed segment — the two terminals are distinct.
    expect(screen.queryByTestId("idea-row-i4")).toBeNull();
  });

  it("renders a row with no leading status dot (status only via the right badge)", async () => {
    renderPage();
    const row = await screen.findByTestId("idea-row-i1");
    // Grouped mode: the FIRST child of the row is the title — no leading status dot precedes it.
    expect(row.firstElementChild?.textContent).toBe("RBAC public decorator");
    // The status is still encoded, once, by the right-side badge.
    expect(within(row).getByText("Open")).toBeTruthy();
  });

  it("toggles between grouped and flat layout, and persists the choice", async () => {
    const first = renderPage();
    // Grouped by default: theme group headers (aria-expanded) are present.
    expect(await screen.findByRole("button", { name: /RBAC/, expanded: true })).toBeTruthy();
    // Switch to flat: no group headers, and each row gains a Theme cell.
    fireEvent.click(screen.getByRole("button", { name: "Flat list" }));
    await waitFor(() =>
      expect(screen.queryByRole("button", { name: /RBAC/, expanded: true })).toBeNull(),
    );
    const row = screen.getByTestId("idea-row-i1");
    expect(within(row).getByText("RBAC")).toBeTruthy(); // the flat-mode Theme cell
    expect(localStorage.getItem("nyx.knowledge.ideas.layout")).toBe("flat");
    // The choice persists across a remount (the toolbar state survives a reload).
    first.unmount();
    renderPage();
    await screen.findByTestId("idea-row-i1");
    expect(screen.queryByRole("button", { name: /RBAC/, expanded: true })).toBeNull();
  });

  it("navigates to the Spec detail when a linked spec is clicked in the drawer", async () => {
    const { router } = renderPage();
    fireEvent.click(await screen.findByTestId("idea-row-i1"));
    const drawer = await screen.findByRole("complementary", { name: "Idea detail" });
    const link = within(drawer).getByRole("link", { name: /RBAC ph2/ });
    // The linked-spec row is a router Link onto the Spec's detail (the dead ↗ is now live).
    expect(link.getAttribute("href")).toBe("/work/s1");
    fireEvent.click(link);
    await waitFor(() => expect(router.state.location.pathname).toBe("/work/s1"));
  });

  it("animates the list↔drawer split via a CSS grid column (not a width-animated drawer)", async () => {
    renderPage();
    const row = await screen.findByTestId("idea-row-i1");
    // The split is a CSS grid transitioning its COLUMNS — the two tracks always sum to the container,
    // so no child can overflow horizontally on open OR close (structural, not overflow-chasing).
    const split = row.closest(".overflow-y-auto")?.parentElement;
    expect(split?.className).toContain("grid");
    expect(split?.className).toContain("transition-[grid-template-columns]");
    expect(split?.className).toContain("motion-reduce:transition-none"); // reduced motion = instant
    // Closed: the drawer track is 0 (its content, if any, is clipped by the cell's overflow-hidden).
    expect(split?.className).toContain("grid-cols-[minmax(0,1fr)_0rem]");

    // Selecting an idea opens the drawer track to its clamped target width — the push reveal.
    fireEvent.click(row);
    const drawer = await screen.findByRole("complementary", { name: "Idea detail" });
    expect(split?.className).toContain("grid-cols-[minmax(0,1fr)_clamp(30rem,45vw,42rem)]");
    // The drawer content sits at a FIXED inner width so it never squishes as the column animates, in
    // a cell that clips the overflow — the reveal is the grid track, not a width/opacity tween.
    expect(drawer.parentElement?.className).toContain("min-w-[clamp(30rem,45vw,42rem)]");
    expect(drawer.parentElement?.parentElement?.className).toContain("overflow-hidden");
  });

  it("keeps the KnowledgePage ROOT clipped on the X axis (belt-and-suspenders under the grid split)", async () => {
    renderPage();
    const heading = await screen.findByRole("heading", { name: "Ideas" });
    // The grid split already makes horizontal overflow impossible; the page root keeps `overflow-x-
    // clip` anyway as a cheap guard — no scroll container, `overflow-y` stays visible so the `h-app`
    // layout is untouched, so no ancestor can ever receive a stray horizontal overflow.
    const pageRoot = heading.closest(".overflow-x-clip");
    expect(pageRoot).not.toBeNull();
    expect(pageRoot?.className).toContain("h-app");
  });

  it("clips the list column horizontally so a narrowed column never shows its own scrollbar", async () => {
    renderPage();
    const row = await screen.findByTestId("idea-row-i1");
    // The scrolling list column keeps its VERTICAL scroll but clips the HORIZONTAL axis so the
    // `shrink-0` row badges never flash a COLUMN-level bar as the grid column narrows the list.
    const listColumn = row.closest(".overflow-y-auto");
    expect(listColumn?.className).toContain("overflow-x-hidden");
  });

  it("uses design tokens only (no hardcoded colour)", async () => {
    const { container } = renderPage();
    await screen.findByRole("heading", { name: "Ideas" });
    expect(container.innerHTML).not.toMatch(/#[0-9a-fA-F]{3,6}\b|rgba?\(/);
  });
});
