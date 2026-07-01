import type { Transition, Variants } from "motion/react";

/**
 * Motion for the `Tabs` panel SWITCH (the Commands/Import cross-fade). Mirrors the
 * sidebar's `section-motion` / `item-motion` convention: variants + a transition
 * resolver that collapses to instant under `prefers-reduced-motion`.
 *
 * The active panel's content cross-fades with a small horizontal slide: the
 * outgoing panel fades out (and drifts a touch left) while the incoming one fades
 * in from a touch right, run with `AnimatePresence mode="wait"` so the switch is a
 * single clean pass (out, then in) — never two panels overlapping. Chrome-only;
 * never the xterm viewport.
 */

/** Small horizontal slide distance (px) the panel content drifts on enter/exit. */
const PANEL_SLIDE = 8;

/**
 * Enter/exit variants for a tab panel's content. `enter` is the resting state;
 * `initial` fades in from the right, `exit` fades out to the left (a directional
 * cross-fade that reads as a forward switch).
 */
export const tabPanelVariants: Variants = {
  initial: { opacity: 0, x: PANEL_SLIDE },
  enter: { opacity: 1, x: 0 },
  exit: { opacity: 0, x: -PANEL_SLIDE },
};

/** A short ease for the panel cross-fade (matches the modal's snappy chrome feel). */
const PANEL_TRANSITION: Transition = { duration: 0.16, ease: [0.4, 0, 0.2, 1] };

/** Zero-duration transition for `prefers-reduced-motion` (no slide, no fade jank). */
const INSTANT_TRANSITION: Transition = { duration: 0 };

/**
 * Resolve the panel-switch transition honouring the user's reduced-motion
 * preference. Pure → unit-testable without rendering (mirrors `itemTransition` /
 * `sectionTransition`).
 *
 * @param reduced whether `prefers-reduced-motion` is set (from `useReducedMotion`).
 */
export function tabPanelTransition(reduced: boolean | null): Transition {
  return reduced ? INSTANT_TRANSITION : PANEL_TRANSITION;
}

/**
 * A snappy spring for the panel CONTAINER's HEIGHT when the active tab's content
 * is taller/shorter than the previous one's — so the modal grows/shrinks smoothly
 * instead of snapping to the new content height instantly (review finding). Mirrors
 * `section-motion`'s collapse spring so the height feel is consistent with the
 * sidebar's `CollapsibleSection`.
 */
const HEIGHT_TRANSITION: Transition = {
  type: "spring",
  stiffness: 500,
  damping: 40,
  mass: 0.6,
};

/**
 * Resolve the tab-container height transition honouring reduced motion (instant
 * when set, so the swap stays correct but skips the tween). Pure → unit-testable.
 *
 * @param reduced whether `prefers-reduced-motion` is set (from `useReducedMotion`).
 */
export function tabHeightTransition(reduced: boolean | null): Transition {
  return reduced ? INSTANT_TRANSITION : HEIGHT_TRANSITION;
}
