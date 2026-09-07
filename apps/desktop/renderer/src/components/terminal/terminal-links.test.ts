import { afterEach, describe, expect, it, vi } from "vitest";

import { createTerminalLinkHandler, isExternalHttpUrl, openTerminalLink } from "./terminal-links";

/**
 * WHAT A TERMINAL LINK MAY OPEN.
 *
 * The strings this module is handed are the most attacker-controlled in the app: anything any
 * program prints into any pane, a `cat` of a hostile file included. So the allowlist is the point,
 * and it is asserted from both ends — what passes, and the whole family of schemes that must not.
 * Electron main refuses non-`http(s)` too (`setWindowOpenHandler`), which is exactly why this is
 * worth pinning here: the two must not drift into each other's blind spot.
 */
describe("terminal links", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("accepts http and https, and nothing else", () => {
    expect(isExternalHttpUrl("https://example.com")).toBe(true);
    expect(isExternalHttpUrl("http://example.com/a?b=c#d")).toBe(true);

    // The dangerous ones — a printed `javascript:` that opened would be script execution from
    // terminal output, and `file:`/`data:` are local reads the browser must never be handed.
    expect(isExternalHttpUrl("javascript:alert(1)")).toBe(false);
    expect(isExternalHttpUrl("data:text/html,<script>alert(1)</script>")).toBe(false);
    expect(isExternalHttpUrl("file:///etc/passwd")).toBe(false);
    expect(isExternalHttpUrl("vbscript:msgbox(1)")).toBe(false);
  });

  it("is total on garbage — a shell prints whatever it likes", () => {
    expect(isExternalHttpUrl("")).toBe(false);
    expect(isExternalHttpUrl("not a url at all")).toBe(false);
    expect(isExternalHttpUrl("./relative/path")).toBe(false);
    expect(isExternalHttpUrl("http://")).toBe(false);
  });

  it("opens an allowed link through window.open, which main routes to the OS browser", () => {
    const open = vi.spyOn(window, "open").mockReturnValue(null);

    expect(openTerminalLink("https://example.com")).toBe(true);

    // `setWindowOpenHandler` in the main process DENIES the window and calls `shell.openExternal`,
    // so this call is the whole mechanism — there is no nyx IPC channel for it.
    expect(open).toHaveBeenCalledWith("https://example.com", "_blank", "noopener,noreferrer");
  });

  it("REFUSES a disallowed link without opening anything", () => {
    const open = vi.spyOn(window, "open").mockReturnValue(null);

    expect(openTerminalLink("javascript:alert(1)")).toBe(false);

    expect(open).not.toHaveBeenCalled();
  });

  it("OSC 8: activating a marked-up hyperlink opens it, and hover/leave report what is under the cursor", () => {
    const open = vi.spyOn(window, "open").mockReturnValue(null);
    const hovered: (string | null)[] = [];
    const handler = createTerminalLinkHandler((uri) => hovered.push(uri));
    const range = { start: { x: 1, y: 1 }, end: { x: 2, y: 1 } };
    const event = new MouseEvent("click");

    handler.activate(event, "https://example.com/osc8", range);
    handler.hover?.(event, "https://example.com/osc8", range);
    handler.leave?.(event, "https://example.com/osc8", range);

    expect(open).toHaveBeenCalledWith("https://example.com/osc8", "_blank", "noopener,noreferrer");
    // The right-click menu reads exactly this cell, so leaving MUST clear it or the menu would go
    // on offering a link the cursor is no longer on.
    expect(hovered).toEqual(["https://example.com/osc8", null]);
  });

  it("OSC 8: a hyperlink pointing somewhere we refuse is never reported as hovered", () => {
    const open = vi.spyOn(window, "open").mockReturnValue(null);
    const hovered: (string | null)[] = [];
    const handler = createTerminalLinkHandler((uri) => hovered.push(uri));
    const range = { start: { x: 1, y: 1 }, end: { x: 2, y: 1 } };

    handler.hover?.(new MouseEvent("mousemove"), "javascript:alert(1)", range);
    handler.activate(new MouseEvent("click"), "javascript:alert(1)", range);

    expect(hovered).toEqual([null]);
    expect(open).not.toHaveBeenCalled();
  });
});
