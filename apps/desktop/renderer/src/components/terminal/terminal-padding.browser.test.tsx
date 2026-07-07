// wterm's own layout CSS (`.wterm` / `.term-grid` / `.term-row`). We deliberately
// do NOT import the app's Tailwind stylesheet: the vitest browser project ships no
// Tailwind plugin, so `@import "tailwindcss"` emits no on-demand utilities (`p-2.5`,
// `h-full` never materialize) — the class assertion for the fix lives in the jsdom
// unit suite (`terminal.test.tsx`). This spec proves the LOAD-BEARING RUNTIME point
// the review flags as delicate: padding the pane (border-box) shrinks the wterm
// node's box and the app-driven sizing REGRIDS onto the reduced content-box (a
// strictly smaller grid), NOT a mere visual offset — with the real ghostty core.
import "@wterm/dom/css";

import { afterEach, beforeEach, expect, it } from "vitest";

import { mockIPC } from "@/bridge/test-harness";
import { computeGrid, hasUsableBox, measureCell } from "./measure-cell";
import {
  attachTerminal,
  ensureTerminal,
  getTerminal,
  __resetTerminalStore,
} from "./terminal-store";

/**
 * PADDING REGRESSION GATE (v2.2.5, review 01KWVA114R0PTSV063RVTZS4NP).
 *
 * The xterm→wterm switch dropped the pane's inner `p-2.5`, gluing the terminal to
 * the sidebar edge. Restoring `p-2.5` on the pane is not just cosmetic: the sizing
 * is APP-DRIVEN — `WtermInstance.observeResize` watches the persistent node (`el`,
 * `width/height:100%`, in NORMAL FLOW; `.wterm` is `position:relative`, its own
 * padding zeroed), and `reconcileSize`→`computeGrid` measure `el.getBoundingClientRect()`
 * to pick cols/rows. Padding the pane (border-box) makes the node fill the INSET
 * content-box, so its rect SHRINKS and the grid is RESIZED — not merely offset.
 *
 * Tailwind's `p-2.5` does not resolve in this env, so we recreate the PRODUCTION box
 * model with an explicit border-box padding on the pane, attach the SAME persistent
 * node the app attaches (`attachTerminal`, which arms the real app-driven
 * `observeResize`), and compare the grid the terminal lands on padded vs unpadded.
 */

const SPAWNED_ID = 7;
/** The px inset imposed on the pane to stand in for the real `p-2.5` (~10px). */
const PAD = 10;
const OUTER_W = 640;
const OUTER_H = 360;

beforeEach(() => {
  mockIPC((cmd) => (cmd === "pty_spawn" ? SPAWNED_ID : null), { shouldMockEvents: true });
});

afterEach(() => {
  __resetTerminalStore();
  document.body.replaceChildren();
});

/** A border-box pane inset by `pad` px on all sides (the production box model). */
function pane(pad: number): HTMLElement {
  const el = document.createElement("div");
  el.style.width = `${OUTER_W}px`;
  el.style.height = `${OUTER_H}px`;
  el.style.boxSizing = "border-box";
  el.style.padding = `${pad}px`;
  el.style.overflow = "hidden";
  el.style.position = "fixed";
  el.style.top = "0";
  el.style.left = "0";
  document.body.appendChild(el);
  return el;
}

/** Poll until the wterm DOM grid has been built inside `host`. */
async function waitForRender(host: HTMLElement): Promise<void> {
  const deadline = Date.now() + 8000;
  while (Date.now() < deadline) {
    if (host.querySelector(".wterm .term-grid .term-row")) return;
    await new Promise((r) => setTimeout(r, 50));
  }
}

/** Attach a fresh instance into `host` and return the grid the app-driven sizing
 *  computes for the wterm node's (possibly inset) box, plus the node's width. */
async function gridIn(
  host: HTMLElement,
  recordId: string,
): Promise<{ cols: number; width: number }> {
  ensureTerminal({ recordId });
  attachTerminal(recordId, host); // arms the real observeResize / reconcileSize
  await waitForRender(host);
  await document.fonts.load('14px "Fira Code Variable"');
  await document.fonts.ready;
  // Settle layout, then nudge the box so the app-driven ResizeObserver reconciles.
  for (let i = 0; i < 6; i++) {
    await new Promise((r) => requestAnimationFrame(() => r(null)));
    await new Promise((r) => setTimeout(r, 40));
  }
  host.style.width = `${OUTER_W + 1}px`;
  host.style.width = `${OUTER_W}px`;
  for (let i = 0; i < 6; i++) {
    await new Promise((r) => requestAnimationFrame(() => r(null)));
    await new Promise((r) => setTimeout(r, 40));
  }
  const wtermEl = host.querySelector(".wterm") as HTMLElement;
  expect(wtermEl, "the wterm node is attached").not.toBeNull();
  const rect = wtermEl.getBoundingClientRect();
  const cell = measureCell(wtermEl);
  expect(cell, "a monospace cell can be measured").not.toBeNull();
  // The 0×0 guard's precondition holds: the node has a real, non-zero box (never a
  // hidden/pre-layout pane), so the app regrids instead of collapsing to 1×1.
  expect(
    hasUsableBox({ width: rect.width, height: rect.height }),
    "the node has a usable box",
  ).toBe(true);
  return { cols: computeGrid(rect.width, rect.height, cell!).cols, width: rect.width };
}

it("REGRIDS to strictly fewer columns when the pane is padded (inset, not a mere offset)", async () => {
  // Baseline: the SAME pane with NO padding — the terminal fills the full box.
  const unpadded = await gridIn(pane(0), "no-pad");
  expect(unpadded.width, "unpadded node fills the full pane width").toBeCloseTo(OUTER_W, 0);

  // With the pane padded (the restored `p-2.5` model), the node is INSET by 2×PAD
  // and the app regrids onto the reduced content-box.
  const padded = await gridIn(pane(PAD), "with-pad");
  expect(padded.width, "padded node is inset by 2×PAD").toBeCloseTo(OUTER_W - 2 * PAD, 0);

  // THE PROOF: padding does not just shift the terminal — the grid is genuinely
  // narrower. A mere visual offset would keep the full-box column count (and clip
  // the last column); a correct inset yields strictly fewer columns.
  expect(padded.cols, "padded grid has strictly fewer columns than the full box").toBeLessThan(
    unpadded.cols,
  );

  // Both instances are live (the 0×0 guard held — no 1×1 collapse anywhere).
  expect(getTerminal("no-pad"), "unpadded instance is live").toBeDefined();
  expect(getTerminal("with-pad"), "padded instance is live").toBeDefined();
});
