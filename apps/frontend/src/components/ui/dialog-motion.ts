import type { Transition, Variants } from "motion/react";

/**
 * Motion for the shared `<Dialog>` primitives (backdrop + popup).
 *
 * WHY MOTION AND NOT CSS: the prior dialog animated via Base UI's
 * `data-[starting-style]`/`data-[ending-style]` + a Tailwind CSS transition.
 * That relies on the browser flushing a style change and THEN starting a
 * transition on the next frame — which WebKitGTK (the Tauri WebView on Linux)
 * does not do reliably for a freshly-portaled element, so the modal popped in
 * INSTANTLY in the real release build even though jsdom tests saw the classes
 * (finding 01KV1NPNGBACH0FY982QQN6ZZ2). Motion animates imperatively (Web
 * Animations API / rAF-driven values), so the enter/exit is real in WebKitGTK.
 *
 * Chrome-only — a dialog never wraps the xterm viewport.
 */

/** Backdrop scrim: a plain opacity fade in/out. */
export const backdropVariants: Variants = {
  hidden: { opacity: 0 },
  visible: { opacity: 1 },
};

/**
 * Popup: fades + subtly scales/rises in on enter, reverses on exit. The popup is
 * centered by the className's `-translate-x-1/2 -translate-y-1/2`; we animate an
 * ADDITIONAL `scale`/`y` here (Motion composes its transform on top), so the
 * centering translate is preserved while it eases in.
 */
export const popupVariants: Variants = {
  hidden: { opacity: 0, scale: 0.95, y: 8 },
  visible: { opacity: 1, scale: 1, y: 0 },
};

/** A quick, snappy ease consistent with the app's motion language. */
const DIALOG_TRANSITION: Transition = {
  type: "spring",
  stiffness: 520,
  damping: 38,
  mass: 0.7,
};

/** Zero-duration transition for `prefers-reduced-motion` (no spring, no scale jank). */
const INSTANT_TRANSITION: Transition = { duration: 0 };

/**
 * Resolve the dialog transition honouring `prefers-reduced-motion`. Pure →
 * unit-testable without rendering (mirrors `itemTransition`/`sectionTransition`).
 *
 * @param reduced whether `prefers-reduced-motion` is set (from `useReducedMotion`).
 */
export function dialogTransition(reduced: boolean | null): Transition {
  return reduced ? INSTANT_TRANSITION : DIALOG_TRANSITION;
}
