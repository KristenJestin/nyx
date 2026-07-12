import { renderHook, waitFor } from "@testing-library/react";
import { bridgeFake, emit } from "@/bridge/test-harness";
import { describe, expect, it, vi } from "vitest";

import { RUNTIME_CHANGED_EVENTS, useRuntimeChangeSubscription } from "./use-runtime-live";

describe("use-runtime-live (shared live engine)", () => {
  it("declares the coarse invalidation topics + the fine exec-state/busy-state events as the single source", () => {
    expect([...RUNTIME_CHANGED_EVENTS]).toEqual([
      "commands://changed",
      "agent-sessions://changed",
      "terminals://changed",
      "workspaces://changed",
      "terminal://exec-state",
      "terminal://busy-state",
    ]);
  });

  it("subscribes to every runtime topic while enabled", async () => {
    const fake = bridgeFake();
    renderHook(() => useRuntimeChangeSubscription(true, () => {}));
    await waitFor(() => {
      for (const topic of RUNTIME_CHANGED_EVENTS) {
        expect(fake.subscriberCount(topic)).toBe(1);
      }
    });
  });

  it("holds NO subscription when disabled", async () => {
    const fake = bridgeFake();
    renderHook(() => useRuntimeChangeSubscription(false, () => {}));
    // Give any (erroneous) subscribe a tick to land, then assert none did.
    await Promise.resolve();
    for (const topic of RUNTIME_CHANGED_EVENTS) {
      expect(fake.subscriberCount(topic)).toBe(0);
    }
  });

  it("re-pulls LIVE on a push — event-driven, never on a timer (no polling)", async () => {
    const fake = bridgeFake();
    const onChange = vi.fn();
    renderHook(() => useRuntimeChangeSubscription(true, onChange));
    await waitFor(() => expect(fake.subscriberCount("terminals://changed")).toBe(1));

    expect(onChange).not.toHaveBeenCalled(); // nothing fired without a push (no timer)
    await emit("terminals://changed", {});
    expect(onChange).toHaveBeenCalledTimes(1);
    await emit("terminal://exec-state", {});
    expect(onChange).toHaveBeenCalledTimes(2);
  });

  it("tears down every subscription on unmount (StrictMode-safe)", async () => {
    const fake = bridgeFake();
    const { unmount } = renderHook(() => useRuntimeChangeSubscription(true, () => {}));
    await waitFor(() => expect(fake.subscriberCount("terminals://changed")).toBe(1));
    unmount();
    await waitFor(() => {
      for (const topic of RUNTIME_CHANGED_EVENTS) {
        expect(fake.subscriberCount(topic)).toBe(0);
      }
    });
  });

  it("does not fire onChange after teardown even if a stale push arrives", async () => {
    const fake = bridgeFake();
    const onChange = vi.fn();
    const { unmount } = renderHook(() => useRuntimeChangeSubscription(true, onChange));
    await waitFor(() => expect(fake.subscriberCount("terminals://changed")).toBe(1));
    unmount();
    await waitFor(() => expect(fake.subscriberCount("terminals://changed")).toBe(0));
    await emit("terminals://changed", {});
    expect(onChange).not.toHaveBeenCalled();
  });
});
