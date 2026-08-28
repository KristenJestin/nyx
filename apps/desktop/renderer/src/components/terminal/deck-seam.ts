import { useEffect } from "react";

import { getTerminal } from "./terminal-store";

/**
 * THE per-terminal e2e/test SEAM — `window.__nyxDeck` / `window.__nyxDeckInput`, published from
 * ONE place.
 *
 *   - `__nyxDeck[id]()`       → that terminal's buffer text (screen + scrollback);
 *   - `__nyxDeckInput[id](s)` → feed `s` as input (routes through the PTY).
 *
 * Both are backed by the module-level {@link getTerminal store}, so they address ANY terminal by
 * record id — visible, hidden behind another pane, or navigated away — and both are inert in
 * production (nothing in the app reads them). They are a CONTRACT, and this is its exact reader
 * inventory — every e2e file naming `__nyxDeck` / `__nyxDeckInput`, i.e. `grep -rln "__nyxDeck" e2e/`
 * (keep the two in sync; a refactor of this module breaks precisely these):
 *   - `e2e/wdio.conf.cjs` and `e2e/README.md` — document the seam;
 *   - `e2e/specs/cockpit-helpers.cjs` — wraps it as `readDeckBuffer` / `typeIntoDeck`;
 *   - `e2e/specs/cockpit-v256-helpers.cjs` — same wrappers for the current shell;
 *   - `e2e/specs/cockpit-v256-terminals.e2e.cjs` — the only CURRENT-shell spec reading it directly;
 *   - `e2e/specs/restore-02-verify.e2e.cjs` — reads buffers after a relaunch.
 * INDIRECT readers (through the helpers above, so they never name the globals) include
 * `e2e/specs/cockpit-01-seed.e2e.cjs`. `e2e/specs/idle-replay.e2e.cjs` is NOT a reader: it drives
 * the older `window.__nyx` seam, retired with `<TerminalManager>` (PDR-0025, e2e/README.md).
 *
 * WHY A REFCOUNT. The seam has TWO hosts — `<TerminalDeck>` (the sidebar's mono-visible deck) and
 * `<PaneTreeHost>` (the Terminals page's tiling) — and they can be mounted at the same time. With
 * a copy of the publication in each, the FIRST unmount deleted the globals the OTHER host was
 * still publishing, and an e2e step reading the seam right after found `undefined`. So the global
 * is owned HERE: it is installed on the first claim and removed only when the last holder
 * releases it. The published objects are stateless proxies over the store, so which host claimed
 * first is irrelevant — every holder sees the same live seam.
 */

/** The window fields this seam owns. */
interface DeckSeamWindow {
  __nyxDeck?: Record<string, () => string>;
  __nyxDeckInput?: Record<string, (data: string) => void>;
}

/** How many live holders claim the seam right now (0 ⇒ the globals are absent). */
let holders = 0;

/**
 * Claim the seam, publishing it if nobody had. Returns the RELEASE function: the globals are
 * deleted only when the last holder releases (calling it twice is a no-op).
 */
export function claimDeckSeam(): () => void {
  if (typeof window === "undefined") return () => {};
  const win = window as unknown as DeckSeamWindow;
  holders += 1;
  if (holders === 1) {
    win.__nyxDeck = new Proxy(
      {},
      { get: (_t, id: string) => () => getTerminal(id)?.readScreenText() ?? "" },
    ) as Record<string, () => string>;
    win.__nyxDeckInput = new Proxy(
      {},
      { get: (_t, id: string) => (data: string) => getTerminal(id)?.sendInput(data) },
    ) as Record<string, (data: string) => void>;
  }
  let released = false;
  return () => {
    if (released) return;
    released = true;
    holders = Math.max(0, holders - 1);
    if (holders === 0) {
      delete win.__nyxDeck;
      delete win.__nyxDeckInput;
    }
  };
}

/** Publish the seam for as long as the calling component is mounted. */
export function useDeckSeam(): void {
  useEffect(() => claimDeckSeam(), []);
}

/** Live holder count — a test seam for the refcount itself. */
export function __deckSeamHolders(): number {
  return holders;
}
