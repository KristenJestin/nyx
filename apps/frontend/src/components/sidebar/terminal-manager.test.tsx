import { act, render, waitFor } from "@testing-library/react";
import { mockIPC } from "@/bridge/test-harness";
import { useEffect } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { TerminalRecord } from "./use-terminals";
import type { ProjectRecord, WorkspaceRecord } from "./use-projects";

/**
 * Tests for the PRODUCTION auto-attach loop in `<TerminalManager>`
 * (terminal-manager.tsx:201-241) — the glue the e2e deliberately bypasses (it
 * drives auto-attach through the `window.__nyx` seam, which the loop disables
 * under `VITE_NYX_E2E`). The loop's building blocks are covered elsewhere
 * (`useTerminals.autoAttach`, the Rust `decide_attachment`), but the ORCHESTRATION
 * — the loose+auto FILTER (`looseAutoIds`), the `hasWorkspaces` GATE, the e2e
 * disable branch, and the `terminal_info` → `auto_attach_terminal` POLL cadence —
 * shipped untested. These tests mount the real manager (with the real
 * `useTerminals`/`useProjects` hooks against a mocked backend) and isolate the
 * loop by replacing the heavy presentational children so the only thing under
 * test is which terminals the loop polls + auto-attaches.
 */

// --- Isolate the loop: stub the heavy presentational children ----------------
// The deck mounts real xterm `<Terminal>` instances (WebGL canvas, PTY spawns)
// and is what feeds PTY ids back via `onPtyId`. We replace it with a stub that
// simply reports a deterministic PTY id per record (id N → ptyId N), which is
// exactly the `ptyIds` map the loop reads to call `terminal_info`. The sidebar
// and chrome bar render nothing — they are irrelevant to the loop.
vi.mock("./terminal-deck", () => ({
  TerminalDeck: ({
    terminals,
    onPtyId,
  }: {
    terminals: TerminalRecord[];
    onPtyId?: (recordId: string, ptyId: number | null) => void;
  }) => {
    // Report a stable PTY id (= the numeric record id) for each terminal, the
    // same contract the real deck honours once a shell spawns. Fired from an
    // effect (NOT during render) to match the real deck, which reports the PTY
    // id from `usePty`'s post-mount effect — reporting during render would be an
    // illegal parent-state update mid-child-render.
    const ids = terminals.map((t) => t.id).join(",");
    useEffect(() => {
      for (const t of terminals) onPtyId?.(t.id, Number(t.id));
      // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [ids]);
    return null;
  },
}));

vi.mock("./app-sidebar", () => ({
  AppSidebar: () => null,
}));

vi.mock("@/components/chrome/chrome-bar", () => ({
  ChromeBar: () => null,
}));

// useManualAdd mounts the add/edit/delete dialogs (Base UI portals); they have
// nothing to do with the loop, so render an inert surface.
vi.mock("./use-manual-add", () => ({
  useManualAdd: () => ({
    addProject: vi.fn(),
    addWorkspace: vi.fn(),
    editProject: vi.fn(),
    removeProject: vi.fn(),
    dialog: null,
  }),
}));

// Imported AFTER the mocks so they take effect.
import { TerminalManager } from "./terminal-manager";

/** A `terminals` row. `wsId`/`mode` default to a loose, auto-mode terminal. */
function term(
  id: number,
  cwd: string,
  opts: {
    wsId?: string | null;
    mode?: "auto" | "manual";
  } = {},
): TerminalRecord {
  return {
    id: String(id),
    cwd,
    label: null,
    scrollback: "",
    status: "alive",
    order_index: id,
    created_at: 0,
    updated_at: 0,
    closed_at: null,
    workspace_id: opts.wsId ?? null,
    workspace_binding_mode: opts.mode ?? "auto",
  };
}

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

interface BackendCall {
  cmd: string;
  args: Record<string, unknown>;
}

interface FakeBackend {
  /** Every IPC call the loop (and hooks) made, in order. */
  calls: BackendCall[];
  rows: TerminalRecord[];
  /** Calls to `terminal_info`, keyed-arg captured for cadence assertions. */
  terminalInfo: { id: unknown }[];
  /** Calls to `auto_attach_terminal` (the resolver the loop drives). */
  autoAttach: { terminalId: string; cwd: string | null }[];
}

/**
 * Install a fake backend mirroring the relevant command semantics:
 *  - `list_terminals` returns `rows` (the loop's source via `useTerminals`);
 *  - `list_projects` / `list_workspaces` feed `useProjects` (the `hasWorkspaces`
 *    gate + the resolver's known-workspace set);
 *  - `terminal_info(id=ptyId)` returns the cwd registered for that PTY id;
 *  - `auto_attach_terminal(terminalId, cwd)` resolves the workspace whose `path`
 *    equals the cwd and, for an auto-mode terminal, binds it (the real backend
 *    never binds a manual terminal — guarded here too as a safety net, though
 *    the loop must not even ASK for manual terminals).
 */
function installBackend(opts: {
  rows: TerminalRecord[];
  projects?: ProjectRecord[];
  workspaces?: Record<string, WorkspaceRecord[]>;
  /** ptyId → live cwd, what `terminal_info` reports. */
  cwdByPty?: Record<number, string>;
  /** workspace path → workspace id, what `auto_attach_terminal` resolves. */
  workspaceByPath?: Record<string, string>;
}): FakeBackend {
  const backend: FakeBackend = {
    calls: [],
    rows: opts.rows.map((r) => ({ ...r })),
    terminalInfo: [],
    autoAttach: [],
  };
  const projects = opts.projects ?? [];
  const workspaces = opts.workspaces ?? {};
  const cwdByPty = opts.cwdByPty ?? {};
  const workspaceByPath = opts.workspaceByPath ?? {};

  mockIPC((cmd, args) => {
    const a = (args ?? {}) as Record<string, unknown>;
    backend.calls.push({ cmd, args: a });
    switch (cmd) {
      case "list_terminals":
        return [...backend.rows].sort((x, y) => x.order_index - y.order_index);
      case "list_projects":
        return projects;
      case "list_workspaces":
        return workspaces[a.projectId as string] ?? [];
      case "set_active":
        return null;
      case "terminal_info": {
        backend.terminalInfo.push({ id: a.id });
        const cwd = cwdByPty[a.id as number] ?? null;
        return { cwd, foreground: null };
      }
      case "auto_attach_terminal": {
        const terminalId = a.terminalId as string;
        const cwd = a.cwd as string | null;
        backend.autoAttach.push({ terminalId, cwd });
        const row = backend.rows.find((r) => r.id === terminalId);
        const matchWs = cwd ? workspaceByPath[cwd] : undefined;
        if (row && matchWs && row.workspace_binding_mode !== "manual") {
          row.workspace_id = matchWs;
          row.workspace_binding_mode = "auto";
          return { workspace_id: matchWs, changed: true };
        }
        return { workspace_id: row?.workspace_id ?? null, changed: false };
      }
      // The loop never issues these, but the hooks might on other paths.
      case "attach_terminal":
        return null;
      case "create_terminal":
        return term(999, (a.cwd as string) ?? ".");
      default:
        return null;
    }
  });
  return backend;
}

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllEnvs();
});

/**
 * Let the bootstrap (`list_terminals`/`list_projects`) microtasks, the deck's
 * PTY-id effect, and the loop's immediate mount pass all settle — wrapped in
 * `act` so the resulting state updates are flushed inside React's batch (no
 * "update not wrapped in act" warning). `ms` comfortably clears one 1500ms poll
 * tick so the NEGATIVE cases prove the loop stayed inert, not merely that it had
 * not ticked yet.
 */
async function settle(ms = 1700): Promise<void> {
  await act(async () => {
    await new Promise((r) => setTimeout(r, ms));
  });
}

describe("<TerminalManager> production auto-attach loop", () => {
  it("auto-attaches a loose + auto terminal whose live cwd resolves to a workspace", async () => {
    // A loose (no workspace_id), auto-mode terminal. Its PTY id (= 1, per the
    // deck stub) reports cwd "/work/ws", which is a known workspace path.
    const backend = installBackend({
      rows: [term(1, "/work/ws")],
      projects: [{ id: "p1", name: "P", collapsed: false, created_at: 0, updated_at: 0 , resume_agent_sessions: false }],
      workspaces: { p1: [workspace("ws-1", "p1", "/work/ws")] },
      cwdByPty: { 1: "/work/ws" },
      workspaceByPath: { "/work/ws": "ws-1" },
    });

    render(<TerminalManager />);

    // The loop polls `terminal_info` → `auto_attach_terminal` for the loose+auto
    // terminal (its immediate mount pass and/or the 1500ms interval — fake timers
    // would mask the PTY-id race against the deck's effect, so wait on the real
    // poll cadence with a generous budget).
    await waitFor(
      () => {
        expect(backend.autoAttach).toContainEqual({
          terminalId: "1",
          cwd: "/work/ws",
        });
      },
      { timeout: 4000 },
    );
    // It read the live cwd via terminal_info first (keyed on the PTY id).
    expect(backend.terminalInfo).toContainEqual({ id: 1 });
    // The backend bound it → the row moved OUT of loose into the workspace.
    await waitFor(() => {
      const row = backend.rows.find((r) => r.id === "1");
      expect(row?.workspace_id).toBe("ws-1");
    });
  });

  it("NEVER passes a manual/pinned terminal to auto_attach_terminal", async () => {
    // One manual (pinned) terminal and one already-attached auto terminal. The
    // loop's filter keeps to LOOSE + auto only, so neither is polled/attached.
    const backend = installBackend({
      rows: [
        term(1, "/work/ws", { mode: "manual" }), // pinned → never auto-attached
        term(2, "/work/ws", { wsId: "ws-1", mode: "auto" }), // already attached
      ],
      projects: [{ id: "p1", name: "P", collapsed: false, created_at: 0, updated_at: 0 , resume_agent_sessions: false }],
      workspaces: { p1: [workspace("ws-1", "p1", "/work/ws")] },
      cwdByPty: { 1: "/work/ws", 2: "/work/ws" },
      workspaceByPath: { "/work/ws": "ws-1" },
    });

    render(<TerminalManager />);
    // Settle past a poll tick and prove the loop stayed empty.
    await settle();

    // The loop filtered BOTH out: no terminal_info poll, no auto_attach call.
    expect(backend.autoAttach).toHaveLength(0);
    expect(backend.terminalInfo).toHaveLength(0);
    // The pinned terminal is untouched (still loose-less / manual on its row).
    const pinned = backend.rows.find((r) => r.id === "1");
    expect(pinned?.workspace_binding_mode).toBe("manual");
  });

  it("does NOTHING when there are no workspaces (hasWorkspaces gate)", async () => {
    // A loose + auto terminal exists, but there are NO projects/workspaces to
    // match against → the gate keeps the loop inert (never polls terminal_info).
    const backend = installBackend({
      rows: [term(1, "/work/ws")],
      projects: [],
      cwdByPty: { 1: "/work/ws" },
    });

    render(<TerminalManager />);
    await settle();

    expect(backend.terminalInfo).toHaveLength(0);
    expect(backend.autoAttach).toHaveLength(0);
  });

  it("polls again on the interval, re-attaching a terminal whose cwd later resolves", async () => {
    // The terminal starts in a directory that matches NO workspace; on a later
    // cd (its terminal_info cwd changes) a subsequent poll resolves + attaches
    // it. This proves the loop is a RECURRING poll (setInterval 1500ms), not a
    // one-shot — a later cwd change is picked up without any explicit trigger.
    const cwdByPty: Record<number, string> = { 1: "/elsewhere" };
    const backend = installBackend({
      rows: [term(1, "/elsewhere")],
      projects: [{ id: "p1", name: "P", collapsed: false, created_at: 0, updated_at: 0 , resume_agent_sessions: false }],
      workspaces: { p1: [workspace("ws-1", "p1", "/work/ws")] },
      cwdByPty,
      workspaceByPath: { "/work/ws": "ws-1" },
    });

    render(<TerminalManager />);

    // First poll (after the PTY id lands via the deck): cwd "/elsewhere" → no
    // match, the terminal stays loose (no binding).
    await waitFor(
      () => {
        expect(backend.autoAttach).toContainEqual({
          terminalId: "1",
          cwd: "/elsewhere",
        });
      },
      { timeout: 4000 },
    );
    expect(backend.rows.find((r) => r.id === "1")?.workspace_id).toBeNull();

    // The shell cd'd into the workspace; a later poll picks up the new cwd and
    // binds it — without any explicit re-trigger.
    cwdByPty[1] = "/work/ws";
    await waitFor(
      () => {
        expect(backend.autoAttach).toContainEqual({
          terminalId: "1",
          cwd: "/work/ws",
        });
      },
      { timeout: 4000 },
    );
    expect(backend.rows.find((r) => r.id === "1")?.workspace_id).toBe("ws-1");
  });

  it("is INERT under VITE_NYX_E2E=1 (the seam drives auto-attach instead)", async () => {
    vi.stubEnv("VITE_NYX_E2E", "1");
    const backend = installBackend({
      rows: [term(1, "/work/ws")],
      projects: [{ id: "p1", name: "P", collapsed: false, created_at: 0, updated_at: 0 , resume_agent_sessions: false }],
      workspaces: { p1: [workspace("ws-1", "p1", "/work/ws")] },
      cwdByPty: { 1: "/work/ws" },
      workspaceByPath: { "/work/ws": "ws-1" },
    });

    render(<TerminalManager />);
    // Settle well past a poll tick: with the loop ENABLED this is enough time to
    // fire (the positive tests bind in ~1500ms), so a still-empty backend proves
    // the e2e flag genuinely disabled the loop — not just that it had not ticked.
    await settle();

    // The background loop is disabled under the e2e flag → no poll, no attach.
    expect(backend.terminalInfo).toHaveLength(0);
    expect(backend.autoAttach).toHaveLength(0);
  });
});
