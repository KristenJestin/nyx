import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { mockIPC, emit } from "@/bridge/test-harness";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { TerminalRecord } from "./use-terminals";

/**
 * PRD-2.1 task #9 — the MANAGER-LEVEL wiring of the read/unread model that lives in
 * `<TerminalManager>` (NOT in `useTerminals` or the leaf components, which are
 * covered by `use-terminals.exec-state.test.tsx` / `run-state.test.tsx` /
 * `terminal-item.test.tsx`). Two done-criteria are realised here:
 *
 *   - "Selecting a terminal clears settled unread state" — `selectTerminal` calls
 *     `markRead`, which fires `terminal_exec_mark_read` and clears the local flag.
 *   - "Active terminal success/error is marked read immediately" — the
 *     `viewedTerminal` effect: a `terminal://exec-state` settle that arrives for the
 *     CURRENTLY-VIEWED (active, no command view) terminal is auto-marked read.
 *
 * The real `<TerminalManager>` is mounted with the real `useTerminals` hook against
 * a mocked backend. The heavy presentational children are stubbed: the deck (xterm
 * + PTY) is inert, and `<AppSidebar>` is replaced by a controllable harness that
 * exposes one "select" button per terminal (wired to the real `onSelect`
 * =`selectTerminal`) and renders each row's `unread` flag — so a click drives the
 * exact production select path and the rendered flag proves the local clear.
 */

vi.mock("./terminal-deck", () => ({
  TerminalDeck: () => null,
}));

vi.mock("@/components/chrome/chrome-bar", () => ({
  ChromeBar: () => null,
}));

vi.mock("./use-manual-add", () => ({
  useManualAdd: () => ({
    addProject: vi.fn(),
    addWorkspace: vi.fn(),
    editProject: vi.fn(),
    removeProject: vi.fn(),
    dialog: null,
  }),
}));

// A controllable sidebar stub: a select button + a printed unread flag per row.
// Only the props this test drives are used; the rest are accepted and ignored.
vi.mock("./app-sidebar", () => ({
  AppSidebar: ({
    terminals,
    activeId,
    onSelect,
  }: {
    terminals: TerminalRecord[];
    activeId: string | null;
    onSelect: (id: string) => void;
  }) => (
    <div data-testid="sidebar-stub">
      <span data-testid="active-id">{activeId ?? "none"}</span>
      {terminals.map((t) => (
        <div key={t.id} data-testid={`row-${t.id}`}>
          <button type="button" data-testid={`select-${t.id}`} onClick={() => onSelect(t.id)}>
            select {t.id}
          </button>
          <span data-testid={`unread-${t.id}`}>{t.exec_state_unread ? "unread" : "read"}</span>
          <span data-testid={`state-${t.id}`}>{t.exec_state ?? "idle"}</span>
        </div>
      ))}
    </div>
  ),
}));

import { TerminalManager } from "./terminal-manager";

function term(
  id: number,
  exec_state: TerminalRecord["exec_state"] = "idle",
  exec_state_unread = false,
  exec_exit_code: number | null = null,
): TerminalRecord {
  return {
    id: String(id),
    cwd: "/x",
    label: null,
    scrollback: "",
    status: "alive",
    order_index: id,
    created_at: 0,
    updated_at: id, // last_active_at fallback uses order; keep distinct
    closed_at: null,
    last_active_at: null,
    workspace_id: null,
    workspace_binding_mode: "auto",
    exec_state,
    exec_state_unread,
    exec_exit_code,
  };
}

interface Backend {
  rows: TerminalRecord[];
  markedRead: string[];
}

function installBackend(rows: TerminalRecord[]): Backend {
  const backend: Backend = { rows: rows.map((r) => ({ ...r })), markedRead: [] };
  mockIPC(
    (cmd, args) => {
      const a = (args ?? {}) as Record<string, unknown>;
      switch (cmd) {
        case "list_terminals":
          return [...backend.rows].sort((x, y) => x.order_index - y.order_index);
        case "list_projects":
        case "list_workspaces":
          return [];
        case "terminal_exec_mark_read": {
          const id = a.id as string;
          backend.markedRead.push(id);
          const row = backend.rows.find((r) => r.id === id);
          if (row) row.exec_state_unread = false;
          return null;
        }
        case "set_active":
        case "terminal_info":
        case "auto_attach_terminal":
          return cmd === "terminal_info" ? { cwd: null, foreground: null } : null;
        default:
          return null;
      }
    },
    { shouldMockEvents: true },
  );
  return backend;
}

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllEnvs();
});

describe("<TerminalManager> read/unread wiring (PRD-2.1 #9)", () => {
  it("selecting a terminal clears its settled unread state (selectTerminal → markRead)", async () => {
    // Two terminals; #2 carries an UNREAD settled error (the user hasn't viewed it).
    const backend = installBackend([term(1, "idle"), term(2, "error", true, 1)]);
    render(<TerminalManager />);

    // The sidebar stub renders once the hook adopts the rows.
    await waitFor(() => expect(screen.getByTestId("row-2")).toBeInTheDocument());
    expect(screen.getByTestId("unread-2")).toHaveTextContent("unread");

    // Select (view) terminal #2 → markRead fires: local flag clears AND the backend
    // mark-read command is invoked; the settled error itself is preserved.
    await act(async () => {
      fireEvent.click(screen.getByTestId("select-2"));
    });

    await waitFor(() => expect(screen.getByTestId("unread-2")).toHaveTextContent("read"));
    expect(screen.getByTestId("state-2")).toHaveTextContent("error"); // result preserved
    await waitFor(() => expect(backend.markedRead).toContain("2"));
    expect(screen.getByTestId("active-id")).toHaveTextContent("2");
  });

  it("a success/error arriving for the ACTIVE terminal is marked read immediately (viewedTerminal effect)", async () => {
    // Terminal #1 is the only/active terminal. A settled error arrives FOR IT while
    // it is being viewed → it must never accumulate an unread badge: marked read at
    // once (and the backend mark-read is invoked).
    const backend = installBackend([term(1, "running")]);
    render(<TerminalManager />);

    await waitFor(() => expect(screen.getByTestId("active-id")).toHaveTextContent("1"));

    await act(async () => {
      await emit("terminal://exec-state", {
        terminal_id: "1",
        state: "error",
        exit_code: 2,
        unread: true,
        updated_at: 10,
      });
    });

    // The settled state lands (error), but because the terminal is the viewed one
    // the effect auto-marks it read — the row never shows unread.
    await waitFor(() => expect(screen.getByTestId("state-1")).toHaveTextContent("error"));
    await waitFor(() => expect(screen.getByTestId("unread-1")).toHaveTextContent("read"));
    await waitFor(() => expect(backend.markedRead).toContain("1"));
  });

  it("a success/error arriving for an INACTIVE terminal stays UNREAD (no premature mark-read)", async () => {
    // #1 active, #2 inactive. A settle for #2 must NOT be auto-marked read — the
    // unread badge is the whole point for a background terminal.
    const backend = installBackend([term(1, "idle"), term(2, "running")]);
    render(<TerminalManager />);

    await waitFor(() => expect(screen.getByTestId("active-id")).toHaveTextContent("1"));

    await act(async () => {
      await emit("terminal://exec-state", {
        terminal_id: "2",
        state: "success",
        exit_code: 0,
        unread: true,
        updated_at: 11,
      });
    });

    await waitFor(() => expect(screen.getByTestId("state-2")).toHaveTextContent("success"));
    // It stays unread (inactive) — and the backend was NOT told to mark #2 read.
    expect(screen.getByTestId("unread-2")).toHaveTextContent("unread");
    expect(backend.markedRead).not.toContain("2");
  });
});
