import { KeyboardSensor, PointerActivationConstraints, PointerSensor } from "@dnd-kit/dom";

/** Movement (px) past which a pointer gesture counts as a DRAG, not a click/select. */
const DRAG_ACTIVATION_DISTANCE = 5;

/**
 * The shared dnd-kit sensor set for the cockpit's drag-reorders (the project rail, the terminal
 * rows AND the commands modal's grouping list) — the SAME setup the v1 `sortable-*-list` used: a
 * POINTER sensor with a small activation distance (a tap under 5px still selects the row / enters
 * the project — the drag never swallows the click) plus the KEYBOARD sensor for a11y. Defined ONCE
 * so the reorder surfaces stay in lockstep and none reintroduces a competing drag config.
 *
 * IT LIVES IN `components/panes/` (review 01M11WE6H9E5D5FCS6RT0ZXAXN) for the reason `group-drag.ts`
 * moved with it: the commands modal is now a third consumer, and `components/command/` reaching into
 * `components/terminals/` would re-open the import cycle finding 01M0ZMHM9GSC39SCN2DBRQ8VX3 closed.
 */
/**
 * THE ONE OPT-OUT — a control that must never ACTIVATE a drag, whatever encloses it.
 *
 * ## Why an attribute, and why it is not `stopPropagation` (human review 01M1312FSV6MAR0P63SEG2RF6C)
 *
 * Every control inside a draggable surface already carries
 * `onPointerDown={(e) => e.stopPropagation()}` and reads as guarded. IT IS NOT. React 19 delegates
 * its listeners to the ROOT container, so a native `pointerdown`/`keydown` bubbles through the
 * enclosing element — where dnd-kit binds its sensor, on `source.handle ?? source.element` — LONG
 * before React gets to run the handler that would stop it. The guard has always been a no-op against
 * this particular listener; nobody noticed because the 5 px activation kept a plain click harmless
 * and because no draggable surface enclosed a control that ALSO answers to Enter and Space.
 *
 * Widening a group's grip to its whole head is the day one does: the head is now the handle and the
 * `…` sits inside it, so Enter on the `…` opened the panel AND lifted the block, leaving a drag
 * running behind a dialog. `preventDefault` is not an option either — dnd-kit does honour
 * `defaultPrevented`, but preventing a button's `keydown` default is exactly what stops it emitting
 * its `click`.
 *
 * So the opt-out is declared where dnd-kit itself asks the question: `preventActivation`, in the ONE
 * shared sensor config, reading a marker off the DOM. It is additive — anything unmarked keeps the
 * `false` the whole-surface drag depends on — and it works for both sensors and both phases, because
 * it is evaluated inside dnd-kit's own listener rather than racing it.
 */
export const DRAG_INERT_ATTR = "data-drag-inert";

/** Whether the event that would start a drag landed on (or inside) a control marked inert. */
function startsOnInertControl(event: Event): boolean {
  const target = event.target;
  return target instanceof Element && target.closest(`[${DRAG_INERT_ATTR}]`) != null;
}

export const reorderSensors = [
  PointerSensor.configure({
    activationConstraints: [
      new PointerActivationConstraints.Distance({ value: DRAG_ACTIVATION_DISTANCE }),
    ],
    // preventActivation OVERRIDE (v2.5.6 finding #23) — WITHOUT this the drag never STARTS.
    // Both reorder surfaces cover their whole draggable element (the `useSortable` ref) with a
    // single `<button>`: the terminal row's select button (`runtime-row.tsx`) and the project
    // tile's enter button (`base-layout.tsx`). dnd-kit's DEFAULT `preventActivation` CANCELS any
    // gesture whose `pointerdown` target is an interactive element (a `<button>`) that is neither
    // `source.element` nor `source.handle` — so a press on that covering button never activated
    // the sensor and the whole reorder was dead. We have no dedicated grip handle (whole-surface
    // drag, like v1), so we opt OUT of that guard: the 5px `Distance` activation still keeps a
    // plain click a pure SELECT (a sub-5px press never becomes a drag), and the row's rename/close
    // controls `stopPropagation` on their own pointer/click events, so single-click selection,
    // double-click/pencil rename and the × close all stay intact.
    //
    // …EXCEPT for a control that marked itself {@link DRAG_INERT_ATTR}, which is the only thing this
    // opt-out ever answers `true` to.
    preventActivation: (event) => startsOnInertControl(event),
  }),
  KeyboardSensor.configure({
    // THE SAME OVERRIDE, FOR THE SAME REASON — and it was missing, which made three of the four
    // gestures unreachable without a pointer.
    //
    // dnd-kit's default is `event.target !== (source.handle ?? source.element)`: the keydown must
    // land on the draggable element ITSELF. A terminal row has no handle and its `useSortable` ref
    // is the row wrapper (`[data-terminal-row]`), which is NOT focusable — the only thing a user
    // can Tab to inside a row is its select `<button>`, which covers the whole surface. So Space
    // arrived on the button, the guard saw a target that was neither the element nor a handle, and
    // the keyboard drag simply never started. (A BLOCK escaped this: its grip IS `source.handle`
    // and IS the focusable element, which is why moving a whole group by keyboard already worked —
    // the one gesture out of four that did.)
    //
    // Opting out makes the gesture reachable from the control the user actually focuses. It cannot
    // swallow anything else: the Space that starts a drag is the same Space that would activate the
    // row's select button, and dnd-kit `preventDefault`s it only once a drag is under way.
    //
    // …EXCEPT on a control marked {@link DRAG_INERT_ATTR}: a `…` that opens a dialog, a lifecycle
    // button that starts a process. Enter and Space are THEIR activation keys, and they are what
    // starts a keyboard drag — so without this the one press would do both.
    preventActivation: (event) => startsOnInertControl(event),
  }),
];

/** Shallow equality of two id sequences (same length, same order) — skip a no-op reorder. */
export function sameOrder(a: readonly string[], b: readonly string[]): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) {
    if (a[i] !== b[i]) return false;
  }
  return true;
}
