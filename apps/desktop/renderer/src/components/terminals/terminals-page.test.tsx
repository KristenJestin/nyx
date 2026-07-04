import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { mockIPC } from "@/bridge/test-harness";
import { describe, expect, it } from "vitest";

import type { ProjectRecord, WorkspaceRecord } from "@/components/sidebar/use-projects";
import { ActiveProjectProvider } from "./active-project";
import { TerminalsPage } from "./terminals-page";
import type { WorkspaceRuntimeSnapshot } from "./use-workspace-runtime";

function project(id: string, name: string): ProjectRecord {
  return { id, name, collapsed: false, created_at: 0, updated_at: 0, resume_agent_sessions: false };
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

function snapshot(wsId: string): WorkspaceRuntimeSnapshot {
  return {
    workspace: { id: wsId, name: wsId, path: `/${wsId}` },
    terminals: [
      {
        id: `${wsId}-term`,
        cwd: `/${wsId}`,
        label: `${wsId}-shell`,
        workspace_id: wsId,
        status: "alive",
        exec_state: "idle",
        exec_state_unread: false,
        sessions: [],
      },
    ],
    agent_sessions: [],
    services: [
      {
        id: `${wsId}-svc`,
        workspace_id: wsId,
        name: `${wsId}-dev`,
        command: "npm run dev",
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
 */
function installBackend(calls: Array<{ cmd: string; args: Record<string, unknown> }> = []) {
  let createSeq = 0;
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
          return snapshot(a.workspaceId as string);
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
        case "attach_terminal":
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
  return render(
    <ActiveProjectProvider initialProjectId="p1">
      <TerminalsPage />
    </ActiveProjectProvider>,
  );
}

describe("TerminalsPage", () => {
  it("enters a project and lists its workspaces as accordions", async () => {
    installBackend();
    renderPage();
    // The entered project name heads the sidebar.
    expect(await screen.findByText("Alpha")).toBeTruthy();
    // Both workspaces appear as accordion headers.
    expect(await screen.findByRole("button", { name: /root/ })).toBeTruthy();
    expect(screen.getByRole("button", { name: /feat/ })).toBeTruthy();
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

  it("mounts the REAL terminal deck for the pane (reuses the shell's xterm, no duplication)", async () => {
    installBackend();
    const { container } = renderPage();
    // The auto-selected first terminal mounts a live deck pane keyed by record id — the real
    // `<TerminalDeck>` the shell uses, not a second terminal implementation.
    await waitFor(() => expect(container.querySelector("[data-terminal-id]")).not.toBeNull());
  });

  it("shows a placeholder when no project is entered", async () => {
    installBackend();
    render(
      <ActiveProjectProvider initialProjectId={null}>
        <TerminalsPage />
      </ActiveProjectProvider>,
    );
    expect(await screen.findByText(/Enter a project/)).toBeTruthy();
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

  it("Ctrl+T opens a new terminal (restored global shortcut)", async () => {
    const calls: Array<{ cmd: string; args: Record<string, unknown> }> = [];
    installBackend(calls);
    const { container } = renderPage();
    // Wait for the auto-selected terminal row (selection committed) so the shortcut resolves.
    await waitFor(() => expect(container.querySelector('[data-selected="true"]')).not.toBeNull());
    fireEvent.keyDown(document, { key: "t", ctrlKey: true });
    await waitFor(() => expect(calls.some((c) => c.cmd === "create_terminal")).toBe(true));
  });

  it("the Commands-section gear opens the command manager (reused ProjectCommandsDialog)", async () => {
    installBackend();
    renderPage();
    const gear = await screen.findByRole("button", { name: "New command" });
    fireEvent.click(gear);
    // The shared command manager modal opens, scoped to the project.
    expect(await screen.findByText(/Commands — Alpha/)).toBeTruthy();
    expect(screen.getByText(/Define named commands/)).toBeTruthy();
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
