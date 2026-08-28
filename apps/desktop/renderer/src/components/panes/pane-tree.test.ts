import { describe, expect, it, vi } from "vitest";

import {
  clampRatio,
  closePane,
  createPaneLayout,
  DEFAULT_SPLIT_RATIO,
  findPane,
  findPaneByTerminal,
  isPaneNode,
  MAX_SPLIT_RATIO,
  MAX_STORED_RATIO,
  MIN_SPLIT_RATIO,
  MIN_STORED_RATIO,
  movePaneToEdge,
  paneAfterClose,
  paneCount,
  paneLeaves,
  paneSwapOf,
  paneTerminalIds,
  pruneStaleTerminals,
  setSplitRatio,
  splitCount,
  splitPane,
  type PaneNode,
  type PaneSplit,
} from "./pane-tree";

// The terminal STORE is mocked so the invariant "closing a pane never closes a terminal" is
// asserted, not assumed: a pane-tree mutation that ever reached for `disposeTerminal` would fail
// the dedicated test below.
vi.mock("@/components/terminal/terminal-store", () => ({
  disposeTerminal: vi.fn(),
  detachTerminal: vi.fn(),
  getTerminal: vi.fn(),
}));

/**
 * Assert the WHOLE-TREE invariants after any mutation: exactly two children per split, unique
 * pane/split ids, unique terminal ids, ratios inside the STORABLE band (the aesthetic 15..85 one
 * is `separatorRatioBounds`' business — a seam may legitimately sit outside it when the px floor
 * demands it), and a COMPACTED shape (a tree of N panes has exactly N-1 splits — no split left
 * holding a single child).
 */
function expectInvariants(node: PaneNode): void {
  expect(isPaneNode(node)).toBe(true);
  expect(splitCount(node)).toBe(paneCount(node) - 1);
  const walk = (n: PaneNode): void => {
    if (n.kind === "leaf") return;
    expect(n.children).toHaveLength(2);
    expect(n.ratio).toBeGreaterThanOrEqual(MIN_STORED_RATIO);
    expect(n.ratio).toBeLessThanOrEqual(MAX_STORED_RATIO);
    walk(n.children[0]);
    walk(n.children[1]);
  };
  walk(node);
}

/** Split `paneId` (asserting the split succeeded) and return the new tree. */
function split(
  root: PaneNode,
  paneId: string,
  terminalId: string,
  direction: "row" | "column" = "row",
  newPaneId?: string,
): PaneNode {
  const next = splitPane(root, { paneId, direction, terminalId, newPaneId });
  expect(next).not.toBeNull();
  expectInvariants(next as PaneNode);
  return next as PaneNode;
}

describe("createPaneLayout", () => {
  it("is a single leaf bound to one terminal", () => {
    const root = createPaneLayout("t1", "p1");
    expect(root).toEqual({ kind: "leaf", id: "p1", terminalId: "t1" });
    expect(paneCount(root)).toBe(1);
    expectInvariants(root);
  });

  it("generates a unique pane id when none is supplied", () => {
    const a = createPaneLayout("t1");
    const b = createPaneLayout("t2");
    expect(a.id).not.toBe(b.id);
    expect(a.id.length).toBeGreaterThan(0);
  });
});

describe("paneTerminalIds", () => {
  it("is empty for NO layout (nothing is on screen before the first restore)", () => {
    expect(paneTerminalIds(null)).toEqual([]);
  });
});

describe("clampRatio", () => {
  /**
   * The band clamped here is the STORABLE one (2..98%), NOT the aesthetic 15..85. A seam may
   * legitimately park outside 15..85 when the absolute px floor demands more than the 85% ceiling
   * allows while the sub-box still has the room (user arbitrage, round 2): this model has no
   * pixels and cannot re-derive that, so it enforces only what holds without a host — a split
   * keeps both sides on screen. `separatorRatioBounds` is what applies 15..85, and every drag and
   * key goes through it before reaching a mutator.
   */
  it("bounds the ratio to the STORABLE band, which CONTAINS the aesthetic one", () => {
    expect(clampRatio(0.5)).toBe(0.5);
    expect(clampRatio(0)).toBe(MIN_STORED_RATIO);
    expect(clampRatio(-3)).toBe(MIN_STORED_RATIO);
    expect(clampRatio(1)).toBe(MAX_STORED_RATIO);
    expect(clampRatio(42)).toBe(MAX_STORED_RATIO);
    // The px floor's own demands live in the gap between the two bands — they must survive.
    expect(clampRatio(0.875)).toBe(0.875);
    expect(clampRatio(0.1)).toBe(0.1);
    expect(MIN_STORED_RATIO).toBeLessThan(MIN_SPLIT_RATIO);
    expect(MAX_STORED_RATIO).toBeGreaterThan(MAX_SPLIT_RATIO);
  });

  it("falls back to the default for a non-finite ratio", () => {
    expect(clampRatio(Number.NaN)).toBe(DEFAULT_SPLIT_RATIO);
    expect(clampRatio(Number.POSITIVE_INFINITY)).toBe(DEFAULT_SPLIT_RATIO);
  });
});

describe("splitPane", () => {
  it("replaces the target leaf by a two-child split, new pane second", () => {
    const root = split(createPaneLayout("t1", "p1"), "p1", "t2", "row", "p2");
    expect(root.kind).toBe("split");
    const asSplit = root as PaneSplit;
    expect(asSplit.direction).toBe("row");
    expect(asSplit.ratio).toBe(DEFAULT_SPLIT_RATIO);
    expect(asSplit.children.map((c) => (c.kind === "leaf" ? c.id : "split"))).toEqual(["p1", "p2"]);
    expect(paneTerminalIds(root)).toEqual(["t1", "t2"]);
  });

  it("splits a nested pane, keeping the invariants", () => {
    let root: PaneNode = createPaneLayout("t1", "p1");
    root = split(root, "p1", "t2", "row", "p2");
    root = split(root, "p2", "t3", "column", "p3");
    root = split(root, "p3", "t4", "row", "p4");
    expect(paneCount(root)).toBe(4);
    expect(paneTerminalIds(root)).toEqual(["t1", "t2", "t3", "t4"]);
    expectInvariants(root);
  });

  it("clamps the requested ratio", () => {
    const root = splitPane(createPaneLayout("t1", "p1"), {
      paneId: "p1",
      direction: "row",
      terminalId: "t2",
      ratio: 0.99,
    });
    expect((root as PaneSplit).ratio).toBe(MAX_STORED_RATIO);
  });

  it("REFUSES a terminal already visible in the tree (uniqueness) and leaves the tree alone", () => {
    const root = split(createPaneLayout("t1", "p1"), "p1", "t2", "row", "p2");
    expect(splitPane(root, { paneId: "p1", direction: "row", terminalId: "t2" })).toBeNull();
    expect(splitPane(root, { paneId: "p2", direction: "column", terminalId: "t1" })).toBeNull();
    expect(paneTerminalIds(root)).toEqual(["t1", "t2"]);
  });

  it("refuses an unknown pane id", () => {
    const root = createPaneLayout("t1", "p1");
    expect(splitPane(root, { paneId: "nope", direction: "row", terminalId: "t2" })).toBeNull();
  });

  it("does not mutate the input tree", () => {
    const root = createPaneLayout("t1", "p1");
    const snapshot = structuredClone(root);
    split(root, "p1", "t2");
    expect(root).toEqual(snapshot);
  });
});

describe("closePane", () => {
  it("compacts the parent onto the surviving sibling", () => {
    let root: PaneNode = createPaneLayout("t1", "p1");
    root = split(root, "p1", "t2", "row", "p2");
    const closed = closePane(root, "p2");
    expect(closed).toEqual({ kind: "leaf", id: "p1", terminalId: "t1" });
    expectInvariants(closed);
  });

  it("compacts a nested split without disturbing the others", () => {
    let root: PaneNode = createPaneLayout("t1", "p1");
    root = split(root, "p1", "t2", "row", "p2");
    root = split(root, "p2", "t3", "column", "p3");
    const closed = closePane(root, "p3");
    expect(paneTerminalIds(closed)).toEqual(["t1", "t2"]);
    expect(splitCount(closed)).toBe(1);
    expectInvariants(closed);
  });

  it("KEEPS the last pane (a layout always shows one terminal)", () => {
    const root = createPaneLayout("t1", "p1");
    expect(closePane(root, "p1")).toBe(root);
    expect(paneCount(closePane(root, "p1"))).toBe(1);
  });

  it("ignores an unknown pane id", () => {
    const root = split(createPaneLayout("t1", "p1"), "p1", "t2", "row", "p2");
    expect(closePane(root, "ghost")).toBe(root);
  });

  it("NEVER closes the terminal: no store disposal, terminal still addressable elsewhere", async () => {
    const { disposeTerminal, detachTerminal } =
      await import("@/components/terminal/terminal-store");
    let root: PaneNode = createPaneLayout("t1", "p1");
    root = split(root, "p1", "t2", "row", "p2");
    const closed = closePane(root, "p2");
    expect(findPaneByTerminal(closed, "t2")).toBeNull();
    // The layout dropped the pane; the terminal instance/PTY/record are untouched.
    expect(disposeTerminal).not.toHaveBeenCalled();
    expect(detachTerminal).not.toHaveBeenCalled();
  });

  it("keeps every mutation's invariants across a close-heavy sequence", () => {
    let root: PaneNode = createPaneLayout("t1", "p1");
    root = split(root, "p1", "t2", "row", "p2");
    root = split(root, "p2", "t3", "column", "p3");
    root = split(root, "p1", "t4", "column", "p4");
    expectInvariants(root);
    for (const paneId of ["p4", "p3", "p2"]) {
      root = closePane(root, paneId);
      expectInvariants(root);
    }
    expect(paneTerminalIds(root)).toEqual(["t1"]);
  });
});

describe("pruneStaleTerminals", () => {
  it("drops the leaf of a SINGLE terminal that is really gone and compacts", () => {
    // The "one terminal disappeared" direction is this same call with that id left out of the live
    // set — the only path the product uses (`use-pane-layout`'s reconcile effect).
    let root: PaneNode = createPaneLayout("t1", "p1");
    root = split(root, "p1", "t2", "row", "p2");
    const next = pruneStaleTerminals(root, ["t2"]);
    expect(next).toEqual({ kind: "leaf", id: "p2", terminalId: "t2" });
    expectInvariants(next as PaneNode);
  });

  it("keeps only the live terminals and compacts", () => {
    let root: PaneNode = createPaneLayout("t1", "p1");
    root = split(root, "p1", "t2", "row", "p2");
    root = split(root, "p2", "t3", "column", "p3");
    const pruned = pruneStaleTerminals(root, ["t1", "t3"]);
    expect(pruned).not.toBeNull();
    expect(paneTerminalIds(pruned as PaneNode)).toEqual(["t1", "t3"]);
    expectInvariants(pruned as PaneNode);
  });

  it("returns null when every terminal is stale", () => {
    let root: PaneNode = createPaneLayout("t1", "p1");
    root = split(root, "p1", "t2", "row", "p2");
    expect(pruneStaleTerminals(root, [])).toBeNull();
    expect(pruneStaleTerminals(root, ["gone"])).toBeNull();
  });

  it("returns the same tree when nothing is stale", () => {
    const root = split(createPaneLayout("t1", "p1"), "p1", "t2", "row", "p2");
    expect(pruneStaleTerminals(root, ["t1", "t2"])).toBe(root);
  });
});

describe("the substitution mutator is GONE", () => {
  it("does not export setPaneTerminal any more — a terminal is never swapped into a pane", async () => {
    // The v1 sidebar path (`setPaneTerminal`) is what made the tiling inescapable: it replaced the
    // focused pane's terminal instead of showing the picked one. The groups model answers that
    // structurally, so the mutator is DELETED — not merely unused, which would leave the old
    // behaviour one call away.
    const paneTree: Record<string, unknown> = await import("./pane-tree");
    expect(Object.keys(paneTree)).not.toContain("setPaneTerminal");
  });
});

describe("setSplitRatio", () => {
  it("clamps and applies the ratio to the addressed split only", () => {
    let root: PaneNode = createPaneLayout("t1", "p1");
    root = split(root, "p1", "t2", "row", "p2");
    root = split(root, "p2", "t3", "column", "p3");
    const outer = root as PaneSplit;
    const inner = outer.children[1] as PaneSplit;
    const resized = setSplitRatio(root, inner.id, 12) as PaneSplit;
    expect((resized.children[1] as PaneSplit).ratio).toBe(MAX_STORED_RATIO);
    expect(resized.ratio).toBe(outer.ratio);
    expectInvariants(resized);
    // A ratio between the two bands is NOT pulled back: that is where the px floor puts a seam
    // when 85% is not enough to keep both sides over 180 × 120px (user arbitrage, round 2).
    expect((setSplitRatio(root, inner.id, 0.95) as PaneSplit).children[1]).toMatchObject({
      ratio: 0.95,
    });
  });

  it("ignores an unknown split id", () => {
    const root = split(createPaneLayout("t1", "p1"), "p1", "t2", "row", "p2");
    expect(setSplitRatio(root, "ghost", 0.3)).toBe(root);
    expect(setSplitRatio(createPaneLayout("t1", "p1"), "ghost", 0.3).kind).toBe("leaf");
  });
});

describe("lookup helpers", () => {
  it("finds panes by id and by terminal", () => {
    const root = split(createPaneLayout("t1", "p1"), "p1", "t2", "row", "p2");
    expect(findPane(root, "p2")?.terminalId).toBe("t2");
    expect(findPane(root, "ghost")).toBeNull();
    expect(findPaneByTerminal(root, "t1")?.id).toBe("p1");
    expect(findPaneByTerminal(root, "ghost")).toBeNull();
    expect(paneLeaves(root).map((l) => l.id)).toEqual(["p1", "p2"]);
  });

  it("names the pane that takes focus after a close (the sibling subtree)", () => {
    let root: PaneNode = createPaneLayout("t1", "p1");
    root = split(root, "p1", "t2", "row", "p2");
    root = split(root, "p2", "t3", "column", "p3");
    expect(paneAfterClose(root, "p3")).toBe("p2");
    expect(paneAfterClose(root, "p1")).toBe("p2");
    expect(paneAfterClose(createPaneLayout("t1", "p1"), "p1")).toBeNull();
  });
});

describe("isPaneNode", () => {
  const leaf = { kind: "leaf", id: "p1", terminalId: "t1" };

  it("accepts a well-formed tree", () => {
    expect(isPaneNode(leaf)).toBe(true);
    expect(
      isPaneNode({
        kind: "split",
        id: "s1",
        direction: "column",
        ratio: 0.5,
        children: [leaf, { kind: "leaf", id: "p2", terminalId: "t2" }],
      }),
    ).toBe(true);
  });

  it("rejects non-objects and unknown kinds", () => {
    expect(isPaneNode(null)).toBe(false);
    expect(isPaneNode("leaf")).toBe(false);
    expect(isPaneNode({ kind: "pane", id: "p1", terminalId: "t1" })).toBe(false);
    expect(isPaneNode({ kind: "leaf", id: "p1" })).toBe(false);
    expect(isPaneNode({ kind: "leaf", id: "", terminalId: "t1" })).toBe(false);
  });

  it("rejects a split that is not binary, mis-directed, or out of band", () => {
    const kids = [leaf, { kind: "leaf", id: "p2", terminalId: "t2" }];
    expect(
      isPaneNode({ kind: "split", id: "s1", direction: "row", ratio: 0.5, children: [leaf] }),
    ).toBe(false);
    expect(
      isPaneNode({
        kind: "split",
        id: "s1",
        direction: "diagonal",
        ratio: 0.5,
        children: kids,
      }),
    ).toBe(false);
    expect(
      isPaneNode({ kind: "split", id: "s1", direction: "row", ratio: 0.99, children: kids }),
    ).toBe(false);
    expect(
      isPaneNode({ kind: "split", id: "s1", direction: "row", ratio: "half", children: kids }),
    ).toBe(false);
    // …but a ratio BETWEEN the two bands is a tree the mutators really can produce (the px floor
    // beating the 85% ceiling — user arbitrage, round 2). Rejecting it would throw the user's
    // whole tiling away at the next restore, so it is ACCEPTED.
    expect(
      isPaneNode({ kind: "split", id: "s1", direction: "row", ratio: 0.9, children: kids }),
    ).toBe(true);
  });

  it("rejects a duplicated terminal id (one terminal = one pane)", () => {
    expect(
      isPaneNode({
        kind: "split",
        id: "s1",
        direction: "row",
        ratio: 0.5,
        children: [leaf, { kind: "leaf", id: "p2", terminalId: "t1" }],
      }),
    ).toBe(false);
  });

  it("rejects duplicated node ids", () => {
    expect(
      isPaneNode({
        kind: "split",
        id: "s1",
        direction: "row",
        ratio: 0.5,
        children: [leaf, { kind: "leaf", id: "p1", terminalId: "t2" }],
      }),
    ).toBe(false);
  });

  it("validates every tree the mutators can produce", () => {
    let root: PaneNode = createPaneLayout("t1");
    root = split(root, paneLeaves(root)[0].id, "t2", "row");
    root = split(root, paneLeaves(root)[1].id, "t3", "column");
    expect(isPaneNode(JSON.parse(JSON.stringify(root)))).toBe(true);
  });
});

/**
 * PERMUTATION vs RESTRUCTURE — the drop indicator's two vocabularies (review finding
 * 01M11WFPFZGAC68VGXJSG6QNMK).
 *
 * The recognition is a DRY RUN: apply `movePaneToEdge`, compare the result to the tree on screen.
 * These specs pin the two families the criterion names, and — the point of the whole exercise — the
 * fact that "permutation" is a property of the RESULT, not of the pane count: a nested tiling of
 * four panes produces one too, and a two-pane tiling produces restructures.
 */
describe("paneSwapOf", () => {
  const row = (a: string, b: string): PaneNode => ({
    kind: "split",
    id: "s-root",
    direction: "row",
    ratio: 0.4,
    children: [
      { kind: "leaf", id: "p-a", terminalId: a },
      { kind: "leaf", id: "p-b", terminalId: b },
    ],
  });

  it("TWO PANES — dropping one on the far edge of the other is a PERMUTATION, and names both", () => {
    const tree = row("ta", "tb");
    const candidate = movePaneToEdge(tree, "p-b", "p-a", "left") as PaneNode;
    expect(paneSwapOf(tree, candidate)).toEqual(["ta", "tb"]);
  });

  it("…and it is NOT a case on the pane count: a FOUR-pane tiling permutes too", () => {
    // col[ row[ta, tb] , row[tc, td] ] — moving `td` onto the RIGHT edge of `tc` swaps the pair,
    // and only that pair: the shape of the whole tiling is untouched.
    const tree: PaneNode = {
      kind: "split",
      id: "q-root",
      direction: "column",
      ratio: 0.5,
      children: [
        {
          kind: "split",
          id: "q-top",
          direction: "row",
          ratio: 0.5,
          children: [
            { kind: "leaf", id: "q-a", terminalId: "ta" },
            { kind: "leaf", id: "q-b", terminalId: "tb" },
          ],
        },
        {
          kind: "split",
          id: "q-bottom",
          direction: "row",
          ratio: 0.5,
          children: [
            { kind: "leaf", id: "q-c", terminalId: "tc" },
            { kind: "leaf", id: "q-d", terminalId: "td" },
          ],
        },
      ],
    };
    const candidate = movePaneToEdge(tree, "q-c", "q-d", "right") as PaneNode;
    expect(paneTerminalIds(candidate)).toEqual(["ta", "tb", "td", "tc"]);
    expect(paneSwapOf(tree, candidate)).toEqual(["tc", "td"]);
  });

  it("THREE PANES — a target that really RESTRUCTURES reads `null` (the edge band stays)", () => {
    // col[ ta , row[tb, tc] ] — dropping `tc` on `ta`'s left rebuilds the shape entirely.
    const tree: PaneNode = {
      kind: "split",
      id: "s-root",
      direction: "column",
      ratio: 0.5,
      children: [
        { kind: "leaf", id: "p-a", terminalId: "ta" },
        {
          kind: "split",
          id: "s-right",
          direction: "row",
          ratio: 0.5,
          children: [
            { kind: "leaf", id: "p-b", terminalId: "tb" },
            { kind: "leaf", id: "p-c", terminalId: "tc" },
          ],
        },
      ],
    };
    const candidate = movePaneToEdge(tree, "p-c", "p-a", "left") as PaneNode;
    expect(paneSwapOf(tree, candidate)).toBeNull();
  });

  it("a drop that changes NOTHING is not a swap either (no pair to name)", () => {
    const tree = row("ta", "tb");
    const candidate = movePaneToEdge(tree, "p-b", "p-a", "right") as PaneNode;
    expect(paneTerminalIds(candidate)).toEqual(["ta", "tb"]);
    expect(paneSwapOf(tree, candidate)).toBeNull();
  });

  it("a THREE-way rotation is not a transposition — it cannot be named as an exchange", () => {
    const tree = row("ta", "tb");
    const rotated: PaneNode = {
      kind: "split",
      id: "s-root",
      direction: "row",
      ratio: 0.4,
      children: [
        { kind: "leaf", id: "p-a", terminalId: "tb" },
        { kind: "leaf", id: "p-b", terminalId: "tc" },
      ],
    };
    expect(paneSwapOf(tree, rotated)).toBeNull();
  });

  it("ignores node ids and ratios, never the DIRECTIONS", () => {
    const tree = row("ta", "tb");
    // The very permutation, re-minted with other ids and another ratio ⇒ still a swap…
    expect(
      paneSwapOf(tree, {
        kind: "split",
        id: "other",
        direction: "row",
        ratio: 0.5,
        children: [
          { kind: "leaf", id: "x", terminalId: "tb" },
          { kind: "leaf", id: "y", terminalId: "ta" },
        ],
      }),
    ).toEqual(["ta", "tb"]);
    // …but a row that became a column is a different tiling, not an exchange.
    expect(
      paneSwapOf(tree, {
        kind: "split",
        id: "s-root",
        direction: "column",
        ratio: 0.4,
        children: [
          { kind: "leaf", id: "p-a", terminalId: "tb" },
          { kind: "leaf", id: "p-b", terminalId: "ta" },
        ],
      }),
    ).toBeNull();
  });
});
