/**
 * Overlay classification — the ONE place that decides which overlay a terminal's Spec set shows
 * and how each Spec reads in the overlay chrome. It leans on the SHARED Spec-domain sources of
 * truth (`spec-meta` for status/priority looks, `tasks-tab`'s `progressOf`/`splitPhaseTasks` for
 * the canonical progress + round grouping) so the overlay never diverges from the real Specs UI.
 *
 * MODE (PRD Impl Decisions): there is no distinct "creating" session state — a session is just
 * `active`/`ended`/… So create-vs-dev is read off each Spec's own STATUS:
 *   - CREATION (the live authoring overlay = the TIROIR): a Spec still being authored — `draft`
 *     (drafting) or `ready` (figé / prêt). Multi-Spec stacking lives here (a feature cut into N
 *     Specs, all created in the same session).
 *   - DEV (the advancement overlay = the RAIL): a Spec under development/review — `in_progress`,
 *     `in_review`, or `blocked`. A single Spec in dev; no stacking, no name.
 * A Spec that is `completed`/`cancelled`/`archived` belongs to NEITHER (the set drops it on
 * done) → it never drives an overlay. No creation AND no dev Spec ⇒ no overlay (non-regression).
 * The session's `state` is the LIVENESS axis (active ⇒ the agent is live ⇒ pulse), not the mode.
 */
import { statusMeta, taskStatusMeta } from "../specs/spec-meta";
import { progressOf, splitPhaseTasks } from "../specs/tabs/tasks-tab";
import { parsePhases, type Spec, type SpecDetail, type Task } from "../specs/types";

/** Spec statuses that read as "being authored" — the CREATION (tiroir) overlay. */
export const CREATION_STATUSES: ReadonlySet<string> = new Set(["draft", "ready"]);
/** Spec statuses that read as "under development/review" — the DEV (rail) overlay. */
export const DEV_STATUSES: ReadonlySet<string> = new Set(["in_progress", "in_review", "blocked"]);

export type OverlayMode = "creation" | "dev";

/** A Spec still being authored (drafting or figé/ready) — drives the creation TIROIR. */
export function isCreationSpec(spec: Spec): boolean {
  return CREATION_STATUSES.has(spec.status);
}
/** A Spec under development/review — drives the advancement RAIL. */
export function isDevSpec(spec: Spec): boolean {
  return DEV_STATUSES.has(spec.status);
}

/** The Specs that drive the CREATION tiroir (drafting/ready), preserving set order. */
export function creationSpecs(specs: readonly Spec[]): Spec[] {
  return specs.filter(isCreationSpec);
}
/** The Specs that drive the DEV rail (in dev/review), preserving set order. */
export function devSpecs(specs: readonly Spec[]): Spec[] {
  return specs.filter(isDevSpec);
}

/**
 * Which overlay this Spec set drives, or `null` for none. CREATION wins when the session is
 * authoring at least one Spec (the tiroir can stack several); otherwise a Spec in dev drives the
 * rail. `null` ⇒ no overlay at all (empty set, or only terminal-state Specs).
 */
export function overlayMode(specs: readonly Spec[]): OverlayMode | null {
  if (specs.some(isCreationSpec)) return "creation";
  if (specs.some(isDevSpec)) return "dev";
  return null;
}

// ---------------------------------------------------------------------------
// The RAIL (closed languette) — the FROZEN design (review #32).
// ---------------------------------------------------------------------------
//
// The languette is a vertical column, ONE entry per phase (top→bottom in phase order):
//   - PAST & FUTURE phases render as DOTS — ALWAYS GREY (done reads grey, never green). A phase
//     that carried rework rounds shows N hollow RING dots tied to its phase dot by a vertical
//     trait (`rounds` = the ring count).
//   - the CURRENT phase renders as GLUED BARS: the plan (unit 0), then one mini-gauge per rework
//     round (N.1, N.2…). A bar is NOT a progress gauge — it is a STRIP of SEGMENTS, one per
//     (non-cancelled) task, stacked top→bottom in task order. EACH segment carries the colour of
//     ITS task's STATUS, from the SAME `taskStatusMeta` the panel dots use (dot ↔ segment
//     coherence): done = green, in_progress = amber (pulse), blocked = red (pulse), a pending
//     kind=human task = magenta (pulse, "besoin de toi"), todo = TRANSPARENT (opacity 0, the track
//     shows through). A cancelled task contributes NO segment (excluded). Rework units are hatched.
// Because each segment self-colours by task status, an in_progress task ALWAYS shows amber even at
// 0 done (the old single-fill bar went fully grey at 0% — the bug this replaces).

/** A task RESOLVED = it left the OPEN set: `done` OR skipped (`cancelled`). */
const RESOLVED_STATUSES: ReadonlySet<string> = new Set(["done", "cancelled"]);

const isResolved = (t: Task): boolean => RESOLVED_STATUSES.has(t.status);
/** A cancelled/skipped task — EXCLUDED from the segment strip entirely (contributes NO segment). */
const isCancelled = (t: Task): boolean => t.status === "cancelled";
/**
 * A `kind=human` task still OPEN — a hand-off the user has to act on ⇒ a magenta "besoin de toi"
 * segment. The SAME helper the pre-segment colour model leaned on for the rail's needs_you signal.
 */
const isPendingHuman = (t: Task): boolean => t.kind === "human" && !isResolved(t);

/** The tasks of one phase (a task's `phase` indexes `spec.phases`; unmatched ⇒ excluded here). */
export function tasksOfPhase(tasks: readonly Task[], phaseNumber: number): Task[] {
  return tasks.filter((t) => t.phase === phaseNumber);
}

/**
 * Whether a phase is ENTIRELY done — all its tasks resolved (`progressOf` ⇒ 100%). A phase with NO
 * tasks is NOT done (nothing has been completed yet); it still reads as pending/current work, so an
 * empty phase never counts as a finished point nor gets skipped over as the current phase.
 */
function isPhaseDone(tasks: readonly Task[]): boolean {
  return tasks.length > 0 && progressOf(tasks).pct >= 100;
}

/**
 * The index of the CURRENT phase = the FIRST phase not entirely done (has todo/in_progress tasks),
 * or `-1` when EVERY phase is already done. Phases before it are past; phases after it are future.
 * `perPhase` is the tasks grouped by phase, in phase order.
 */
export function currentPhaseIndex(perPhase: readonly (readonly Task[])[]): number {
  return perPhase.findIndex((tasks) => !isPhaseDone(tasks));
}

/** The number of distinct rework ROUNDS (round ≥ 1) of a phase's tasks — the hollow-ring count. */
export function roundCount(tasks: readonly Task[]): number {
  const rounds = new Set<number>();
  for (const t of tasks) {
    const r = t.round ?? 0;
    if (r >= 1) rounds.add(r);
  }
  return rounds.size;
}

/** The colour STATE of ONE segment (one per non-cancelled task) — drives its tone/pulse/opacity. */
export type SegmentState = "done" | "in_progress" | "blocked" | "needs_you" | "todo";

/** The magenta "besoin de toi" tone for a pending kind=human segment (`primary` = magenta in dark). */
export const RAIL_NEEDS_TONE = "bg-primary";

/**
 * ONE segment of a unit's status strip = ONE non-cancelled task, coloured by its STATUS. Except for
 * the two special cases below, the tone is `taskStatusMeta(status).dot` — the EXACT SAME source of
 * truth as the panel's task dots, so a segment and its dot always read the same colour:
 *   - `done` → green, `in_progress` → amber (pulse), `blocked` → red (pulse) — straight from
 *     `taskStatusMeta`.
 *   - a pending `kind=human` task → magenta (pulse) "besoin de toi" — a hand-off, special-cased over
 *     its raw status (matches the rail's needs_you language and `/tmp/segments.html` `.seg.human`).
 *   - `todo` / any other not-started status → TRANSPARENT (opacity 0): no colour, the unit's track
 *     shows through (so an all-todo unit reads as a bare track, and in_progress next to it pops).
 */
export interface RailSegment {
  readonly state: SegmentState;
  /** The segment's `bg-*` tone — `taskStatusMeta(status).dot` (or {@link RAIL_NEEDS_TONE} for needs). */
  readonly tone: string;
  /** `todo`/not-started ⇒ rendered at opacity 0 (the track shows through — no colour painted). */
  readonly transparent: boolean;
  /** Live/attention states breathe (pulse): `in_progress`, `blocked`, `needs_you`. */
  readonly pulse: boolean;
}

/**
 * The {@link RailSegment} for ONE non-cancelled task. A pending `kind=human` task wins magenta;
 * otherwise the tone comes from `taskStatusMeta(status).dot` (dot ↔ segment coherence), with `todo`
 * (and any non-active status) rendered transparent. Cancelled tasks are filtered out UPSTREAM
 * ({@link unitSegments}) and never reach here.
 */
export function segmentOf(t: Task): RailSegment {
  if (isPendingHuman(t)) {
    return { state: "needs_you", tone: RAIL_NEEDS_TONE, transparent: false, pulse: true };
  }
  switch (t.status) {
    case "done":
      return { state: "done", tone: taskStatusMeta("done").dot, transparent: false, pulse: false };
    case "in_progress":
      return {
        state: "in_progress",
        tone: taskStatusMeta("in_progress").dot,
        transparent: false,
        pulse: true,
      };
    case "blocked":
      return {
        state: "blocked",
        tone: taskStatusMeta("blocked").dot,
        transparent: false,
        pulse: true,
      };
    default:
      // `todo` / `ready` / `in_review` — not started ⇒ transparent, the track shows through.
      return { state: "todo", tone: taskStatusMeta("todo").dot, transparent: true, pulse: false };
  }
}

/**
 * One glued UNIT of the current phase's bar — the plan (unit 0, `rework=false`) then one per rework
 * round (`rework=true`, hatched). A unit is a STRIP of `segments`: one {@link RailSegment} per
 * non-cancelled task, in task order (top→bottom). An empty unit (no tasks, or all cancelled) has an
 * empty `segments` array ⇒ it renders as a bare track. NOT a proportional fill: each segment carries
 * its own task's status colour, so the unit's state is visible per-task (not a single aggregate hue).
 */
export interface RailUnit {
  readonly key: string;
  readonly rework: boolean;
  readonly segments: RailSegment[];
}

/**
 * The SEGMENT strip of ONE unit — one {@link RailSegment} per task, in task order, with cancelled
 * tasks EXCLUDED (they contribute no segment). 2 done + 1 cancelled ⇒ 2 (green) segments, not 3;
 * 1 in_progress + 2 todo ⇒ 1 amber + 2 transparent segments. An empty task list ⇒ no segments.
 */
function unitSegments(tasks: readonly Task[], key: string, rework: boolean): RailUnit {
  const segments = tasks.filter((t) => !isCancelled(t)).map(segmentOf);
  return { key, rework, segments };
}

/**
 * The current phase's glued UNITS, top→bottom: the PLAN first (round null/0 tasks) then one unit per
 * rework round (ascending). There is ALWAYS at least one unit — an empty phase yields a single empty
 * plan unit (no segments = a bare track). "En rework le plan est plein" falls out naturally: once the
 * plan's tasks are all done its segments read all-green while the active round below shows its own.
 */
export function phaseUnits(tasks: readonly Task[]): RailUnit[] {
  const { plan, rounds } = splitPhaseTasks(tasks);
  const units: RailUnit[] = [];
  if (plan.length > 0 || rounds.length === 0) units.push(unitSegments(plan, "plan", false));
  for (const rg of rounds) units.push(unitSegments(rg.tasks, `round-${rg.round}`, true));
  return units;
}

/**
 * One phase of the rail, in phase order:
 *  - `past` / `future` → a GREY dot (+ `rounds` hollow ring dots for a reworked phase).
 *  - `current`         → the glued bars (`units`), each a segment strip self-coloured per task status.
 */
export type RailPhase =
  | { readonly kind: "past"; readonly rounds: number }
  | { readonly kind: "future"; readonly rounds: number }
  | { readonly kind: "current"; readonly units: RailUnit[] };

/** The rail's ordered per-phase model (one entry per phase, top→bottom). */
export interface RailModel {
  readonly phases: RailPhase[];
}

/**
 * The rail model for a Spec: one {@link RailPhase} per phase in order. The CURRENT phase (first not
 * entirely done) is the segment BAR; earlier phases are PAST grey dots, later ones FUTURE grey dots
 * (both carrying their rework-round ring count). Each current-phase bar self-colours per task status
 * (see {@link segmentOf}) — there is no aggregate phase state to derive. Edge cases, all crash-safe:
 *  - FLAT spec (no phases) ⇒ a SINGLE current bar over the whole task set.
 *  - EVERY phase done (Spec finished) ⇒ the LAST phase reads as an all-GREEN current bar ("tout
 *    vert = Spec fini"), the earlier ones as past grey dots.
 */
export function railModel(detail: SpecDetail): RailModel {
  const phases = parsePhases(detail.spec.phases);

  // FLAT spec: one implicit phase = ALL tasks, always the current bar.
  if (phases.length === 0) {
    return { phases: [{ kind: "current", units: phaseUnits(detail.tasks) }] };
  }

  const perPhase = phases.map((p) => tasksOfPhase(detail.tasks, p.number));
  const currentIdx = currentPhaseIndex(perPhase);
  const allDone = currentIdx === -1;
  // Spec finished ⇒ no not-done phase: pin the current bar to the LAST phase so it reads all-green.
  const currentPos = allDone ? phases.length - 1 : currentIdx;

  return {
    phases: phases.map((_, i): RailPhase => {
      const tasks = perPhase[i];
      if (i === currentPos) return { kind: "current", units: phaseUnits(tasks) };
      return { kind: i < currentPos ? "past" : "future", rounds: roundCount(tasks) };
    }),
  };
}

// --- Rail colour tokens (design-system classes ONLY — no raw hex; magenta = `primary` in dark) ---

/** The grey PHASE dot (past/future) — always neutral, never green (a done phase reads grey). */
export const RAIL_DOT_TONE = "bg-muted-foreground/50";
/** A hollow ROUND ring dot — a bordered circle over the surface. */
export const RAIL_RING_TONE = "border border-ring bg-background";
/** The vertical TRAIT tying a reworked phase's rings to its dot. */
export const RAIL_TIE_TONE = "bg-border";
/**
 * A bar unit's neutral TRACK (grey) — the unit container's background, shown through `todo`
 * (transparent) segments and behind an empty (no-task) unit.
 */
export const RAIL_TRACK_TONE = "bg-muted-foreground/20";

/**
 * The hatched texture of a rework unit's segments (`.rw` in `/tmp/segments.html`) — a diagonal
 * pattern layered OVER a segment's status colour so a rework round reads distinct from the plan while
 * keeping the segment's own hue. Alternate stripes are darkened with a translucent `--background` (a
 * design token) via `color-mix`, so it follows the theme and bakes in no raw colour.
 */
export const RAIL_HATCH_STYLE: Readonly<Record<string, string>> = {
  backgroundImage:
    "repeating-linear-gradient(-45deg, transparent 0 2px, color-mix(in oklab, var(--background) 55%, transparent) 2px 5px)",
};

/**
 * The drawer tab's single status POINT. Its COLOUR comes from the GLOBAL `statusMeta(spec.status)`
 * — the SAME source of truth as the status BADGE shown in the open panel (and the rail / Spec detail
 * page), so the closed tab's dot and the open panel's badge always read the same status.
 * `statusMeta` already exposes a solid `dot` token (`bg-*`), so no bespoke tones.
 *
 * The `pulse` flag is kept as an INDEPENDENT vivacité signal — set for a Spec being authored LIVE
 * (`draft` while the session is active) — but it no longer drives the COLOUR: it only makes the dot
 * breathe on top of its statusMeta colour.
 */
export interface StatusPoint {
  /** A `bg-*` token class for the point — derived from `statusMeta(spec.status).dot`. */
  readonly tone: string;
  /** Whether the point breathes (the agent is live and authoring a draft). */
  readonly pulse: boolean;
  /** An accessible label for the point — the global status label (`statusMeta.label`). */
  readonly label: string;
}

export function drawerStatusPoint(spec: Spec, live: boolean): StatusPoint {
  const meta = statusMeta(spec.status);
  // Vivacité: a draft being authored while the session is live still breathes — but the COLOUR is
  // the global status colour, not a bespoke "creating" accent.
  const pulse = spec.status === "draft" && live;
  return { tone: meta.dot, pulse, label: meta.label };
}
