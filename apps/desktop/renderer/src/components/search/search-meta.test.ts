import { describe, expect, it } from "vitest";

import { relativeSuggestionTime, SEARCH_GROUP_ORDER, SEARCH_KIND_META } from "./search-meta";

// REVIEW-FINDING (« pour moi ça doit être mélangé ») : the palette no longer groups, so there is no
// `groupHits` left to test. What survives of the type vocabulary is what a ROW needs — a glyph, and
// a name for the kind — and the guarantee that the backend cannot emit a kind the palette has no
// way to draw.
describe("the kind vocabulary — one row's worth, no ordering", () => {
  it("gives every kind the backend trims on a glyph and a name", () => {
    for (const kind of SEARCH_GROUP_ORDER) {
      expect(SEARCH_KIND_META[kind].icon, `${kind} has a glyph`).toBeTruthy();
      expect(SEARCH_KIND_META[kind].label, `${kind} has a name`).toBeTruthy();
    }
    expect(SEARCH_GROUP_ORDER).toHaveLength(Object.keys(SEARCH_KIND_META).length);
  });
});

describe("relativeSuggestionTime — the fixed bounds of the suggestion recency", () => {
  const now = new Date("2026-08-28T12:00:00").getTime();
  const ago = (ms: number) => relativeSuggestionTime(now - ms, now);

  it("`now` under a minute", () => {
    expect(ago(0)).toBe("now");
    expect(ago(59_000)).toBe("now");
  });

  it("`Nm` under an hour", () => {
    expect(ago(60_000)).toBe("1m");
    expect(ago(59 * 60_000)).toBe("59m");
  });

  it("`Nh` under a day", () => {
    expect(ago(60 * 60_000)).toBe("1h");
    expect(ago(23 * 60 * 60_000)).toBe("23h");
  });

  it("`Yest.` on the previous calendar day, past the 24 h bound", () => {
    // 12:00 → the day before at 10:00 is 26 h ago: past `Nh`, and yesterday.
    expect(ago(26 * 60 * 60_000)).toBe("Yest.");
  });

  it("`MMM D` beyond, in en-US", () => {
    expect(ago(5 * 24 * 60 * 60_000)).toBe("Aug 23");
    expect(relativeSuggestionTime(new Date("2026-01-04T09:00:00").getTime(), now)).toBe("Jan 4");
  });

  it("absorbs a timestamp in the future (clock skew) rather than printing a negative age", () => {
    expect(relativeSuggestionTime(now + 10_000, now)).toBe("now");
  });

  it("says nothing for an absent timestamp", () => {
    expect(relativeSuggestionTime(0, now)).toBe("");
  });
});
