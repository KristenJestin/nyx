import { useEffect, useRef, useState } from "react";
import { WTerm } from "@wterm/dom";
import "@wterm/dom/css";

import { cn } from "@/lib/utils";
import { loadGhosttyCore } from "@/components/terminal/ghostty-core";
import {
  computeGrid,
  hasUsableBox,
  measureCell,
  type CellMetrics,
} from "@/components/terminal/measure-cell";
import { applyWtermTheme, ensureTerminalFontLoaded } from "@/components/terminal/wterm-theme";
import { useCommandOutput } from "./use-command-output";

export interface CommandOutputPanelProps {
  /**
   * The `command_instances.id` whose output to show. On change the parent re-keys
   * this component so a fresh wterm starts from a clean buffer and rehydrates that
   * instance's history.
   */
  instanceId: string | null;
  /** Optional className for the full-bleed container (the parent gives it a size). */
  className?: string;
  /** Surfaces the wterm instance to the parent / tests as it appears/disappears. */
  onInstance?: (instance: WTerm | null) => void;
}

/**
 * `<CommandOutputPanel>` — the READ-ONLY wterm surface that renders a managed
 * command instance's output (T8). Post-v2.2.5 it uses wterm + the ghostty core
 * (same lib as the interactive terminal — xterm is gone) but is wired to
 * `useCommandOutput` (rehydrate via `command_output`, stream `command://output`
 * filtered by `instanceId`) instead of a PTY.
 *
 * READ-ONLY STRICT (the T8 contract): the `WTerm` is built with NO `onData`
 * callback and `useCommandOutput` wires no input/resize→process backchannel, so
 * the panel never sends a byte to the process. Selection + scroll (DOM-local)
 * stay available for reading.
 *
 * Sizing is APP-DRIVEN with the 0×0 guard (`autoResize:false`), matching the
 * interactive terminal's guard-rails: wterm's own observer would resize a hidden
 * panel to 1×1 and destroy the grid, so we own sizing here.
 */
export function CommandOutputPanel({ instanceId, className, onInstance }: CommandOutputPanelProps) {
  const hostRef = useRef<HTMLDivElement | null>(null);
  const [term, setTerm] = useState<WTerm | null>(null);

  const onInstanceRef = useRef(onInstance);
  onInstanceRef.current = onInstance;

  // Build one read-only wterm (ghostty core) for this mount. Torn down on unmount
  // (a real close of the panel — not a navigation-persisted terminal), so
  // `destroy()` is correct here (this surface is per-selection, keyed by the parent).
  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;
    let disposed = false;
    let built: WTerm | null = null;
    let observer: ResizeObserver | null = null;
    let cell: CellMetrics | null = null;
    let cols = 80;
    let rows = 24;

    const reconcileSize = () => {
      if (!built) return;
      const rect = host.getBoundingClientRect();
      if (!hasUsableBox({ width: rect.width, height: rect.height })) return; // 0×0 guard
      const measured = measureCell(host) ?? cell;
      if (!measured) return;
      cell = measured;
      const grid = computeGrid(rect.width, rect.height, measured);
      if (grid.cols === cols && grid.rows === rows) return;
      cols = grid.cols;
      rows = grid.rows;
      built.resize(cols, rows);
    };

    void (async () => {
      const core = await loadGhosttyCore();
      if (disposed) return;
      applyWtermTheme(host);
      await ensureTerminalFontLoaded();
      if (disposed) return;
      const wterm = new WTerm(host, {
        core,
        cols,
        rows,
        // Read-only: no `onData`, so nothing the user types reaches the process.
        autoResize: false, // app-driven sizing with the 0×0 guard (see reconcileSize).
      });
      await wterm.init();
      if (disposed) {
        wterm.destroy();
        return;
      }
      built = wterm;
      cell = measureCell(host);
      setTerm(wterm);
      onInstanceRef.current?.(wterm);
      if (typeof ResizeObserver !== "undefined") {
        observer = new ResizeObserver(() => reconcileSize());
        observer.observe(host);
        reconcileSize();
      }
    })();

    return () => {
      disposed = true;
      observer?.disconnect();
      onInstanceRef.current?.(null);
      built?.destroy();
      setTerm(null);
    };
  }, []);

  // The READ-ONLY output wiring: rehydrate + stream, no input path (see the hook).
  useCommandOutput(term, instanceId);

  return (
    <div className={cn("h-full w-full overflow-hidden bg-background p-2.5", className)}>
      <div ref={hostRef} className="h-full w-full overflow-hidden" />
    </div>
  );
}
