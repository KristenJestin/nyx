import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { bridgeFake, mockIPC } from "@/bridge/test-harness";
import { beforeEach, describe, expect, it } from "vitest";

import { WindowControls } from "./window-controls";
import type { CloseWarning } from "./close-warning";

/**
 * Stub `agent_close_warnings` with a configurable result + count the window `close`
 * ops. The close goes through `nyxBridge.window.close()` (shell-agnostic), which the
 * fake records into `windowCalls` — we count those (no Tauri-internals plumbing).
 */
function installIpc(warnings: CloseWarning[]): { closeCalls: () => number } {
  mockIPC((cmd) => {
    if (typeof cmd === "string" && cmd.includes("agent_close_warnings")) return warnings;
    return null;
  });
  const fake = bridgeFake();
  return { closeCalls: () => fake.windowCalls.filter((c) => c === "close").length };
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
