/**
 * APP-DRIVEN sizing math for wterm. We run wterm with `autoResize:false` (a
 * MANDATORY guard-rail: wterm's own ResizeObserver resizes to 1×1 the moment a
 * pane is hidden or reattached at 0×0, which DESTROYS the grid). Instead the app
 * owns sizing via its OWN ResizeObserver, and this pure module converts a pixel
 * box into terminal cols/rows so it stays unit-testable without real layout.
 */

/** A measured monospace cell: the advance width of one glyph + one row's height. */
export interface CellMetrics {
  charWidth: number;
  rowHeight: number;
}

/**
 * Compute the cols/rows that fit a `width × height` pixel box given the measured
 * cell, clamped to at least 1×1. Mirrors wterm's own internal formula
 * (`floor(width/charWidth)`, `floor(height/rowHeight)`), so an app-driven resize
 * lands on the same grid wterm would have chosen — just gated by us on a non-zero
 * box (see `hasUsableBox`). Pure + total.
 */
export function computeGrid(
  width: number,
  height: number,
  cell: CellMetrics,
): { cols: number; rows: number } {
  if (cell.charWidth <= 0 || cell.rowHeight <= 0) return { cols: 1, rows: 1 };
  const cols = Math.max(1, Math.floor(width / cell.charWidth));
  const rows = Math.max(1, Math.floor(height / cell.rowHeight));
  return { cols, rows };
}

/**
 * The 0×0 GUARD, extracted + pure. A box is usable only when BOTH dimensions are
 * strictly positive. Every app-driven resize/reconcile path gates on this so a
 * hidden (`display:none`) or freshly-reattached (pre-layout) pane never triggers
 * a destructive 1×1 resize. This is the guard the PRD makes mandatory.
 */
export function hasUsableBox(
  box: { width: number; height: number } | null | undefined,
): box is { width: number; height: number } {
  return !!box && box.width > 0 && box.height > 0;
}

/**
 * Measure a monospace cell inside a `.wterm` host by appending a hidden probe
 * `.term-row > span` (the exact markup wterm renders, so the measured advance
 * matches the real grid) and reading its box. Returns `null` when the probe has
 * no box yet (no layout / detached / font not loaded) so callers keep the current
 * size rather than dividing by zero.
 */
export function measureCell(host: HTMLElement): CellMetrics | null {
  if (typeof document === "undefined") return null;
  const row = document.createElement("div");
  row.className = "term-row";
  row.style.visibility = "hidden";
  row.style.position = "absolute";
  const span = document.createElement("span");
  span.textContent = "W";
  row.appendChild(span);
  host.appendChild(row);
  const charWidth = span.getBoundingClientRect().width;
  const rowHeight = row.getBoundingClientRect().height;
  row.remove();
  if (charWidth <= 0 || rowHeight <= 0) return null;
  return { charWidth, rowHeight };
}
