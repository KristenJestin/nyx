import type { Transition, Variants } from "motion/react";

/**
 * ENTER variant for a sidebar terminal item: a plain OPACITY fade-in. There is
 * intentionally NO `exit` and NO `height` here.
 *
 * All POSITIONAL motion — a closed row's gap closing, a new row pushing the
 * others down, and reordering — is owned by motion's `Reorder` (each
 * `Reorder.Item` animates its own `layout`, see `reorder-terminal-item`). That
 * gives a single, coherent slide with no second animator to fight (the old
 * dnd-kit setup did, which is gone now). A removed row simply unmounts and its
 * neighbours slide up via `layout`.
 *
 * These animate ONLY the chrome (the sidebar row) — never the xterm viewport,
 * which is a hard project rule (animating the terminal content revives the
 * flash/perf problem).
 */
export const itemVariants: Variants = {
  initial: { opacity: 0 },
  animate: { opacity: 1 },
};

/** A snappy spring for the item fade-in + the Reorder layout reflow. */
const MOTION_TRANSITION: Transition = {
  type: "spring",
  stiffness: 500,
  damping: 40,
  mass: 0.6,
};

/** A zero-duration transition so motion settles instantly (reduced motion). */
const INSTANT_TRANSITION: Transition = { duration: 0 };

/**
 * Resolve the item transition honouring the user's reduced-motion preference.
 * When the user prefers reduced motion we return an instant transition so the
 * item still appears/moves correctly (no broken layout) but without the spring.
 * Pure → unit-testable without rendering.
 *
 * @param reduced whether `prefers-reduced-motion` is set (from `useReducedMotion`).
 */
export function itemTransition(reduced: boolean | null): Transition {
  return reduced ? INSTANT_TRANSITION : MOTION_TRANSITION;
}
