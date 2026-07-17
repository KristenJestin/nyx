import type { Transition, Variants } from "motion/react";

/**
 * Motion vocabulary for the terminal↔Spec overlay (rail A + tiroir B). Mirrors the app's
 * established convention (`sidebar/item-motion`, `ui/tabs-motion`, `ui/dialog-motion`): exported
 * VARIANTS + PURE transition RESOLVERS that collapse to an instant transition under
 * `prefers-reduced-motion`, so the motion is unit-testable WITHOUT rendering.
 *
 * Chrome-only — the overlay chrome floats OVER the xterm viewport; it never wraps or animates the
 * terminal surface itself (a hard project rule). The spring is the SAME feel as
 * `ui/sidebar-shell`'s collapsible rail (`{ type: "spring", duration: 0.4, bounce: 0.15 }`), so the
 * overlay "suit la sidebar" (review finding — the languette must read like the sidebar's animation).
 */

/**
 * The OPEN panel's fixed width (px). The rail (A) / tiroir (B) both dock a `w-[380px]` panel to the
 * terminal's right edge; the width-animated SHELL (sidebar-shell idiom) animates its outer width
 * between `0` (closed) and this value (open), so the languette column to its LEFT "suit" the panel
 * as it opens/closes. Shared so the animate target and the fixed inner width never drift apart.
 */
export const OVERLAY_PANEL_WIDTH = 380;

/** Horizontal travel (px) the surface slides in from the terminal's right edge. */
const SURFACE_SLIDE = 24;
/** Horizontal travel (px) the closed languette attaches from the right edge. */
const TAB_SLIDE = 10;

/**
 * The whole overlay SURFACE (rail A / tiroir B). Slides IN from the terminal's right edge + fades
 * when a Spec is MAPPED to the session; reverses (slide-OUT to the right + fade) when the Spec is
 * UN-mapped (`/clear`). Driven by an `<AnimatePresence>` at the overlay root.
 */
export const overlaySurfaceVariants: Variants = {
  initial: { opacity: 0, x: SURFACE_SLIDE },
  animate: { opacity: 1, x: 0 },
  exit: { opacity: 0, x: SURFACE_SLIDE },
};

/**
 * The OPEN PANEL of the rail (A) / tiroir (B) — the wide Spec panel revealed when the languette is
 * CLICKED. It is OPACITY-ONLY: the panel's MOVEMENT (open/close collapse-expand) is driven ENTIRELY
 * by the width-animated SHELL (`0↔380`, sidebar-shell idiom), so the panel content stays FLUSH to the
 * shell's left edge for the whole animation and the fused active languette "suit" it and stays COLLÉE
 * (no horizontal `x` slide that would decouple the content from the shell's edge and open a hole /
 * read as a "pop" on close — the regression this fix removes). This shares the SAME sidebar spring as
 * the surface/languette (`overlayMotionTransition`) via the wrapper; `prefers-reduced-motion`
 * collapses it to instant via that shared resolver. This is DISTINCT from the surface's own arrival
 * slide (Spec mapped / `clear`, `overlaySurfaceVariants` — untouched) and from the languette's
 * arrival ring/pulse.
 *
 * SWITCH OVERLAP (review finding — "frame VIDE au switch"): on a Spec→Spec switch the shell width
 * stays `380` (a Spec remains open) and the panel's `<AnimatePresence>` runs in its default (sync)
 * mode — NOT `mode="wait"` — so the exiting and entering panels CHEVAUCHENT in the same
 * `absolute inset-0` slot instead of serializing. The `zIndex` keeps the ENTERING panel ABOVE the
 * exiting one (`initial`/`animate` = 1, `exit` = 0), so the incoming opaque `bg-card/95` surface rises
 * ON TOP as its opacity climbs — the shell is NEVER momentarily empty and the switch reads as a clean
 * OPACITY crossfade of CONTENT, never a hole. No horizontal slide: a light VERTICAL `y` would be
 * acceptable texture (it never opens a left-edge gap), but the panel keeps a pure crossfade here.
 */
export const overlayPanelVariants: Variants = {
  initial: { opacity: 0, zIndex: 1 },
  animate: { opacity: 1, zIndex: 1 },
  exit: { opacity: 0, zIndex: 0 },
};

/**
 * The SHARED closed LANGUETTE (`<OverlayTab>`, rail + tiroir). Attaches/slides from the right edge
 * with the sidebar spring, so a closed tab reads as a soigné languette rather than a static strip.
 */
export const overlayTabVariants: Variants = {
  initial: { opacity: 0, x: TAB_SLIDE },
  animate: { opacity: 1, x: 0 },
  exit: { opacity: 0, x: TAB_SLIDE },
};

/**
 * The SHARED `layoutId` carried by the ACTIVE languette's accent bar (the fused "repère" of model
 * A). Rendered ONLY on the languette whose panel is open, so when the active Spec SWITCHES, Motion
 * FLIPs this single indicator from the old languette to the new one — the accent bar GLIDES to the
 * new position in sync with the panel swap (review finding: "le repère actif doit SUIVRE le switch",
 * not jump). Same shared-layout idiom as `ui/tabs`' underline + the sidebar's `<ActiveRail>`; the
 * travel collapses to instant under `prefers-reduced-motion` via `overlayMotionTransition`. Only ONE
 * overlay (rail A OR tiroir B) is mounted at a time (single active terminal), so a constant id shares
 * cleanly across a stack's languettes without colliding.
 */
export const OVERLAY_ACTIVE_INDICATOR_LAYOUT_ID = "overlay-active-tab";

/**
 * The one-shot ARRIVAL highlight for a NEWLY-mounted languette (a freshly-mapped Spec, or a new
 * tab appearing in the stack): a brief ring that fades out so the user NOTICES it. It is distinct
 * from the status point's continuous "authoring live" breath — that stays on the tiny point
 * (`motion-safe:animate-pulse`), whereas THIS is a single-pass ring on the whole tab — so the two
 * signals never read as the same thing.
 */
export const overlayArrivalVariants: Variants = {
  initial: { opacity: 0.7 },
  animate: { opacity: 0 },
};

/** The sidebar-consistent spring shared by the surface + languette (see module note). */
const SPRING: Transition = { type: "spring", duration: 0.4, bounce: 0.15 };
/** The one-shot arrival ring's fade-out ramp. */
const ARRIVAL: Transition = { duration: 0.9, ease: [0.4, 0, 0.2, 1] };
/** Zero-duration transition for `prefers-reduced-motion` (settles correctly, no tween). */
const INSTANT: Transition = { duration: 0 };

/** Resolve the surface/languette spring, honouring `prefers-reduced-motion`. Pure → testable. */
export function overlayMotionTransition(reduced: boolean | null): Transition {
  return reduced ? INSTANT : SPRING;
}

/** Resolve the arrival ring's transition, honouring `prefers-reduced-motion`. Pure → testable. */
export function overlayArrivalTransition(reduced: boolean | null): Transition {
  return reduced ? INSTANT : ARRIVAL;
}
