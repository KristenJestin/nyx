// wterm's own layout CSS (`.wterm` / `.term-grid` / `.term-row`) — the same
// stylesheet the app entry (main.tsx) loads — so the live DOM render carries the
// real grid rules and the ghostty core paints one `.term-row` per grid row. We do
// NOT import the app's Tailwind sheet (the browser project ships no Tailwind
// plugin), so we recreate the production box model (a padded, fixed-size pane)
// explicitly, exactly as `terminal-padding.browser.test.tsx` does.
import "@wterm/dom/css";

import { afterEach, beforeEach, expect, it } from "vitest";

import { mockIPC } from "@/bridge/test-harness";
import {
  attachTerminal,
  ensureTerminal,
  getTerminal,
  __resetTerminalStore,
} from "./terminal-store";

/**
 * HEIGHT-FILL REGRESSION GATE (v2.2.5, review 01KWVC1R0JY2JJ7KDJW39Q4BFA,
 * finding 01KWVC1RPRC329MR4AS7M6B34Y).
 *
 * The xterm→wterm switch regressed VERTICAL sizing: with `autoResize:false` wterm
 * locks `el.style.height` to `rows×rowHeight` ONCE at init (~24 rows) and its
 * `resize()` never re-locks. Measuring `el` (height-frozen) pinned rows to ~24
 * forever, leaving a dead band below the terminal; the width recomputed only
 * because `el` is `width:100%`.
 *
 * The fix makes sizing measure the PANE CONTAINER (the parent's content-box, which
 * is height-authoritative and unlocked), regrids onto it, and RE-LOCKS `el`'s
 * height so the node grows to fill. `observeResize` watches the PARENT so a pane
 * growing taller fires a reconcile.
 *
 * This spec runs in a REAL Chromium with the REAL ghostty core and proves, over the
 * genuine app seams (`attachTerminal` arms the real `observeResize`/`reconcileSize`):
 *   (a) a TALL pane yields rows FAR above the ~24 lock (it fills the height);
 *   (b) GROWING the pane increases rows (reflow, not a static offset);
 *   (c) SHRINKING the pane decreases rows;
 *   (d) the 0×0 guard holds — a 0×0 / display:none pane never collapses the grid.
 */

const SPAWNED_ID = 11;
/** The px inset standing in for the real pane `p-2.5` (~10px). */
const PAD = 10;
const PANE_W = 800;

beforeEach(() => {
  mockIPC((cmd) => (cmd === "pty_spawn" ? SPAWNED_ID : null), { shouldMockEvents: true });
});

afterEach(() => {
  __resetTerminalStore();
  document.body.replaceChildren();
});

/** A border-box pane inset by PAD px on all sides (the production `p-2.5` box). */
function pane(width: number, height: number): HTMLElement {
  const el = document.createElement("div");
  el.style.width = `${width}px`;
  el.style.height = `${height}px`;
  el.style.boxSizing = "border-box";
  el.style.padding = `${PAD}px`;
  el.style.overflow = "hidden";
  el.style.position = "fixed";
  el.style.top = "0";
  el.style.left = "0";
  el.style.background = "black";
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

/** Let layout + the app-driven ResizeObserver settle over a few frames. */
async function settle(): Promise<void> {
  for (let i = 0; i < 8; i++) {
    await new Promise((r) => requestAnimationFrame(() => r(null)));
    await new Promise((r) => setTimeout(r, 30));
  }
}

/**
 * The number of LIVE grid rows the ghostty core currently holds, read off the
 * DOM projection (one `.term-row:not(.term-scrollback-row)` per grid row — the
 * same surface the gate-lossless spec reads). This is the real, painted row count,
 * so it proves the grid actually grew (not just a style tweak).
 */
function liveRowCount(host: HTMLElement): number {
  return host.querySelectorAll(".wterm .term-grid .term-row:not(.term-scrollback-row)").length;
}

/** Poll until `pred()` holds (or the deadline), then return its final value. */
async function waitFor(pred: () => boolean, timeoutMs = 6000): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (pred()) return true;
    await new Promise((r) => setTimeout(r, 50));
  }
  return pred();
}

it("FILLS the pane height: a tall pane yields rows far above the ~24 lock, and rows track the pane size", async () => {
  // A tall pane: ~720px content height. At ~19px/row that is ~37 rows — WAY above
  // the ~24-row init lock the regression was frozen at.
  const host = pane(PANE_W, 720);
  ensureTerminal({ recordId: "fill" });
  attachTerminal("fill", host); // arms the REAL observeResize on the parent + reconciles
  await waitForRender(host);
  await document.fonts.load('14px "Fira Code Variable"');
  await document.fonts.ready;
  await settle();

  const term = getTerminal("fill")!;
  expect(term, "the instance is live").toBeDefined();

  // (a) The grid FILLS the tall pane: rows are far above the ~24-row lock and the
  // node's height was re-locked to the grid (fills the content-box, no dead band).
  const filledRows = liveRowCount(host);
  expect(filledRows, "a tall pane yields rows far above the ~24 lock").toBeGreaterThan(30);

  // The re-locked node height fills the pane content-box (720 − 2×PAD = 700px),
  // within one row of it (floor(avail / rowHeight) × rowHeight ≤ avail < +1 row).
  const wtermEl = host.querySelector(".wterm") as HTMLElement;
  const nodeH = wtermEl.getBoundingClientRect().height;
  const availH = 720 - 2 * PAD;
  const rowH = availH / filledRows; // ≈ the measured row height
  expect(nodeH, "the re-locked node fills the pane height (no dead band below)").toBeGreaterThan(
    availH - rowH,
  );
  expect(nodeH, "the node never overshoots the pane content-box").toBeLessThanOrEqual(availH + 1);

  // (b) GROW the pane → the observer on the PARENT fires and rows INCREASE.
  host.style.height = "1040px"; // +320px ≈ +16 rows
  const grew = await waitFor(() => liveRowCount(host) > filledRows);
  expect(grew, "growing the pane must add rows (vertical reflow, not a static offset)").toBe(true);
  const grownRows = liveRowCount(host);
  expect(grownRows, "a taller pane holds more rows than the shorter one").toBeGreaterThan(
    filledRows,
  );

  // (c) SHRINK the pane → rows DECREASE back down.
  host.style.height = "380px"; // ≈ half of the original tall pane
  const shrank = await waitFor(() => liveRowCount(host) < grownRows);
  expect(shrank, "shrinking the pane must drop rows").toBe(true);
  const shrunkRows = liveRowCount(host);
  expect(shrunkRows, "a shorter pane holds fewer rows than the tall/grown ones").toBeLessThan(
    filledRows,
  );
  // Still a real, usable grid — never collapsed to 1 row.
  expect(shrunkRows, "the shrunk grid is still a real multi-row grid").toBeGreaterThan(1);
});

it("0×0 GUARD: a display:none / zero-size pane never collapses the grid to 1×1", async () => {
  // Establish a real grid in a tall pane first.
  const host = pane(PANE_W, 640);
  ensureTerminal({ recordId: "guard" });
  attachTerminal("guard", host);
  await waitForRender(host);
  await document.fonts.load('14px "Fira Code Variable"');
  await document.fonts.ready;
  await settle();

  const term = getTerminal("guard")!;
  const rowsBefore = liveRowCount(host);
  expect(rowsBefore, "the grid filled the tall pane before hiding").toBeGreaterThan(20);

  // Hide the pane (display:none → 0×0 measured box). The app-driven reconcile must
  // hit the 0×0 guard and SKIP — never a destructive 1×1 resize.
  host.style.display = "none";
  await settle();

  // The instance survives and its grid did NOT collapse: the last good row count is
  // preserved (the guard skipped the resize on the 0×0 box).
  expect(getTerminal("guard"), "the instance survives while hidden").toBe(term);
  expect(
    liveRowCount(host),
    "a hidden (0×0) pane must not collapse the grid — the 0×0 guard held",
  ).toBe(rowsBefore);

  // Reveal it again: the grid is intact and reconciles back to the pane.
  host.style.display = "";
  await settle();
  expect(
    liveRowCount(host),
    "the grid is restored (still a full multi-row grid) after the pane reappears",
  ).toBeGreaterThan(20);
});
