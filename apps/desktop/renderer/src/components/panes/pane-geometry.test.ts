import { describe, expect, it } from "vitest";

import {
  canSplitPane,
  compactPaneRatios,
  FLOOR_EPSILON_RATIO,
  floorEpsilonPx,
  MIN_PANE_HEIGHT_PX,
  MIN_PANE_WIDTH_PX,
  minPaneSpanPx,
  PANE_CONTROLS_HEIGHT_PX,
  paneControlsMode,
  paneControlsWidthPx,
  paneNeighbor,
  paneRects,
  paneRectStyle,
  paneRectsByTerminal,
  paneSeparators,
  ratioFromKey,
  ratioFromPointer,
  separatorFloorSatisfiable,
  separatorRatioBounds,
  separatorStyle,
  SEPARATOR_KEY_PAGE_STEP,
  SEPARATOR_KEY_STEP,
  SEPARATOR_THICKNESS_PX,
  type HostSize,
  type PaneSeparator,
} from "./pane-geometry";
import {
  clampRatio,
  closePane,
  createPaneLayout,
  isPaneNode,
  MAX_SPLIT_RATIO,
  MAX_STORED_RATIO,
  MIN_SPLIT_RATIO,
  setSplitRatio,
  splitPane,
  type PaneNode,
  type PaneSplit,
} from "./pane-tree";

function split(
  root: PaneNode,
  paneId: string,
  terminalId: string,
  direction: "row" | "column",
  newPaneId: string,
  ratio?: number,
): PaneNode {
  return splitPane(root, { paneId, direction, terminalId, newPaneId, ratio }) as PaneNode;
}

describe("paneRects", () => {
  it("gives a single pane the whole host", () => {
    expect(paneRects(createPaneLayout("t1", "p1"))).toEqual([
      { paneId: "p1", terminalId: "t1", rect: { left: 0, top: 0, width: 100, height: 100 } },
    ]);
  });

  it("splits the WIDTH for a row and the HEIGHT for a column", () => {
    const row = split(createPaneLayout("t1", "p1"), "p1", "t2", "row", "p2");
    expect(paneRects(row).map((p) => p.rect)).toEqual([
      { left: 0, top: 0, width: 50, height: 100 },
      { left: 50, top: 0, width: 50, height: 100 },
    ]);
    const column = split(createPaneLayout("t1", "p1"), "p1", "t2", "column", "p2");
    expect(paneRects(column).map((p) => p.rect)).toEqual([
      { left: 0, top: 0, width: 100, height: 50 },
      { left: 0, top: 50, width: 100, height: 50 },
    ]);
  });

  it("honours the split ratio", () => {
    const row = split(createPaneLayout("t1", "p1"), "p1", "t2", "row", "p2", 0.2);
    expect(paneRects(row).map((p) => p.rect.width)).toEqual([20, 80]);
  });

  it("nests: a split inside a split subdivides only its own box", () => {
    let root: PaneNode = createPaneLayout("t1", "p1");
    root = split(root, "p1", "t2", "row", "p2");
    root = split(root, "p2", "t3", "column", "p3");
    expect(paneRects(root)).toEqual([
      { paneId: "p1", terminalId: "t1", rect: { left: 0, top: 0, width: 50, height: 100 } },
      { paneId: "p2", terminalId: "t2", rect: { left: 50, top: 0, width: 50, height: 50 } },
      { paneId: "p3", terminalId: "t3", rect: { left: 50, top: 50, width: 50, height: 50 } },
    ]);
  });

  it("tiles the host exactly — the four-pane grid covers 100% with no overlap", () => {
    let root: PaneNode = createPaneLayout("t1", "p1");
    root = split(root, "p1", "t2", "row", "p2");
    root = split(root, "p1", "t3", "column", "p3");
    root = split(root, "p2", "t4", "column", "p4");
    const rects = paneRects(root);
    expect(rects).toHaveLength(4);
    const area = rects.reduce((sum, p) => sum + (p.rect.width * p.rect.height) / 100, 0);
    expect(area).toBeCloseTo(100, 6);
    for (const { rect } of rects) {
      expect(rect.left + rect.width).toBeLessThanOrEqual(100.000001);
      expect(rect.top + rect.height).toBeLessThanOrEqual(100.000001);
    }
  });
});

describe("paneRectsByTerminal", () => {
  it("indexes the placements by terminal id", () => {
    const root = split(createPaneLayout("t1", "p1"), "p1", "t2", "row", "p2");
    const map = paneRectsByTerminal(root);
    expect(map.get("t2")?.paneId).toBe("p2");
    expect(map.get("nope")).toBeUndefined();
  });

  it("is empty for no layout", () => {
    expect(paneRectsByTerminal(null).size).toBe(0);
  });
});

describe("paneRectStyle", () => {
  it("emits percent-based absolute positioning", () => {
    expect(paneRectStyle({ left: 50, top: 0, width: 50, height: 100 })).toEqual({
      position: "absolute",
      left: "50%",
      top: "0%",
      width: "50%",
      height: "100%",
    });
  });
});

/**
 * A standalone separator, so the pointer/key maths can be stated without building a tree. Both
 * sides default to ONE pane's floor — the simple "two leaves" case.
 */
function separator(overrides: Partial<PaneSeparator> = {}): PaneSeparator {
  const direction = overrides.direction ?? "row";
  const floor = direction === "row" ? MIN_PANE_WIDTH_PX : MIN_PANE_HEIGHT_PX;
  return {
    splitId: "s1",
    direction: "row",
    ratio: 0.5,
    bounds: { left: 0, top: 0, width: 100, height: 100 },
    minFirstPx: floor,
    minSecondPx: floor,
    ...overrides,
  };
}

describe("paneSeparators", () => {
  it("has none for a leaf or for no layout", () => {
    expect(paneSeparators(createPaneLayout("t1", "p1"))).toEqual([]);
    expect(paneSeparators(null)).toEqual([]);
  });

  it("gives one separator per split, carrying its direction, ratio and OWN box", () => {
    let root: PaneNode = createPaneLayout("t1", "p1");
    root = split(root, "p1", "t2", "row", "p2", 0.4);
    root = split(root, "p2", "t3", "column", "p3", 0.25);
    const separators = paneSeparators(root);

    expect(separators).toHaveLength(2);
    // Outermost first: the row split spans the WHOLE host…
    expect(separators[0]).toMatchObject({
      direction: "row",
      ratio: 0.4,
      bounds: { left: 0, top: 0, width: 100, height: 100 },
    });
    // …while the NESTED column split only spans the right-hand 60%, which is what makes a nested
    // drag map onto its own sub-box instead of the whole page.
    expect(separators[1]).toMatchObject({
      direction: "column",
      ratio: 0.25,
      bounds: { left: 40, top: 0, width: 60, height: 100 },
    });
  });

  it("sits exactly on the seam between the two panes it separates", () => {
    const root = split(createPaneLayout("t1", "p1"), "p1", "t2", "row", "p2", 0.3);
    const [first, second] = paneRects(root);
    const [sep] = paneSeparators(root);
    const boundary = (sep as PaneSeparator).bounds.left + 100 * (sep as PaneSeparator).ratio;
    expect(boundary).toBeCloseTo((first as { rect: { width: number } }).rect.width, 6);
    expect(boundary).toBeCloseTo((second as { rect: { left: number } }).rect.left, 6);
  });
});

describe("separatorStyle", () => {
  it("spans the split's box and CENTRES the band on the boundary", () => {
    const half = SEPARATOR_THICKNESS_PX / 2;
    expect(separatorStyle(separator({ direction: "row", ratio: 0.4 }))).toEqual({
      position: "absolute",
      top: "0%",
      height: "100%",
      left: `calc(40% - ${half}px)`,
      width: `${SEPARATOR_THICKNESS_PX}px`,
    });
    expect(
      separatorStyle(
        separator({
          direction: "column",
          ratio: 0.5,
          bounds: { left: 50, top: 0, width: 50, height: 100 },
        }),
      ),
    ).toEqual({
      position: "absolute",
      left: "50%",
      width: "50%",
      top: `calc(50% - ${half}px)`,
      height: `${SEPARATOR_THICKNESS_PX}px`,
    });
  });
});

describe("ratioFromPointer", () => {
  const host = { width: 1000, height: 500 };

  it("maps a pointer onto the split's own axis", () => {
    expect(ratioFromPointer(separator(), { x: 300, y: 0 }, host)).toBeCloseTo(0.3, 6);
    expect(
      ratioFromPointer(separator({ direction: "column" }), { x: 0, y: 200 }, host),
    ).toBeCloseTo(0.4, 6);
  });

  it("is relative to a NESTED split's box, not to the host", () => {
    // The split owns the right-hand half: x=750px is its MIDDLE, so the ratio is 0.5, not 0.75.
    const nested = separator({ bounds: { left: 50, top: 0, width: 50, height: 100 } });
    expect(ratioFromPointer(nested, { x: 750, y: 0 }, host)).toBeCloseTo(0.5, 6);
  });

  it("CLAMPS: dragging past either edge stops at the usable band", () => {
    // On a 1000px host the ABSOLUTE floor (180px a side) is TIGHTER than the relative 15/85 band,
    // so it is the floor the drag buts on: 18% / 82%, in pixels, not the constants.
    expect(ratioFromPointer(separator(), { x: -400, y: 0 }, host)).toBeCloseTo(0.18, 6);
    expect(ratioFromPointer(separator(), { x: 4000, y: 0 }, host)).toBeCloseTo(0.82, 6);
    // On a WIDE host the relative band is the tighter of the two and takes over again — the two
    // are CUMULATIVE, neither replaces the other.
    const wide: HostSize = { width: 4000, height: 500 };
    expect(ratioFromPointer(separator(), { x: -400, y: 0 }, wide)).toBe(MIN_SPLIT_RATIO);
    expect(ratioFromPointer(separator(), { x: 40000, y: 0 }, wide)).toBe(MAX_SPLIT_RATIO);
  });

  it("keeps the current ratio when the box is not measurable (0px axis)", () => {
    expect(
      ratioFromPointer(separator({ ratio: 0.42 }), { x: 10, y: 0 }, { width: 0, height: 0 }),
    ).toBe(0.42);
  });
});

describe("ratioFromKey", () => {
  it("steps a VERTICAL bar left/right and a HORIZONTAL one up/down", () => {
    const row = separator({ ratio: 0.5 });
    expect(ratioFromKey(row, "ArrowLeft")).toBeCloseTo(0.5 - SEPARATOR_KEY_STEP, 6);
    expect(ratioFromKey(row, "ArrowRight")).toBeCloseTo(0.5 + SEPARATOR_KEY_STEP, 6);
    // The other axis' arrows are NOT this separator's keys — they stay with the page.
    expect(ratioFromKey(row, "ArrowUp")).toBeNull();

    const column = separator({ direction: "column", ratio: 0.5 });
    expect(ratioFromKey(column, "ArrowUp")).toBeCloseTo(0.5 - SEPARATOR_KEY_STEP, 6);
    expect(ratioFromKey(column, "ArrowDown")).toBeCloseTo(0.5 + SEPARATOR_KEY_STEP, 6);
    expect(ratioFromKey(column, "ArrowLeft")).toBeNull();
  });

  it("takes a coarse step on Page Up/Down and jumps to the bounds on Home/End", () => {
    const row = separator({ ratio: 0.5 });
    expect(ratioFromKey(row, "PageUp")).toBeCloseTo(0.5 - SEPARATOR_KEY_PAGE_STEP, 6);
    expect(ratioFromKey(row, "PageDown")).toBeCloseTo(0.5 + SEPARATOR_KEY_PAGE_STEP, 6);
    expect(ratioFromKey(row, "Home")).toBe(MIN_SPLIT_RATIO);
    expect(ratioFromKey(row, "End")).toBe(MAX_SPLIT_RATIO);
  });

  it("CLAMPS at the bounds — no key sequence can collapse a pane", () => {
    expect(ratioFromKey(separator({ ratio: MIN_SPLIT_RATIO }), "ArrowLeft")).toBe(MIN_SPLIT_RATIO);
    expect(ratioFromKey(separator({ ratio: MIN_SPLIT_RATIO }), "PageUp")).toBe(MIN_SPLIT_RATIO);
    expect(ratioFromKey(separator({ ratio: MAX_SPLIT_RATIO }), "ArrowRight")).toBe(MAX_SPLIT_RATIO);
    expect(ratioFromKey(separator({ ratio: MAX_SPLIT_RATIO }), "PageDown")).toBe(MAX_SPLIT_RATIO);
  });

  it("ignores every other key (the event stays the page's)", () => {
    for (const key of ["a", "Enter", " ", "Tab", "Escape"]) {
      expect(ratioFromKey(separator(), key)).toBeNull();
    }
  });

  it("Home / End jump to the PX-FLOORED bounds once the host is measured", () => {
    const row = separator({ ratio: 0.5 });
    const host: HostSize = { width: 1000, height: 500 };
    // 180px of 1000 = 18% — the real end of the course, not the constant 15 / 85.
    expect(ratioFromKey(row, "Home", host)).toBeCloseTo(0.18, 6);
    expect(ratioFromKey(row, "End", host)).toBeCloseTo(0.82, 6);
    // …and a held arrow buts on that same floor instead of walking through it.
    let ratio = 0.5;
    for (let i = 0; i < 200; i++) {
      ratio = ratioFromKey({ ...row, ratio }, "ArrowLeft", host) as number;
    }
    expect(ratio).toBeCloseTo(0.18, 6);
    expect(1000 * ratio).toBeGreaterThanOrEqual(MIN_PANE_WIDTH_PX);
  });
});

/* ─────────────────────── THE ABSOLUTE FLOOR (finding F1) ─────────────────────── */

/**
 * THE TESTS' OWN TOLERANCE — deliberately NOT the module's (review round 3).
 *
 * Every guard of the floor used to be written `MIN_PANE_WIDTH_PX - FLOOR_EPSILON_PX`: the tolerance
 * of the ASSERTION was the very constant the CODE compares against, so both sides moved together
 * and raising the module's number a hundredfold widened what panes were allowed to do without a
 * single test going red. A guard cannot be graduated in the unit it is guarding.
 *
 * These two numbers are therefore fixed HERE and owned by the tests:
 *  - a MILLIONTH of a CSS pixel of floor deficit, ~1.4e6 × the worst the multi-host walk actually
 *    measures (7.4e-13px on width, 2.7e-13px on height) and 5 orders of magnitude under what the
 *    module tolerates at the largest host swept (7.7e-6px at a 7680px sub-box);
 *  - a TRILLIONTH of a sub-box of shortfall still judged feasible, ~375 × the worst measured
 *    (2.665e-15, about 12 ULPs).
 * Both are absolute numbers in this file, so they hold whatever `FLOOR_EPSILON_RATIO` becomes.
 */
const FLOOR_TEST_TOLERANCE_PX = 1e-6;
/** The worst RELATIVE shortfall a separator may still be judged feasible on — see above. */
const FLOOR_TEST_DRIFT_CAP_REL = 1e-12;

describe("FLOOR_EPSILON_RATIO", () => {
  /**
   * The value is PINNED, and so is the PROPERTY that makes it safe — that it is a FRACTION.
   *
   * The 0.05px this replaced was not merely too large: an absolute tolerance is the wrong SHAPE.
   * What a tolerance lets through is conceded to the panes below as a relative deficit, and a
   * relative deficit re-read at an ancestor is worth that fraction of the ancestor's much bigger
   * box — so a fixed number of pixels grows into a starvation as it climbs (0.05px on a ~1500px
   * sub-box read as 0.064px at a 1920px root, 1.29× the same 0.05px, and the root then handed back
   * 15..85 at a CONSTANT window and crushed a pane to 19.17px).
   *
   * AND THIS TEST IS THE ONLY THING THAT CATCHES IT (measured, review round 4). Make
   * `floorEpsilonPx` absolute again and change NOTHING else: the ten-host walk at the bottom of
   * this file stays GREEN — zero violations, at 1920×1080 included — and the single failure in the
   * suite is the scaling assertion below, "180px scales: expected 1 to be close to 2". The 329486
   * violations quoted next to the constant need the MIDPOINT pinning back as well; the two are one
   * mechanism, and this half is the one no walk can see. A shape assertion is not decoration here:
   * it is the whole guard.
   */
  it("is pinned at a billionth of the sub-box, and SCALES with it instead of being a pixel count", () => {
    expect(FLOOR_EPSILON_RATIO).toBe(1e-9);
    // THE SHAPE: doubling the sub-box doubles the tolerance. Any absolute constant answers 1 here,
    // whatever its value — this is what the old `FLOOR_EPSILON_PX` could not have satisfied.
    for (const span of [180, 360, 720, 1920, 7680]) {
      expect(floorEpsilonPx(2 * span) / floorEpsilonPx(span), `${span}px scales`).toBeCloseTo(
        2,
        12,
      );
    }
    // THE SIZE, at both ends of the range the walk sweeps: invisible on any screen…
    expect(floorEpsilonPx(1920)).toBeCloseTo(1.92e-6, 12);
    expect(floorEpsilonPx(7680)).toBeLessThan(1e-4);
    // …and still far ABOVE the drift it has to absorb, which the walk measures rather than assumes.
    expect(FLOOR_EPSILON_RATIO).toBeGreaterThan(100 * FLOOR_TEST_DRIFT_CAP_REL);
  });
});

describe("minPaneSpanPx", () => {
  it("gives a leaf the floor, composes ALONG the axis by ratio and takes the MAX across it", () => {
    const leaf = createPaneLayout("t1", "p1");
    expect(minPaneSpanPx(leaf, "width")).toBe(MIN_PANE_WIDTH_PX);
    expect(minPaneSpanPx(leaf, "height")).toBe(MIN_PANE_HEIGHT_PX);

    // A ROW split shares the WIDTH (a 50/50 cut needs 2 × 180) but each child owns the whole
    // HEIGHT (max = 120).
    const row = split(leaf, "p1", "t2", "row", "p2");
    expect(minPaneSpanPx(row, "width")).toBe(2 * MIN_PANE_WIDTH_PX);
    expect(minPaneSpanPx(row, "height")).toBe(MIN_PANE_HEIGHT_PX);

    // The RATIO is part of the demand: a 70/30 cut needs 180/0.3 = 600px, not 360 — the ratios
    // are not rebalanced when an ancestor moves, so the tight side is what sets the demand.
    const skewed = split(createPaneLayout("t1", "p1"), "p1", "t2", "row", "p2", 0.7);
    expect(minPaneSpanPx(skewed, "width")).toBeCloseTo(600, 6);

    // THE COMPOSITION the relative clamp misses: three nested 50/50 cuts leave the last pane at
    // W/8, so the tiling needs 8 × 180 = 1440px — not the 4 × 180 a flat count would suggest.
    let four: PaneNode = createPaneLayout("t1", "p1");
    four = split(four, "p1", "t2", "row", "p2");
    four = split(four, "p2", "t3", "row", "p3");
    four = split(four, "p3", "t4", "row", "p4");
    expect(minPaneSpanPx(four, "width")).toBe(8 * MIN_PANE_WIDTH_PX);
    expect(minPaneSpanPx(four, "height")).toBe(MIN_PANE_HEIGHT_PX);
  });
});

describe("separatorRatioBounds", () => {
  it("TIGHTENS the relative band with the px floor of EACH SIDE's whole subtree", () => {
    // The right half holds THREE panes at 50/50/50: at its current ratios it needs 720px, not 180.
    // On a 1600px host that caps the seam at 55% — a bound the constant 85% hid completely.
    let root: PaneNode = createPaneLayout("t1", "p1");
    root = split(root, "p1", "t2", "row", "p2");
    root = split(root, "p2", "t3", "row", "p3");
    root = split(root, "p3", "t4", "row", "p4");
    const [outer] = paneSeparators(root) as [PaneSeparator];
    expect(outer.minFirstPx).toBe(MIN_PANE_WIDTH_PX);
    expect(outer.minSecondPx).toBe(4 * MIN_PANE_WIDTH_PX);

    const bounds = separatorRatioBounds(outer, { width: 1600, height: 900 });
    expect(bounds.min).toBeCloseTo(0.15, 6); // 180/1600 = 0.1125, so the RELATIVE floor still wins
    expect(bounds.max).toBeCloseTo(0.55, 6); // 1 - 720/1600 — the PX floor, well under 85%
    expect(bounds.max).toBeLessThan(MAX_SPLIT_RATIO);
    // The band always contains where the seam stands today (the tiling is feasible at 1600px).
    expect(outer.ratio).toBeGreaterThanOrEqual(bounds.min);
    expect(outer.ratio).toBeLessThanOrEqual(bounds.max);
  });

  it("falls back to the relative band when the host is not measurable", () => {
    const sep = separator();
    expect(separatorRatioBounds(sep, null)).toEqual({ min: MIN_SPLIT_RATIO, max: MAX_SPLIT_RATIO });
    expect(separatorRatioBounds(sep, { width: 0, height: 0 })).toEqual({
      min: MIN_SPLIT_RATIO,
      max: MAX_SPLIT_RATIO,
    });
  });

  /* ───── GRACEFUL DEGRADATION when the constraint is UNSATISFIABLE (review arbitrage) ─────
   *
   * The seam used to FREEZE (min = max) as soon as the sub-box could no longer hold both sides at
   * the floor. The arbitrage reverses that: the floor becomes BEST-EFFORT there and the relative
   * 15..85 course comes back, because the window is already too small, both panes are already
   * under the floor, and freezing only removes the user's last manual recovery. Creating a pane
   * under the floor stays refused — that is `canSplitPane`, tested below and unchanged.
   */
  it("DEGRADES to the relative band when the box can no longer hold both sides at the floor", () => {
    // 300px cannot hold 2 × 180. Instead of collapsing onto 0.42, the band reverts to 15..85 —
    // the course the user had before the floor existed.
    const bounds = separatorRatioBounds(separator({ ratio: 0.42 }), { width: 300, height: 500 });
    expect(bounds).toEqual({ min: MIN_SPLIT_RATIO, max: MAX_SPLIT_RATIO });
    expect(bounds.min).not.toBe(bounds.max);
  });

  it("switches at EXACTLY 2× the floor — both sides of the threshold, so it cannot drift", () => {
    const sep = separator({ ratio: 0.5 });
    const floor = 2 * MIN_PANE_WIDTH_PX; // 360px for two leaves on the `width` axis

    // JUST ABOVE (and AT) the threshold: the strict px clamp of F1, untouched.
    for (const width of [floor, floor + 1]) {
      const bounds = separatorRatioBounds(sep, { width, height: 500 });
      expect(bounds.min, `${width}px min`).toBeCloseTo(MIN_PANE_WIDTH_PX / width, 6);
      expect(bounds.max, `${width}px max`).toBeCloseTo(1 - MIN_PANE_WIDTH_PX / width, 6);
      expect(bounds.min).toBeLessThanOrEqual(bounds.max);
    }
    // At EXACTLY 2× the floor the two bounds meet on 0.5 — the last feasible layout, still the
    // strict clamp (both panes sit exactly at 180px), NOT the degraded band.
    expect(separatorRatioBounds(sep, { width: floor, height: 500 })).toEqual({
      min: 0.5,
      max: 0.5,
    });

    // JUST BELOW: one pixel short is already unsatisfiable ⇒ the relative course comes back.
    expect(separatorRatioBounds(sep, { width: floor - 1, height: 500 })).toEqual({
      min: MIN_SPLIT_RATIO,
      max: MAX_SPLIT_RATIO,
    });

    // JUST ABOVE, the band is NARROW but REAL — the numbers `<PaneTreeHost>`'s whole-percent
    // `aria-value*` cannot express (review finding: 361..363px announce 50 / 50 while the seam
    // still travels 1 to 3px). 360px is the ONLY width where the course is genuinely nil.
    for (const [width, travelPx] of [
      [361, 1],
      [362, 2],
      [363, 3],
    ] as const) {
      const band = separatorRatioBounds(sep, { width, height: 500 });
      expect(band.min, `${width}px min`).toBeCloseTo(MIN_PANE_WIDTH_PX / width, 9);
      expect(band.max, `${width}px max`).toBeCloseTo(1 - MIN_PANE_WIDTH_PX / width, 9);
      expect(width * (band.max - band.min), `${width}px travel`).toBeCloseTo(travelPx, 6);
      expect(Math.round(band.min * 100), `${width}px announced`).toBe(Math.round(band.max * 100));
    }
    expect(separatorRatioBounds(sep, { width: floor, height: 500 })).toEqual({
      min: 0.5,
      max: 0.5,
    });

    // Same threshold on the OTHER axis, from the height floor.
    const column = separator({ direction: "column", ratio: 0.5 });
    expect(separatorRatioBounds(column, { width: 500, height: 2 * MIN_PANE_HEIGHT_PX })).toEqual({
      min: 0.5,
      max: 0.5,
    });
    expect(
      separatorRatioBounds(column, { width: 500, height: 2 * MIN_PANE_HEIGHT_PX - 1 }),
    ).toEqual({ min: MIN_SPLIT_RATIO, max: MAX_SPLIT_RATIO });
  });

  /* ───── THE EPSILON THAT USED TO CROSS THE FRONTIER (review round 2, critical) ─────
   *
   * Measured on the REAL gesture path, at a CONSTANT 720×900 window: two guarded splits, then End
   * on the inner seam, leave the OUTER sub-box owing exactly 540 + 180px of 720. That is
   * satisfiable — at the single ratio 0.75, which is where the seam already stands. But the demand
   * is reached through `180 / (1 − 180/540)`, and IEEE-754 answers 540.000000000000114: 1.14e-13px
   * too much. Compared without tolerance, those 114 femtometres declared the split STARVED and
   * handed the full 15..85 course back on a sub-box worth FOUR times the floor — from where one
   * Home key parked a pane at 36px (replayed in full further down).
   *
   * And the tipping point is the DEFAULT destination of a gesture, not a fluke: every drag and
   * every Home/End is clamped EXACTLY onto a bound.
   */
  it("does NOT call a sub-box starved over an IEEE-754 epsilon (720px owing 540 + 180)", () => {
    const host: HostSize = { width: 720, height: 900 };
    // The inner seam sits where End left it — `1 − 180/540`, NOT the literal 2/3. They are one ULP
    // apart and only this one carries the error (2/3 computes 539.99999999999989, i.e. UNDER).
    const innerRatio = 1 - MIN_PANE_WIDTH_PX / 540;
    let root: PaneNode = createPaneLayout("t0", "p0");
    root = split(root, "p0", "t1", "row", "p1", 0.75);
    root = split(root, "p0", "t2", "row", "p2", innerRatio);
    const [outer] = paneSeparators(root) as [PaneSeparator];

    // The measured numbers, stated: the demand overshoots 540px by 1.1e-13, on a 720px sub-box.
    expect(outer.minFirstPx).toBeGreaterThan(540);
    expect(outer.minFirstPx - 540).toBeLessThan(1e-12);
    expect(outer.minSecondPx).toBe(MIN_PANE_WIDTH_PX);

    expect(
      separatorFloorSatisfiable(outer, host),
      "a 720px sub-box owing 540 + 180px is satisfiable — at 0.75, where the seam already is",
    ).toBe(true);
    const bounds = separatorRatioBounds(outer, host);
    expect(bounds.min).toBeCloseTo(0.75, 12);
    expect(bounds.max).toBeCloseTo(0.75, 12);
    // Said the other way round, because THIS is the regression: not the relative course.
    expect(bounds.max).toBeLessThan(MAX_SPLIT_RATIO);
    expect(ratioFromKey(outer, "Home", host)).toBeCloseTo(0.75, 12);
    expect(ratioFromKey(outer, "End", host)).toBeCloseTo(0.75, 12);
  });

  /* ───── SATURATED BUT NOT STARVED — the user's arbitrage, in code ─────
   *
   * `minFirstPx / span > MAX_SPLIT_RATIO` while `minFirstPx + minSecondPx ≤ span`: the px floor
   * asks for MORE than the relative ceiling allows, and the sub-box HAS the room. Reached by
   * shrinking a window under an existing tiling (ratios are never rebalanced, out of scope), which
   * grows one side's demand while the box only loses part of what it had.
   *
   * ARBITRAGE: the px floor wins. Handing back 15..85 here would starve a subtree the box can
   * perfectly well feed, which is the exact opposite of what the floor is for.
   */
  it("lets the PX FLOOR beat the 85% ceiling when the box still has the room (2000px, 1750 + 180)", () => {
    const host: HostSize = { width: 2000, height: 900 };
    const sep = separator({ ratio: MAX_SPLIT_RATIO, minFirstPx: 1750, minSecondPx: 180 });
    // The setup, stated: over the ceiling (0.875 > 0.85), and yet 70px of REAL slack in the box.
    expect(sep.minFirstPx / host.width).toBeGreaterThan(MAX_SPLIT_RATIO);
    expect(sep.minFirstPx + sep.minSecondPx).toBeLessThan(host.width);

    expect(separatorFloorSatisfiable(sep, host)).toBe(true);
    const bounds = separatorRatioBounds(sep, host);
    expect(bounds).not.toEqual({ min: MIN_SPLIT_RATIO, max: MAX_SPLIT_RATIO });
    expect(bounds.min).toBeCloseTo(0.875, 12); // 1750 / 2000 — the first side's px demand
    expect(bounds.max).toBeCloseTo(0.91, 12); // (2000 − 180) / 2000 — the second side's
    // Every ratio of that band keeps BOTH sides at or above their floor — which 0.85 did not.
    for (const ratio of [bounds.min, (bounds.min + bounds.max) / 2, bounds.max]) {
      expect(host.width * ratio).toBeGreaterThanOrEqual(sep.minFirstPx - 1e-9);
      expect(host.width * (1 - ratio)).toBeGreaterThanOrEqual(sep.minSecondPx - 1e-9);
    }
    expect(host.width * MAX_SPLIT_RATIO).toBeLessThan(sep.minFirstPx);
    // …and the seam really REACHES it: the model stores a ratio outside 15..85 (assumed).
    expect(clampRatio(bounds.min)).toBe(bounds.min);
    expect(clampRatio(bounds.max)).toBe(bounds.max);
  });

  /**
   * THE TWO READERS OF THE SAME RULE, CONFRONTED (review finding: the inequality used to be
   * written twice in the module and a third time here, and nothing compared them).
   *
   * `separatorFloorSatisfiable` and `separatorRatioBounds` now cut from ONE expression
   * (`pxFloorBand`), and this is what would notice if they ever drifted apart again — deliberately
   * WITHOUT re-deriving the predicate here: a fourth copy would only move the problem. It states
   * the CONSEQUENCE instead, over a sweep of spans that crosses every regime:
   *   satisfiable ⇒ the band it hands back really keeps both sides at their px floor;
   *   NOT satisfiable ⇒ the band is the relative course, and nothing else.
   */
  it("agrees with itself: satisfiable ⟺ the bounds really honour the px floor", () => {
    const sep = separator({ ratio: 0.5, minFirstPx: 540, minSecondPx: 180 });
    let satisfiable = 0;
    let starved = 0;
    for (let width = 300; width <= 2000; width += 1) {
      const host: HostSize = { width, height: 500 };
      const bounds = separatorRatioBounds(sep, host);
      if (separatorFloorSatisfiable(sep, host)) {
        satisfiable += 1;
        // Both ends of the band pay both demands — that is what "satisfiable" is worth.
        expect(width * bounds.min, `${width}px min`).toBeGreaterThanOrEqual(
          sep.minFirstPx - FLOOR_TEST_TOLERANCE_PX,
        );
        expect(width * (1 - bounds.max), `${width}px max`).toBeGreaterThanOrEqual(
          sep.minSecondPx - FLOOR_TEST_TOLERANCE_PX,
        );
      } else {
        starved += 1;
        expect(bounds, `${width}px starved`).toEqual({
          min: MIN_SPLIT_RATIO,
          max: MAX_SPLIT_RATIO,
        });
      }
    }
    // The sweep really crossed the frontier — 720px is where 540 + 180 stops fitting.
    expect(starved).toBe(720 - 300);
    expect(satisfiable).toBe(2000 - 720 + 1);
  });

  it("the DEGRADED band really moves the seam — pointer AND keys, where both used to be inert", () => {
    // A 300px sub-box: under 2 × 180, so the floor is unsatisfiable and best-effort.
    const host: HostSize = { width: 300, height: 500 };
    const sep = separator({ ratio: 0.42 });

    // POINTER — a drag lands where the pointer is, and buts on 15 / 85 rather than on 0.42.
    expect(ratioFromPointer(sep, { x: 210, y: 0 }, host)).toBeCloseTo(0.7, 6);
    expect(ratioFromPointer(sep, { x: -400, y: 0 }, host)).toBe(MIN_SPLIT_RATIO);
    expect(ratioFromPointer(sep, { x: 4000, y: 0 }, host)).toBe(MAX_SPLIT_RATIO);
    expect(ratioFromPointer(sep, { x: 210, y: 0 }, host)).not.toBe(sep.ratio);

    // KEYS — arrows, Page Up/Down and Home/End all displace the seam instead of returning 0.42.
    expect(ratioFromKey(sep, "ArrowLeft", host)).toBeCloseTo(0.42 - SEPARATOR_KEY_STEP, 6);
    expect(ratioFromKey(sep, "ArrowRight", host)).toBeCloseTo(0.42 + SEPARATOR_KEY_STEP, 6);
    expect(ratioFromKey(sep, "PageUp", host)).toBeCloseTo(0.42 - SEPARATOR_KEY_PAGE_STEP, 6);
    expect(ratioFromKey(sep, "PageDown", host)).toBeCloseTo(0.42 + SEPARATOR_KEY_PAGE_STEP, 6);
    expect(ratioFromKey(sep, "Home", host)).toBe(MIN_SPLIT_RATIO);
    expect(ratioFromKey(sep, "End", host)).toBe(MAX_SPLIT_RATIO);
    for (const key of ["ArrowLeft", "ArrowRight", "PageUp", "PageDown", "Home", "End"]) {
      expect(ratioFromKey(sep, key, host), `${key} moves the seam`).not.toBe(sep.ratio);
    }
    // …and the user can really carry a pane out to a readable width: End gives it 255 of 300px.
    expect(host.width * (ratioFromKey(sep, "End", host) as number)).toBeCloseTo(255, 6);
  });
});

/**
 * THE RESTORE'S PX PASS, tested WHERE ITS PROPERTIES ARE OBSERVABLE.
 *
 * `compactPaneRatios` used to be reached only through `restorePaneLayout`, i.e. through a
 * `JSON.parse` — a surface on which REFERENCE IDENTITY cannot be asserted at all, so the property
 * the loop's stop condition rests on (`next === current`) had no test that could contradict it
 * (review round 4). It is exported; these call it directly.
 */
describe("compactPaneRatios", () => {
  const widthsOf = (tree: PaneNode, host: HostSize) =>
    paneRects(tree).map((p) => (host.width * p.rect.width) / 100);

  /**
   * THE STORABLE BAND IS AN OUTPUT CONTRACT, NOT ONLY AN INPUT ONE (review round 4, major).
   *
   * `separatorRatioBounds` answers in PIXELS, and in the SATURATED regime it hands its px band back
   * RAW — the px floor beats 15..85 there on purpose. Over a sub-box of
   * `MIN_PANE_WIDTH_PX / (1 − MAX_STORED_RATIO)` = 9000px (or `MIN_PANE_HEIGHT_PX / 0.02` = 6000px
   * tall), a first side may demand MORE than 98% of the box, and that band then sits ENTIRELY above
   * the band a ratio may be STORED in. Clamped onto it and written as-is, the restored tree is one
   * `isPaneNode` REJECTS — so the next launch discards the whole entry and the user's tiling
   * collapses to a single pane. The repair has to end where every other ratio writer ends:
   * `clampRatio`.
   */
  describe("the STORABLE band is never crossed on the way out", () => {
    /** The 5-pane / 4-seam tiling of the round-4 report, reachable by gestures alone. */
    const wideRepro = (): PaneNode => ({
      kind: "split",
      id: "s0",
      direction: "row",
      ratio: MAX_STORED_RATIO,
      children: [
        {
          kind: "split",
          id: "s1",
          direction: "row",
          ratio: 0.5,
          children: [
            { kind: "leaf", id: "p90", terminalId: "t90" },
            {
              kind: "split",
              id: "s2",
              direction: "row",
              ratio: MAX_SPLIT_RATIO,
              children: [
                { kind: "leaf", id: "p97", terminalId: "t97" },
                {
                  kind: "split",
                  id: "s3",
                  direction: "row",
                  // Chosen so the demand of `s1` lands on EXACTLY 98% of a 9001px box.
                  ratio: 0.7279213874195385,
                  children: [
                    { kind: "leaf", id: "p114", terminalId: "t114" },
                    { kind: "leaf", id: "p135", terminalId: "t135" },
                  ],
                },
              ],
            },
          ],
        },
        { kind: "leaf", id: "p26", terminalId: "t26" },
      ],
    });

    /** The same knife edge on the OTHER axis: a column tree owing 5881px of a 6001px box. */
    const tallRepro = (): PaneNode => ({
      kind: "split",
      id: "s0",
      direction: "column",
      ratio: MAX_STORED_RATIO,
      children: [
        {
          kind: "split",
          id: "s1",
          direction: "column",
          ratio: 0.5,
          children: [
            { kind: "leaf", id: "q1", terminalId: "u1" },
            {
              kind: "split",
              id: "s2",
              direction: "column",
              ratio: MAX_SPLIT_RATIO,
              children: [
                { kind: "leaf", id: "q2", terminalId: "u2" },
                {
                  kind: "split",
                  id: "s3",
                  direction: "column",
                  ratio: 1 - 1600 / 5881,
                  children: [
                    { kind: "leaf", id: "q3", terminalId: "u3" },
                    { kind: "leaf", id: "q4", terminalId: "u4" },
                  ],
                },
              ],
            },
          ],
        },
        { kind: "leaf", id: "q5", terminalId: "u5" },
      ],
    });

    it("clamps a px band sitting ENTIRELY ABOVE 98% back into the storable one (9001×4320)", () => {
      const host: HostSize = { width: 9001, height: 4320 };
      const tree = wideRepro();
      // The entry really is a valid tree — this is not a corrupted-input test.
      expect(isPaneNode(tree)).toBe(true);

      // WHY THE BAND LEAVES THE STORABLE ONE, in the numbers the reader needs to see it:
      // a 9001.0000px sub-box owing 8820.9800 + 180.0000px is SATISFIABLE (0.02px of slack), so the
      // saturated regime returns the px band — and 8820.98 / 9001 is 98% to the last bit, so BOTH
      // ends land above `MAX_STORED_RATIO`.
      const seam = paneSeparators(tree)[0] as PaneSeparator;
      expect((host.width * seam.bounds.width) / 100).toBe(9001);
      expect(seam.minFirstPx).toBeCloseTo(8820.98, 9);
      expect(seam.minSecondPx).toBe(MIN_PANE_WIDTH_PX);
      expect(separatorFloorSatisfiable(seam, host)).toBe(true);
      const band = separatorRatioBounds(seam, host);
      // A WHOLE-SHAPE `toEqual`, deliberately NOT `[band.min, band.max]`: the saturated regime is
      // the one branch that used to hand the module's INTERNAL px band back, `slackPx` / `spanPx`
      // and all, and a destructured assertion cannot see two extra fields ride out through a
      // `RatioBand` signature (review round 5). `toEqual` compares the whole shape, so this is the
      // assertion that reddens the day they come back — and while they were there, every
      // `.not.toEqual({min, max})` written against such a band passed VACUOUSLY.
      expect(band).toEqual({ min: 0.9800000000000002, max: 0.980002221975336 });
      expect(band.min).toBeGreaterThan(MAX_STORED_RATIO);
      expect(band.max).toBeGreaterThan(MAX_STORED_RATIO);

      // THE OUTPUT IS STILL STORABLE — this is what used to be false.
      const compacted = compactPaneRatios(tree, host);
      expect((compacted as PaneSplit).ratio).toBe(MAX_STORED_RATIO);
      expect(isPaneNode(compacted)).toBe(true);
      expect(paneRects(compacted)).toHaveLength(5);
    });

    it("does the same on the COLUMN axis, at the 6000px threshold (1600×6001)", () => {
      const host: HostSize = { width: 1600, height: 6001 };
      const tree = tallRepro();
      expect(isPaneNode(tree)).toBe(true);
      const seam = paneSeparators(tree)[0] as PaneSeparator;
      // 5881.0000px demanded of a 6001px box, against a 120px floor: the two demands cross by a
      // trillionth of a pixel, so the band is PINNED — onto a point above 98%.
      expect(seam.minFirstPx).toBeCloseTo(5881, 9);
      expect(seam.minSecondPx).toBe(MIN_PANE_HEIGHT_PX);
      const band = separatorRatioBounds(seam, host);
      expect(band.min).toBe(band.max);
      expect(band.min).toBeGreaterThan(MAX_STORED_RATIO);
      const compacted = compactPaneRatios(tree, host);
      expect(isPaneNode(compacted)).toBe(true);
      expect((compacted as PaneSplit).ratio).toBe(MAX_STORED_RATIO);
    });

    it("holds ACROSS the threshold, half a pixel at a time — 8990…9010 and 5990…6010", () => {
      // The break is arithmetically located, not statistical: it opens where the sub-box first
      // becomes able to pay both demands while the first still owes over 98% of it. Swept on both
      // axes, in 0.5px steps, so the crossing point cannot be stepped over.
      const failures: string[] = [];
      for (let width = 8990; width <= 9010; width += 0.5) {
        const out = compactPaneRatios(wideRepro(), { width, height: 4320 });
        if (!isPaneNode(out)) failures.push(`${width}×4320 → ratio ${(out as PaneSplit).ratio}`);
      }
      for (let height = 5990; height <= 6010; height += 0.5) {
        const out = compactPaneRatios(tallRepro(), { width: 1600, height });
        if (!isPaneNode(out)) failures.push(`1600×${height} → ratio ${(out as PaneSplit).ratio}`);
      }
      expect(failures, "every compacted tree must still be a STORABLE tree").toEqual([]);
    });

    it("holds over a GESTURE WALK restored at oversized hosts — where the edge really comes from", () => {
      // A RANDOM ratio never lands on the knife edge; a GESTURE does. Every drag and every Home/End
      // is clamped EXACTLY onto a band end, so a persisted tiling routinely holds a seam whose
      // subtree demands its whole sub-box to the last bit — which is how `minFirstPx / span` gets
      // to 98%. The walk therefore SPLITS and DRAGS at each host, then judges the tree it really
      // produced at every host of the set, its own included.
      const hosts: HostSize[] = [
        { width: 1920, height: 1080 },
        { width: 9001, height: 4320 },
        { width: 11520, height: 2160 },
        { width: 15360, height: 2160 },
        { width: 1600, height: 6001 },
        { width: 3840, height: 6480 },
      ];
      const rejected: string[] = [];
      let judged = 0;
      let tiled = 0;
      for (const host of hosts) {
        for (let s = 1; s <= 3; s++) {
          let seed = Math.imul(s, 0x9e3779b9);
          const random = () => {
            seed = (seed + 0x6d2b79f5) | 0;
            let t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
            t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
            return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
          };
          let root: PaneNode = createPaneLayout("t0", "p0");
          let n = 0;
          for (let step = 0; step < 40; step++) {
            const panes = paneRects(root);
            const target = panes[Math.floor(random() * panes.length)] as (typeof panes)[number];
            const direction = random() < 0.5 ? "row" : "column";
            if (canSplitPane(root, target.paneId, direction, host)) {
              n += 1;
              root = split(root, target.paneId, `t${n}`, direction, `p${n}`);
              tiled += 1;
            }
            for (const { splitId } of paneSeparators(root)) {
              const sep = paneSeparators(root).find((x) => x.splitId === splitId);
              if (sep == null) continue;
              const push = random() < 0.5 ? -100000 : 100000;
              root = setSplitRatio(
                root,
                splitId,
                ratioFromPointer(sep, { x: push, y: push }, host),
              );
            }
            // Whatever the gestures wrote IS storable — that is the mutators' own guarantee…
            if (!isPaneNode(root)) rejected.push(`gestures at ${host.width}×${host.height}`);
            // …and it must SURVIVE a restore at any host, or the next launch throws the tiling away.
            for (const restore of hosts) {
              judged += 1;
              const out = compactPaneRatios(root, restore);
              if (!isPaneNode(out)) {
                rejected.push(
                  `built at ${host.width}×${host.height}, restored at ${restore.width}×${restore.height}`,
                );
              }
            }
          }
        }
      }
      expect(tiled, "the walk really tiled").toBeGreaterThan(100);
      expect(judged, "the walk really judged").toBeGreaterThan(4000);
      expect(
        [...new Set(rejected)],
        "a compacted tree isPaneNode would throw away at the next launch",
      ).toEqual([]);
    });
  });

  /**
   * REFERENCE IDENTITY is not a micro-optimisation here: `compactPaneRatios` stops its fixpoint
   * loop on `next === current`, so losing it means every restore runs `paneTreeDepth(root) + 1`
   * full passes instead of one. It is asserted with `toBe`, on the exported function, because
   * through `restorePaneLayout` it is UNOBSERVABLE — that path parses fresh objects out of JSON.
   */
  describe("what it does NOT touch", () => {
    it("hands a healthy tree back BY REFERENCE — the loop's stop condition, asserted with toBe", () => {
      const host: HostSize = { width: 1440, height: 900 };
      let tree: PaneNode = createPaneLayout("t1", "p1");
      tree = split(tree, "p1", "t2", "row", "p2");
      tree = split(tree, "p2", "t3", "column", "p3");
      // Nothing to repair: every seam already sits inside the band its own sub-box allows.
      for (const seam of paneSeparators(tree)) {
        const band = separatorRatioBounds(seam, host);
        expect(seam.ratio).toBeGreaterThanOrEqual(band.min);
        expect(seam.ratio).toBeLessThanOrEqual(band.max);
      }
      expect(compactPaneRatios(tree, host)).toBe(tree);
    });

    it("rebuilds ONLY the spine it repaired — an untouched SIBLING comes back by reference", () => {
      const host: HostSize = { width: 1440, height: 900 };
      let tree: PaneNode = createPaneLayout("t1", "p1");
      tree = split(tree, "p1", "t2", "row", "p2");
      // LEFT half: a 0.98 seam on a 720px box — a 14.40px pane, repaired to 0.75.
      tree = split(tree, "p1", "t3", "row", "p3", MAX_STORED_RATIO);
      // RIGHT half: perfectly sound at the same 720px, and it must come back untouched.
      tree = split(tree, "p2", "t4", "row", "p4");
      const before = tree as PaneSplit;

      const after = compactPaneRatios(tree, host) as PaneSplit;
      expect(after).not.toBe(before);
      expect((after.children[0] as PaneSplit).ratio).toBe(0.75);
      expect(after.children[0], "the repaired half is a new node").not.toBe(before.children[0]);
      expect(after.children[1], "the untouched half is the VERY SAME node").toBe(
        before.children[1],
      );
    });

    it("leaves a STARVED seam's ratio EXACTLY where it was, and an unmeasurable host untouched", () => {
      let tree: PaneNode = createPaneLayout("t1", "p1");
      tree = split(tree, "p1", "t2", "row", "p2", MAX_STORED_RATIO);
      // 340px cannot hold 2 × 180: no ratio satisfies both sides, so the seam is not a repair site.
      expect((compactPaneRatios(tree, { width: 340, height: 900 }) as PaneSplit).ratio).toBe(
        MAX_STORED_RATIO,
      );
      // Nothing to measure ⇒ nothing to judge, and the SAME object comes back.
      expect(compactPaneRatios(tree, null)).toBe(tree);
      expect(compactPaneRatios(tree, { width: 0, height: 900 })).toBe(tree);
      expect(compactPaneRatios(tree, { width: 1440, height: 0 })).toBe(tree);
    });
  });

  /**
   * IT NORMALISES AS WELL AS REPAIRS, and the round-4 audit measured the amplitude: 0.59% of
   * reachable layouts whose every pane was ALREADY above the floor still moved, the worst by
   * 0.056250 of an axis — 108px at 1920×1080. This pins that exact case rather than leaving the
   * module claiming it "repairs, it does not normalise".
   */
  it("RE-CASTS a legitimate out-of-band seam once a close drops its subtree's demand (108px)", () => {
    const host: HostSize = { width: 1920, height: 1080 };
    // `A | B|C` on the left of a single pane on the right. The left side demands 1714.29px of the
    // 1920px box — over the 85% ceiling, under the box — so the SATURATED regime applies and the
    // px floor legitimately beats 15..85.
    let tree: PaneNode = createPaneLayout("tA", "pA");
    tree = split(tree, "pA", "tL", "row", "pL");
    tree = split(tree, "pA", "tB", "row", "pB", 0.3);
    tree = split(tree, "pB", "tC", "row", "pC", MIN_SPLIT_RATIO);
    const seam = paneSeparators(tree)[0] as PaneSeparator;
    expect(seam.minFirstPx).toBeCloseTo(1714.2857142857142, 9);
    const band = separatorRatioBounds(seam, host);
    expect(band.min).toBeGreaterThan(MAX_SPLIT_RATIO);
    // The SATURATED regime at an ORDINARY 1920×1080 — no 9000px host is needed to reach it — pinned
    // WHOLE (review round 5): a bare `RatioBand`, never the module's internal px band. The shape is
    // what a `toEqual` compares, so it is the shape that decides whether the `.not.toEqual` of the
    // deep-nesting test below asserts anything at all.
    expect(band).toEqual({ min: 0.8928571428571429, max: 0.90625 });

    // THE GESTURE: a pointer shoved as far right as it goes stops at 0.90625 — outside 15..85,
    // and that is the arbitrated behaviour, not a defect.
    const dragged = ratioFromPointer(seam, { x: 100000, y: 100000 }, host);
    expect(dragged).toBe(0.90625);
    tree = setSplitRatio(tree, seam.splitId, dragged);
    expect(Math.min(...widthsOf(tree, host))).toBe(MIN_PANE_WIDTH_PX);

    // CLOSE one pane inside that left subtree. Its demand falls to 600px; the seam is NOT
    // rebalanced (out of scope), so 0.90625 survives — sound, every pane still over the floor, but
    // no longer reachable by a drag at this host.
    tree = closePane(tree, "pB");
    expect(Math.min(...widthsOf(tree, host))).toBe(MIN_PANE_WIDTH_PX);
    expect((tree as PaneSplit).ratio).toBe(0.90625);

    // THE RESTORE MOVES IT ANYWAY, back onto the aesthetic ceiling: 108px of travel on a layout
    // that had nothing to repair.
    const compacted = compactPaneRatios(tree, host) as PaneSplit;
    expect(compacted.ratio).toBe(MAX_SPLIT_RATIO);
    expect((0.90625 - compacted.ratio) * host.width).toBeCloseTo(108, 9);
    expect(Math.min(...widthsOf(compacted, host))).toBeGreaterThan(MIN_PANE_WIDTH_PX);
  });
});

/**
 * THE CONTROL CLUSTER'S FIT (review finding F2) — the pure half of the fallback, so the exact
 * thresholds are stated once and are readable without a browser. The RENDERED consequence (one
 * `…` button below the width, three above, none covered, none out of frame) is asserted in the
 * browser spec, where boxes and hit-testing are real.
 */
describe("paneControlsMode", () => {
  it("prices the cluster: 3 buttons = 88px, 2 = 60px, the overflow fallback = 32px", () => {
    expect(paneControlsWidthPx(3)).toBe(88);
    expect(paneControlsWidthPx(2)).toBe(60);
    expect(paneControlsWidthPx(1)).toBe(32);
    expect(PANE_CONTROLS_HEIGHT_PX).toBe(32);
  });

  /**
   * THE COUPLING THE TWO MODULES ONLY ASSERTED AT EACH OTHER IN COMMENTS (review finding).
   *
   * `MIN_PANE_WIDTH_PX = 180` is justified by "room for its toolbar", and the cluster's own note
   * says "180 was chosen precisely so the full cluster always fits". Nothing checked it. Lower the
   * floor, or add a FOURTH command, and the invariant breaks in silence — the only symptom being
   * the overflow fallback firing permanently on panes that are at the floor.
   */
  it("FITS INSIDE THE FLOOR: the full cluster is why MIN_PANE_WIDTH_PX is 180", () => {
    expect(
      paneControlsWidthPx(3),
      "the 180px pane floor exists so the FULL 3-button cluster fits inside a pane at the floor — lower the floor or add a fourth command and the fallback fires permanently",
    ).toBeLessThanOrEqual(MIN_PANE_WIDTH_PX);
    expect(
      PANE_CONTROLS_HEIGHT_PX,
      "…and the 120px height floor exists so one row of buttons fits inside a pane at the floor",
    ).toBeLessThanOrEqual(MIN_PANE_HEIGHT_PX);
    // Stated the way the component asks it: a pane sitting EXACTLY on the floor renders the full
    // cluster, never the fallback. That is the user-visible form of the inequality above.
    expect(
      paneControlsMode({ width: MIN_PANE_WIDTH_PX, height: MIN_PANE_HEIGHT_PX }, 3),
      "a pane at the floor still renders the full cluster",
    ).toBe("full");
  });

  it("keeps the full cluster while it FITS and collapses to the fallback below", () => {
    const tall = (width: number) => ({ width, height: 400 });
    expect(paneControlsMode(tall(88), 3)).toBe("full");
    expect(paneControlsMode(tall(87), 3)).toBe("overflow");
    expect(paneControlsMode(tall(32), 3)).toBe("overflow");
    // Below even ONE button + its insets there is genuinely no room: nothing is rendered rather
    // than something sticking out of the frame (the chords remain the way in).
    expect(paneControlsMode(tall(31), 3)).toBe("none");
    // A two-button cluster (the LAST pane, whose close control is gone) is cheaper.
    expect(paneControlsMode(tall(60), 2)).toBe("full");
    expect(paneControlsMode(tall(59), 2)).toBe("overflow");
    // The HEIGHT is a floor of its own — one row of buttons plus its insets.
    expect(paneControlsMode({ width: 800, height: 32 }, 3)).toBe("full");
    expect(paneControlsMode({ width: 800, height: 31 }, 3)).toBe("none");
  });

  it("an UNMEASURED box keeps the full cluster — the fallback answers a measurement, not a guess", () => {
    expect(paneControlsMode(null, 3)).toBe("full");
  });
});

describe("canSplitPane", () => {
  const host: HostSize = { width: 1200, height: 800 };

  it("allows a split while BOTH halves stay above the floor, and refuses below", () => {
    let root: PaneNode = createPaneLayout("t1", "p1");
    // 1200px → 600 / 600 → 300 / 300: the third cut would make 150px halves. REFUSED.
    expect(canSplitPane(root, "p1", "row", host)).toBe(true);
    root = split(root, "p1", "t2", "row", "p2");
    expect(canSplitPane(root, "p1", "row", host)).toBe(true); // 600 → 300 / 300 ✓
    root = split(root, "p1", "t3", "row", "p3");
    expect(canSplitPane(root, "p1", "row", host)).toBe(false); // 300 → 150 / 150 ✗
    // …while the OTHER axis is untouched by that cut and stays available (800 → 400 / 400).
    expect(canSplitPane(root, "p1", "column", host)).toBe(true);
  });

  it("uses the exact 180 / 120 thresholds on the axis being cut", () => {
    const root = createPaneLayout("t1", "p1");
    expect(canSplitPane(root, "p1", "row", { width: 2 * MIN_PANE_WIDTH_PX, height: 800 })).toBe(
      true,
    );
    expect(canSplitPane(root, "p1", "row", { width: 2 * MIN_PANE_WIDTH_PX - 1, height: 800 })).toBe(
      false,
    );
    expect(
      canSplitPane(root, "p1", "column", { width: 1200, height: 2 * MIN_PANE_HEIGHT_PX }),
    ).toBe(true);
    expect(
      canSplitPane(root, "p1", "column", { width: 1200, height: 2 * MIN_PANE_HEIGHT_PX - 1 }),
    ).toBe(false);
  });

  it("cannot refuse what it cannot measure (no host, 0px box, unknown pane)", () => {
    const root = createPaneLayout("t1", "p1");
    expect(canSplitPane(root, "p1", "row", null)).toBe(true);
    expect(canSplitPane(root, "p1", "row", { width: 0, height: 0 })).toBe(true);
    expect(canSplitPane(root, "nope", "row", host)).toBe(true);
    expect(canSplitPane(null, "p1", "row", host)).toBe(true);
  });
});

/**
 * THE MEASURED HOST of the user's 7-pane report: an 906×600 main area. Both numbers come from the
 * review — the separator band was measured 8×600, and 906px is the width whose panes reproduce
 * the reading below exactly.
 */
const REPORTED_HOST: HostSize = { width: 906, height: 600 };

/**
 * THE 7-PANE TREE THAT PRODUCED THE REPORTED READING — three panes at 18px.
 *
 * Every ratio here is INSIDE the relative 15..85% band, which is the whole point of the finding:
 * the relative clamp is satisfied at every single split, yet the composition of five nested row
 * splits leaves three terminals 18px wide. Widths on {@link REPORTED_HOST}, in leaf order:
 * 408 / 204 / 204 / 36 / 18 / 18 / 18 — the multiset the user read off the DOM (whose order is
 * the terminals' record id, not the tree's).
 */
function reportedSevenPanes(): PaneNode {
  const leaf = (n: number): PaneNode => ({ kind: "leaf", id: `p${n}`, terminalId: `t${n}` });
  const rowSplit = (id: string, ratio: number, a: PaneNode, b: PaneNode): PaneNode => ({
    kind: "split",
    id,
    direction: "row",
    ratio,
    children: [a, b],
  });
  //            906
  //   408 ┃            498
  //       204 ┃          294
  //            204 ┃      90
  //                 36 ┃  54
  //                     18 ┃ 36
  //                         18 ┃ 18
  return rowSplit(
    "s1",
    408 / 906,
    leaf(1),
    rowSplit(
      "s2",
      204 / 498,
      leaf(2),
      rowSplit(
        "s3",
        204 / 294,
        leaf(3),
        rowSplit(
          "s4",
          36 / 90,
          leaf(4),
          rowSplit("s5", 18 / 54, leaf(5), rowSplit("s6", 0.5, leaf(6), leaf(7))),
        ),
      ),
    ),
  );
}

describe("the absolute floor holds where the relative clamp did not", () => {
  it("REPRODUCES the reported 7-pane reading — every ratio in band, three panes at 18px", () => {
    const root = reportedSevenPanes();
    const widths = paneRects(root).map((p) => (REPORTED_HOST.width * p.rect.width) / 100);
    expect(widths.map((w) => Math.round(w))).toEqual([408, 204, 204, 36, 18, 18, 18]);

    // Every single split obeys the RELATIVE band — the bug is not one bad ratio, it is their
    // composition, which is exactly why a relative-only clamp cannot fix it.
    for (const sep of paneSeparators(root)) {
      expect(sep.ratio).toBeGreaterThanOrEqual(MIN_SPLIT_RATIO);
      expect(sep.ratio).toBeLessThanOrEqual(MAX_SPLIT_RATIO);
    }
    expect(Math.min(...widths)).toBeLessThan(MIN_PANE_WIDTH_PX);
  });

  it("REFUSES the very split that produced it: the 90px box can never be cut", () => {
    // Replay the construction through the GUARD. The first four cuts are legitimate; the fifth
    // (cutting the 90px remainder) is the one that manufactured the 18px panes — and it is now
    // refused, so the tree above is unreachable.
    let root: PaneNode = createPaneLayout("t1", "p1");
    const cuts: { pane: string; ratio: number; next: string; term: string }[] = [
      { pane: "p1", ratio: 408 / 906, next: "p2", term: "t2" },
      { pane: "p2", ratio: 204 / 498, next: "p3", term: "t3" },
      { pane: "p3", ratio: 204 / 294, next: "p4", term: "t4" },
      { pane: "p4", ratio: 36 / 90, next: "p5", term: "t5" },
    ];
    // The FIRST cut is allowed (906 → 453 / 453, both over 180)…
    expect(canSplitPane(root, "p1", "row", REPORTED_HOST)).toBe(true);
    root = split(root, "p1", "t2", "row", "p2", cuts[0]?.ratio);
    // …the SECOND is already refused: the remaining 498px box would have to become 249/249, which
    // is fine — but the 408px pane cannot be cut (204/204 is over the floor, so that one passes).
    expect(canSplitPane(root, "p2", "row", REPORTED_HOST)).toBe(true);
    root = split(root, "p2", "t3", "row", "p3", cuts[1]?.ratio);
    // The 294px remainder: 147/147 — UNDER 180. The chain stops here, five splits short of the
    // 18px panes the user measured.
    expect(canSplitPane(root, "p3", "row", REPORTED_HOST)).toBe(false);
    const widths = paneRects(root).map((p) => (REPORTED_HOST.width * p.rect.width) / 100);
    expect(Math.min(...widths)).toBeGreaterThanOrEqual(MIN_PANE_WIDTH_PX);
  });

  /**
   * THE FIVE KEYSTROKES THAT PARKED A PANE AT 36px (review round 2, critical) — replayed through
   * the real key maths, at a CONSTANT 720×900 window, with NO resize anywhere.
   *
   * Every step of the sequence is legitimate: two GUARDED splits (the third is refused), then
   * End / End / Home on separators whose bounds the module itself computed. The reading before the
   * fix was 72 / 36 / 612px against a 180px floor — worse than the 18px report that motivated the
   * floor in the first place.
   */
  it("REPLAYS the five keystrokes that used to park a pane at 36px on a CONSTANT window", () => {
    const host: HostSize = { width: 720, height: 900 };
    const widths = (node: PaneNode) =>
      paneRects(node).map((p) => (host.width * p.rect.width) / 100);
    /** One keypress on the `index`-th separator (outermost first), applied to the tree. */
    const press = (node: PaneNode, index: number, key: string): PaneNode => {
      const sep = paneSeparators(node)[index] as PaneSeparator;
      return setSplitRatio(node, sep.splitId, ratioFromKey(sep, key, host) as number);
    };

    let root: PaneNode = createPaneLayout("t0", "p0");
    // G1 / G2 — two splits the guard allows; the third is refused (180px cannot be cut in two).
    expect(canSplitPane(root, "p0", "row", host)).toBe(true);
    root = split(root, "p0", "t1", "row", "p1");
    expect(canSplitPane(root, "p0", "row", host)).toBe(true);
    root = split(root, "p0", "t2", "row", "p2");
    expect(canSplitPane(root, "p0", "row", host)).toBe(false);
    expect(widths(root).map(Math.round)).toEqual([180, 180, 360]);

    // G3 — End on the OUTER seam: the strict px clamp stops it at 75%, three panes at 270/270/180.
    root = press(root, 0, "End");
    expect(widths(root).map(Math.round)).toEqual([270, 270, 180]);

    // G4 — End on the INNER seam. THIS is where the outer split's demand becomes 540.0000000000001
    // against a 720px box, and where the bounds used to flip to the full 15..85 course.
    root = press(root, 1, "End");
    const outer = paneSeparators(root)[0] as PaneSeparator;
    expect(separatorFloorSatisfiable(outer, host)).toBe(true);
    expect(
      separatorRatioBounds(outer, host).min,
      "the outer seam is still px-clamped after the inner one hit its stop",
    ).toBeGreaterThan(0.7);

    // G5 — Home on the OUTER seam: the stop of a course that must still be the px one.
    root = press(root, 0, "Home");
    expect(
      Math.min(...widths(root)),
      // Held to the TESTS' tolerance, not the module's (review round 3): the measured minimum here
      // is 179.99999999999994px, 6e-14 under the floor — 16 million times inside the 1e-6px this
      // file allows, and unchanged by whatever the module's own epsilon becomes. The pre-fix
      // reading at this exact point was 36.00px.
      "no pane may sit under the 180px floor at a constant window size",
    ).toBeGreaterThanOrEqual(MIN_PANE_WIDTH_PX - FLOOR_TEST_TOLERANCE_PX);
  });

  /**
   * THE CONCESSION THAT MANUFACTURED A STARVATION (review round 3, critical), pinned on both halves
   * of the mechanism — because the walk below needs ~5 900 gestures and a 43-pane tiling to reach
   * it, and a defect that only a five-minute walk can name is a defect nobody will bisect.
   *
   * WHAT WENT WRONG. A tolerance here is never only a comparison: when the band it accepts has its
   * ends CROSSED, a ratio still has to be picked, and the pick CONCEDES the crossing to the panes
   * below. Conceded at the MIDPOINT, the crossing was split in absolute pixels, so the side with
   * the SMALLER demand paid the larger FRACTION of itself — and a fraction re-read at an ancestor
   * is worth that share of the ancestor's much bigger box. 0.05px conceded on a ~1500px sub-box
   * came back as 0.064px of starvation at the 1920px root, 1.29× the very tolerance that had
   * allowed it, and the root then handed back 15..85 AT A CONSTANT WINDOW.
   *
   * Neither half is optional: `FLOOR_EPSILON_RATIO` makes what is forgiven a fraction of the
   * sub-box, and the PROPORTIONAL pinning below makes what is conceded a fraction bounded by it.
   */
  it("pays a forgiven crossing PROPORTIONALLY, and stays satisfiable 16 levels down at 1920×1080", () => {
    const host: HostSize = { width: 1920, height: 1080 };

    // A — THE PINNING. A 1920px sub-box owing 200 + 1720.000001px: the two demands cross by a
    // MILLIONTH of a pixel, inside what the module forgives, so SOMEONE is about to be shortchanged.
    const sep = separator({ minFirstPx: 200, minSecondPx: 1720 + 1e-6 });
    expect(separatorFloorSatisfiable(sep, host), "a 1e-6px crossing is not a starvation").toBe(
      true,
    );
    const band = separatorRatioBounds(sep, host);
    expect(band.min, "a crossed band is pinned onto ONE point").toBe(band.max);
    expect(band).not.toEqual({ min: MIN_SPLIT_RATIO, max: MAX_SPLIT_RATIO });

    // WHO PAYS, and in what proportion, IS the fix. Proportionally: both sides come out short by
    // the SAME FRACTION, so the deficit inherited below is bounded by the tolerance that allowed
    // it. The MIDPOINT this replaced halves the crossing in ABSOLUTE px, so the smaller demand pays
    // 1720/200 = 8.6× more of itself (measured: 2.5e-9 against 2.9e-10) — and 8.6× is exactly how
    // a tolerance grows into a starvation one level up.
    const firstDeficitRel = (sep.minFirstPx - host.width * band.min) / sep.minFirstPx;
    const secondDeficitRel = (sep.minSecondPx - host.width * (1 - band.max)) / sep.minSecondPx;
    expect(firstDeficitRel, "the first side really is short").toBeGreaterThan(0);
    expect(secondDeficitRel, "the second side really is short").toBeGreaterThan(0);
    expect(
      firstDeficitRel / secondDeficitRel,
      "both sides pay the same FRACTION of themselves (the midpoint pays 8.6:1)",
    ).toBeCloseTo(1, 3);
    // …and that fraction is bounded by the tolerance that let the crossing through — the property
    // the whole closure rests on.
    expect(firstDeficitRel).toBeLessThan(FLOOR_EPSILON_RATIO);

    // B — THE SHAPE THE FINDING NAMED: a REAL nesting at 1920×1080, ~10 levels and past, with every
    // seam pushed onto its stop, so every sub-box sits EXACTLY on its own demand. Nothing here may
    // read as starved, and no band may be the relative 15..85 course.
    let root: PaneNode = createPaneLayout("t0", "p0");
    let n = 0;
    for (let round = 0; round < 40; round++) {
      const direction = round % 2 === 0 ? "row" : "column";
      const first = paneRects(root)[0] as ReturnType<typeof paneRects>[number];
      if (canSplitPane(root, first.paneId, direction, host)) {
        n += 1;
        root = split(root, first.paneId, `t${n}`, direction, `p${n}`);
      }
      for (const splitId of paneSeparators(root).map((s) => s.splitId)) {
        const seam = paneSeparators(root).find((s) => s.splitId === splitId);
        if (seam == null) continue;
        root = setSplitRatio(root, splitId, ratioFromKey(seam, "End", host) as number);
      }
    }
    const depth = (node: PaneNode): number =>
      node.kind === "leaf" ? 0 : 1 + Math.max(depth(node.children[0]), depth(node.children[1]));
    expect(n, "the nesting really went deep").toBe(16);
    expect(depth(root), "…and it really is nested, not a bush").toBeGreaterThanOrEqual(10);

    const seams = paneSeparators(root);
    for (const seam of seams) {
      expect(separatorFloorSatisfiable(seam, host), `${seam.splitId} at a constant host`).toBe(
        true,
      );
      // A WHOLE-SHAPE `not.toEqual`, and it only asserts anything because the band really is a
      // bare `RatioBand`: 4 of these 16 seams are SATURATED, and while that branch handed the
      // module's internal px band back, those 4 carried `slackPx` / `spanPx` and could not have
      // failed even at exactly 15..85 — 12 assertions holding and 4 decorative (round 5).
      expect(separatorRatioBounds(seam, host), `${seam.splitId} band`).not.toEqual({
        min: MIN_SPLIT_RATIO,
        max: MAX_SPLIT_RATIO,
      });
    }
    // The OUTERMOST seam is where an inherited deficit lands, magnified by the whole tree: its
    // sub-box is the host itself, so this number IS the accounting. Measured here: 2.3e-13px of
    // shortfall, against 5.9e-10px — 2 600× more — when the crossing was conceded at the midpoint.
    const outer = seams[0] as PaneSeparator;
    const outerSpan = (host.width * outer.bounds.width) / 100;
    expect(outerSpan).toBe(host.width);
    expect(
      outerSpan - outer.minFirstPx - outer.minSecondPx,
      "the root's sub-box still pays both demands after 16 levels of stops",
    ).toBeGreaterThan(-1e-11);
    // …and the panes it all comes down to are still on the floor, to the tests' own tolerance.
    const boxes = paneRects(root).map((p) => ({
      w: (host.width * p.rect.width) / 100,
      h: (host.height * p.rect.height) / 100,
    }));
    expect(Math.min(...boxes.map((b) => b.w))).toBeGreaterThan(
      MIN_PANE_WIDTH_PX - FLOOR_TEST_TOLERANCE_PX,
    );
    expect(Math.min(...boxes.map((b) => b.h))).toBeGreaterThan(
      MIN_PANE_HEIGHT_PX - FLOOR_TEST_TOLERANCE_PX,
    );
  });

  /**
   * THE INVARIANT OF THE WHOLE FEATURE, unconditional: at a CONSTANT window size, NO sequence of
   * guarded splits and seam pushes can put a pane under 180 × 120px. Not "except when…" — there is
   * no exception to make, and the walk asserts there is none: `separatorFloorSatisfiable` is
   * checked to hold at EVERY separator at every step, so a starved sub-box (the one regime where
   * the floor is best-effort) is a FAILURE here rather than an excuse. That regime is reached by
   * SHRINKING the window, and is covered on its own terms two tests down.
   *
   * IT RUNS ON TEN HOSTS, and THAT is the finding of review round 3 — not a flourish. The walk used
   * to be parameterised on 1600×900 alone, and 1600×900 happens to be one of the sizes on which the
   * defect did not show. On the same code, 1920×1080 crushed a pane to 19.17px, 2560×1440 to
   * 14.38px of height and 7680×1440 to 18.73px, while 1600×900, 3440×1440 and 3840×2160 stayed
   * clean: what trips it is the SHAPE of the reachable tree, not a size threshold, so no single
   * host can stand for the others. All ten are listed below, the broken and the clean alike.
   *
   * IT ALSO MEASURES ITS OWN TOLERANCE. Collecting violations is not enough when the thing under
   * test IS a tolerance: the walk records the worst shortfall it ever asked the module to forgive
   * and the worst deficit a pane ever ran, and holds both to numbers written in THIS file
   * ({@link FLOOR_TEST_TOLERANCE_PX}, {@link FLOOR_TEST_DRIFT_CAP_REL}) — so the margin is asserted
   * instead of assumed, and the assertion cannot be widened by editing the module.
   *
   * COST. The walk is deliberately exhaustive per step — every pane × every direction, every seam
   * pushed to BOTH stops, every pane re-measured after every mutation. What used to make it slow
   * was not the walk but the ASSERTION: two `expect()` per pane per mutation, each building its
   * message eagerly, is millions of assertions and ~56s of vitest bookkeeping around ~2s of
   * geometry. Violations are therefore COLLECTED and asserted ONCE. The step budget shrinks as the
   * host grows because the work per step is quadratic in the number of separators (a 7680×4320 host
   * saturates at ~145 panes against ~48 at 1600×900), and every budget below is past the point
   * where its host stops growing new panes.
   */
  const INVARIANT_HOSTS: readonly (readonly [HostSize, number])[] = [
    [{ width: 720, height: 900 }, 200], // the window of the round-2 critical
    [{ width: 1600, height: 900 }, 400], // was the ONLY host — clean, and that was the problem
    [{ width: 1920, height: 1080 }, 300], // BROKEN before this round: a pane at 19.17px
    [{ width: 2560, height: 1440 }, 200], // BROKEN: 14.38px of height
    [{ width: 3440, height: 1440 }, 160], // clean before this round
    [{ width: 3840, height: 2160 }, 140], // clean before this round
    [{ width: 5120, height: 1440 }, 120], // BROKEN: 31.76 × 10.39px
    [{ width: 5120, height: 2160 }, 120], // BROKEN: 14.69px wide
    [{ width: 7680, height: 1440 }, 100], // BROKEN: 18.73px wide — three 2560×1440 side by side
    [{ width: 7680, height: 4320 }, 80], // BROKEN: 31.76 × 10.53px
  ];

  for (const [host, steps] of INVARIANT_HOSTS) {
    const hostName = `${host.width} × ${host.height}`;
    it(`NO combination of guarded splits and dragged seams goes under 180 × 120px at ${hostName}`, () => {
      // A deterministic walk over the whole guarded surface: at every step it tries EVERY pane ×
      // EVERY direction (keeping only what the guard allows) and then pushes EVERY separator to
      // BOTH extremes through the real pointer maths. After each mutation, every pane is measured.
      //
      // Violations are COLLECTED, not asserted on the spot: one `expect()` at the end instead of
      // two per pane per mutation (see the note above). The first few are kept verbatim — a failure
      // must still name the pane, its box and the step that produced it — and the rest are only
      // counted, so a broken floor cannot flood the heap before it is reported.
      const violations: string[] = [];
      let violationCount = 0;
      const violation = (message: string) => {
        violationCount += 1;
        if (violations.length < 5) violations.push(message);
      };
      const px = (n: number) => n.toFixed(2);
      // What the walk MEASURES about the tolerance itself (see the note above the test).
      let worstFeasibleShortfallRel = 0; // the deepest shortfall the module was asked to forgive
      let worstPaneDeficitPx = 0; // the deepest a pane ever went under its floor

      const check = (root: PaneNode, what: string) => {
        // At a CONSTANT window size the floor has no best-effort regime to fall into: a starved
        // sub-box would itself be the bug (it is reached by SHRINKING the window, not by moving a
        // seam), so it is reported rather than used as an excuse for the panes inside it.
        for (const sep of paneSeparators(root)) {
          const span =
            sep.direction === "row"
              ? (host.width * sep.bounds.width) / 100
              : (host.height * sep.bounds.height) / 100;
          if (separatorFloorSatisfiable(sep, host)) {
            // Feasible — but by HOW MUCH? This is the drift the module's tolerance exists to
            // absorb, and the only honest way to know it is far enough under it.
            const shortfallRel = (span - sep.minFirstPx - sep.minSecondPx) / span;
            if (shortfallRel < worstFeasibleShortfallRel) worstFeasibleShortfallRel = shortfallRel;
          } else {
            violation(
              `${what}: ${sep.splitId} went STARVED at a constant ${hostName} host (claims ${px(sep.minFirstPx)} + ${px(sep.minSecondPx)}px of a ${px(span)}px sub-box)`,
            );
          }
        }
        for (const placement of paneRects(root)) {
          const w = (host.width * placement.rect.width) / 100;
          const h = (host.height * placement.rect.height) / 100;
          worstPaneDeficitPx = Math.max(
            worstPaneDeficitPx,
            MIN_PANE_WIDTH_PX - w,
            MIN_PANE_HEIGHT_PX - h,
          );
          if (
            w < MIN_PANE_WIDTH_PX - FLOOR_TEST_TOLERANCE_PX ||
            h < MIN_PANE_HEIGHT_PX - FLOOR_TEST_TOLERANCE_PX
          ) {
            violation(`${what}: ${placement.paneId} is ${px(w)} × ${px(h)}px`);
          }
        }
      };

      // A tiny deterministic PRNG — the walk must be reproducible, so a failure is a bug report.
      let seed = 0x9e3779b9;
      const random = () => {
        seed = (seed + 0x6d2b79f5) | 0;
        let t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
        t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
        return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
      };

      let root: PaneNode = createPaneLayout("t0", "p0");
      let n = 0;
      let refused = 0;
      for (let step = 0; step < steps; step++) {
        const panes = paneRects(root);
        const target = panes[Math.floor(random() * panes.length)] as (typeof panes)[number];
        const direction = random() < 0.5 ? "row" : "column";
        if (canSplitPane(root, target.paneId, direction, host)) {
          n += 1;
          root = split(root, target.paneId, `t${n}`, direction, `p${n}`);
          check(root, `after splitting ${target.paneId} ${direction}`);
        } else {
          refused += 1;
          // The PURE mutator would happily have built it — the FLOOR is what refuses, and it is
          // consulted BEFORE the tree is touched, so the refusal leaves nothing behind. Compared
          // against an independent DEEP COPY taken first: `toBe(root)` could only ever compare a
          // variable with itself and would survive `splitPane` rewriting the tree in place.
          const before = structuredClone(root);
          expect(
            splitPane(root, { paneId: target.paneId, direction, terminalId: `x${step}` }),
          ).not.toBeNull();
          expect(root, "a refused split leaves the tree untouched").toEqual(before);
        }

        // Now shove every seam as far as a pointer can push it, both ways. The separator list is
        // RE-DERIVED at each push: moving one seam changes every descendant's sub-box, so a stale
        // one would be clamped against a box that no longer exists.
        const splitIds = paneSeparators(root).map((s) => s.splitId);
        for (const splitId of splitIds) {
          for (const push of [-100000, 100000]) {
            const sep = paneSeparators(root).find((s) => s.splitId === splitId);
            if (sep == null) continue;
            const ratio = ratioFromPointer(sep, { x: push, y: push }, host);
            root = setSplitRatio(root, splitId, ratio);
            check(root, `after dragging ${splitId} to ${push}`);
          }
        }
      }
      // The walk must have DONE something on both sides, or it proves nothing.
      expect(n, `the walk really tiled ${hostName}`).toBeGreaterThan(5);
      expect(refused, `the guard really refused at ${hostName}`).toBeGreaterThan(5);
      // THE INVARIANT — unconditional, and reported in one place, naming the host it really ran on.
      expect(
        violations,
        `${violationCount} violation(s) of the 180 × 120px floor at a constant ${hostName} host`,
      ).toEqual([]);
      // …and the MARGIN, measured rather than assumed (see the note above the test).
      expect(
        -worstFeasibleShortfallRel,
        `worst shortfall forgiven at ${hostName}, as a fraction of the sub-box`,
      ).toBeLessThan(FLOOR_TEST_DRIFT_CAP_REL);
      expect(
        worstPaneDeficitPx,
        `worst pane deficit under the floor at ${hostName}, in px`,
      ).toBeLessThan(FLOOR_TEST_TOLERANCE_PX);
    });
  }

  /**
   * THE STARVED REGIME, on its own terms — the case the review actually arbitrated: the WINDOW
   * SHRINKS under an existing tiling. It is CONSTRUCTED here, not stumbled upon, because it cannot
   * be reached by moving a seam (the walk above asserts exactly that).
   *
   * The arbitrage: below the point where the sub-box can hold both sides at the floor, the floor
   * becomes BEST-EFFORT and the relative 15..85 course comes back — freezing the seam would
   * protect nothing (both panes are already under the floor) and would remove the user's one
   * manual recovery.
   */
  it("a SHRUNK window hands the 15..85 course back, and the crushed pane can be dragged out", () => {
    // A plain 50/50 tiling, comfortable at 1600px…
    const root = split(createPaneLayout("t0", "p0"), "p0", "t1", "row", "p1");
    const roomy: HostSize = { width: 1600, height: 900 };
    const widthsOn = (node: PaneNode, host: HostSize) =>
      paneRects(node).map((p) => (host.width * p.rect.width) / 100);
    expect(widthsOn(root, roomy)).toEqual([800, 800]);
    expect(separatorFloorSatisfiable(paneSeparators(root)[0] as PaneSeparator, roomy)).toBe(true);

    // …and the SAME tree measured after the window shrank under 2 × 180px. Nothing was rebalanced:
    // both panes are now 170px, i.e. BOTH under the floor — the literal premise of the arbitrage.
    const cramped: HostSize = { width: 340, height: 900 };
    expect(widthsOn(root, cramped)).toEqual([170, 170]);
    const sep = paneSeparators(root)[0] as PaneSeparator;
    expect(separatorFloorSatisfiable(sep, cramped)).toBe(false);
    expect(separatorRatioBounds(sep, cramped)).toEqual({
      min: MIN_SPLIT_RATIO,
      max: MAX_SPLIT_RATIO,
    });

    // The seam MOVES again — pointer and keys alike, where a frozen band would have made both inert.
    expect(ratioFromPointer(sep, { x: 240, y: 0 }, cramped)).toBeCloseTo(240 / 340, 6);
    expect(ratioFromKey(sep, "ArrowRight", cramped)).toBeCloseTo(0.5 + SEPARATOR_KEY_STEP, 6);
    expect(ratioFromKey(sep, "End", cramped)).toBe(MAX_SPLIT_RATIO);

    // …and the recovery the arbitrage exists for: End carries the first pane back OVER the floor,
    // at the second one's expense. Best-effort means one readable pane instead of two unusable ones.
    const recovered = setSplitRatio(root, sep.splitId, ratioFromKey(sep, "End", cramped) as number);
    const [first, second] = widthsOn(recovered, cramped) as [number, number];
    expect(first).toBeGreaterThanOrEqual(MIN_PANE_WIDTH_PX);
    expect(first + second).toBeCloseTo(cramped.width, 6);
  });
});

/**
 * DIRECTIONAL FOCUS — the Alt+arrows neighbour (review 01M0ABR6V501D0SPR2BYBVZC4A, minor).
 *
 * "The neighbour in that direction" is a GEOMETRY question, not a tree-order one: once splits nest,
 * the leaf order says nothing about what is to the LEFT of a pane, and a user pressing Alt+← is
 * pointing at the screen.
 */
describe("paneNeighbor", () => {
  /**
   * A FOUR-PANE tiling of UNEQUAL sizes:
   *
   *   ┌──────────────┬─────────────────────────┐
   *   │      A       │            B            │   A: 0..30 × 0..60
   *   │  (0..30 w,   ├────────────┬────────────┤   B: 30..100 × 0..40
   *   │   0..60 h)   │     C      │     D      │   C: 30..65 × 40..100
   *   ├──────────────┤            │            │   D: 65..100 × 40..100
   *   │      E       │            │            │   E: 0..30 × 60..100
   *   └──────────────┴────────────┴────────────┘
   */
  const TILING: PaneNode = {
    kind: "split",
    id: "s-root",
    direction: "row",
    ratio: 0.3,
    children: [
      {
        kind: "split",
        id: "s-left",
        direction: "column",
        ratio: 0.6,
        children: [
          { kind: "leaf", id: "pa", terminalId: "a" },
          { kind: "leaf", id: "pe", terminalId: "e" },
        ],
      },
      {
        kind: "split",
        id: "s-right",
        direction: "column",
        ratio: 0.4,
        children: [
          { kind: "leaf", id: "pb", terminalId: "b" },
          {
            kind: "split",
            id: "s-bottom",
            direction: "row",
            ratio: 0.5,
            children: [
              { kind: "leaf", id: "pc", terminalId: "c" },
              { kind: "leaf", id: "pd", terminalId: "d" },
            ],
          },
        ],
      },
    ],
  };

  it("resolves the neighbour in all FOUR directions on unequal panes", () => {
    // RIGHT: A (y 0..60, centre 30) faces B (y 0..40, centre 20) and C (y 40..100, centre 70);
    // B's centre is 10 away, C's is 40 — B wins.
    expect(paneNeighbor(TILING, "pa", "right")).toBe("pb");
    // LEFT: from B (y 0..40) the only pane whose RIGHT edge is 30 % AND overlaps it vertically is
    // A — E sits at y 60..100 and does not face B at all.
    expect(paneNeighbor(TILING, "pb", "left")).toBe("pa");
    // DOWN: from A, the pane whose top edge is 60 % on the left column.
    expect(paneNeighbor(TILING, "pa", "down")).toBe("pe");
    // UP: from C, the pane whose bottom edge is 40 % over the right column.
    expect(paneNeighbor(TILING, "pc", "up")).toBe("pb");
    // RIGHT again, one level deeper: C → D.
    expect(paneNeighbor(TILING, "pc", "right")).toBe("pd");
    expect(paneNeighbor(TILING, "pd", "left")).toBe("pc");
  });

  it("SEVERAL CANDIDATES: the one whose centre is nearest on the perpendicular axis wins", () => {
    // From C (y 40..100, centre 70) the panes touching its LEFT edge are A (y 0..60, centre 30)
    // and E (y 60..100, centre 80). E's centre is 10 away, A's is 40 — so E should win, and does.
    // (The direction of the tie-break is what this pins: it is the CENTRE distance, not the tree.)
    expect(paneNeighbor(TILING, "pc", "left")).toBe("pe");
  });

  it("does NOT move without a neighbour on that side — no wrap, no jump to the far end", () => {
    expect(paneNeighbor(TILING, "pa", "left")).toBeNull();
    expect(paneNeighbor(TILING, "pa", "up")).toBeNull();
    expect(paneNeighbor(TILING, "pd", "right")).toBeNull();
    expect(paneNeighbor(TILING, "pd", "down")).toBeNull();
    // …and E, at the bottom-left, has nothing below or to its left either.
    expect(paneNeighbor(TILING, "pe", "down")).toBeNull();
    expect(paneNeighbor(TILING, "pe", "left")).toBeNull();
  });

  it("a SOLO pane has no neighbour in any direction", () => {
    const solo = createPaneLayout("only", "p-only");
    for (const direction of ["left", "right", "up", "down"] as const) {
      expect(paneNeighbor(solo, "p-only", direction)).toBeNull();
    }
  });

  it("an unknown pane id resolves to nothing rather than guessing", () => {
    expect(paneNeighbor(TILING, "p-gone", "right")).toBeNull();
  });

  it("is a pure lookup — it never touches the tree", () => {
    const before = JSON.stringify(TILING);
    paneNeighbor(TILING, "pa", "right");
    expect(JSON.stringify(TILING)).toBe(before);
  });
});
