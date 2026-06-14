import type { Transition, Variants } from "motion/react";

/**
 * ENTER + EXIT variants for a sidebar terminal row. Chrome-only; never touches
 * the xterm viewport (a hard project rule).
 *
 * SINGLE-ANIMATOR (FOR HEIGHT) DESIGN (read before changing): the rows are PLAIN
 * `motion.li` elements — NOT `Reorder.Item`. A `Reorder.Item` ALWAYS runs Motion's
 * `layout` projection intrinsically, and a height tween on top of that projection
 * is a second animator: the row animates then teleports (the "double-tp" that kept
 * regressing). Confirmed against Motion's docs + discussion #1651: Reorder + a
 * height-collapse exit is not a supported combo. So we dropped Motion's Reorder
 * and Motion only animates HEIGHT + OPACITY here, in NORMAL DOCUMENT FLOW.
 *
 * IMPORTANT — NO `transform` props (no `x`/`y`/`scale`): drag-reorder is now
 * dnd-kit (`@dnd-kit/react`, see `sortable-terminal-item.tsx`), which drives the
 * row's `transform` imperatively during a drag + reflow. Motion writing a
 * `transform` here would fight dnd-kit on the same property. Splitting the work by
 * CSS property — Motion owns `height`/`opacity`, dnd-kit owns `transform` — lets
 * both coexist on the same element with nothing to fight.
 *
 * ENTER: a new row grows from height 0 → auto while fading in, pushing the rows
 * below down. EXIT: the closed row collapses height → 0 while fading; because the
 * height shrinks in normal flow, the rows below AND the enclosing project/workspace
 * band follow up over the SAME window — a smooth, single-pass close (no
 * fade-then-pop). `overflow-hidden` on the row clips the content while it
 * collapses. Reduced motion ⇒ instant (see `itemTransition`).
 */
export const itemVariants: Variants = {
  initial: { opacity: 0, height: 0 },
  animate: { opacity: 1, height: "auto" },
  exit: { opacity: 0, height: 0 },
};

/** Tween shared by the row enter/exit ramps. */
const MOTION_TRANSITION: Transition = {
  duration: 0.2,
  ease: [0.4, 0, 0.2, 1],
};

/** A zero-duration transition so motion settles instantly (reduced motion). */
const INSTANT_TRANSITION: Transition = { duration: 0 };

/**
 * Resolve the item transition honouring the user's reduced-motion preference.
 * When the user prefers reduced motion we return an instant transition so the
 * item still appears/moves correctly (no broken layout) but without the tween.
 * Pure → unit-testable without rendering.
 *
 * @param reduced whether `prefers-reduced-motion` is set (from `useReducedMotion`).
 */
export function itemTransition(reduced: boolean | null): Transition {
  return reduced ? INSTANT_TRANSITION : MOTION_TRANSITION;
}
