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
import { useSpecReviewComments } from "../specs/use-spec-review-comments";
import { OverlayPanelActions } from "./overlay-panel-actions";
import { OverlayPrototypes } from "./overlay-prototypes";
import { OverlayTab } from "./overlay-tab";
import {
  OVERLAY_PANEL_WIDTH,
  overlayMotionTransition,
  overlayPanelVariants,
  overlaySurfaceVariants,
} from "./overlay-motion";
import {
  RAIL_DOT_TONE,
  RAIL_HATCH_STYLE,
  RAIL_RING_TONE,
  RAIL_TIE_TONE,
  RAIL_TRACK_TONE,
  railModel,
  type RailModel,
  type RailPhase,
  type RailSegment,
  type RailUnit,
} from "./overlay-status";

/** Fixed geometry of the frozen rail (px) — a dot pastille, a hollow round ring, a glued bar unit. */
const RAIL_DOT_PX = 8;
const RAIL_RING_PX = 6;
const RAIL_UNIT_HEIGHT_PX = 40;
const RAIL_UNIT_WIDTH_PX = 10;

/**
 * The closed rail tab's CONTENT — the FROZEN languette design (review #32). ONE entry per phase,
 * top→bottom in phase order:
 *  - PAST / FUTURE → a GREY dot (always neutral, never green); a reworked phase adds N hollow RING
 *    dots tied to it by a vertical trait ({@link RailDotPhase}).
 *  - CURRENT → the glued segment BARS (plan then rework rounds), the ONLY coloured element, each bar
 *    a strip of per-task status segments ({@link RailCurrentPhase}).
 * `reduced` (from `useReducedMotion`) drops the segments' pulse.
 */
export function RailTabContent({ model, reduced }: { model: RailModel; reduced?: boolean | null }) {
  return (
    <div className="flex min-h-0 flex-col items-center gap-2.5 overflow-y-auto py-1">
      {model.phases.map((phase, i) =>
        phase.kind === "current" ? (
          <RailCurrentPhase key={i} phase={phase} reduced={reduced} />
        ) : (
          <RailDotPhase key={i} phase={phase} />
        ),
      )}
    </div>
  );
}

/**
 * A PAST or FUTURE phase — a grey filled DOT (done reads grey, never green). A phase that carried
 * rework rounds shows `rounds` hollow RING dots below it, tied to the phase dot by a vertical trait
 * (the "round rattaché à sa phase" grouping).
 */
function RailDotPhase({ phase }: { phase: Extract<RailPhase, { kind: "past" | "future" }> }) {
  const tied = phase.rounds > 0;
  return (
    <div
      data-rail-phase=""
      data-rail-kind={phase.kind}
      className="relative flex flex-col items-center gap-1"
    >
      {tied ? (
        <span
          data-rail-tie=""
          aria-hidden
          className={cn("absolute left-1/2 top-2 bottom-1 w-px -translate-x-1/2", RAIL_TIE_TONE)}
        />
      ) : null}
      <span
        data-rail-dot=""
        className={cn("relative z-[1] rounded-full", RAIL_DOT_TONE)}
        style={{ width: RAIL_DOT_PX, height: RAIL_DOT_PX }}
      />
      {phase.rounds > 0
        ? Array.from({ length: phase.rounds }).map((_, r) => (
            <span
              key={r}
              data-rail-round-ring=""
              className={cn("relative z-[1] rounded-full", RAIL_RING_TONE)}
              style={{ width: RAIL_RING_PX, height: RAIL_RING_PX }}
            />
          ))
        : null}
    </div>
  );
}

/**
 * The CURRENT phase — the glued BARS: the plan (unit 0) then one bar per rework round, each a
 * fixed-height STRIP of SEGMENTS (one per non-cancelled task, self-coloured by task status). This is
 * the ONLY coloured element of the rail. Rework units are hatched.
 */
function RailCurrentPhase({
  phase,
  reduced,
}: {
  phase: Extract<RailPhase, { kind: "current" }>;
  reduced?: boolean | null;
}) {
  return (
    <div
      data-rail-phase=""
      data-rail-kind="current"
      className="flex flex-col items-center overflow-hidden rounded-sm"
    >
      {phase.units.map((unit, i) => (
        <RailUnitBar key={unit.key} unit={unit} first={i === 0} reduced={reduced} />
      ))}
    </div>
  );
}

/**
 * ONE glued unit of the current phase's bar — a fixed-height STRIP of SEGMENTS, one per non-cancelled
 * task in task order (top→bottom), each `flex-1` so they split the height evenly. Each segment carries
 * ITS task's status colour ({@link RailSegmentCell}), so an in_progress task reads amber even with 0
 * done (the bug: a single-fill bar went fully grey at 0%). The unit's background is the neutral TRACK,
 * shown through `todo` (transparent) segments and behind an empty (no-task) unit. A rework unit hatches
 * its segments. No provenance tint ever leaks into the rail — task status only.
 */
function RailUnitBar({
  unit,
  first,
  reduced,
}: {
  unit: RailUnit;
  first: boolean;
  reduced?: boolean | null;
}) {
  return (
    <div
      data-rail-unit={unit.key}
      data-rail-rework={unit.rework ? "" : undefined}
      className={cn(
        "flex flex-col overflow-hidden",
        RAIL_TRACK_TONE,
        !first && "border-t border-background",
      )}
      style={{ width: RAIL_UNIT_WIDTH_PX, height: RAIL_UNIT_HEIGHT_PX }}
    >
      {unit.segments.map((segment, i) => (
        <RailSegmentCell key={i} segment={segment} rework={unit.rework} reduced={reduced} />
      ))}
    </div>
  );
}

/**
 * ONE segment of a unit's strip = ONE non-cancelled task, painted in its STATUS colour (the SAME
 * `taskStatusMeta` tone as the panel dot ⇒ dot ↔ segment coherence). `todo` renders at opacity 0 (the
 * track shows through); in_progress/blocked/needs (kind=human) pulse (unless reduced motion). A rework
 * unit hatches each coloured segment. Segments within a unit are CONTIGUOUS — NO split line between
 * adjacent tasks (two same-status tasks merge into one block); the only trait is BETWEEN units
 * ({@link RailUnitBar}).
 */
function RailSegmentCell({
  segment,
  rework,
  reduced,
}: {
  segment: RailSegment;
  rework: boolean;
  reduced?: boolean | null;
}) {
  const pulse = segment.pulse && !reduced ? "motion-safe:animate-pulse" : undefined;
  return (
    <span
      data-rail-seg={segment.state}
      className={cn(
        "min-h-0 flex-1",
        // `todo` = opacity 0 (no colour, the track shows through); otherwise the status tone.
        segment.transparent ? "opacity-0" : segment.tone,
        pulse,
      )}
      // A rework round hatches its coloured segments (skip the invisible `todo` ones).
      style={rework && !segment.transparent ? RAIL_HATCH_STYLE : undefined}
    />
  );
}

/**
 * `<SpecRailOverlay>` — the DEV/REVIEW advancement overlay (rail A). It is anchored to the
 * terminal's right edge and pointer-transparent except for its own chrome, so the terminal stays
 * interactive underneath.
 *
 *  - CLOSED: the FROZEN languette ({@link RailTabContent}) — grey dots for past/future phases (with
 *    rework rings), a coloured glued bar for the current phase. No name/stacking (one Spec in dev).
 *  - OPEN: a fixed-width right panel — a header (human key + title + status badge) then the task
 *    decomposition rendered FIDÈLE by REUSING `<TasksTab>` (roll-up + reprises, nested rounds with
 *    provenance/severity).
 *
 * The Spec detail (tasks + dependencies + workspace context) is pulled live via `useSpecDetail`
 * (`specs://changed`); the review comments — the source of the panel's provenance — via
 * `useSpecReviewComments` (`repos://changed`). No polling. The rail's "besoin de toi" (magenta) now
 * rides a pending `kind=human` task's own segment (per-task status colour), not a phase-level signal.
 */
export function SpecRailOverlay({ spec }: { spec: Spec }) {
  const [open, setOpen] = useState(false);
  const { detail, loading } = useSpecDetail(spec.id);
  const { authorByCommentId } = useSpecReviewComments(detail?.workspace_context ?? null, spec.id);
  const reduced = useReducedMotion();

  const model: RailModel = detail ? railModel(detail) : { phases: [] };
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
      {/* The closed languette sits to the LEFT of the panel and STAYS mounted when open — it FUSES
          with the panel (shared surface, no shared border) and becomes its close HANDLE. */}
      <div className="pointer-events-auto flex max-h-full min-h-0 flex-col justify-center">
        <OverlayTab
          label={`Open ${title} progress`}
          active={open}
          onClick={() => setOpen(!open)}
          // Content-driven height: the languette grows with the phase count, capped to the pane so a
          // very large count scrolls instead of overflowing.
          className="max-h-full min-h-0"
        >
          {loading && !detail ? (
            <Spinner className="size-3.5 text-muted-foreground" />
          ) : (
            <RailTabContent model={model} reduced={reduced} />
          )}
        </OverlayTab>
      </div>
      {/* WIDTH-ANIMATED SHELL (sidebar-shell idiom, same as the tiroir): outer width springs 0↔380 so
          the languette "suit" the panel; `overflow-hidden` over a FIXED-width inner ⇒ no reflow. */}
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
                {/* FIDÈLE task rendering — the real `<TasksTab>` component, fed the review comments'
                    author map so its nested rounds derive provenance. `overflow-x-hidden` clamps the
                    reused content to the 380px panel (no horizontal scrollbar / clipped text).
                    Above it, the Spec's PROTOTYPE PAGES (finding `01KYW342EE5DN6F70YB8VSTJR8`): each
                    row opens the workbench ON that page. It renders nothing for a Spec that has no
                    prototype, so a dev rail is never taxed by a block about an absence. */}
                <div className="min-h-0 flex-1 overflow-y-auto overflow-x-hidden px-3 py-4">
                  <div className="mb-4 empty:mb-0">
                    <OverlayPrototypes specId={spec.id} />
                  </div>
                  <TasksTab detail={detail} authorByCommentId={authorByCommentId} />
                </div>
              </motion.section>
            ) : null}
          </AnimatePresence>
        </div>
      </motion.div>
    </motion.div>
  );
}
