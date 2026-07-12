import { render, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { emit, mockIPC } from "@/bridge/test-harness";
import { __resetTerminalStore, ensureTerminal, getTerminal } from "./terminal-store";
import { useReconcileTerminals } from "./use-reconcile-terminals";

/**
 * `useReconcileTerminals` is the APP-WIDE reconciliation: it disposes a live store instance ONLY
 * when its backing record has left the GLOBAL `list_terminals` alive set (a real close), and NEVER
 * on a mere project switch (which does not change `list_terminals`). These tests drive the store +
 * a mocked `list_terminals` and assert the dispose (`pty_close`) happens for a truly-gone terminal
 * and NOT for a terminal that merely belongs to another (still-alive) project.
 */

interface TermRow {
  id: string;
  status: "alive" | "closed";
}

/** What the next `list_terminals` resolves to (or `null` for an unreliable/empty-ish result). */
let liveRows: TermRow[] | null;
/** PTY ids passed to `pty_close` — a NON-empty array means a terminal was disposed. */
let closes: number[];
/** How many times `list_terminals` was invoked (so tests can await the async reconcile). */
let listCalls: number;

/** A minimal harness component that just mounts the hook. */
function Harness() {
  useReconcileTerminals();
  return null;
}

describe("useReconcileTerminals (app-wide global reconcile)", () => {
  beforeEach(() => {
    closes = [];
    listCalls = 0;
    liveRows = [];
    let nextPtyId = 100;
    mockIPC(
      (cmd, args) => {
        const a = (args ?? {}) as Record<string, unknown>;
        if (cmd === "pty_spawn") return nextPtyId++;
        if (cmd === "pty_close") {
          closes.push((a.id as number) ?? -1);
          return null;
        }
        if (cmd === "list_terminals") {
          listCalls += 1;
          return liveRows;
        }
        return null;
      },
      { shouldMockEvents: true },
    );
  });

  afterEach(() => {
    __resetTerminalStore();
  });

  it("keeps terminals of ALL projects — a project switch never disposes the other project's terminal", async () => {
    // Two terminals in DIFFERENT projects, both mounted (their PTYs spawned) and both ALIVE.
    ensureTerminal({ recordId: "pA" }); // project A (the deck on screen)
    ensureTerminal({ recordId: "pB" }); // project B (its pane not currently mounted)
    await waitFor(() => expect(getTerminal("pA")?.pty != null && getTerminal("pB")?.pty != null));
    liveRows = [
      { id: "pA", status: "alive" },
      { id: "pB", status: "alive" },
    ];

    // Mount the app-wide reconcile. Its initial `list_terminals` returns BOTH (all projects), so it
    // must dispose NEITHER — switching to project A must not close project B's terminal/agent.
    render(<Harness />);
    await waitFor(() => expect(listCalls).toBeGreaterThanOrEqual(1));
    // Let the async reconcile settle, then assert nothing was disposed.
    await new Promise((r) => setTimeout(r, 20));
    expect(closes).toHaveLength(0);
    expect(getTerminal("pA")).toBeDefined();
    expect(getTerminal("pB")).toBeDefined();
  });

  it("disposes a terminal once it leaves the global set (closed by an agent over MCP)", async () => {
    ensureTerminal({ recordId: "pA" });
    ensureTerminal({ recordId: "pB" });
    await waitFor(() => expect(getTerminal("pA")?.pty != null && getTerminal("pB")?.pty != null));
    liveRows = [
      { id: "pA", status: "alive" },
      { id: "pB", status: "alive" },
    ];

    render(<Harness />);
    await waitFor(() => expect(listCalls).toBeGreaterThanOrEqual(1));
    await new Promise((r) => setTimeout(r, 20));
    expect(closes).toHaveLength(0);

    // pB is REALLY closed (by an agent over MCP): it drops from `list_terminals` and the backend
    // pushes `terminals://changed`. The reconcile re-pulls and disposes pB's leaked instance —
    // while pA (still alive, another project) is untouched.
    liveRows = [{ id: "pA", status: "alive" }];
    await emit("terminals://changed");

    await waitFor(() => expect(getTerminal("pB")).toBeUndefined());
    expect(getTerminal("pA")).toBeDefined();
    expect(closes).toHaveLength(1);
  });

  it("closing a terminal in the current project disposes it (records a status:'closed' row)", async () => {
    // A record flipped to `closed` (the normal UI close persists `closed`) is NOT alive, so it must
    // be pruned exactly like an MCP close — the normal open/close path still frees the PTY.
    ensureTerminal({ recordId: "t1" });
    await waitFor(() => expect(getTerminal("t1")?.pty != null));
    liveRows = [{ id: "t1", status: "alive" }];
    render(<Harness />);
    await waitFor(() => expect(listCalls).toBeGreaterThanOrEqual(1));
    await new Promise((r) => setTimeout(r, 20));
    expect(getTerminal("t1")).toBeDefined();

    liveRows = [{ id: "t1", status: "closed" }];
    await emit("terminals://changed");
    await waitFor(() => expect(getTerminal("t1")).toBeUndefined());
    expect(closes).toHaveLength(1);
  });

  it("never disposes when list_terminals is unreliable (null result)", async () => {
    ensureTerminal({ recordId: "pA" });
    await waitFor(() => expect(getTerminal("pA")?.pty != null));
    // A null (non-array) result is NOT a reliable "these are all the terminals" signal — the hook
    // must skip the reconcile rather than treat it as "everything is gone".
    liveRows = null;

    render(<Harness />);
    await waitFor(() => expect(listCalls).toBeGreaterThanOrEqual(1));
    await new Promise((r) => setTimeout(r, 20));
    expect(closes).toHaveLength(0);
    expect(getTerminal("pA")).toBeDefined();
  });
});
