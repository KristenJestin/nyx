/**
 * Overlay classification — the ONE place that decides which overlay a terminal's Spec set shows
 * and how each Spec reads in the overlay chrome. It leans on the SHARED Spec-domain sources of
 * truth (`spec-meta` for status/priority looks, `tasks-tab`'s `progressOf` for the canonical
 * progress formula) so the overlay never diverges from the real Specs UI.
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
import { progressOf } from "../specs/tabs/tasks-tab";
import { parsePhases, type Spec, type SpecDetail, type Task, type TaskStatus } from "../specs/types";

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

export type PhasePointState = "done" | "remaining";

/** The tasks of one phase (a task's `phase` indexes `spec.phases`; unmatched ⇒ excluded here). */
export function tasksOfPhase(tasks: readonly Task[], phaseNumber: number): Task[] {
  return tasks.filter((t) => t.phase === phaseNumber);
}

/**
 * Whether a phase is ENTIRELY done — all its tasks resolved (`progressOf` ⇒ 100%). A phase with NO
 * tasks is NOT done (nothing has been completed yet); it still reads as pending/current work, so an
 * empty phase never counts as a finished `done` point nor gets skipped over as the current phase.
 */
function isPhaseDone(tasks: readonly Task[]): boolean {
  return tasks.length > 0 && progressOf(tasks).pct >= 100;
}

/**
 * The completion % (0..100) of a task set FOR THE RAIL BAR. It is `progressOf` normally, but an
 * EMPTY set reads as 0% — NOT the 100% `progressOf` returns for an "all resolved" (empty/all-skip)
 * set — so a current phase with no tasks, or a flat spec with no tasks, shows an EMPTY bar rather
 * than a spuriously full one. No NaN, no divide-by-zero.
 */
function barPctOf(tasks: readonly Task[]): number {
  return tasks.length === 0 ? 0 : progressOf(tasks).pct;
}

/**
 * The index of the CURRENT phase = the FIRST phase not entirely done (has todo/in_progress tasks),
 * or `-1` when EVERY phase is already done. Phases before it are done; phases after it are
 * remaining. `perPhase` is the tasks grouped by phase, in phase order.
 */
export function currentPhaseIndex(perPhase: readonly (readonly Task[])[]): number {
  return perPhase.findIndex((tasks) => !isPhaseDone(tasks));
}

/** One rail item = one phase, in ORDER: a `done` dot, the `bar`, or a `remaining` dot. */
export type RailItemKind = "bar" | PhasePointState;

/**
 * Task statuses that read as "actively being worked" — `in_progress` OR `in_review`. These are the
 * tasks the rail bar's PULSING zone surfaces so a phase with a task underway no longer reads as
 * 0%/idle. NB: this is NARROWER than `progressOf`'s `active` (which is `total − skipped`).
 */
export const ACTIVE_STATUSES: ReadonlySet<string> = new Set(["in_progress", "in_review"]);

/** The rail bar's stack keys, in ORDER top→bottom (= progression): done, active, blocked, skipped. */
export type BarZoneKey = "done" | "active" | "blocked" | "skipped";

/**
 * The rail bar's zone DEFINITIONS, in the fixed STACK ORDER (top→bottom = progression). Each zone
 * groups one or more `task_status` values, takes its colour from the GLOBAL task-status dot mapping
 * (`taskStatusMeta` — the SAME source of truth as the panel's task dots, so the bar "follows the
 * global component"), and declares whether it PULSES (only the active/en-cours zone). The
 * remaining `task_status` values (todo/ready) form NO zone — they are the EMPTY track at the bottom.
 *  - `done`    → the `done` status              → `bg-success` (green), solid, at the TOP.
 *  - `active`  → `in_progress` + `in_review`    → `bg-warning` (amber), PULSES, below done.
 *  - `blocked` → the `blocked` status           → `bg-destructive` (red), solid.
 *  - `skipped` → the `cancelled` (skip) status  → the neutral `cancelled` dot tone.
 */
const BAR_ZONE_DEFS: readonly {
  readonly key: BarZoneKey;
  readonly statuses: readonly TaskStatus[];
  readonly toneStatus: TaskStatus;
  readonly pulse: boolean;
}[] = [
  { key: "done", statuses: ["done"], toneStatus: "done", pulse: false },
  { key: "active", statuses: ["in_progress", "in_review"], toneStatus: "in_progress", pulse: true },
  { key: "blocked", statuses: ["blocked"], toneStatus: "blocked", pulse: false },
  { key: "skipped", statuses: ["cancelled"], toneStatus: "cancelled", pulse: false },
];

/**
 * ONE stacked zone of the rail bar — a contiguous segment for a task-status group, ready to be
 * absolutely stacked (the render computes the cumulative `top`). `tone` is a design-system `bg-*`
 * class straight from {@link BAR_ZONE_DEFS} (i.e. `taskStatusMeta`), never a bespoke token.
 */
export interface BarSegment {
  /** Which zone this is (stable hook for tests / render). */
  readonly key: BarZoneKey;
  /** The zone's fill colour — a design-system token `bg-*` from the global task-status dot mapping. */
  readonly tone: string;
  /** The zone's height as a % of the phase's TOTAL task count (0..100). */
  readonly heightPct: number;
  /** Whether the zone breathes (`motion-safe:animate-pulse`) — only the active/en-cours zone. */
  readonly pulse: boolean;
}

/**
 * The rail bar's zones for a task set, as an ORDERED list of {@link BarSegment} (stack order
 * top→bottom = done, active, blocked, skipped). Height of each = `count(status)/total × 100`. A
 * status ABSENT from the set yields NO segment (zero-height zones are omitted, so the render draws
 * only what exists). The zones are DISJOINT subsets of the tasks ⇒ their heights sum to `≤ 100`; the
 * remainder (todo/ready) is the EMPTY track at the bottom. An EMPTY set ⇒ `[]` (no NaN, no
 * divide-by-zero) ⇒ a wholly empty bar.
 */
export function barZonesOf(tasks: readonly Task[]): BarSegment[] {
  const total = tasks.length;
  if (total === 0) return [];
  const segments: BarSegment[] = [];
  for (const def of BAR_ZONE_DEFS) {
    const count = tasks.filter((t) => def.statuses.includes(t.status as TaskStatus)).length;
    if (count === 0) continue; // absent status ⇒ zone at 0 ⇒ no segment rendered
    segments.push({
      key: def.key,
      tone: taskStatusMeta(def.toneStatus).dot,
      heightPct: (count / total) * 100,
      pulse: def.pulse,
    });
  }
  return segments;
}

/**
 * One entry in the rail's ORDERED sequence — one per phase, top→bottom in phase order:
 *  - `done`      → a phase BEFORE the current one (a green done dot).
 *  - `bar`       → the CURRENT phase (the progress bar; `pct` = that phase's completion %, and the
 *    per-status `segments` — done/active/blocked/skipped — for the stacked bar).
 *  - `remaining` → a phase AFTER the current one (a neutral remaining dot).
 * Only the `bar` item carries `pct` / `segments`; the dots leave them `undefined`.
 */
export interface RailItem {
  readonly kind: RailItemKind;
  /** The CURRENT phase's completion %, 0..100 — set ONLY on the `bar` item. */
  readonly pct?: number;
  /** The stacked per-status zones (done/active/blocked/skipped) — set ONLY on the `bar` item. */
  readonly segments?: BarSegment[];
}

/**
 * The rail's closed-tab roll-up (Option B) as an ORDERED SEQUENCE — one item per phase, in phase
 * order, with the PROGRESS BAR sitting INLINE at the CURRENT phase's position (NOT pinned to the
 * bottom). For each phase `i`:
 *  - `i < currentIdx`  → a `done` dot (phase already finished, green / success).
 *  - `i == currentIdx` → the `bar` item = the CURRENT phase's completion % ONLY (not the global %),
 *    painted in the in_progress status colour (`BAR_FILL`). "The bar follows the current phase."
 *  - `i > currentIdx`  → a `remaining` dot (neutral, still to do).
 * So `items.length === (number of phases)` — the bar counts for the current phase — and when the
 * current phase is the FIRST one the bar is the FIRST item (top). Examples: 2 phases / phase 1
 * current ⇒ `[bar, remaining]`; 5 phases / phase 3 current ⇒ `[done, done, bar, remaining, remaining]`.
 *
 * CURRENT PHASE = the first phase not entirely done (`currentPhaseIndex`). EDGE CASES, all handled
 * without crash / NaN:
 *  - EVERY phase done ⇒ no current phase: a `done` dot per phase and NO bar at all.
 *  - FLAT spec (no phases) ⇒ one implicit phase = the whole task set: a SINGLE `bar` at the global %.
 *  - NO tasks ⇒ the bar (at the first phase) reads 0% and the later phases are `remaining` dots.
 */
export interface RailRollup {
  /** The ordered per-phase items (`done` dots, the `bar`, `remaining` dots), top→bottom. */
  readonly items: RailItem[];
}

export function railRollup(detail: SpecDetail): RailRollup {
  const phases = parsePhases(detail.spec.phases);

  // FLAT spec: one implicit phase covering ALL tasks — a SINGLE bar tracking the whole set's
  // progress (empty set ⇒ empty bar via `barPctOf`/`barZonesOf`).
  if (phases.length === 0) {
    return {
      items: [{ kind: "bar", pct: barPctOf(detail.tasks), segments: barZonesOf(detail.tasks) }],
    };
  }

  const perPhase = phases.map((p) => tasksOfPhase(detail.tasks, p.number));
  const currentIdx = currentPhaseIndex(perPhase);

  // EVERY phase done ⇒ no current phase: a done dot for each, NO bar.
  if (currentIdx === -1) {
    return { items: phases.map((): RailItem => ({ kind: "done" })) };
  }

  // ORDERED sequence, ONE item per phase: `done` before the current phase, the `bar` AT it,
  // `remaining` after — so the bar is inline at the current phase (first item when currentIdx === 0).
  const items: RailItem[] = phases.map((_, i): RailItem => {
    if (i < currentIdx) return { kind: "done" };
    if (i === currentIdx) {
      const tasks = perPhase[currentIdx];
      return { kind: "bar", pct: barPctOf(tasks), segments: barZonesOf(tasks) };
    }
    return { kind: "remaining" };
  });

  return { items };
}

/**
 * Constant WIDTH (px) shared by the DOT and the BAR (`w-1.5`) so the dot↔bar morph is PURELY
 * VERTICAL — only the height + radius change, never the width. Used as the inline width on every
 * rail phase item.
 */
export const RAIL_ITEM_WIDTH_PX = 6;
/** The DOT's height (px) — a `size-1.5` round pastille (the retracted state). */
export const RAIL_DOT_SIZE_PX = 6;
/** The BAR's fixed height (px) — the `h-16` vertical gauge (the stretched state). */
export const RAIL_BAR_HEIGHT_PX = 64;
/**
 * The BAR's corner RADIUS (px) — a soft rounded-RECT. It is deliberately SMALLER than the DOT's
 * fully-round radius (`RAIL_DOT_SIZE_PX / 2 = 3`) so the border-radius genuinely ANIMATES across the
 * morph (rond ↔ rounded-rect), and on a 6px-wide bar it rounds the ends without fully pilling them.
 */
export const RAIL_BAR_RADIUS_PX = 2;

/** The animated morph TARGET (height + border-radius) a rail phase item settles to for its `kind`. */
export interface RailItemMorph {
  /** The animated HEIGHT target (px): DOT = {@link RAIL_DOT_SIZE_PX}, BAR = {@link RAIL_BAR_HEIGHT_PX}. */
  readonly height: number;
  /** The animated border-RADIUS target (px): DOT = fully round (size ÷ 2), BAR = rounded-rect. */
  readonly borderRadius: number;
}

/**
 * The MORPH target (height + border-radius) a rail phase's `motion.div` animates to for a given
 * `kind`. The WIDTH is CONSTANT ({@link RAIL_ITEM_WIDTH_PX}) so these two values are the ONLY things
 * that change ⇒ a purely VERTICAL dot↔bar morph. When `currentPhaseIndex` moves, a phase's `kind`
 * flips (`bar → done` as it finishes, `remaining → bar` as it becomes current) and the SAME element
 * (keyed by phase index) animates between these targets:
 *  - `bar`                → the tall {@link RAIL_BAR_HEIGHT_PX} gauge with a rounded-RECT radius.
 *  - `done` / `remaining` → the tiny {@link RAIL_DOT_SIZE_PX} pastille, FULLY round (radius = size ÷ 2).
 */
export function railItemMorph(kind: RailItemKind): RailItemMorph {
  return kind === "bar"
    ? { height: RAIL_BAR_HEIGHT_PX, borderRadius: RAIL_BAR_RADIUS_PX }
    : { height: RAIL_DOT_SIZE_PX, borderRadius: RAIL_DOT_SIZE_PX / 2 };
}

/**
 * Phase-point fill tokens (Option B). `done` ⇒ the success dot (green). `remaining` ⇒ a VISIBLE
 * neutral: a solid `muted-foreground/60` dot with a crisp inset ring — deliberately MORE opaque
 * than the 45%-opacity scrollbar thumb behind the rail (globals.css `::-webkit-scrollbar-thumb`),
 * so it no longer BLENDS into the scrollbar the way the old `bg-muted-foreground/35` pip did, while
 * staying clearly distinct from the green done point.
 */
export const POINT_TONE: Record<PhasePointState, string> = {
  done: "bg-success",
  remaining: "bg-muted-foreground/60 ring-1 ring-inset ring-foreground/25",
};

/**
 * The rail bar's TRACK — a visible neutral that contrasts with BOTH the amber fill and the
 * scrollbar/languette behind it (more present than the old `bg-muted`, which was ~the scrollbar
 * colour and vanished).
 */
export const BAR_TRACK = "bg-muted-foreground/25";

/**
 * The CONTINUOUS base fill of a rail phase pill — the ONE always-mounted layer that gives the
 * dot↔bar morph its continuity (the pill is the SAME element that only changes HEIGHT, never a
 * crossfade of two views). Its colour follows the phase's `kind`:
 *  - `bar`       → the neutral {@link BAR_TRACK} (the empty todo track behind the stacked zones).
 *  - `done`      → the success dot tone (green) so a finished phase reads as a green dot.
 *  - `remaining` → the visible neutral {@link POINT_TONE} so a still-to-do phase reads as a neutral dot.
 * When the current phase advances, a phase's `kind` flips (`bar → done` / `remaining → bar`) and this
 * base fill's colour swaps in place UNDER the (converging/empty) zone stack while the shared HEIGHT
 * animation shrinks the bar to a dot (or grows the dot to a bar) — a continuous size morph, never a
 * swap of two representations.
 */
export function railFillTone(kind: RailItemKind): string {
  return kind === "bar" ? BAR_TRACK : POINT_TONE[kind];
}

/**
 * The drawer tab's single status POINT. Its COLOUR comes from the GLOBAL `statusMeta(spec.status)`
 * — the SAME source of truth as the status BADGE shown in the open panel (and the rail / Spec detail
 * page), so the closed tab's dot and the open panel's badge always read the same status (review
 * finding: "le dot doit prendre la couleur du badge global"). `statusMeta` already exposes a solid
 * `dot` token (`bg-*`), so no bespoke tones (the old orange `var(--product-claude)` / blue / green).
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

// NOTE: the drawer header/footer BADGE shows the Spec's REAL status via the global `statusMeta` (see
// `spec-live-panel.tsx`), exactly like the rail and the Spec detail page (review finding, test
// terrain). `drawerStatusPoint` above now derives the closed tab's status POINT colour from the SAME
// `statusMeta`, so the dot and the badge are coherent; the pulse stays as the separate "authoring
// live" liveness affordance.
