// wterm's own layout CSS (`.wterm` / `.term-grid` / `.term-row`) — the same
// stylesheet the app entry (main.tsx) loads — so the live DOM carries the real
// grid rules AND the scroll model: `.wterm.has-scrollback { overflow-y:auto }`
// makes the persistent node (`el`) the scroll viewport once the buffer overflows.
// We do NOT import the app's Tailwind sheet (the browser project ships no Tailwind
// plugin); the production box model is recreated explicitly, exactly as
// `terminal-height-fill.browser.test.tsx` / `terminal-padding.browser.test.tsx` do.
import "@wterm/dom/css";

import { afterEach, beforeEach, expect, it } from "vitest";

import { mockIPC } from "@/bridge/test-harness";
import {
  attachTerminal,
  detachTerminal,
  ensureTerminal,
  getTerminal,
  __resetTerminalStore,
} from "./terminal-store";

/**
 * SCROLL-FOLLOW REGRESSION GATE (v2.2.5, review 01KWVGFK9DG3FKDWENQHC1K5Z7,
 * finding 01KWVGFKRRVHNWKRRSVX6YTGRK).
 *
 * The height-fill fix regressed scroll following. `reconcileSize` re-locks
 * `el.style.height` (and may `term.resize()`) on every reconcile — including the
 * one `observeResize` fires on every REATTACH. `el` is the `.wterm` scroll viewport
 * (overflow-y:auto via `.has-scrollback`), and a detach (`el.remove()`) RESETS its
 * `scrollTop` to 0. So after navigating away and back, the reconcile leaves the view
 * parked at the TOP: `_isScrolledToBottom()` (wterm.js:83) reads false, so the next
 * write/resize does NOT follow (wterm.js:102,114,164) — the live prompt/input sits
 * far below the viewport and a keystroke barely moves it. (wterm's `_scrollToBottom`/
 * `_isScrolledToBottom` are PRIVATE, so the app pilots `el.scrollTop` directly.)
 *
 * The fix ("sticky bottom"): track a standing `followBottom` intent that SURVIVES the
 * detach (captured in `stopObserving` while the old `scrollTop` is still valid), and
 * on reconcile re-glue `el.scrollTop = el.scrollHeight` (deferred a double rAF for
 * wterm's async render) when that intent holds — while a user who had scrolled up into
 * the scrollback keeps their position (no forced jump).
 *
 * This spec runs in a REAL Chromium with the REAL ghostty core over the genuine app
 * seams (`attachTerminal`/`detachTerminal` drive the real `observeResize` +
 * `reconcileSize`; `writeToScreen` feeds the core the path a `pty://output` chunk
 * takes). It writes FAR more lines than the pane holds (a real scrollback overflow),
 * detaches + reattaches into a DIFFERENT-sized pane (the nav round-trip), and asserts
 * the view is glued to the bottom and new output keeps following. WITHOUT the fix the
 * reset `scrollTop` leaves the view parked at the top and the assertion FAILS — the
 * spec captures the regression. (Verified by temporarily disabling the sticky-bottom
 * restore: this test reddens, `A reattached gap ≈ 4590px` instead of ≈ 0.)
 */

const SPAWNED_ID = 21;
/** The px inset standing in for the real pane `p-2.5` (~10px). */
const PAD = 10;
const PANE_W = 800;
/** Far more lines than any bounded pane holds, so a real scrollback overflow forms. */
const LINE_COUNT = 300;

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

/** Let layout + the app-driven ResizeObserver + wterm's async render settle. */
async function settle(): Promise<void> {
  for (let i = 0; i < 14; i++) {
    await new Promise((r) => requestAnimationFrame(() => r(null)));
    await new Promise((r) => setTimeout(r, 30));
  }
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

/** The `.wterm` node the instance owns — the app-driven scroll viewport. */
function wtermNode(recordId: string): HTMLElement {
  const el = getTerminal(recordId)?.el;
  expect(el, "the instance's wterm node exists").toBeTruthy();
  return el!;
}

/** True when the viewport is glued to (within a row of) the bottom of the buffer. */
function atBottom(el: HTMLElement, tolerance: number): boolean {
  return el.scrollTop + el.clientHeight >= el.scrollHeight - tolerance;
}

/** Attach a fresh instance into `host`, then write far more lines than it holds. */
async function buildOverflowing(recordId: string, host: HTMLElement): Promise<HTMLElement> {
  ensureTerminal({ recordId });
  attachTerminal(recordId, host);
  await waitForRender(host);
  await document.fonts.load('14px "Fira Code Variable"');
  await document.fonts.ready;
  await settle();

  const term = getTerminal(recordId)!;
  for (let i = 0; i < LINE_COUNT; i++) term.writeToScreen(`scroll-follow line ${i}\r\n`);
  term.writeToScreen("PROMPT$ "); // the live-prompt stand-in the user must keep seeing
  await settle();

  const el = wtermNode(recordId);
  await waitFor(() => el.classList.contains("has-scrollback"));
  expect(
    el.scrollHeight,
    "the buffer overflows the viewport (there is real scrollback)",
  ).toBeGreaterThan(el.clientHeight + 50);
  return el;
}

const TOLERANCE = 40; // ~2 rows — far below the multi-hundred-px gap the regression parks at.

it("FOLLOWS the bottom across a nav round-trip: after detach + reattach the view stays glued to the bottom and new output keeps following", async () => {
  // A bounded pane; the 300-line buffer overflows it, so the view is following the
  // bottom (wterm stuck it there on the writes).
  const host = await pane(PANE_W, 420);
  const el = await buildOverflowing("follow", host);
  expect(atBottom(el, TOLERANCE), "the view starts glued to the bottom").toBe(true);

  // NAV AWAY: detach the node (removes it from the DOM — this RESETS `el.scrollTop` to
  // 0). Then NAV BACK: reattach into a DIFFERENT-sized pane (the real nav round-trip),
  // which fires the real `observeResize` → `reconcileSize`. Without the sticky-bottom
  // intent surviving the detach, the reset `scrollTop` leaves the view at the TOP.
  detachTerminal("follow");
  await settle();
  const host2 = await pane(PANE_W, 560);
  attachTerminal("follow", host2);
  await settle();

  // THE PROOF: the view is glued to the bottom again — the prompt is visible, not
  // parked at the top of a several-thousand-px buffer.
  const glued = await waitFor(() => atBottom(el, TOLERANCE));
  expect(
    glued,
    "after a nav round-trip the view must be glued to the bottom (the prompt stays visible)",
  ).toBe(true);
  expect(atBottom(el, TOLERANCE), "scrollTop + clientHeight ≥ scrollHeight − tolerance").toBe(true);

  // And new output (the user typing / a command running after nav-back) keeps
  // following the bottom — the exact behaviour the regression broke.
  const term = getTerminal("follow")!;
  for (let i = 0; i < 20; i++) term.writeToScreen(`post-nav output ${i}\r\n`);
  term.writeToScreen("LIVE-PROMPT$ ");
  await settle();

  const followed = await waitFor(() => atBottom(el, TOLERANCE));
  expect(
    followed,
    "output written after nav-back must scroll the view to the bottom (the live prompt stays visible)",
  ).toBe(true);
});

it("FOLLOWS the bottom while OUTPUT STREAMS (chained commands, NO resize): several successive batches keep the view glued to the bottom", async () => {
  // Follow-up review 01KWVMF7PCWM5S289TDKMSGFR3, finding 01KWVMF89AAWR1RRYSHE4937JY.
  //
  // Regression captured here: the previous fix re-glued to the bottom ONLY on resize
  // (reconcileSize via the ResizeObserver). On output that STREAMS through `write()`
  // WITHOUT any resize — the real case of chaining short commands — wterm follows only
  // when `_isScrolledToBottom()` was true before the write (<5px, wterm.js:83,102,164);
  // once the view drifts (the prompt sits ~48px below the fold) wterm decouples and the
  // lag accrues, so the prompt stays below the fold with no resize nudge to recollect it.
  //
  // This spec writes SEVERAL successive batches (chained commands), each a few lines,
  // with NO resize between them, and asserts the view is glued to the bottom AFTER the
  // last batch. WITHOUT the output-side sticky-bottom hook it reddens: the view parks
  // hundreds of px below the fold. (Verified by temporarily removing the `writeToScreen`
  // stick — the assertion fails, gap ≈ several hundred px instead of ≤ TOLERANCE.)
  const host = await pane(PANE_W, 420);
  const el = await buildOverflowing("stream", host);
  expect(atBottom(el, TOLERANCE), "the view starts glued to the bottom").toBe(true);

  const term = getTerminal("stream")!;

  // Reproduce the ACCRUED DRIFT the finding observed: a SUB-ROW gap below the true
  // bottom (here ~12px, between wterm's own 5px "at bottom" threshold and one row). In
  // production this residue comes from wterm's `_scrollToBottom` flooring `scrollTop` to a
  // row multiple, so the prompt ends up a fraction of a row below the fold. At >5px wterm
  // DECOUPLES: its next `write` reads `_isScrolledToBottom()` false and stops following
  // (wterm.js:83,102,164), so the gap then GROWS batch after batch with no resize to
  // recollect it. It is a DRIFT, not a deliberate scroll-up, so the standing follow-bottom
  // intent stays ON (the gap is under one row, below the app's own intent threshold).
  el.scrollTop = el.scrollHeight - el.clientHeight - 12;
  await settle();

  // Simulate CHAINED commands: several separated output batches, each > a few lines, with
  // NO resize in between. Between batches we let wterm's deferred render (setTimeout(0) →
  // rAF) paint — exactly as streamed output arrives in bursts. With wterm decoupled, each
  // batch grows `scrollHeight` while `scrollTop` stays put, so the gap balloons far past
  // TOLERANCE. Only the output-side sticky-bottom hook recollects it — the whole fix.
  const BATCHES = 5;
  const LINES_PER_BATCH = 6;
  for (let b = 0; b < BATCHES; b++) {
    for (let i = 0; i < LINES_PER_BATCH; i++) {
      term.writeToScreen(`chained batch ${b} line ${i}\r\n`);
    }
    term.writeToScreen(`PROMPT$ `); // a fresh prompt after each "command", as a shell does
    // A short settle between batches so each render lands before the next batch — the
    // absence of any resize call here is the whole point.
    await new Promise((r) => requestAnimationFrame(() => r(null)));
    await new Promise((r) => setTimeout(r, 30));
  }
  await settle();

  // THE PROOF: after the LAST batch the view is glued to the bottom, with NO resize
  // having fired — the streamed output alone kept the prompt visible.
  const glued = await waitFor(() => atBottom(el, TOLERANCE));
  expect(
    glued,
    "streamed output (chained commands, no resize) must keep the view glued to the bottom",
  ).toBe(true);
  expect(
    atBottom(el, TOLERANCE),
    "scrollTop + clientHeight ≥ scrollHeight − tolerance after the final streamed batch",
  ).toBe(true);
});

it("DOES NOT JITTER on in-place echoes: typing a character onto the prompt row (no newline) and a cursor mutation NEVER re-scroll the view", async () => {
  // Finding 01KWVQ65SG8QCMKK88TGS94821 (jitter). The scroll-follow-on-output fix
  // over-corrected: the grid MutationObserver fired on EVERY mutation — an echoed
  // character AND the blinking cursor (which adds/removes a DOM node) — and the stick
  // wrote `scrollTop = scrollHeight` UNCONDITIONALLY whenever following. So each
  // keystroke (and the cursor blink at rest) re-scrolled to the bottom during wterm's
  // transient render → the view bounced → a permanent per-keystroke / at-rest jitter.
  //
  // The fix: a stick only re-glues on a REAL content growth (new lines → `scrollHeight`
  // grows since the last stick), never on an in-place echo / cursor (same row count,
  // unchanged `scrollHeight`), and is a NO-OP when already at the bottom.
  //
  // We reproduce the jitter DETERMINISTICALLY by parking the view a SUB-ROW gap below
  // the true bottom (like the streaming case — a real accrued drift, intent still
  // "follow") and then echoing IN PLACE. The buggy unconditional stick snaps such a
  // drifted view straight to the bottom on the very first echo (Δ = the gap, tens of
  // px); the fix leaves it put because no new line appeared. THIS is the assertion that
  // REDDENS without the fix.
  const host = await pane(PANE_W, 420);
  const el = await buildOverflowing("jitter", host);
  expect(atBottom(el, TOLERANCE), "the view starts glued to the bottom").toBe(true);

  const term = getTerminal("jitter")!;

  // Park a SUB-ROW gap (~12px) below the true bottom — the same drift the streaming case
  // uses: above this spec's JITTER dead-band (2px) but UNDER one row, so it is still
  // "near bottom" for the app's follow intent (which stays ON) — a genuine accrued drift,
  // not a deliberate scroll-up. This is where a real prompt sits after wterm floors
  // `scrollTop` to a row multiple. The buggy unconditional stick snaps such a view to the
  // bottom on the very first echo (Δ ≈ 12px); the fix leaves it put (no new line).
  el.scrollTop = el.scrollHeight - el.clientHeight - 12;
  await settle();
  const baseline = el.scrollTop;
  expect(baseline, "the view is parked a sub-row gap below the bottom").toBeGreaterThan(0);

  // Type characters onto the prompt row, one at a time, NO newline — pure in-place
  // echoes. Each mutates the grid (the MutationObserver fires; the write hook fires) but
  // adds NO row, so `scrollHeight` is unchanged: a correct follow must NOT touch
  // `scrollTop`. The buggy code snaps to the bottom on the first echo (Δ ≈ 18px).
  let maxDelta = 0;
  for (let i = 0; i < 12; i++) {
    term.writeToScreen("x");
    // Settle across wterm's deferred render (setTimeout(0) → rAF) AND the double-rAF
    // stick, so any stray stick has had every chance to fire and move the view.
    await settle();
    maxDelta = Math.max(maxDelta, Math.abs(el.scrollTop - baseline));
  }
  expect(
    maxDelta,
    `typing in place must NOT re-scroll the view (max Δ scrollTop = ${maxDelta}px, expected ≤ 1)`,
  ).toBeLessThanOrEqual(1);

  // A cursor-style in-place mutation: backspace + rewrite the same cell keeps the caret
  // on the same row (the closest deterministic stand-in for the blinking cursor's grid
  // mutation). Still NO scroll movement.
  const beforeCursor = el.scrollTop;
  for (let i = 0; i < 6; i++) {
    term.writeToScreen("\by");
    await settle();
  }
  expect(
    Math.abs(el.scrollTop - beforeCursor),
    "a cursor-style in-place mutation must NOT re-scroll the view",
  ).toBeLessThanOrEqual(1);

  // Typing at the TRUE bottom is stable too (the user's literal report): glue to the
  // bottom, then echo in place — the view must not bounce.
  el.scrollTop = el.scrollHeight;
  await settle();
  const atBottomBase = el.scrollTop;
  let atBottomMaxDelta = 0;
  for (let i = 0; i < 8; i++) {
    term.writeToScreen("z");
    await settle();
    atBottomMaxDelta = Math.max(atBottomMaxDelta, Math.abs(el.scrollTop - atBottomBase));
  }
  expect(
    atBottomMaxDelta,
    `typing while glued to the bottom must not bounce the view (max Δ = ${atBottomMaxDelta}px)`,
  ).toBeLessThanOrEqual(1);

  // And the terminal is STILL following: a real new line (with newline) re-glues to the
  // bottom — the anti-jitter guard did not break the genuine follow-on-output.
  term.writeToScreen("\r\nreal new line\r\nLIVE-PROMPT$ ");
  await settle();
  const followed = await waitFor(() => atBottom(el, TOLERANCE));
  expect(followed, "a genuine new line after in-place echoes must still follow the bottom").toBe(
    true,
  );
});

it("PRESERVES scroll-up across a nav round-trip: a user who scrolled into the scrollback is NOT forced to the bottom by the reattach", async () => {
  const host = await pane(PANE_W, 420);
  const el = await buildOverflowing("scrolled", host);

  // The user scrolls UP into the scrollback (well away from the bottom).
  el.scrollTop = 0;
  await settle();
  expect(atBottom(el, TOLERANCE), "the user is parked at the top, not the bottom").toBe(false);

  // NAV round-trip. Because the standing intent was "not following" when we detached,
  // the reattach must leave the position at the top — no forced jump to the bottom.
  detachTerminal("scrolled");
  await settle();
  const host2 = await pane(PANE_W, 560);
  attachTerminal("scrolled", host2);
  await settle();

  expect(
    atBottom(el, TOLERANCE),
    "a nav round-trip must NOT force a scrolled-up user down to the bottom",
  ).toBe(false);
});
