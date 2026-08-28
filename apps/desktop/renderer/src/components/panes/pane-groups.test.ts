import { describe, expect, it } from "vitest";

import {
  autoLayoutTree,
  closeGroupPane,
  createGroupFromSplit,
  detachTerminal,
  dissolveGroup,
  EMPTY_COLLECTION,
  findGroup,
  groupInsertAnchor,
  groupMemberIds,
  groupOfTerminal,
  insertIntoGroup,
  isGroupColor,
  isPaneCollection,
  nextAppendDirection,
  orderGroupMembers,
  pruneCollection,
  setGroupColor,
  setGroupName,
  splitInGroup,
  type PaneCollection,
} from "./pane-groups";
import { paneRects } from "./pane-geometry";
import {
  createPaneLayout,
  paneCount,
  paneTerminalIds,
  splitPane,
  type PaneNode,
} from "./pane-tree";

/** A two-member group tree (`t1` | `t2`). */
function pair(a = "t1", b = "t2", prefix = "p"): PaneNode {
  return splitPane(createPaneLayout(a, `${prefix}-${a}`), {
    paneId: `${prefix}-${a}`,
    direction: "row",
    terminalId: b,
    newPaneId: `${prefix}-${b}`,
  }) as PaneNode;
}

/** A collection holding ONE group made of `ids` (auto-laid-out, deterministic ids). */
function groupOf(ids: readonly string[], id = "g1"): PaneCollection {
  let n = 0;
  const tree = autoLayoutTree(ids, (prefix) => `${prefix}-${id}-${n++}`) as PaneNode;
  return { groups: [{ id, tree, name: null, color: null }] };
}

/**
 * JOIN at the AUTOMATIC POSITION — which is `insertIntoGroup` at the END index, and nothing else
 * (review 01M0BSNTEN5N2GC9QQBASJYHHB). The model used to carry a second function for it,
 * `joinGroup`, written for a sidebar menu entry that no longer exists; at `index = members.length`
 * `groupInsertAnchor` returns the LAST leaf and `nextAppendDirection`, i.e. exactly the split that
 * function performed, and the member re-order is the identity. The tests that pinned the automatic
 * position now pin it on the path the app really takes.
 */
function appendToGroup(collection: PaneCollection, groupId: string, terminalId: string) {
  const group = findGroup(collection, groupId);
  const end = group == null ? 0 : groupMemberIds(group).length;
  return insertIntoGroup(collection, groupId, terminalId, end);
}

/**
 * A GROUP IS BORN FROM A SPLIT, NEVER DECLARED. These are the transitions the model must keep
 * consistent whatever the order they are applied in.
 */
describe("pane-groups — one terminal, at most one group", () => {
  it("holds the invariant across creation, split, leave, join, close and prune", () => {
    // A split from a solo terminal creates the group of two.
    const born = createGroupFromSplit(EMPTY_COLLECTION, {
      terminalId: "t1",
      newTerminalId: "t2",
      direction: "row",
      groupId: "g1",
    });
    expect(born).not.toBeNull();
    let collection = born!.collection;
    expect(groupMemberIds(collection.groups[0])).toEqual(["t1", "t2"]);

    // A split from a MEMBER adds a leaf to that group — it does not create a second one.
    const grown = splitInGroup(collection, {
      groupId: "g1",
      paneId: born!.paneId,
      direction: "column",
      terminalId: "t3",
    });
    collection = grown!.collection;
    expect(collection.groups).toHaveLength(1);
    expect(groupMemberIds(collection.groups[0]).sort()).toEqual(["t1", "t2", "t3"]);

    // A SECOND group, from another solo pair.
    collection = createGroupFromSplit(collection, {
      terminalId: "t8",
      newTerminalId: "t9",
      direction: "row",
      groupId: "g2",
    })!.collection;

    // Joining moves a terminal — it never duplicates it.
    collection = appendToGroup(collection, "g2", "t3")!.collection;
    expect(groupOfTerminal(collection, "t3")?.id).toBe("g2");
    expect(groupMemberIds(findGroup(collection, "g1")!)).toEqual(["t1", "t2"]);

    // Every membership check, at every step, sees each terminal in AT MOST one group.
    const seen = collection.groups.flatMap(groupMemberIds);
    expect(new Set(seen).size).toBe(seen.length);

    // Leaving, closing a pane and pruning all keep it.
    collection = detachTerminal(collection, "t9");
    collection = closeGroupPane(
      collection,
      "g1",
      paneRects(findGroup(collection, "g1")!.tree)[0].paneId,
    );
    collection = pruneCollection(collection, ["t1", "t2", "t3", "t8", "t9"]);
    const after = collection.groups.flatMap(groupMemberIds);
    expect(new Set(after).size).toBe(after.length);
  });

  it("REFUSES to split a terminal that is already a member (that is splitInGroup's job)", () => {
    const collection = groupOf(["t1", "t2"]);
    expect(
      createGroupFromSplit(collection, {
        terminalId: "t1",
        newTerminalId: "t3",
        direction: "row",
      }),
    ).toBeNull();
  });

  it("REFUSES to place a terminal that already sits somewhere", () => {
    const collection: PaneCollection = {
      groups: [
        { id: "g1", tree: pair("t1", "t2"), name: null, color: null },
        { id: "g2", tree: pair("t3", "t4", "q"), name: null, color: null },
      ],
    };
    expect(
      splitInGroup(collection, {
        groupId: "g1",
        paneId: "p-t1",
        direction: "row",
        terminalId: "t3",
      }),
    ).toBeNull();
    // Joining the group it is ALREADY in is a no-op, not a duplication.
    expect(appendToGroup(collection, "g1", "t1")).toBeNull();
  });
});

describe("pane-groups — dissolution", () => {
  it("leaves the last member SOLO when the second-to-last is removed", () => {
    const collection = groupOf(["t1", "t2"]);
    const after = detachTerminal(collection, "t1");
    expect(after.groups).toEqual([]);
    expect(groupOfTerminal(after, "t2")).toBeNull();
    expect(groupOfTerminal(after, "t1")).toBeNull();
  });

  it("keeps the group when it still has two members after the removal", () => {
    const collection = groupOf(["t1", "t2", "t3"]);
    const after = detachTerminal(collection, "t2");
    expect(after.groups).toHaveLength(1);
    expect(groupMemberIds(after.groups[0])).toEqual(["t1", "t3"]);
  });

  it("closing a pane of a PAIR dissolves the group and leaves both terminals solo", () => {
    const collection = groupOf(["t1", "t2"]);
    const paneId = paneRects(collection.groups[0].tree)[0].paneId;
    const after = closeGroupPane(collection, "g1", paneId);
    expect(after.groups).toEqual([]);
  });

  it("'Dégrouper tout' makes every member solo at once", () => {
    const collection = groupOf(["t1", "t2", "t3", "t4"]);
    const after = dissolveGroup(collection, "g1");
    expect(after.groups).toEqual([]);
    for (const id of ["t1", "t2", "t3", "t4"]) expect(groupOfTerminal(after, id)).toBeNull();
    // An unknown id changes nothing (and does not throw).
    expect(dissolveGroup(collection, "nope")).toBe(collection);
  });

  it("PRUNES a terminal that really disappeared, compacting and dissolving as needed", () => {
    const collection: PaneCollection = {
      groups: [
        { id: "g1", tree: groupOf(["t1", "t2", "t3"], "g1").groups[0].tree, name: "a", color: 3 },
        { id: "g2", tree: pair("t8", "t9", "q"), name: null, color: null },
      ],
    };
    const after = pruneCollection(collection, ["t1", "t3", "t8"]);
    // g1 lost t2 and compacted; g2 fell to one member and dissolved.
    expect(after.groups.map((g) => g.id)).toEqual(["g1"]);
    expect(groupMemberIds(after.groups[0])).toEqual(["t1", "t3"]);
    // The labels survive a prune — only the tree changed.
    expect([after.groups[0].name, after.groups[0].color]).toEqual(["a", 3]);
    // Nothing to prune ⇒ the SAME object back (the reconcile runs on every snapshot push).
    expect(pruneCollection(after, ["t1", "t3"])).toBe(after);
  });
});

describe("pane-groups — name and colour are optional and independent", () => {
  it("is born grey and nameless, and stays usable that way", () => {
    const born = createGroupFromSplit(EMPTY_COLLECTION, {
      terminalId: "t1",
      newTerminalId: "t2",
      direction: "row",
    })!;
    expect(born.group.name).toBeNull();
    expect(born.group.color).toBeNull();
    expect(paneCount(born.group.tree)).toBe(2);
  });

  it("names without colouring, colours without naming, and neither erases the other", () => {
    let collection = groupOf(["t1", "t2"]);
    collection = setGroupName(collection, "g1", "build");
    expect([collection.groups[0].name, collection.groups[0].color]).toEqual(["build", null]);
    collection = setGroupColor(collection, "g1", 4);
    expect([collection.groups[0].name, collection.groups[0].color]).toEqual(["build", 4]);
  });

  it("clearing either one leaves the GROUP alive with its members", () => {
    let collection = setGroupColor(setGroupName(groupOf(["t1", "t2"]), "g1", "build"), "g1", 4);
    collection = setGroupName(collection, "g1", "");
    expect(collection.groups[0].name).toBeNull();
    expect(collection.groups).toHaveLength(1);
    collection = setGroupColor(collection, "g1", null);
    expect(collection.groups[0].color).toBeNull();
    expect(groupMemberIds(collection.groups[0])).toEqual(["t1", "t2"]);
  });

  it("trims a name and treats blank as absent", () => {
    const named = setGroupName(groupOf(["t1", "t2"]), "g1", "  build  ");
    expect(named.groups[0].name).toBe("build");
    expect(setGroupName(named, "g1", "   ").groups[0].name).toBeNull();
  });

  it("only accepts the SIX group colours", () => {
    expect([1, 2, 3, 4, 5, 6].every(isGroupColor)).toBe(true);
    for (const bad of [0, 7, -1, 1.5, "3", null, undefined]) expect(isGroupColor(bad)).toBe(false);
  });
});

describe("pane-groups — the automatic disposition", () => {
  /** Deterministic ids so the rectangles can be pinned. */
  const layout = (ids: readonly string[]) => {
    let n = 0;
    return autoLayoutTree(ids, (prefix) => `${prefix}-${n++}`) as PaneNode;
  };
  const boxes = (tree: PaneNode) =>
    paneRects(tree).map((p) => [
      p.terminalId,
      p.rect.left,
      p.rect.top,
      p.rect.width,
      p.rect.height,
    ]);

  it("2 — side by side", () => {
    expect(boxes(layout(["a", "b"]))).toEqual([
      ["a", 0, 0, 50, 100],
      ["b", 50, 0, 50, 100],
    ]);
  });

  it("3 — the third passes UNDERNEATH the second, the first keeps the full-height column", () => {
    expect(boxes(layout(["a", "b", "c"]))).toEqual([
      ["a", 0, 0, 50, 100],
      ["b", 50, 0, 50, 50],
      ["c", 50, 50, 50, 50],
    ]);
  });

  it("4 — a quadrant, filled row-major", () => {
    expect(boxes(layout(["a", "b", "c", "d"]))).toEqual([
      ["a", 0, 0, 50, 50],
      ["c", 0, 50, 50, 50],
      ["b", 50, 0, 50, 50],
      ["d", 50, 50, 50, 50],
    ]);
  });

  /**
   * A COLUMN OF THREE IS THIRDS, NOT 50/25/25 (review 01M11WE6H9E5D5FCS6RT0ZXAXN, finding
   * 01M11XTRXVVPZ9XMDW5ARWKWHA). The chain used to nest at a flat 0.5, so the first member of a
   * column took half the box and every following one was squeezed into the remainder — visibly
   * lopsided from five members on, and worse the longer the column. A latent defect, found while
   * designing the shapes and fixed on its own.
   */
  it("5 — the column of three is EQUAL BANDS, not a half and two quarters", () => {
    const five = boxes(layout(["a", "b", "c", "d", "e"]));
    const heightOf = (id: string) => five.find((b) => b[0] === id)![4] as number;
    const topOf = (id: string) => five.find((b) => b[0] === id)![2] as number;

    // Left column: a, c, e — three EQUAL thirds, stacked with no gap and no overlap.
    for (const id of ["a", "c", "e"]) expect(heightOf(id)).toBeCloseTo(100 / 3, 10);
    expect(topOf("a")).toBeCloseTo(0, 10);
    expect(topOf("c")).toBeCloseTo(100 / 3, 10);
    expect(topOf("e")).toBeCloseTo(200 / 3, 10);
    expect(topOf("e") + heightOf("e")).toBeCloseTo(100, 10);
    // The 50/25/25 of before, named so the regression cannot come back unnoticed.
    expect(heightOf("a")).not.toBeCloseTo(50, 6);

    // Right column: b, d — two members still cut at the half.
    expect(heightOf("b")).toBeCloseTo(50, 10);
    expect(heightOf("d")).toBeCloseTo(50, 10);
  });

  it("6 — a column of three on each side, all six bands equal", () => {
    const six = boxes(layout(["a", "b", "c", "d", "e", "f"]));
    for (const box of six) expect(box[4] as number).toBeCloseTo(100 / 3, 10);
    // …and the two columns still tile the width exactly.
    expect(new Set(six.map((b) => b[3]))).toEqual(new Set([50]));
  });

  it("REORDERING the members changes the rectangles — that is what 'réglable' means", () => {
    const before = boxes(layout(["a", "b", "c", "d"]));
    const after = boxes(layout(["b", "a", "c", "d"]));
    expect(after).not.toEqual(before);
    // `b` now holds the top-left box `a` used to hold.
    expect(after[0]).toEqual(["b", 0, 0, 50, 50]);
  });

  it("fewer than two members is not a group", () => {
    expect(autoLayoutTree([])).toBeNull();
    expect(autoLayoutTree(["a"])).toBeNull();
  });

  it("APPENDS right, then down — the same order a pointer split produces", () => {
    // The automatic position is reached through the END index of `insertIntoGroup` and through
    // nothing else: `groupInsertAnchor` hands it the LAST leaf and `nextAppendDirection`, which is
    // the pointer split's own order (one leaf at a time, right then down).
    expect(nextAppendDirection(createPaneLayout("a", "pa")), "the FIRST append goes right").toBe(
      "row",
    );
    const two = groupOf(["a", "b"]);
    expect(boxes(two.groups[0].tree)).toEqual([
      ["a", 0, 0, 50, 100],
      ["b", 50, 0, 50, 100],
    ]);
    expect(nextAppendDirection(two.groups[0].tree), "…the SECOND goes down").toBe("column");
    const three = appendToGroup(two, "g1", "c")!;
    expect(boxes(three.collection.groups[0].tree)).toEqual([
      ["a", 0, 0, 50, 100],
      ["b", 50, 0, 50, 50],
      ["c", 50, 50, 50, 50],
    ]);
  });

  it("a terminal JOINING a group lands at that automatic position", () => {
    const collection = groupOf(["t1", "t2"]);
    const joined = appendToGroup(collection, "g1", "t3")!;
    expect(paneTerminalIds(joined.collection.groups[0].tree)).toEqual(["t1", "t2", "t3"]);
  });
});

describe("pane-groups — the untrusted-shape guard", () => {
  it("accepts what the mutators produce", () => {
    expect(isPaneCollection(EMPTY_COLLECTION)).toBe(true);
    expect(isPaneCollection(groupOf(["t1", "t2", "t3"]))).toBe(true);
  });

  it("rejects a non-collection, a duplicate group id, a one-member group and a shared terminal", () => {
    expect(isPaneCollection(null)).toBe(false);
    expect(isPaneCollection({})).toBe(false);
    expect(isPaneCollection({ groups: "no" })).toBe(false);
    const tree = pair();
    expect(
      isPaneCollection({
        groups: [
          { id: "g1", tree, name: null, color: null },
          { id: "g1", tree: pair("t5", "t6", "q"), name: null, color: null },
        ],
      }),
    ).toBe(false);
    expect(
      isPaneCollection({
        groups: [{ id: "g1", tree: createPaneLayout("t1", "p1"), name: null, color: null }],
      }),
    ).toBe(false);
    expect(
      isPaneCollection({
        groups: [
          { id: "g1", tree, name: null, color: null },
          { id: "g2", tree: pair("t1", "t7", "q"), name: null, color: null },
        ],
      }),
    ).toBe(false);
  });
});

/**
 * WHAT THE DRAG NEEDS OF THE MODEL (review 01M0BPP79090KAQ9ZAFJ23ZRTX, critical). Two mutators, and
 * one anchor rule between them: re-ordering a group's members must not rebuild the tiling the user
 * has already sized, and entering a group must take the SPLIT's path, at the position of the line.
 */
describe("orderGroupMembers", () => {
  /** A three-member group: `t1` beside (`t2` over `t3`). */
  function trio(): PaneCollection {
    const tree = splitPane(pair(), {
      paneId: "p-t2",
      direction: "column",
      terminalId: "t3",
      newPaneId: "p-t3",
    }) as PaneNode;
    return { groups: [{ id: "g1", tree, name: null, color: null }] };
  }

  it("re-orders the PANES without rebuilding the tree — same rectangles, new occupants", () => {
    const before = trio();
    const rectsBefore = paneRects(before.groups[0].tree).map((p) => p.rect);
    const after = orderGroupMembers(before, "g1", ["t3", "t1", "t2"]);

    expect(groupMemberIds(after.groups[0])).toEqual(["t3", "t1", "t2"]);
    // The geometry is IDENTICAL: nothing was re-split, no ratio was reset.
    expect(paneRects(after.groups[0].tree).map((p) => p.rect)).toEqual(rectsBefore);
    expect(paneCount(after.groups[0].tree)).toBe(3);
    // Membership is untouched — the same three terminals, still one group.
    expect(after.groups).toHaveLength(1);
  });

  it("is a no-op — by REFERENCE — for the order it already has, or for a non-permutation", () => {
    const before = trio();
    expect(orderGroupMembers(before, "g1", ["t1", "t2", "t3"])).toBe(before);
    expect(orderGroupMembers(before, "g1", ["t1", "t2"]), "wrong length").toBe(before);
    expect(orderGroupMembers(before, "g1", ["t1", "t2", "zz"]), "foreign id").toBe(before);
    expect(orderGroupMembers(before, "unknown", ["t3", "t1", "t2"])).toBe(before);
  });
});

describe("insertIntoGroup", () => {
  const base = (): PaneCollection => ({
    groups: [{ id: "g1", tree: pair(), name: null, color: null }],
  });

  it("lands the newcomer AT the index, and the tree gains a pane there", () => {
    const inserted = insertIntoGroup(base(), "g1", "t9", 1);
    expect(inserted).not.toBeNull();
    const group = findGroup(inserted!.collection, "g1")!;
    expect(groupMemberIds(group)).toEqual(["t1", "t9", "t2"]);
    expect(paneCount(group.tree), "one more pane, not a rebuilt tree").toBe(3);
    // The pane really is the newcomer's own, and the caller gets its id back to focus it.
    expect(paneTerminalIds(group.tree)).toContain("t9");
    expect(inserted!.paneId).toBeTruthy();
  });

  it("accepts the TOP of the block — index 0 puts the newcomer first", () => {
    const inserted = insertIntoGroup(base(), "g1", "t9", 0);
    expect(groupMemberIds(findGroup(inserted!.collection, "g1")!)).toEqual(["t9", "t1", "t2"]);
  });

  it("accepts the BOTTOM — an index past the last member appends", () => {
    const inserted = insertIntoGroup(base(), "g1", "t9", 9);
    expect(groupMemberIds(findGroup(inserted!.collection, "g1")!)).toEqual(["t1", "t2", "t9"]);
  });

  it("detaches from the PREVIOUS group first — one terminal is never in two", () => {
    const two: PaneCollection = {
      groups: [
        { id: "g1", tree: pair(), name: null, color: null },
        { id: "g2", tree: pair("t3", "t4", "q"), name: null, color: null },
      ],
    };
    const moved = insertIntoGroup(two, "g1", "t3", 0)!;
    expect(groupMemberIds(findGroup(moved.collection, "g1")!)).toEqual(["t3", "t1", "t2"]);
    // g2 was a PAIR: losing `t3` dissolves it and leaves `t4` solo.
    expect(findGroup(moved.collection, "g2")).toBeNull();
    expect(groupOfTerminal(moved.collection, "t4")).toBeNull();
  });

  it("refuses the impossible: an unknown group, or a terminal already in that very group", () => {
    expect(insertIntoGroup(base(), "nope", "t9", 0)).toBeNull();
    expect(insertIntoGroup(base(), "g1", "t1", 0), "already a member").toBeNull();
  });

  it("names the SAME anchor the floor is asked about — the split's path, not a second one", () => {
    // `groupInsertAnchor` is what `canSplitPane` is run on before the insertion, so the pane the
    // check measures is the pane the insertion really halves.
    const group = base().groups[0];
    const anchor = groupInsertAnchor(group, 1);
    expect(anchor.direction).toBe(nextAppendDirection(group.tree));
    const split = splitPane(group.tree, {
      paneId: anchor.paneId,
      direction: anchor.direction,
      terminalId: "t9",
      newPaneId: "p-t9",
    });
    expect(split, "the anchor is a real, splittable pane of THIS tree").not.toBeNull();
    expect(paneTerminalIds(split as PaneNode)).toEqual(
      paneTerminalIds(insertIntoGroup(base(), "g1", "t9", 1)!.collection.groups[0].tree),
    );
  });
});
