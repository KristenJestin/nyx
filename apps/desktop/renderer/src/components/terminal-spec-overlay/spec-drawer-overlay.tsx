import { useRef, useState } from "react";
import { AnimatePresence, motion, useReducedMotion } from "motion/react";

import { cn } from "@/lib/utils";
import { OVERLAY_SURFACE_Z } from "@/components/panes/pane-geometry";

import type { Spec } from "../specs/types";
import { OverlayTab } from "./overlay-tab";
import {
  overlayMotionTransition,
  overlayPanelVariants,
  overlayPanelWidth,
  overlaySurfaceVariants,
} from "./overlay-motion";
import { drawerStatusPoint } from "./overlay-status";
import { SpecLivePanel } from "./spec-live-panel";
import {
  overlayPanelId,
  useOverlayPaneWidth,
  useOverlayPanelDisclosure,
} from "./use-overlay-panel";

/** Every stacked drawer tab is the SAME fixed size (PRD: taille FIXE par onglet), so N Specs
 *  read as a uniform pile and the cap+scroll math is stable. */
const DRAWER_TAB_HEIGHT = "h-36";

/**
 * One SOBER closed tab of the creation tiroir: a vertical Spec name + a SINGLE status point.
 * Nothing else — no gauge, %, check or typing glyph (PRD round v6: sober onglets). Built on the
 * SHARED `<OverlayTab>` chrome so the closed onglet is COHERENT with the rail A tab. Fixed height
 * so a stack of N tabs is uniform.
 */
function DrawerTab({
  spec,
  live,
  active,
  onClick,
}: {
  spec: Spec;
  live: boolean;
  active: boolean;
  onClick: () => void;
}) {
  const point = drawerStatusPoint(spec, live);
  const name = spec.title || `#${spec.number}`;
  return (
    <OverlayTab
      label={`Open ${name} live spec`}
      // The DISCLOSURE target: this languette's `aria-controls` and ITS panel's `id` are the same
      // Spec-derived string, so they can never drift (`use-overlay-panel`).
      panelId={overlayPanelId(spec.id)}
      active={active}
      onClick={onClick}
      className={DRAWER_TAB_HEIGHT}
    >
      {/* 1. Vertical Spec name (short). */}
      <span
        className="min-h-0 flex-1 truncate text-2xs font-medium text-foreground/80 [writing-mode:vertical-rl]"
        style={{ maxHeight: "10rem" }}
        title={name}
      >
        {name}
      </span>
      {/* 2. One status point at the bottom — coloured by the GLOBAL statusMeta (same as the panel's
             status badge); a live draft still pulses (vivacité), but the colour is the status colour. */}
      <span
        aria-label={point.label}
        className={cn(
          "size-1.5 shrink-0 rounded-full",
          point.tone,
          point.pulse && "motion-safe:animate-pulse",
        )}
      />
    </OverlayTab>
  );
}

/**
 * `<SpecDrawerOverlay>` — the CREATION live overlay (tiroir B). Anchored to the terminal's right
 * edge, pointer-transparent except for its own chrome.
 *
 *  - CLOSED (aucun panneau ouvert): a STACK of sober tabs — one per authored Spec (vertical name +
 *    a single status point), built on the shared `<OverlayTab>` chrome so it is coherent with the
 *    rail A tab. The stack is flush to the terminal's RIGHT edge, each tab a FIXED size, capped to
 *    the pane height and scrolling vertically beyond it (a session cutting a feature into many Specs
 *    never overflows).
 *  - OPEN: the selected Spec's full live drawer (`<SpecLivePanel>`) docks FLUSH to the terminal's
 *    right edge; the tab column is pushed to its LEFT and the open Spec's languette
 *    FUSES with the panel (shared surface, no border on the shared edge) while the others stay as
 *    dimmed closed tabs beside it. Only ONE panel open at a time — clicking another switches it.
 *
 * LAYOUT (model A — sidebar-shell idiom): the panel lives in a width-animated SHELL whose outer
 * width springs between `0` (closed) and `overlayPanelWidth(pane)` (open) — the width BOUNDED on the
 * host pane, `min(380, max(45% of pane, 260))` — `overflow-hidden` over an inner FROZEN at that same
 * pixel width so nothing reflows. Because the assembly is right-packed (`justify-end`), the shell's
 * growing width pushes the languette column LEFT — the languettes "suivent" the panel. The panel
 * remains SUPERPOSED over the terminal: the pane is never resized, so opening it triggers no xterm
 * refit and no PTY SIGWINCH.
 *
 * This multi-Spec stacking is the CREATION-only affordance (PRD): the rail A (dev) is always a
 * single Spec with no name and no stacking. Placement = prototype `spec-live` round v6.
 */
export function SpecDrawerOverlay({ specs, live }: { specs: readonly Spec[]; live: boolean }) {
  const [openSpecId, setOpenSpecId] = useState<string | null>(null);
  const reduced = useReducedMotion();

  // Only a Spec still in the creation set can be open (it drops out when it moves to dev / done).
  const openSpec = specs.find((s) => s.id === openSpecId) ?? null;

  // The NON-MODAL disclosure behaviour, shared with the rail (`use-overlay-panel`): focus INTO the
  // panel on open (and again on a Spec SWITCH, which remounts it), focus RESTORED to the languette
  // of the Spec that was open, panel-scoped Escape and outside-click dismissal.
  const rootRef = useRef<HTMLDivElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  const { dismiss, onPanelKeyDown } = useOverlayPanelDisclosure({
    open: openSpec !== null,
    panelId: openSpec === null ? null : overlayPanelId(openSpec.id),
    rootRef,
    panelRef,
    onDismiss: () => setOpenSpecId(null),
  });

  // Panel width BOUNDED on the host pane (`min(380, max(45% of pane, 260))`), remeasured on a pane
  // resize and resolved to a NUMBER — the shell animates to it, the inner box is frozen at it. The
  // pane is never reflowed (the panel stays superposed ⇒ no xterm refit, no SIGWINCH).
  const panelWidth = overlayPanelWidth(useOverlayPaneWidth(rootRef));

  if (specs.length === 0) return null;

  // Re-clicking the ACTIVE languette closes its panel (through `dismiss`, so focus comes back to it);
  // clicking ANOTHER switches to it (a single state transition — the outside-click listener excludes
  // the overlay's whole surface, languettes included, precisely so a switch never reads as
  // close-then-reopen).
  const toggle = (id: string) => (id === openSpecId ? dismiss() : setOpenSpecId(id));

  return (
    <motion.div
      ref={rootRef}
      data-overlay-surface=""
      variants={overlaySurfaceVariants}
      initial="initial"
      animate="animate"
      exit="exit"
      transition={overlayMotionTransition(reduced)}
      // The stack order is {@link OVERLAY_SURFACE_Z}, not a literal: this surface is pinned to the
      // pane's RIGHT EDGE, which for any pane that is not the rightmost IS a separator band, and
      // the languette has to win that overlap while still losing to the control cluster.
      style={{ zIndex: OVERLAY_SURFACE_Z }}
      className="pointer-events-none absolute inset-y-0 right-0 flex items-stretch justify-end"
    >
      {/* The STACK of sober tabs sits to the LEFT of the panel and STAYS as N Specs are mapped/opened
          (the open one becomes the ACTIVE fused languette — it is NOT dropped). Fixed-size tiles,
          capped to the pane height + scrolling beyond it so N Specs never overflow. Centered
          vertically; `<AnimatePresence>` runs each tab's enter/leave.

          TWO nested boxes, because the full-height WRAPPER and the SCROLLER are two different
          surfaces with opposite pointer needs:
           - the OUTER wrapper is what `items-stretch` on the `inset-y-0` root blows up to the FULL
             pane height while `justify-center` parks the stack in its middle. Its empty zones
             above/below the stack cover the TERMINAL's own scroll-to-bottom button (`terminal.tsx`,
             `right-3 bottom-3 z-10`) and the global 0.5rem scrollbar gutter (`globals.css`, `*`) —
             none of which belongs to the overlay, so it is `pointer-events-none` (click-through).
           - the INNER box is the SCROLLER (`overflow-y-auto`): it hugs the stack and, once N Specs
             overflow the pane, paints its OWN 0.5rem scrollbar inside the 36px band. That bar IS an
             overlay control, and in Chromium a `pointer-events: none` box is not hit-testable —
             scrollbar included — so the scroller keeps `pointer-events-auto` and its thumb stays
             draggable. Sizing it to the stack (instead of to the pane) is what keeps the empty
             zones click-through. Being INSIDE `[data-overlay-surface]`, this box is overlay chrome
             for the outside-click rule too (`use-overlay-panel`): a mousedown on its bar, its thumb
             or a `gap-2` gutter between languettes does NOT dismiss the open panel — dragging the
             pile that COMMANDS the panel must not close it.
          The split carries a SECOND, independent reason: `justify-center` and `overflow-y-auto` on
          the SAME box make the TOP OF AN OVERFLOWING STACK UNREACHABLE. A centred box distributes
          its overflow symmetrically around the middle, but scrolling only ever exposes what lies
          BELOW the origin, so the first languettes end up ABOVE `scrollTop === 0` with no gesture
          that brings them back (measured on this 320px pane at n=6: the first languette sat at
          `top = -292px` while `scrollTop` was already 0, and `maxScrollTop` was 296 instead of 592 —
          exactly half the pile, permanently out of reach). Centring on the OUTER wrapper and
          scrolling on the INNER box that HUGS the stack is what makes the pile start at its own
          origin. So the ban on recollapsing this pair carries BOTH reasons, and the overflow test of
          `overlay-hit-testing.browser.test.tsx` pins this one (first languette fully inside the
          scroller at `scrollTop === 0`).
          Each `<OverlayTab>` also carries its own `pointer-events-auto` (name + status point inherit
          it as children of the button), so a languette stays clickable whatever the wrapper does.
          The sibling overlay `spec-rail-overlay` deliberately keeps ONE box for the same job: it
          holds a single languette and is NOT a scroll container, so it has no bar to make hittable
          and nothing to re-arm beyond the tab. The asymmetry is the point — do NOT collapse this
          pair into one box to match it.

          KNOWN BOUNDARY of the click-through guarantee (measured in browser mode with
          `document.elementFromPoint` at the PRD's repro coordinates): the 36px band is traversable
          only WHILE THE STACK DOES NOT FILL THE PANE. A tile is 152px tall (`h-36` + `gap-2` + the
          scroller's `py-1`), so on a 700px pane n=3 (456px) and n=4 (608px) let all four right-edge
          targets through, while from n=5 (760px) the scroller — `max-h-full`, i.e. the WHOLE pane —
          covers the band with opaque languettes and the PRD's repro comes back verbatim (the
          terminal's scroll-to-bottom button and the underlay's scrollbar track become unclickable
          again). On the 320px pane of `overlay-hit-testing.browser.test.tsx` the threshold is n=3,
          and n=2 is already enough to cover the scroll-to-bottom button.
          That residual is the OTHER half of the frozen decision ("visible controls stay
          `pointer-events-auto`"), not a cost of the two-box split: forcing the scroller itself to
          `pointer-events-none` yields IDENTICAL labels past the threshold — the blocker is the
          opaque `w-9` languette, not the boxes around it. Capping the scroller's height to keep a
          traversable strip at the bottom of the pane would be a NEW scroll behaviour, out of this
          PRD's scope (parked as idea `01M034FQKH1C2F1BWSKGG4DCKS`). The overflow test of
          `overlay-hit-testing.browser.test.tsx` pins this boundary as a characterisation test, so a
          change of state there goes red instead of unnoticed. */}
      <div className="pointer-events-none flex max-h-full min-h-0 flex-col justify-center">
        <div
          aria-label="Live specs"
          className="pointer-events-auto flex max-h-full min-h-0 flex-col gap-2 overflow-y-auto py-1"
        >
          <AnimatePresence initial={false}>
            {specs.map((spec) => (
              <DrawerTab
                key={spec.id}
                spec={spec}
                live={live}
                active={spec.id === openSpecId}
                // Open THIS Spec's drawer, switch to it, or (if already active) close it.
                onClick={() => toggle(spec.id)}
              />
            ))}
          </AnimatePresence>
        </div>
      </div>
      {/* WIDTH-ANIMATED SHELL (sidebar-shell idiom): outer width springs 0↔`panelWidth` so the tab column to
          its left "suit" the panel; `overflow-hidden` clips the FIXED-width inner so nothing reflows
          and no horizontal overflow escapes mid-animation. `shadow-xl` (only when open) defines the
          panel's left edge in the gaps above/below the languettes — the panel carries NO left border
          so the fused active languette reads seamless. */}
      <motion.div
        data-overlay-panel-shell=""
        initial={false}
        animate={{ width: openSpec ? panelWidth : 0 }}
        transition={overlayMotionTransition(reduced)}
        className={cn(
          "pointer-events-none flex h-full justify-end overflow-hidden",
          openSpec && "shadow-xl",
        )}
      >
        <div
          data-overlay-panel-inner=""
          className="relative h-full shrink-0"
          style={{ width: panelWidth }}
        >
          {/* Keyed by `spec.id` (NOT a stable key): switching Specs REMOUNTS the panel so the FULL
              exit+enter crossfade replays (l'ancien s'efface, le nouveau apparaît). NO `mode="wait"` —
              the default (sync) mode lets exit+enter CHEVAUCHER in this shared `absolute inset-0` slot:
              the new opaque `bg-card/95` panel rises ON TOP (its `zIndex` in `overlayPanelVariants`) as
              its opacity climbs WHILE the old one fades out — the shell width is unchanged throughout, so
              it is NEVER momentarily empty (review finding: "frame VIDE au switch"). The panel is
              OPACITY-ONLY (no horizontal slide): its open/close MOVEMENT is driven by the shell width
              (0↔`panelWidth`) so the content stays flush to the left edge and the fused languette stays COLLÉE. */}
          <AnimatePresence>
            {openSpec ? (
              <motion.div
                key={openSpec.id}
                id={overlayPanelId(openSpec.id)}
                ref={panelRef}
                data-overlay-panel=""
                data-overlay-panel-spec={openSpec.id}
                variants={overlayPanelVariants}
                initial="initial"
                animate="animate"
                exit="exit"
                transition={overlayMotionTransition(reduced)}
                className="absolute inset-0 focus:outline-none"
                // NON-MODAL dialog (decided): the role, the NAME and the focus target all live on
                // THIS box — the same element that carries `data-overlay-panel` and the id the
                // languette points at. `<SpecLivePanel>` inside is therefore a plain box: naming it
                // too would nest two named regions for one panel. No `aria-modal`, no focus trap.
                role="dialog"
                aria-label={`${openSpec.title || `#${openSpec.number}`} live spec`}
                tabIndex={-1}
                onKeyDown={onPanelKeyDown}
              >
                <SpecLivePanel spec={openSpec} onClose={dismiss} />
              </motion.div>
            ) : null}
          </AnimatePresence>
        </div>
      </motion.div>
    </motion.div>
  );
}
