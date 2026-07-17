import { useEffect, useState, type ReactNode } from "react";
import { motion, useReducedMotion } from "motion/react";

import { cn } from "@/lib/utils";

import {
  OVERLAY_ACTIVE_INDICATOR_LAYOUT_ID,
  overlayArrivalTransition,
  overlayArrivalVariants,
  overlayMotionTransition,
  overlayTabVariants,
} from "./overlay-motion";

/**
 * `<OverlayTab>` — the SHARED closed-tab LANGUETTE of the terminal↔Spec overlay: a thin vertical
 * strip flush to the terminal's right edge. Both the RAIL (A) and the TIROIR (B) build their closed
 * onglet from it, so the closed chrome is COHERENT across the two overlays (PRD: "chrome d'onglet
 * fermé cohérent A↔B"). The LOOK is all design-system tokens (card surface, border, radius, hover,
 * focus ring); the CONTENT (a rail gauge+pips, or a drawer name+point) is the caller's children.
 *
 * MOTION (review finding — "languette animée cohérente avec la sidebar"): it is a `motion.button`
 * that ATTACHES/slides in from the right edge with the sidebar spring (`overlay-motion`, the SAME
 * feel as `ui/sidebar-shell`'s collapsible rail) and flashes a one-shot ARRIVAL ring on mount so a
 * newly-mapped Spec is noticed. Hover is a pure COLOUR highlight (`hover:bg-*`) — NO horizontal
 * translate, so hovering never opens a gap between the languette and the fused panel (review finding:
 * "hover sans décalage"). All of it is `motion-safe` — `useReducedMotion` collapses the springs to
 * instant and drops the ring.
 *
 * ACTIVE vs INACTIVE (model A — languette-poignée à gauche du panneau):
 *  - ACTIVE (its panel is OPEN): the languette FUSES with the panel to its right — it shares the
 *    panel's surface (`bg-card/95`) and DROPS its border + rounding on the SHARED right edge
 *    (`border-r-0`), carries a coloured ACCENT BAR on its OUTER (left) edge + VIVID bold text so the
 *    open Spec is obvious at a glance. The accent bar is a `motion.span` sharing a single
 *    `layoutId` (`OVERLAY_ACTIVE_INDICATOR_LAYOUT_ID`) — rendered only here, so on a Spec SWITCH
 *    Motion FLIPs it GLIDING to the newly-active languette in sync with the panel swap (it does not
 *    jump). It stays mounted as the panel's close HANDLE (re-click ⇒ close).
 *  - INACTIVE (closed): a DIMMED, fully-bordered languette (`bg-muted/80`, visible right border,
 *    greyed `text-foreground/70`, no accent bar) that clearly reads "fermée" — the cue for WHICH
 *    Spec is currently open in a stack.
 */
export function OverlayTab({
  onClick,
  label,
  active = false,
  className,
  children,
}: {
  onClick: () => void;
  /** Accessible label (the tab has no visible text on the rail). */
  label: string;
  /** Whether this tab's panel is currently open (a pressed/selected state). */
  active?: boolean;
  className?: string;
  children: ReactNode;
}) {
  const reduced = useReducedMotion();

  // One-shot ARRIVAL highlight: every freshly-MOUNTED languette flashes a brief ring, then never
  // again for the life of that tab. A tab is keyed by its Spec id, so a persistent Spec mounts once
  // (pulses once); a NEW Spec mapped into the session mounts a NEW tab → its own arrival pulse.
  // `useReducedMotion` is `null` on the first render then resolves, so we start the ring ON and
  // retire it either after its window OR immediately once reduced-motion resolves true (motion-safe).
  const [arrived, setArrived] = useState(true);
  useEffect(() => {
    if (reduced) {
      setArrived(false);
      return;
    }
    const t = setTimeout(() => setArrived(false), 1000);
    return () => clearTimeout(t);
  }, [reduced]);

  return (
    <motion.button
      type="button"
      onClick={onClick}
      aria-label={label}
      aria-pressed={active}
      data-overlay-tab=""
      variants={overlayTabVariants}
      initial="initial"
      animate="animate"
      exit="exit"
      transition={overlayMotionTransition(reduced)}
      className={cn(
        "pointer-events-auto relative flex w-9 shrink-0 flex-col items-center gap-2 rounded-l-lg border border-border/70 py-3 shadow-md backdrop-blur-sm transition-colors",
        "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/50",
        active
          ? // FUSED to the open panel: shares its surface + drops the border/rounding on the shared
            // (right) edge, VIVID bold text — reads as an onglet collé à son contenu et OUVERT.
            "border-r-0 bg-card/95 font-semibold text-foreground"
          : // Closed: dimmed, fully-bordered (visible right border), greyed text. Hover LIGHTENS the
            // surface in place (no translate) so the fusion with the panel never breaks.
            "bg-muted/80 text-foreground/70 hover:bg-muted hover:text-foreground",
        className,
      )}
    >
      {children}
      {/* ACTIVE accent bar on the OUTER (left) edge — the validated model-A repère. A single
          `layoutId` element rendered only on the active languette, so Motion GLIDES it to the next
          languette on a Spec switch (FLIP), instant under reduced motion. */}
      {active ? (
        <motion.span
          aria-hidden
          data-overlay-active-indicator=""
          data-layout-id={OVERLAY_ACTIVE_INDICATOR_LAYOUT_ID}
          layoutId={OVERLAY_ACTIVE_INDICATOR_LAYOUT_ID}
          transition={overlayMotionTransition(reduced)}
          className="pointer-events-none absolute inset-y-2 left-0 w-0.5 rounded-full bg-primary"
        />
      ) : null}
      {arrived ? (
        <motion.span
          aria-hidden
          data-overlay-arrival=""
          className="pointer-events-none absolute inset-0 rounded-l-lg ring-2 ring-primary/60"
          variants={overlayArrivalVariants}
          initial="initial"
          animate="animate"
          transition={overlayArrivalTransition(reduced)}
        />
      ) : null}
    </motion.button>
  );
}
