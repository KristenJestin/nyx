import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";
import { bridgeFake, mockIPC } from "@/bridge/test-harness";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { ProjectRecord, WorkspaceRecord } from "@/components/sidebar/use-projects";
import { ActiveProjectProvider } from "./active-project";
import { BaseLayout } from "./base-layout";
import { saveLastProjectId } from "./ui-persistence";

function project(id: string, name: string): ProjectRecord {
  return { id, name, key: "P", collapsed: false, created_at: 0, updated_at: 0, resume_agent_sessions: false };
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
 * A workspace hosting a live agent session BLOCKED ON THE USER (`waiting`) — the terminal is
 * present in the workspace snapshot (with a live, non-ended agent session) and its runtime
 * activity is `waiting`, served by the agent-activity read model. Used to prove the yellow
 * `waiting` dot is reachable on the rail end-to-end.
 */
interface WaitingAgent {
  workspaceId: string;
  terminalId: string;
}

/**
 * Serve `list_projects` / `list_workspaces` from a fixed set of projects, plus a
 * `workspace_runtime_snapshot` per workspace so the rail's aggregated project dot resolves.
 * `runStateByWorkspace` maps a workspace id → the run_state of a single managed-command
 * service in that workspace's runtime (absent ⇒ an empty, idle runtime). `waitingAgent`
 * (optional) additionally seeds a terminal hosting a live agent session whose runtime
 * activity is `waiting`, served via `agent_active_sessions` + `agent_activity_snapshot`.
 */
function installBackend(
  projects: ProjectRecord[],
  runStateByWorkspace: Record<string, string> = {},
  waitingAgent?: WaitingAgent,
): void {
  mockIPC((cmd, args) => {
    const a = (args ?? {}) as Record<string, unknown>;
    switch (cmd) {
      case "list_projects":
        return projects;
      case "list_workspaces":
        return [root(`ws-${a.projectId as string}`, a.projectId as string)];
      case "workspace_runtime_snapshot": {
        const wsId = a.workspaceId as string;
        const run = runStateByWorkspace[wsId];
        const hostsAgent = waitingAgent?.workspaceId === wsId;
        return {
          workspace: { id: wsId, name: "root", path: `/${wsId}` },
          terminals: hostsAgent
            ? [
                {
                  id: waitingAgent!.terminalId,
                  cwd: `/${wsId}`,
                  label: null,
                  workspace_id: wsId,
                  status: "alive",
                  exec_state: "idle",
                  exec_state_unread: false,
                  // A LIVE (non-ended) agent session so `activeAgentSession` sees it.
                  sessions: [
                    {
                      id: `sess-${waitingAgent!.terminalId}`,
                      terminal_id: waitingAgent!.terminalId,
                      workspace_id: wsId,
                      agent_kind: "claude_code",
                      external_session_id: "ext-1",
                      state: "active",
                    },
                  ],
                },
              ]
            : [],
          agent_sessions: [],
          services: run
            ? [
                {
                  id: `svc-${wsId}`,
                  workspace_id: wsId,
                  name: "dev",
                  command: "npm run dev",
                  run_state: run,
                  exit_code: null,
                  unread: false,
                },
              ]
            : [],
        };
      }
      // The agent-activity read model (`useAgentSessions`): the session-icon map + the
      // runtime activity map. Only populated when a waiting agent is seeded.
      case "agent_active_sessions":
        return waitingAgent
          ? [{ terminal_id: waitingAgent.terminalId, agent_kind: "claude_code" }]
          : [];
      case "agent_activity_snapshot":
        return waitingAgent
          ? [{ terminal_id: waitingAgent.terminalId, activity: "waiting", ready_unread: false }]
          : [];
      default:
        return undefined;
    }
  });
}

/**
 * Mount `<BaseLayout>` inside a real router (memory history) with stub pages for the routes,
 * so the section-nav `<Link>`s and the `<Outlet/>` behave exactly as in the app. A Spec-detail
 * stub (`/work/$specId`) is part of the tree so a test can start on a PROJECT-SCOPED page (the
 * situation the project switch is about) — the ROUTER is returned alongside the render result so
 * the assertion can read the real `pathname`, not just the rail's `aria-current`.
 */
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
    mk("/", "TERMINALS PAGE"),
    mk("/home", "HOME PAGE"),
    mk("/work", "WORK PAGE"),
    mk("/work/$specId", "SPEC PAGE"),
    mk("/knowledge", "KNOWLEDGE PAGE"),
  ]);
  const router = createRouter({
    routeTree,
    history: createMemoryHistory({ initialEntries: [initialEntry] }),
  });
  return { ...render(<RouterProvider router={router} />), router };
}

describe("BaseLayout", () => {
  beforeEach(() => {
    installBackend([project("p1", "Alpha"), project("p2", "Beta")]);
  });

  it("renders the project rail from list_projects with a Project label (never Workspace)", async () => {
    renderLayout();
    // Each project appears as a tile labelled "Project <name>" — never "Workspace".
    expect(await screen.findByRole("button", { name: "Project Alpha" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Project Beta" })).toBeTruthy();
    expect(screen.queryByTitle("Workspace")).toBeNull();
    expect(screen.queryByRole("button", { name: "Add workspace" })).toBeNull();
    // The add affordance is a PROJECT add, not a workspace add.
    expect(screen.getByRole("button", { name: "Add project" })).toBeTruthy();
  });

  it("marks the first project active once the list loads", async () => {
    renderLayout();
    const alpha = await screen.findByRole("button", { name: "Project Alpha" });
    await waitFor(() => expect(alpha.getAttribute("aria-current")).toBe("true"));
    expect(screen.getByRole("button", { name: "Project Beta" }).getAttribute("aria-current")).toBe(
      "false",
    );
  });

  it("enters a project on click (moves the active indicator)", async () => {
    renderLayout();
    const beta = await screen.findByRole("button", { name: "Project Beta" });
    fireEvent.click(beta);
    await waitFor(() => expect(beta.getAttribute("aria-current")).toBe("true"));
    expect(screen.getByRole("button", { name: "Project Alpha" }).getAttribute("aria-current")).toBe(
      "false",
    );
  });

  /**
   * ENTERING A PROJECT IS A NAVIGATION, NOT ONLY A SELECTION (PRD fix 2).
   *
   * The bug: from a page SCOPED to project A (here a Spec detail, `/work/s1`), entering project B
   * from the rail moved the active indicator but LEFT THE VIEW on A's page — one ends up looking
   * at a page that does not belong to the entered project. The cause is `<BaseLayout>`'s tile
   * `onEnter` callback, which only called `setActiveProjectId` and never navigated.
   *
   * So this test asserts the two things SEPARATELY — the selection moved (`aria-current`) AND the
   * ROUTE left the old project's page for the entered project's HOME. Only the second assertion is
   * red before the fix, which is exactly the defect.
   */
  it("entering another project LEAVES the previous project's page for its HOME (route, not just selection)", async () => {
    const { router } = renderLayout("/work/s1");
    const beta = await screen.findByRole("button", { name: "Project Beta" });
    // Starting point: a project-scoped page (a Spec of the project one is currently in).
    expect(router.state.location.pathname).toBe("/work/s1");
    expect(await screen.findByText("SPEC PAGE")).toBeTruthy();

    fireEvent.click(beta);

    // The SELECTION follows the click (this half already worked)…
    await waitFor(() => expect(beta.getAttribute("aria-current")).toBe("true"));
    // …and so does the ROUTE: the view lands on the entered project's home.
    await waitFor(() => expect(router.state.location.pathname).toBe("/home"));
    expect(await screen.findByText("HOME PAGE")).toBeTruthy();
  });

  /**
   * THE SWITCH ALWAYS LANDS ON THE HOME, FROM EVERY PAGE (user feedback: « si je suis sur une autre
   * page et que je change de projet au lieu d'aller dans terminals ca va sur la home »).
   *
   * This test used to assert the opposite — that `/work` and `/knowledge` still landed on `/`,
   * which is what the switch did before the home became the one landing page. The list is the
   * whole section-nav plus the Catalogue at its foot: none of them is an exception, and the landing
   * search is `{}` everywhere so nothing of the page one left rides along. (The `/` case, with its
   * stale-deep-link semantics, is covered by `project-switch.test.tsx` against the real pages.)
   */
  it("lands on the project's HOME from every other page — `/work`, `/knowledge`, `/`", async () => {
    for (const from of ["/work", "/knowledge", "/"]) {
      const { router, unmount } = renderLayout(from);
      const beta = await screen.findByRole("button", { name: "Project Beta" });
      expect(router.state.location.pathname).toBe(from);

      fireEvent.click(beta);

      await waitFor(() => expect(router.state.location.pathname).toBe("/home"));
      expect(router.state.location.search).toEqual({});
      unmount();
    }
  });

  it("renders the section-nav (Terminals / Work / Ideas) and the index page in the Outlet", async () => {
    renderLayout("/");
    expect(await screen.findByRole("link", { name: "Terminals" })).toBeTruthy();
    expect(screen.getByRole("link", { name: "Work" })).toBeTruthy();
    expect(screen.getByRole("link", { name: "Ideas" })).toBeTruthy();
    expect(screen.getByText("TERMINALS PAGE")).toBeTruthy();
  });

  it("navigates between at least two pages via the section-nav", async () => {
    renderLayout("/");
    expect(await screen.findByText("TERMINALS PAGE")).toBeTruthy();
    fireEvent.click(screen.getByRole("link", { name: "Work" }));
    expect(await screen.findByText("WORK PAGE")).toBeTruthy();
  });

  /**
   * DONE-CRITERION (H5) — THE DESIGN ENTRY CAME UP FROM THE FOOT INTO THE SECTION-NAV, ONCE.
   *
   * It used to sit at the foot of the rail under the label `Catalogue` (finding
   * `01KZ41YKMB63GFJ3QMK2ZHJN1C`, since lifted): the foot was for the REFERENCE surfaces,
   * consulted FROM the screen one is on. The design system turned out to be a section of the
   * project like the others, so the entry moved up and took the name the rest of the app uses. The
   * ROUTE is untouched — only the label and the place moved — and it is NOT duplicated: nothing
   * is left between the section-nav and the Settings control.
   */
  it("puts Design LAST in the section-nav — not at the foot, and not twice", async () => {
    renderLayout("/");
    const design = await screen.findByRole("link", { name: "Design" });
    expect(design.getAttribute("href")).toBe("/design");
    // The former label is gone from the rail entirely.
    expect(screen.queryByRole("link", { name: "Catalogue" })).toBeNull();
    // IN the section-nav list, after every other entry — one entry, one list.
    const ideas = screen.getByRole("link", { name: "Ideas" });
    expect(design.closest("ul")).toBe(ideas.closest("ul"));
    expect(screen.getAllByRole("link", { name: "Design" })).toHaveLength(1);
    expect(ideas.compareDocumentPosition(design) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    // …and the Settings control still closes the rail below it.
    const settings = screen.getByRole("button", { name: "Settings" });
    expect(
      design.compareDocumentPosition(settings) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
  });
});

describe("BaseLayout — restores the last entered project", () => {
  it("re-enters the LAST project (persisted) instead of the first on load", async () => {
    installBackend([project("p1", "Alpha"), project("p2", "Beta")]);
    // The user last entered Beta; a relaunch must reopen Beta, not snap to the first (Alpha).
    saveLastProjectId("p2");
    renderLayout();
    const beta = await screen.findByRole("button", { name: "Project Beta" });
    await waitFor(() => expect(beta.getAttribute("aria-current")).toBe("true"));
    expect(screen.getByRole("button", { name: "Project Alpha" }).getAttribute("aria-current")).toBe(
      "false",
    );
  });

  it("falls back to the first project when the remembered project no longer exists", async () => {
    installBackend([project("p1", "Alpha")]);
    // A stale id (the remembered project was deleted while away) falls back cleanly to the first.
    saveLastProjectId("p-deleted-while-away");
    renderLayout();
    const alpha = await screen.findByRole("button", { name: "Project Alpha" });
    await waitFor(() => expect(alpha.getAttribute("aria-current")).toBe("true"));
  });
});

/**
 * The View Transitions API cross-fade on a main-screen change (finding
 * 01KWV88BMDYXR8F0V6TSNFJSQQ). jsdom has NO `document.startViewTransition`, so:
 *   - by DEFAULT (no spy) the section-nav still navigates — the fallback path;
 *   - with a SPY installed, a section-nav click wraps the navigation in it.
 */
interface VTDoc {
  startViewTransition?: (cb: () => void) => unknown;
}
const vtDoc = document as unknown as VTDoc;

describe("BaseLayout — navigation cross-fade (View Transitions API)", () => {
  beforeEach(() => {
    installBackend([project("p1", "Alpha")]);
  });

  it("FALLBACK: navigates normally when startViewTransition is absent (jsdom default)", async () => {
    // The API does not exist in jsdom — the fallback path. Navigation must still work.
    expect(vtDoc.startViewTransition).toBeUndefined();
    renderLayout("/");
    expect(await screen.findByText("TERMINALS PAGE")).toBeTruthy();
    fireEvent.click(screen.getByRole("link", { name: "Ideas" }));
    expect(await screen.findByText("KNOWLEDGE PAGE")).toBeTruthy();
  });

  it("wraps a section-nav navigation in document.startViewTransition when the API exists", async () => {
    // Install a spy that runs its callback synchronously (like the real API's swap).
    const vt = vi.fn((cb: () => void) => {
      cb();
      return {
        finished: Promise.resolve(),
        ready: Promise.resolve(),
        updateCallbackDone: Promise.resolve(),
      };
    });
    vtDoc.startViewTransition = vt as unknown as VTDoc["startViewTransition"];
    try {
      renderLayout("/");
      expect(await screen.findByText("TERMINALS PAGE")).toBeTruthy();
      fireEvent.click(screen.getByRole("link", { name: "Work" }));
      // The transition wrapped the route change, and the new page rendered.
      expect(vt).toHaveBeenCalledTimes(1);
      expect(await screen.findByText("WORK PAGE")).toBeTruthy();
    } finally {
      delete vtDoc.startViewTransition;
    }
  });
});

describe("BaseLayout — + Add project (rail)", () => {
  it("picks a folder, confirms the create modal, and the new project appears in the rail", async () => {
    // Serve the base projects + capture the create_project call; the folder picker returns a
    // path so the create modal opens seeded with the folder basename.
    const createCalls: Record<string, unknown>[] = [];
    mockIPC((cmd, args) => {
      const a = (args ?? {}) as Record<string, unknown>;
      switch (cmd) {
        case "list_projects":
          return [project("p1", "Alpha")];
        case "list_workspaces":
          return [root(`ws-${a.projectId as string}`, a.projectId as string)];
        case "workspace_runtime_snapshot":
          return {
            workspace: { id: a.workspaceId as string, name: "root", path: "/x" },
            terminals: [],
            agent_sessions: [],
            services: [],
          };
        case "create_project": {
          createCalls.push(a);
          const created = project("p-new", (a.name as string) ?? "new-app");
          return { project: created, root: root("ws-p-new", "p-new") };
        }
        default:
          return undefined;
      }
    });
    // The native folder picker resolves to this absolute path (basename → default name).
    bridgeFake().pickedDirectory = "/home/kris/new-app";

    renderLayout();
    const addBtn = await screen.findByRole("button", { name: "Add project" });
    fireEvent.click(addBtn);

    // The create modal opens with the folder basename pre-filled; confirm it.
    const nameField = await screen.findByRole("textbox", { name: /project name/i });
    await waitFor(() => expect((nameField as HTMLInputElement).value).toBe("new-app"));
    const confirm = screen.getByRole("button", { name: "Add project" });
    // (two "Add project" buttons now exist — the rail one and the modal confirm; grab the last)
    const confirmBtns = screen.getAllByRole("button", { name: "Add project" });
    fireEvent.click(confirmBtns[confirmBtns.length - 1] ?? confirm);

    // The backend create ran with the picked path, and the new project shows on the rail.
    await waitFor(() => expect(createCalls.length).toBe(1));
    expect(createCalls[0]!.rootPath).toBe("/home/kris/new-app");
    expect(await screen.findByRole("button", { name: "Project new-app" })).toBeTruthy();
  });

  it("does nothing when the folder picker is cancelled (no modal, no create)", async () => {
    const createCalls: unknown[] = [];
    mockIPC((cmd, args) => {
      const a = (args ?? {}) as Record<string, unknown>;
      switch (cmd) {
        case "list_projects":
          return [project("p1", "Alpha")];
        case "list_workspaces":
          return [root(`ws-${a.projectId as string}`, a.projectId as string)];
        case "workspace_runtime_snapshot":
          return {
            workspace: { id: a.workspaceId as string, name: "root", path: "/x" },
            terminals: [],
            agent_sessions: [],
            services: [],
          };
        case "create_project":
          createCalls.push(a);
          return undefined;
        default:
          return undefined;
      }
    });
    bridgeFake().pickedDirectory = null; // cancelled

    renderLayout();
    const addBtn = await screen.findByRole("button", { name: "Add project" });
    fireEvent.click(addBtn);
    // A cancelled pick opens no modal → the name field never appears, no create fires.
    await new Promise((r) => setTimeout(r, 0));
    expect(screen.queryByRole("textbox", { name: /project name/i })).toBeNull();
    expect(createCalls.length).toBe(0);
    vi.clearAllMocks();
  });
});

describe("BaseLayout — aggregated project dot on the rail", () => {
  it("shows an aggregated run-state dot on a non-entered project with live runtime", async () => {
    // Beta (the non-entered tile — Alpha is auto-entered) has a running service in its
    // workspace, so its rail tile carries a significant aggregated dot.
    installBackend([project("p1", "Alpha"), project("p2", "Beta")], { "ws-p2": "running" });
    renderLayout();
    const beta = await screen.findByRole("button", { name: "Project Beta" });
    const tile = beta.closest("li");
    expect(tile).not.toBeNull();
    await waitFor(() => {
      expect(tile!.querySelector("[data-state]")?.getAttribute("data-state")).toBe("running");
    });
  });

  it("renders NO dot on a non-entered project whose runtime is idle/empty", async () => {
    // Beta has no running services → its aggregated state is idle → the shared quiet-state
    // rule hides the dot (no `[data-state]` in the tile).
    installBackend([project("p1", "Alpha"), project("p2", "Beta")]);
    renderLayout();
    const beta = await screen.findByRole("button", { name: "Project Beta" });
    const tile = beta.closest("li");
    expect(tile).not.toBeNull();
    // Give the runtime hook a chance to resolve, then assert the dot stayed hidden.
    await waitFor(() => expect(beta.getAttribute("aria-current")).toBe("false"));
    expect(tile!.querySelector("[data-state]")).toBeNull();
  });

  it("MASKS the aggregated dot on the ACTIVE/entered project (detail already on screen)", async () => {
    // Alpha is auto-entered (active) and HAS a running service — but because the user is
    // INSIDE it (its workspaces + rows are on screen), the summary dot is masked on the active
    // tile (finding: the dot only summarises the OTHER projects). Beta, non-active with a
    // running service, still shows its dot — proving the mask is scoped to the active tile.
    installBackend([project("p1", "Alpha"), project("p2", "Beta")], {
      "ws-p1": "running",
      "ws-p2": "running",
    });
    renderLayout();
    const alpha = await screen.findByRole("button", { name: "Project Alpha" });
    await waitFor(() => expect(alpha.getAttribute("aria-current")).toBe("true"));
    const beta = screen.getByRole("button", { name: "Project Beta" });
    // The non-active Beta tile shows its dot once the runtime resolves…
    await waitFor(() => {
      expect(beta.closest("li")!.querySelector("[data-state]")).not.toBeNull();
    });
    // …while the active Alpha tile masks its own (no dot despite a running service).
    expect(alpha.closest("li")!.querySelector("[data-state]")).toBeNull();
  });

  it("surfaces a YELLOW (waiting) dot on the rail when a project hosts an agent blocked on the user", async () => {
    // Beta hosts a terminal whose agent is `waiting` (blocked on the user), with NO other
    // runnable state. The rail dot must roll up to `waiting` — the yellow "needs you" dot,
    // reachable ONLY because the agent-activity axis is folded into the rollup.
    installBackend([project("p1", "Alpha"), project("p2", "Beta")], undefined, {
      workspaceId: "ws-p2",
      terminalId: "t-agent",
    });
    renderLayout();
    const beta = await screen.findByRole("button", { name: "Project Beta" });
    const tile = beta.closest("li");
    expect(tile).not.toBeNull();
    await waitFor(() => {
      expect(tile!.querySelector("[data-state]")?.getAttribute("data-state")).toBe("waiting");
    });
  });

  it("pins the aggregated dot to the BOTTOM-RIGHT corner of the tile (badge placement)", async () => {
    installBackend([project("p1", "Alpha"), project("p2", "Beta")], { "ws-p2": "running" });
    renderLayout();
    const beta = await screen.findByRole("button", { name: "Project Beta" });
    const tile = beta.closest("li");
    expect(tile).not.toBeNull();
    await waitFor(() => expect(tile!.querySelector("[data-state]")).not.toBeNull());
    // The dot's badge wrapper is absolutely pinned to the tile's bottom-right corner.
    const dot = tile!.querySelector("[data-state]")!;
    const badge = dot.closest("span.absolute");
    expect(badge).not.toBeNull();
    const cls = badge!.className;
    expect(cls).toContain("absolute");
    expect(cls).toContain("-right-0.5");
    expect(cls).toContain("-bottom-0.5");
  });
});

/**
 * The global Settings entry point rebranched into the active shell (base-layout). The
 * removed `<TerminalManager>` owned the ONLY gear that opened the Integrations tab (where
 * the Claude plugin is installed/uninstalled via `integration_list` / `integration_install`
 * / `integration_remove`); this shell restores it. The gear opens the modal and pulls a
 * fresh integration list on the OPEN EVENT (the dialog's imperative `reload`), exactly as
 * the old wiring did.
 */
describe("BaseLayout — global Settings entry point (Integrations)", () => {
  function installSettingsBackend(calls: string[]): void {
    mockIPC((cmd, args) => {
      const a = (args ?? {}) as Record<string, unknown>;
      calls.push(cmd);
      switch (cmd) {
        case "list_projects":
          return [project("p1", "Alpha")];
        case "list_workspaces":
          return [root(`ws-${a.projectId as string}`, a.projectId as string)];
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
        // The Integrations pane's read model: the single bundled Claude plugin, not installed.
        case "integration_list":
          return [
            { provider: "claude_code", label: "Claude Code", installed: false, available: true },
          ];
        default:
          return undefined;
      }
    });
  }

  it("exposes a Settings gear in the rail that opens the Integrations tab and lists the Claude plugin", async () => {
    const calls: string[] = [];
    installSettingsBackend(calls);
    renderLayout();

    // The entry point is present in the active shell — an accessible Settings gear.
    const gear = await screen.findByRole("button", { name: "Settings" });
    // Not yet loaded: the modal is closed, so no integration list was pulled.
    expect(calls).not.toContain("integration_list");

    fireEvent.click(gear);

    // Opening pulls a fresh list (integration_list) and shows the Claude row with its
    // single Install control — the install/uninstall surface (integration_install/remove).
    expect(await screen.findByText("Claude Code")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Install" })).toBeTruthy();
    await waitFor(() => expect(calls).toContain("integration_list"));
  });
});

/**
 * The per-project settings entry point rebranched into the active shell (base-layout). The
 * removed `<TerminalManager>` owned the ONLY surface that mounted `<ProjectSettingsDialog>`;
 * this shell restores it via a right-click / kebab "Project settings" on the rail tile. The
 * Global pane's `resume_agent_sessions` toggle is wired to `set_project_resume_agent_sessions`
 * — the setting that was previously reachable only through a manual database edit.
 *
 * PRD-B SCOPE: the Global pane now ALSO exposes the project rename (`showRename` →
 * `update_project`) as the rename half of the kebab CRUD — the resume toggle keeps working
 * alongside it.
 */
describe("BaseLayout — project settings entry point (resume toggle)", () => {
  function installResumeBackend(calls: { cmd: string; args: Record<string, unknown> }[]): void {
    mockIPC((cmd, args) => {
      const a = (args ?? {}) as Record<string, unknown>;
      calls.push({ cmd, args: a });
      switch (cmd) {
        case "list_projects":
          return [project("p1", "Alpha")];
        case "list_workspaces":
          return [root(`ws-${a.projectId as string}`, a.projectId as string)];
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
        // The resume opt-in persistence — the command the toggle drives.
        case "set_project_resume_agent_sessions":
          return undefined;
        default:
          return undefined;
      }
    });
  }

  it("right-click a rail tile → Project settings, and the resume toggle persists via set_project_resume_agent_sessions", async () => {
    const calls: { cmd: string; args: Record<string, unknown> }[] = [];
    installResumeBackend(calls);
    renderLayout();

    // The per-project entry point: right-click the tile → a "Project settings" context menu.
    const tile = await screen.findByRole("button", { name: "Project Alpha" });
    fireEvent.contextMenu(tile);
    const item = await screen.findByRole("menuitem", { name: "Project settings for Alpha" });
    fireEvent.click(item);

    // The project-settings modal opens on Global, exposing the resume toggle (off by default).
    const toggle = await screen.findByRole("switch", { name: /resume agent sessions/i });
    expect(toggle).toHaveAttribute("aria-checked", "false");

    // PRD-B: the Global pane now ALSO exposes the project rename (Name field + Save) — the
    // rename half of the kebab CRUD, wired to `update_project`.
    expect(screen.getByLabelText("Project name")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Save" })).toBeTruthy();

    // Flipping it persists immediately via the backend command, scoped to THIS project id.
    fireEvent.click(toggle);
    await waitFor(() =>
      expect(calls.some((c) => c.cmd === "set_project_resume_agent_sessions")).toBe(true),
    );
    const call = calls.find((c) => c.cmd === "set_project_resume_agent_sessions");
    expect(call?.args.id).toBe("p1");
    expect(call?.args.resume).toBe(true);
    // Toggling resume alone never fires a rename (the Name field was left untouched).
    expect(calls.some((c) => c.cmd === "update_project")).toBe(false);
  });
});

/**
 * THE IDENTITY BLOCK, END TO END — the Global pane's dressing gestures, from the press to the
 * command on the bridge and back to the rail tile.
 *
 * This is where "persists immediately AND repaints without a reload" is actually proven: the
 * `<ProjectIdentity>` unit file asserts the callbacks, this one asserts what those callbacks reach
 * (`set_project_accent` / `set_project_mark`, the very writes the MCP tool uses) and what the rail
 * does about it (the optimistic tree update in `useProjects`). The rail tile node is captured
 * BEFORE the modal opens, because the dialog hides the rest of the tree from the role queries
 * while it is up — the node itself stays mounted, which is the whole point.
 */
describe("BaseLayout — project identity (accent, glyph, picture)", () => {
  function installIdentityBackend(calls: { cmd: string; args: Record<string, unknown> }[]): void {
    mockIPC((cmd, args) => {
      const a = (args ?? {}) as Record<string, unknown>;
      calls.push({ cmd, args: a });
      switch (cmd) {
        case "list_projects":
          return [project("p1", "Alpha")];
        case "list_workspaces":
          return [root(`ws-${a.projectId as string}`, a.projectId as string)];
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

  /** Open Global on Alpha and hand back the rail tile's mark node (captured before the modal). */
  async function openIdentity(): Promise<HTMLElement> {
    const tile = await screen.findByRole("button", { name: "Project Alpha" });
    const mark = tile.querySelector("span") as HTMLElement;
    fireEvent.contextMenu(tile);
    fireEvent.click(await screen.findByRole("menuitem", { name: "Project settings for Alpha" }));
    await screen.findByRole("group", { name: "Accent" });
    return mark;
  }

  it("an accent, then a glyph, then a picture — each persists at once and repaints the rail", async () => {
    const calls: { cmd: string; args: Record<string, unknown> }[] = [];
    installIdentityBackend(calls);
    renderLayout();
    const mark = await openIdentity();
    // Undressed: the initial on the neutral tile — what every project looked like before.
    expect(mark).toHaveTextContent("A");
    expect(mark.className).toContain("bg-muted");

    // ACCENT — the token NAME, never a pixel value, and the eraser is not `identity-0`.
    fireEvent.click(screen.getByRole("button", { name: "Accent 3" }));
    await waitFor(() => expect(calls.some((c) => c.cmd === "set_project_accent")).toBe(true));
    expect(calls.find((c) => c.cmd === "set_project_accent")?.args).toEqual({
      id: "p1",
      accent: "identity-3",
    });
    // Repainted with no reload: the neutral tile classes are gone, the accent paints inline.
    await waitFor(() => expect(mark.className).not.toContain("bg-muted"));

    // GLYPH — one write, and it is the one that also clears the picture column.
    fireEvent.click(screen.getByRole("button", { name: "Rocket" }));
    await waitFor(() => expect(calls.some((c) => c.cmd === "set_project_mark")).toBe(true));
    expect(calls.find((c) => c.cmd === "set_project_mark")?.args).toEqual({
      id: "p1",
      icon: "rocket",
    });
    await waitFor(() => expect(mark.querySelector("svg")).not.toBeNull());

    // PICTURE — the COPY's path is what is written, and adopting it drops the glyph in the same
    // write (icon/image are ONE slot, in two spellings).
    bridgeFake().pickedProjectImage = "/data/project-icons/p1.png";
    fireEvent.click(screen.getByRole("button", { name: "Choose image…" }));
    await waitFor(() =>
      expect(calls.filter((c) => c.cmd === "set_project_mark")).toHaveLength(2),
    );
    expect(calls.filter((c) => c.cmd === "set_project_mark")[1]?.args).toEqual({
      id: "p1",
      image: "/data/project-icons/p1.png",
    });
    await waitFor(() => expect(mark.querySelector("img")).not.toBeNull());
    // …and the glyph is no longer worn, on screen as in the database.
    expect(screen.getByRole("button", { name: "Rocket" })).toHaveAttribute("aria-pressed", "false");

    // REMOVAL — the three fields at once, and the copied file goes with the reference.
    fireEvent.click(screen.getByRole("button", { name: /remove identity/i }));
    await waitFor(() =>
      expect(calls.filter((c) => c.cmd === "set_project_mark")).toHaveLength(3),
    );
    expect(calls.filter((c) => c.cmd === "set_project_mark")[2]?.args).toEqual({
      id: "p1",
      icon: null,
    });
    await waitFor(() =>
      expect(calls.filter((c) => c.cmd === "set_project_accent")).toHaveLength(2),
    );
    expect(calls.filter((c) => c.cmd === "set_project_accent")[1]?.args).toEqual({
      id: "p1",
      accent: null,
    });
    expect(bridgeFake().discardedProjectImages).toContain("p1");
    // Back to the initial on the neutral tile.
    await waitFor(() => expect(mark.className).toContain("bg-muted"));
    expect(mark).toHaveTextContent("A");
  });

  it("a REFUSED picture says so in line and writes nothing", async () => {
    const calls: { cmd: string; args: Record<string, unknown> }[] = [];
    installIdentityBackend(calls);
    renderLayout();
    await openIdentity();

    bridgeFake().projectImageRefusal =
      "A project picture must stay under 2 MB — hero-banner.png is 4.7 MB.";
    fireEvent.click(screen.getByRole("button", { name: "Choose image…" }));

    await waitFor(() =>
      expect(screen.getByRole("alert")).toHaveTextContent(
        "A project picture must stay under 2 MB — hero-banner.png is 4.7 MB.",
      ),
    );
    expect(calls.some((c) => c.cmd === "set_project_mark")).toBe(false);
  });
});

/**
 * The project CRUD rebranched onto the rail (PRD-B) — reached via the tile's RIGHT-CLICK context
 * menu (v2.5.6 finding #24: the ⋮ kebab trigger was removed, right-click is the sole entry point).
 * It drives project RENAME (via the project-settings modal → `update_project`), DELETE project
 * (`delete_project`), and ADD / REMOVE workspace (`create_workspace` / `workspace_delete`). The two
 * destructive actions sit behind an explicit CONFIRMATION step, and every action routes to the SAME
 * backend command the old sidebar kebab used.
 */
describe("BaseLayout — project right-click CRUD", () => {
  function ws(id: string, projectId: string, name: string, isRoot: boolean): WorkspaceRecord {
    return {
      id,
      project_id: projectId,
      name,
      path: `/${id}`,
      branch: null,
      is_root: isRoot,
      collapsed: false,
      created_at: 0,
      updated_at: 0,
    };
  }

  // One project (Alpha) with a root workspace "main" + one NON-root workspace "api" (so the
  // kebab offers a per-workspace remove). Captures every IPC call so a test can assert the
  // exact command + args a kebab action fired.
  function installCrudBackend(calls: { cmd: string; args: Record<string, unknown> }[]): void {
    mockIPC((cmd, args) => {
      const a = (args ?? {}) as Record<string, unknown>;
      calls.push({ cmd, args: a });
      switch (cmd) {
        case "list_projects":
          return [project("p1", "Alpha")];
        case "list_workspaces":
          return [ws("root-p1", "p1", "main", true), ws("api-p1", "p1", "api", false)];
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
        // The kebab CRUD commands — all succeed (undefined = Ok).
        case "update_project":
        case "create_workspace":
        case "delete_project":
        case "workspace_delete":
          return undefined;
        default:
          return undefined;
      }
    });
  }

  // Open the project's actions via the tile's RIGHT-CLICK menu (no visible kebab — finding #24).
  async function openActions(): Promise<void> {
    const tile = await screen.findByRole("button", { name: "Project Alpha" });
    fireEvent.contextMenu(tile);
  }

  it("shows NO visible kebab (⋮) trigger on a rail tile (right-click only)", async () => {
    installCrudBackend([]);
    renderLayout();
    // The tile is present, but there is no visible 3-dot actions button anymore.
    await screen.findByRole("button", { name: "Project Alpha" });
    expect(screen.queryByRole("button", { name: "Project actions for Alpha" })).toBeNull();
  });

  it("right-click → Project settings renames the project via update_project", async () => {
    const calls: { cmd: string; args: Record<string, unknown> }[] = [];
    installCrudBackend(calls);
    renderLayout();

    await openActions();
    fireEvent.click(await screen.findByRole("menuitem", { name: "Project settings for Alpha" }));

    // Rename lives in the settings modal's Global pane (per the task's reuse of
    // project-settings-dialog). Edit the Name and Save.
    const nameField = await screen.findByLabelText("Project name");
    fireEvent.change(nameField, { target: { value: "Alpha Renamed" } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => expect(calls.some((c) => c.cmd === "update_project")).toBe(true));
    const call = calls.find((c) => c.cmd === "update_project");
    expect(call?.args.id).toBe("p1");
    expect(call?.args.name).toBe("Alpha Renamed");
  });

  it("right-click → Add workspace creates a workspace via create_workspace", async () => {
    const calls: { cmd: string; args: Record<string, unknown> }[] = [];
    installCrudBackend(calls);
    // The folder picker resolves to this path; the add dialog then opens.
    bridgeFake().pickedDirectory = "/home/kris/api";
    renderLayout();

    await openActions();
    fireEvent.click(await screen.findByRole("menuitem", { name: "Add workspace to Alpha" }));

    // The add-workspace name dialog opens (folder read-only + editable name); confirm it.
    const nameField = await screen.findByRole("textbox", { name: /workspace name/i });
    fireEvent.change(nameField, { target: { value: "api" } });
    fireEvent.click(screen.getByRole("button", { name: "Add workspace" }));

    await waitFor(() => expect(calls.some((c) => c.cmd === "create_workspace")).toBe(true));
    const call = calls.find((c) => c.cmd === "create_workspace");
    expect(call?.args.projectId).toBe("p1");
    expect(call?.args.path).toBe("/home/kris/api");
  });

  it("right-click → Delete project asks to confirm BEFORE deleting via delete_project", async () => {
    const calls: { cmd: string; args: Record<string, unknown> }[] = [];
    installCrudBackend(calls);
    renderLayout();

    await openActions();
    fireEvent.click(await screen.findByRole("menuitem", { name: "Delete project Alpha" }));

    // A destructive-confirm step appears — nothing is deleted until it is confirmed.
    const confirm = await screen.findByRole("button", { name: "Delete" });
    expect(calls.some((c) => c.cmd === "delete_project")).toBe(false);

    fireEvent.click(confirm);
    await waitFor(() => expect(calls.some((c) => c.cmd === "delete_project")).toBe(true));
    expect(calls.find((c) => c.cmd === "delete_project")?.args.id).toBe("p1");
  });

  it("right-click → Remove <workspace> asks to confirm BEFORE deleting via workspace_delete", async () => {
    const calls: { cmd: string; args: Record<string, unknown> }[] = [];
    installCrudBackend(calls);
    renderLayout();

    await openActions();
    fireEvent.click(
      await screen.findByRole("menuitem", { name: "Remove workspace api from Alpha" }),
    );

    // The single-workspace destructive-confirm step; nothing is removed until confirmed.
    const confirm = await screen.findByRole("button", { name: "Remove workspace" });
    expect(calls.some((c) => c.cmd === "workspace_delete")).toBe(false);

    fireEvent.click(confirm);
    await waitFor(() => expect(calls.some((c) => c.cmd === "workspace_delete")).toBe(true));
    expect(calls.find((c) => c.cmd === "workspace_delete")?.args.id).toBe("api-p1");
  });

  it("right-click → Rename workspace renames it via rename_workspace", async () => {
    const calls: { cmd: string; args: Record<string, unknown> }[] = [];
    installCrudBackend(calls);
    renderLayout();

    await openActions();
    // The workspace's line in the rail menu now carries its OWN rename — the command existed in
    // the bridge and had NO UI caller before this (review 01M04WQEQDDPH7NQEK9MPYCXKS).
    fireEvent.click(await screen.findByRole("menuitem", { name: "Rename workspace api in Alpha" }));

    const nameField = await screen.findByRole("textbox", { name: "Workspace name" });
    expect((nameField as HTMLInputElement).value).toBe("api");
    fireEvent.change(nameField, { target: { value: "api-v2" } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => expect(calls.some((c) => c.cmd === "rename_workspace")).toBe(true));
    const call = calls.find((c) => c.cmd === "rename_workspace");
    expect(call?.args.id).toBe("api-p1");
    expect(call?.args.name).toBe("api-v2");
  });

  it("the ROOT workspace gets NO group at all: no Rename, no Remove, not even its label", async () => {
    // Review `01M0A7QGBK43MZB10RF51QR6TS`. The root briefly HAD a group here — an active Rename
    // over a DISABLED Remove carrying "the root workspace leaves with its project". The user's
    // verdict: on the root, show nothing. A section whose only content is one greyed row and an
    // explanation costs a read to discover there is nothing to do in it. Its rename goes with it.
    installCrudBackend([]);
    renderLayout();

    await openActions();
    // The non-root workspace's group is the proof the menu opened and still carries the actions.
    expect(
      await screen.findByRole("menuitem", { name: "Rename workspace api in Alpha" }),
    ).toBeTruthy();
    expect(screen.getByRole("menuitem", { name: "Remove workspace api from Alpha" })).toBeTruthy();

    // The ROOT ("main") contributes NOTHING — not a disabled row, not a group label.
    expect(screen.queryByRole("menuitem", { name: "Rename workspace main in Alpha" })).toBeNull();
    expect(screen.queryByRole("menuitem", { name: "Remove workspace main from Alpha" })).toBeNull();
    expect(screen.queryByText("main")).toBeNull();
    // …and no leftover disabled lifecycle row anywhere in the menu.
    expect(
      screen.queryAllByRole("menuitem").filter((i) => i.getAttribute("aria-disabled") === "true"),
    ).toHaveLength(0);
  });
});

/**
 * REVIEW-FINDING (« Registre d'actions : créer un terminal ou un workspace depuis la palette ») —
 * the `Add a workspace to …` action, END TO END through the shell.
 *
 * The palette writes NO creation of its own: `<BaseLayout>` hands it `onAddWorkspace`, which is the
 * rail's `useManualAdd` flow — native folder picker, then the name dialog, then `create_workspace`.
 * These two tests drive the palette and assert that flow, cancellation included: the whole point of
 * reusing it is that "cancel creates nothing" is a property of the existing path rather than a new
 * rule written beside it.
 */
describe("BaseLayout — the palette's Add-a-workspace action", () => {
  /** Serve the rail + a `global_search` answering ONE project hit, and record `create_workspace`. */
  function installSearchBackend(created: Array<Record<string, unknown>>) {
    mockIPC((cmd, args) => {
      const a = (args ?? {}) as Record<string, unknown>;
      switch (cmd) {
        case "list_projects":
          return [project("p1", "Alpha")];
        case "list_workspaces":
          return [root("ws-p1", "p1")];
        case "workspace_runtime_snapshot":
          return {
            workspace: { id: a.workspaceId as string, name: "root", path: "/ws-p1" },
            terminals: [],
            agent_sessions: [],
            services: [],
          };
        case "global_search":
          return {
            hits:
              (a.query as string) === ""
                ? []
                : [
                    {
                      kind: "project",
                      id: "p1",
                      project_id: "p1",
                      project_name: "Alpha",
                      workspace_id: null,
                      workspace_name: null,
                      spec_id: null,
                      terminal_id: null,
                      label: "Alpha",
                      detail: null,
                      extract: null,
                      human_key: null,
                      score: 1,
                      updated_at: 0,
                    },
                  ],
            knowledge_empty: false,
          };
        case "create_workspace": {
          created.push(a);
          return {
            id: "ws-new",
            project_id: "p1",
            name: (a.name as string) ?? "feature",
            path: a.path as string,
            branch: null,
            is_root: false,
            collapsed: false,
            created_at: 0,
            updated_at: 0,
          };
        }
        default:
          return undefined;
      }
    });
  }

  /** Open the palette (`Mod+K`, Ctrl under jsdom), type, and hand back the action row. */
  async function openActionRow() {
    await screen.findByRole("button", { name: "Project Alpha" });
    fireEvent.keyDown(document, { key: "k", ctrlKey: true, bubbles: true });
    const input = await screen.findByRole("combobox");
    fireEvent.change(input, { target: { value: "alpha" } });
    return await screen.findByTestId("search-action-project:new-workspace-p1");
  }

  it("runs the rail's picker → name dialog → create_workspace, with no second creation path", async () => {
    const created: Array<Record<string, unknown>> = [];
    installSearchBackend(created);
    bridgeFake().pickedDirectory = "/home/kris/feature";

    renderLayout();
    fireEvent.click(await openActionRow());

    // The palette gave way and the RAIL's own dialog took over, seeded by the picked folder — the
    // very modal the project kebab's "Add workspace" opens, not a second one.
    await waitFor(() => expect(screen.queryByRole("combobox")).toBeNull());
    const nameField = await screen.findByRole("textbox", { name: "Workspace name" });
    await waitFor(() => expect((nameField as HTMLInputElement).value).toBe("feature"));

    const confirmBtns = screen.getAllByRole("button", { name: "Add workspace" });
    fireEvent.click(confirmBtns[confirmBtns.length - 1]!);
    await waitFor(() => expect(created.length).toBe(1));
    expect(created[0]!.projectId).toBe("p1");
    expect(created[0]!.path).toBe("/home/kris/feature");
  });

  it("creates NOTHING when the folder picker is cancelled, and leaves the app coherent", async () => {
    const created: Array<Record<string, unknown>> = [];
    installSearchBackend(created);
    bridgeFake().pickedDirectory = null; // cancelled

    const { router } = renderLayout();
    fireEvent.click(await openActionRow());

    // The palette closed, and NOTHING opened behind it: no name dialog, no backend write.
    await waitFor(() => expect(screen.queryByRole("combobox")).toBeNull());
    await new Promise((r) => setTimeout(r, 0));
    expect(screen.queryByRole("textbox", { name: "Workspace name" })).toBeNull();
    expect(created.length).toBe(0);

    // COHERENT: the app is on the project the action named, on its Terminals page — the landing an
    // ordinary activation of that same project hit would have produced, and nothing more.
    await waitFor(() => expect(router.state.location.pathname).toBe("/"));
    expect(screen.getByRole("button", { name: "Project Alpha" }).getAttribute("aria-current")).toBe(
      "true",
    );
    vi.clearAllMocks();
  });
});
