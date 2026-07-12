import { renderHook, act } from "@testing-library/react";
import { bridgeFake } from "@/bridge/test-harness";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { vi } from "vitest";

import { useIdleUnload } from "./use-idle-unload";

/** Every `unload_idle_sessions` invoke recorded by the fake bridge. */
function unloadCalls(fake: ReturnType<typeof bridgeFake>) {
  return fake.calls.filter((c) => c.command === "unload_idle_sessions");
}

describe("use-idle-unload (idle agent-session unload sweep)", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("sweeps on the interval, passing the visited set + threshold", () => {
    const fake = bridgeFake();
    fake.on("unload_idle_sessions", () => []);

    renderHook(() => useIdleUnload(["t-visible"], { intervalMs: 1000, thresholdSecs: 900 }));

    // No sweep before the first interval elapses.
    expect(unloadCalls(fake)).toHaveLength(0);

    act(() => {
      vi.advanceTimersByTime(1000);
    });
    const first = unloadCalls(fake);
    expect(first).toHaveLength(1);
    expect(first[0]?.args).toEqual({ thresholdSecs: 900, visited: ["t-visible"] });

    // It keeps sweeping on each subsequent tick.
    act(() => {
      vi.advanceTimersByTime(1000);
    });
    expect(unloadCalls(fake)).toHaveLength(2);
  });

  it("reads the LATEST visited set without re-arming the timer on a focus change", () => {
    const fake = bridgeFake();
    fake.on("unload_idle_sessions", () => []);

    const { rerender } = renderHook(({ v }) => useIdleUnload(v, { intervalMs: 1000 }), {
      initialProps: { v: ["t-a"] as string[] },
    });
    // Focus moves to another terminal BEFORE the first tick — the sweep must use the new set.
    rerender({ v: ["t-b"] });
    act(() => {
      vi.advanceTimersByTime(1000);
    });
    const calls = unloadCalls(fake);
    expect(calls).toHaveLength(1);
    expect(calls[0]?.args?.visited).toEqual(["t-b"]);
  });

  it("defaults to the 15-minute threshold when unspecified", () => {
    const fake = bridgeFake();
    fake.on("unload_idle_sessions", () => []);

    renderHook(() => useIdleUnload([], { intervalMs: 1000 }));
    act(() => {
      vi.advanceTimersByTime(1000);
    });
    expect(unloadCalls(fake)[0]?.args).toEqual({ thresholdSecs: 900, visited: [] });
  });

  it("never throws when the sweep invoke rejects (best-effort, inert)", () => {
    const fake = bridgeFake();
    fake.on("unload_idle_sessions", () => {
      throw new Error("sidecar busy");
    });

    renderHook(() => useIdleUnload(["t"], { intervalMs: 1000 }));
    expect(() =>
      act(() => {
        vi.advanceTimersByTime(1000);
      }),
    ).not.toThrow();
    // The failed tick still fires (best-effort), and a later tick retries.
    expect(unloadCalls(fake)).toHaveLength(1);
  });

  it("stops sweeping after unmount (the interval is cleared)", () => {
    const fake = bridgeFake();
    fake.on("unload_idle_sessions", () => []);

    const { unmount } = renderHook(() => useIdleUnload(["t"], { intervalMs: 1000 }));
    act(() => {
      vi.advanceTimersByTime(1000);
    });
    expect(unloadCalls(fake)).toHaveLength(1);

    unmount();
    act(() => {
      vi.advanceTimersByTime(5000);
    });
    expect(unloadCalls(fake)).toHaveLength(1);
  });
});
