import type { Transition, Variants } from "motion/react";

/**
 * Motion for the sidebar's COLLAPSIBLE sections (a project's workspace list, a
 * workspace's typed subsections). A clean height+opacity expand/collapse, the
 * variant-A spine's only positional chrome animation.
 *
 * Distinct from `item-motion.ts` (which owns the terminal ROWS, where positional
 * motion is delegated to Reorder): here there is no reorder, so the section owns
 * its own enter/exit via `AnimatePresence` + an animated `height: auto`. As with
 * every animation in nyx this touches ONLY chrome — never the xterm viewport.
 */

/**
 * Expand/collapse variants for a collapsible region. `collapsed` clamps height
 * to 0 and fades out (used as both the `initial` and the `exit` state so a
 * region animates symmetrically on mount/unmount); `expanded` animates to the
 * content's natural height.
 */
export const sectionVariants: Variants = {
  collapsed: { height: 0, opacity: 0 },
  expanded: { height: "auto", opacity: 1 },
};

/** A snappy spring, matching the terminal-row reflow feel (item-motion). */
const SECTION_TRANSITION: Transition = {
  type: "spring",
  stiffness: 500,
  damping: 40,
  mass: 0.6,
};

/** Zero-duration transition for `prefers-reduced-motion` (no spring, no jank). */
const INSTANT_TRANSITION: Transition = { duration: 0 };

/**
 * Resolve the section transition honouring the user's reduced-motion preference.
 * Pure → unit-testable without rendering (mirrors `itemTransition`).
 *
 * @param reduced whether `prefers-reduced-motion` is set (from `useReducedMotion`).
 */
export function sectionTransition(reduced: boolean | null): Transition {
  return reduced ? INSTANT_TRANSITION : SECTION_TRANSITION;
}
