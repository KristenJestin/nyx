import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";
import { bridgeFake, emit, mockIPC } from "@/bridge/test-harness";
import { describe, expect, it } from "vitest";

import type { ProjectRecord, WorkspaceRecord } from "@/components/sidebar/use-projects";
import { ActiveProjectProvider } from "./active-project";
import { PendingTerminalProvider, usePendingTerminal } from "./pending-terminal";
import { TerminalsPage } from "./terminals-page";
import { loadLastTerminalId, saveLastTerminalId } from "./ui-persistence";
import type { WorkspaceRuntimeSnapshot } from "@/components/runtime/use-workspace-runtime";

/**
 * The far end of the search palette's `Open a new terminal in …` action, stood in for: a button
 * that RAISES the one-shot request, and a probe reading whether it is still standing. The palette
 * itself is not mounted here — what it owes (raising the signal for the right workspace) is
 * asserted in `search/search-palette.test.tsx`; what the PAGE owes is below.
 */
function PendingTerminalHarness({ projectId = "p1" }: { projectId?: string } = {}) {
  const { pendingTerminal, requestTerminal } = usePendingTerminal();
  return (
    <>
      <button type="button" onClick={() => requestTerminal("ws-feat", projectId)}>
        raise pending terminal
      </button>
      {/* A request this project OWNS but whose workspace no tree carries — the "gone between the
          search and the activation" case, which the page must drop rather than keep. */}
      <button type="button" onClick={() => requestTerminal("ws-ghost", projectId)}>
        raise pending terminal for a ghost
      </button>
      <div data-testid="pending-terminal">{pendingTerminal?.workspaceId ?? "none"}</div>
    </>
  );
}

/**
 * Mount `<TerminalsPage>` at `/` under a memory router so its `?terminal=<id>` deep-link search
 * (read via `getRouteApi("/").useSearch()`) resolves. `initialProjectId` seeds the entered
 * project; `terminal` seeds the deep-link param (the Runtime-tab open-terminal seam). The
 * active-project provider wraps the router root so the page and rail share the selection.
 */
function mountTerminalsPage(opts: {
  initialProjectId: string | null;
  terminal?: string;
  command?: string;
  /**
   * Mount the one-shot PENDING-TERMINAL channel around the page, plus the harness that raises a
   * request into it — the palette's `Open a new terminal in …` action, stood in for. Opt-in so
   * every other test keeps the exact tree (and the exact button set) it had.
   */
  pendingTerminal?: boolean;
  /**
   * The project the raised request NAMES. Defaults to the page's own, which is the ordinary case;
   * a test passes another one to stand in for a request aimed at a project this page is not.
   */
  pendingTerminalProjectId?: string;
}) {
  const rootRoute = createRootRoute({
    component: () =>
      opts.pendingTerminal ? (
        <ActiveProjectProvider initialProjectId={opts.initialProjectId}>
          <PendingTerminalProvider>
            <PendingTerminalHarness
              projectId={opts.pendingTerminalProjectId ?? opts.initialProjectId ?? "p1"}
            />
            <TerminalsPage />
          </PendingTerminalProvider>
        </ActiveProjectProvider>
      ) : (
        <ActiveProjectProvider initialProjectId={opts.initialProjectId}>
          <TerminalsPage />
        </ActiveProjectProvider>
      ),
  });
  const indexRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/",
    component: () => null,
    validateSearch: (s: Record<string, unknown>): { terminal?: string; command?: string } => {
      const out: { terminal?: string; command?: string } = {};
      if (typeof s.terminal === "string") out.terminal = s.terminal;
      if (typeof s.command === "string") out.command = s.command;
      return out;
    },
  });
  const search = opts.terminal
    ? `?terminal=${opts.terminal}`
    : opts.command
      ? `?command=${opts.command}`
      : "";
  const router = createRouter({
    routeTree: rootRoute.addChildren([indexRoute]),
    history: createMemoryHistory({ initialEntries: [`/${search}`] }),
  });
  return render(<RouterProvider router={router} />);
}

function project(id: string, name: string): ProjectRecord {
  return {
    id,
    name,
    key: "P",
    collapsed: false,
    created_at: 0,
    updated_at: 0,
    resume_agent_sessions: false,
  };
}

function workspace(id: string, projectId: string, name: string, isRoot: boolean): WorkspaceRecord {
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

function snapshot(
  wsId: string,
  extraTerminalIds: readonly string[] = [],
): WorkspaceRuntimeSnapshot {
  const runtimeTerminal = (id: string) => ({
    id,
    cwd: `/${wsId}`,
    label: `${wsId}-shell`,
    workspace_id: wsId,
    status: "alive" as const,
    exec_state: "idle" as const,
    exec_state_unread: false,
    sessions: [],
  });
  return {
    workspace: { id: wsId, name: wsId, path: `/${wsId}` },
    terminals: [runtimeTerminal(`${wsId}-term`), ...extraTerminalIds.map(runtimeTerminal)],
    agent_sessions: [],
    services: [
      {
        id: `${wsId}-svc`,
        command_id: `${wsId}-cmd`,
        workspace_id: wsId,
        name: `${wsId}-dev`,
        command: "npm run dev",
        subfolder: null,
        description: null,
        run_state: "running",
        exit_code: null,
        unread: false,
      },
    ],
  };
}

/**
 * A backend serving the project tree, the per-workspace runtime, a PTY spawn, and the terminal
 * CRUD commands `useTerminals` drives (`list_terminals`/`create_terminal`/`attach_terminal`/
 * `close_terminal`). `calls` records every command so a test can assert the create/close path
 * ran. Terminal ids are minted deterministically per create.
 *
 * A created terminal LANDS IN ITS WORKSPACE'S SNAPSHOT once it is attached, and the attach pushes
 * `terminals://changed` — the real backend's own sequence. Without it the page could never SHOW a
 * terminal it just created, so a split's new pane would be invisible for reasons that have nothing
 * to do with the product.
 */
function installBackend(calls: Array<{ cmd: string; args: Record<string, unknown> }> = []) {
  let createSeq = 0;
  /** Terminals created + attached during the test, per workspace id. */
  const attached = new Map<string, string[]>();
  mockIPC(
    (cmd, args) => {
      const a = (args ?? {}) as Record<string, unknown>;
      calls.push({ cmd, args: a });
      switch (cmd) {
        case "list_projects":
          return [project("p1", "Alpha")];
        case "list_workspaces":
          return [
            workspace("ws-root", "p1", "root", true),
            workspace("ws-feat", "p1", "feat", false),
          ];
        case "workspace_runtime_snapshot":
          return snapshot(a.workspaceId as string, attached.get(a.workspaceId as string) ?? []);
        case "pty_spawn":
          return 100;
        case "list_terminals":
          return [];
        case "command_list":
        case "command_instance_list":
        case "command_import_scripts":
          return [];
        case "create_terminal": {
          createSeq += 1;
          return {
            id: `new-term-${createSeq}`,
            cwd: (a.cwd as string) ?? "/home",
            label: null,
            status: "alive",
            order_index: 0,
            created_at: 0,
            updated_at: 0,
            closed_at: null,
            workspace_id: null,
          };
        }
        case "attach_terminal": {
          const workspaceId = a.workspaceId as string;
          const id = a.terminalId as string;
          const list = attached.get(workspaceId) ?? [];
          if (!list.includes(id)) attached.set(workspaceId, [...list, id]);
          // The real backend pushes this on the attach; the page re-pulls its snapshot on it.
          setTimeout(() => void emit("terminals://changed"), 0);
          return undefined;
        }
        case "close_terminal":
          return undefined;
        default:
          return undefined;
      }
    },
    { shouldMockEvents: true },
  );
}

function renderPage() {
  return mountTerminalsPage({ initialProjectId: "p1" });
}

describe("TerminalsPage", () => {
  it("enters a project and lists its workspaces as accordions", async () => {
    installBackend();
    renderPage();
    // The entered project name heads the sidebar.
    expect(await screen.findByText("Alpha")).toBeTruthy();
    // Both workspaces appear as accordion headers.
    // Queried by the toggle's EXACT name: each header now also carries an actions kebab, so a
    // loose /root/ would match two buttons.
    expect(await screen.findByRole("button", { name: "Toggle workspace root" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Toggle workspace feat" })).toBeTruthy();
  });

  it("renders the first workspace's runtime tree (terminals + commands) open by default", async () => {
    installBackend();
    renderPage();
    // The first workspace is open by default → its runtime tree shows.
    expect(await screen.findByText("ws-root-shell")).toBeTruthy();
    expect(screen.getByText("ws-root-dev")).toBeTruthy();
    // Terminals + Commands sections, never an Agents 3rd category.
    expect(screen.getAllByText("Terminals").length).toBeGreaterThan(0);
    expect(screen.getAllByText("Commands").length).toBeGreaterThan(0);
    expect(screen.queryByText("Agents")).toBeNull();
  });

  it("mounts the REAL tiling host for the pane (reuses the shell's xterm, no duplication)", async () => {
    installBackend();
    const { container } = renderPage();
    // The auto-selected first terminal mounts a live pane keyed by record id, inside the tiling
    // host — the same `<Terminal>`/store the shell uses, not a second terminal implementation.
    await waitFor(() => expect(container.querySelector("[data-terminal-id]")).not.toBeNull());
    expect(container.querySelector("[data-pane-host]"), "the page tiles").not.toBeNull();
  });

  it("shows a placeholder when no project is entered", async () => {
    installBackend();
    mountTerminalsPage({ initialProjectId: null });
    expect(await screen.findByText(/Enter a project/)).toBeTruthy();
  });

  it("opens the terminal named by the `?terminal=<id>` deep-link (not the auto-first)", async () => {
    installBackend();
    // Deep-link to the SECOND workspace's terminal — the Runtime tab's open-terminal seam.
    const { container } = mountTerminalsPage({ initialProjectId: "p1", terminal: "ws-feat-term" });
    // The deep-linked terminal's pane is the FOCUSED one (not the auto-first ws-root-term). With
    // the tiling, "focused" is the ONE pane that takes the keyboard — the distinction the deck's
    // single `data-active` could not express.
    await waitFor(() => {
      const pane = container.querySelector('[data-terminal-id="ws-feat-term"]');
      expect(pane?.getAttribute("data-focused")).toBe("true");
    });
    const first = container.querySelector('[data-terminal-id="ws-root-term"]');
    expect(first?.getAttribute("data-focused")).toBe("false");
    // …and it is not even tiled: nothing was split, so the shown terminal is alone on screen.
    expect(first?.getAttribute("data-visible")).toBe("false");
  });

  it("restores this project's LAST active terminal (persisted) instead of the first", async () => {
    installBackend();
    // The user last had the SECOND workspace's terminal focused in this project; a relaunch /
    // return to Terminals must reopen it, not snap back to the first (ws-root-term).
    saveLastTerminalId("p1", "ws-feat-term");
    const { container } = renderPage();
    await waitFor(() => {
      const pane = container.querySelector('[data-terminal-id="ws-feat-term"]');
      expect(pane?.getAttribute("data-focused")).toBe("true");
    });
    const first = container.querySelector('[data-terminal-id="ws-root-term"]');
    expect(first?.getAttribute("data-focused")).toBe("false");
  });

  it("falls back to the first terminal when the persisted one no longer exists", async () => {
    installBackend();
    // A stale id (the remembered terminal was closed while away) must not pin the pane to
    // something that is gone — the existence guard falls back cleanly to the first terminal.
    saveLastTerminalId("p1", "ghost-term-that-is-gone");
    const { container } = renderPage();
    await waitFor(() => {
      const pane = container.querySelector('[data-terminal-id="ws-root-term"]');
      expect(pane?.getAttribute("data-focused")).toBe("true");
    });
  });

  it("remembers the selected terminal so a later entry can restore it", async () => {
    installBackend();
    renderPage();
    // The auto-selected first terminal is persisted as this project's last active terminal — the
    // write half of the restore round-trip (the read half is covered above).
    await waitFor(() => expect(loadLastTerminalId("p1")).toBe("ws-root-term"));
  });

  it("opens the managed command named by the `?command=<id>` deep-link (its pane, tiling hidden)", async () => {
    installBackend();
    // Deep-link to the SECOND workspace's service — the Runtime tab's open-command seam.
    const { container } = mountTerminalsPage({ initialProjectId: "p1", command: "ws-feat-svc" });
    // A selected SERVICE hides the TILING and shows the command's own pane, so no terminal is the
    // active surface (the whole tiling region is hidden — its terminals stay alive underneath).
    await waitFor(() => {
      const pane = container.querySelector('[data-terminal-id="ws-root-term"]');
      expect(pane).not.toBeNull();
      expect(pane?.closest(".hidden")).not.toBeNull();
    });
  });
});

describe("TerminalsPage — restored terminal/command affordances (finding 50)", () => {
  it("the Terminals-section + opens a new terminal in that workspace (reused sidebar CRUD)", async () => {
    const calls: Array<{ cmd: string; args: Record<string, unknown> }> = [];
    installBackend(calls);
    renderPage();
    // The first (root) workspace's tree is open; its Terminals section carries a "+".
    const addBtn = await screen.findByRole("button", { name: "New terminal" });
    fireEvent.click(addBtn);
    // The + creates a terminal (create_terminal at the workspace path) and binds it to the
    // workspace (attach_terminal) — the exact old-shell per-workspace "+" path, reused.
    await waitFor(() => expect(calls.some((c) => c.cmd === "create_terminal")).toBe(true));
    const create = calls.find((c) => c.cmd === "create_terminal");
    expect(create!.args.cwd).toBe("/ws-root");
    await waitFor(() => {
      const attach = calls.find((c) => c.cmd === "attach_terminal");
      expect(attach?.args.workspaceId).toBe("ws-root");
    });
  });

  /**
   * REVIEW-FINDING (« Registre d'actions : créer un terminal ou un workspace depuis la palette ») —
   * the PAGE's half of the palette's `Open a new terminal in …` action.
   *
   * The palette can only REQUEST it (`terminals/pending-terminal`): the path that creates a
   * terminal AND ENTERS it is `useTerminalActions().newTerminalInWorkspace`, whose "enter" is a
   * selection in THIS page's runtime. So the request lands here and is honoured by the very
   * callback the workspace band's "+" calls — which is what "no second creation path" means, and
   * what this test pins by asserting the SAME three facts as the "+" test above, plus the entering.
   */
  it("honours a pending-terminal request: creates in that workspace AND enters the terminal", async () => {
    const calls: Array<{ cmd: string; args: Record<string, unknown> }> = [];
    installBackend(calls);
    const { container } = mountTerminalsPage({ initialProjectId: "p1", pendingTerminal: true });
    // Wait for the page to settle on its auto-selected first terminal, so "the selection moved"
    // below is a real move and not the initial one.
    await waitFor(() => {
      const first = container.querySelector('[data-terminal-id="ws-root-term"]');
      expect(first?.getAttribute("data-focused")).toBe("true");
    });

    fireEvent.click(screen.getByRole("button", { name: "raise pending terminal" }));

    // CREATE at the named workspace's path, ATTACH to the named workspace — the sidebar CRUD, not
    // a second one. `ws-feat` is NOT the workspace the selection was in, which is the point: the
    // request names its target rather than inheriting one.
    await waitFor(() => expect(calls.some((c) => c.cmd === "create_terminal")).toBe(true));
    expect(calls.find((c) => c.cmd === "create_terminal")!.args.cwd).toBe("/ws-feat");
    await waitFor(() =>
      expect(calls.find((c) => c.cmd === "attach_terminal")?.args.workspaceId).toBe("ws-feat"),
    );

    // …AND ENTERS IT: the created terminal is the focused pane, which is the half the user asked
    // for by name (« ajouter ET entrer »).
    await waitFor(() => {
      const pane = container.querySelector('[data-terminal-id="new-term-1"]');
      expect(pane?.getAttribute("data-focused")).toBe("true");
    });

    // ONE SHOT: the signal is consumed before the creation, so a re-render cannot open a second
    // terminal for one request.
    expect(screen.getByTestId("pending-terminal").textContent).toBe("none");
    expect(calls.filter((c) => c.cmd === "create_terminal")).toHaveLength(1);

    // …but the guard is about ONE request, not about the workspace: asking AGAIN for the same
    // workspace opens a second terminal, which is what the user asked for.
    fireEvent.click(screen.getByRole("button", { name: "raise pending terminal" }));
    await waitFor(() => expect(calls.filter((c) => c.cmd === "create_terminal")).toHaveLength(2));
  });

  /**
   * REVIEW-FINDING (« Demande de terminal orpheline créée à retardement ») — a request THIS project
   * owns but cannot satisfy is DROPPED, not left standing.
   *
   * The request names a workspace and its owning project. When the owning project is the one on
   * screen and the workspace is still absent from the tree, the workspace is gone — deleted between
   * the search and the activation, or never there — rather than one render away. Left standing, the
   * request outlived the intent that raised it and fired the day the user reached that project by
   * some other route, opening a terminal nobody had asked for. Unlike a stale reveal, which merely
   * opens a band, a stale creation cannot be undone by ignoring it.
   *
   * A request naming ANOTHER project is the opposite case and stays: the palette enters that
   * project in the same breath, so the page able to honour it is one navigation away.
   */
  it("drops a pending-terminal request its own project cannot satisfy, and creates nothing", async () => {
    const calls: Array<{ cmd: string; args: Record<string, unknown> }> = [];
    installBackend(calls);
    const { container } = mountTerminalsPage({ initialProjectId: "p1", pendingTerminal: true });
    await waitFor(() => {
      const first = container.querySelector('[data-terminal-id="ws-root-term"]');
      expect(first?.getAttribute("data-focused")).toBe("true");
    });
    const before = calls.filter((c) => c.cmd === "create_terminal").length;

    fireEvent.click(screen.getByRole("button", { name: "raise pending terminal for a ghost" }));

    // The signal is consumed — the page proved it unhonourable — and nothing was created.
    await waitFor(() => expect(screen.getByTestId("pending-terminal").textContent).toBe("none"));
    expect(calls.filter((c) => c.cmd === "create_terminal")).toHaveLength(before);
  });

  it("Ctrl+T opens a new terminal (restored global shortcut)", async () => {
    const calls: Array<{ cmd: string; args: Record<string, unknown> }> = [];
    installBackend(calls);
    const { container } = renderPage();
    // Wait for the auto-selected terminal row (selection committed) so the shortcut resolves.
    await waitFor(() => expect(container.querySelector('[data-selected="true"]')).not.toBeNull());
    fireEvent.keyDown(document, { key: "t", ctrlKey: true });
    await waitFor(() => expect(calls.some((c) => c.cmd === "create_terminal")).toBe(true));
  });

  /**
   * ONE MODAL, TWO ENTRY POINTS (finding 01M132YSBKJ5J22VG60P5GN7W0). The COMMANDS gear used to open
   * `<ProjectCommandsDialog>` — a 44rem standalone modal whose body was the very same
   * `<CommandsSection>` the project settings show. One body, two useful widths, two chromes: the
   * same duplication as the two lists inside it, one level up. The standalone modal is deleted and
   * the gear opens the settings ON THE COMMANDS SECTION, which they already knew how to do.
   */
  it("the Commands-section gear opens the project settings ON the Commands section", async () => {
    installBackend();
    renderPage();
    const gear = await screen.findByRole("button", { name: "New command" });
    fireEvent.click(gear);

    // The shared settings modal, on Commands: its rail marks the section as current and the
    // catalogue is mounted.
    const commands = await screen.findByRole("button", { name: "Commands" });
    expect(commands).toHaveAttribute("aria-current", "page");
    // The dialog is PORTALLED, so the catalogue lives outside the render container.
    await waitFor(() => expect(document.querySelector("[data-command-list]")).not.toBeNull());
    // …and the deleted modal's own chrome is nowhere.
    expect(screen.queryByText(/Commands — Alpha/)).toBeNull();
    expect(screen.queryByText(/Define named commands/)).toBeNull();
  });

  it("…and the header gear still opens the SAME modal on Global", async () => {
    installBackend();
    renderPage();
    fireEvent.click(await screen.findByRole("button", { name: "Project settings" }));
    expect(await screen.findByRole("switch", { name: /resume agent sessions/i })).toBeTruthy();
    // ONE dialog mounted, whichever gear opened it.
    expect(document.querySelectorAll("[role='dialog']").length).toBeLessThanOrEqual(1);
  });

  it("Mod+Shift+→ SPLITS the focused pane: one new terminal, in the focused pane's workspace", async () => {
    const calls: Array<{ cmd: string; args: Record<string, unknown> }> = [];
    installBackend(calls);
    const { container } = renderPage();
    // The tiling is up and one pane holds the keyboard before the chord is fired.
    await waitFor(() =>
      expect(
        container.querySelector('[data-focused="true"]')?.getAttribute("data-terminal-id"),
      ).toBe("ws-root-term"),
    );

    fireEvent.keyDown(document, { key: "ArrowRight", ctrlKey: true, shiftKey: true });

    // The split creates its terminal through the SAME sidebar CRUD as the "+" — create at the
    // focused pane's workspace path, then attach it to that workspace. The LEAF only lands after
    // that succeeds, so a chord that reaches nothing would show here as no `create_terminal`.
    await waitFor(() => expect(calls.some((c) => c.cmd === "create_terminal")).toBe(true));
    const create = calls.find((c) => c.cmd === "create_terminal");
    expect(create!.args.cwd, "the focused pane's own workspace").toBe("/ws-root");
    await waitFor(() => {
      const attach = calls.find((c) => c.cmd === "attach_terminal");
      expect(attach?.args.workspaceId).toBe("ws-root");
    });
    // …and the tiling now shows TWO panes — the leaf was placed, not just a terminal created.
    await waitFor(() =>
      expect(
        container.querySelectorAll('[data-visible="true"]'),
        "the pane was added",
      ).toHaveLength(2),
    );
    expect(container.querySelectorAll('[data-focused="true"]'), "still ONE focus").toHaveLength(1);
  });

  it("Ctrl+W closes the selected terminal (restored close shortcut)", async () => {
    const calls: Array<{ cmd: string; args: Record<string, unknown> }> = [];
    installBackend(calls);
    const { container } = renderPage();
    // Wait for the auto-selected terminal ROW to be highlighted (selection committed → the
    // close handler is rebound to `ws-root-term`) before firing the chord.
    await waitFor(() => expect(container.querySelector('[data-selected="true"]')).not.toBeNull());
    fireEvent.keyDown(document, { key: "w", ctrlKey: true });
    await waitFor(() => {
      const close = calls.find((c) => c.cmd === "close_terminal");
      expect(close?.args.id).toBe("ws-root-term");
    });
  });
});

describe("TerminalsPage — Add workspace (surfaced in the current shell)", () => {
  it("adds a workspace via folder picker → name dialog → create_workspace, and it appears", async () => {
    const created: Array<Record<string, unknown>> = [];
    // Start with just the root workspace; the created one is appended so the authoritative
    // re-list (`list_workspaces` after `create_workspace`) reflects it — proving the new
    // workspace shows up in the sidebar list.
    const workspaces: WorkspaceRecord[] = [workspace("ws-root", "p1", "root", true)];
    mockIPC(
      (cmd, args) => {
        const a = (args ?? {}) as Record<string, unknown>;
        switch (cmd) {
          case "list_projects":
            return [project("p1", "Alpha")];
          case "list_workspaces":
            return workspaces;
          case "workspace_runtime_snapshot":
            return snapshot(a.workspaceId as string);
          case "pty_spawn":
            return 100;
          case "list_terminals":
          case "command_list":
          case "command_instance_list":
          case "command_import_scripts":
            return [];
          case "create_workspace": {
            created.push(a);
            const ws = workspace("ws-new", "p1", (a.name as string) ?? "feature", false);
            workspaces.push(ws);
            return ws;
          }
          default:
            return undefined;
        }
      },
      { shouldMockEvents: true },
    );
    // The native folder picker resolves to this path (basename → the seeded default name).
    bridgeFake().pickedDirectory = "/home/kris/feature";

    mountTerminalsPage({ initialProjectId: "p1" });
    // The Terminals sidebar header carries an "Add workspace" affordance (the current shell only
    // surfaced "Add project" before — the capability existed but was unwired).
    const addBtn = await screen.findByRole("button", { name: "Add workspace" });
    fireEvent.click(addBtn);
    // The name dialog opens seeded with the folder basename; confirm it.
    const nameField = await screen.findByRole("textbox", { name: "Workspace name" });
    await waitFor(() => expect((nameField as HTMLInputElement).value).toBe("feature"));
    // Two "Add workspace" buttons now exist — the header trigger + the modal confirm; click the last.
    const confirmBtns = screen.getAllByRole("button", { name: "Add workspace" });
    fireEvent.click(confirmBtns[confirmBtns.length - 1]!);

    // The backend create ran with the picked path + project, and the new workspace shows in the list.
    await waitFor(() => expect(created.length).toBe(1));
    expect(created[0]!.path).toBe("/home/kris/feature");
    expect(created[0]!.projectId).toBe("p1");
    expect(await screen.findByRole("button", { name: "Toggle workspace feature" })).toBeTruthy();
  });

  it("does nothing when the folder picker is cancelled (no dialog, no create)", async () => {
    const created: unknown[] = [];
    mockIPC(
      (cmd, args) => {
        const a = (args ?? {}) as Record<string, unknown>;
        switch (cmd) {
          case "list_projects":
            return [project("p1", "Alpha")];
          case "list_workspaces":
            return [workspace("ws-root", "p1", "root", true)];
          case "workspace_runtime_snapshot":
            return snapshot(a.workspaceId as string);
          case "pty_spawn":
            return 100;
          case "list_terminals":
          case "command_list":
          case "command_instance_list":
          case "command_import_scripts":
            return [];
          case "create_workspace":
            created.push(a);
            return undefined;
          default:
            return undefined;
        }
      },
      { shouldMockEvents: true },
    );
    bridgeFake().pickedDirectory = null; // cancelled

    mountTerminalsPage({ initialProjectId: "p1" });
    const addBtn = await screen.findByRole("button", { name: "Add workspace" });
    fireEvent.click(addBtn);
    // A cancelled pick opens no dialog → the name field never appears, no create fires.
    await new Promise((r) => setTimeout(r, 0));
    expect(screen.queryByRole("textbox", { name: "Workspace name" })).toBeNull();
    expect(created.length).toBe(0);
  });
});

describe("TerminalsPage — no loose terminals band (v2.5.6 finding #26)", () => {
  /** A backend whose `list_terminals` returns ONE unattached terminal (`workspace_id null`). */
  function installBackendWithLooseTerminal() {
    mockIPC(
      (cmd, args) => {
        const a = (args ?? {}) as Record<string, unknown>;
        switch (cmd) {
          case "list_projects":
            return [project("p1", "Alpha")];
          case "list_workspaces":
            return [workspace("ws-root", "p1", "root", true)];
          case "workspace_runtime_snapshot":
            return snapshot(a.workspaceId as string);
          case "pty_spawn":
            return 100;
          case "list_terminals":
            return [
              {
                id: "loose-1",
                cwd: "/tmp",
                label: "loose-shell",
                status: "alive",
                order_index: 0,
                created_at: 0,
                updated_at: 0,
                closed_at: null,
                workspace_id: null,
              },
            ];
          case "command_list":
          case "command_instance_list":
          case "command_import_scripts":
            return [];
          default:
            return undefined;
        }
      },
      { shouldMockEvents: true },
    );
  }

  it("does NOT render a Loose terminals band, even when an unattached terminal exists", async () => {
    installBackendWithLooseTerminal();
    renderPage();
    // The workspace tree must render (so we know the page mounted), but the removed band + its
    // unattached terminal must be absent — the user cannot have a terminal attached to nothing.
    expect(await screen.findByText("ws-root-shell")).toBeTruthy();
    expect(screen.queryByText("Loose terminals")).toBeNull();
    expect(screen.queryByText("loose-shell")).toBeNull();
    // The band's "New unattached terminal" affordance is gone too.
    expect(screen.queryByRole("button", { name: "New unattached terminal" })).toBeNull();
  });
});

describe("TerminalsPage — Project settings (visible gear entry point)", () => {
  it("the header exposes a Project settings gear that opens the dialog on Global (resume toggle, rename hidden)", async () => {
    installBackend();
    renderPage();
    // The active project's header carries a VISIBLE, discoverable "Project settings" gear — the
    // twin of the rail tile's right-click entry point, surfaced so the reglage is FINDABLE (it
    // was previously only reachable via a hidden context menu).
    const gear = await screen.findByRole("button", { name: "Project settings" });
    // Closed by default: the resume toggle is not mounted until the gear is clicked.
    expect(screen.queryByRole("switch", { name: /resume agent sessions/i })).toBeNull();

    fireEvent.click(gear);

    // The shared ProjectSettingsDialog opens on the Global section: the resume toggle shows and,
    // because rename/delete belong to the PRD-B kebab CRUD (showRename=false), the Project name
    // field stays hidden.
    const toggle = await screen.findByRole("switch", { name: /resume agent sessions/i });
    expect(toggle).toHaveAttribute("aria-checked", "false");
    expect(screen.queryByLabelText("Project name")).toBeNull();
  });

  it("flipping the resume toggle persists via set_project_resume_agent_sessions and reflects live", async () => {
    const calls: Array<{ cmd: string; args: Record<string, unknown> }> = [];
    installBackend(calls);
    renderPage();
    fireEvent.click(await screen.findByRole("button", { name: "Project settings" }));
    const toggle = await screen.findByRole("switch", { name: /resume agent sessions/i });

    fireEvent.click(toggle);

    // The flip calls the SAME backend command the rail right-click wiring uses; the project id +
    // requested value are threaded through `setProjectResumeAgentSessions`.
    await waitFor(() => {
      const call = calls.find((c) => c.cmd === "set_project_resume_agent_sessions");
      expect(call?.args).toEqual({ id: "p1", resume: true });
    });
    // The optimistic tree update flows back to the live `resumeAgentSessions` prop, so the toggle
    // reflects the persisted ON state (proving the setting sticks, not just fires-and-forgets).
    await waitFor(() => expect(toggle).toHaveAttribute("aria-checked", "true"));
  });
});

/**
 * The WORKSPACE LIFECYCLE, reachable from the workspace's OWN line (review
 * `01M04WQEQDDPH7NQEK9MPYCXKS`). Before this, `rename_workspace` had no UI caller at all and
 * `workspace_delete` was reachable only from the PROJECT's rail menu — so from a worktree there
 * was no way to rename or drop it. These tests hold the line's kebab to the four things that
 * matter: the rename really calls `rename_workspace`, the removal is CONFIRMED before it calls
 * `workspace_delete`, cancelling the confirmation calls NOTHING, and a ROOT workspace shows no
 * kebab at all (review `01M0A7QGBK43MZB10RF51QR6TS` — not a disabled row, not the three dots).
 */
describe("TerminalsPage — workspace lifecycle from its own line", () => {
  /** Open the kebab of the workspace line named `name`. */
  async function openWorkspaceActions(name: string): Promise<void> {
    fireEvent.click(await screen.findByRole("button", { name: `Workspace actions for ${name}` }));
  }

  it("offers Rename + Remove on a non-root workspace line, without the project menu", async () => {
    installBackend();
    renderPage();

    await openWorkspaceActions("feat");
    expect(await screen.findByRole("menuitem", { name: "Rename workspace feat" })).toBeTruthy();
    expect(screen.getByRole("menuitem", { name: "Remove workspace feat" })).toBeTruthy();
  });

  it("renames the workspace through rename_workspace, pre-filled with its current name", async () => {
    const calls: Array<{ cmd: string; args: Record<string, unknown> }> = [];
    installBackend(calls);
    renderPage();

    await openWorkspaceActions("feat");
    fireEvent.click(await screen.findByRole("menuitem", { name: "Rename workspace feat" }));

    // The rename dialog is the SHARED one, worded for a workspace and seeded with the current name.
    const field = await screen.findByRole("textbox", { name: "Workspace name" });
    expect((field as HTMLInputElement).value).toBe("feat");
    fireEvent.change(field, { target: { value: "feature-x" } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => expect(calls.some((c) => c.cmd === "rename_workspace")).toBe(true));
    const call = calls.find((c) => c.cmd === "rename_workspace");
    expect(call?.args.id).toBe("ws-feat");
    expect(call?.args.name).toBe("feature-x");
  });

  it("removes the workspace only AFTER the destructive confirmation, which says the folder stays", async () => {
    const calls: Array<{ cmd: string; args: Record<string, unknown> }> = [];
    installBackend(calls);
    renderPage();

    await openWorkspaceActions("feat");
    fireEvent.click(await screen.findByRole("menuitem", { name: "Remove workspace feat" }));

    // The confirmation is explicit about what a removal is NOT: nyx forgets the entry, Git and the
    // folder are untouched.
    expect(await screen.findByText(/folder on disk is not deleted/i)).toBeTruthy();
    const confirm = screen.getByRole("button", { name: "Remove workspace" });
    expect(calls.some((c) => c.cmd === "workspace_delete")).toBe(false);

    fireEvent.click(confirm);
    await waitFor(() => expect(calls.some((c) => c.cmd === "workspace_delete")).toBe(true));
    expect(calls.find((c) => c.cmd === "workspace_delete")?.args.id).toBe("ws-feat");
  });

  it("cancelling the confirmation removes NOTHING (no backend call at all)", async () => {
    const calls: Array<{ cmd: string; args: Record<string, unknown> }> = [];
    installBackend(calls);
    renderPage();

    await openWorkspaceActions("feat");
    fireEvent.click(await screen.findByRole("menuitem", { name: "Remove workspace feat" }));
    fireEvent.click(await screen.findByRole("button", { name: "Cancel" }));

    // Nothing was dropped — and nothing was even attempted.
    await waitFor(() =>
      expect(screen.queryByRole("button", { name: "Remove workspace" })).toBeNull(),
    );
    expect(calls.some((c) => c.cmd === "workspace_delete")).toBe(false);
  });

  it("a ROOT workspace line has NO kebab at all — no trigger, no Rename, no Remove", async () => {
    // Review `01M0A7QGBK43MZB10RF51QR6TS`. The root line used to carry a kebab opening on an
    // active Rename and a DISABLED Remove with its reason — a click spent to learn there was
    // nothing to do. The user asked for nothing on the root, its rename included: the root's
    // lifecycle is the project's. The trigger itself is gone, so nothing behind it can leak.
    installBackend();
    renderPage();

    // The non-root line still has its kebab (so the page really rendered both workspaces).
    expect(await screen.findByRole("button", { name: "Workspace actions for feat" })).toBeTruthy();
    // The root line has none — and therefore no lifecycle entry for it anywhere.
    expect(screen.queryByRole("button", { name: "Workspace actions for root" })).toBeNull();
    expect(screen.queryByRole("menuitem", { name: "Rename workspace root" })).toBeNull();
    expect(screen.queryByRole("menuitem", { name: "Remove workspace root" })).toBeNull();
  });
});
