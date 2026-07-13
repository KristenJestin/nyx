import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { bridgeFake, mockIPC } from "@/bridge/test-harness";
import { describe, expect, it } from "vitest";

import { CloseGuard } from "./close-guard";
import type { CloseWarning } from "./close-warning";

/**
 * Stub `agent_close_warnings` with a configurable result and expose the fake so a test
 * can (a) fire the OS "close requested" signal (`triggerCloseRequested`) and (b) count
 * the `confirmClose` calls the guard makes to proceed with the real close.
 */
function installIpc(warnings: CloseWarning[]): {
  triggerClose: () => void;
  confirmCalls: () => number;
} {
  mockIPC((cmd) => {
    if (cmd === "agent_close_warnings") return warnings;
    return null;
  });
  const fake = bridgeFake();
  return {
    triggerClose: () => fake.triggerCloseRequested(),
    confirmCalls: () => fake.windowCalls.filter((c) => c === "confirmClose").length,
  };
}

const warn = (id: string, message: string): CloseWarning => ({
  terminal_id: id,
  agent_kind: "claude_code",
  message,
});

describe("<CloseGuard> OS close warning (PRD v2.5.6 #1)", () => {
  // Done-criterion: with NO live sessions to warn about, the OS close proceeds
  // immediately — no dialog, the guard confirms the real close.
  it("proceeds immediately when there are no warnings", async () => {
    const ipc = installIpc([]);
    render(<CloseGuard />);
    await act(async () => {
      ipc.triggerClose();
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(ipc.confirmCalls()).toBeGreaterThan(0);
    expect(screen.queryByText(/still active/i)).not.toBeInTheDocument();
  });

  // Done-criterion: a live session in a NON-resuming project WITHHOLDS the close and
  // shows the warning dialog listing the session — confirm not yet called.
  it("opens the warning dialog (and withholds close) when a live session would be dropped", async () => {
    const ipc = installIpc([
      warn("t1", "Claude Code has an active session in build that won't be resumed."),
    ]);
    render(<CloseGuard />);
    await act(async () => {
      ipc.triggerClose();
      await Promise.resolve();
      await Promise.resolve();
    });
    await waitFor(() => expect(screen.getByText(/still active/i)).toBeInTheDocument());
    expect(screen.getByText(/Claude Code has an active session in build/i)).toBeInTheDocument();
    expect(ipc.confirmCalls()).toBe(0);
  });

  it("closes anyway on confirm", async () => {
    const ipc = installIpc([
      warn("t1", "Claude Code has an active session in build that won't be resumed."),
    ]);
    render(<CloseGuard />);
    await act(async () => {
      ipc.triggerClose();
      await Promise.resolve();
      await Promise.resolve();
    });
    await waitFor(() => expect(screen.getByText(/still active/i)).toBeInTheDocument());
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: /close anyway/i }));
      await Promise.resolve();
    });
    expect(ipc.confirmCalls()).toBeGreaterThan(0);
  });

  it("keeps the window open on cancel (no confirm call)", async () => {
    const ipc = installIpc([
      warn("t1", "Claude Code has an active session in build that won't be resumed."),
    ]);
    render(<CloseGuard />);
    await act(async () => {
      ipc.triggerClose();
      await Promise.resolve();
      await Promise.resolve();
    });
    await waitFor(() => expect(screen.getByText(/still active/i)).toBeInTheDocument());
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: /keep open/i }));
      await Promise.resolve();
    });
    expect(ipc.confirmCalls()).toBe(0);
  });
});
