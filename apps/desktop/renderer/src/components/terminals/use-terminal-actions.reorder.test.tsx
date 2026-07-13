import { act, renderHook, waitFor } from "@testing-library/react";
import { mockIPC } from "@/bridge/test-harness";
import type { ReactNode } from "react";
import { describe, expect, it } from "vitest";

import type { TerminalRecord } from "@/components/sidebar/use-terminals";
import { RuntimeSelectionProvider } from "./runtime-selection";
import { useTerminalActions } from "./use-terminal-actions";

/**
 * Drag-reorder persistence for the v2.5.6 cockpit terminal rows (task
 * `01KXBNX51EPQ69MVKYG4W960HD`). `useTerminalActions.reorderTerminals(group, ids)` is the seam the
 * sortable list commits a drop through: it must splice the group's new order into the GLOBAL
 * terminal sequence (other groups keep their slots) and persist it via the backend `reorder`
 * (each terminal's `order_index`), and it must reflect the new order optimistically on
 * `orderedTerminalIds` (the tree's live-order authority) so a drop repaints without a re-pull.
 */

function term(id: string, workspaceId: string | null, orderIndex: number): TerminalRecord {
  return {
    id,
    cwd: `/${id}`,
    label: null,
    scrollback: "",
    status: "alive",
    order_index: orderIndex,
    created_at: 0,
    updated_at: 0,
    closed_at: null,
    workspace_id: workspaceId,
    workspace_binding_mode: "manual",
  };
}

interface Backend {
  rows: TerminalRecord[];
  reorders: string[][];
}

function installBackend(initial: TerminalRecord[]): Backend {
  const backend: Backend = { rows: initial.map((r) => ({ ...r })), reorders: [] };
  mockIPC((cmd, args) => {
    const a = (args ?? {}) as Record<string, unknown>;
    switch (cmd) {
      case "list_terminals":
        return [...backend.rows].sort(
          (x, y) => x.order_index - y.order_index || x.id.localeCompare(y.id),
        );
      case "reorder": {
        const ids = a.ids as string[];
        backend.reorders.push(ids);
        ids.forEach((id, idx) => {
          const row = backend.rows.find((r) => r.id === id);
          if (row) row.order_index = idx;
        });
        return null;
      }
      case "list_projects":
        return [];
      case "list_workspaces":
        return [];
      default:
        return null;
    }
  });
  return backend;
}

function wrapper({ children }: { children: ReactNode }) {
  return <RuntimeSelectionProvider>{children}</RuntimeSelectionProvider>;
}

describe("useTerminalActions.reorderTerminals (drag persistence)", () => {
  it("splices a WORKSPACE reorder into the global order and persists it via `reorder`", async () => {
    // Global order A(ws1) B(ws1) C(loose): reordering ws1 to [B,A] keeps C's slot.
    const backend = installBackend([term("A", "ws1", 0), term("B", "ws1", 1), term("C", null, 2)]);
    const { result } = renderHook(() => useTerminalActions([]), { wrapper });

    await waitFor(() => expect(result.current.orderedTerminalIds).toEqual(["A", "B", "C"]));

    act(() => result.current.reorderTerminals("ws1", ["B", "A"]));

    // Persisted the FULL spliced global sequence (ws1 slots filled B,A; C untouched).
    expect(backend.reorders.at(-1)).toEqual(["B", "A", "C"]);
    // And reflected the new order optimistically (the tree's live-order authority).
    await waitFor(() => expect(result.current.orderedTerminalIds).toEqual(["B", "A", "C"]));
  });
});
