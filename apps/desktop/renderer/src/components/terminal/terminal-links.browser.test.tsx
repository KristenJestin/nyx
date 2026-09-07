// xterm's own stylesheet, exactly as the app entry loads it — the screen/rows layers are
// positioned by it, and a link is found by pointing at a CELL, so the geometry has to be real.
import "@xterm/xterm/css/xterm.css";

import { render } from "@testing-library/react";
import { page } from "vitest/browser";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

import { nyxBridge } from "@/bridge";
import type { FakeNyxBridge } from "@/bridge/fake";
import { Terminal } from "./terminal";
import { __resetTerminalStore, getTerminal } from "./terminal-store";

/**
 * LINKS, IN A REAL BROWSER (Chromium via Playwright).
 *
 * `terminal-links.test.ts` holds the POLICY — what may be opened, and what must not. What it
 * cannot hold is everything that only exists once text is painted: that a URL a shell merely
 * PRINTED is detected at all, that hovering it underlines it, that clicking it opens it, and that
 * a right-click on it offers to. Those are xterm's real link layer over a real layout, driven by a
 * real pointer, and nothing short of a browser answers them.
 *
 * POINTING AT A CELL, NOT AT AN ELEMENT. xterm's rows are `pointer-events: none` — `.xterm-screen`
 * takes every mouse event and resolves it to a cell itself. So a link is hovered by moving the
 * pointer to a POSITION on the screen layer (computed from the span that carries the text), never
 * by hovering the span, which Playwright rightly refuses as covered.
 *
 * The bridge is driven directly rather than through `@/bridge/test-harness` — see the note in
 * `terminal-respawn.browser.test.tsx`: that harness's `vi.mock` does not apply in browser mode,
 * and with no Electron preload in the page `@/bridge` has already chosen a `FakeNyxBridge`.
 */

const PTY_ID = 800;
const URL_IN_OUTPUT = "https://example.com/from-the-shell";

const bridge = nyxBridge as unknown as FakeNyxBridge;

let host: HTMLDivElement;

// Generous on purpose — see the note on the twin in `terminal-respawn.browser.test.tsx`: a starved
// browser worker spends most of a wait not scheduled at all.
async function waitFor(pred: () => boolean, timeoutMs = 20000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (pred()) return;
    await new Promise((r) => setTimeout(r, 20));
  }
  expect(pred(), "waitFor predicate timed out").toBe(true);
}

/** Mount a pane and have its shell PRINT `text`, exactly as a real `echo` would. */
async function printInPane(recordId: string, text: string): Promise<void> {
  render(<Terminal recordId={recordId} />, { container: host });
  await waitFor(() => getTerminal(recordId)?.pty != null);
  await waitFor(() => host.querySelector(".xterm-rows") != null);
  bridge.emit("pty://output", {
    id: PTY_ID,
    bytes: Array.from(new TextEncoder().encode(`${text}\r\n`)),
  });
  await waitFor(() => (getTerminal(recordId)?.readScreenText() ?? "").includes(text));
  // xterm paints on its own schedule; the span has to exist before it can be pointed at.
  await waitFor(() => spanCarrying(text) != null);
}

/** The rendered span whose text carries `needle`. */
function spanCarrying(needle: string): HTMLElement | null {
  const spans = host.querySelectorAll<HTMLElement>(".xterm-rows span");
  return Array.from(spans).find((s) => (s.textContent ?? "").includes(needle)) ?? null;
}

/** The screen layer — the element that actually receives the mouse. */
function screenLayer(): HTMLElement {
  const el = host.querySelector<HTMLElement>(".xterm-screen");
  if (!el) throw new Error("the xterm screen layer must be painted");
  return el;
}

/** The centre of `span`, expressed relative to the screen layer (what Playwright wants). */
function positionOf(span: HTMLElement): { x: number; y: number } {
  const s = span.getBoundingClientRect();
  const screen = screenLayer().getBoundingClientRect();
  return { x: s.left - screen.left + s.width / 2, y: s.top - screen.top + s.height / 2 };
}

beforeEach(() => {
  bridge.reset();
  bridge.onAnyCommand((cmd) => {
    if (cmd === "pty_spawn") return PTY_ID;
    if (cmd === "terminal_screen_rehydration") return { bytes: [] };
    return null;
  });
  host = document.createElement("div");
  host.style.width = "640px";
  host.style.height = "360px";
  host.style.position = "fixed";
  host.style.top = "0";
  host.style.left = "0";
  document.body.appendChild(host);
});

afterEach(() => {
  vi.restoreAllMocks();
  __resetTerminalStore();
  bridge.reset();
  document.body.replaceChildren();
});

it("underlines a printed URL on hover and opens it externally on click", async () => {
  const open = vi.spyOn(window, "open").mockReturnValue(null);
  await printInPane("rec-link", URL_IN_OUTPUT);

  const span = spanCarrying(URL_IN_OUTPUT)!;
  // Nothing is decorated until the pointer is on it: the underline IS the hover cue.
  expect(getComputedStyle(span).textDecorationLine).not.toContain("underline");

  const at = positionOf(span);
  await page.elementLocator(screenLayer()).hover({ position: at });

  await waitFor(() =>
    getComputedStyle(spanCarrying(URL_IN_OUTPUT)!).textDecorationLine.includes("underline"),
  );
  expect(
    screenLayer().classList.contains("xterm-cursor-pointer"),
    "a link must also read as clickable under the cursor",
  ).toBe(true);

  await page.elementLocator(screenLayer()).click({ position: at });

  // `window.open` IS the mechanism: Electron main's `setWindowOpenHandler` denies the window and
  // hands http(s) to `shell.openExternal`, so the link lands in the user's real browser.
  expect(open).toHaveBeenCalledWith(URL_IN_OUTPUT, "_blank", "noopener,noreferrer");
});

it("offers « Open link » only when the right-click lands on a link", async () => {
  const open = vi.spyOn(window, "open").mockReturnValue(null);
  await printInPane("rec-menu", URL_IN_OUTPUT);
  const screen = screenLayer();

  // FIRST, a right-click that is NOT on a link (an empty row well below the text).
  const empty = { x: 8, y: screen.getBoundingClientRect().height - 8 };
  await page.elementLocator(screen).click({ button: "right", position: empty });
  await waitFor(() => document.querySelector('[role="menu"]') != null);
  expect(
    document.body.textContent?.includes("Open link"),
    "an item for a link that is not there would be noise on every right-click",
  ).toBe(false);
  await page.getByRole("menuitem", { name: "Paste" }).click();
  await waitFor(() => document.querySelector('[role="menu"]') == null);

  // NOW on the link. The span is re-queried here rather than reused: xterm repaints its rows
  // freely (the menu's open/close is enough), and a detached node measures 0×0 — which would aim
  // the pointer at the top-left corner and prove nothing.
  const at = positionOf(spanCarrying(URL_IN_OUTPUT)!);
  await page.elementLocator(screen).hover({ position: at });
  await waitFor(() => getTerminal("rec-menu")?.hoveredLink === URL_IN_OUTPUT);
  await page.elementLocator(screen).click({ button: "right", position: at });

  await page.getByRole("menuitem", { name: "Open link" }).click();

  expect(open).toHaveBeenCalledWith(URL_IN_OUTPUT, "_blank", "noopener,noreferrer");
});
