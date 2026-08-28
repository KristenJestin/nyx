import { useRef, useState } from "react";
import { ChevronRightIcon } from "lucide-react";
import { AnimatePresence, motion, useReducedMotion } from "motion/react";

import { cn } from "@/lib/utils";
import { OVERLAY_SURFACE_Z } from "@/components/panes/pane-geometry";
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
  overlayMotionTransition,
  overlayPanelVariants,
  overlayPanelWidth,
  overlaySurfaceVariants,
} from "./overlay-motion";
import {
  overlayPanelId,
  useOverlayPaneWidth,
  useOverlayPanelDisclosure,
} from "./use-overlay-panel";
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
 *  - OPEN: a right panel BOUNDED on the host pane (`min(380, max(45% of pane, 260))`,
 *    `overlayPanelWidth` — same rule and same shell as the tiroir) — a header (human key + title +
 *    status badge) then the task decomposition rendered FIDÈLE by REUSING `<TasksTab>` (roll-up +
 *    reprises, nested rounds with provenance/severity).
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
  // The DISCLOSURE pair: the languette announces this id through `aria-controls`, the panel carries
  // it as its own `id`. Derived from the Spec id, so the two can never drift (`use-overlay-panel`).
  const panelId = overlayPanelId(spec.id);

  // The NON-MODAL disclosure behaviour, shared with the tiroir (`use-overlay-panel`): focus INTO the
  // panel on open, focus RESTORED to the languette on every dismissal, panel-scoped Escape and
  // outside-click dismissal. `open` alone is not enough — the panel only mounts once `detail` has
  // loaded, so the focus-in must key off the state in which it is actually on screen.
  //
  // `panelOpen` is THE disclosure state of this overlay, and everything the user or a screen reader
  // perceives as "open" derives from it: the panel's mount, the shell's width, and the languette's
  // `aria-expanded` + fused styling. `open` alone would announce "expanded" during the `spec_detail`
  // load, while `aria-controls` still points at an id NOTHING has mounted (a screen reader says
  // "développé" and finds nothing to explore) and the shell to the languette's right is still 0px
  // wide (a languette fused to nothing). The RAIL is the only one of the two overlays that can split
  // the two states: the tiroir mounts its panel from the same state its languette is active from.
  const rootRef = useRef<HTMLDivElement>(null);
  const panelRef = useRef<HTMLElement>(null);
  const panelOpen = open && detail !== null;
  const { dismiss, onPanelKeyDown } = useOverlayPanelDisclosure({
    open: panelOpen,
    panelId: panelOpen ? panelId : null,
    rootRef,
    panelRef,
    onDismiss: () => setOpen(false),
  });

  // The panel's width is BOUNDED on the host pane (`min(380, max(45% of pane, 260))`), remeasured on
  // a pane resize and resolved to a NUMBER here — Motion animates the shell to it and the inner box
  // is frozen at it (`overlay-motion`). The pane itself is never reflowed: the panel stays
  // superposed, so no xterm refit / SIGWINCH is triggered by opening or closing it.
  const panelWidth = overlayPanelWidth(useOverlayPaneWidth(rootRef));

  return (
    <motion.div
      ref={rootRef}
      data-overlay-surface=""
      variants={overlaySurfaceVariants}
      initial="initial"
      animate="animate"
      exit="exit"
      transition={overlayMotionTransition(reduced)}
      // The stack order is {@link OVERLAY_SURFACE_Z}, not a literal — same three-way contract as
      // the tiroir's surface: above the separator band it overlaps at the pane's right edge, below
      // the pane control cluster.
      style={{ zIndex: OVERLAY_SURFACE_Z }}
      className="pointer-events-none absolute inset-y-0 right-0 flex items-stretch justify-end"
    >
      {/* The closed languette sits to the LEFT of the panel and STAYS mounted when open — it FUSES
          with the panel (shared surface, no shared border) and becomes its close HANDLE.
          `pointer-events-none` on the COLUMN: `items-stretch` on the `inset-y-0` root stretches this
          wrapper over the FULL pane height while `justify-center` parks the languette in the middle,
          so an interactive column swallowed every click in the 36px right-edge band — including the
          empty zones above/below the languette, which cover the TERMINAL's own scroll-to-bottom
          button (`terminal.tsx`, `right-3 bottom-3 z-10`) and the global 0.5rem scrollbar gutter
          (`globals.css`, `*`). None of that belongs to the overlay, so the column must be
          click-through; `<OverlayTab>` re-arms itself with its own `pointer-events-auto` (and its
          content inherits it as children of the button), so the languette stays clickable.
          ONE box here, where `spec-drawer-overlay` nests TWO: the rail holds a SINGLE languette and
          is not a scroll container, so it has no scrollbar of its own to re-arm — nothing needs
          `pointer-events-auto` back except the tab itself. Do NOT "align" the two files: the
          tiroir's inner box exists to keep ITS scrollbar hittable (see the comment there), and a box
          added here would only re-capture the 36px band this `pointer-events-none` just freed. */}
      <div className="pointer-events-none flex max-h-full min-h-0 flex-col justify-center">
        <OverlayTab
          label={`Open ${title} progress`}
          panelId={panelId}
          // The DISCLOSURE state — `panelOpen`, never `open`: `aria-expanded` may only say "true"
          // while the panel `aria-controls` names is actually mounted (see the note above).
          active={panelOpen}
          // Re-clicking the ACTIVE languette closes its panel — routed through `dismiss` so the
          // close half also restores focus here, exactly like the chevron / Escape / outside click.
          // Keyed on `open` and NOT on `panelOpen` on purpose: a click during the `spec_detail` load
          // CANCELS that load instead of being swallowed as a no-op re-open.
          onClick={() => (open ? dismiss() : setOpen(true))}
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
      {/* WIDTH-ANIMATED SHELL (sidebar-shell idiom, same as the tiroir): outer width springs between
          0 and the BOUNDED panel width so the languette "suit" the panel; `overflow-hidden` over an
          inner FROZEN at that same pixel width ⇒ no reflow, no horizontal overflow mid-animation. */}
      <motion.div
        data-overlay-panel-shell=""
        initial={false}
        animate={{ width: panelOpen ? panelWidth : 0 }}
        transition={overlayMotionTransition(reduced)}
        className={cn(
          "pointer-events-none flex h-full justify-end overflow-hidden",
          panelOpen && "shadow-xl",
        )}
      >
        <div
          data-overlay-panel-inner=""
          className="relative h-full shrink-0"
          style={{ width: panelWidth }}
        >
          <AnimatePresence mode="wait">
            {panelOpen && detail ? (
              <motion.section
                key={spec.id}
                id={panelId}
                ref={panelRef}
                data-overlay-panel=""
                data-overlay-panel-spec={spec.id}
                variants={overlayPanelVariants}
                initial="initial"
                animate="animate"
                exit="exit"
                transition={overlayMotionTransition(reduced)}
                className="pointer-events-auto absolute inset-0 flex flex-col bg-card/95 backdrop-blur focus:outline-none"
                // NON-MODAL dialog (decided): a `dialog` role + a name so the panel is ANNOUNCED,
                // `tabIndex={-1}` so the focus lands on the CONTAINER (never on the first command,
                // which navigates away) — but deliberately NO `aria-modal` and NO focus trap, so Tab
                // walks out and the terminal stays keyboard-usable while the Spec is on screen.
                role="dialog"
                aria-label={`${title} progress`}
                tabIndex={-1}
                // Escape is PANEL-SCOPED — handled here, never on `document`, so the key keeps
                // working in the terminal (`use-overlay-panel`).
                onKeyDown={onPanelKeyDown}
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
                    onClick={() => dismiss()}
                    aria-label="Close progress panel"
                    className="grid size-6 shrink-0 place-items-center rounded-md text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/50"
                  >
                    <ChevronRightIcon className="size-4" />
                  </button>
                </header>
                {/* FIDÈLE task rendering — the real `<TasksTab>` component, fed the review comments'
                    author map so its nested rounds derive provenance. `overflow-x-hidden` clamps the
                    reused content to the panel's bounded width (no horizontal scrollbar / clipped text).
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
