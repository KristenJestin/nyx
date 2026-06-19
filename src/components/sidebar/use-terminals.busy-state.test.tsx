import { act, renderHook, waitFor } from "@testing-library/react";
import { emit } from "@tauri-apps/api/event";
import { mockIPC } from "@tauri-apps/api/mocks";
import { describe, expect, it } from "vitest";

import { useTerminals, type TerminalRecord } from "./use-terminals";

/**
 * PRD task #1: the FRONTEND busy/idle wiring of `useTerminals` — consuming the
 * backend `terminal://busy-state` event (keyed by `terminal_id`) and folding its
 * `busy` flag onto the matching record WITHOUT a full reload. This is the AUTHORITY
 * for the running dot, and it is INDEPENDENT of OSC 133 / `exec_state`: a
 * busy-state event flips `busy` regardless of what `exec_state` says, and an
 * `exec_state` event never touches `busy`. Events are driven via `emit` (the IPC
 * mock is installed with `shouldMockEvents: true` so `emit` reaches `listen`).
 */

function installBackend(initial: TerminalRecord[]) {
  const rows = initial.map((r) => ({ ...r }));
  mockIPC(
    (cmd) => {
      switch (cmd) {
        case "list_terminals":
          return [...rows].sort(
            (x, y) => x.order_index - y.order_index || x.id.localeCompare(y.id),
          );
        case "set_active":
        case "pty_spawn":
          return null;
        default:
          return null;
      }
    },
    { shouldMockEvents: true },
  );
}

function aliveRow(id: string, exec_state: TerminalRecord["exec_state"] = "idle"): TerminalRecord {
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
    exec_state_unread: false,
    exec_exit_code: null,
  };
}

function find(rows: TerminalRecord[], id: string): TerminalRecord | undefined {
  return rows.find((t) => t.id === id);
}

describe("useTerminals busy-state wiring (PRD task #1)", () => {
  it("folds busy=true/false from terminal://busy-state onto the right record (no reload)", async () => {
    installBackend([aliveRow("0"), aliveRow("1")]);
    const { result } = renderHook(() => useTerminals());
    await waitFor(() => expect(result.current.terminals).toHaveLength(2));

    // Idle by default until the first event.
    expect(find(result.current.terminals, "1")?.busy ?? false).toBe(false);

    await act(async () => {
      await emit("terminal://busy-state", { terminal_id: "1", busy: true });
    });
    await waitFor(() => expect(find(result.current.terminals, "1")?.busy).toBe(true));
    // The other terminal is untouched (folded onto the addressed record only).
    expect(find(result.current.terminals, "0")?.busy ?? false).toBe(false);

    await act(async () => {
      await emit("terminal://busy-state", { terminal_id: "1", busy: false });
    });
    await waitFor(() => expect(find(result.current.terminals, "1")?.busy).toBe(false));
  });

  it("busy is independent of exec_state (OSC 133): the dot signal does not need OSC 133", async () => {
    // A terminal whose exec_state is 'idle' (no OSC 133 ever fired) still becomes
    // busy from the OS signal — proving the running dot no longer derives from the
    // OSC-133 exec_state.
    installBackend([aliveRow("0", "idle")]);
    const { result } = renderHook(() => useTerminals());
    await waitFor(() => expect(result.current.terminals).toHaveLength(1));

    await act(async () => {
      await emit("terminal://busy-state", { terminal_id: "0", busy: true });
    });
    await waitFor(() => expect(find(result.current.terminals, "0")?.busy).toBe(true));
    // exec_state is left exactly as it was — busy and exec_state are separate channels.
    expect(find(result.current.terminals, "0")?.exec_state).toBe("idle");
  });

  it("a busy-state event for an unknown record is ignored", async () => {
    installBackend([aliveRow("0")]);
    const { result } = renderHook(() => useTerminals());
    await waitFor(() => expect(result.current.terminals).toHaveLength(1));
    const before = result.current.terminals;

    await act(async () => {
      await emit("terminal://busy-state", { terminal_id: "nope", busy: true });
    });
    // No record changed (same array identity preserved by the functional updater).
    expect(result.current.terminals).toBe(before);
  });
});
