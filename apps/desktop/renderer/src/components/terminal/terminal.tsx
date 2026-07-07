import { useEffect, useRef } from "react";

import { cn } from "@/lib/utils";
import { buildDeadHistory } from "./dead-history";
import { attachTerminal, detachTerminal, ensureTerminal, getTerminal } from "./terminal-store";
import { FALLBACK_THEME, resolveCssColor } from "./wterm-theme";

export interface TerminalProps {
  /** Optional className for the full-bleed container; the parent gives it a size. */
  className?: string;
  /**
   * Working directory for the spawned shell. `undefined` lets the backend pick
   * its default (it inherits nyx's cwd).
   */
  cwd?: string;
  /**
   * Whether this pane is the active/visible one. Only the active pane restores
   * keyboard focus on (re)attach; an inactive (`display:none`) pane keeps its
   * instance alive but does not steal focus.
   */
  active?: boolean;
  /**
   * The SQLite RECORD id this terminal is bound to. This is the KEY into the
   * module-level store: the instance (and its ghostty screen state) lives there
   * across navigation; this component only ATTACHES the instance's persistent DOM
   * node into its container and DETACHES it on unmount — it NEVER destroys.
   */
  recordId: string;
  /**
   * Prior serialized scrollback restored as READ-ONLY dead history above a fresh
   * shell, written once when the instance is first created. Never sent to the PTY.
   */
  deadHistory?: string;
  /**
   * Called with the live PTY id once the shell spawns (and `null` on exit). The
   * sidebar's auto-naming reads `terminal_info` for this id.
   */
  onPtyId?: (id: number | null) => void;
}

/**
 * `<Terminal>` — a thin, LIFECYCLE-DECOUPLED pane for a store-held wterm instance.
 *
 * v2.2.5 removed the per-component xterm instance. The live terminal now lives in
 * the module-level {@link ensureTerminal store}; this component's ONLY job is to
 * project its persistent DOM node into a container:
 *  - on mount: `ensureTerminal` (create the instance once if absent) then
 *    `attachTerminal` (move its node into this container + arm the app-driven
 *    resize observer with the 0×0 guard);
 *  - on unmount (route change, close of this pane): `detachTerminal` — remove the
 *    node from the container and stop observing. NEVER `destroy()`; the instance
 *    and its full screen/alt-screen/cursor state stay in the store and reattach
 *    losslessly next time.
 *
 * Because the instance is decoupled from React's lifecycle, navigating away
 * (unmounting this component with the whole route) preserves the terminal — the
 * core of the no-host persistence model.
 */
export function Terminal({
  className,
  cwd,
  active = true,
  recordId,
  deadHistory,
  onPtyId,
}: TerminalProps) {
  const containerRef = useRef<HTMLDivElement | null>(null);

  // Keep the latest onPtyId on a ref so the attach effect stays keyed only on the
  // record id (the instance is created once; a changing callback must not respawn).
  const onPtyIdRef = useRef(onPtyId);
  onPtyIdRef.current = onPtyId;

  // Ensure the instance exists and attach its node into this container. Keyed on
  // `recordId` only: the instance is created once and reattached on every mount;
  // the cleanup DETACHES (never destroys), so a route unmount preserves it.
  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    // Build the read-only dead-history payload from the design-token separator
    // colour, so a restored terminal shows its prior session above a fresh shell.
    const sepColor = resolveCssColor("--muted-foreground") ?? FALLBACK_THEME.foreground;
    const deadHistoryPayload = deadHistory ? buildDeadHistory(deadHistory, sepColor) : undefined;

    ensureTerminal({
      recordId,
      cwd,
      deadHistory: deadHistoryPayload,
      onPtyId: (id) => onPtyIdRef.current?.(id),
    });
    attachTerminal(recordId, container);

    return () => {
      // DETACH only — the instance survives in the store for the next mount.
      detachTerminal(recordId);
    };
  }, [recordId, cwd, deadHistory]);

  // Restore keyboard focus when this pane becomes the active one (and after a
  // reattach). Deferred one frame so the node is laid out first (a just-attached
  // or just-shown pane has no box until the browser paints).
  useEffect(() => {
    if (!active) return;
    const frame =
      typeof requestAnimationFrame === "function"
        ? requestAnimationFrame
        : (cb: FrameRequestCallback) => setTimeout(() => cb(0), 0);
    const cancel =
      typeof cancelAnimationFrame === "function"
        ? cancelAnimationFrame
        : (id: number) => clearTimeout(id);
    const raf = frame(() => {
      try {
        getTerminal(recordId)?.focus();
      } catch {
        // focus is best-effort: a detached/closing instance must never throw.
      }
    }) as unknown as number;
    return () => cancel(raf);
  }, [active, recordId]);

  // OUTER container carries the padding + background (parity with the pre-wterm
  // xterm pane and the read-only `CommandOutputPanel`). `attachTerminal`
  // `appendChild`s the instance's persistent node (`el`, `width:100%`, in NORMAL
  // FLOW — `.wterm` is `position:relative` and `applyWtermTheme` zeroes its own
  // padding) INTO this box. `observeResize` watches THIS container (`el`'s parent),
  // and `reconcileSize`/`computeGrid` measure the container's CONTENT-BOX (its
  // border-box inset by `p-2.5`) for BOTH width and height, then re-lock `el`'s
  // height to the grid so the node fills the pane vertically. The container is
  // measured — not `el` — because wterm height-LOCKS `el` at init (with
  // `autoResize:false`), so `el`'s own height is frozen and cannot report the pane
  // growing taller. The grid is correctly RESIZED on the reduced box (not just
  // offset): no edge row/col is clipped, the 0×0 guard is untouched.
  return (
    <div
      ref={containerRef}
      data-terminal-container={recordId}
      className={cn("h-full w-full overflow-hidden bg-background p-2.5", className)}
    />
  );
}

export default Terminal;
