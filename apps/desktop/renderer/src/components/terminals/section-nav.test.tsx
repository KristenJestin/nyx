import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { RouterProvider } from "@tanstack/react-router";
import { beforeEach, describe, expect, it } from "vitest";

import { mockIPC } from "@/bridge/test-harness";
import { createAppRouter } from "@/router";
import type { ProjectRecord, WorkspaceRecord } from "@/components/sidebar/use-projects";

/**
 * THE SECTION-NAV IS DERIVED FROM THE SCOPE (H2 + H5).
 *
 * Six entries inside a project — Home · Terminals · Work · Ideas · Engine · Design — and THREE
 * across every project: Home · Work · Ideas. The three others are ABSENT FROM THE DOM, not greyed
 * and not present-and-refusing: `Terminals`, `Engine` and `Design` have no cross-project reading
 * that means anything, and an entry one can click into a lie is worse than no entry. So this file
 * asserts by ABSENCE, never by a disabled attribute.
 *
 * `Design` is the entry that came UP from the foot of the rail, under its old label `Catalogue`;
 * `Ideas` is the label of the `/knowledge` route (the route is unchanged — see
 * `docs/product/README.md`). Both are checked here because both are what the rail SAYS.
 */

/** The rail entries of each scope, in the order the nav lists them. */
const PROJECT_NAV = ["Home", "Terminals", "Work", "Ideas", "Engine", "Design"] as const;
const GLOBAL_NAV = ["Home", "Work", "Ideas"] as const;
/** The three the global scope drops. */
const PROJECT_ONLY = ["Terminals", "Engine", "Design"] as const;

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
        case "system_scope_detail":
          // The Système singleton is ABSENT on this fixture — the documented `null` answer of a
          // database with no scope. The rail then renders no tile, which is what these cases are
          // about; the catch-all below answers `[]`, and `[]` is not a `{project, workspace, home}`.
          return null;
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

/** The labels of the section-nav, in DOM order — the list the `Home` entry's own `<ul>` holds. */
function navLabels(): string[] {
  const list = screen.getByRole("link", { name: "Home" }).closest("ul")!;
  return [...list.querySelectorAll<HTMLElement>("a")].map(
    (a) => a.getAttribute("aria-label") ?? "",
  );
}

describe("The section-nav follows the scope", () => {
  beforeEach(() => {
    installBackend();
  });

  it("carries SIX entries inside a project, Design last", async () => {
    mount("/");
    await screen.findByRole("link", { name: "Home" });
    expect(navLabels()).toEqual([...PROJECT_NAV]);
    // `Design` kept the `/design` route it had at the foot — only the label and the place moved.
    expect(screen.getByRole("link", { name: "Design" }).getAttribute("href")).toBe("/design");
    // `Ideas` is the label of the UNCHANGED `/knowledge` route.
    expect(screen.getByRole("link", { name: "Ideas" }).getAttribute("href")).toBe("/knowledge");
  });

  it("carries THREE across every project — the other three are ABSENT from the DOM", async () => {
    const { container } = mount("/home?scope=all");
    await screen.findByRole("link", { name: "Home" });
    await waitFor(() => expect(navLabels()).toEqual([...GLOBAL_NAV]));
    // ABSENT, not disabled: there is no node to grey out, and no route left pointing at them.
    for (const label of PROJECT_ONLY) {
      expect(screen.queryByRole("link", { name: label })).toBeNull();
    }
    for (const href of ["/", "/engine", "/design"]) {
      expect(container.querySelector(`aside a[href="${href}"]`)).toBeNull();
    }
  });

  /**
   * H7 — ONE NAME IN THE UI, the route untouched underneath.
   *
   * `Ideas` is what the rail says and what the page's own heading says; `Knowledge` is the
   * GLOSSARY's umbrella (the project knowledge covers the ideas AND the design system) and stays
   * the internal route name. The router runs on a memory history, so that path is never on screen
   * — the whole cost of keeping it is this test and the glossary line it points at
   * (`docs/product/glossary.md`).
   */
  it("names the page `Ideas` and leaves the `/knowledge` route alone", async () => {
    const { container } = mount("/knowledge");
    // The rail entry, and the route it still points at.
    const ideas = await screen.findByRole("link", { name: "Ideas" });
    expect(ideas.getAttribute("href")).toBe("/knowledge");
    expect(screen.queryByRole("link", { name: "Knowledge" })).toBeNull();
    // The page itself: its heading and its breadcrumb both say `Ideas`.
    expect(await screen.findByRole("heading", { name: "Ideas" })).toBeTruthy();
    expect(container.textContent).not.toMatch(/knowledge/i);
  });

  it("changes LIVE in both directions, with nothing reloaded", async () => {
    mount("/");
    await screen.findByRole("link", { name: "Home" });
    expect(navLabels()).toEqual([...PROJECT_NAV]);

    // Into the cross-project scope — the same mounted app, the nav simply shortens.
    fireEvent.click(screen.getByRole("link", { name: "All projects" }));
    await waitFor(() => expect(navLabels()).toEqual([...GLOBAL_NAV]));

    // …and back out of it, by the ONE gesture that leaves the mode: entering a project.
    fireEvent.click(screen.getByRole("button", { name: "Project Beta" }));
    await waitFor(() => expect(navLabels()).toEqual([...PROJECT_NAV]));
  });
});
