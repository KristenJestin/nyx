// wterm's own layout CSS so the live DOM carries the real `.wterm`/`.term-row` rules
// and the renderer paints per-attribute `<span>`s exactly as in production.
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
 * RESIDUAL-CELL REGRESSION GATE — Bug A, review 01KWY0KXSF0X8E6JDH2QVXTRRQ, task
 * 01KWY0KYRV21YPSHVQHVVM0214. Runs with the REAL ghostty WASM core (Playwright), the
 * only place the residual actually reproduces.
 *
 * MECHANISM (confirmed by instrumentation before the fix):
 *   - `ESC[2J` / `ESC[K` / scroll BLANK a cell's codepoint (raw codepoint → 0) but
 *     ghostty can leave the pen's per-cell SGR attributes on it (`flags` e.g.
 *     underline=8 stays set).
 *   - The affected row IS re-marked dirty and IS re-rendered (`isDirtyRow` was true
 *     immediately after `ESC[2J`), so this is NOT a stale-innerHTML problem — the
 *     renderer faithfully paints whatever `getCell` returns.
 *   - Unpatched `@wterm/ghostty` `getCell` (`ghostty-core.js:107`) only blanked a cell
 *     when `codepoint===0 && flags===0 && colorFlags===0`; an erased cell with residual
 *     flags fell through to `char: 32` + `flags: <residual>` → a STYLED SPACE → the DOM
 *     rendered a stray underline/color on an empty line ("drwxr-xr-x_", "SETUP.md_").
 *   - A background-color-erased region is DISTINCT: ghostty stores those as a real space
 *     (raw codepoint===32), so the fix (blank on codepoint===0) never touches them.
 *
 * FIX (Piste 2, minimal): patch `getCell`/`getScrollbackCell` to return `BLANK_CELL`
 * whenever `codepoint===0`, ignoring residual flags. No renderer change → no per-frame
 * full rebuild → no jitter regression.
 *
 * WITHOUT the patch this test REDDENS: the erased row's rendered spans still carry
 * `text-decoration`/`color`, and the core still reports styled blank cells.
 */

const SPAWNED_ID = 11;

beforeEach(() => {
  mockIPC((cmd) => (cmd === "pty_spawn" ? SPAWNED_ID : null), { shouldMockEvents: true });
});
afterEach(() => {
  __resetTerminalStore();
  document.body.replaceChildren();
});

function pane(): HTMLElement {
  const el = document.createElement("div");
  el.style.cssText = "width:640px;height:360px;position:fixed;top:0;left:0;overflow:hidden;";
  document.body.appendChild(el);
  return el;
}
async function waitForRender(host: HTMLElement): Promise<void> {
  const deadline = Date.now() + 8000;
  while (Date.now() < deadline) {
    if (host.querySelector(".wterm .term-grid .term-row")) return;
    await new Promise((r) => setTimeout(r, 50));
  }
}
const settle = async (n = 8) => {
  for (let i = 0; i < n; i++) {
    await new Promise((r) => requestAnimationFrame(() => r(null)));
    await new Promise((r) => setTimeout(r, 30));
  }
};

/** LIVE screen rows (excludes scrollback history rows). */
function liveRows(host: HTMLElement): HTMLElement[] {
  return Array.from(
    host.querySelectorAll<HTMLElement>(".wterm .term-grid .term-row:not(.term-scrollback-row)"),
  );
}
/** True if ANY live row carries a text-decoration or foreground `color:` in its markup. */
function anyRowHasTextStyle(host: HTMLElement): boolean {
  return liveRows(host).some((r) => {
    const html = r.innerHTML || "";
    return html.includes("text-decoration") || html.includes("color:");
  });
}

interface CoreProbe {
  getCols(): number;
  getCell(r: number, c: number): { char: number; flags: number };
}
function coreOf(inst: unknown): CoreProbe {
  return (inst as { term: { bridge: CoreProbe } }).term.bridge;
}
/** How many cells of the given grid row carry any residual SGR flags, per the core. */
function styledCellCount(core: CoreProbe, row: number): number {
  let n = 0;
  for (let c = 0; c < core.getCols(); c++) if (core.getCell(row, c).flags !== 0) n++;
  return n;
}

async function mount(recordId: string): Promise<{ host: HTMLElement; inst: unknown }> {
  const host = pane();
  ensureTerminal({ recordId });
  attachTerminal(recordId, host);
  await waitForRender(host);
  await document.fonts.ready;
  await settle();
  return { host, inst: getTerminal(recordId)! };
}

it("no residual text style on cleared cells after ESC[2J (DOM matches the core, both blank)", async () => {
  const { host, inst } = await mount("resid-clear");
  const core = coreOf(inst);
  const w = inst as { writeToScreen(d: string): void };

  // Paint underlined + red text on row 0 (the diag's repro: a styled line).
  w.writeToScreen("\x1b[4;31mUNDERLINED-RED\x1b[0m\r\n");
  await settle();
  // Pre-condition: the style really landed (guards against a no-op test).
  expect(styledCellCount(core, 0), "row 0 is styled before the clear").toBeGreaterThan(0);
  expect(anyRowHasTextStyle(host), "the DOM shows the style before the clear").toBe(true);

  // Clear the whole screen. The row is re-rendered (dirty), so a correct core makes
  // the rebuilt row blank; a buggy core keeps painting a styled space.
  w.writeToScreen("\x1b[2J\x1b[H");
  await settle();

  // CORE truth: no cell on row 0 carries residual flags anymore.
  expect(styledCellCount(core, 0), "the core reports NO styled cells on the cleared row").toBe(0);
  // DOM truth: no live row carries a text-decoration / color on an empty cell.
  expect(anyRowHasTextStyle(host), "no residual text style survives the clear in the DOM").toBe(
    false,
  );
});

it("no residual underline after erase-to-EOL (ESC[K) on a styled line", async () => {
  const { host, inst } = await mount("resid-el");
  const core = coreOf(inst);
  const w = inst as { writeToScreen(d: string): void };

  // Line 5: underlined green text, then erase the whole line from col 1.
  w.writeToScreen("\x1b[5;1H\x1b[4;32mSETUP.md\x1b[0m");
  await settle();
  expect(styledCellCount(core, 4), "line 5 is styled before the erase").toBeGreaterThan(0);

  w.writeToScreen("\x1b[5;1H\x1b[K");
  await settle();

  expect(styledCellCount(core, 4), "the erased line carries no residual flags in the core").toBe(0);
  const row4 = liveRows(host)[4];
  expect(
    row4?.innerHTML.includes("text-decoration"),
    "no residual underline in the erased row",
  ).toBe(false);
});
