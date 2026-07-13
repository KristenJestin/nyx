import { act, renderHook, waitFor } from "@testing-library/react";
import { mockIPC } from "@/bridge/test-harness";
import type { ReactNode } from "react";
import { describe, expect, it } from "vitest";

import type { TerminalRecord } from "@/components/sidebar/use-terminals";
import { RuntimeSelectionProvider } from "./runtime-selection";
import { useTerminalActions } from "./use-terminal-actions";

/**
 * Ctrl+T "new terminal" FALLBACK resolution (v2.5.6 finding #26, review finding #30). When the
 * gesture cannot inherit a workspace from the selection (nothing terminal-selected), the manager
 * must NOT spawn a loose terminal: it attaches the new terminal to the active project's FALLBACK
 * workspace instead (finding #26 forbids terminals attached to nothing). A loose terminal is the
 * ABSOLUTE last resort, reached ONLY when there is no fallback workspace at all.
 *
 * These tests drive the real Ctrl+T path end-to-end: `useTerminalActions` registers the chord via
 * `useTerminalShortcuts` (TanStack Hotkeys on `document`), so dispatching a real Ctrl+T keydown
 * fires `newTerminalSmart` → create + (attach | loose). We assert on the actual backend calls
 * (`create_terminal` cwd + whether `attach_terminal` was called) so "not loose" is proven, not
 * assumed.
 */

function term(id: string, workspaceId: string | null): TerminalRecord {
  return {
    id,
    cwd: `/${id}`,
    label: null,
    scrollback: "",
    status: "alive",
    order_index: 0,
    created_at: 0,
    updated_at: 0,
    closed_at: null,
    workspace_id: workspaceId,
    workspace_binding_mode: "manual",
  };
}

interface Backend {
  creates: Array<{ cwd: unknown }>;
  attaches: Array<{ terminalId: unknown; workspaceId: unknown; mode: unknown }>;
}

/** Empty world (no terminals / projects / workspaces) so Ctrl+T can inherit NO workspace from the
 *  selection — the exact precondition that reaches the fallback (then loose) branch. Captures the
 *  create/attach calls the branch makes. */
function installBackend(): Backend {
  const backend: Backend = { creates: [], attaches: [] };
  let seq = 0;
  mockIPC((cmd, args) => {
    const a = (args ?? {}) as Record<string, unknown>;
    switch (cmd) {
      case "list_terminals":
        return [];
      case "list_projects":
        return [];
      case "list_workspaces":
        return [];
      case "create_terminal": {
        backend.creates.push({ cwd: a.cwd });
        return term(`new-${++seq}`, null);
      }
      case "attach_terminal": {
        backend.attaches.push({
          terminalId: a.terminalId,
          workspaceId: a.workspaceId,
          mode: a.mode,
        });
        return null;
      }
      default:
        return null;
    }
  });
  return backend;
}

function wrapper({ children }: { children: ReactNode }) {
  return <RuntimeSelectionProvider>{children}</RuntimeSelectionProvider>;
}

/** Fire the real Ctrl+T chord on `document` (jsdom is non-mac → `Mod` is Control), the same way
 *  `use-terminal-shortcuts.test.tsx` drives the hotkey manager. */
function pressCtrlT() {
  document.dispatchEvent(
    new KeyboardEvent("keydown", { key: "t", ctrlKey: true, bubbles: true, cancelable: true }),
  );
}

describe("useTerminalActions — Ctrl+T fallback (finding #26/#30)", () => {
  it("with NO selectable workspace, attaches the new terminal to the FALLBACK workspace (not loose)", async () => {
    const backend = installBackend();
    const fallback = { id: "ws-root", path: "/proj/root" };
    // Nothing terminal-selected + empty terminal set ⇒ resolveNewTerminalWorkspace returns null.
    renderHook(() => useTerminalActions([], fallback), { wrapper });

    act(() => pressCtrlT());

    // Created a terminal spawned at the fallback workspace's path...
    await waitFor(() => expect(backend.creates).toEqual([{ cwd: "/proj/root" }]));
    // ...and ATTACHED it to the fallback workspace (manual binding) — the proof it is NOT loose.
    await waitFor(() =>
      expect(backend.attaches).toEqual([
        { terminalId: "new-1", workspaceId: "ws-root", mode: "manual" },
      ]),
    );
  });

  it("with NO selectable workspace AND no fallback, spawns a loose terminal (last resort)", async () => {
    const backend = installBackend();
    // No fallback workspace (e.g. a project with zero workspaces) ⇒ the absolute last resort.
    renderHook(() => useTerminalActions([], null), { wrapper });

    act(() => pressCtrlT());

    // A terminal was created...
    await waitFor(() => expect(backend.creates.length).toBe(1));
    // ...but NEVER attached — that is what makes it loose (contrast with the fallback case above).
    expect(backend.attaches).toEqual([]);
  });
});
