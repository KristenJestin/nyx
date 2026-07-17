import { useState } from "react";
import { ChevronRightIcon } from "lucide-react";
import { AnimatePresence, motion, useReducedMotion } from "motion/react";

import { cn } from "@/lib/utils";
import { Badge } from "@/components/ui/badge";
import { Spinner } from "@/components/ui/spinner";

import { specKey, statusMeta } from "../specs/spec-meta";
import { TasksTab } from "../specs/tabs/tasks-tab";
import type { Spec } from "../specs/types";
import { useSpecDetail } from "../specs/use-spec-detail";
import { OverlayPanelActions } from "./overlay-panel-actions";
import { OverlayTab } from "./overlay-tab";
import {
  OVERLAY_PANEL_WIDTH,
  overlayMotionTransition,
  overlayPanelVariants,
  overlaySurfaceVariants,
} from "./overlay-motion";
import {
  RAIL_ITEM_WIDTH_PX,
  railFillTone,
  railItemMorph,
  railRollup,
  type RailItem,
  type RailRollup,
} from "./overlay-status";

/**
 * The closed rail tab's CONTENT (Option B sequence + per-status bar). NO Spec name, NO stacking (dev =
 * a single Spec). It renders the rail's ORDERED SEQUENCE (one item per phase, top→bottom in phase
 * order), with the PROGRESS BAR sitting INLINE at the CURRENT phase's position — NOT pinned to the
 * bottom. Each phase is a {@link RailPhaseItem} keyed by its PHASE INDEX (a STABLE key — NOT the
 * `kind`), so when `currentPhaseIndex` moves the SAME element MORPHS in place rather than remounting:
 *  - `done`      → a green (success) DOT for a phase already finished (before the current one).
 *  - `bar`       → the CURRENT phase's PROGRESS BAR (the stretched state) — see {@link RailPhaseItem}.
 *  - `remaining` → a neutral (visible) DOT for a phase still to do (after the current one).
 * As the current phase advances, the finished phase's `kind` flips `bar → done` (its element RETRACTS
 * from bar to dot) while the next phase flips `remaining → bar` (its element STRETCHES from dot to
 * bar) — "the bar descends the timeline". The items stack naturally and the languette's height
 * FOLLOWS their count (content-driven); the list can shrink + scroll (`min-h-0` + `overflow-y-auto`)
 * so a large phase count never squeezes the bar. Colours (see `overlay-status`) stay VISIBLE against
 * the languette AND the scrollbar behind it. `reduced` (from `useReducedMotion`) makes the active
 * zone STATIC (no pulse) AND collapses every animation (morph, zones, dot enter/leave) to INSTANT —
 * honouring reduced-motion.
 *
 * The phase items are wrapped in an `<AnimatePresence initial={false}>` so that when the PHASE COUNT
 * changes (a phase added/removed ⇒ a dot appears/disappears) the item ENTERS (grows in) / EXITS
 * (collapses out) on the shared spring instead of popping, and the neighbours reflow via each item's
 * `layout`. `initial={false}` keeps the FIRST paint instant (no arrival cascade); only phases added
 * AFTER mount animate in. This AnimatePresence is INDEPENDENT of the active languette's accent bar
 * (`layoutId` in `<OverlayTab>`): the items use local `layout` (no shared id), so the two never
 * collide.
 */
export function RailTabContent({
  rollup,
  reduced,
}: {
  rollup: RailRollup;
  reduced?: boolean | null;
}) {
  return (
    <div className="flex min-h-0 flex-col items-center gap-1 overflow-y-auto">
      <AnimatePresence initial={false}>
        {rollup.items.map((item, i) => (
          // KEY = the phase INDEX (STABLE) — NOT the `kind`. Keying by index means a `kind` flip
          // (bar→done / remaining→bar) MORPHS the SAME element in place (height + radius animate)
          // instead of remounting a fresh node (which would "jump"/reorder rather than morph); a
          // phase COUNT change adds/removes a trailing index ⇒ AnimatePresence animates that dot's
          // enter/leave.
          <RailPhaseItem key={i} item={item} reduced={reduced} />
        ))}
      </AnimatePresence>
    </div>
  );
}

/**
 * ONE phase of the rail as a SINGLE CONTINUOUS `motion.div` that MORPHS between a dot and the bar by
 * animating its SIZE — there is NO crossfade of two views (the old `<AnimatePresence mode="wait">`
 * dot↔bar swap is gone). It animates a purely VERTICAL morph — HEIGHT + border-RADIUS between the DOT
 * and BAR targets ({@link railItemMorph}), the WIDTH held CONSTANT ({@link RAIL_ITEM_WIDTH_PX}) — with
 * the spring shared by the rest of the overlay (`overlayMotionTransition`; `reduced` ⇒ INSTANT, no
 * morph). `layout` lets the NEIGHBOURS reflow fluidly as this item grows/shrinks. This is INDEPENDENT
 * of the active languette's accent bar (which travels via a `layoutId` in `<OverlayTab>`): these items
 * use local `layout` (no shared id), so the accent bar still GLIDES on a Spec switch and never
 * collides with the item morphs.
 *
 * The whole item ENTERS/EXITS through the parent `<AnimatePresence>` (a phase added/removed): it
 * grows in / collapses out (`opacity` + `height`) on the shared spring rather than popping.
 *
 * The CONTENT is CONTINUOUS — the SAME two layers stay mounted across every `kind`, so a `bar → done`
 * (or `remaining → bar`) flip is a pure SIZE change of one element, never a swap:
 *  - the BASE FILL (`data-rail-fill`) — an always-mounted full-bleed layer painted in the phase's
 *    dominant colour ({@link railFillTone}: `bar` = the neutral track, `done` = success green,
 *    `remaining` = the visible neutral). When the pill shrinks 64→6 this same layer scales down, so a
 *    finished phase reads as its dominant colour condensed into a dot (and the reverse on grow).
 *  - the per-status ZONES (`item.segments`, only ever present on the CURRENT phase's `bar`) stacked
 *    top→bottom OVER the base track so the gauge FLOWS DOWN like the timeline (done → active/pulse →
 *    blocked → skipped; the leftover track at the BOTTOM is the empty todo zone). Each zone is its OWN
 *    `motion.div`, wrapped in a nested `<AnimatePresence initial={false}>`, ANIMATING its cumulative
 *    `top` + `height` on the same spring: when a status COUNT changes (a task moves
 *    todo→in_progress→done→blocked…) the SAME zone element (keyed by `seg.key`) TRANSITIONS its
 *    height/top instead of jumping; a zone that first APPEARS (e.g. the first `blocked`) grows from
 *    height 0; one that DISAPPEARS shrinks to 0 before unmounting. Tones come from the GLOBAL
 *    task-status dot mapping (`segment.tone`); the active zone still PULSES (a className, left off the
 *    motion-animated `top`/`height` so it never fights the opacity keyframes) unless `reduced`.
 * As a phase FINISHES its zones naturally CONVERGE to a single 100% `done` (green) zone while the
 * `kind` flips `bar → done` and the height shrinks to a dot — the zones melt into the green base fill,
 * so the bar CONDENSES into a green dot with no crossfade; the next phase's dot GROWS the reverse way.
 */
function RailPhaseItem({ item, reduced }: { item: RailItem; reduced?: boolean | null }) {
  const morph = railItemMorph(item.kind);
  const transition = overlayMotionTransition(reduced ?? null);
  return (
    <motion.div
      layout
      data-rail-phase=""
      data-rail-kind={item.kind}
      className="relative shrink-0 overflow-hidden"
      style={{ width: RAIL_ITEM_WIDTH_PX }}
      // ENTER/EXIT for the parent AnimatePresence (phase added/removed). `initial={false}` on that
      // parent skips this on the FIRST paint, so mount is instant and only LATER add/remove animates.
      // The `height` + `borderRadius` in `animate` ARE the dot↔bar morph: on a `kind` flip the SAME
      // element springs between the DOT (6px, round) and BAR (64px, rounded-rect) targets.
      initial={{ opacity: 0, height: 0 }}
      animate={{ opacity: 1, height: morph.height, borderRadius: morph.borderRadius }}
      exit={{ opacity: 0, height: 0 }}
      transition={transition}
    >
      {/* CONTINUOUS base fill — the ALWAYS-mounted dominant-colour layer that carries the morph's
          continuity: the bar's neutral track, or a dot's status colour (done green / remaining
          neutral). It never enters/exits on a `kind` flip, so the pill is one element that only
          changes SIZE. The animated radius (round at the dot height) + `overflow-hidden` make the
          retracted state read as a round dot. */}
      <div data-rail-fill="" className={cn("absolute inset-0", railFillTone(item.kind))} />
      {/* The per-status ZONES stacked from the TOP over the track so the gauge FLOWS DOWN (done →
          active → blocked → skipped; the leftover track at the BOTTOM is the empty TODO zone). Only
          the CURRENT phase's `bar` carries `segments`; the dots have none, so this renders nothing for
          them (the base fill is the whole dot). Each zone is a motion.div in a nested AnimatePresence
          so height/top TRANSITION on a status-count change and a zone grows-in / shrinks-out on appear
          / disappear. `initial={false}` ⇒ the zones present on first paint settle at their resting
          height (no cascade); a zone added LATER animates from height 0. */}
      <AnimatePresence initial={false}>
        {(() => {
          let top = 0;
          return (item.segments ?? []).map((seg) => {
            const zoneTop = top;
            top += seg.heightPct;
            return (
              <motion.div
                key={seg.key}
                data-rail-bar-zone={seg.key}
                className={cn(
                  "absolute inset-x-0",
                  seg.tone,
                  seg.pulse && !reduced && "motion-safe:animate-pulse",
                )}
                initial={{ top: `${zoneTop}%`, height: 0 }}
                animate={{ top: `${zoneTop}%`, height: `${seg.heightPct}%` }}
                exit={{ height: 0 }}
                transition={transition}
              />
            );
          });
        })()}
      </AnimatePresence>
    </motion.div>
  );
}

/**
 * `<SpecRailOverlay>` — the DEV/REVIEW advancement overlay (rail A). It is anchored to the
 * terminal's right edge and pointer-transparent except for its own chrome, so the terminal stays
 * interactive underneath.
 *
 *  - CLOSED: a SIMPLE thin tab (vertical gauge + phase pips), no name/stacking (one Spec in dev).
 *  - OPEN: a fixed-width right panel — a header (human key + title + status badge) then the task
 *    decomposition rendered FIDÈLE to the real Spec detail by REUSING `<TasksTab>` wholesale
 *    (roll-up, phase cards, status dots, priority badges, blocked-by, finding/human, skipped).
 *
 * Placement = prototype `hud-progression` round v6 (the retained SIMPLE rail). The Spec detail
 * (tasks + dependencies + workspace context) is pulled live via `useSpecDetail`, which re-fetches
 * on `specs://changed` — so the gauge, pips and open panel all track dev progress with NO polling.
 */
export function SpecRailOverlay({ spec }: { spec: Spec }) {
  const [open, setOpen] = useState(false);
  const { detail, loading } = useSpecDetail(spec.id);
  const reduced = useReducedMotion();

  const rollup: RailRollup = detail ? railRollup(detail) : { items: [] };
  const title = spec.title || `#${spec.number}`;

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
      {/* The closed languette (gauge) sits to the LEFT of the panel and STAYS mounted when open — it
          FUSES with the panel (shared surface, no shared border) and becomes its close HANDLE. This
          is the SAME chrome as the tiroir (shared `<OverlayTab>`, model A). Keeping the tab mounted
          means its one-shot arrival ring is NOT re-triggered when the panel closes. */}
      <div className="pointer-events-auto flex max-h-full min-h-0 flex-col justify-center">
        <OverlayTab
          label={`Open ${title} progress`}
          active={open}
          onClick={() => setOpen(!open)}
          // Content-driven height: the languette grows with the phase-point count (the bar stays
          // fixed), capped to the pane so a very large count scrolls instead of overflowing.
          className="max-h-full min-h-0"
        >
          {loading && !detail ? (
            <Spinner className="size-3.5 text-muted-foreground" />
          ) : (
            <RailTabContent rollup={rollup} reduced={reduced} />
          )}
        </OverlayTab>
      </div>
      {/* WIDTH-ANIMATED SHELL (sidebar-shell idiom, same as the tiroir): outer width springs 0↔380 so
          the languette "suit" the panel; `overflow-hidden` over a FIXED-width inner ⇒ no reflow / no
          horizontal overflow. `shadow-xl` (only when open) defines the left edge in the gaps around
          the languette — the panel has NO left border so the fused languette reads seamless. */}
      <motion.div
        initial={false}
        animate={{ width: open && detail ? OVERLAY_PANEL_WIDTH : 0 }}
        transition={overlayMotionTransition(reduced)}
        className={cn(
          "pointer-events-none flex h-full justify-end overflow-hidden",
          open && detail && "shadow-xl",
        )}
      >
        <div className="relative h-full shrink-0" style={{ width: OVERLAY_PANEL_WIDTH }}>
          <AnimatePresence mode="wait">
            {open && detail ? (
              <motion.section
                key={spec.id}
                data-overlay-panel=""
                data-overlay-panel-spec={spec.id}
                variants={overlayPanelVariants}
                initial="initial"
                animate="animate"
                exit="exit"
                transition={overlayMotionTransition(reduced)}
                className="pointer-events-auto absolute inset-0 flex flex-col bg-card/95 backdrop-blur"
                aria-label={`${title} progress`}
              >
                {/* Header: human key + title + status badge, with a chevron to close. */}
                <header className="flex shrink-0 items-center gap-2 border-b border-border/60 px-4 py-3">
                  <span className="font-mono text-2xs text-muted-foreground">
                    {specKey(detail.workspace_context?.project.key ?? "", spec.number)}
                  </span>
                  <span className="min-w-0 flex-1 truncate font-heading text-sm font-semibold text-foreground">
                    {title}
                  </span>
                  <Badge variant={statusMeta(spec.status).variant} className="font-mono">
                    {statusMeta(spec.status).label}
                  </Badge>
                  {/* Status-aware quick actions (open the Spec / open the review) — between the
                      badge and the close chevron; a click navigates AWAY, never closes the panel. */}
                  <OverlayPanelActions spec={spec} />
                  <button
                    type="button"
                    onClick={() => setOpen(false)}
                    aria-label="Close progress panel"
                    className="grid size-6 shrink-0 place-items-center rounded-md text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/50"
                  >
                    <ChevronRightIcon className="size-4" />
                  </button>
                </header>
                {/* FIDÈLE task rendering — the real `<TasksTab>` component. `overflow-x-hidden` clamps
                    the reused content to the 380px panel (no horizontal scrollbar / clipped text). */}
                <div className="min-h-0 flex-1 overflow-y-auto overflow-x-hidden px-3 py-4">
                  <TasksTab detail={detail} />
                </div>
              </motion.section>
            ) : null}
          </AnimatePresence>
        </div>
      </motion.div>
    </motion.div>
  );
}
