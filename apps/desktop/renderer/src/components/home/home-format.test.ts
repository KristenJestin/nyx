import { describe, expect, it } from "vitest";

import { formatAge } from "./home-format";

const NOW = 1_700_000_000_000;
const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

describe("formatAge", () => {
  it("reads `now` under a minute — the age has nothing to say yet", () => {
    expect(formatAge(NOW, NOW)).toBe("now");
    expect(formatAge(NOW - 59_999, NOW)).toBe("now");
  });

  it("reads minutes, then hours, then days — ONE unit, never two", () => {
    expect(formatAge(NOW - MINUTE, NOW)).toBe("1m");
    expect(formatAge(NOW - 59 * MINUTE, NOW)).toBe("59m");
    expect(formatAge(NOW - HOUR, NOW)).toBe("1h");
    expect(formatAge(NOW - 23 * HOUR, NOW)).toBe("23h");
    expect(formatAge(NOW - DAY, NOW)).toBe("1d");
    expect(formatAge(NOW - 9 * DAY, NOW)).toBe("9d");
  });

  it("reads `now` for a timestamp in the FUTURE — a clock skew is not a negative age", () => {
    expect(formatAge(NOW + HOUR, NOW)).toBe("now");
  });
});
