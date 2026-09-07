import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { RouterProvider } from "@tanstack/react-router";
import { describe, expect, it } from "vitest";

import { mockIPC } from "@/bridge/test-harness";
import { createAppRouter } from "@/router";
import type { ProjectRecord, WorkspaceRecord } from "@/components/sidebar/use-projects";

/**
 * THE RAIL'S ACTIVITY DOTS — one rule, and it is about the SCREEN, not about the selection.
 *
 * A tile's dot folds its whole runtime to one state and shows it as a corner badge, unless the
 * user is already LOOKING at that runtime — in which case the rows on screen say it better and the
 * badge would only repeat them.
 *
 * ## What this file exists to prevent
 *
 * The mask used to be `active`: "is this the ENTERED project". That is a property of the RAIL, and
 * the entered project keeps it in every scope — so the moment one opened `All projects` or Système,
 * the single project whose activity one most wants signalled went permanently dark. One could sit
 * on the host's terminals for an hour with an agent of the project one had just left blocked on a
 * question, and the rail would say nothing. The mask is the honest question now: this tile's own
 * detail is on screen when the app is in ITS scope AND the route is one that shows it.
 *
 * ## And the `All projects` tile has NO dot at all
 *
 * It had one — a rollup over the global `home_snapshot`, on the reasoning that it is the only read
 * that never misses a project the rail did not load. Two problems, both fatal: it aggregates every
 * project of the install, so on any working machine it is permanently lit and therefore ranks
 * nothing; and with the per-tile dots no longer masked outside their own screen, it says a second
 * time — anonymously — what the project tiles already say by name. `all-projects-dot.test.tsx`
 * left with it.
 */

/** A terminal whose foreground command is RUNNING — the simplest non-quiet runtime there is. */
function busyTerminal(workspaceId: string) {
  return {
    id: `t-${workspaceId}`,
    cwd: "/",
    label: null,
    workspace_id: workspaceId,
    status: "alive" as const,
    exec_state: "idle" as const,
    exec_state_unread: false,
    // The OS busy bit — `terminalRunState` folds it to `running`, which `isQuietState` does NOT
    // hide. No agent, no notification: the dot here is purely "something is running".
    busy: true,
    sessions: [],
  };
}

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

/**
 * Two user projects (Alpha, Beta) and the Système singleton, ALL BUSY — so every tile of the rail
 * has something to say and the only thing under test is which of them is allowed to say it.
 * `list_projects` answers user projects only, faithfully to `db::list_projects`' `kind = 'user'`
 * filter: the singleton reaches the rail through `system_scope_detail` alone.
 */
function installBackend() {
  mockIPC(
    (cmd, args) => {
      const a = (args ?? {}) as Record<string, unknown>;
      switch (cmd) {
        case "list_projects":
          return [project("p1", "Alpha"), project("p2", "Beta")];
        case "list_workspaces":
          return [workspace((a.projectId as string) ?? "p1")];
        case "system_scope_detail":
          return {
            project: { ...project("sys", "System"), kind: "system" },
            workspace: { ...workspace("sys"), name: "system", path: "/home/kris" },
            home: "/home/kris",
          };
        case "workspace_runtime_snapshot": {
          const id = (a.workspaceId as string) ?? "ws-p1";
          return {
            workspace: { id, name: "root", path: "/" },
            terminals: [busyTerminal(id)],
            agent_sessions: [],
            services: [],
          };
        }
        case "home_snapshot":
          return { sessions: [], services: [], specs: [], terminals: [] };
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

/** The dot's state inside an element, or `null` when it renders none. */
function dotState(el: HTMLElement): string | null {
  return el.querySelector<HTMLElement>("[data-state]")?.dataset.state ?? null;
}

/** A project tile's own `<li>` — where its badge lives. */
function projectTile(name: string): HTMLElement {
  return screen.getByRole("button", { name: `Project ${name}` }).closest("li")!;
}

/** The Système tile's `<li>`. */
function systemTile(): HTMLElement {
  return screen.getByRole("button", { name: "System" }).closest("li")!;
}

/** The `All projects` bar's `<li>`. */
function allProjectsTile(): HTMLElement {
  return screen.getByRole("link", { name: "All projects" }).closest("li")!;
}

describe("The rail's activity dots", () => {
  it("masks the ENTERED project's dot on its own screen, and shows every other one", async () => {
    installBackend();
    mount("/");
    const alpha = await screen.findByRole("button", { name: "Project Alpha" });
    await waitFor(() => expect(alpha.getAttribute("aria-current")).toBe("true"));

    // Alpha is entered, the project scope is up and `/` is one of its pages: its runtime is the
    // screen, so the badge would repeat the rows under it.
    await waitFor(() => expect(dotState(projectTile("Beta"))).toBe("running"));
    expect(dotState(projectTile("Alpha"))).toBeNull();
    // The host is not on screen either way — its dot shows.
    expect(dotState(systemTile())).toBe("running");
  });

  it("SHOWS the entered project's dot from the Système scope — the defect this rule fixes", async () => {
    installBackend();
    mount("/");
    const alpha = await screen.findByRole("button", { name: "Project Alpha" });
    await waitFor(() => expect(alpha.getAttribute("aria-current")).toBe("true"));
    await waitFor(() => expect(dotState(projectTile("Alpha"))).toBeNull());

    fireEvent.click(screen.getByRole("button", { name: "System" }));

    // Alpha is STILL the entered project (it is remembered across the scope, which is what makes
    // going back one click) — and that is exactly why the mask could not be `active`. Its runtime
    // is not on screen any more, so it speaks.
    await waitFor(() => expect(dotState(projectTile("Alpha"))).toBe("running"));
    expect(alpha.getAttribute("aria-current")).toBe("true");
    expect(dotState(projectTile("Beta"))).toBe("running");
    // …and the Système tile falls silent, because ITS runtime is now the screen.
    await waitFor(() => expect(dotState(systemTile())).toBeNull());
  });

  it("SHOWS every project's dot in the cross-project scope", async () => {
    installBackend();
    mount("/");
    const alpha = await screen.findByRole("button", { name: "Project Alpha" });
    await waitFor(() => expect(alpha.getAttribute("aria-current")).toBe("true"));

    fireEvent.click(screen.getByRole("link", { name: "All projects" }));

    // The cross-project home lists everything, but it is not a project's runtime page: no tile's
    // detail is on screen, so no tile is masked.
    await waitFor(() => expect(dotState(projectTile("Alpha"))).toBe("running"));
    expect(dotState(projectTile("Beta"))).toBe("running");
    expect(dotState(systemTile())).toBe("running");
  });

  it("gives the `All projects` bar NO dot — even with every project busy", async () => {
    installBackend();
    mount("/");
    await screen.findByRole("button", { name: "Project Alpha" });
    // Every other tile of the rail is lit, so this is an absence and not an empty fixture.
    await waitFor(() => expect(dotState(projectTile("Beta"))).toBe("running"));
    expect(dotState(allProjectsTile())).toBeNull();

    // …and it stays absent in the scope it marks, where a cross-project rollup would have been at
    // its most tempting.
    fireEvent.click(screen.getByRole("link", { name: "All projects" }));
    await waitFor(() => expect(dotState(projectTile("Alpha"))).toBe("running"));
    expect(dotState(allProjectsTile())).toBeNull();
  });

  it("carries NO number, anywhere on the rail", async () => {
    installBackend();
    const { container } = mount("/");
    await screen.findByRole("button", { name: "Project Alpha" });
    await waitFor(() => expect(dotState(projectTile("Beta"))).toBe("running"));
    // Sweep the WHOLE rail: a badge counter anywhere would leave a digit in its text. The dot
    // already ranks what matters (`STATE_PRIORITY`); a count beside it duplicates the signal
    // (user decision H1).
    const rail = container.querySelector("aside")!;
    expect(rail.textContent ?? "").not.toMatch(/[0-9]/);
  });
});
