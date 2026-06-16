import { act, renderHook, waitFor } from "@testing-library/react";
import { emit } from "@tauri-apps/api/event";
import { mockIPC } from "@tauri-apps/api/mocks";
import { beforeEach, describe, expect, it } from "vitest";

import { useTerminals, type TerminalRecord } from "./use-terminals";

/**
 * PRD-2.1 task #7: the FRONTEND exec-state behavior of `useTerminals` —
 * consuming `terminal://exec-state` events (keyed by `terminal_id`) WITHOUT a
 * full reload, the persisted-unread model, and the `markRead` (mark-read-on-view)
 * path. Events are driven via `emit` (the IPC mock is installed with
 * `shouldMockEvents: true` so `emit` reaches the hook's `listen`).
 */

interface ExecBackend {
  rows: TerminalRecord[];
  /** Ids passed to `terminal_exec_mark_read`. */
  markedRead: string[];
}

function installBackend(initial: TerminalRecord[]): ExecBackend {
  const backend: ExecBackend = {
    rows: initial.map((r) => ({ ...r })),
    markedRead: [],
  };
  mockIPC(
    (cmd, args) => {
      const a = (args ?? {}) as Record<string, unknown>;
      switch (cmd) {
        case "list_terminals":
          return [...backend.rows].sort(
            (x, y) => x.order_index - y.order_index || x.id.localeCompare(y.id),
          );
        case "terminal_exec_mark_read": {
          const id = a.id as string;
          backend.markedRead.push(id);
          const row = backend.rows.find((r) => r.id === id);
          if (row) row.exec_state_unread = false;
          return null;
        }
        case "set_active":
        case "pty_spawn":
          return null;
        default:
          return null;
      }
    },
    { shouldMockEvents: true },
  );
  return backend;
}

function aliveRow(
  id: string,
  exec_state: TerminalRecord["exec_state"] = "idle",
  exec_state_unread = false,
  exec_exit_code: number | null = null,
): TerminalRecord {
  return {
    id,
    cwd: "/x",
    label: null,
    scrollback: "",
    status: "alive",
    order_index: Number(id),
    created_at: 0,
    updated_at: 0,
    closed_at: null,
    exec_state,
    exec_state_unread,
    exec_exit_code,
  };
}

function find(rows: TerminalRecord[], id: string): TerminalRecord | undefined {
  return rows.find((t) => t.id === id);
}

describe("useTerminals exec-state wiring (PRD-2.1)", () => {
  beforeEach(() => {
    // mockIPC installed per-test via installBackend.
  });

  it("restart data from list_terminals renders the persisted exec-state on startup", async () => {
    installBackend([
      aliveRow("0", "idle"),
      aliveRow("1", "success", true, 0),
      aliveRow("2", "error", false, 2), // already-read settled result survives
    ]);
    const { result } = renderHook(() => useTerminals());
    await waitFor(() => expect(result.current.terminals).toHaveLength(3));

    expect(find(result.current.terminals, "1")).toMatchObject({
      exec_state: "success",
      exec_state_unread: true,
      exec_exit_code: 0,
    });
    expect(find(result.current.terminals, "2")).toMatchObject({
      exec_state: "error",
      exec_state_unread: false,
      exec_exit_code: 2,
    });
  });

  it("running updates immediately on terminal://exec-state (no full reload)", async () => {
    installBackend([aliveRow("0", "idle"), aliveRow("1", "idle")]);
    const { result } = renderHook(() => useTerminals());
    await waitFor(() => expect(result.current.terminals).toHaveLength(2));

    await act(async () => {
      await emit("terminal://exec-state", {
        terminal_id: "1",
        state: "running",
        exit_code: null,
        unread: false,
        updated_at: 123,
      });
    });

    await waitFor(() => expect(find(result.current.terminals, "1")?.exec_state).toBe("running"));
    expect(find(result.current.terminals, "1")?.exec_state_unread).toBe(false);
    // The OTHER terminal is untouched (folded onto the right record only).
    expect(find(result.current.terminals, "0")?.exec_state).toBe("idle");
  });

  it("success/error on an inactive terminal arrives UNREAD (the settled-badge driver)", async () => {
    installBackend([aliveRow("0", "idle"), aliveRow("1", "idle")]);
    const { result } = renderHook(() => useTerminals());
    await waitFor(() => expect(result.current.terminals).toHaveLength(2));

    await act(async () => {
      await emit("terminal://exec-state", {
        terminal_id: "1",
        state: "error",
        exit_code: 1,
        unread: true,
        updated_at: 456,
      });
    });

    await waitFor(() => expect(find(result.current.terminals, "1")?.exec_state).toBe("error"));
    expect(find(result.current.terminals, "1")).toMatchObject({
      exec_state: "error",
      exec_state_unread: true,
      exec_exit_code: 1,
    });
  });

  it("success on an inactive terminal arrives UNREAD with its exit code (settled-badge driver)", async () => {
    // The PRD success path on a NON-active terminal: a green settled badge must
    // show, which is driven by the persisted `unread` bit (here true). Mirrors the
    // error case above so BOTH settled outcomes are proven at the hook level.
    installBackend([aliveRow("0", "running"), aliveRow("1", "idle")]);
    const { result } = renderHook(() => useTerminals());
    await waitFor(() => expect(result.current.terminals).toHaveLength(2));

    await act(async () => {
      await emit("terminal://exec-state", {
        terminal_id: "1",
        state: "success",
        exit_code: 0,
        unread: true,
        updated_at: 789,
      });
    });

    await waitFor(() => expect(find(result.current.terminals, "1")?.exec_state).toBe("success"));
    expect(find(result.current.terminals, "1")).toMatchObject({
      exec_state: "success",
      exec_state_unread: true,
      exec_exit_code: 0,
    });
    // The other terminal's running state is untouched (event folded onto id "1" only).
    expect(find(result.current.terminals, "0")?.exec_state).toBe("running");
  });

  it("markRead clears unread locally, invokes terminal_exec_mark_read, and PRESERVES the settled result", async () => {
    const backend = installBackend([aliveRow("0", "success", true, 0)]);
    const { result } = renderHook(() => useTerminals());
    await waitFor(() => expect(find(result.current.terminals, "0")?.exec_state_unread).toBe(true));

    await act(async () => {
      result.current.markRead("0");
    });

    // Local clear is immediate; the settled state + exit code survive.
    expect(find(result.current.terminals, "0")).toMatchObject({
      exec_state: "success",
      exec_state_unread: false,
      exec_exit_code: 0,
    });
    await waitFor(() => expect(backend.markedRead).toContain("0"));
  });

  it("markRead is a no-op (no round-trip) for a terminal that is already read", async () => {
    const backend = installBackend([aliveRow("0", "success", false, 0)]);
    const { result } = renderHook(() => useTerminals());
    await waitFor(() => expect(result.current.terminals).toHaveLength(1));

    await act(async () => {
      result.current.markRead("0");
    });

    expect(backend.markedRead).toHaveLength(0);
  });

  it("a settled badge stays hidden after re-deselect once read (user story #3 — unread is the only driver)", async () => {
    // Emit an unread error, mark it read, then re-emit nothing: the record stays
    // read so the badge would not re-appear (the component is unread-driven).
    installBackend([aliveRow("0", "idle")]);
    const { result } = renderHook(() => useTerminals());
    await waitFor(() => expect(result.current.terminals).toHaveLength(1));

    await act(async () => {
      await emit("terminal://exec-state", {
        terminal_id: "0",
        state: "error",
        exit_code: 1,
        unread: true,
        updated_at: 1,
      });
    });
    await waitFor(() => expect(find(result.current.terminals, "0")?.exec_state_unread).toBe(true));

    await act(async () => {
      result.current.markRead("0");
    });
    // Read: the badge driver (`exec_state_unread`) is false; `exec_state` keeps
    // its error color but the badge component renders nothing for a read settle.
    expect(find(result.current.terminals, "0")).toMatchObject({
      exec_state: "error",
      exec_state_unread: false,
    });
  });

  it("ignores a terminal://exec-state event for a record it does not hold", async () => {
    installBackend([aliveRow("0", "idle")]);
    const { result } = renderHook(() => useTerminals());
    await waitFor(() => expect(result.current.terminals).toHaveLength(1));
    const before = result.current.terminals;

    await act(async () => {
      await emit("terminal://exec-state", {
        terminal_id: "does-not-exist",
        state: "running",
        exit_code: null,
        unread: false,
        updated_at: 9,
      });
    });

    // No record matched → the list array reference is unchanged (no churn).
    expect(result.current.terminals).toBe(before);
  });
});
