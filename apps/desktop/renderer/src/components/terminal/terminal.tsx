import { useCallback, useEffect, useId, useRef, useState } from "react";
import { ArrowDownIcon } from "lucide-react";
import type { ITerminalOptions, Terminal as XTerm } from "@xterm/xterm";

import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { buildDeadHistory } from "./dead-history";
import { copySelection, pasteFromClipboard } from "./terminal-clipboard";
import { TerminalContextMenu } from "./terminal-context-menu";
import { useAtBottom } from "./use-at-bottom";
import { useScrollbackPersist } from "./scrollback-persist";
import { attachTerminal, detachTerminal, ensureTerminal, getTerminal } from "./terminal-store";
import { FALLBACK_THEME, resolveCssColor } from "./xterm-theme";

export interface TerminalProps {
  /**
   * Optional className for the full-bleed container. The container fills its
   * parent; the parent is responsible for giving it a size.
   */
  className?: string;
  /** Override / extend the default xterm options (applied on first create only). */
  options?: ITerminalOptions;
  /**
   * Working directory for the spawned shell. `undefined` lets the backend pick
   * its default (it inherits nyx's cwd, i.e. home/current).
   */
  cwd?: string;
  /**
   * Called with the store-held xterm instance once it is attached (and with
   * `null` on unmount). Useful for parents that need to drive the terminal
   * directly; also the seam used by unit tests to assert on the xterm buffer.
   */
  onInstance?: (instance: XTerm | null) => void;
  /**
   * Whether this pane is the active/visible one. Only the active pane restores
   * keyboard focus on (re)attach; an inactive (`display:none`) pane keeps its
   * instance alive but does not steal focus. Defaults to `true`.
   */
  active?: boolean;
  /**
   * The SQLite RECORD id this terminal is bound to — the KEY into the module-level
   * store: the live instance (xterm + PTY + buffer) lives there ACROSS
   * NAVIGATION; this component only ATTACHES its persistent DOM node into a
   * container and DETACHES it on unmount — it NEVER disposes. Omit for a
   * record-less standalone terminal (the socle / unit harness): a per-mount id is
   * synthesized so it still gets a live instance (keep-alive across navigation is
   * moot without a durable record).
   */
  recordId?: string;
  /**
   * Prior serialized scrollback restored as READ-ONLY dead history above a fresh
   * shell, written once when the instance is first created. NEVER sent to the PTY.
   */
  deadHistory?: string;
  /**
   * Called with the live PTY id once the shell spawns (and `null` on exit). The
   * sidebar's auto-naming reads `terminal_info` for this id.
   */
  onPtyId?: (id: number | null) => void;
}

/**
 * `<Terminal>` — a thin, LIFECYCLE-DECOUPLED pane for a store-held xterm instance.
 *
 * The live terminal (xterm instance + FitAddon + PTY wiring + buffer) lives in the
 * module-level {@link ensureTerminal store}, NOT in this component. This
 * component's only job is to project the instance's persistent DOM node into a
 * container:
 *  - on mount: `ensureTerminal` (create the instance once if absent — it spawns
 *    the PTY and wires its `pty://output` subscription at module level) then
 *    `attachTerminal` (move its node into this container + arm the geometry
 *    pipeline);
 *  - on unmount (route change / close of this pane): `detachTerminal` — remove the
 *    node and stop observing. NEVER dispose; the instance and its full buffer stay
 *    in the store and reattach losslessly next time.
 *
 * Because the instance is decoupled from React's lifecycle, navigating away
 * (unmounting this component with the whole route) PRESERVES the terminal — a
 * running command keeps streaming into the buffer and is still running on return.
 * The instance uses xterm's default DOM renderer (NO WebGL), which survives a
 * detach/reattach and redraws from the buffer.
 */
export function Terminal({
  className,
  options,
  cwd,
  onInstance,
  active = true,
  recordId,
  deadHistory,
  onPtyId,
}: TerminalProps) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  // A stable per-mount id for a record-less standalone terminal, so it still keys
  // into the store (and stays the same across a StrictMode remount → one spawn).
  const synthId = useId();
  const id = recordId ?? synthId;

  // The store-held xterm, surfaced for the React-side affordances below and for
  // `onInstance`. Set once the instance is ensured/attached.
  const [term, setTerm] = useState<XTerm | null>(null);

  // Keep the latest callbacks on refs so the attach effect stays keyed only on
  // the record id (the instance is created once; a changing callback must not
  // respawn it).
  const onPtyIdRef = useRef(onPtyId);
  onPtyIdRef.current = onPtyId;
  const onInstanceRef = useRef(onInstance);
  onInstanceRef.current = onInstance;

  // Ensure the instance exists and attach its node into this container. Keyed on
  // `id`/`cwd`/`deadHistory`: the instance is created once and reattached on every
  // mount; the cleanup DETACHES (never disposes), so a route unmount preserves it.
  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    // Build the read-only dead-history payload from the design-token separator
    // colour, so a restored terminal shows its prior session above a fresh shell.
    const sepColor = resolveCssColor("--muted-foreground") ?? FALLBACK_THEME.foreground;
    const deadHistoryPayload = deadHistory ? buildDeadHistory(deadHistory, sepColor) : undefined;

    const instance = ensureTerminal({
      recordId: id,
      cwd,
      options,
      deadHistory: deadHistoryPayload,
      onPtyId: (ptyId) => onPtyIdRef.current?.(ptyId),
    });
    attachTerminal(id, container);
    setTerm(instance.terminal);
    onInstanceRef.current?.(instance.terminal);

    return () => {
      // DETACH only — the instance survives in the store for the next mount.
      detachTerminal(id);
      onInstanceRef.current?.(null);
      setTerm(null);
    };
    // `options` is intentionally excluded: it is consumed once at create time and
    // changing it must not respawn the terminal.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [id, cwd, deadHistory]);

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
        : (handle: number) => clearTimeout(handle);
    const raf = frame(() => getTerminal(id)?.focus()) as unknown as number;
    return () => cancel(raf);
  }, [active, id]);

  // Serialize + DEBOUNCE this terminal's scrollback to the backend so it can be
  // restored after a close/restart (no-op when record-less). Debounced (never per
  // byte); also flushes on tab/app close.
  useScrollbackPersist(term, recordId ?? null);

  // Floating "jump to bottom" affordance: track whether the viewport is pinned to
  // the live bottom and expose a smooth scroll-to-bottom. Shown only while
  // scrolled up.
  const { atBottom, scrollToBottom } = useAtBottom(term);

  // RIGHT-CLICK copy/paste — the same clipboard helpers as the keyboard chords
  // (which live in the instance), surfaced through `<TerminalContextMenu>`. Stable
  // callbacks so the menu's props don't churn; each is a no-op while `term` is null.
  const handleHasSelection = useCallback(() => (term ? term.hasSelection() : false), [term]);
  const handleCopy = useCallback(() => {
    if (term) void copySelection(term);
  }, [term]);
  const handlePaste = useCallback(() => {
    if (term) void pasteFromClipboard(term);
  }, [term]);

  // The container carries the padding + background; `attachTerminal` `appendChild`s
  // the instance's persistent node (`el`, 100%×100%) INTO it, so the terminal fills
  // the padded content box. The FitAddon measures that node — no edge row/col is
  // clipped.
  return (
    <TerminalContextMenu
      hasSelection={handleHasSelection}
      onCopy={handleCopy}
      onPaste={handlePaste}
    >
      <div
        ref={containerRef}
        data-terminal-container={id}
        className={cn("relative h-full w-full overflow-hidden bg-background p-2.5", className)}
      >
        {/* Jump-to-bottom: hidden (and click-through) while pinned to the bottom, so
            it never blocks the terminal when there is nothing to scroll back to. */}
        <Button
          size="icon-sm"
          variant="secondary"
          aria-label="Scroll to bottom"
          aria-hidden={atBottom}
          tabIndex={atBottom ? -1 : 0}
          onClick={scrollToBottom}
          className={cn(
            "absolute right-3 bottom-3 z-10 rounded-full shadow-md transition-opacity duration-150",
            atBottom ? "pointer-events-none opacity-0" : "opacity-100",
          )}
        >
          <ArrowDownIcon />
        </Button>
      </div>
    </TerminalContextMenu>
  );
}

export default Terminal;
