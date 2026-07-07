// Load wterm's own stylesheet exactly as the app entry (main.tsx) does — it
// carries the `.wterm` / `.term-grid` / `.term-row` layout rules the DOM
// renderer relies on. Importing it here lets the browser suite assert the live
// DOM render.
import "@wterm/dom/css";

import { render } from "@testing-library/react";
import { page } from "vitest/browser";
import { afterEach, beforeEach, expect, it } from "vitest";

import { mockIPC } from "@/bridge/test-harness";
import { Terminal } from "./terminal";
import { getTerminal, __resetTerminalStore } from "./terminal-store";

/**
 * BROWSER-MODE render test (real Chromium via Playwright, headless).
 *
 * Unlike the jsdom unit suite, this runs in a browser that actually PAINTS and
 * can INSTANTIATE THE REAL GHOSTTY WASM CORE, so it is the only place we can
 * prove the wterm + ghostty stack renders a shell AND a TUI (alt-screen)
 * correctly, and pin a visual baseline. The backend does not exist in the
 * browser, so we mock its IPC; we drive the visible output by writing straight
 * into the store instance (deterministic — no shell, no timing, no prompt noise).
 */

const SPAWNED_ID = 7;

let ipcCalls: { cmd: string; args: Record<string, unknown> }[] = [];

beforeEach(() => {
  ipcCalls = [];
  mockIPC(
    (cmd, args) => {
      ipcCalls.push({ cmd, args: (args ?? {}) as Record<string, unknown> });
      return cmd === "pty_spawn" ? SPAWNED_ID : null;
    },
    { shouldMockEvents: true },
  );
});

afterEach(() => {
  __resetTerminalStore();
  document.body.replaceChildren();
});

/** Poll until a store instance's wterm DOM has been built inside `host`. */
async function waitForRender(host: HTMLElement): Promise<void> {
  const deadline = Date.now() + 8000;
  while (Date.now() < deadline) {
    if (host.querySelector(".wterm .term-grid .term-row")) return;
    await new Promise((r) => setTimeout(r, 50));
  }
}

it("renders a shell with wterm + the real ghostty core and matches the visual baseline", async () => {
  const host = document.createElement("div");
  host.setAttribute("data-testid", "terminal-host");
  host.style.width = "640px";
  host.style.height = "360px";
  host.style.position = "fixed";
  host.style.top = "0";
  host.style.left = "0";
  document.body.appendChild(host);

  render(<Terminal recordId="t1" />, { container: host });
  await waitForRender(host);

  const term = getTerminal("t1");
  expect(term, "store instance must exist").toBeDefined();

  await document.fonts.load('14px "Fira Code Variable"');
  await document.fonts.ready;

  // Write a fixed, known sequence (shell-like) with SGR colours, straight into
  // the screen (the same path a pty://output chunk takes) for a deterministic frame.
  term!.writeToScreen("nyx wterm+ghostty render check\r\n");
  term!.writeToScreen("\x1b[32mgreen\x1b[0m \x1b[31mred\x1b[0m \x1b[34mblue\x1b[0m\r\n");
  term!.writeToScreen("12345 67890 ABCDE fghij\r\n");

  await new Promise((r) => requestAnimationFrame(() => r(null)));
  await new Promise((r) => setTimeout(r, 120));

  // The DOM projection reflects the written text (real render, not a canvas).
  const gridText = host.querySelector(".term-grid")?.textContent ?? "";
  expect(gridText).toContain("render check");
  expect(gridText).toContain("green");

  await expect(page.getByTestId("terminal-host")).toMatchScreenshot("terminal-wterm-render");
});

it("renders a TUI on the alt-screen (full-screen redraw survives)", async () => {
  const host = document.createElement("div");
  host.style.width = "480px";
  host.style.height = "300px";
  document.body.appendChild(host);

  render(<Terminal recordId="t2" />, { container: host });
  await waitForRender(host);
  const term = getTerminal("t2")!;

  // Enter the alt-screen and paint a full-screen frame (what vim/htop do).
  term.writeToScreen("\x1b[?1049h"); // enter alt-screen
  term.writeToScreen("\x1b[2J\x1b[H"); // clear + home
  term.writeToScreen("TUI-ALT-SCREEN-FRAME");
  await new Promise((r) => setTimeout(r, 120));

  // The core reports it is on the alt-screen and the frame is projected.
  expect(term.usingAltScreen()).toBe(true);
  const gridText = host.querySelector(".term-grid")?.textContent ?? "";
  expect(gridText).toContain("TUI-ALT-SCREEN-FRAME");
});

it("focuses the terminal input when it becomes the active pane (typing fix)", async () => {
  const host = document.createElement("div");
  host.style.width = "320px";
  host.style.height = "200px";
  document.body.appendChild(host);

  const stealFocus = document.createElement("button");
  stealFocus.textContent = "sidebar";
  document.body.appendChild(stealFocus);

  const { rerender } = render(<Terminal recordId="t3" active={false} />, { container: host });
  await waitForRender(host);

  stealFocus.focus();
  expect(document.activeElement).toBe(stealFocus);

  rerender(<Terminal recordId="t3" active />);

  // The focus effect defers one animation frame; poll until the wterm input
  // (a child textarea of the persistent node) is the active element.
  const deadline = Date.now() + 3000;
  let focused = false;
  while (Date.now() < deadline) {
    const ae = document.activeElement as HTMLElement | null;
    if (ae && ae.tagName === "TEXTAREA" && host.contains(ae)) {
      focused = true;
      break;
    }
    await new Promise((r) => requestAnimationFrame(() => r(null)));
  }
  expect(focused, "activating a terminal must focus its input so typing reaches it").toBe(true);
});

it("derives the terminal palette from the CSS tokens as a parseable hex (oklch → #rrggbb)", async () => {
  const host = document.createElement("div");
  host.style.width = "320px";
  host.style.height = "200px";
  document.body.appendChild(host);
  document.documentElement.classList.add("dark");

  render(<Terminal recordId="t4" />, { container: host });
  await waitForRender(host);

  const wtermEl = host.querySelector(".wterm") as HTMLElement | null;
  expect(wtermEl, "the .wterm host must exist").not.toBeNull();

  // The palette is applied as CSS variables on the host (oklch → sRGB hex in JS).
  const bg = wtermEl!.style.getPropertyValue("--term-bg").trim();
  const fg = wtermEl!.style.getPropertyValue("--term-fg").trim();
  expect(bg).toMatch(/^#[0-9a-f]{6}$/i);
  expect(fg).toMatch(/^#[0-9a-f]{6}$/i);
  expect(bg).not.toMatch(/oklch/i);
  const lum = (h: string) =>
    parseInt(h.slice(1, 3), 16) + parseInt(h.slice(3, 5), 16) + parseInt(h.slice(5, 7), 16);
  expect(lum(bg)).toBeLessThan(lum(fg));
});
