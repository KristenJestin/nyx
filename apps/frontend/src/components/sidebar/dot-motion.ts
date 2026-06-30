import type { Transition } from "motion/react";

/**
 * Shared motion for the RUN-STATE dots/badges (the `<StatusDot>` +
 * `<TerminalStateBadge>` here, and `<CommandStateDot>` in the command view). One
 * module so the three primitives animate IDENTICALLY — appear, disappear, and the
 * colour transition between states all read the same.
 *
 * Chrome-only — never the xterm viewport (a hard project rule).
 *
 * COLOUR TRANSITION = CROSS-FADE, not an animated `backgroundColor`. The dot fills
 * are design-system TOKENS (`--info`/`--warning`/`--success`/`--destructive`/
 * `--muted-foreground`), written in `oklch(...)`. Tweening `backgroundColor` would
 * force resolving each token to an sRGB value at runtime (the chroma-js dance the
 * xterm theme does in `terminal.tsx`) — heavy and fragile. Instead we keep the
 * Tailwind token CLASSES and cross-fade two stacked dots keyed on the state: the
 * outgoing colour fades out as the incoming colour fades in. Going to/from the
 * yellow `waiting` fill is then free — it is just another keyed colour.
 */

/** APPEAR/DISAPPEAR target: the dot pops in (spring scale + fade) and fades+scales out. */
export const DOT_PRESENCE = {
  initial: { scale: 0, opacity: 0 },
  animate: { scale: 1, opacity: 1 },
  exit: { scale: 0, opacity: 0 },
} as const;

/** The snappy overshoot spring for a dot's appear pop (matches the proto). */
const DOT_APPEAR_SPRING: Transition = { type: "spring", stiffness: 700, damping: 22 };

/** A short tween for the disappear (a clean fade-out, no bouncy overshoot on exit). */
const DOT_EXIT_TWEEN: Transition = { duration: 0.15, ease: [0.4, 0, 1, 1] };

/** Zero-duration transition for `prefers-reduced-motion` (instant snap, no motion). */
const INSTANT_TRANSITION: Transition = { duration: 0 };

/**
 * Resolve the dot's APPEAR transition (the spring pop), honouring reduced motion.
 * Reduced ⇒ instant snap. Pure → unit-testable without rendering.
 */
export function dotAppearTransition(reduced: boolean | null): Transition {
  return reduced ? INSTANT_TRANSITION : DOT_APPEAR_SPRING;
}

/**
 * Resolve the dot's DISAPPEAR/EXIT transition, honouring reduced motion. Reduced ⇒
 * instant (the node leaves without a fade). Pure → unit-testable without rendering.
 */
export function dotExitTransition(reduced: boolean | null): Transition {
  return reduced ? INSTANT_TRANSITION : DOT_EXIT_TWEEN;
}

/**
 * The cross-fade duration for a colour SWAP between two live states (e.g.
 * `running`→`waiting`, or a settled `success`→`error`). Short enough to read as a
 * transition, not a flash. Reduced motion ⇒ 0 (instant snap to the new colour).
 *
 * @param reduced whether `prefers-reduced-motion` is set (from `useReducedMotion`).
 */
export function dotCrossfadeTransition(reduced: boolean | null): Transition {
  return reduced ? INSTANT_TRANSITION : { duration: 0.18, ease: "easeInOut" };
}
