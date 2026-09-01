import { render, screen, waitFor } from "@testing-library/react";
import { RouterProvider } from "@tanstack/react-router";
import { describe, expect, it } from "vitest";

import { mockIPC } from "@/bridge/test-harness";
import { createAppRouter } from "@/router";
import type { HomeSnapshot } from "@/components/home/types";
import type { ProjectRecord, WorkspaceRecord } from "@/components/sidebar/use-projects";

/**
 * THE `All projects` TILE'S DOT (H1) — one signal, no number, and a rollup that misses nothing.
 *
 * The tile keeps its place at the head of the project list and carries a state dot whose rollup
 * spans EVERY workspace. The load-bearing part is WHICH READ it is built on: the rail's project
 * tiles come from `list_projects` / `list_workspaces`, which serve the USER's projects — a rollup
 * summed over those tiles would silently skip the `system` project, and the cross-project view is
 * exactly where the user refuses to miss it. The dot reads the GLOBAL `home_snapshot` instead,
 * which joins terminals → workspaces → projects with no filter of any kind.
 *
 * So this file makes the two disagree on purpose: `list_projects` returns Alpha and Beta, while
 * `home_snapshot` also carries a row belonging to a project the rail cannot see (`Système`). If
 * that row lights the tile, the rollup is built on the read that sees everything.
 *
 * And NO TILE OF THE RAIL RENDERS A DIGIT: the dot already ranks what matters, so a counter beside
 * it would duplicate the signal. That is asserted by sweeping the whole rail's text.
 */

const EMPTY: HomeSnapshot = { sessions: [], services: [], specs: [], terminals: [] };

/** The `Système` project — present in the home's read model, ABSENT from the rail's own tree. */
const SYSTEM_PROJECT = {
  id: "p-system",
  name: "Système",
  accent: null,
  icon: null,
  image_path: null,
};

/** A snapshot whose ONLY attention-worthy row belongs to `Système`. */
const SYSTEM_ONLY: HomeSnapshot = {
  ...EMPTY,
  sessions: [
    {
      session_id: "s-sys",
      terminal_id: "t-sys",
      agent_kind: "claude_code",
      // A human on the critical path — the top of `STATE_PRIORITY`, so the tile must go yellow.
      state: "waiting",
      secondary: "Can I write this file?",
      label: null,
      cwd: "/system",
      started_at: 0,
      workspace: { id: "ws-system", name: "system", branch: null },
      project: SYSTEM_PROJECT,
    },
  ],
};

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
 * The rail's project tree is Alpha + Beta — the USER's projects, all `list_projects` ever returns.
 * `home_snapshot` answers whatever the caller passes, which is how a project outside that tree
 * gets into the picture.
 */
function installBackend(snapshot: HomeSnapshot) {
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
          // The GLOBAL read (no `projectId`) is the one the tile makes.
          return a.projectId === undefined ? snapshot : EMPTY;
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

/** The `All projects` tile's own `<li>` — where its badge lives. */
function allProjectsTile(): HTMLElement {
  return screen.getByRole("link", { name: "All projects" }).closest("li")!;
}

/** The dot's state inside an element, or `null` when it renders none. */
function dotState(el: HTMLElement): string | null {
  return el.querySelector<HTMLElement>("[data-state]")?.dataset.state ?? null;
}

describe("The `All projects` tile's dot", () => {
  it("rides its OWN region at the head of the rail, above the project list", async () => {
    installBackend(EMPTY);
    mount();
    const alpha = await screen.findByRole("button", { name: "Project Alpha" });
    const tile = allProjectsTile();
    // Alone in its list — the pinned region (review `01M1EJK7VF9WG86V85AJ5K8JFC`)…
    expect(Array.from(tile.parentElement!.children)).toEqual([tile]);
    // …and still the FIRST thing of the rail, above the projects.
    expect(tile.compareDocumentPosition(alpha) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });

  it("draws NOTHING while every project is quiet", async () => {
    installBackend(EMPTY);
    mount();
    await screen.findByRole("button", { name: "Project Alpha" });
    await waitFor(() => expect(dotState(allProjectsTile())).toBeNull());
  });

  it("lights on a `Système` workspace the rail's own project list never returns", async () => {
    installBackend(SYSTEM_ONLY);
    mount();
    // The rail knows two projects, and `Système` is not one of them…
    await screen.findByRole("button", { name: "Project Alpha" });
    expect(screen.queryByRole("button", { name: "Project Système" })).toBeNull();
    // …yet its waiting agent is what the cross-project tile reports.
    await waitFor(() => expect(dotState(allProjectsTile())).toBe("waiting"));
  });

  it("carries NO number — and neither does any other tile of the rail", async () => {
    installBackend(SYSTEM_ONLY);
    const { container } = mount();
    await screen.findByRole("button", { name: "Project Alpha" });
    await waitFor(() => expect(dotState(allProjectsTile())).toBe("waiting"));
    // Sweep the WHOLE rail: a badge counter anywhere would leave a digit in its text.
    const rail = container.querySelector("aside")!;
    expect(rail.textContent ?? "").not.toMatch(/[0-9]/);
  });
});
