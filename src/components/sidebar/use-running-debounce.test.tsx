import { act, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { ExecState } from "./use-terminals";
import { RUNNING_BADGE_DELAY_MS, useRunningDebounce } from "./use-running-debounce";

/**
 * PRD-2.1 finding #14 (anti-flicker): a command that runs for less than
 * `RUNNING_BADGE_DELAY_MS` must NOT flash a badge AT ALL — neither the `running`
 * dot nor its settled result ("si < 500 ms on n'affiche pas, peu importe le
 * statut"). `useRunningDebounce` holds a fresh running episode for the threshold
 * before revealing anything; if it settles first (an instant command) the result
 * is suppressed too (display falls back to idle). A running episode that outlasts
 * the threshold reveals `running`, then its result.
 *
 * Mocked timers prove both legs deterministically: settled-before-threshold ⇒
 * nothing shown; running-past-threshold ⇒ running then the result.
 */
describe("useRunningDebounce (running anti-flicker, finding #14)", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it("the threshold is a named constant (tunable)", () => {
    expect(RUNNING_BADGE_DELAY_MS).toBeGreaterThan(0);
  });

  it("shows NOTHING when a command settles BEFORE the threshold (no flash, result suppressed too)", () => {
    vi.useFakeTimers();
    const { result, rerender } = renderHook(({ raw }: { raw: ExecState }) => useRunningDebounce(raw), {
      initialProps: { raw: "idle" as ExecState },
    });
    expect(result.current).toBe("idle");

    // Command starts → running, but we are still BELOW the threshold so the dot
    // stays hidden (display keeps the prior idle state).
    rerender({ raw: "running" });
    expect(result.current).toBe("idle");

    // Advance PART of the threshold (below it) then settle (instant command).
    act(() => {
      vi.advanceTimersByTime(Math.floor(RUNNING_BADGE_DELAY_MS / 2));
    });
    expect(result.current).toBe("idle"); // still no running flash

    // Settle to success BEFORE the threshold elapses → an instant command shows
    // NOTHING: neither running nor the success result. Display stays idle (the
    // result badge is suppressed too — "si < 500 ms on n'affiche pas").
    rerender({ raw: "success" });
    expect(result.current).toBe("idle");

    // Even after the original threshold would have elapsed, nothing appears (the
    // reveal timer was cancelled when the state left running).
    act(() => {
      vi.advanceTimersByTime(RUNNING_BADGE_DELAY_MS);
    });
    expect(result.current).toBe("idle");
  });

  it("SHOWS running once it has lasted past the threshold, then settles to the result", () => {
    vi.useFakeTimers();
    const { result, rerender } = renderHook(({ raw }: { raw: ExecState }) => useRunningDebounce(raw), {
      initialProps: { raw: "idle" as ExecState },
    });

    rerender({ raw: "running" });
    expect(result.current).toBe("idle"); // below threshold: hidden

    // Cross the threshold while still running → the dot reveals.
    act(() => {
      vi.advanceTimersByTime(RUNNING_BADGE_DELAY_MS);
    });
    expect(result.current).toBe("running");

    // The command finishes → settled result shows immediately.
    rerender({ raw: "error" });
    expect(result.current).toBe("error");
  });

  it("settled and idle states are shown IMMEDIATELY (never delayed)", () => {
    vi.useFakeTimers();
    const { result, rerender } = renderHook(({ raw }: { raw: ExecState }) => useRunningDebounce(raw), {
      initialProps: { raw: "success" as ExecState },
    });
    // Seeded from raw → success is visible at once.
    expect(result.current).toBe("success");

    rerender({ raw: "error" });
    expect(result.current).toBe("error");

    rerender({ raw: "idle" });
    expect(result.current).toBe("idle");
  });

  it("a record that mounts ALREADY running shows running at once (restored snapshot)", () => {
    vi.useFakeTimers();
    const { result } = renderHook(() => useRunningDebounce("running"));
    // No fresh transition to debounce — the initial running snapshot (e.g. restored
    // from the DB on relaunch) is shown immediately, not held back.
    expect(result.current).toBe("running");
  });

  it("is identical for active and inactive rows (it keys off the raw state only)", () => {
    // The hook takes only the raw state — there is no `active` input — so the same
    // raw sequence yields the same display regardless of selection. Prove the
    // instant-command sequence twice (standing in for active vs inactive rows):
    // both suppress the badge identically (an instant command ⇒ nothing shown).
    vi.useFakeTimers();
    const run = () => {
      const h = renderHook(({ raw }: { raw: ExecState }) => useRunningDebounce(raw), {
        initialProps: { raw: "idle" as ExecState },
      });
      h.rerender({ raw: "running" });
      act(() => vi.advanceTimersByTime(Math.floor(RUNNING_BADGE_DELAY_MS / 2)));
      h.rerender({ raw: "success" });
      const out = h.result.current;
      h.unmount();
      return out;
    };
    expect(run()).toBe("idle");
    expect(run()).toBe("idle");
  });
});
