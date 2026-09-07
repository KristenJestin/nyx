import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { RouterProvider } from "@tanstack/react-router";
import { beforeEach, describe, expect, it } from "vitest";

import { mockIPC } from "@/bridge/test-harness";
import { createAppRouter } from "@/router";
import type { ProjectRecord, WorkspaceRecord } from "@/components/sidebar/use-projects";

import type { HomeSnapshot } from "./types";

/**
 * CROSS-PROJECT FOCUS — clicking a session of ANOTHER project must land on that session.
 *
 * The whole file exists for one trap, named in the PRD and traced to review
 * `01M069MWRNR1PEZZW7YTPAHSKK`: the rail's `enterProject` navigates to `/` with `search: {}`,
 * because a deep-link left in the url belongs to the project one is LEAVING. Routed through it,
 * the home's `?terminal=<id>` would be ERASED by the very navigation meant to carry it, and the
 * user would land on the entered project's default deck instead of on the session they clicked.
 *
 * So this mounts the REAL shell and the REAL Terminals page — the landing is a collaboration
 * between the two — and asserts ALL THREE halves of the landing: the project is entered, the
 * cross-project SCOPE is left with it, and the deep-link survives both. The scope half is the
 * user's reversal of the foundation pass's scope-neutral hook (review
 * `01M1ECXAXVGHJWPWA019QAER6G`); it is asserted HERE because it has to hold WITHOUT the search
 * being cleared, which is the whole trap. The last test pins the contrast: the rail's own path
 * still clears the search, on purpose.
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

/** Alpha owns one shell; Beta owns the terminal the blocked agent sits on. */
const TERMINALS: Record<string, string[]> = {
  "ws-p1": ["p1-term"],
  "ws-p2": ["p2-term"],
};

/** The GLOBAL home: one session, and it belongs to Beta — the project we are NOT in. */
const GLOBAL_SNAPSHOT: HomeSnapshot = {
  sessions: [
    {
      session_id: "s1",
      terminal_id: "p2-term",
      agent_kind: "claude_code",
      state: "waiting",
      secondary: "Fold phases 2 and 3 together?",
      label: "beta agent",
      cwd: "/ws-p2",
      started_at: 0,
      workspace: { id: "ws-p2", name: "root", branch: null },
      project: {
        id: "p2",
        name: "Beta",
        kind: "user" as const,
        accent: null,
        icon: null,
        image_path: null,
      },
    },
  ],
  services: [],
  specs: [],
  terminals: [],
};

function installBackend() {
  mockIPC(
    (cmd, args) => {
      const a = (args ?? {}) as Record<string, unknown>;
      switch (cmd) {
        case "list_projects":
          return [project("p1", "Alpha"), project("p2", "Beta")];
        case "list_workspaces":
          return [workspace(`ws-${a.projectId as string}`, a.projectId as string)];
        case "home_snapshot":
          return GLOBAL_SNAPSHOT;
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

function mountGlobalHome() {
  const router = createAppRouter(["/home?scope=all"]);
  return { ...render(<RouterProvider router={router} />), router };
}

describe("Cross-project focus from the global home", () => {
  beforeEach(() => {
    installBackend();
  });

  it("enters the session's project AND carries the deep-link — the search is NOT erased", async () => {
    const { container, router } = mountGlobalHome();
    // We start inside Alpha (the rail enters the first project) and the blocked session is Beta's.
    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "Project Alpha" }).getAttribute("aria-current"),
      ).toBe("true"),
    );
    const row = await screen.findByTestId("home-session-s1");

    fireEvent.click(row);

    await waitFor(() => expect(router.state.location.pathname).toBe("/"));
    // THE TRAP: `search: {}` would have erased this, and the page would have waited forever for a
    // target it was never told about.
    expect(router.state.location.search).toEqual({ terminal: "p2-term" });
    // Beta is the entered project…
    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "Project Beta" }).getAttribute("aria-current"),
      ).toBe("true"),
    );
    // …and we have LEFT the cross-project scope with it (user ruling, review
    // `01M1ECXAXVGHJWPWA019QAER6G`): the rail says we are INSIDE Beta — its tile wears the entered
    // ring — and the `All projects` tile has gone dark. Clicking a row IS entering its project.
    await waitFor(() =>
      expect(
        screen
          .getByRole("link", { name: "All projects" })
          .closest("li")!
          .getAttribute("data-active"),
      ).toBe("false"),
    );
    expect(screen.getByRole("button", { name: "Project Beta" }).className).toContain("ring-2");
    // …and the clicked session's terminal is the pane that actually has the focus.
    await waitFor(() =>
      expect(
        container.querySelector('[data-terminal-id="p2-term"]')?.getAttribute("data-focused"),
      ).toBe("true"),
    );
  });

  it("leaves the entered project alone for a row that has none (a loose terminal)", async () => {
    mockIPC(
      (cmd, args) => {
        if (cmd === "home_snapshot") {
          return {
            ...GLOBAL_SNAPSHOT,
            sessions: [{ ...GLOBAL_SNAPSHOT.sessions[0]!, workspace: null, project: null }],
          };
        }
        const a = (args ?? {}) as Record<string, unknown>;
        if (cmd === "list_projects") return [project("p1", "Alpha"), project("p2", "Beta")];
        if (cmd === "list_workspaces")
          return [workspace(`ws-${a.projectId as string}`, a.projectId as string)];
        if (cmd === "workspace_runtime_snapshot")
          return {
            workspace: { id: a.workspaceId as string, name: "root", path: "/" },
            terminals: [],
            agent_sessions: [],
            services: [],
          };
        if (cmd === "pty_spawn") return 100;
        // The Système singleton is ABSENT here — the documented `null` of a database with no scope.
        // The catch-all below answers `[]`, and `[]` is not a `{project, workspace, home}`: the rail
        // would dereference it and the shell error boundary would swallow the screen.
        if (cmd === "system_scope_detail") return null;
        return [];
      },
      { shouldMockEvents: true },
    );
    const { router } = mountGlobalHome();
    fireEvent.click(await screen.findByTestId("home-session-s1"));

    await waitFor(() => expect(router.state.location.pathname).toBe("/"));
    expect(router.state.location.search).toEqual({ terminal: "p2-term" });
    // Alpha is still the entered project: a row with no project asks for no switch.
    expect(screen.getByRole("button", { name: "Project Alpha" }).getAttribute("aria-current")).toBe(
      "true",
    );
    // …and the SCOPE is untouched too, which is the decided answer for this case: there is no
    // project to enter, so there is nothing to leave the transverse view for.
    expect(
      screen.getByRole("link", { name: "All projects" }).closest("li")!.getAttribute("data-active"),
    ).toBe("true");
  });

  it("takes a SERVICE to its live output pane, and a bare TERMINAL to its deck", async () => {
    mockIPC(
      (cmd, args) => {
        const a = (args ?? {}) as Record<string, unknown>;
        switch (cmd) {
          case "list_projects":
            return [project("p1", "Alpha")];
          case "list_workspaces":
            return [workspace("ws-p1", "p1")];
          case "home_snapshot":
            return {
              sessions: [],
              services: [
                {
                  instance_id: "inst-1",
                  name: "renderer dev",
                  run_state: "running",
                  exit_code: null,
                  unread: false,
                  workspace: { id: "ws-p1", name: "root", branch: null },
                  project: {
                    id: "p1",
                    name: "Alpha",
                    kind: "user" as const,
                    accent: null,
                    icon: null,
                    image_path: null,
                  },
                },
              ],
              specs: [
                {
                  spec_id: "sp1",
                  key: "ALP-1",
                  title: "A spec",
                  status: "in_progress",
                  updated_at: 0,
                  tasks_done: 1,
                  tasks_total: 2,
                  phase: null,
                  project: {
                    id: "p1",
                    name: "Alpha",
                    kind: "user" as const,
                    accent: null,
                    icon: null,
                    image_path: null,
                  },
                },
              ],
              terminals: [
                {
                  terminal_id: "p1-term",
                  label: "p1-term",
                  cwd: "/ws-p1",
                  exec_state: "idle",
                  exec_state_unread: false,
                  workspace: { id: "ws-p1", name: "root", branch: null },
                  project: {
                    id: "p1",
                    name: "Alpha",
                    kind: "user" as const,
                    accent: null,
                    icon: null,
                    image_path: null,
                  },
                },
              ],
            };
          case "workspace_runtime_snapshot":
            return {
              workspace: { id: (a.workspaceId as string) ?? "ws-p1", name: "root", path: "/" },
              terminals: [],
              agent_sessions: [],
              services: [],
            };
          case "pty_spawn":
            return 100;
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
    const router = createAppRouter(["/home"]);
    render(<RouterProvider router={router} />);

    // A SPEC card is a plain router link onto its detail — no focus dance needed.
    const card = await screen.findByTestId("home-spec-sp1");
    expect(card.getAttribute("href")).toBe("/work/sp1");

    // A SERVICE opens its live output pane through the `?command=` deep-link.
    fireEvent.click(screen.getByTestId("home-service-inst-1"));
    await waitFor(() => expect(router.state.location.pathname).toBe("/"));
    expect(router.state.location.search).toEqual({ command: "inst-1" });

    // …and a bare TERMINAL through the `?terminal=` one.
    void router.navigate({ to: "/home", search: {} });
    fireEvent.click(await screen.findByTestId("home-terminal-p1-term"));
    await waitFor(() => expect(router.state.location.search).toEqual({ terminal: "p1-term" }));
  });

  it("CONTRAST: the rail's own project switch still clears the search, which is why the home cannot use it", async () => {
    const { router } = mountGlobalHome();
    fireEvent.click(await screen.findByTestId("home-session-s1"));
    await waitFor(() => expect(router.state.location.search).toEqual({ terminal: "p2-term" }));

    // Now switch project THROUGH THE RAIL — the path the home deliberately does not take.
    fireEvent.click(screen.getByRole("button", { name: "Project Alpha" }));

    await waitFor(() => expect(router.state.location.search).toEqual({}));
  });
});

/**
 * THE SYSTÈME ROW — the same landing, one scope over, and the bug that made this scope necessary.
 *
 * The cross-project home lists the Système singleton's terminals and services: `home_snapshot`'s
 * global read joins with NO filter, deliberately — the host is exactly what one does not want to
 * miss from a transverse view. Clicking such a row used to pose the singleton as the ENTERED
 * project and navigate to `/`. That project is absent from `list_projects` (`kind = 'user'`), so
 * `<BaseLayout>`'s restore effect saw an entered project that "no longer exists", fell back to the
 * remembered one and ejected the user onto a stranger's Terminals page — the deep-link pointing at
 * a terminal that project will never have.
 *
 * The row now goes where its object lives: the Système SCOPE, at `/system`, deep-link intact. What
 * makes it possible is the backend MARKING the row (`HomeProject.kind`, joined at source) rather
 * than the renderer comparing ids against a second read.
 */
describe("Focus on a SYSTÈME row from the global home", () => {
  const SYSTEM_HOME = "/home/kris";
  const SYSTEM_PROJECT = {
    id: "p-sys",
    name: "System",
    kind: "system" as const,
    accent: null,
    icon: null,
    image_path: null,
  };

  /** A global home whose only rows belong to the singleton: one bare shell, one live service. */
  const SYSTEM_SNAPSHOT: HomeSnapshot = {
    sessions: [],
    services: [
      {
        instance_id: "sys-inst",
        name: "docker",
        run_state: "running",
        exit_code: null,
        unread: false,
        workspace: { id: "ws-sys", name: "system", branch: null },
        project: SYSTEM_PROJECT,
      },
    ],
    specs: [],
    terminals: [
      {
        terminal_id: "sys-term",
        label: "postgres",
        cwd: SYSTEM_HOME,
        exec_state: "idle",
        exec_state_unread: false,
        workspace: { id: "ws-sys", name: "system", branch: null },
        project: SYSTEM_PROJECT,
      },
    ],
  };

  beforeEach(() => {
    mockIPC(
      (cmd, args) => {
        const a = (args ?? {}) as Record<string, unknown>;
        switch (cmd) {
          case "list_projects":
            // USER projects only — the backend contract, reproduced faithfully: the singleton is
            // NOT here, which is the whole reason entering it as a project could not work.
            return [project("p1", "Alpha")];
          case "list_workspaces":
            return [workspace(`ws-${a.projectId as string}`, a.projectId as string)];
          case "home_snapshot":
            return SYSTEM_SNAPSHOT;
          case "system_scope_detail":
            return {
              project: { ...project("p-sys", "System"), kind: "system" },
              workspace: { ...workspace("ws-sys", "p-sys"), name: "system", path: SYSTEM_HOME },
              home: SYSTEM_HOME,
            };
          case "workspace_runtime_snapshot": {
            const wsId = a.workspaceId as string;
            const terminals =
              wsId === "ws-sys"
                ? [
                    {
                      id: "sys-term",
                      cwd: SYSTEM_HOME,
                      label: "postgres",
                      workspace_id: wsId,
                      status: "alive",
                      exec_state: "idle",
                      exec_state_unread: false,
                      sessions: [],
                    },
                  ]
                : [];
            const services =
              wsId === "ws-sys"
                ? [
                    {
                      id: "sys-inst",
                      command_id: "cmd-1",
                      workspace_id: wsId,
                      name: "docker",
                      command: "docker compose up",
                      run_state: "running",
                      exit_code: null,
                      unread: false,
                    },
                  ]
                : [];
            return {
              workspace: { id: wsId, name: "root", path: `/${wsId}` },
              terminals,
              agent_sessions: [],
              services,
            };
          }
          case "pty_spawn":
            return 100;
          default:
            return [];
        }
      },
      { shouldMockEvents: true },
    );
  });

  it("takes a Système TERMINAL to `/system?terminal=`, in the Système scope, deep-link intact", async () => {
    const router = createAppRouter(["/home?scope=all"]);
    render(<RouterProvider router={router} />);
    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "Project Alpha" }).getAttribute("aria-current"),
      ).toBe("true"),
    );

    fireEvent.click(await screen.findByTestId("home-terminal-sys-term"));

    await waitFor(() => expect(router.state.location.pathname).toBe("/system"));
    // The deep-link SURVIVES the scope change — the whole reason this hook does not go through
    // `enterProject`, one scope over.
    expect(router.state.location.search).toEqual({ terminal: "sys-term" });
    // The rail says Système, and NO project is selected…
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "System" }).getAttribute("aria-current")).toBe(
        "true",
      ),
    );
    expect(screen.getByRole("button", { name: "Project Alpha" }).className).not.toContain("ring-2");
    // …while Alpha is still ENTERED, untouched: the singleton is not a project one enters, so the
    // way back stays one click. This is exactly what the old path destroyed.
    expect(screen.getByRole("button", { name: "Project Alpha" }).getAttribute("aria-current")).toBe(
      "true",
    );
  });

  it("takes a Système SERVICE to `/system?command=`, and the page selects it", async () => {
    const router = createAppRouter(["/home?scope=all"]);
    render(<RouterProvider router={router} />);
    await screen.findByRole("button", { name: "System" });

    fireEvent.click(await screen.findByTestId("home-service-sys-inst"));

    await waitFor(() => expect(router.state.location.pathname).toBe("/system"));
    expect(router.state.location.search).toEqual({ command: "sys-inst" });
    // The deep-link is CONSUMED, not merely carried: the service's row is the selected one, which
    // is what puts its live output pane on screen.
    await waitFor(() =>
      expect(
        (screen.getByText("docker").closest("[data-selected]") as HTMLElement | null)?.getAttribute(
          "data-selected",
        ),
      ).toBe("true"),
    );
  });
});
