import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { mockIPC } from "@tauri-apps/api/mocks";
import { beforeEach, describe, expect, it } from "vitest";

import { WindowControls } from "./window-controls";
import type { CloseWarning } from "./close-warning";

/** Record window IPC calls + stub `agent_close_warnings` with a configurable result. */
function installIpc(warnings: CloseWarning[]): { closeCalls: () => number } {
  let closeCount = 0;
  mockIPC((cmd) => {
    if (typeof cmd === "string" && cmd.includes("agent_close_warnings")) return warnings;
    if (typeof cmd === "string" && cmd.includes("is_maximized")) return false;
    if (typeof cmd === "string" && cmd.includes("close")) {
      closeCount += 1;
      return null;
    }
    return null;
  });
  // Seed the window metadata `getCurrentWindow()` reads (mockIPC doesn't inject it).
  const internals = (
    globalThis as unknown as { __TAURI_INTERNALS__: Record<string, unknown> }
  ).__TAURI_INTERNALS__;
  internals.metadata = { currentWindow: { label: "main" }, windows: [] };
  return { closeCalls: () => closeCount };
}

const warn = (id: string, message: string): CloseWarning => ({
  terminal_id: id,
  agent_kind: "claude_code",
  message,
});

describe("<WindowControls> close warning (PRD-5 #6)", () => {
  beforeEach(() => {
    // clearMocks runs in vitest.setup.ts afterEach.
  });

  // Done-criterion: with NO live sessions to warn about (e.g. resume ON, or none
  // active), the close proceeds immediately — no dialog, the window closes.
  it("closes immediately when there are no warnings (option ON / nothing active)", async () => {
    const ipc = installIpc([]);
    render(<WindowControls />);
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: /close/i }));
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(ipc.closeCalls()).toBeGreaterThan(0);
    expect(screen.queryByText(/still active/i)).not.toBeInTheDocument();
  });

  // Done-criterion: when a live session in a NON-resuming project exists, the close is
  // WITHHELD and the warning dialog is shown listing the session — close not yet called.
  it("opens the warning dialog (and withholds close) when a live session would be dropped", async () => {
    const ipc = installIpc([warn("t1", "Claude Code has an active session in build that won't be resumed.")]);
    render(<WindowControls />);
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: /close/i }));
      await Promise.resolve();
      await Promise.resolve();
    });
    await waitFor(() => expect(screen.getByText(/still active/i)).toBeInTheDocument());
    // The message (naming the agent + terminal) is shown; the window did NOT close yet.
    expect(screen.getByText(/Claude Code has an active session in build/i)).toBeInTheDocument();
    expect(ipc.closeCalls()).toBe(0);
  });

  it("closes anyway on confirm", async () => {
    const ipc = installIpc([warn("t1", "Claude Code has an active session in build that won't be resumed.")]);
    render(<WindowControls />);
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: /close/i }));
      await Promise.resolve();
      await Promise.resolve();
    });
    await waitFor(() => expect(screen.getByText(/still active/i)).toBeInTheDocument());
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: /close anyway/i }));
      await Promise.resolve();
    });
    expect(ipc.closeCalls()).toBeGreaterThan(0);
  });

  it("keeps the window open on cancel (no close call)", async () => {
    const ipc = installIpc([warn("t1", "Claude Code has an active session in build that won't be resumed.")]);
    render(<WindowControls />);
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: /close/i }));
      await Promise.resolve();
      await Promise.resolve();
    });
    await waitFor(() => expect(screen.getByText(/still active/i)).toBeInTheDocument());
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: /keep open/i }));
      await Promise.resolve();
    });
    expect(ipc.closeCalls()).toBe(0);
  });
});
