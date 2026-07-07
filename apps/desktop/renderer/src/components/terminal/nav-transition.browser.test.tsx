// Load wterm's stylesheet exactly as the app entry (main.tsx) does, so the live
// DOM render carries the real `.wterm` / `.term-grid` / `.term-row` rules.
import "@wterm/dom/css";

import { render } from "@testing-library/react";
import { afterEach, beforeEach, expect, it } from "vitest";

import { nyxBridge } from "@/bridge";
import { startNavTransition } from "@/lib/nav-transition";
import { Terminal } from "./terminal";
import { getTerminal, __resetTerminalStore } from "./terminal-store";

/**
 * NAV CROSS-FADE — the terminal SURVIVES a View-Transition-wrapped navigation.
 *
 * The v2.2.5 follow-up (finding 01KWV88BMDYXR8F0V6TSNFJSQQ) wraps the main-screen
 * change in `document.startViewTransition` (`src/lib/nav-transition.ts`). The
 * critical constraint is that the cross-fade must NOT compromise the lossless
 * terminal: the persistent DOM node + its wterm/ghostty instance (module-level
 * store) must survive the transition — same instance, no `destroy()`, no respawn.
 *
 * This runs a real detach→reattach cycle INSIDE `startNavTransition` in a real
 * Chromium (Playwright, headless), where `document.startViewTransition` actually
 * exists (unlike jsdom), and asserts identity + spawn count are unchanged across
 * the traversal. It is kept OUT of `gate-lossless.browser.test.tsx` so that gate
 * stays exactly at its 5 validation assertions.
 */

interface CatchAllBridge {
  onAnyCommand(handler: (command: string, args?: Record<string, unknown>) => unknown): unknown;
  reset?(): void;
  calls?: ReadonlyArray<{ command: string }>;
}
function installBackend(handler: (cmd: string, args?: Record<string, unknown>) => unknown): void {
  (nyxBridge as unknown as CatchAllBridge).onAnyCommand(handler);
}
/** How many `pty_spawn` invokes the terminals have made so far (respawn detector). */
function spawnCount(): number {
  const calls = (nyxBridge as unknown as CatchAllBridge).calls ?? [];
  return calls.filter((c) => c.command === "pty_spawn").length;
}

beforeEach(() => {
  installBackend((cmd) => {
    if (cmd === "pty_spawn") return 7;
    if (cmd === "terminal_screen_rehydration") return { bytes: [] };
    if (cmd === "list_terminals") return [];
    if (cmd === "terminal_info") return { cwd: null, foreground: null };
    return null;
  });
});

afterEach(() => {
  __resetTerminalStore();
  (nyxBridge as unknown as CatchAllBridge).reset?.();
  document.body.replaceChildren();
});

function mountHost(w = 640, h = 360): HTMLElement {
  const host = document.createElement("div");
  host.style.width = `${w}px`;
  host.style.height = `${h}px`;
  host.style.position = "fixed";
  host.style.top = "0";
  host.style.left = "0";
  document.body.appendChild(host);
  return host;
}

async function waitForRender(host: HTMLElement): Promise<void> {
  const deadline = Date.now() + 8000;
  while (Date.now() < deadline) {
    if (host.querySelector(".wterm .term-grid .term-row")) return;
    await new Promise((r) => setTimeout(r, 50));
  }
}

async function waitFor(pred: () => boolean, timeoutMs = 6000): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (pred()) return true;
    await new Promise((r) => setTimeout(r, 50));
  }
  return pred();
}

const raf = () => new Promise((r) => requestAnimationFrame(() => r(null)));

it("the terminal instance SURVIVES a navigation wrapped in startNavTransition (same instance, no respawn)", async () => {
  // The real View Transitions API exists in this Chromium — this is the branch
  // where `startNavTransition` DOES wrap the swap (not the jsdom fallback).
  expect(
    typeof (document as { startViewTransition?: unknown }).startViewTransition,
    "real Chromium exposes the View Transitions API",
  ).toBe("function");

  const host = mountHost();
  const { unmount } = render(<Terminal recordId="vt" active />, { container: host });
  await waitForRender(host);

  const term = getTerminal("vt")!;
  expect(term, "the terminal instance exists").toBeDefined();
  term.writeToScreen("VT-CROSS-FADE-KEEP\r\n");
  await raf();
  await new Promise((r) => setTimeout(r, 120));

  await waitFor(() => spawnCount() === 1, 4000);
  const spawnsBefore = spawnCount();
  const nodeBefore = term.el;
  const bufBefore = term.readScreenText();
  expect(bufBefore).toContain("VT-CROSS-FADE-KEEP");

  // NAVIGATE AWAY inside a View Transition: unmount the pane (a route change). The
  // VT captures a snapshot for the fade; the store must keep the LIVE instance.
  await new Promise<void>((resolve) => {
    startNavTransition(() => {
      unmount();
      resolve();
    });
  });
  await new Promise((r) => setTimeout(r, 100));

  // The instance survived the transition in the module-level store — same object,
  // its persistent node detached (not rebuilt), full screen state intact.
  expect(getTerminal("vt"), "same instance survives the View Transition").toBe(term);
  expect(term.el, "the persistent node was not rebuilt by the VT").toBe(nodeBefore);
  expect(term.el.isConnected, "the node is detached while navigated away").toBe(false);
  expect(term.readScreenText(), "screen state intact while detached").toBe(bufBefore);

  // NAVIGATE BACK inside a View Transition: remount a fresh pane for the same id.
  const host2 = mountHost();
  await new Promise<void>((resolve) => {
    startNavTransition(() => {
      render(<Terminal recordId="vt" active />, { container: host2 });
      resolve();
    });
  });
  await waitForRender(host2);
  await raf();
  await new Promise((r) => setTimeout(r, 120));

  const after = getTerminal("vt")!;
  expect(after, "still the SAME instance after the return transition").toBe(term);
  expect(after.el, "still the SAME persistent node").toBe(nodeBefore);
  expect(host2.contains(after.el), "the node was re-attached into the returned pane").toBe(true);
  expect(after.readScreenText(), "screen state intact across the cross-fade").toBe(bufBefore);
  // Hard proof of zero reconstruction: the shell was never respawned by the VT.
  expect(spawnCount(), "no respawn across a View-Transition-wrapped navigation").toBe(spawnsBefore);
});
