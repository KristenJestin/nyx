import { describe, expect, it } from "vitest";

import { SYSTEM_DEFAULT_CWD_LABEL, systemCwdLabel } from "./system-home-path";

/**
 * The `~` of the Système scope is a RENDERING and nothing else — the backend value is always the
 * absolute home. These pin the one direction (absolute → display) and, just as importantly, that
 * the function never invents an abbreviation for a path that is not under the home.
 */
describe("systemCwdLabel", () => {
  it("renders the home itself as `~`", () => {
    expect(systemCwdLabel("/home/kris", "/home/kris")).toBe("~");
    expect(systemCwdLabel("C:\\Users\\kris", "C:\\Users\\kris")).toBe("~");
  });

  it("renders a descendant of the home as `~/…`", () => {
    expect(systemCwdLabel("/home/kris/logs", "/home/kris")).toBe("~/logs");
    expect(systemCwdLabel("C:\\Users\\kris\\logs\\api", "C:\\Users\\kris")).toBe("~/logs/api");
  });

  it("is separator-agnostic and case-insensitive (the Windows drive-letter drift)", () => {
    // The same folder comes back as `C:\…` or `c:/…` depending on who resolved it; a display that
    // flickered between `~` and an absolute path on that difference would be worse than none.
    expect(systemCwdLabel("c:/Users/kris", "C:\\Users\\kris")).toBe("~");
    expect(systemCwdLabel("C:/Users/kris/Logs", "c:\\users\\kris")).toBe("~/Logs");
  });

  it("tolerates a trailing separator on either side", () => {
    expect(systemCwdLabel("/home/kris/", "/home/kris")).toBe("~");
    expect(systemCwdLabel("/home/kris/logs", "/home/kris/")).toBe("~/logs");
  });

  it("leaves a path OUTSIDE the home untouched — no invented abbreviation", () => {
    expect(systemCwdLabel("/srv/app", "/home/kris")).toBe("/srv/app");
    // A sibling whose name merely STARTS with the home's is not under it.
    expect(systemCwdLabel("/home/kristina", "/home/kris")).toBe("/home/kristina");
  });

  it("returns the cwd unchanged when the home is unknown or empty", () => {
    expect(systemCwdLabel("/home/kris", null)).toBe("/home/kris");
    expect(systemCwdLabel("/home/kris", undefined)).toBe("/home/kris");
    expect(systemCwdLabel("/home/kris", "")).toBe("/home/kris");
  });

  it("names the DEFAULT cwd `~` — the label the sidebar foot shows", () => {
    expect(SYSTEM_DEFAULT_CWD_LABEL).toBe("~");
  });
});
