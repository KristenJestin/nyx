import { describe, expect, it } from "vitest";

import { buildDeadHistory, RESTORE_SEPARATOR_LABEL } from "./dead-history";

describe("buildDeadHistory", () => {
  it("returns empty string when there is no prior scrollback (nothing to restore)", () => {
    expect(buildDeadHistory("", "#888888")).toBe("");
    expect(buildDeadHistory("   \n  ", "#888888")).toBe("");
  });

  it("emits the prior scrollback followed by a separator line", () => {
    const out = buildDeadHistory("old line 1\r\nold line 2", "#888888");
    // The original history is preserved verbatim at the head.
    expect(out.startsWith("old line 1\r\nold line 2")).toBe(true);
    // The separator label appears after the history.
    expect(out).toContain(RESTORE_SEPARATOR_LABEL);
  });

  it("colours the separator with the provided (token-derived) colour, not a hardcoded one", () => {
    const out = buildDeadHistory("hist", "#abcdef");
    // The separator uses a 24-bit SGR colour built from the passed hex
    // (171;205;239 = 0xab;0xcd;0xef) — proving the colour flows from the token.
    expect(out).toContain("38;2;171;205;239");
  });

  it("ends with a CRLF so the fresh shell's first prompt starts on its own line BELOW the separator", () => {
    const out = buildDeadHistory("hist", "#888888");
    expect(out.endsWith("\r\n")).toBe(true);
  });

  it("resets SGR after the separator so the colour never bleeds into live output", () => {
    const out = buildDeadHistory("hist", "#888888");
    // A reset (\x1b[0m) appears after the coloured separator.
    const sep = out.indexOf(RESTORE_SEPARATOR_LABEL);
    expect(out.slice(sep)).toContain("\x1b[0m");
  });
});
