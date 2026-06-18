import { act, render, screen, waitFor } from "@testing-library/react";
import { mockIPC } from "@tauri-apps/api/mocks";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { TerminalRecord } from "./use-terminals";
import type { ProjectRecord, WorkspaceRecord } from "./use-projects";
import type { CommandRecord } from "./use-projects";

/**
 * Tests for the ACKNOWLEDGE-ON-SELECT wiring in `<TerminalManager>` (finding
 * 01KV6HVBPYTZ…): selecting a command whose live state is terminal (success/error)
 * invokes `command_acknowledge` so its "unseen result" dot reverts to idle; a
 * running (or idle) command is NEVER acknowledged.
 *
 * We replace the heavy presentational children. `AppSidebar` is stubbed with an
 * inert surface that exposes one "select" button per command (calling the real
 * `onSelectCommand`), so the test can drive selection without mounting the full
 * sidebar tree.
 */

// Capture the commands-by-workspace + onSelectCommand the manager passes down, and
// render a select button per command so the test can click it.
vi.mock("./app-sidebar", () => ({
  AppSidebar: ({
    commandsByWorkspace,
    onSelectCommand,
  }: {
    commandsByWorkspace: Map<string, CommandRecord[]>;
    onSelectCommand: (id: string) => void;
  }) => {
    const all = Array.from(commandsByWorkspace.values()).flat();
    return (
      <div>
        {all.map((c) => (
          <button key={c.id} type="button" onClick={() => onSelectCommand(c.id)}>
            select {c.id}
          </button>
        ))}
      </div>
    );
  },
}));

vi.mock("./terminal-deck", () => ({ TerminalDeck: () => null }));
vi.mock("@/components/chrome/chrome-bar", () => ({ ChromeBar: () => null }));
vi.mock("@/components/command/command-view", () => ({ CommandView: () => null }));
vi.mock("./use-manual-add", () => ({
  useManualAdd: () => ({
    addProject: vi.fn(),
    addWorkspace: vi.fn(),
    editProject: vi.fn(),
    removeProject: vi.fn(),
    dialog: null,
  }),
}));

import { TerminalManager } from "./terminal-manager";

function workspace(id: string, projectId: string, path: string): WorkspaceRecord {
  return {
    id,
    project_id: projectId,
    name: id,
    path,
    branch: null,
    is_root: true,
    collapsed: false,
    created_at: 0,
    updated_at: 0,
  };
}

interface Inst {
  id: string;
  last_state: string;
  /** v4: the "unseen result" flag. A finished result is unread until acknowledged. */
  unread?: boolean;
}

interface Backend {
  calls: { cmd: string; args: Record<string, unknown> }[];
  acknowledge: string[];
}

/** Install a backend with one project/workspace and the given command instances. */
function installBackend(instances: Inst[]): Backend {
  const backend: Backend = { calls: [], acknowledge: [] };
  const projects: ProjectRecord[] = [
    { id: "p1", name: "P", collapsed: false, created_at: 0, updated_at: 0 , resume_agent_sessions: false },
  ];
  // One alive terminal so `useTerminals` bootstrap adopts it instead of calling
  // `create_terminal` (which, unmocked, would push a null record and crash the
  // auto-attach memo). It is pinned/manual so the auto-attach loop ignores it.
  const rows: TerminalRecord[] = [
    {
      id: "t1",
      cwd: "/p",
      label: null,
      scrollback: "",
      status: "alive",
      order_index: 0,
      created_at: 0,
      updated_at: 0,
      closed_at: null,
      workspace_id: "ws1",
      workspace_binding_mode: "manual",
    },
  ];
  mockIPC(
    (cmd, args) => {
      const a = (args ?? {}) as Record<string, unknown>;
      backend.calls.push({ cmd, args: a });
      switch (cmd) {
        case "list_terminals":
          return rows;
        case "list_projects":
          return projects;
        case "list_workspaces":
          return a.projectId === "p1" ? [workspace("ws1", "p1", "/p")] : [];
        case "command_instance_list":
          return a.workspaceId === "ws1"
            ? instances.map((i) => ({
                id: i.id,
                command_id: "c1",
                workspace_id: "ws1",
                last_state: i.last_state,
                scrollback: "",
                was_running_on_shutdown: false,
                created_at: 0,
                updated_at: 0,
                last_exit_code: i.last_state === "error" ? 1 : i.last_state === "success" ? 0 : null,
                ended_at: i.last_state === "success" || i.last_state === "error" ? 1 : null,
                unread: i.unread ?? false,
                name: i.id,
                command: "bun run dev",
                subfolder: null,
                order_index: 0,
                source_kind: null,
                source_package_json_path: null,
                source_script_name: null,
                package_manager: null,
                workspace_path: "/p",
                cwd: "/p",
              }))
            : [];
        case "command_acknowledge":
          // v4: the bridge returns the UNCHANGED factual state (never collapses to
          // idle); here the seeded instance's last_state is echoed back.
          backend.acknowledge.push(a.instanceId as string);
          return instances.find((i) => i.id === a.instanceId)?.last_state ?? "idle";
        default:
          return null;
      }
    },
    { shouldMockEvents: true },
  );
  return backend;
}

afterEach(() => {
  vi.unstubAllEnvs();
});

describe("<TerminalManager> acknowledge-on-select", () => {
  it("acknowledges an UNREAD SUCCESS command when it is selected", async () => {
    const backend = installBackend([{ id: "ok", last_state: "success", unread: true }]);
    render(<TerminalManager />);
    const btn = await screen.findByRole("button", { name: "select ok" });
    await act(async () => {
      btn.click();
    });
    await waitFor(() => expect(backend.acknowledge).toContain("ok"));
  });

  it("acknowledges an UNREAD ERROR command when it is selected", async () => {
    const backend = installBackend([{ id: "bad", last_state: "error", unread: true }]);
    render(<TerminalManager />);
    const btn = await screen.findByRole("button", { name: "select bad" });
    await act(async () => {
      btn.click();
    });
    await waitFor(() => expect(backend.acknowledge).toContain("bad"));
  });

  it("does NOT re-acknowledge an ALREADY-READ (acknowledged) result on select", async () => {
    // A settled result that has already been acknowledged (unread=false) must not
    // trigger another `command_acknowledge` round-trip — the v4 gating.
    const backend = installBackend([{ id: "seen", last_state: "error", unread: false }]);
    render(<TerminalManager />);
    const btn = await screen.findByRole("button", { name: "select seen" });
    await act(async () => {
      btn.click();
      await new Promise((r) => setTimeout(r, 20));
    });
    expect(backend.acknowledge).toHaveLength(0);
  });

  it("NEVER acknowledges a RUNNING command on select", async () => {
    const backend = installBackend([{ id: "live", last_state: "running" }]);
    render(<TerminalManager />);
    const btn = await screen.findByRole("button", { name: "select live" });
    await act(async () => {
      btn.click();
      await new Promise((r) => setTimeout(r, 20));
    });
    expect(backend.acknowledge).toHaveLength(0);
  });

  it("does NOT acknowledge an already-idle command on select", async () => {
    const backend = installBackend([{ id: "rest", last_state: "idle" }]);
    render(<TerminalManager />);
    const btn = await screen.findByRole("button", { name: "select rest" });
    await act(async () => {
      btn.click();
      await new Promise((r) => setTimeout(r, 20));
    });
    expect(backend.acknowledge).toHaveLength(0);
  });
});
