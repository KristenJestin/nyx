import { act, renderHook, waitFor } from "@testing-library/react";
import { mockIPC } from "@tauri-apps/api/mocks";
import { StrictMode } from "react";
import { beforeEach, describe, expect, it } from "vitest";

import { useTerminals, type TerminalRecord } from "./use-terminals";

/**
 * A fake backend that mirrors the Diesel `terminals` CRUD in memory, so the hook
 * is exercised against realistic command semantics (auto-order on create,
 * status flip on close, order persistence on reorder) without a real backend.
 */
interface FakeBackend {
  rows: TerminalRecord[];
  /** Every `create_terminal` cwd, for asserting what the hook created. */
  createdCwds: string[];
  /** Every `reorder` id-array the hook persisted. */
  reorders: string[][];
  /** Ids passed to `close_terminal`. */
  closed: string[];
  /** Every `attach_terminal` call, for asserting workspace binding. */
  attaches: { terminalId: string; workspaceId: string; mode: string }[];
  /** Map of cwd → workspace id, the fake `auto_attach_terminal` resolves against. */
  workspacePaths: Record<string, string>;
}

function installBackend(
  initial: TerminalRecord[] = [],
  workspacePaths: Record<string, string> = {},
): FakeBackend {
  const backend: FakeBackend = {
    rows: initial.map((r) => ({ ...r })),
    createdCwds: [],
    reorders: [],
    closed: [],
    attaches: [],
    workspacePaths,
  };
  let nextId = initial.length + 1;

  mockIPC((cmd, args) => {
    const a = (args ?? {}) as Record<string, unknown>;
    switch (cmd) {
      case "list_terminals":
        return [...backend.rows].sort(
          (x, y) => x.order_index - y.order_index || x.id.localeCompare(y.id),
        );
      case "create_terminal": {
        const order = backend.rows.reduce((m, r) => Math.max(m, r.order_index), -1) + 1;
        const row: TerminalRecord = {
          id: String(nextId++),
          cwd: a.cwd as string,
          label: (a.label as string | null) ?? null,
          scrollback: "",
          status: "alive",
          order_index: order,
          created_at: 0,
          updated_at: 0,
          closed_at: null,
        };
        backend.rows.push(row);
        backend.createdCwds.push(row.cwd);
        return row;
      }
      case "close_terminal": {
        const id = a.id as string;
        backend.closed.push(id);
        const row = backend.rows.find((r) => r.id === id);
        if (row) row.status = "closed";
        return null;
      }
      case "reorder": {
        const ids = a.ids as string[];
        backend.reorders.push(ids);
        ids.forEach((id, idx) => {
          const row = backend.rows.find((r) => r.id === id);
          if (row) row.order_index = idx;
        });
        return null;
      }
      case "rename": {
        const row = backend.rows.find((r) => r.id === (a.id as string));
        if (row) row.label = (a.label as string | null) ?? null;
        return null;
      }
      case "attach_terminal": {
        const row = backend.rows.find((r) => r.id === (a.terminalId as string));
        if (row) {
          row.workspace_id = a.workspaceId as string;
          row.workspace_binding_mode = a.mode as "auto" | "manual";
        }
        backend.attaches.push({
          terminalId: a.terminalId as string,
          workspaceId: a.workspaceId as string,
          mode: a.mode as string,
        });
        return null;
      }
      case "auto_attach_terminal": {
        // The fake resolver: attach to the workspace whose path === the cwd.
        const id = a.terminalId as string;
        const cwd = a.cwd as string | null;
        const row = backend.rows.find((r) => r.id === id);
        const matchWs = cwd ? backend.workspacePaths[cwd] : undefined;
        if (row && matchWs && row.workspace_binding_mode !== "manual") {
          row.workspace_id = matchWs;
          row.workspace_binding_mode = "auto";
          return { workspace_id: matchWs, changed: true };
        }
        return { workspace_id: row?.workspace_id ?? null, changed: false };
      }
      // PTY commands the mounted <Terminal> would issue are not under test here.
      case "pty_spawn":
        return 1;
      default:
        return null;
    }
  });
  return backend;
}

function aliveRow(id: number, cwd: string, order: number): TerminalRecord {
  return {
    id: String(id),
    cwd,
    label: null,
    scrollback: "",
    status: "alive",
    order_index: order,
    created_at: 0,
    updated_at: 0,
    closed_at: null,
  };
}

describe("useTerminals", () => {
  beforeEach(() => {
    // mockIPC installed per-test via installBackend.
  });

  it("creates a default terminal when none exist and marks it active", async () => {
    const backend = installBackend([]);
    const { result } = renderHook(() => useTerminals());

    await waitFor(() => expect(result.current.terminals).toHaveLength(1));
    expect(backend.createdCwds).toHaveLength(1);
    expect(result.current.activeId).toBe(result.current.terminals[0].id);
  });

  it("adopts existing alive records on mount (no default created)", async () => {
    const backend = installBackend([aliveRow(10, "/a", 0), aliveRow(11, "/b", 1)]);
    const { result } = renderHook(() => useTerminals());

    await waitFor(() => expect(result.current.terminals).toHaveLength(2));
    // Did NOT create a new one â€” adopted the existing rows.
    expect(backend.createdCwds).toHaveLength(0);
    expect(result.current.terminals.map((t) => t.id)).toEqual(["10", "11"]);
    // First in order is active.
    expect(result.current.activeId).toBe("10");
  });

  it("restores the LAST-active terminal (greatest last_active_at), not the first", async () => {
    installBackend([
      aliveRow(10, "/a", 0),
      { ...aliveRow(11, "/b", 1), last_active_at: 5_000 },
      { ...aliveRow(12, "/c", 2), last_active_at: 2_000 },
    ]);
    const { result } = renderHook(() => useTerminals());
    await waitFor(() => expect(result.current.terminals).toHaveLength(3));
    // id 11 was active most recently → it reopens active, not the first in order.
    expect(result.current.activeId).toBe("11");
  });

  it("bootstraps under React StrictMode (dev double-invoke) — adopts, NOT empty", async () => {
    // Regression guard: <React.StrictMode> mounts→unmounts→remounts the hook in
    // dev. A prior `cancelled`-flag bootstrap discarded its result on that fake
    // unmount, so `tauri dev` opened with ZERO terminals. The wrapper reproduces
    // the double-invoke; the hook must still adopt the alive records exactly once.
    installBackend([aliveRow(1, "/a", 0), aliveRow(2, "/b", 1)]);
    const { result } = renderHook(() => useTerminals(), { wrapper: StrictMode });
    await waitFor(() => expect(result.current.terminals).toHaveLength(2));
    expect(result.current.terminals.map((t) => t.id)).toEqual(["1", "2"]);
    expect(result.current.activeId).toBe("1");
  });

  it("create() appends a terminal and activates it", async () => {
    installBackend([aliveRow(1, "/a", 0)]);
    const { result } = renderHook(() => useTerminals());
    await waitFor(() => expect(result.current.terminals).toHaveLength(1));

    await act(async () => {
      await result.current.create();
    });

    expect(result.current.terminals).toHaveLength(2);
    const newId = result.current.terminals[1].id;
    expect(result.current.activeId).toBe(newId);
  });

  it("create(cwd) opens at the given cwd and returns the new record (per-workspace +)", async () => {
    const backend = installBackend([aliveRow(1, "/a", 0)]);
    const { result } = renderHook(() => useTerminals());
    await waitFor(() => expect(result.current.terminals).toHaveLength(1));

    let created: TerminalRecord | undefined;
    await act(async () => {
      created = await result.current.create("/work/ws");
    });
    // Opened at the workspace cwd and the record is returned for attach.
    expect(backend.createdCwds).toContain("/work/ws");
    expect(created?.cwd).toBe("/work/ws");
  });

  it("attach() binds a terminal to a workspace (local + attach_terminal)", async () => {
    const backend = installBackend([aliveRow(1, "/a", 0)]);
    const { result } = renderHook(() => useTerminals());
    await waitFor(() => expect(result.current.terminals).toHaveLength(1));

    let created: TerminalRecord | undefined;
    await act(async () => {
      created = await result.current.create("/work/ws");
      await result.current.attach(created!.id, "ws-1", "manual");
    });

    // Persisted via attach_terminal with the right args…
    expect(backend.attaches).toContainEqual({
      terminalId: created!.id,
      workspaceId: "ws-1",
      mode: "manual",
    });
    // …and reflected locally so the spine groups it under the workspace.
    const bound = result.current.terminals.find((t) => t.id === created!.id);
    expect(bound?.workspace_id).toBe("ws-1");
  });

  it("pin/unpin a terminal's workspace binding via attach (manual ↔ auto)", async () => {
    // ZE2 done-criterion: a test covers pin/unpin. Pin/unpin is NOT a sidebar UI
    // control in this phase (the spine has no pin button); it is reachable at the
    // hook/command level through the binding MODE: `attach(id, ws, "manual")` PINS
    // the terminal to a workspace (a later cd no longer moves it), and
    // `attach(id, ws, "auto")` UNPINS it (auto-attach resumes). This drives that
    // seam and asserts the persisted mode + the local reflection the spine reads.
    const backend = installBackend([aliveRow(1, "/a", 0)]);
    const { result } = renderHook(() => useTerminals());
    await waitFor(() => expect(result.current.terminals).toHaveLength(1));

    let created: TerminalRecord | undefined;
    await act(async () => {
      created = await result.current.create("/work/ws");
      // PIN: bind to the workspace in manual mode.
      await result.current.attach(created!.id, "ws-1", "manual");
    });

    // Persisted as a PIN (mode manual) on the right workspace…
    expect(backend.attaches).toContainEqual({
      terminalId: created!.id,
      workspaceId: "ws-1",
      mode: "manual",
    });
    // …and reflected locally as a manual (pinned) binding.
    let row = result.current.terminals.find((t) => t.id === created!.id);
    expect(row?.workspace_id).toBe("ws-1");
    expect(row?.workspace_binding_mode).toBe("manual");

    // UNPIN: re-attach the same workspace in auto mode (auto-attach resumes).
    await act(async () => {
      await result.current.attach(created!.id, "ws-1", "auto");
    });
    expect(backend.attaches).toContainEqual({
      terminalId: created!.id,
      workspaceId: "ws-1",
      mode: "auto",
    });
    row = result.current.terminals.find((t) => t.id === created!.id);
    expect(row?.workspace_binding_mode).toBe("auto");
    // The last persisted call is the unpin (mode auto), not a stale manual.
    expect(backend.attaches[backend.attaches.length - 1].mode).toBe("auto");
  });

  it("close() removes the terminal and activates a neighbour", async () => {
    installBackend([aliveRow(1, "/a", 0), aliveRow(2, "/b", 1), aliveRow(3, "/c", 2)]);
    const { result } = renderHook(() => useTerminals());
    await waitFor(() => expect(result.current.terminals).toHaveLength(3));

    // Activate the middle one, then close it.
    act(() => result.current.setActive("2"));
    await act(async () => {
      await result.current.close("2");
    });

    expect(result.current.terminals.map((t) => t.id)).toEqual(["1", "3"]);
    // Active fell to a surviving neighbour (the previous index â†’ id 1, or next).
    expect(["1", "3"]).toContain(result.current.activeId);
    expect(result.current.activeId).not.toBe("2");
  });

  it("close() persists the closed status via close_terminal", async () => {
    const backend = installBackend([aliveRow(1, "/a", 0), aliveRow(2, "/b", 1)]);
    const { result } = renderHook(() => useTerminals());
    await waitFor(() => expect(result.current.terminals).toHaveLength(2));

    await act(async () => {
      await result.current.close("1");
    });
    expect(backend.closed).toContain("1");
  });

  it("activeNext / activePrev cycle through terminals", async () => {
    installBackend([aliveRow(1, "/a", 0), aliveRow(2, "/b", 1), aliveRow(3, "/c", 2)]);
    const { result } = renderHook(() => useTerminals());
    await waitFor(() => expect(result.current.terminals).toHaveLength(3));

    expect(result.current.activeId).toBe("1");
    act(() => result.current.activeNext());
    expect(result.current.activeId).toBe("2");
    act(() => result.current.activeNext());
    expect(result.current.activeId).toBe("3");
    // Wraps around.
    act(() => result.current.activeNext());
    expect(result.current.activeId).toBe("1");
    // Prev wraps the other way.
    act(() => result.current.activePrev());
    expect(result.current.activeId).toBe("3");
  });

  it("autoAttach() binds a loose terminal when its cwd resolves to a workspace", async () => {
    // A loose terminal (no workspace_id) whose live cwd matches a known
    // workspace path auto-attaches: the backend binds it and the hook reflects
    // the new workspace_id (so the sidebar moves it out of the loose section).
    installBackend([aliveRow(1, "/work/ws", 0)], { "/work/ws": "ws-1" });
    const { result } = renderHook(() => useTerminals());
    await waitFor(() => expect(result.current.terminals).toHaveLength(1));
    expect(result.current.terminals[0].workspace_id).toBeUndefined();

    let changed: boolean | undefined;
    await act(async () => {
      changed = await result.current.autoAttach("1", "/work/ws");
    });
    expect(changed).toBe(true);
    expect(result.current.terminals[0].workspace_id).toBe("ws-1");
    expect(result.current.terminals[0].workspace_binding_mode).toBe("auto");
  });

  it("autoAttach() is a no-op when the cwd matches no known workspace", async () => {
    installBackend([aliveRow(1, "/elsewhere", 0)], { "/work/ws": "ws-1" });
    const { result } = renderHook(() => useTerminals());
    await waitFor(() => expect(result.current.terminals).toHaveLength(1));

    let changed: boolean | undefined;
    await act(async () => {
      changed = await result.current.autoAttach("1", "/elsewhere");
    });
    expect(changed).toBe(false);
    expect(result.current.terminals[0].workspace_id).toBeUndefined();
  });

  it("reorder() reflects the new order and persists it", async () => {
    const backend = installBackend([
      aliveRow(1, "/a", 0),
      aliveRow(2, "/b", 1),
      aliveRow(3, "/c", 2),
    ]);
    const { result } = renderHook(() => useTerminals());
    await waitFor(() => expect(result.current.terminals).toHaveLength(3));

    await act(async () => {
      await result.current.reorder(["3", "1", "2"]);
    });

    expect(result.current.terminals.map((t) => t.id)).toEqual(["3", "1", "2"]);
    expect(backend.reorders[backend.reorders.length - 1]).toEqual(["3", "1", "2"]);
  });
});
