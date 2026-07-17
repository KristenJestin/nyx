import { useState } from "react";
import { AnimatePresence, motion, useReducedMotion } from "motion/react";

import { cn } from "@/lib/utils";

import type { Spec } from "../specs/types";
import { OverlayTab } from "./overlay-tab";
import {
  OVERLAY_PANEL_WIDTH,
  overlayMotionTransition,
  overlayPanelVariants,
  overlaySurfaceVariants,
} from "./overlay-motion";
import { drawerStatusPoint } from "./overlay-status";
import { SpecLivePanel } from "./spec-live-panel";

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
 *    right edge (`w-[380px]`); the tab column is pushed to its LEFT and the open Spec's languette
 *    FUSES with the panel (shared surface, no border on the shared edge) while the others stay as
 *    dimmed closed tabs beside it. Only ONE panel open at a time — clicking another switches it.
 *
 * LAYOUT (model A — sidebar-shell idiom): the panel lives in a width-animated SHELL whose outer
 * width springs between `0` (closed) and `OVERLAY_PANEL_WIDTH` (open), `overflow-hidden` over a
 * FIXED-width inner so nothing reflows. Because the assembly is right-packed (`justify-end`), the
 * shell's growing width pushes the languette column LEFT — the languettes "suivent" the panel.
 *
 * This multi-Spec stacking is the CREATION-only affordance (PRD): the rail A (dev) is always a
 * single Spec with no name and no stacking. Placement = prototype `spec-live` round v6.
 */
export function SpecDrawerOverlay({ specs, live }: { specs: readonly Spec[]; live: boolean }) {
  const [openSpecId, setOpenSpecId] = useState<string | null>(null);
  const reduced = useReducedMotion();

  // Only a Spec still in the creation set can be open (it drops out when it moves to dev / done).
  const openSpec = specs.find((s) => s.id === openSpecId) ?? null;

  if (specs.length === 0) return null;

  // Re-clicking the ACTIVE languette closes its panel; clicking ANOTHER switches to it.
  const toggle = (id: string) => setOpenSpecId((cur) => (cur === id ? null : id));

  return (
    <motion.div
      data-overlay-surface=""
      variants={overlaySurfaceVariants}
      initial="initial"
      animate="animate"
      exit="exit"
      transition={overlayMotionTransition(reduced)}
      className="pointer-events-none absolute inset-y-0 right-0 z-20 flex items-stretch justify-end"
    >
      {/* The STACK of sober tabs sits to the LEFT of the panel and STAYS as N Specs are mapped/opened
          (the open one becomes the ACTIVE fused languette — it is NOT dropped). Fixed-size tiles,
          capped to the pane height + scrolling beyond it so N Specs never overflow. Centered
          vertically; `<AnimatePresence>` runs each tab's enter/leave. */}
      <div
        aria-label="Live specs"
        className="pointer-events-auto flex max-h-full flex-col justify-center gap-2 overflow-y-auto py-1"
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
      {/* WIDTH-ANIMATED SHELL (sidebar-shell idiom): outer width springs 0↔380 so the tab column to
          its left "suit" the panel; `overflow-hidden` clips the FIXED-width inner so nothing reflows
          and no horizontal overflow escapes mid-animation. `shadow-xl` (only when open) defines the
          panel's left edge in the gaps above/below the languettes — the panel carries NO left border
          so the fused active languette reads seamless. */}
      <motion.div
        initial={false}
        animate={{ width: openSpec ? OVERLAY_PANEL_WIDTH : 0 }}
        transition={overlayMotionTransition(reduced)}
        className={cn(
          "pointer-events-none flex h-full justify-end overflow-hidden",
          openSpec && "shadow-xl",
        )}
      >
        <div className="relative h-full shrink-0" style={{ width: OVERLAY_PANEL_WIDTH }}>
          {/* Keyed by `spec.id` (NOT a stable key): switching Specs REMOUNTS the panel so the FULL
              exit+enter crossfade replays (l'ancien s'efface, le nouveau apparaît). NO `mode="wait"` —
              the default (sync) mode lets exit+enter CHEVAUCHER in this shared `absolute inset-0` slot:
              the new opaque `bg-card/95` panel rises ON TOP (its `zIndex` in `overlayPanelVariants`) as
              its opacity climbs WHILE the old one fades out — the shell width stays 380 throughout, so
              it is NEVER momentarily empty (review finding: "frame VIDE au switch"). The panel is
              OPACITY-ONLY (no horizontal slide): its open/close MOVEMENT is driven by the shell width
              (0↔380) so the content stays flush to the left edge and the fused languette stays COLLÉE. */}
          <AnimatePresence>
            {openSpec ? (
              <motion.div
                key={openSpec.id}
                data-overlay-panel=""
                data-overlay-panel-spec={openSpec.id}
                variants={overlayPanelVariants}
                initial="initial"
                animate="animate"
                exit="exit"
                transition={overlayMotionTransition(reduced)}
                className="absolute inset-0"
              >
                <SpecLivePanel spec={openSpec} onClose={() => setOpenSpecId(null)} />
              </motion.div>
            ) : null}
          </AnimatePresence>
        </div>
      </motion.div>
    </motion.div>
  );
}
