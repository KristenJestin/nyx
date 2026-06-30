import type { FitAddon } from "@xterm/addon-fit";
import type { Terminal as XTerm } from "@xterm/xterm";

/**
 * The dependencies the geometry-reconcile pipeline needs. Kept as a plain object
 * so the pipeline is a PURE function of its inputs — exercisable in jsdom with
 * fakes (which has no real layout/WebGL), independently of `<Terminal>`'s effects.
 */
export interface ReconcileGeometryDeps {
  /** The xterm instance to repaint (null while it is not yet created → no-op). */
  instance: XTerm | null;
  /** The element xterm opened into; its box drives the size + the 0×0 gate. */
  element: HTMLElement | null;
  /** The fit addon that maps the element's pixel box to xterm cols/rows. */
  fitAddon: FitAddon;
  /** Pushes the terminal's CURRENT cols/rows to the PTY (→ `pty_resize` → SIGWINCH). */
  resyncSize: () => void;
  /** Rebuilds the WebGL glyph atlas (no-op when WebGL is not attached). */
  clearWebglAtlas: () => void;
}

/**
 * GEOMETRY RECONCILIATION — the single, ordered pipeline that makes a terminal
 * pane's RENDER match its real pixel size. It is the fix for the whole
 * "dimensions / garbled render" family (FEEDBACK #20 + #23).
 *
 * The order is LOAD-BEARING:
 *
 *   1. `fit()`              — measure the (now visible, laid-out) element and set
 *                             xterm's cols/rows to it.
 *   2. `resyncSize()`       — push those REAL cols/rows to the PTY out-of-band, so
 *                             the kernel resizes the pty and delivers **SIGWINCH**
 *                             to the child. This is what forces a resumed `claude`
 *                             (or any TUI) to REDRAW at the correct size — without
 *                             it the TUI stays pinned to its spawn-time geometry
 *                             (#23: "calcule mal la taille", invisible input, no
 *                             scroll). `pty_resize` is idempotent on the backend.
 *   3. `clearWebglAtlas()`  — drop the glyph atlas the WebGL renderer baked while
 *                             the pane was hidden / at stale metrics, so it is
 *                             rebuilt against the current cell size (#20: the
 *                             "bouillie" of glyphs is a stale atlas/canvas).
 *   4. `refresh(0, rows-1)` — force xterm to repaint every row NOW, so the freshly
 *                             rebuilt atlas is drawn instead of the corrupted frame.
 *   5. `scrollToBottom()`   — keep the live prompt in view (restore-from-history).
 *
 * GATED on a NON-ZERO element box: while the pane is `display:none` (an inactive
 * deck terminal, or the whole deck hidden behind a command view) the element is
 * 0×0 — fitting / atlas-building there bakes a bogus geometry, which is exactly
 * what produces the garbled render. We skip and let the caller rerun once visible.
 * Returns whether the pipeline actually ran (`false` = skipped, 0×0 / detached).
 *
 * Every step after the gate is wrapped so a detached/closing instance can never
 * throw out of a render or rAF callback.
 */
export function reconcileTerminalGeometry(deps: ReconcileGeometryDeps): boolean {
  const { instance, element, fitAddon, resyncSize, clearWebglAtlas } = deps;
  if (!instance || !element) return false;
  // The 0×0 gate: a hidden pane has no box. Reconciling there is the bug, not the fix.
  if (element.clientWidth === 0 || element.clientHeight === 0) return false;

  try {
    fitAddon.fit();
  } catch {
    // ignore transient fit failures (no layout / detached)
  }
  // PTY resize → SIGWINCH (the TUI redraw trigger). Independent of xterm's onResize
  // event, which is only wired after pty_spawn resolves and only fires when cols/rows
  // actually change — neither holds when a pane merely reappears at the same size,
  // so we always push the current size here.
  resyncSize();
  // Rebuild the glyph atlas against the now-current metrics, then repaint every row
  // so the corrupted/stale frame is replaced by a clean one.
  clearWebglAtlas();
  try {
    instance.refresh(0, Math.max(0, instance.rows - 1));
  } catch {
    // refresh is best-effort: a detached/closing instance must never throw.
  }
  try {
    instance.scrollToBottom();
  } catch {
    // ignore (detached / closing instance)
  }
  return true;
}
