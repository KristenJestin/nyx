import {
  type PointerEvent as ReactPointerEvent,
  type ReactNode,
  type RefObject,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { Columns2Icon, GripVerticalIcon, MoreVerticalIcon, Rows2Icon, XIcon } from "lucide-react";
import { formatForDisplay } from "@tanstack/react-hotkeys";

import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Menu, MenuItem } from "@/components/ui/menu";
import { useDeckSeam } from "@/components/terminal/deck-seam";
import { Terminal } from "@/components/terminal/terminal";
import type { TerminalRecord } from "@/components/sidebar/use-terminals";
import { SPLIT_DOWN_CHORD, SPLIT_RIGHT_CHORD } from "@/components/sidebar/use-terminal-shortcuts";
import {
  paneControlsMode,
  paneControlsWidthPx,
  paneDropIndicatorStyle,
  paneRectPx,
  paneRectsByTerminal,
  paneRectStyle,
  paneSeparators,
  ratioFromKey,
  ratioFromPointer,
  separatorRatioBounds,
  separatorStyle,
  type HostSize,
  type PanePlacement,
  type PaneSeparator,
  type SplitResizePhase,
} from "@/components/panes/pane-geometry";
import {
  movePaneToEdge,
  paneCount,
  paneSwapOf,
  type PaneEdge,
  type PaneNode,
  type SplitDirection,
} from "@/components/panes/pane-tree";

/**
 * The split chords AS THE USER READS THEM (`Ctrl+Shift+→` / `⌘ ⇧ →`), derived once from the very
 * constants `useTerminalShortcuts` registers — the tooltip and the binding cannot drift.
 */
const SPLIT_RIGHT_LABEL = formatForDisplay(SPLIT_RIGHT_CHORD);
const SPLIT_DOWN_LABEL = formatForDisplay(SPLIT_DOWN_CHORD);

/**
 * The outer fraction of each side of a target pane that reads as "drop on THIS edge" — the middle
 * is not a drop zone. At module scope like every other tuning constant of this host
 * (`SEPARATOR_THICKNESS_PX`, `OVERLAY_SURFACE_Z`, `CONTROL_*` next door), so the memoized
 * `resolveDropAt` closes over a value that cannot be re-created by a render.
 */
const PANE_EDGE_BAND = 0.25;

export interface PaneTreeHostProps {
  /** EVERY terminal of the entered project — each gets exactly one mounted `<Terminal>`. */
  terminals: readonly TerminalRecord[];
  /** The tiling: which terminals are VISIBLE and where. `null` shows nothing (no terminal yet). */
  layout: PaneNode | null;
  /** The ONE pane that owns the keyboard + the sidebar selection (never "all visible panes"). */
  focusedPaneId: string | null;
  /** A pane took the focus (click / focus-in). */
  onFocusPane: (paneId: string) => void;
  /** Split `paneId` right (`row`) or down (`column`) — creates the new pane's terminal. */
  onSplitPane: (paneId: string, direction: SplitDirection) => void;
  /** Close `paneId` — a LAYOUT edit: the terminal keeps running (see `closePane`). */
  onClosePane: (paneId: string) => void;
  /**
   * A separator moved `splitId` to `ratio` (already clamped). OMITTED ⇒ the tiling is READ-ONLY
   * and NO separator is rendered at all — an inert focusable splitter would be a lie to a
   * keyboard/screen-reader user, so the affordance exists exactly when it works.
   */
  onResizeSplit?: (splitId: string, ratio: number, phase: SplitResizePhase) => void;
  /**
   * RESTRUCTURE the tiling: the pane dragged by its grip was dropped on `edge` of `targetPaneId`
   * (the VS Code motif). A pure LAYOUT edit — no terminal is created; the leaf changes place and
   * keeps its xterm/PTY instance. OMITTED ⇒ no grip is rendered and the whole gesture is inert
   * (this is how the COMMAND groups' surfaces opt out: their disposition belongs to the settings).
   */
  onMovePane?: (draggedPaneId: string, targetPaneId: string, edge: PaneEdge) => void;
  /** Whether that drop clears the 180×120px floor — asked DURING the drag, before the drop. */
  canMovePane?: (draggedPaneId: string, targetPaneId: string, edge: PaneEdge) => boolean;
  /**
   * Extra content rendered INSIDE **each VISIBLE pane's** box — the Spec surcouche anchor.
   *
   * It used to be called for the FOCUSED pane only (`renderFocusedOverlay`), which meant a terminal
   * carrying a Spec showed NOTHING as soon as the keyboard went elsewhere: three panes running three
   * agents showed one languette (review finding 01M11WF11C0H8G2MG515FBT6RD). The surcouche belongs
   * to the TERMINAL, not to the focus, so every visible pane gets the call and a pane whose terminal
   * has no session renders nothing — the renderer's own answer, unchanged.
   *
   * The COST invariant it must not break lives in the renderer, not here: `<TerminalSpecsProvider>`
   * holds ONE resolver (one `specs://changed`, one `agent-sessions://changed`) for the whole tiling,
   * so N panes cost the subscriptions ONE used to. OMITTED ⇒ no overlay at all (how a selected
   * SERVICE takes the surcouche away, and how the COMMAND groups' surface opts out).
   */
  renderPaneOverlay?: (terminalId: string) => ReactNode;
}

/**
 * `<PaneTreeHost>` — the TILING host that replaces the mono-visible deck on the Terminals page.
 *
 * It mounts ONE `<Terminal>` per project record (exactly like the deck did, so a terminal that is
 * not on screen keeps its instance, its PTY and its buffer — the store owns them, this component
 * only projects their DOM node) and positions the VISIBLE ones — the tree's leaves — as
 * absolutely-placed slots computed by {@link paneRectsByTerminal}. A terminal that is not in the
 * tree stays mounted but `display:none` (its 0×0 box is the geometry pipeline's existing guard).
 *
 * VISIBLE vs FOCUSED — the distinction the single-pane deck could not express and this host makes
 * explicit:
 *  - `data-visible="true"` is EVERY leaf: mounted, laid out, live, resized, streaming — and every
 *    one of them carries its terminal's Spec surcouche, which belongs to the TERMINAL and not to
 *    the focus (review finding 01M11WF11C0H8G2MG515FBT6RD);
 *  - `data-focused="true"` is the ONE pane that takes the keyboard (`<Terminal active>`) and drives
 *    the sidebar selection.
 *
 * The store is untouched: `attachTerminal` already accepts an arbitrary container and each
 * instance's ResizeObserver watches its own `el.parentElement`, so N live panes need no change
 * below this component.
 */
export function PaneTreeHost({
  terminals,
  layout,
  focusedPaneId,
  onFocusPane,
  onSplitPane,
  onClosePane,
  onResizeSplit,
  onMovePane,
  canMovePane,
  renderPaneOverlay,
}: PaneTreeHostProps) {
  // The per-terminal test/e2e seams (`__nyxDeck` / `__nyxDeckInput`) — the SAME handles the deck
  // publishes, from the SAME shared owner, so the existing e2e specs and the phase-2 gate keep
  // reading one seam whichever surface (or both) is mounted. Inert in production.
  useDeckSeam();

  // Where each VISIBLE terminal sits (absent ⇒ that terminal is not in the tiling right now).
  const placements = useMemo(() => paneRectsByTerminal(layout), [layout]);
  const panes = layout == null ? 0 : paneCount(layout);

  // Render the slots in a STABLE order (by record id), independent of the sidebar order and of the
  // tree: the position comes from the rect, so the DOM order only decides paint order — pinning it
  // keeps every pane's DOM node in its slot across a split/close (no remount, no reattach).
  const ordered = useMemo(
    () => [...terminals].sort((a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0)),
    [terminals],
  );

  // The separators — one per split, positioned by the SAME recursion as the panes. They are
  // SIBLINGS of the pane slots (never wrappers), so showing, moving or dropping one leaves every
  // `<Terminal>` exactly where it is: the zero-remount property survives the resize affordance.
  const separators = useMemo(
    () => (onResizeSplit ? paneSeparators(layout) : []),
    [layout, onResizeSplit],
  );
  // The host box is the DRAG's measuring stick (a pointer position → a ratio of the split's own
  // sub-box). Measured on demand, never stored: no second geometry pipeline, no cached size to go
  // stale — the panes' sizes still come from the browser and the PTYs' from their ResizeObserver.
  const hostRef = useRef<HTMLDivElement | null>(null);
  const [draggedSplitId, setDraggedSplitId] = useState<string | null>(null);
  // The host's px size, for the ONE thing a pointer measurement cannot serve: the separators'
  // `aria-valuemin` / `aria-valuemax`, which must be in the DOM at RENDER time (see
  // {@link useHostSize}). The drag itself keeps measuring on demand.
  const hostSize = useHostSize(hostRef);

  /* ───────────── PANE RESTRUCTURE DRAG — the view's VS Code motif ─────────────
   *
   * A grip press takes POINTER CAPTURE (the separators' own trick), so the whole gesture — move,
   * up, cancel — is delivered to the host even when the pointer flies over a terminal or a
   * separator. On every move the drop is RESOLVED GEOMETRICALLY: which visible pane (never the
   * dragged one) holds the pointer, and which of its four EDGE BANDS (the outer quarter of each
   * side; the middle is not a drop zone). The floor is then asked through `canMovePane` BEFORE any
   * indicator is shown, so an invalid hover shows NOTHING — the refusal is visible during the
   * gesture, not discovered at the release. The indicator itself is a sibling overlay positioned
   * by {@link paneDropIndicatorStyle} over HALF the target's box: where the moved pane will land.
   */
  const canRestructure = onMovePane != null && canMovePane != null;
  /** The pane currently dragged by its grip (`null` ⇒ idle). */
  const [draggedPaneId, setDraggedPaneId] = useState<string | null>(null);
  /** The active drop zone: target pane + edge, only while it clears the floor. */
  const [dropTarget, setDropTarget] = useState<{ paneId: string; edge: PaneEdge } | null>(null);

  const resolveDropAt = useCallback(
    (
      clientX: number,
      clientY: number,
      draggedId: string,
    ): { paneId: string; edge: PaneEdge } | null => {
      const host = hostRef.current;
      if (host == null) return null;
      const box = host.getBoundingClientRect();
      if (!(box.width > 0) || !(box.height > 0)) return null;
      for (const placement of placements.values()) {
        if (placement.paneId === draggedId) continue;
        const left = box.left + (box.width * placement.rect.left) / 100;
        const top = box.top + (box.height * placement.rect.top) / 100;
        const width = (box.width * placement.rect.width) / 100;
        const height = (box.height * placement.rect.height) / 100;
        if (clientX < left || clientX >= left + width || clientY < top || clientY >= top + height) {
          continue;
        }
        const fx = (clientX - left) / width;
        const fy = (clientY - top) / height;
        let edge: PaneEdge | null = null;
        if (fx < PANE_EDGE_BAND) edge = "left";
        else if (fx > 1 - PANE_EDGE_BAND) edge = "right";
        else if (fy < PANE_EDGE_BAND) edge = "up";
        else if (fy > 1 - PANE_EDGE_BAND) edge = "down";
        return edge == null ? null : { paneId: placement.paneId, edge };
      }
      return null;
    },
    [placements],
  );

  /**
   * The SAME two values as the state above, mirrored in refs — because the COMMIT must not happen
   * inside a state updater.
   *
   * `endPaneDrag` used to read them by calling `onMovePane` from within nested `setDraggedPaneId` /
   * `setDropTarget` updaters, i.e. a side effect in the RENDER phase. React is explicitly free to
   * run an updater more than once, and `<React.StrictMode>` — which `main.tsx` wraps the whole app
   * in — does it on every commit: measured, one release fired `onMovePane` TWICE
   * (`pane-move.browser.test.tsx`, "under <StrictMode> … EXACTLY ONCE"). The restructure happened to
   * be idempotent, so nothing visibly broke, but the drop was one non-idempotent handler away from
   * applying itself twice. The gesture's end is an EVENT, so it commits from the event handler and
   * the updaters go back to doing nothing but setting state.
   */
  const draggedPaneRef = useRef<string | null>(null);
  const dropTargetRef = useRef<{ paneId: string; edge: PaneEdge } | null>(null);

  const endPaneDrag = useCallback(
    (commit: boolean) => {
      const dragged = draggedPaneRef.current;
      const target = dropTargetRef.current;
      draggedPaneRef.current = null;
      dropTargetRef.current = null;
      setDraggedPaneId(null);
      setDropTarget(null);
      if (commit && dragged != null && target != null) {
        onMovePane?.(dragged, target.paneId, target.edge);
      }
    },
    [onMovePane],
  );

  const startPaneDrag = useCallback((paneId: string) => {
    draggedPaneRef.current = paneId;
    dropTargetRef.current = null;
    setDraggedPaneId(paneId);
    setDropTarget(null);
  }, []);

  /** The indicator's target placement, looked up once per render. */
  const indicatorPlacement: PanePlacement | null =
    draggedPaneId == null || dropTarget == null
      ? null
      : ([...placements.values()].find((p) => p.paneId === dropTarget.paneId) ?? null);

  /**
   * IS THE HOVERED DROP A PERMUTATION? (review finding 01M11WFPFZGAC68VGXJSG6QNMK)
   *
   * The edge band says "I am about to CUT here", and for the drop the user pointed at — two panes,
   * A dropped on the far edge of B — that is simply false: A detaches, B is left alone, A comes
   * back on the other side, and what happens is that the two terminals EXCHANGE places. So the
   * question is asked of the mutation itself rather than guessed from the pane count: run
   * `movePaneToEdge` DRY on the hovered target and compare the result to the tree on screen
   * ({@link paneSwapOf}). Exact by construction, and still exact the day a nested split produces the
   * same permutation somewhere else — which is exactly what a `panes.length === 2` shortcut would
   * get wrong.
   *
   * `null` ⇒ a genuine restructure ⇒ the edge band, unchanged.
   */
  const swap = useMemo(() => {
    if (layout == null || draggedPaneId == null || dropTarget == null) return null;
    const candidate = movePaneToEdge(layout, draggedPaneId, dropTarget.paneId, dropTarget.edge);
    return candidate == null ? null : paneSwapOf(layout, candidate);
  }, [layout, draggedPaneId, dropTarget]);

  /** The two panes that would exchange — the dragged one and the hovered one. */
  const swapPlacements: PanePlacement[] =
    swap == null
      ? []
      : [draggedPaneId, dropTarget?.paneId].flatMap((paneId) => {
          const found = [...placements.values()].find((p) => p.paneId === paneId);
          return found ? [found] : [];
        });

  return (
    <div
      ref={hostRef}
      // `select-none` WHILE A DRAG IS IN FLIGHT: the pointer is captured by the separator, so no
      // mouse event reaches xterm's selection layer — but a stray native drag-select over the
      // terminals would still paint a selection. Suppressing it for the duration keeps a resize
      // from leaving text highlighted in every pane it swept over.
      className={cn(
        "relative h-full w-full",
        (draggedSplitId != null || draggedPaneId != null) && "select-none",
      )}
      data-pane-host="true"
      data-pane-count={panes}
      data-resizing={draggedSplitId ?? undefined}
      data-moving-pane={draggedPaneId ?? undefined}
      // The pane-restructure drag's move/up/cancel: the grip owns the POINTER CAPTURE, so these
      // arrive here through it (bubbling) whichever surface sits under the pointer. Guarded by
      // `draggedPaneId`, so the separators' own drags — whose events bubble here too — are ignored.
      onPointerMove={(event) => {
        if (draggedPaneId == null || !canRestructure) return;
        const resolved = resolveDropAt(event.clientX, event.clientY, draggedPaneId);
        const next =
          resolved != null && canMovePane(draggedPaneId, resolved.paneId, resolved.edge)
            ? resolved
            : null;
        // The ref carries the comparison (a drag that jitters within one zone must not churn the
        // tree), so the updater stays a plain value and the commit can read the same truth.
        const current = dropTargetRef.current;
        if (current?.paneId === next?.paneId && current?.edge === next?.edge) return;
        dropTargetRef.current = next;
        setDropTarget(next);
      }}
      onPointerUp={() => endPaneDrag(true)}
      onPointerCancel={() => endPaneDrag(false)}
    >
      {ordered.map((terminal) => {
        const placement = placements.get(terminal.id);
        const visible = placement != null;
        const focused = visible && placement.paneId === focusedPaneId;
        return (
          <div
            key={terminal.id}
            data-terminal-id={terminal.id}
            data-pane-id={placement?.paneId}
            data-visible={visible ? "true" : "false"}
            data-focused={focused ? "true" : "false"}
            // A pane that is not in the tiling stays MOUNTED (instance/PTY/buffer alive) but has no
            // box — the instance's 0×0 guard skips it, so it never resizes to a bogus geometry.
            style={placement ? paneRectStyle(placement.rect) : { display: "none" }}
            className={cn(
              // Every pane draws its OWN limits at rest (`outline`, inset so it stays inside the
              // tile): with the separator's hairline this is what makes "where does this pane
              // stop" answerable without moving the pointer. The unfocused outline is the plain
              // `border` token — it used to be `/60`, faint enough to read as no border at all.
              //
              // THE FOCUSED PANE IS THE SAME LINE, THICKER AND LIGHTER (review
              // 01M0ABR6V501D0SPR2BYBVZC4A, minor — the user's arbitrage on the mockups): 1px of
              // `ring/70` was not enough to tell where the keyboard was going, so it becomes 2px
              // of `muted-foreground` (oklch 0.708 in dark — a whole step lighter than `ring`'s
              // 0.556). NOTHING ELSE marks the focus: no per-pane header (it would cost a line of
              // height per tile) and no dimming of the neighbours (it would degrade the very
              // output the user watches out of the corner of their eye). Both were explicitly
              // refused; so is naming the terminal here — a border says WHERE the focus is, never
              // WHICH terminal is in the pane, and that limit is assumed.
              //
              // WHY `outline` AND NOT `border`: an outline is painted OUT of the flow — it has no
              // box-model contribution at all, so it cannot move a pane's rect or a separator by
              // a single pixel (the 180×120px floor and every audited rectangle stay put). The
              // negative offset pulls the whole stroke INSIDE the tile so a 2px line never bleeds
              // over the neighbour's edge either.
              "group/pane overflow-hidden",
              focused
                ? "outline outline-2 outline-offset-[-2px] outline-muted-foreground"
                : "outline outline-1 outline-offset-[-1px] outline-border",
            )}
            // Focus follows the pointer press (before the terminal grabs the keyboard) and any
            // focus landing inside the pane (keyboard traversal), so the two entry points agree.
            onMouseDownCapture={placement ? () => onFocusPane(placement.paneId) : undefined}
            onFocusCapture={placement ? () => onFocusPane(placement.paneId) : undefined}
          >
            <Terminal
              cwd={terminal.cwd}
              // ONLY the focused pane restores the keyboard focus. Every visible pane is live.
              active={focused}
              recordId={terminal.id}
              deadHistory={terminal.scrollback}
            />
            {visible ? (
              <PaneControls
                paneId={placement.paneId}
                canClose={panes > 1}
                // The restructure grip exists only when the gesture can actually run (handlers
                // wired) AND there is something to restructure (a second pane to drop onto).
                canMove={canRestructure && panes > 1}
                onGripPointerDown={(event) => {
                  if (event.button !== 0) return;
                  // No native selection/drag, no separator under a neighbouring edge stealing the
                  // press — then own the gesture until the release.
                  event.preventDefault();
                  event.stopPropagation();
                  try {
                    event.currentTarget.setPointerCapture(event.pointerId);
                  } catch {
                    // Robustness only (see the separator's identical guard): the host-level
                    // handlers are bound either way, so a synthetic pointer id changes nothing.
                  }
                  startPaneDrag(placement.paneId);
                }}
                // The box the pane's REAL px decide the cluster's shape with — same measurement as
                // before, unchanged.
                box={hostSize == null ? null : paneRectPx(placement.rect, hostSize)}
                onSplitPane={onSplitPane}
                onClosePane={onClosePane}
              />
            ) : null}
            {/* The Spec surcouche is anchored HERE — inside THIS pane's box, which is the
                positioning block for its `absolute` sub-overlays (and the `overflow-hidden` that
                keeps it off the neighbours) — instead of the whole main area.

                EVERY VISIBLE PANE, not just the focused one (review finding
                01M11WF11C0H8G2MG515FBT6RD): a Spec belongs to the terminal that is working it, so
                it stays readable while the keyboard is elsewhere. The focus is still marked, by the
                pane's own outline and by `<Terminal active>` — the surcouche was never the signal.
                The single-resolver invariant is upheld one level up (see `renderPaneOverlay`). */}
            {visible && renderPaneOverlay ? renderPaneOverlay(terminal.id) : null}
          </div>
        );
      })}
      {/* The separators LAST so they paint over the tiles' edges — they are the only surface above
          a pane that must still receive the pointer. */}
      {onResizeSplit
        ? separators.map((separator) => (
            <PaneSeparatorHandle
              key={separator.splitId}
              separator={separator}
              hostRef={hostRef}
              hostSize={hostSize}
              dragging={draggedSplitId === separator.splitId}
              onDraggingChange={setDraggedSplitId}
              onResize={onResizeSplit}
            />
          ))
        : null}
      {/* THE DROP INDICATOR, IN ITS TWO VOCABULARIES. By default it is a BAND — half of the target
          pane's box, on the hovered edge: where the moved pane will land, i.e. "I am about to cut
          here". Where the mutation would in fact PERMUTE two terminals rather than cut anything,
          the band would be a promise the drop does not keep, so the indicator switches shape (see
          `swap` above). Both are painted ABOVE everything (z-50, over the separators' z-30 and the
          controls' z-40); the band deliberately KEEPS pointer events (no `pointer-events-none`):
          the gesture reads the pointer through its capture, so the overlay eats nothing, and a
          hit-test at its centre answers with the indicator itself. Either exists ONLY while the
          hovered drop clears the floor — an invalid hover shows nothing at all. */}
      {swap != null && swapPlacements.length === 2 ? (
        /* THE PERMUTATION INDICATOR — the SECOND vocabulary (review finding
             01M11WFPFZGAC68VGXJSG6QNMK). Where the drop does not cut anything but makes two
             terminals exchange places, an edge band would promise a split that never happens. So
             BOTH panes are outlined WHOLE — "these two swap" — instead of half of one being
             highlighted. Same tokens, same z-50, same pointer behaviour as the band: only the
             SHAPE and the SPAN change, because only the meaning did. */
        swapPlacements.map((placement) => (
          <div
            key={placement.paneId}
            data-pane-swap-indicator={placement.paneId}
            style={paneRectStyle(placement.rect)}
            className="pointer-events-none z-50 bg-ring/20 outline outline-2 -outline-offset-2 outline-dashed outline-ring"
          />
        ))
      ) : indicatorPlacement != null && dropTarget != null ? (
        <div
          data-pane-drop-indicator={dropTarget.edge}
          data-drop-target-pane={dropTarget.paneId}
          style={paneDropIndicatorStyle(indicatorPlacement.rect, dropTarget.edge)}
          className="z-50 bg-ring/25 outline outline-2 -outline-offset-2 outline-ring"
        />
      ) : null}
    </div>
  );
}

/**
 * The host box in CSS PIXELS, kept in state — `null` until it has been measured once.
 *
 * WHY IT IS STORED AT ALL. Everything else about the tiling is percent-based and the drag measures
 * the box on demand (no cached size to go stale). But the ABSOLUTE pane floor is expressed in
 * pixels, so a separator's REAL course — the `aria-valuemin` / `aria-valuemax` a screen reader
 * announces — cannot be derived from the tree alone: it needs the host's px size AT RENDER TIME.
 * Hence this one observer, and nothing more: it feeds ARIA only. It is NOT a second geometry
 * pipeline — no terminal is measured, fitted or resized from it; each pane's size still comes from
 * the browser and each PTY's from that terminal's own ResizeObserver → FitAddon → `pty_resize`.
 * Rounded to whole pixels so a sub-pixel reflow cannot churn the tree of separators.
 */
function useHostSize(hostRef: RefObject<HTMLDivElement | null>): HostSize | null {
  const [size, setSize] = useState<HostSize | null>(null);
  useEffect(() => {
    const host = hostRef.current;
    if (host == null) return;
    const measure = () => {
      const box = host.getBoundingClientRect();
      // A 0×0 box is NOT a measurement — it is the absence of one (an unattached host, and every
      // box in jsdom, which has no layout engine). Reporting it as a size would make the floor and
      // the control-cluster fallback act on a lie; `null` keeps them at their pre-measure default.
      if (!(box.width > 0) || !(box.height > 0)) return;
      const next = { width: Math.round(box.width), height: Math.round(box.height) };
      setSize((current) =>
        current != null && current.width === next.width && current.height === next.height
          ? current
          : next,
      );
    };
    measure();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(measure);
    observer.observe(host);
    return () => observer.disconnect();
  }, [hostRef]);
  return size;
}

/**
 * ONE SEPARATOR — the draggable, keyboard-operable band on a split's boundary.
 *
 * POINTER. The press takes POINTER CAPTURE, so every subsequent move/up is delivered here even
 * when the pointer flies over a terminal (or leaves the window): the drag can never be "dropped"
 * mid-gesture, and xterm's selection layer never sees a single one of those events. The press is
 * also `preventDefault`-ed (no native text selection starts) and `stopPropagation`-ed (the pane
 * under it does not steal the focus), then focuses the band itself — which is what makes
 * "click the seam, then use the arrows" a real keyboard entry point.
 *
 * KEYBOARD. `role="separator"` + `tabindex=0` + the `aria-value*` triple is the ARIA WINDOW
 * SPLITTER pattern, so the band is a first-class widget: arrows step it, Page Up/Down step it
 * coarsely, Home/End jump to the bounds, and every result is CLAMPED by {@link ratioFromKey}.
 * CAVEAT worth knowing: xterm consumes a plain `Tab` while a terminal has the keyboard, so
 * tabbing OUT of a focused terminal does not reach this band — reaching it means tabbing from
 * outside the tiling (the sidebar) or clicking the seam once. That is a property of the terminal,
 * not of the separator, and it is why the pointer press deliberately grants focus.
 *
 * PHASES. A drag streams `preview` ratios (applied live, so the panes move under the pointer and
 * each terminal's own ResizeObserver → FitAddon → `pty_resize` follows — the existing and only
 * geometry pipeline) and ends with exactly one `commit` (the persisted write). A key press is
 * already the whole interaction, so it commits directly.
 */
function PaneSeparatorHandle({
  separator,
  hostRef,
  hostSize,
  dragging,
  onDraggingChange,
  onResize,
}: {
  separator: PaneSeparator;
  hostRef: RefObject<HTMLDivElement | null>;
  /** The measured host — what turns the absolute px floor into THIS split's ratio bounds. */
  hostSize: HostSize | null;
  dragging: boolean;
  onDraggingChange: (splitId: string | null) => void;
  onResize: (splitId: string, ratio: number, phase: SplitResizePhase) => void;
}) {
  // The pointer OWNING the drag (`null` ⇒ idle) — a second finger/button cannot hijack it.
  const pointerRef = useRef<number | null>(null);
  // The last ratio a preview applied: what the COMMIT falls back to when the gesture ends without
  // a usable position (pointercancel, capture lost because the element went away).
  const lastRatioRef = useRef(separator.ratio);

  const ratioAt = useCallback(
    (event: ReactPointerEvent<HTMLDivElement>): number | null => {
      const host = hostRef.current;
      if (host == null) return null;
      const box = host.getBoundingClientRect();
      if (!(box.width > 0) || !(box.height > 0)) return null;
      return ratioFromPointer(
        separator,
        { x: event.clientX - box.left, y: event.clientY - box.top },
        { width: box.width, height: box.height },
      );
    },
    [hostRef, separator],
  );

  const endDrag = useCallback(
    (event: ReactPointerEvent<HTMLDivElement>, fromPosition: boolean) => {
      if (pointerRef.current !== event.pointerId) return;
      pointerRef.current = null;
      onDraggingChange(null);
      const ratio = (fromPosition ? ratioAt(event) : null) ?? lastRatioRef.current;
      lastRatioRef.current = ratio;
      onResize(separator.splitId, ratio, "commit");
    },
    [onDraggingChange, onResize, ratioAt, separator.splitId],
  );

  const vertical = separator.direction === "row";
  // THIS split's REAL course: the relative band tightened by the absolute px floor of everything
  // on each side (nested panes composed). Announcing the constant 15 / 85 instead would lie about
  // how far the seam can actually travel — a screen-reader user would keep pressing into a wall.
  // And when the sub-box can no longer satisfy the floor at all, the band DEGRADES back to 15..85
  // rather than collapsing (see {@link separatorRatioBounds}): two equal bounds would announce a
  // widget that cannot move, exactly when the manual recovery matters most.
  const bounds = separatorRatioBounds(separator, hostSize);
  return (
    <div
      role="separator"
      tabIndex={0}
      // A focusable separator IS a widget: it must publish where it stands and how far it may go,
      // or a screen-reader user has no way to tell a resize happened at all.
      aria-orientation={vertical ? "vertical" : "horizontal"}
      aria-label={vertical ? "Resize panes left and right" : "Resize panes up and down"}
      // WHOLE PERCENTS, deliberately (review finding). ARIA values are read out loud, and a
      // separator that announces "49.9" then "50.1" is noise, not information. The ASSUMED cost is
      // that a course NARROWER than one percent of the sub-box is announced as no course at all:
      // between 361 and 363px of sub-box the band really is ~[49.9%, 50.1%] and the seam really
      // moves by up to 3px, yet both bounds round to 50. The error is BOUNDED by ~1% of the
      // sub-box (≈25px on a 2560px one) and only ever appears within a few px of the point where
      // the band genuinely collapses (360px for two leaves, where 50/50 is the one feasible
      // layout) — never in the DEGRADED regime, whose whole point is to announce a real 15..85
      // course rather than two equal bounds.
      aria-valuenow={Math.round(separator.ratio * 100)}
      aria-valuemin={Math.round(bounds.min * 100)}
      aria-valuemax={Math.round(bounds.max * 100)}
      aria-valuetext={`${Math.round(separator.ratio * 100)}%`}
      data-separator-id={separator.splitId}
      data-direction={separator.direction}
      style={separatorStyle(separator)}
      className={cn(
        // `touch-none` (touch-action:none) so a touch drag resizes instead of scrolling the page.
        "z-30 touch-none outline-none transition-colors",
        vertical ? "cursor-col-resize" : "cursor-row-resize",
        // THE SEAM AT REST — a HAIRLINE drawn INSIDE the grab band by a pseudo-element, so the
        // boundary (and hence the handle) is visible with NO interaction: the band itself stays
        // transparent until hovered, but the 1px trait always says "two panes meet here, grab
        // this". The pseudo-element is the whole point of the split: the HIT AREA keeps its exact
        // SEPARATOR_THICKNESS_PX geometry (the inline `separatorStyle` box the drag tests measure)
        // while the PAINTED trait is thin enough not to eat into the terminals.
        // `pointer-events-none` on it so hit-testing can only ever reach the band.
        "before:pointer-events-none before:absolute before:transition-colors before:content-['']",
        vertical
          ? "before:inset-y-0 before:left-1/2 before:w-px before:-translate-x-1/2"
          : "before:inset-x-0 before:top-1/2 before:h-px before:-translate-y-1/2",
        // FOUR DISCERNIBLE STATES, all built from ONE design-system token (`ring`, the same the
        // drag state already used) at rising strengths — no raw colour anywhere:
        //   rest           → transparent band + a `ring/85` hairline (permanent, quiet);
        //   hover          → `ring/40` band + a `ring/95` hairline;
        //   focus-visible  → `ring/60` band + a full `ring` hairline;
        //   dragging       → `ring/70` band + a full `ring` hairline.
        //
        // WHY /85 AND NOT /50 (review finding F3). The hairline WAS painted — the finding's own
        // measurement, taken on the pseudo-element in headless Chromium, dark theme, reads
        // `oklab(0.556 0 0 / 0.5)` on a 1×700px box centred in the band, over a
        // `oklch(0.145 0 0)` terminal background. So this was never a "not drawn" bug: composited,
        // it lands at rgb(63,63,63) for a contrast of 1.88:1 — under the WCAG 1.4.11 non-text
        // floor of 3:1, i.e. PAINTED BUT UNREADABLE. Measured against that same background the
        // token gives 0.5 → 1.88, 0.7 → 2.61, 0.8 → 3.05, 0.85 → 3.30, 0.9 → 3.61, 1 → 4.18, so
        // /85 is the first step that clears 3:1 with any margin, and the hover value moves to /95
        // to keep the "it strengthens under the pointer" progression intact above it.
        dragging
          ? "bg-ring/70 before:bg-ring"
          : "bg-transparent before:bg-ring/85 hover:bg-ring/40 hover:before:bg-ring/95 focus-visible:bg-ring/60 focus-visible:before:bg-ring",
      )}
      onPointerDown={(event) => {
        if (event.button !== 0 || pointerRef.current !== null) return;
        // No native selection, no pane focus steal — then take the keyboard for the arrows.
        event.preventDefault();
        event.stopPropagation();
        try {
          event.currentTarget.setPointerCapture(event.pointerId);
        } catch {
          // Capture is a ROBUSTNESS enhancement (it keeps a drag alive over a terminal), never the
          // mechanism: the handlers below are bound to this element either way. A pointer id the
          // UA does not consider active (a synthetic event) makes it throw — the drag still works.
        }
        event.currentTarget.focus();
        pointerRef.current = event.pointerId;
        lastRatioRef.current = separator.ratio;
        onDraggingChange(separator.splitId);
      }}
      onPointerMove={(event) => {
        if (pointerRef.current !== event.pointerId) return;
        const ratio = ratioAt(event);
        if (ratio == null) return;
        lastRatioRef.current = ratio;
        onResize(separator.splitId, ratio, "preview");
      }}
      onPointerUp={(event) => endDrag(event, true)}
      onPointerCancel={(event) => endDrag(event, false)}
      // Capture can also be lost WITHOUT a pointerup (the node re-rendered away, another element
      // grabbed it). `endDrag` is guarded by the owning pointer id, so the ordinary
      // pointerup-then-lostpointercapture pair still commits exactly once.
      onLostPointerCapture={(event) => endDrag(event, false)}
      onKeyDown={(event) => {
        // Measured HERE (not from `hostSize`) for the same reason the drag does: the freshest box
        // is the honest one, and a key can land in the same frame a resize did.
        const box = hostRef.current?.getBoundingClientRect();
        const measured =
          box != null && box.width > 0 && box.height > 0
            ? { width: box.width, height: box.height }
            : hostSize;
        const ratio = ratioFromKey(separator, event.key, measured);
        if (ratio == null) return;
        // Own the key: no page scroll (arrows/Page/Home/End), no bubbling to the page hotkeys.
        event.preventDefault();
        event.stopPropagation();
        lastRatioRef.current = ratio;
        // A key press is the whole interaction — its end is now, so it persists immediately.
        onResize(separator.splitId, ratio, "commit");
      }}
    />
  );
}

/** One pane command — the SAME definition backs the button and the overflow menu row. */
interface PaneCommand {
  readonly id: string;
  /** The accessible name, identical in both shapes so a test (and a user) reads one vocabulary. */
  readonly label: string;
  /** The button's tooltip — where the keyboard twin becomes discoverable. */
  readonly hint: string;
  readonly icon: ReactNode;
  readonly destructive?: boolean;
  readonly run: () => void;
}

/**
 * The per-pane controls: SPLIT RIGHT / SPLIT DOWN / CLOSE PANE. Discreet (revealed on hover or
 * keyboard focus inside the pane) so they never sit permanently over the terminal, but always
 * reachable by keyboard. "Close pane" disappears when a single pane is left — the last pane is
 * kept by construction.
 *
 * THE INVARIANT (review finding F2, critical): NO control is ever out of its pane's frame, and
 * NONE is ever covered — a click always reaches the command it points at. Two things enforce it:
 *
 *  - STACKING. The cluster is `z-40`, ABOVE the separators' `z-30`. Both compete in the HOST's
 *    stacking context (a pane slot is `position:absolute` with `z-index:auto`, which is NOT a
 *    stacking context), so at `z-20` the cluster used to lose to any seam it touched — and the
 *    click landed on the separator instead of on "Close pane". Raising it is the whole fix, and
 *    it is INDEPENDENT of the pane's width: it bit wide panes too, wherever a seam ran past the
 *    top-right corner. The container keeps `pointer-events-none` with `pointer-events-auto` on
 *    the buttons ONLY, so the seam stays grabbable everywhere the buttons are not.
 *  - FIT. The cluster needs `paneControlsWidthPx` px — priced in `pane-geometry.ts`, NEXT TO the
 *    absolute pane floor it justifies (180px was chosen so the three-button cluster always fits),
 *    with a test pinning that inequality instead of two comments asserting it at each other.
 *    Below that width the three buttons used to
 *    overflow the frame (anchored right, they grew leftwards past the pane's edge, where
 *    `overflow-hidden` clipped them out of sight while they kept eating the clicks) — so under
 *    that width the cluster COLLAPSES to a single `…` overflow button opening the very same
 *    commands. Nothing is dropped, nothing sticks out.
 */
function PaneControls({
  paneId,
  canClose,
  canMove = false,
  onGripPointerDown,
  box,
  onSplitPane,
  onClosePane,
}: {
  paneId: string;
  canClose: boolean;
  /** Whether the restructure grip is offered (handlers wired AND a second pane exists). */
  canMove?: boolean;
  /** The grip's pointerdown — the press that starts the view's restructure drag. */
  onGripPointerDown?: (event: ReactPointerEvent<HTMLButtonElement>) => void;
  /** The pane's measured box in CSS px, or `null` while the host has not been measured. */
  box: { readonly width: number; readonly height: number } | null;
  onSplitPane: (paneId: string, direction: SplitDirection) => void;
  onClosePane: (paneId: string) => void;
}) {
  const commands: PaneCommand[] = [
    {
      id: "split-right",
      label: "Split pane right",
      // The tooltip is where the KEYBOARD twin of this button becomes discoverable: the chord is
      // formatted for the running platform (Ctrl on Windows/Linux, ⌘ on macOS) from the one
      // constant that also registers it, so the two can never drift apart.
      hint: `Split right (${SPLIT_RIGHT_LABEL})`,
      icon: <Columns2Icon className="size-3.5" />,
      run: () => onSplitPane(paneId, "row"),
    },
    {
      id: "split-down",
      label: "Split pane down",
      hint: `Split down (${SPLIT_DOWN_LABEL})`,
      icon: <Rows2Icon className="size-3.5" />,
      run: () => onSplitPane(paneId, "column"),
    },
  ];
  if (canClose) {
    commands.push({
      id: "close-pane",
      label: "Close pane",
      hint: "Close pane (the terminal keeps running)",
      icon: <XIcon className="size-3.5" />,
      destructive: true,
      run: () => onClosePane(paneId),
    });
  }

  const mode = paneControlsMode(box, commands.length + (canMove ? 1 : 0));
  if (mode === "none") return null;

  // THE GRIP IS PART OF THE BUDGET IN *BOTH* SHAPES — the half of F2's invariant the `overflow`
  // fallback was missing. `paneControlsMode` prices that fallback at `paneControlsWidthPx(1)`
  // (32px, one `…`), but the collapsed cluster below ALSO renders the grip when the gesture is
  // available: two buttons, i.e. `paneControlsWidthPx(2)` = 60px. Between those two widths the
  // grip grew leftwards past the pane's edge — clipped out of sight by `overflow-hidden` while it
  // kept eating the clicks, which is the exact defect the collapse exists to prevent. So the grip
  // claims space only where it measurably fits; below that the `…` is alone and the move stays
  // reachable from the pane's own drag surface.
  const gripFits = box == null || box.width >= paneControlsWidthPx(2);
  const showGrip = canMove && (mode === "full" || gripFits);

  // THE CLUSTER'S SKIN, layered ON TOP of `<Button variant="ghost" size="icon-xs">` (review
  // finding — the pane kebab must be the design system's `<Button>` like the sidebar's three
  // kebabs, not a hand-rolled `<button>`: that is what brings `focus-visible:ring-2 ring-ring
  // ring-offset-1` instead of the browser's own outline).
  //
  //  - `size-6` PINS `CONTROL_BUTTON_PX`. `icon-xs` is `size-7 sm:size-6`, i.e. 24px only above
  //    the `sm` breakpoint; the cluster's px budget is not responsive, so the size is fixed here
  //    and a browser spec measures the RENDERED box against the constant.
  //  - the border/background/text tokens are NOT `ghost`'s: these buttons float OVER a terminal,
  //    where a fully transparent control would be unreadable. `ghost` keeps the interaction
  //    states, the surface stays opaque.
  const button =
    "pointer-events-auto size-6 border-border/70 bg-background/90 text-muted-foreground hover:bg-muted hover:text-foreground";
  // `top-1 right-1` IS `CONTROL_INSET_PX` and `gap-1` IS `CONTROL_GAP_PX` — same mirror, same
  // spec. `z-40` beats the separators' `z-30` and the Spec surcouche's {@link OVERLAY_SURFACE_Z}
  // (`pane-geometry.ts`, where the whole three-way stack order is written down — this comment used
  // to name a constant that had been designed and never written, so the surcouche sat at a bare
  // `z-20` and LOST to the seam);
  // `has-[[aria-expanded=true]]` keeps the `…` trigger visible while its menu is open (focus has
  // moved into the portaled popup, so `focus-within` no longer holds and the trigger would
  // otherwise fade out under the user's pointer).
  const cluster =
    "pointer-events-none absolute top-1 right-1 z-40 flex items-center gap-1 opacity-0 transition-opacity focus-within:opacity-100 group-hover/pane:opacity-100 has-[[aria-expanded=true]]:opacity-100";

  // THE RESTRUCTURE GRIP — the pane's ONE prise in the tiled view (the sidebar rows have their
  // own), first in the cluster so it reads "move · split · split · close". It claims space only
  // when the gesture can actually run (`canMove`: handlers wired AND a second pane to drop onto),
  // and `touch-none` lets a touch drag move the pane instead of scrolling.
  const grip = showGrip ? (
    <Button
      variant="ghost"
      size="icon-xs"
      className={cn(button, "cursor-grab touch-none")}
      data-pane-grip={paneId}
      aria-label="Move pane"
      title="Move pane — drop on an edge of another pane"
      onPointerDown={onGripPointerDown}
    >
      <GripVerticalIcon className="size-3.5" />
    </Button>
  ) : null;

  if (mode === "overflow") {
    return (
      <div className={cluster} data-pane-controls={paneId} data-pane-controls-mode="overflow">
        {grip}
        {/* THE OVERFLOW KEBAB — the SAME shape as the product's three other `<Menu>` callers
            (sidebar's terminal / workspace / project rows): a `<Button>` trigger, the menu's OWN
            `tooltip` prop (design-system popover, not the OS's native `title` bubble), and the
            product's one overflow glyph, `MoreVerticalIcon`. */}
        <Menu
          side="bottom"
          align="end"
          tooltip="Pane commands"
          trigger={
            <Button variant="ghost" size="icon-xs" className={button} aria-label="Pane commands">
              <MoreVerticalIcon className="size-3.5" />
            </Button>
          }
        >
          {commands.map((command) => (
            <MenuItem
              key={command.id}
              icon={command.icon}
              destructive={command.destructive}
              onClick={command.run}
            >
              {command.label}
            </MenuItem>
          ))}
        </Menu>
      </div>
    );
  }

  return (
    <div className={cluster} data-pane-controls={paneId} data-pane-controls-mode="full">
      {grip}
      {commands.map((command) => (
        // The three direct commands share the kebab's `<Button>` treatment, so the cluster reads
        // as ONE control surface whichever shape it takes. Their hint stays a native `title`: no
        // `<Menu>` is involved here to carry it, and mounting three portaled design-system
        // tooltips over a live terminal — on a surface that only appears on hover — would cost
        // more than the consistency it buys. The kebab uses the DS tooltip because `<Menu>`
        // already offers it on the same element.
        <Button
          key={command.id}
          variant="ghost"
          size="icon-xs"
          className={button}
          aria-label={command.label}
          title={command.hint}
          onClick={command.run}
        >
          {command.icon}
        </Button>
      ))}
    </div>
  );
}

export default PaneTreeHost;
