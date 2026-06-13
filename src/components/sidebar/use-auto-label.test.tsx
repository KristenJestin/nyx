import { renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useAutoLabel, type TerminalInfo } from "./auto-label";

describe("useAutoLabel", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("returns null while there is no PTY id yet", () => {
    const poll = vi.fn();
    const { result } = renderHook(() => useAutoLabel(null, { poll }));
    expect(result.current).toBeNull();
    expect(poll).not.toHaveBeenCalled();
  });

  it("primes the auto label from the first reading (cwd basename)", async () => {
    const poll = vi.fn(
      async (): Promise<TerminalInfo> => ({ cwd: "/home/x/projetA", foreground: "bash" }),
    );
    const { result } = renderHook(() => useAutoLabel(7, { poll, pollMs: 1000 }));

    await vi.waitFor(() => expect(result.current).toBe("projetA"));
    expect(poll).toHaveBeenCalledWith(7);
  });

  it("reflects a running program on the NEXT poll (htop)", async () => {
    let info: TerminalInfo = { cwd: "/home/x/projetA", foreground: "bash" };
    const poll = vi.fn(async (): Promise<TerminalInfo> => info);
    const { result } = renderHook(() => useAutoLabel(1, { poll, pollMs: 1000 }));

    await vi.waitFor(() => expect(result.current).toBe("projetA"));

    // Launch htop → the next poll picks it up.
    info = { cwd: "/home/x/projetA", foreground: "htop" };
    await vi.advanceTimersByTimeAsync(1000);
    await vi.waitFor(() => expect(result.current).toBe("projetA · htop"));
  });

  it("polls on the cadence — recompute is DEBOUNCED by the fixed interval, not per byte", async () => {
    const poll = vi.fn(
      async (): Promise<TerminalInfo> => ({ cwd: "/p", foreground: "bash" }),
    );
    renderHook(() => useAutoLabel(2, { poll, pollMs: 1000 }));

    // Prime call.
    await vi.waitFor(() => expect(poll).toHaveBeenCalledTimes(1));
    // Over 3 seconds at a 1s cadence we get ~3 more polls — a bounded rate, NOT
    // one read per terminal output event.
    await vi.advanceTimersByTimeAsync(3000);
    expect(poll.mock.calls.length).toBeLessThanOrEqual(4);
    expect(poll.mock.calls.length).toBeGreaterThanOrEqual(3);
  });

  it("stops polling after unmount (no leaked interval)", async () => {
    const poll = vi.fn(
      async (): Promise<TerminalInfo> => ({ cwd: "/p", foreground: "bash" }),
    );
    const { unmount } = renderHook(() => useAutoLabel(3, { poll, pollMs: 1000 }));
    await vi.waitFor(() => expect(poll).toHaveBeenCalledTimes(1));

    unmount();
    const callsAtUnmount = poll.mock.calls.length;
    await vi.advanceTimersByTimeAsync(5000);
    expect(poll.mock.calls.length).toBe(callsAtUnmount);
  });
});
