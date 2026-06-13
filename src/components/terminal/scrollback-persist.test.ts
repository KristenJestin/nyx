import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  boundToLines,
  createScrollbackPersister,
  SCROLLBACK_MAX_LINES,
} from "./scrollback-persist";

describe("boundToLines", () => {
  it("returns the string unchanged when under the cap", () => {
    const s = "a\nb\nc";
    expect(boundToLines(s, 10)).toBe(s);
  });

  it("keeps only the LAST `max` lines (the recent tail)", () => {
    const s = "l1\nl2\nl3\nl4\nl5";
    // Keep the last 2 lines.
    expect(boundToLines(s, 2)).toBe("l4\nl5");
  });

  it("treats CRLF rows as lines too (splits on \\n, keeps the tail)", () => {
    const s = "a\r\nb\r\nc\r\nd";
    expect(boundToLines(s, 2)).toBe("c\r\nd");
  });

  it("never returns more than `max` newline-separated rows", () => {
    const many = Array.from({ length: 100 }, (_, i) => `line${i}`).join("\n");
    const bounded = boundToLines(many, 10);
    expect(bounded.split("\n")).toHaveLength(10);
    // It is the TAIL: the last produced line is preserved.
    expect(bounded.endsWith("line99")).toBe(true);
  });
});

describe("createScrollbackPersister", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.runOnlyPendingTimers();
    vi.useRealTimers();
  });

  it("debounces a burst into a SINGLE persist after the quiet delay", () => {
    const persist = vi.fn();
    const serialize = vi.fn(() => "history-snapshot");
    const p = createScrollbackPersister({
      recordId: "7",
      serialize,
      persist,
      debounceMs: 500,
    });

    // A burst of activity: many schedule() calls in quick succession.
    for (let i = 0; i < 50; i++) p.schedule();
    // Nothing persisted yet (still within the debounce window).
    expect(persist).not.toHaveBeenCalled();

    // Advance past the debounce: exactly ONE persist fires (not 50).
    vi.advanceTimersByTime(500);
    expect(persist).toHaveBeenCalledTimes(1);
    expect(persist).toHaveBeenCalledWith("7", "history-snapshot");

    p.dispose();
  });

  it("each new schedule() resets the debounce timer (trailing edge)", () => {
    const persist = vi.fn();
    const p = createScrollbackPersister({
      recordId: "1",
      serialize: () => "snap",
      persist,
      debounceMs: 300,
    });

    p.schedule();
    vi.advanceTimersByTime(200); // not yet
    p.schedule(); // resets the timer
    vi.advanceTimersByTime(200); // 200 since the reset â†’ still not fired
    expect(persist).not.toHaveBeenCalled();
    vi.advanceTimersByTime(100); // now 300 since the last schedule
    expect(persist).toHaveBeenCalledTimes(1);

    p.dispose();
  });

  it("bounds the serialized snapshot to the line cap before persisting", () => {
    const persist = vi.fn();
    // serialize returns far more lines than the cap.
    const big = Array.from({ length: SCROLLBACK_MAX_LINES + 500 }, (_, i) => `r${i}`).join(
      "\n",
    );
    const p = createScrollbackPersister({
      recordId: "2",
      serialize: () => big,
      persist,
      debounceMs: 100,
      maxLines: SCROLLBACK_MAX_LINES,
    });

    p.schedule();
    vi.advanceTimersByTime(100);

    expect(persist).toHaveBeenCalledTimes(1);
    const sent = persist.mock.calls[0][1] as string;
    expect(sent.split("\n").length).toBeLessThanOrEqual(SCROLLBACK_MAX_LINES);
    // It kept the tail (most-recent rows).
    expect(sent.endsWith(`r${SCROLLBACK_MAX_LINES + 499}`)).toBe(true);

    p.dispose();
  });

  it("flush() persists IMMEDIATELY (tab/app close path), bypassing the debounce", () => {
    const persist = vi.fn();
    const p = createScrollbackPersister({
      recordId: "9",
      serialize: () => "on-close",
      persist,
      debounceMs: 1000,
    });

    p.flush();
    // No timer advance: flush is synchronous.
    expect(persist).toHaveBeenCalledTimes(1);
    expect(persist).toHaveBeenCalledWith("9", "on-close");

    p.dispose();
  });

  it("flush() cancels a pending debounced snapshot so it writes once, not twice", () => {
    const persist = vi.fn();
    const p = createScrollbackPersister({
      recordId: "3",
      serialize: () => "snap",
      persist,
      debounceMs: 400,
    });

    p.schedule(); // schedule a debounced write
    p.flush(); // flush now
    expect(persist).toHaveBeenCalledTimes(1);

    // The previously-scheduled debounce must NOT fire a second write.
    vi.advanceTimersByTime(400);
    expect(persist).toHaveBeenCalledTimes(1);

    p.dispose();
  });

  it("dispose() cancels any pending snapshot (no write after teardown)", () => {
    const persist = vi.fn();
    const p = createScrollbackPersister({
      recordId: "4",
      serialize: () => "snap",
      persist,
      debounceMs: 200,
    });

    p.schedule();
    p.dispose();
    vi.advanceTimersByTime(500);
    expect(persist).not.toHaveBeenCalled();
  });

  it("does NOT persist per-activity: 1000 schedule()s collapse to one write", () => {
    const persist = vi.fn();
    const p = createScrollbackPersister({
      recordId: "5",
      serialize: () => "snap",
      persist,
      debounceMs: 50,
    });

    for (let i = 0; i < 1000; i++) p.schedule();
    vi.advanceTimersByTime(50);
    // The whole burst produced exactly one write â€” never one-per-activity.
    expect(persist).toHaveBeenCalledTimes(1);

    p.dispose();
  });
});
