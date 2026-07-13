import { KeyboardSensor, PointerActivationConstraints, PointerSensor } from "@dnd-kit/dom";

/** Movement (px) past which a pointer gesture counts as a DRAG, not a click/select. */
const DRAG_ACTIVATION_DISTANCE = 5;

/**
 * The shared dnd-kit sensor set for the cockpit's drag-reorders (the project rail AND the
 * terminal rows) — the SAME setup the v1 `sortable-*-list` used: a POINTER sensor with a small
 * activation distance (a tap under 5px still selects the row / enters the project — the drag
 * never swallows the click) plus the KEYBOARD sensor for a11y. Defined ONCE so the two reorder
 * surfaces stay in lockstep and neither reintroduces a competing drag config.
 */
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
    preventActivation: () => false,
  }),
  KeyboardSensor,
];

/** Shallow equality of two id sequences (same length, same order) — skip a no-op reorder. */
export function sameOrder(a: readonly string[], b: readonly string[]): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) {
    if (a[i] !== b[i]) return false;
  }
  return true;
}
