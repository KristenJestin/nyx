/**
 * The SHARED behaviour of the terminal↔Spec overlay's OPEN PANEL — the half of the chrome that is
 * identical in the rail (A, `spec-rail-overlay`) and the tiroir (B, `spec-drawer-overlay`), so it is
 * written ONCE here instead of twice there:
 *
 *  - {@link overlayPanelId} — the DETERMINISTIC id of a Spec's panel, the target of the languette's
 *    `aria-controls` (disclosure pattern) and the `id` of the panel container itself.
 *  - {@link useOverlayPanelDisclosure} — the NON-MODAL disclosure behaviour: focus INTO the panel on
 *    open, focus RESTORED to its languette on any dismissal, Escape (panel-scoped only) and
 *    outside-click dismissal.
 *  - {@link useOverlayPaneWidth} — the live width of the HOST PANE the overlay is anchored over, the
 *    input of `overlayPanelWidth` (`overlay-motion`).
 *
 * The two overlays keep their DELIBERATE structural asymmetry (the tiroir nests a scroller inside
 * its full-height wrapper to keep its own scrollbar hittable, the rail has a single box) — this
 * module shares BEHAVIOUR, never layout.
 */

import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import type { KeyboardEvent as ReactKeyboardEvent, RefObject } from "react";

/**
 * The DOM id of a Spec's overlay panel. Derived from the Spec id alone, so the languette can name
 * the panel it controls (`aria-controls`) WITHOUT the panel being mounted — the closed half of the
 * disclosure pattern, where the panel legitimately does not exist yet (both overlays mount their
 * panel under an `<AnimatePresence>`). The same function feeds the panel's own `id`, so the two can
 * never drift apart.
 */
export function overlayPanelId(specId: string): string {
  return `overlay-panel-${specId}`;
}

/**
 * What an outside-click must NOT dismiss, as ONE selector of explicit chrome markers — no list of
 * special cases:
 *
 *  - `[data-overlay-surface]` is the overlay's animated ROOT, so ANYTHING belonging to the overlay is
 *    a descendant of it: the open panel, every languette, and — the case a `panelRef`+`[data-overlay-tab]`
 *    pair missed — the tiroir's SCROLLER (`spec-drawer-overlay`), which deliberately stays
 *    `pointer-events-auto` to keep its own scrollbar hittable. Its bar, its thumb and the gutters
 *    between languettes are overlay chrome; dragging the pile that COMMANDS the panel must not
 *    congratulate itself by closing it. The wrapper's `pointer-events-none` zones are NOT excluded by
 *    this and must not be: a mousedown there never reaches the overlay at all (Chromium hit-tests
 *    straight through to the terminal underneath, so `event.target` IS the terminal) — and dismissing
 *    is the wanted behaviour there.
 *  - `[data-portal-layer]` is the container `ui/dialog`'s `<Dialog.Portal>` stamps on the node Base UI
 *    appends to `document.body`. A layer opened FROM the panel (the orchestration run modal of
 *    `<OverlayPanelActions>`) lives there: outside the overlay in the DOM, inside it for the user.
 *    `<SpecRunButton>` OWNS the modal's open state and renders it in place, so dismissing the panel
 *    would unmount the modal under the cursor. The marker is deliberately NOT `role="dialog"` — the
 *    panel itself carries that role, so it would discriminate nothing.
 */
const OVERLAY_INSIDE_SELECTOR = "[data-overlay-surface],[data-portal-layer]";

/**
 * The PANE the overlay is anchored over — the containing block of its `absolute inset-y-0 right-0`
 * root (`runtime-pane-host`'s `relative h-full w-full` box in the app). `offsetParent` IS that
 * containing block in a real browser; jsdom does not compute it, so the parent element is the
 * fallback that keeps the unit tests meaningful.
 */
function overlayPaneOf(root: HTMLElement | null): HTMLElement | null {
  if (root === null) return null;
  const offset = root.offsetParent;
  return offset instanceof HTMLElement ? offset : root.parentElement;
}

/**
 * The languette that CONTROLS the panel `panelId` — resolved through the DISCLOSURE relation the
 * languette itself declares (`aria-controls`), not through a ref.
 *
 * Deliberate: the tiroir stacks N languettes and only ONE of them is the trigger of what is open, so
 * a ref would have to be attached CONDITIONALLY — and a conditional `ref` on a `motion` element
 * inside `<AnimatePresence>` does NOT re-attach when the condition flips (measured: the ref stays
 * `null` after the state change, so focus was never restored). Reading the attribute the tab already
 * publishes is both robust and self-consistent: focus returns to whatever element CLAIMS to control
 * this panel. Attribute compared in a loop rather than in a selector so a Spec id never has to be
 * CSS-escaped.
 */
function tabControlling(root: HTMLElement | null, panelId: string | null): HTMLElement | null {
  if (root === null || panelId === null) return null;
  for (const tab of root.querySelectorAll<HTMLElement>("[data-overlay-tab]")) {
    if (tab.getAttribute("aria-controls") === panelId) return tab;
  }
  return null;
}

/**
 * The OPEN panel itself, resolved through the SAME disclosure relation as {@link tabControlling} —
 * the `id` the languette points at — rather than through `panelRef` alone.
 *
 * Deliberate, and it is the tiroir that makes it unavoidable: its `<AnimatePresence>` runs in the
 * default (SYNC) mode, so on a Spec→Spec switch the EXITING and the ENTERING panel are mounted at
 * the same time and BOTH carry `panelRef`. The ref then names whichever of the two React committed
 * last — not necessarily the one that is opening — and the exiting one's unmount later sets it back
 * to `null` while the entering panel is still on screen. Matching on the id is what makes "focus
 * the panel that is open" mean the panel that is open. Attribute compared in a loop rather than in
 * a selector so a Spec id never has to be CSS-escaped.
 */
function panelWithId(root: HTMLElement | null, panelId: string | null): HTMLElement | null {
  if (root === null || panelId === null) return null;
  for (const panel of root.querySelectorAll<HTMLElement>("[data-overlay-panel]")) {
    if (panel.id === panelId) return panel;
  }
  return null;
}

/**
 * The NON-MODAL disclosure behaviour of an open overlay panel.
 *
 * NON-MODAL is a decided property, not an oversight: the panel takes `role="dialog"` and a name, but
 * NO `aria-modal` and NO focus trap, so Tab walks OUT of it and the terminal underneath stays usable
 * at the keyboard while the Spec is on screen. What this hook adds is the part that was missing:
 *
 *  - OPEN ⇒ focus moves INTO the panel — onto the CONTAINER (`tabIndex={-1}`), never onto its first
 *    command: the first command is a navigation ("Open the Spec"), so landing on it would risk an
 *    accidental trigger and would skip the screen reader's announcement of the dialog and its name.
 *  - DISMISS ⇒ focus is RESTORED to the languette that controls the panel, whatever the gesture
 *    (close chevron, re-click on the languette, Escape, outside click).
 *  - ESCAPE is PANEL-SCOPED. Escape is a WORKING key in a terminal (shells, vim, TUIs), so there is
 *    deliberately NO `document`/`window` keydown listener here: {@link onPanelKeyDown} is attached to
 *    the panel container, so it only ever fires when focus is INSIDE the panel — focus in the
 *    terminal ⇒ Escape reaches the terminal untouched. When it does fire it stops propagating, so the
 *    keystroke that closed the panel does not also reach the terminal.
 *  - OUTSIDE CLICK dismisses — a click in the terminal area (the reflex gesture the review asked
 *    for), but also one in the SIDEBAR or the window chrome: the listener is bound to `document`, so
 *    "outside" means outside the OVERLAY, not merely outside the host pane. What it does NOT dismiss
 *    is named by ONE selector of explicit chrome markers ({@link OVERLAY_INSIDE_SELECTOR}) instead of
 *    a list of special cases. A languette in particular owns its own toggle, and treating its click
 *    as an outside click would produce a double transition (close then re-open, or the reverse).
 *    FOCUS RESTORATION IS DEFERRED here, unlike every other gesture. The listener runs on `mousedown`
 *    in the CAPTURE phase, i.e. BEFORE the browser's own default action, and that default action
 *    OVERWRITES the focus in BOTH branches (measured in Chromium: a focusable target takes the focus;
 *    a dead zone sends it to `document.body`) — so restoring it there is a plain no-op. It is instead
 *    scheduled on the macrotask AFTER the default action, and only fires when the click took NO focus
 *    at all (`document.activeElement` is `body`/`null`): a dead zone gives the languette back to the
 *    keyboard user, while a click on the terminal keeps the terminal focused. Nothing is stolen.
 */
export function useOverlayPanelDisclosure({
  open,
  panelId,
  rootRef,
  panelRef,
  onDismiss,
}: {
  /** Whether the panel is currently MOUNTED/open. */
  open: boolean;
  /**
   * The OPEN panel's id ({@link overlayPanelId}), or `null` when none is open. It is both the
   * focus-in key (a Spec SWITCH changes it, so the new panel is announced too) and the way the
   * controlling languette is found again ({@link tabControlling}).
   */
  panelId: string | null;
  /** The overlay's animated root (used to resolve the host pane + the controlling languette). */
  rootRef: RefObject<HTMLElement | null>;
  /** The panel container (`role="dialog"`, `tabIndex={-1}`). */
  panelRef: RefObject<HTMLElement | null>;
  /** Close the panel. Called by every dismissal gesture routed through {@link dismiss}. */
  onDismiss: () => void;
}): {
  /** Dismiss + restore focus — wire it to the close chevron and to the active languette's re-click. */
  dismiss: () => void;
  /** Panel-scoped Escape handler — put it on the panel container, NEVER on `document`. */
  onPanelKeyDown: (event: ReactKeyboardEvent<HTMLElement>) => void;
} {
  // Keep the caller's callback in a ref so the listener effect below depends on `open` alone and is
  // not re-bound on every render of the overlay (which re-renders on every live Spec push).
  const dismissRef = useRef(onDismiss);
  useEffect(() => {
    dismissRef.current = onDismiss;
  }, [onDismiss]);

  // FOCUS IN: on open, and again when the open Spec SWITCHES (the panel remounts, so the new dialog
  // must be announced too). `preventScroll` keeps the terminal's viewport from jumping.
  //
  // The panel is resolved by ITS ID first ({@link panelWithId}) and only then from `panelRef`: on a
  // switch, the exiting and the entering panel are BOTH mounted and both hold that ref, so the ref
  // alone can name the one that is leaving. The fallback covers the overlay that would not stamp an
  // id (none today) and the first frame of a jsdom test.
  useEffect(() => {
    if (!open) return;
    const panel = panelWithId(rootRef.current, panelId) ?? panelRef.current;
    panel?.focus({ preventScroll: true });
  }, [open, panelId, panelRef, rootRef]);

  const dismiss = useCallback(() => {
    // Read the languette BEFORE the close: `onDismiss` is a React state update, so the DOM is still
    // the open one here either way, but resolving first keeps the order obvious.
    const tab = tabControlling(rootRef.current, panelId);
    dismissRef.current();
    tab?.focus({ preventScroll: true });
  }, [panelId, rootRef]);

  // The DEFERRED focus restoration of the outside click (see the note above). Held in a ref so the
  // pending timer survives the listener effect's own teardown — that teardown runs as soon as
  // `open` flips to false, i.e. BEFORE the restoration is due — and is cancelled only when the hook
  // itself unmounts.
  const restoreTimerRef = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  useEffect(() => () => clearTimeout(restoreTimerRef.current), []);

  // OUTSIDE CLICK — bound to `document`, on `mousedown`, capturing; see the note above.
  useEffect(() => {
    if (!open) return;
    const onMouseDown = (event: MouseEvent) => {
      const target = event.target;
      if (!(target instanceof Element)) return;
      if (target.closest(OVERLAY_INSIDE_SELECTOR) !== null) return;
      dismissRef.current();
      // AFTER the browser's default mousedown action, never before it: the languette is given the
      // focus back only when the click took none (a dead zone), so a focusable target — the
      // terminal's own textarea first of all — keeps what the user just aimed at.
      clearTimeout(restoreTimerRef.current);
      restoreTimerRef.current = setTimeout(() => {
        // "The click took no focus" = nothing OUTSIDE the overlay holds it. In a browser that is
        // `document.body` (the focus-fixup step of a mousedown on a dead zone) or `null`; under
        // `<AnimatePresence>` the focus can also still be sitting on the panel that is playing its
        // exit, which is the same statement — the click parked the focus nowhere.
        const active = document.activeElement;
        const tookFocus =
          active !== null &&
          active !== document.body &&
          active.closest(OVERLAY_INSIDE_SELECTOR) === null;
        if (tookFocus) return;
        tabControlling(rootRef.current, panelId)?.focus({ preventScroll: true });
      }, 0);
    };
    document.addEventListener("mousedown", onMouseDown, true);
    return () => document.removeEventListener("mousedown", onMouseDown, true);
  }, [open, panelId, rootRef]);

  const onPanelKeyDown = useCallback(
    (event: ReactKeyboardEvent<HTMLElement>) => {
      if (event.key !== "Escape") return;
      // Focus is inside the panel (this handler lives ON it), so the keystroke belongs to the panel:
      // close, and keep it from also reaching the terminal / any document-level handler.
      event.stopPropagation();
      dismiss();
    },
    [dismiss],
  );

  return { dismiss, onPanelKeyDown };
}

/**
 * The live WIDTH of the host pane (px), remeasured whenever the pane is resized — the input the
 * panel's own width is bounded on (`overlayPanelWidth`, `overlay-motion`). Bounding on the PANE and
 * not on the viewport is the point: the pane is what the panel actually covers, sidebar excluded.
 *
 * `0` until the first measurement (and for the whole life of a jsdom test, which has no layout):
 * `overlayPanelWidth` floors that at its minimum, and the panel is CLOSED at mount anyway, so no
 * width jump is ever visible.
 */
export function useOverlayPaneWidth(rootRef: RefObject<HTMLElement | null>): number {
  const [paneWidth, setPaneWidth] = useState(0);

  useLayoutEffect(() => {
    const pane = overlayPaneOf(rootRef.current);
    if (pane === null) return;
    const measure = () => setPaneWidth(pane.getBoundingClientRect().width);
    measure();
    // A pane resize does NOT imply a window resize (a split/collapsed sidebar resizes it alone), so
    // the observer is the only correct trigger. jsdom stubs it to a no-op (`vitest.setup`).
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(measure);
    observer.observe(pane);
    return () => observer.disconnect();
  }, [rootRef]);

  return paneWidth;
}
