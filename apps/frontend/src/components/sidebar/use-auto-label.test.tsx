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
    const poll = vi.fn(async (): Promise<TerminalInfo> => ({ cwd: "/p", foreground: "bash" }));
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
    const poll = vi.fn(async (): Promise<TerminalInfo> => ({ cwd: "/p", foreground: "bash" }));
    const { unmount } = renderHook(() => useAutoLabel(3, { poll, pollMs: 1000 }));
    await vi.waitFor(() => expect(poll).toHaveBeenCalledTimes(1));

    unmount();
    const callsAtUnmount = poll.mock.calls.length;
    await vi.advanceTimersByTimeAsync(5000);
    expect(poll.mock.calls.length).toBe(callsAtUnmount);
  });

  // FEEDBACK #32 — persist the live cwd into the record on change (debounced).
  it("persists a CHANGED cwd into the record, debounced (incl. same-workspace subdir)", async () => {
    let info: TerminalInfo = { cwd: "/work/palbank", foreground: "bash" };
    const poll = vi.fn(async (): Promise<TerminalInfo> => info);
    const persistCwd = vi.fn();
    renderHook(() =>
      useAutoLabel(7, {
        poll,
        pollMs: 1000,
        recordId: "rec-1",
        persistCwd,
        persistDebounceMs: 1500,
      }),
    );

    // Prime: first reading observes the initial cwd. It is a NEW value, so it
    // schedules a debounced write; let the debounce elapse.
    await vi.waitFor(() => expect(poll).toHaveBeenCalledTimes(1));
    await vi.advanceTimersByTimeAsync(1500);
    expect(persistCwd).toHaveBeenLastCalledWith("rec-1", "/work/palbank");
    const callsAfterPrime = persistCwd.mock.calls.length;

    // A `cd` into a SUBDIR of the SAME workspace (auto_attach would NOT fire — the
    // binding is unchanged) must still be persisted.
    info = { cwd: "/work/palbank/pfm-palbank-tests", foreground: "bash" };
    await vi.advanceTimersByTimeAsync(1000); // next poll observes the new cwd
    // Not yet written — still inside the debounce window.
    expect(persistCwd.mock.calls.length).toBe(callsAfterPrime);
    await vi.advanceTimersByTimeAsync(1500);
    expect(persistCwd).toHaveBeenLastCalledWith("rec-1", "/work/palbank/pfm-palbank-tests");
  });

  it("does NOT persist when the cwd is unchanged across polls", async () => {
    const poll = vi.fn(
      async (): Promise<TerminalInfo> => ({ cwd: "/work/palbank", foreground: "bash" }),
    );
    const persistCwd = vi.fn();
    renderHook(() =>
      useAutoLabel(8, {
        poll,
        pollMs: 1000,
        recordId: "rec-2",
        persistCwd,
        persistDebounceMs: 1500,
      }),
    );

    // One write for the initial reading, then NO further writes for a stationary
    // terminal even across many polls (the change-gate, not one write per poll).
    await vi.advanceTimersByTimeAsync(1500);
    expect(persistCwd).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(5000);
    expect(persistCwd).toHaveBeenCalledTimes(1);
  });

  it("rapid cd's collapse into a SINGLE debounced write", async () => {
    let info: TerminalInfo = { cwd: "/a", foreground: "bash" };
    const poll = vi.fn(async (): Promise<TerminalInfo> => info);
    const persistCwd = vi.fn();
    renderHook(() =>
      useAutoLabel(9, {
        poll,
        pollMs: 1000,
        recordId: "rec-3",
        persistCwd,
        persistDebounceMs: 1500,
      }),
    );

    // Prime then drain the first write so the burst below is measured cleanly.
    await vi.advanceTimersByTimeAsync(1500);
    persistCwd.mockClear();

    // Three quick cd's, each observed by a poll within the debounce window: the
    // timer keeps resetting, so only the LAST one is written, once. Advance one
    // poll cadence between each so a poll observes the new cwd (and resets the
    // pending debounce); none fires yet because each reset lands before 1500ms.
    info = { cwd: "/b", foreground: "bash" };
    await vi.advanceTimersByTimeAsync(1000);
    info = { cwd: "/c", foreground: "bash" };
    await vi.advanceTimersByTimeAsync(1000);
    info = { cwd: "/d", foreground: "bash" };
    await vi.advanceTimersByTimeAsync(1000); // a poll observes /d, resets the timer
    expect(persistCwd).not.toHaveBeenCalled(); // still mid-debounce on /d
    // Now go quiet (no further cd): the debounce settles on the LAST cwd, once.
    await vi.advanceTimersByTimeAsync(1500);

    expect(persistCwd).toHaveBeenCalledTimes(1);
    expect(persistCwd).toHaveBeenCalledWith("rec-3", "/d");
  });

  it("does not persist when there is no recordId (record-less terminal)", async () => {
    let info: TerminalInfo = { cwd: "/a", foreground: "bash" };
    const poll = vi.fn(async (): Promise<TerminalInfo> => info);
    const persistCwd = vi.fn();
    renderHook(() => useAutoLabel(10, { poll, pollMs: 1000, persistCwd, persistDebounceMs: 1500 }));

    await vi.advanceTimersByTimeAsync(1500);
    info = { cwd: "/b", foreground: "bash" };
    await vi.advanceTimersByTimeAsync(2500);
    expect(persistCwd).not.toHaveBeenCalled();
  });
});
