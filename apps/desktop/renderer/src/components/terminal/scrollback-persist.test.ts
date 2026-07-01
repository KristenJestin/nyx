import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { buildDeadHistory, RESTORE_SEPARATOR_LABEL } from "./dead-history";
import {
  boundToLines,
  createScrollbackPersister,
  SCROLLBACK_MAX_LINES,
  stripDeadHistory,
} from "./scrollback-persist";

/** The bare (uncoloured) separator line as written by buildDeadHistory. */
const SEP = `── ${RESTORE_SEPARATOR_LABEL} ──\r\n`;

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

describe("stripDeadHistory", () => {
  it("(c) returns the blob unchanged when there is NO separator (fresh terminal)", () => {
    const fresh = "$ echo hi\r\nhi\r\n$ ";
    expect(stripDeadHistory(fresh)).toBe(fresh);
  });

  it("(a) with N separators, keeps ONLY the content after the LAST one", () => {
    const blob =
      `old session 1\r\n${SEP}` +
      `restored once\r\n${SEP}` +
      `restored twice\r\n${SEP}` +
      `live output\r\n$ `;
    // Three separators (3 restore cycles) collapse to just the live tail.
    expect(stripDeadHistory(blob)).toBe("live output\r\n$ ");
  });

  it("(b) with a trailing separator and NO live content after, keeps the content BEFORE it", () => {
    const before = "previous work\r\n$ command\r\noutput\r\n";
    const blob = `${before}${SEP}`;
    // Closed immediately after restore: never lose the prior session.
    expect(stripDeadHistory(blob)).toBe(before);
  });

  it("(b) treats whitespace-only content after the last separator as 'nothing live'", () => {
    const before = "previous work\r\n";
    const blob = `${before}${SEP}\r\n   \r\n`;
    expect(stripDeadHistory(blob)).toBe(before);
  });

  it("(d) matches a separator wrapped in SGR ANSI colour codes around the label", () => {
    // Exactly what buildDeadHistory emits for a real coloured separator.
    const coloured = buildDeadHistory("old history line", "#808080");
    expect(coloured).toContain("\x1b[38;2;128;128;128m");
    const blob = `${coloured}live below\r\n$ `;
    expect(stripDeadHistory(blob)).toBe("live below\r\n$ ");
  });

  it("(d) edge: SGR-wrapped separator with only ANSI/whitespace after keeps the content before", () => {
    const history = "prior session output";
    const coloured = buildDeadHistory(history, "#808080");
    // coloured = <history>\r\n<sgr separator sgr>\r\n → nothing live after it, so
    // we keep everything before the separator line: the history plus the \r\n
    // buildDeadHistory inserted before the divider.
    expect(stripDeadHistory(coloured)).toBe(`${history}\r\n`);
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
    const big = Array.from({ length: SCROLLBACK_MAX_LINES + 500 }, (_, i) => `r${i}`).join("\n");
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

  it("snapshot() strips the injected dead-history BEFORE bounding (persists only the live tail)", () => {
    const persist = vi.fn();
    // The serialized buffer carries a prior session + the injected separator,
    // then the live session below it — the exact shape SerializeAddon produces
    // after a restore.
    const serialized = `prior session\r\n${SEP}live line\r\n$ `;
    const p = createScrollbackPersister({
      recordId: "11",
      serialize: () => serialized,
      persist,
      debounceMs: 100,
    });

    p.flush();

    expect(persist).toHaveBeenCalledTimes(1);
    const sent = persist.mock.calls[0][1] as string;
    // The dead-history (prior session + separator) is gone; only the live tail
    // remains. bounding is a no-op here (well under the cap).
    expect(sent).toBe("live line\r\n$ ");
    expect(sent).not.toContain(RESTORE_SEPARATOR_LABEL);

    p.dispose();
  });

  it("snapshot() composes strip THEN bound: caps the LIVE tail after stripping", () => {
    const persist = vi.fn();
    // A dead-history separator, then more live lines than the cap below it.
    const liveLines = Array.from({ length: 30 }, (_, i) => `live${i}`).join("\r\n");
    const serialized = `prior\r\n${SEP}${liveLines}`;
    const p = createScrollbackPersister({
      recordId: "12",
      serialize: () => serialized,
      persist,
      debounceMs: 50,
      maxLines: 5,
    });

    p.flush();

    expect(persist).toHaveBeenCalledTimes(1);
    const sent = persist.mock.calls[0][1] as string;
    // Stripped (no separator, no prior session) AND bounded to the last 5 lines.
    expect(sent).not.toContain(RESTORE_SEPARATOR_LABEL);
    expect(sent).not.toContain("prior");
    expect(sent.split("\n")).toHaveLength(5);
    expect(sent.endsWith("live29")).toBe(true);

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
