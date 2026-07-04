import { act, renderHook } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import {
  ConsumedNotificationsProvider,
  useConsumedNotifications,
} from "./use-consumed-notifications";

describe("useConsumedNotifications", () => {
  it("remembers a consumed id per kind (terminals and services never collide)", () => {
    const { result } = renderHook(() => useConsumedNotifications(), {
      wrapper: ({ children }) => (
        <ConsumedNotificationsProvider>{children}</ConsumedNotificationsProvider>
      ),
    });
    expect(result.current.isConsumed("terminal", "x")).toBe(false);
    act(() => result.current.consume("terminal", "x"));
    expect(result.current.isConsumed("terminal", "x")).toBe(true);
    // A SERVICE with the same id is a different notification — still unconsumed.
    expect(result.current.isConsumed("service", "x")).toBe(false);
  });

  it("is a no-op fallback with no provider (nothing consumed, consume inert)", () => {
    const { result } = renderHook(() => useConsumedNotifications());
    expect(result.current.isConsumed("terminal", "x")).toBe(false);
    act(() => result.current.consume("terminal", "x"));
    // Without a provider the registry has no state to mutate — stays false.
    expect(result.current.isConsumed("terminal", "x")).toBe(false);
  });
});
