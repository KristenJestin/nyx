import type { CSSProperties } from "react";

import {
  clampRatio,
  DEFAULT_SPLIT_RATIO,
  MAX_SPLIT_RATIO,
  MIN_SPLIT_RATIO,
  type PaneEdge,
  type PaneNode,
  type PaneSplit,
  type SplitDirection,
} from "./pane-tree";

/**
 * PANE GEOMETRY — the pure projection of a {@link PaneNode} tree onto RECTANGLES.
 *
 * The host does NOT render nested flex boxes: it renders ONE absolutely-positioned slot per pane,
 * all siblings in a single container, positioned from the percentages computed here. That choice
 * is load-bearing:
 *  - a split/close RESTRUCTURES the tree but never moves a pane's DOM node to another parent, so
 *    the `<Terminal>` of an untouched pane is never unmounted/remounted (no detach+reattach churn,
 *    no lost focus, ONE DOM attachment per terminal at all times);
 *  - the rectangles are pure data, so the layout is unit-testable without a browser (jsdom has no
 *    layout engine), and phase 2's separators can be positioned from the same numbers.
 *
 * Rectangles are expressed in PERCENT of the host box, so the geometry is resolution-independent
 * and every resize is handled by the browser (and picked up by each terminal's ResizeObserver →
 * FitAddon → `pty_resize`, the existing and only geometry pipeline).
 */

/** A pane's box, in PERCENT of the host container. */
export interface PaneRect {
  readonly left: number;
  readonly top: number;
  readonly width: number;
  readonly height: number;
}

/** Where one pane sits, and which terminal it shows. */
export interface PanePlacement {
  readonly paneId: string;
  readonly terminalId: string;
  readonly rect: PaneRect;
}

/** The whole host box. */
const FULL_RECT: PaneRect = { left: 0, top: 0, width: 100, height: 100 };

/* ───────────────────────── THE ABSOLUTE PANE FLOOR (in CSS pixels) ─────────────────────────
 *
 * `MIN_SPLIT_RATIO` / `MAX_SPLIT_RATIO` bound a split's share of ITS PARENT — a RELATIVE floor,
 * and {@link paneRects} MULTIPLIES ratios as it descends, so nested splits COMPOSE those bounds
 * and the floor evaporates: three nested 15% splits leave a pane at 0.15³ ≈ 0.3% of the host
 * (the measured 18px of a 7-pane tiling). A terminal that narrow shows nothing usable, so the
 * relative band is doubled with an ABSOLUTE one expressed where the user actually reads it: in
 * pixels. The two are cumulative — the relative band still applies, the px floor tightens it.
 */

/** Smallest WIDTH a pane may ever have, in CSS pixels (~24 columns, and room for its toolbar). */
export const MIN_PANE_WIDTH_PX = 180;
/** Smallest HEIGHT a pane may ever have, in CSS pixels (~7 rows). */
export const MIN_PANE_HEIGHT_PX = 120;

/* ─────────────────────── THE PANE CONTROL CLUSTER — geometry ───────────────────────
 *
 * The cluster is the ONE surface where a mis-hit is destructive: aiming at "split right" and
 * reaching "close terminal" is a one-pixel accident. So its size is not left to the layout — it
 * is COMPUTED, and the pane's measured box decides which shape can be rendered without a single
 * button leaving the frame.
 *
 * IT LIVES HERE, next to the floor, because the two are COUPLED: "room for its toolbar" above is
 * exactly `paneControlsWidthPx(3) ≤ MIN_PANE_WIDTH_PX`. That inequality used to be an assertion
 * written in two comments in two different modules and nowhere in code — lowering the floor or
 * adding a fourth command would have broken it in silence, the only symptom being the overflow
 * fallback triggering permanently. `pane-geometry.test.ts` now pins it.
 *
 * The three constants below MIRROR Tailwind classes (`size-6`, `gap-1`, `top-1 right-1`), which is
 * the second coupling nothing enforced: a browser spec measures the RENDERED button against them,
 * so a class change without a constant change fails the suite instead of quietly making
 * `paneControlsMode` price a cluster that no longer fits.
 */

/** One control button's box (`size-6`), in CSS pixels. */
export const CONTROL_BUTTON_PX = 24;
/** The gap between two buttons (`gap-1`). */
export const CONTROL_GAP_PX = 4;
/** The inset from the pane's edges (`top-1 right-1`), counted on BOTH sides. */
export const CONTROL_INSET_PX = 4;

/** The px a pane needs for a cluster of `count` buttons to sit INSIDE its frame. */
export function paneControlsWidthPx(count: number): number {
  return count * CONTROL_BUTTON_PX + Math.max(0, count - 1) * CONTROL_GAP_PX + 2 * CONTROL_INSET_PX;
}

/** The px of HEIGHT any cluster needs (one row of buttons plus its insets). */
export const PANE_CONTROLS_HEIGHT_PX = CONTROL_BUTTON_PX + 2 * CONTROL_INSET_PX;

/**
 * THE STACK ORDER OF A PANE'S CHROME, in one place — because it is a THREE-WAY contract and it was
 * only ever written down in two of its three corners.
 *
 * Over a pane, three surfaces can meet on the same pixels:
 *
 *  1. the SEPARATOR band (`z-30` in `pane-tree-host.tsx`) — it straddles the boundary between two
 *     panes, so it reaches ~4px INTO each of them;
 *  2. the focused pane's SPEC SURCOUCHE (`[data-overlay-surface]`, this constant) — it is pinned to
 *     the pane's right edge, which for any pane that is not the rightmost IS a separator;
 *  3. the pane CONTROL CLUSTER (`z-40`) — F2's invariant: it outranks everything where they meet.
 *
 * The surcouche used to carry a hard-coded `z-20` and therefore LOST to the separator: on the
 * common two-pane layout the Spec languette of the LEFT pane went under the seam, so the strip the
 * user aims at was covered by an invisible 8px grab band. This constant is the missing middle term
 * — strictly above the separator, strictly below the cluster — and it is spent as an INLINE
 * `zIndex` rather than a Tailwind class so the number lives exactly once.
 */
export const OVERLAY_SURFACE_Z = 35;

/** Which shape a pane's box can hold: everything, the `…` fallback, or nothing at all. */
export type PaneControlsMode = "full" | "overflow" | "none";

/**
 * The shape a pane of `box` pixels can render WITHOUT any button leaving its frame.
 *
 * An UNMEASURED box (`null` — before the host's first measurement, and in jsdom, which has no
 * layout engine) renders the full cluster: the fallback is a response to a measurement, never a
 * guess. The `none` case only exists for a window shrunk far under the absolute pane floor
 * ({@link MIN_PANE_WIDTH_PX} keeps panes at 180×120px, and 180 was chosen precisely so the full
 * cluster always fits); there the chords (Ctrl+Shift+arrows) remain the way in.
 */
export function paneControlsMode(
  box: { readonly width: number; readonly height: number } | null,
  count: number,
): PaneControlsMode {
  if (box == null) return "full";
  if (box.height < PANE_CONTROLS_HEIGHT_PX) return "none";
  if (box.width >= paneControlsWidthPx(count)) return "full";
  return box.width >= paneControlsWidthPx(1) ? "overflow" : "none";
}

/** The host box in CSS PIXELS — the yardstick that turns a percent rect into a real size. */
export interface HostSize {
  readonly width: number;
  readonly height: number;
}

/** Which axis a floor/demand is expressed on. */
export type PaneAxis = "width" | "height";

/** The floor on `axis`, in CSS pixels. */
export function minPanePx(axis: PaneAxis): number {
  return axis === "width" ? MIN_PANE_WIDTH_PX : MIN_PANE_HEIGHT_PX;
}

/**
 * The PIXELS a subtree needs along ONE axis to keep EVERY pane it contains above the floor, AT ITS
 * CURRENT RATIOS — the composition rule the relative clamp is missing:
 *  - a LEAF demands the floor itself;
 *  - a split laid out ACROSS the axis (a `column` for `width`) demands the MAX of its children —
 *    they each get the whole axis;
 *  - a split laid out ALONG it (a `row` for `width`) shares the axis by `ratio`, so it demands
 *    `max(first / ratio, second / (1 - ratio))`: the span at which its TIGHTEST side reaches the
 *    floor. NOT the plain sum — the ratios are NOT rebalanced when an ancestor moves (explicitly
 *    out of scope), so a 70/30 split of two panes needs 600px, not 360.
 *
 * This is what makes the floor survive NESTING: the outermost separator of a tiling whose right
 * half holds three panes knows exactly how few pixels that half can take before one of them —
 * three levels down — falls under 180px.
 */
export function minPaneSpanPx(node: PaneNode, axis: PaneAxis): number {
  if (node.kind === "leaf") return minPanePx(axis);
  const first = minPaneSpanPx(node.children[0], axis);
  const second = minPaneSpanPx(node.children[1], axis);
  const along = axis === "width" ? "row" : "column";
  if (node.direction !== along) return Math.max(first, second);
  // The REAL ratio (`clampRatio` only guards against a hand-built 0 / 1 / NaN — it does NOT pull
  // the value back into the aesthetic 15..85 band, which a split may legitimately sit outside).
  const ratio = clampRatio(node.ratio);
  return Math.max(first / ratio, second / (1 - ratio));
}

/**
 * The rectangle of every pane, in leaf order. A `row` split gives its FIRST child `ratio` of the
 * WIDTH (side by side); a `column` split gives it `ratio` of the HEIGHT (stacked). Children tile
 * their parent exactly — no gap, no overlap — so the panes always cover the host.
 */
export function paneRects(root: PaneNode, bounds: PaneRect = FULL_RECT): PanePlacement[] {
  if (root.kind === "leaf") {
    return [{ paneId: root.id, terminalId: root.terminalId, rect: bounds }];
  }
  const [first, second] = splitChildBounds(bounds, root.direction, root.ratio);
  return [...paneRects(root.children[0], first), ...paneRects(root.children[1], second)];
}

/**
 * THE ONE EXPRESSION of "how a split cuts its own box": the FIRST child takes `ratio` of the axis,
 * the SECOND takes the REMAINDER — a subtraction, not `1 − ratio`, so the two always tile the
 * parent exactly (no gap, no overlap, whatever the ratio's last bits say).
 *
 * It lives alone because THREE readers need the same cut and any disagreement between them is a
 * geometry bug that compiles: {@link paneRects} (where the panes go), {@link paneSeparators} (the
 * sub-box a drag maps onto) and {@link compactPaneRatios} (the sub-box a restored ratio is judged
 * against). Two of them used to carry their own copy.
 */
function splitChildBounds(
  bounds: PaneRect,
  direction: SplitDirection,
  ratio: number,
): [PaneRect, PaneRect] {
  if (direction === "row") {
    const width = bounds.width * ratio;
    return [
      { ...bounds, width },
      { ...bounds, left: bounds.left + width, width: bounds.width - width },
    ];
  }
  const height = bounds.height * ratio;
  return [
    { ...bounds, height },
    { ...bounds, top: bounds.top + height, height: bounds.height - height },
  ];
}

/** The placements keyed by TERMINAL id — what the host looks up per mounted pane. */
export function paneRectsByTerminal(root: PaneNode | null): Map<string, PanePlacement> {
  const map = new Map<string, PanePlacement>();
  if (root == null) return map;
  for (const placement of paneRects(root)) map.set(placement.terminalId, placement);
  return map;
}

/** A percent rect resolved against a measured host — the pane's real box, in CSS pixels. */
export function paneRectPx(rect: PaneRect, host: HostSize): { width: number; height: number } {
  return { width: (host.width * rect.width) / 100, height: (host.height * rect.height) / 100 };
}

/**
 * Can `paneId` be SPLIT along `direction` WITHOUT dropping either half under the absolute floor?
 *
 * A fresh split cuts at {@link DEFAULT_SPLIT_RATIO}, so the pane must measure at least TWICE the
 * floor on the axis being cut (360px wide for a "split right", 240px tall for a "split down").
 * The OTHER axis is untouched by the cut and is deliberately NOT tested: a window that shrank
 * under an existing tiling must not turn every split into a refusal on an axis it does not touch.
 *
 * A host that is not measurable yet (`null`, or a 0px box before the first layout) CANNOT refuse:
 * this is a floor, not a gate — an unmeasured split proceeds exactly as it did before.
 */
export function canSplitPane(
  root: PaneNode | null,
  paneId: string,
  direction: SplitDirection,
  host: HostSize | null,
): boolean {
  if (root == null || host == null) return true;
  if (!(host.width > 0) || !(host.height > 0)) return true;
  const placement = paneRects(root).find((p) => p.paneId === paneId);
  if (placement == null) return true;
  const box = paneRectPx(placement.rect, host);
  const axis: PaneAxis = direction === "row" ? "width" : "height";
  const span = axis === "width" ? box.width : box.height;
  if (!(span > 0)) return true;
  const first = span * DEFAULT_SPLIT_RATIO;
  return first >= minPanePx(axis) && span - first >= minPanePx(axis);
}

/** The absolute-positioning style of a pane slot (percent units, no layout maths in JSX). */
export function paneRectStyle(rect: PaneRect): CSSProperties {
  return {
    position: "absolute",
    left: `${rect.left}%`,
    top: `${rect.top}%`,
    width: `${rect.width}%`,
    height: `${rect.height}%`,
  };
}

/**
 * The drop-indicator overlay for a pane dragged onto the `edge` of a target whose rect is `rect`
 * (percent of the host, the SAME units {@link paneRects} speaks): HALF of the target's box, on the
 * hovered side — where the moved pane will actually land once the split at 50/50 is committed.
 *
 * Pure geometry, same units as every other rect here, so a browser spec can assert the indicator's
 * measured box against the target pane's own box without re-deriving any of it.
 */
export function paneDropIndicatorStyle(rect: PaneRect, edge: PaneEdge): CSSProperties {
  const half = { position: "absolute" } as const;
  switch (edge) {
    case "left":
      return {
        ...half,
        left: `${rect.left}%`,
        top: `${rect.top}%`,
        width: `${rect.width / 2}%`,
        height: `${rect.height}%`,
      };
    case "right":
      return {
        ...half,
        left: `${rect.left + rect.width / 2}%`,
        top: `${rect.top}%`,
        width: `${rect.width / 2}%`,
        height: `${rect.height}%`,
      };
    case "up":
      return {
        ...half,
        left: `${rect.left}%`,
        top: `${rect.top}%`,
        width: `${rect.width}%`,
        height: `${rect.height / 2}%`,
      };
    case "down":
      return {
        ...half,
        left: `${rect.left}%`,
        top: `${rect.top + rect.height / 2}%`,
        width: `${rect.width}%`,
        height: `${rect.height / 2}%`,
      };
  }
}

/* ────────────────────────────── SEPARATORS ──────────────────────────────
 *
 * A SEPARATOR is the draggable band sitting on a split's boundary. Like the pane slots it is an
 * absolutely-positioned sibling of the tiles (never a wrapper around them), so adding, moving or
 * removing one NEVER reparents a `<Terminal>` — the zero-remount property the flat host is built
 * on. It carries the split's BOUNDS (the box the split's two children tile) because that box, not
 * the whole host, is the axis a drag maps onto: a nested split's separator must translate a
 * pointer position into a ratio of ITS OWN sub-box.
 */

/**
 * Thickness of a separator's HIT BAND, in CSS pixels. The visible boundary is the panes' own
 * 1px outline; this band is the (invisible until hovered) grab target, centred on the boundary —
 * wide enough to hit with a mouse, narrow enough never to swallow a click meant for a terminal.
 */
export const SEPARATOR_THICKNESS_PX = 8;

/**
 * Which half of a separator interaction a ratio change belongs to: a mid-drag `preview` (apply it
 * live so the panes — and through the existing ResizeObserver/FitAddon pipeline their PTYs —
 * follow the pointer, but do NOT write it down) or the `commit` that ENDS the interaction (the one
 * persisted write, so a drag costs a single `localStorage` round-trip instead of one per frame).
 */
export type SplitResizePhase = "preview" | "commit";

/** One split's separator: where it sits, which axis it moves, and its current ratio. */
export interface PaneSeparator {
  /** The split whose `ratio` this separator edits ({@link import("./pane-tree").setSplitRatio}). */
  readonly splitId: string;
  /** `row` ⇒ a VERTICAL bar moving left/right; `column` ⇒ a HORIZONTAL one moving up/down. */
  readonly direction: SplitDirection;
  readonly ratio: number;
  /** The box the split's two children tile, in PERCENT of the host — the drag's own axis. */
  readonly bounds: PaneRect;
  /**
   * The PIXELS the FIRST child needs on this split's axis for every pane INSIDE it to stay above
   * the floor ({@link minPaneSpanPx}) — the number that makes the absolute floor survive nesting.
   */
  readonly minFirstPx: number;
  /** Same, for the SECOND child. */
  readonly minSecondPx: number;
}

/** A closed band of ratios. `min > max` means it is EMPTY — no ratio satisfies it. */
export interface RatioBand {
  readonly min: number;
  readonly max: number;
}

/**
 * How short a sub-box may fall of its two demands before it is BELIEVED starved — as a FRACTION of
 * that sub-box, never as a fixed number of pixels.
 *
 * WHY IT EXISTS. The demands ({@link minPaneSpanPx}) divide pixels by ratios that are themselves
 * quotients, so a sub-box that is EXACTLY satisfiable reads a hair short: a 720px sub-box owing
 * 540 + 180px computes `minFirstPx = 540.000000000000114`, i.e. 1.1e-13px of "starvation". And the
 * tipping point is the DEFAULT destination of a gesture, not a fluke — every drag and every
 * Home/End is clamped EXACTLY onto a bound ({@link clampToBounds}). Believed literally, that noise
 * flips a healthy tiling into the STARVED regime at a CONSTANT window size, which is how a 720×900
 * window, five keystrokes and no resize at all used to park a pane at 36px (review round 2).
 *
 * WHY A FRACTION, and not the 0.05px this used to be (review round 3, critical). A tolerance here
 * is not only a comparison: a band accepted as feasible may still have its ends CROSSED, and the
 * ratio then pinned (below) CONCEDES that crossing to the panes underneath. The concession is
 * inherited by every pane of the subtree as a RELATIVE deficit — and re-read at an ANCESTOR, a
 * relative deficit is worth that same fraction of the ANCESTOR's much larger box, i.e. MORE PIXELS
 * the higher it climbs. An ABSOLUTE tolerance therefore refutes itself: 0.05px conceded on a
 * ~1500px sub-box reads as 0.064px of starvation at a 1920px root — 1.29× the very same 0.05px —
 * so the root fell into the STARVED regime AT A CONSTANT WINDOW, handed back 15..85, and crushed a
 * pane to 19.17px.
 * Nothing about that is IEEE-754: replayed in exact rational arithmetic, the root really claims
 * 1740.0643 + 180px of a 1920px box. The concession was real; only its accounting was wrong.
 *
 * HOW TO FALSIFY IT, and what each half really costs — the four corners, all replayed (round 4;
 * the round-3 figure of 329486 was correct but was attributed to this line alone, which it is not):
 *   relative ε + PROPORTIONAL pinning (as shipped) ..... 61/61 green, 0 violations;
 *   ABSOLUTE ε (this line) + proportional pinning ...... 1 red — and it is `pane-geometry.test.ts`'s
 *       SHAPE assertion, "180px scales: expected 1 to be close to 2". The ten-host walk stays
 *       GREEN, 0 violations at 1920×1080. This line alone costs nothing the WALK can see;
 *   ABSOLUTE ε + MIDPOINT pinning ...................... 9 red, "329486 violation(s) of the
 *       180 × 120px floor at a constant 1920 × 1080 host" (the reviewer read 571493 on the looser
 *       assertion of the time). THAT is where the number comes from: the PAIR, not one line;
 *   relative ε + MIDPOINT pinning ...................... 8 red, 144067 at 1600×900 and 357459 at
 *       1920×1080.
 * So the pinning carries the MEASURE and this constant carries the PROOF: it is what makes the
 * concession's accounting closed, and only the scaling test can catch its absence.
 *
 * A FRACTION is invariant under that climb — a crossing of at most `ε × span` conceded here is
 * worth at most `ε × span` at every ancestor — so the tolerance can no longer be overrun by what
 * it itself allowed. That closure needs the PROPORTIONAL pinning of {@link separatorRatioBounds}
 * as its second half: the two are one mechanism (the proof is written there).
 *
 * THE VALUE. A BILLIONTH of the sub-box: 1.9e-6px at a 1920px box, 7.7e-6px at a 7680px one —
 * four orders of magnitude under one device pixel, so it can never hide a starvation a user could
 * see (the real ones come from a window shrinking by whole pixels). Measured against the drift it
 * must absorb — over a sweep of 14 host sizes × 4 seeds × pointer AND keys (the ten-host walk below
 * is its checked-in subset), the worst relative shortfall still judged feasible is 2.665e-15, about
 * 12 ULPs, and no pane ever ran more than 7.4e-13px under its floor. That is a margin of ~3.8e5.
 * `pane-geometry.test.ts` pins the value, and ASSERTS that margin rather than assuming it.
 */
export const FLOOR_EPSILON_RATIO = 1e-9;

/** That tolerance in CSS pixels, for a sub-box of `span` px — what the comparison actually uses. */
export function floorEpsilonPx(span: number): number {
  return span * FLOOR_EPSILON_RATIO;
}

/**
 * THE ONE EXPRESSION of the px-floor rule — the band in which BOTH sides of the split stand at or
 * above the absolute floor, `[minFirstPx / span, 1 − minSecondPx / span]`, plus the PIXELS the
 * sub-box has left once they both do. `null` when there is nothing to measure.
 *
 * Negative slack — the two demands out-claiming the sub-box — is the one definition of "starved",
 * and `min` crossing `max` is the same statement divided by `span`. {@link separatorRatioBounds}
 * and {@link separatorFloorSatisfiable} both read the answer from HERE rather than each
 * re-deriving the inequality (review finding: the same comparison used to be written twice in this
 * module and a third time in its test, so a change to one could leave the others compiling,
 * linting — and lying).
 */
function pxFloorBand(separator: PaneSeparator, host: HostSize | null): PxFloorBand | null {
  const span = separatorSpanPx(separator, host);
  if (span == null) return null;
  return {
    min: separator.minFirstPx / span,
    max: 1 - separator.minSecondPx / span,
    slackPx: span - separator.minFirstPx - separator.minSecondPx,
    spanPx: span,
  };
}

/** A {@link pxFloorBand}, with the sub-box it was cut from and the pixels it has left over. */
interface PxFloorBand extends RatioBand {
  /** The pixels the sub-box has left once both demands are paid — negative ⇒ starved. */
  readonly slackPx: number;
  /** The sub-box itself, on this split's axis — the yardstick {@link floorEpsilonPx} scales to. */
  readonly spanPx: number;
}

/**
 * Can this sub-box really pay both demands, once {@link FLOOR_EPSILON_RATIO} of its OWN size is
 * discounted? The tolerance scales with the sub-box on purpose — see the constant.
 */
function bandIsFeasible(band: PxFloorBand): boolean {
  return band.slackPx >= -floorEpsilonPx(band.spanPx);
}

/**
 * The RATIO BAND this split may really use. This is the single source the drag, the keys AND the
 * separator's `aria-valuemin` / `aria-valuemax` read: what a screen reader announces is the course
 * that really exists, not the constant 15 / 85.
 *
 * An UNMEASURED host (`null` / 0px, or a degenerate sub-box) has no px to reason with and falls
 * back to the relative band.
 *
 * THREE REGIMES, in the order they are decided:
 *
 * 1. NOMINAL — the px band and the relative {@link MIN_SPLIT_RATIO}..{@link MAX_SPLIT_RATIO} band
 *    OVERLAP. Their intersection is the answer: the two are cumulative, the tighter one wins on
 *    each end. This is the F1 behaviour, and the vast majority of the time.
 *
 * 2. SATURATED BUT NOT STARVED — the sub-box CAN pay both px demands, but doing so needs a ratio
 *    outside 15..85 (a shrunk window under an existing tiling: the ratios are never rebalanced,
 *    that is out of scope, so one side's demand grows past the relative ceiling while the box
 *    still has room). USER ARBITRAGE: THE PX FLOOR WINS. The band is the px one, both panes keep
 *    their 180×120px, and the ratio leaves 15..85 — assumed, because 15..85 is a heuristic of
 *    proportion and the px floor is the usability constraint. `pane-tree` stores such a ratio
 *    (see its `MIN_STORED_RATIO`), so the seam really reaches it.
 *
 * 3. STARVED — the sub-box CANNOT pay both demands, whatever the ratio. The floor reverts to
 *    BEST-EFFORT and the relative 15..85 course is handed back, instead of collapsing the band
 *    onto the current ratio. WHY: the window is already too small and BOTH panes are already
 *    under the floor; freezing the seam protects nothing any more, and it removes the one manual
 *    recovery the user had before the floor existed (drag a pane out to a readable width). The
 *    floor was introduced to stop a pane from being CREATED unusable — that arbitrage lives in
 *    {@link canSplitPane}, which stays STRICT — not to lock the RESIZING of a layout already on
 *    screen. `aria-valuemin` / `aria-valuemax` follow the same band, so a screen reader is told
 *    about the course that really exists (15..85) rather than being handed two equal bounds that
 *    read as "this widget cannot move".
 */
export function separatorRatioBounds(separator: PaneSeparator, host: HostSize | null): RatioBand {
  const relative = { min: MIN_SPLIT_RATIO, max: MAX_SPLIT_RATIO };
  const floor = pxFloorBand(separator, host);
  // (3) STARVED — or nothing to measure.
  if (floor == null || !bandIsFeasible(floor)) return relative;
  // Feasible, but its ends may still CROSS — by at most `FLOOR_EPSILON_RATIO` of the sub-box,
  // which is what "feasible" was just allowed to mean. Handing an inverted band to the clamp is
  // meaningless, so it is pinned onto one point; WHICH point is the other half of the tolerance's
  // closure argument, not a detail:
  //   the crossing `c = minFirstPx + minSecondPx − span` has to come out of SOMEONE's demand, and
  //   the PROPORTIONAL point `minFirstPx / (minFirstPx + minSecondPx)` takes it from both in
  //   proportion, leaving EACH side short by the same FRACTION `c / (minFirstPx + minSecondPx)`.
  //   Since a crossing means `minFirstPx + minSecondPx ≥ span`, that fraction is at most
  //   `c / span ≤ FLOOR_EPSILON_RATIO` — so the relative deficit conceded here is bounded by the
  //   tolerance itself, at every ancestor that re-reads it, and can never grow into a starvation.
  //   The MIDPOINT this used to use takes `c / 2` from EACH side instead, so the smaller demand
  //   pays a fraction `c / (2 × minFirstPx)` — unbounded as one side shrinks towards the floor,
  //   and hence a starvation manufactured one level up (review round 3, critical).
  const point = separator.minFirstPx / (separator.minFirstPx + separator.minSecondPx);
  const px = floor.min <= floor.max ? floor : { min: point, max: point };
  const min = Math.max(MIN_SPLIT_RATIO, px.min);
  const max = Math.min(MAX_SPLIT_RATIO, px.max);
  // (1) NOMINAL when the two bands overlap, (2) SATURATED — the px floor wins — when they do not.
  // The saturated answer is PROJECTED onto a bare `RatioBand` rather than handing `px` back: `px`
  // may be the {@link PxFloorBand} itself, and TS's structural typing cannot see the two extra
  // fields ride out through a `RatioBand` signature. That is not cosmetic — the regime is reached
  // at an ordinary 1920×1080 (4 of the 16 seams of the deep nesting in `pane-geometry.test.ts`),
  // and `toEqual` compares the WHOLE shape, so every `.not.toEqual({min, max})` written against a
  // leaked band passes VACUOUSLY: an assertion that compiles, lints — and cannot fail (round 5).
  return min <= max ? { min, max } : { min: px.min, max: px.max };
}

/** This split's OWN sub-box, on ITS axis, in CSS px — `null` when there is nothing to measure. */
function separatorSpanPx(separator: PaneSeparator, host: HostSize | null): number | null {
  if (host == null) return null;
  const span =
    separator.direction === "row"
      ? (host.width * separator.bounds.width) / 100
      : (host.height * separator.bounds.height) / 100;
  return span > 0 ? span : null;
}

/**
 * Can this split's sub-box hold BOTH sides at the absolute floor, at their current ratios?
 *
 * `false` is the STARVED regime {@link separatorRatioBounds} hands the relative band back in: the
 * box shrank under an existing tiling (the ratios are never rebalanced, out of scope), so NO ratio
 * satisfies both sides and the px floor becomes best-effort. A sub-box that can pay both demands
 * is satisfiable EVEN IF that costs a ratio outside 15..85 (regime 2 above): the answer comes from
 * {@link pxFloorBand}, the same expression the bounds are cut from — never a second copy of it.
 * An UNMEASURED host cannot refute anything, so it is `true`.
 */
export function separatorFloorSatisfiable(
  separator: PaneSeparator,
  host: HostSize | null,
): boolean {
  const floor = pxFloorBand(separator, host);
  return floor == null || bandIsFeasible(floor);
}

/** Clamp a ratio into the band {@link separatorRatioBounds} allows for THIS split. */
function clampToBounds(ratio: number, bounds: RatioBand): number {
  if (!Number.isFinite(ratio)) return bounds.min;
  return Math.min(bounds.max, Math.max(bounds.min, ratio));
}

/**
 * THE ONE EXPRESSION of "what a split DEMANDS of its own sub-box" — the {@link PaneSeparator} a
 * split becomes once it is given a box: its axis, and the floor of EACH SIDE composed over that
 * side's whole subtree ({@link minPaneSpanPx}), not one pane's floor.
 *
 * It lives alone for the same reason {@link splitChildBounds} does, and between the same readers:
 * {@link paneSeparators} (the seam a drag maps onto) and {@link compactPaneRatios} (the seam a
 * restored ratio is judged against) must judge a split by the SAME demand, or the restore accepts
 * ratios no drag could reach — a geometry bug that compiles. They used to build it twice, field
 * for field (review round 4).
 */
function seamOf(node: PaneSplit, bounds: PaneRect): PaneSeparator {
  const axis: PaneAxis = node.direction === "row" ? "width" : "height";
  return {
    splitId: node.id,
    direction: node.direction,
    ratio: node.ratio,
    bounds,
    minFirstPx: minPaneSpanPx(node.children[0], axis),
    minSecondPx: minPaneSpanPx(node.children[1], axis),
  };
}

/**
 * Every separator of the tree, outermost first — one per split, in the same recursion (and hence
 * the same bounds) as {@link paneRects}, so a separator can never disagree with the panes it sits
 * between. `null` (no layout) has none.
 */
export function paneSeparators(
  root: PaneNode | null,
  bounds: PaneRect = FULL_RECT,
): PaneSeparator[] {
  if (root == null || root.kind === "leaf") return [];
  const [first, second] = root.children;
  const self = seamOf(root, bounds);
  const [firstBounds, secondBounds] = splitChildBounds(bounds, root.direction, root.ratio);
  return [self, ...paneSeparators(first, firstBounds), ...paneSeparators(second, secondBounds)];
}

/**
 * PULL EVERY RATIO OF A TREE BACK INTO WHAT ITS OWN SUB-BOX ALLOWS at `host` — the px half of the
 * restore's validation (review round 3).
 *
 * `isPaneNode` validates the SHAPE of a persisted tree and `pruneStaleTerminals` reconciles its
 * TERMINALS, but nothing looked at a single PIXEL: a hand-edited or corrupted
 * `nyx.ui.paneLayout:<projectId>` holding a ratio of 0.98 (inside the storable band, so accepted
 * wholesale) restored a 14.40px pane on a 720px host. No GESTURE can write such a ratio — every
 * drag and every key goes through {@link separatorRatioBounds} first — so this is the second line
 * of defence for the one path that bypasses them, and it repairs instead of rejecting: throwing the
 * tree away over one bad number would cost the user the whole tiling.
 *
 * Each pass walks TOP-DOWN, because clamping a seam moves every sub-box beneath it, and it clamps
 * through the very band the gestures use, so a restored layout can hold no ratio a drag could not
 * have produced at that host. TWO cases are deliberately left ALONE:
 *  - an UNMEASURED host (`null` / 0px — jsdom, or a page whose tiling is not on screen yet) has no
 *    pixels to judge with, exactly as {@link canSplitPane} has none;
 *  - a STARVED sub-box, the arbitrated best-effort regime: no ratio satisfies both sides there, so
 *    moving the seam would only choose a victim, and the user's manual recovery (drag one pane back
 *    over the floor) is the arbitrage that was signed off.
 *
 * AND THE PASSES REPEAT TO A FIXPOINT, because a repair LOWERS the demand of the subtree it
 * repaired: a 0.98 nested inside a 0.98 claims 9000px, so on a 1600px host the OUTER seam reads as
 * starved and is (rightly) left alone — but once the inner one is back in band its demand falls to
 * 1200px and the outer one becomes repairable. One pass would have left a 32px pane behind. Each
 * pass either changes the tree or ends the loop, and the tree's own depth bounds how many can.
 *
 * An UNCHANGED subtree is returned BY REFERENCE, and that identity is the loop's STOP CONDITION
 * (`next === current` above), not a micro-optimisation: a tree with nothing to move settles in ONE
 * pass. What it does NOT promise — and used to claim — is that a HEALTHY layout always comes back
 * as the very same object. `clampToBounds` re-derives each band from the CURRENT sub-box, and a
 * ratio a gesture parked EXACTLY on a band end (which every drag and every Home/End does) gets
 * re-clamped onto a bound that can differ in its last bits. Measured over a 38 400-state gesture
 * walk across 12 hosts, each state compacted at its own host: 16% came back by reference, 84% were
 * rebuilt — and 93% of those rebuilds moved no ratio by more than 1e-9, i.e. nothing a pixel could
 * show (the round-4 audit's own walk read 49.5% / 50.5%, same conclusion). The rest are REAL moves:
 * a ratio legitimately outside 15..85 stops being reachable once a pane CLOSE drops its subtree's
 * demand, and this pass then pulls it back onto the ceiling — up to 108px at 1920×1080 on a layout
 * that had nothing to repair. `pane-geometry.test.ts` pins both, with `toBe`.
 */
export function compactPaneRatios(root: PaneNode, host: HostSize | null): PaneNode {
  if (host == null || !(host.width > 0) || !(host.height > 0)) return root;
  let current = root;
  for (let pass = 0; pass <= paneTreeDepth(root); pass++) {
    const next = compactPaneRatiosOnce(current, host);
    if (next === current) break;
    current = next;
  }
  return current;
}

/** Longest root-to-leaf path — the number of levels a repair can have to climb back through. */
function paneTreeDepth(node: PaneNode): number {
  if (node.kind === "leaf") return 0;
  return 1 + Math.max(paneTreeDepth(node.children[0]), paneTreeDepth(node.children[1]));
}

/** ONE top-down repair pass of {@link compactPaneRatios} — same tree back when nothing moved. */
function compactPaneRatiosOnce(root: PaneNode, host: HostSize): PaneNode {
  const walk = (node: PaneNode, bounds: PaneRect): PaneNode => {
    if (node.kind === "leaf") return node;
    const seam = seamOf(node, bounds);
    // `clampRatio` LAST, and it is not belt-and-braces: `separatorRatioBounds` answers in the PX
    // band, which in the SATURATED regime is handed back RAW (regime 2) and can sit entirely
    // OUTSIDE the storable 2..98% — a sub-box over `MIN_PANE_WIDTH_PX / (1 − MAX_STORED_RATIO)`
    // = 9000px wide (or 6000px tall) can demand more than 98% for its first side, and the clamp
    // onto that band then writes a ratio `isPaneNode` REJECTS at the next launch, costing the user
    // the whole tiling — the very outcome this function exists to prevent (review round 4, major:
    // 5 panes at 9001×4320 restored as 1). Every OTHER ratio writer already ends here
    // (`setSplitRatio`, `splitPane`); this one used to be the single path that did not.
    // It is applied to the CLAMPED value, so the identity shortcut below still sees the ratio that
    // is really written and a healthy tree still comes back by reference.
    const ratio = separatorFloorSatisfiable(seam, host)
      ? clampRatio(clampToBounds(node.ratio, separatorRatioBounds(seam, host)))
      : node.ratio;
    const [firstBounds, secondBounds] = splitChildBounds(bounds, node.direction, ratio);
    const first = walk(node.children[0], firstBounds);
    const second = walk(node.children[1], secondBounds);
    if (ratio === node.ratio && first === node.children[0] && second === node.children[1]) {
      return node;
    }
    return { ...node, ratio, children: [first, second] };
  };
  return walk(root, FULL_RECT);
}

/**
 * The absolute-positioning style of a separator's hit band: it spans its split's box across the
 * axis and is CENTRED on the boundary (`calc(<boundary>% - <half thickness>px)`), so the grab area
 * straddles the seam instead of eating one side's first pixels.
 */
export function separatorStyle(
  separator: PaneSeparator,
  thickness: number = SEPARATOR_THICKNESS_PX,
): CSSProperties {
  const { bounds, ratio } = separator;
  const half = thickness / 2;
  if (separator.direction === "row") {
    return {
      position: "absolute",
      top: `${bounds.top}%`,
      height: `${bounds.height}%`,
      left: `calc(${bounds.left + bounds.width * ratio}% - ${half}px)`,
      width: `${thickness}px`,
    };
  }
  return {
    position: "absolute",
    left: `${bounds.left}%`,
    width: `${bounds.width}%`,
    top: `calc(${bounds.top + bounds.height * ratio}% - ${half}px)`,
    height: `${thickness}px`,
  };
}

/**
 * The ratio a POINTER at `pointer` (in CSS pixels RELATIVE to the host box) asks for — the pure
 * half of the drag: the host measures its box once, this maps the position onto the split's own
 * sub-box and clamps it into {@link separatorRatioBounds}. A drag pushed past the ABSOLUTE floor
 * therefore BUTS on it (in pixels, for this split's real sub-box) instead of walking through it.
 * A degenerate host or split box (0px on the axis) is not measurable, so the current ratio is kept
 * rather than dividing by zero.
 */
export function ratioFromPointer(
  separator: PaneSeparator,
  pointer: { readonly x: number; readonly y: number },
  host: HostSize,
): number {
  const { bounds } = separator;
  const [position, origin, span] =
    separator.direction === "row"
      ? [pointer.x, (host.width * bounds.left) / 100, (host.width * bounds.width) / 100]
      : [pointer.y, (host.height * bounds.top) / 100, (host.height * bounds.height) / 100];
  if (!(span > 0)) return clampRatio(separator.ratio);
  return clampToBounds((position - origin) / span, separatorRatioBounds(separator, host));
}

/** One arrow key's share of the split's axis (2%) — the fine keyboard step. */
export const SEPARATOR_KEY_STEP = 0.02;
/** One Page Up/Down's share (10%) — the coarse keyboard step. */
export const SEPARATOR_KEY_PAGE_STEP = 0.1;

/**
 * The ratio a KEY asks for, or `null` when the key is not a resize key (the host then leaves the
 * event alone). The ARIA window-splitter mapping, resolved against the separator's OWN axis so the
 * keys always match what the user sees:
 *  - a VERTICAL bar (`row` split) moves with ArrowLeft / ArrowRight;
 *  - a HORIZONTAL one (`column` split) moves with ArrowUp / ArrowDown;
 *  - PageUp / PageDown take the coarse step in the same two directions;
 *  - Home / End jump to the bounds — the REAL ones ({@link separatorRatioBounds}), so End cannot
 *    park a pane under the absolute floor any more than a drag can.
 * Every result is clamped into that same band, so no key sequence can walk a pane out of it. An
 * unmeasured `host` falls back to the relative band (the pre-floor behaviour).
 */
export function ratioFromKey(
  separator: PaneSeparator,
  key: string,
  host: HostSize | null = null,
): number | null {
  const back = separator.direction === "row" ? "ArrowLeft" : "ArrowUp";
  const forward = separator.direction === "row" ? "ArrowRight" : "ArrowDown";
  const bounds = separatorRatioBounds(separator, host);
  switch (key) {
    case back:
      return clampToBounds(separator.ratio - SEPARATOR_KEY_STEP, bounds);
    case forward:
      return clampToBounds(separator.ratio + SEPARATOR_KEY_STEP, bounds);
    case "PageUp":
      return clampToBounds(separator.ratio - SEPARATOR_KEY_PAGE_STEP, bounds);
    case "PageDown":
      return clampToBounds(separator.ratio + SEPARATOR_KEY_PAGE_STEP, bounds);
    case "Home":
      return bounds.min;
    case "End":
      return bounds.max;
    default:
      return null;
  }
}

/* ─────────────────────── DIRECTIONAL FOCUS — the Alt+arrows neighbour ───────────────────────
 *
 * "The neighbour in that direction" is resolved on the GEOMETRY, never on leaf order: the tree's
 * reading order says nothing about what sits to the LEFT of a pane once splits nest, and a
 * user pressing Alt+← is pointing at the screen, not at the tree.
 *
 * The rule, from the focused pane's rect:
 *   1. keep the panes whose facing EDGE touches ours on the asked side (its right edge is our
 *      left edge, etc. — a tolerance absorbs the float drift of multiplied ratios);
 *   2. keep those that OVERLAP us on the perpendicular axis (a pane above-left of us is not to
 *      our left);
 *   3. among the survivors take the one whose CENTRE is closest to ours on that perpendicular
 *      axis — the "several candidates" tie-break;
 *   4. no survivor ⇒ NO MOVE. The focus does not wrap and does not jump to the far side.
 */

/** The four directions Alt+arrows can ask for. */
export type FocusDirection = "left" | "right" | "up" | "down";

/**
 * Tolerance, in PERCENT of the host, for "these two edges touch". Rects come from multiplying
 * ratios down the tree, so an edge shared by two panes can differ in the last float bits; 0.01%
 * of the host is far below one pixel on any real screen and far above that drift.
 */
const EDGE_EPSILON_PCT = 0.01;

/** Whether `a` and `b` overlap on the axis perpendicular to `direction` (a shared span, not a point). */
function overlapsPerpendicular(a: PaneRect, b: PaneRect, direction: FocusDirection): boolean {
  if (direction === "left" || direction === "right") {
    return (
      b.top < a.top + a.height - EDGE_EPSILON_PCT && b.top + b.height > a.top + EDGE_EPSILON_PCT
    );
  }
  return (
    b.left < a.left + a.width - EDGE_EPSILON_PCT && b.left + b.width > a.left + EDGE_EPSILON_PCT
  );
}

/** Whether `b`'s facing edge touches `a` on the `direction` side. */
function facesOn(a: PaneRect, b: PaneRect, direction: FocusDirection): boolean {
  switch (direction) {
    case "left":
      return Math.abs(b.left + b.width - a.left) <= EDGE_EPSILON_PCT;
    case "right":
      return Math.abs(b.left - (a.left + a.width)) <= EDGE_EPSILON_PCT;
    case "up":
      return Math.abs(b.top + b.height - a.top) <= EDGE_EPSILON_PCT;
    case "down":
      return Math.abs(b.top - (a.top + a.height)) <= EDGE_EPSILON_PCT;
  }
}

/** The centre of `rect` on the axis PERPENDICULAR to `direction`. */
function perpendicularCenter(rect: PaneRect, direction: FocusDirection): number {
  return direction === "left" || direction === "right"
    ? rect.top + rect.height / 2
    : rect.left + rect.width / 2;
}

/**
 * The pane id of `paneId`'s GEOMETRIC neighbour on the `direction` side, or `null` when there is
 * none (the focus then stays put — see the note above). An unknown `paneId` also yields `null`.
 */
export function paneNeighbor(
  root: PaneNode,
  paneId: string,
  direction: FocusDirection,
): string | null {
  const placements = paneRects(root);
  const from = placements.find((placement) => placement.paneId === paneId);
  if (from == null) return null;
  let best: { paneId: string; distance: number } | null = null;
  for (const candidate of placements) {
    if (candidate.paneId === paneId) continue;
    if (!facesOn(from.rect, candidate.rect, direction)) continue;
    if (!overlapsPerpendicular(from.rect, candidate.rect, direction)) continue;
    const distance = Math.abs(
      perpendicularCenter(candidate.rect, direction) - perpendicularCenter(from.rect, direction),
    );
    if (best == null || distance < best.distance) best = { paneId: candidate.paneId, distance };
  }
  return best?.paneId ?? null;
}

/**
 * Does EVERY pane of `root` clear the absolute floor at this host?
 *
 * The terminals' floor is enforced at the SPLIT ({@link canSplitPane}): a pointer cuts one pane in
 * two and the question is about those two halves. A COMMANDS group has no pointer split at all
 * (user arbitrage: composition happens in the settings screen) — its tree is RE-DERIVED from the
 * ordered member list, so the whole layout changes at once and the question is about all of it.
 * Same floor, same yardstick, one more shape of the same check — which is why it lives HERE, next
 * to `canSplitPane`, rather than in a second geometry module.
 *
 * An unmeasured host (`null`) cannot refuse anything, exactly like `canSplitPane`.
 */
export function layoutFitsFloor(root: PaneNode, host: HostSize | null): boolean {
  if (host == null) return true;
  return paneRects(root).every((placement) => {
    const box = paneRectPx(placement.rect, host);
    return box.width >= MIN_PANE_WIDTH_PX && box.height >= MIN_PANE_HEIGHT_PX;
  });
}
