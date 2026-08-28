import { describe, expect, it } from "vitest";

import {
  blockSlotId,
  buildDragSlots,
  dragPreview,
  dropBand,
  dropIntent,
  flattenDragSlots,
  MERGE_BAND_RATIO,
  mergeTarget,
  projectDrop,
  ROOT_SLOT,
  slotGroupId,
  slotOf,
  type DragSlots,
} from "./group-drag";
import { groupListBlocks } from "@/components/panes/group-list-blocks";
import type { PaneGroup } from "@/components/panes/pane-groups";
import { createPaneLayout, splitPane, type PaneNode } from "@/components/panes/pane-tree";

/**
 * READING A DROP (review 01M0BPP79090KAQ9ZAFJ23ZRTX, critical) — the pure half of the four gestures.
 *
 * The list is a TWO-LEVEL shape: the bare rows and the blocks at the root, a block's members under
 * their block. Which of the four gestures happened is then a single question — where did the row
 * end up? — and that question is answered here, with no dnd-kit and no DOM in sight.
 */

function pair(a: string, b: string): PaneNode {
  return splitPane(createPaneLayout(a, `${a}-pane`), {
    paneId: `${a}-pane`,
    direction: "row",
    terminalId: b,
    newPaneId: `${b}-pane`,
  }) as PaneNode;
}

function group(id: string, a: string, b: string): PaneGroup {
  return { id, tree: pair(a, b), name: null, color: null };
}

/** g1 = [A, B] as a block, then C and D as bare rows. */
function slots(): DragSlots {
  const g1 = group("g1", "A", "B");
  const blocks = groupListBlocks(
    ["A", "B", "C", "D"].map((id) => ({ id })),
    (item) => item.id,
    (id) => (id === "A" || id === "B" ? g1 : null),
  );
  return buildDragSlots(blocks, (item) => item.id);
}

describe("The two-level list", () => {
  it("puts the bare rows AND the blocks at the root, and the members under their block", () => {
    expect(slots()).toEqual({
      [ROOT_SLOT]: ["block:g1", "C", "D"],
      "block:g1": ["A", "B"],
    });
  });

  it("flattens back to exactly what the list shows, top to bottom", () => {
    expect(flattenDragSlots(slots())).toEqual(["A", "B", "C", "D"]);
  });

  it("keeps a block's id out of the terminals' namespace", () => {
    expect(slotGroupId(blockSlotId("g1"))).toBe("g1");
    expect(slotGroupId("A"), "a terminal id is not a block slot").toBeNull();
    expect(slotOf(slots(), "A")).toBe("block:g1");
    expect(slotOf(slots(), "C")).toBe(ROOT_SLOT);
    expect(slotOf(slots(), "block:g1"), "a block is an ITEM of the root, not only a key").toBe(
      ROOT_SLOT,
    );
  });
});

describe("What a drop MEANS", () => {
  const before = slots();

  it("gesture 1 — same block, new index: the members re-order", () => {
    const after: DragSlots = { ...before, "block:g1": ["B", "A"] };
    expect(dropIntent(before, after, "B")).toEqual({
      kind: "reorder-members",
      groupId: "g1",
      memberIds: ["B", "A"],
      ids: ["B", "A", "C", "D"],
    });
  });

  it("gesture 2 — block → root: the row LEAVES", () => {
    const after: DragSlots = { [ROOT_SLOT]: ["block:g1", "B", "C", "D"], "block:g1": ["A"] };
    expect(dropIntent(before, after, "B")).toEqual({
      kind: "leave",
      terminalId: "B",
      ids: ["A", "B", "C", "D"],
    });
  });

  it("gesture 3 — root → block: the row ENTERS at the line's index", () => {
    const after: DragSlots = { [ROOT_SLOT]: ["block:g1", "D"], "block:g1": ["A", "C", "B"] };
    expect(dropIntent(before, after, "C")).toEqual({
      kind: "enter",
      groupId: "g1",
      terminalId: "C",
      index: 1,
      ids: ["A", "C", "B", "D"],
    });
  });

  it("gesture 4 — a BLOCK moved at the root: a pure list re-order, no group touched", () => {
    const after: DragSlots = { ...before, [ROOT_SLOT]: ["C", "D", "block:g1"] };
    expect(dropIntent(before, after, "block:g1")).toEqual({
      kind: "reorder-list",
      ids: ["C", "D", "A", "B"],
    });
  });

  it("a drop that moved nothing is NOTHING — no reorder, no membership edit", () => {
    expect(dropIntent(before, before, "B")).toEqual({ kind: "none" });
    expect(dropIntent(before, before, "C")).toEqual({ kind: "none" });
    expect(dropIntent(before, before, "block:g1")).toEqual({ kind: "none" });
    expect(dropIntent(before, before, "unknown")).toEqual({ kind: "none" });
  });

  /**
   * GESTURE 5 — the birth, and it NAMES BOTH MEMBERS (human review 01M1313GN2ESW0BCM8S1G0ZY2P).
   *
   * It used to be a dotted slot that carried one member and picked the other itself; the partner is
   * the line the row was dropped on now, so nothing is guessed and there is no case where a
   * completed gesture has no answer.
   */
  it("gesture 5 — the middle band of a root row: a folder of the two, both named", () => {
    // `after` is `move()`'s own projection — a plain re-order, which is all a sortable can express.
    // The band OVERRIDES it, and the order the birth produces is composed here, not taken from it.
    const after: DragSlots = { ...before, [ROOT_SLOT]: ["block:g1", "D", "C"] };
    expect(dropIntent(before, after, "C", "D")).toEqual({
      kind: "create",
      terminalId: "C",
      partnerId: "D",
      ids: ["A", "B", "D", "C"],
    });
  });

  it("…and the pair comes out CONTIGUOUS wherever the two rows were", () => {
    // C is dropped on a row that sits BELOW it; the block is drawn at its first member's position,
    // so the held row has to end up right after its partner rather than where it started.
    const wide: DragSlots = { [ROOT_SLOT]: ["C", "A", "D"] };
    expect(dropIntent(wide, wide, "C", "D")).toEqual({
      kind: "create",
      terminalId: "C",
      partnerId: "D",
      ids: ["A", "D", "C"],
    });
  });

  it("a MEMBER dropped on a root row founds the new group and leaves the old one to the model", () => {
    expect(dropIntent(before, before, "A", "C")).toEqual({
      kind: "create",
      terminalId: "A",
      partnerId: "C",
      ids: ["B", "C", "A", "D"],
    });
  });

  it("a BLOCK never merges — dropping a group on a row is still gesture 4", () => {
    const after: DragSlots = { ...before, [ROOT_SLOT]: ["C", "block:g1", "D"] };
    expect(dropIntent(before, after, "block:g1", "C")).toEqual({
      kind: "reorder-list",
      ids: ["C", "A", "B", "D"],
    });
  });
});

/**
 * THE THREE BANDS OF A ROOT LINE (human review 01M1313GN2ESW0BCM8S1G0ZY2P).
 *
 * The geometry is the caller's — only the DOM knows where in a line a pointer sits — but WHICH band
 * a reading falls in, and whether a merge is possible at all, are decided here so the during-drag
 * ring and the commit cannot disagree.
 */
describe("The middle band founds a folder; the edges insert", () => {
  const before = slots();

  it("splits a line into before / merge / after, and the middle is what is named", () => {
    const edge = (1 - MERGE_BAND_RATIO) / 2;
    expect(dropBand(0)).toBe("before");
    expect(dropBand(edge - 0.01)).toBe("before");
    expect(dropBand(0.5), "dead centre").toBe("merge");
    expect(dropBand(edge + 0.01)).toBe("merge");
    expect(dropBand(1 - edge - 0.01)).toBe("merge");
    expect(dropBand(1 - edge + 0.01)).toBe("after");
    expect(dropBand(1)).toBe("after");
  });

  it("names the ROOT ROW under the pointer — and refuses every other kind of target", () => {
    expect(mergeTarget(before, "C", "D", 0.5), "two bare rows").toBe("D");
    expect(mergeTarget(before, "C", "D", 0.05), "…on an edge band, no merge").toBeNull();
    expect(mergeTarget(before, "C", "C", 0.5), "a line never merges with itself").toBeNull();
    expect(
      mergeTarget(before, "C", "block:g1", 0.5),
      "a BLOCK takes a drop INSIDE it — gesture 3, and it has its own indicator",
    ).toBeNull();
    expect(
      mergeTarget(before, "C", "A", 0.5),
      "a MEMBER row means 'enter that block', the older and more specific reading",
    ).toBeNull();
    expect(
      mergeTarget(before, "block:g1", "C", 0.5),
      "a block dropped on a row is gesture 4 — the group travels whole",
    ).toBeNull();
  });

  it("lets a MEMBER be the one dropped — it leaves its group and founds a new one", () => {
    expect(mergeTarget(before, "A", "C", 0.5)).toBe("C");
  });
});

describe("What the drag SHOWS", () => {
  const before = slots();

  it("names the block the drop lands IN, and none when it lands in the bare list", () => {
    const inside: DragSlots = { ...before, "block:g1": ["B", "A"] };
    expect(dragPreview(before, inside, "B")?.groupId).toBe("g1");

    const outside: DragSlots = { [ROOT_SLOT]: ["block:g1", "B", "C", "D"], "block:g1": ["A"] };
    expect(dragPreview(before, outside, "B")?.groupId).toBeNull();
  });

  it("says WHERE the line sits — before the next row, or at the end of its slot", () => {
    const middle: DragSlots = { [ROOT_SLOT]: ["block:g1", "D"], "block:g1": ["A", "C", "B"] };
    expect(dragPreview(before, middle, "C")).toMatchObject({
      parent: "block:g1",
      index: 1,
      beforeId: "B",
    });
    const last: DragSlots = { [ROOT_SLOT]: ["block:g1", "D"], "block:g1": ["A", "B", "C"] };
    expect(dragPreview(before, last, "C")).toMatchObject({ index: 2, beforeId: null });
  });

  it("announces the DISSOLUTION of a block down to its last pair — and only then", () => {
    const out: DragSlots = { [ROOT_SLOT]: ["block:g1", "B", "C", "D"], "block:g1": ["A"] };
    expect(dragPreview(before, out, "B")?.dissolvingGroupId, "two members ⇒ it dies").toBe("g1");

    // The same gesture on a block of THREE announces nothing: it survives the departure.
    const three: DragSlots = { [ROOT_SLOT]: ["block:g1", "D"], "block:g1": ["A", "B", "C"] };
    const survives: DragSlots = { [ROOT_SLOT]: ["block:g1", "C", "D"], "block:g1": ["A", "B"] };
    expect(dragPreview(three, survives, "C")?.dissolvingGroupId).toBeNull();

    // …and a row staying INSIDE its block never announces one.
    const shuffled: DragSlots = { ...before, "block:g1": ["B", "A"] };
    expect(dragPreview(before, shuffled, "B")?.dissolvingGroupId).toBeNull();
  });
});

/**
 * A FOLDED BLOCK IS STILL A PARENT (human review 01M12MW1DV1V455EWBMV048TE6).
 *
 * `@dnd-kit/helpers`' `move` finds a drop target's parent by looking the target id up among the
 * slots' CHILDREN. A block is a child of the ROOT list AND a key of its own members, and the child
 * lookup wins — so a drop resolved to the BLOCK has always meant "beside it", and entering a group
 * has always gone through a member ROW under the pointer. Folding takes those rows away.
 *
 * {@link projectDrop} is where that is repaired, and this is its pure half: no dnd-kit, no DOM, just
 * the record in and the record out. The browser walk in `terminals/group-drag.browser.test.tsx`
 * proves the same thing through a real pointer.
 */
describe("Projecting a drop onto a FOLDED block", () => {
  const folded: DragSlots = {
    [ROOT_SLOT]: [blockSlotId("g1"), "D", "E"],
    [blockSlotId("g1")]: ["A", "B"],
  };
  /** A drop of `source` onto `target`, in the shape `move`/`projectDrop` read. */
  const drop = (source: string, target: string) =>
    ({
      operation: { source: { id: source }, target: { id: target } },
      // `move` needs a manager to fall back on; it is never reached on the folded path.
    }) as unknown as Parameters<typeof projectDrop>[1];

  it("puts the dragged ROW inside the folded block, and takes it out of the list", () => {
    const after = projectDrop(folded, drop("D", blockSlotId("g1")), () => true);
    expect(after[blockSlotId("g1")], "it joined the group, at the end").toEqual(["A", "B", "D"]);
    expect(after[ROOT_SLOT], "and left the bare list").toEqual([blockSlotId("g1"), "E"]);
    // …and the whole point: the drop READS as an ENTRY, through the same `dropIntent` as ever.
    const intent = dropIntent(folded, after, "D");
    expect(intent.kind).toBe("enter");
    expect(intent.kind === "enter" && intent.groupId).toBe("g1");
  });

  it("does NOT nest a block inside another block — a block dropped on a block is gesture 4", () => {
    const twoBlocks: DragSlots = {
      [ROOT_SLOT]: [blockSlotId("g1"), blockSlotId("g2")],
      [blockSlotId("g1")]: ["A", "B"],
      [blockSlotId("g2")]: ["E", "F"],
    };
    const after = projectDrop(twoBlocks, drop(blockSlotId("g1"), blockSlotId("g2")), () => true);
    expect(
      after[blockSlotId("g2")],
      "a group travels whole; it never becomes a member of another",
    ).toEqual(["E", "F"]);
  });

  it("leaves an OPEN block to `move()` — the member rows are there to resolve it", () => {
    // With `isFolded` false the projection is `move`'s, untouched. `move` needs a live drag manager,
    // so the assertion is that this path is NOT the folded one: the members are unchanged.
    const after = projectDrop(folded, drop("D", blockSlotId("g1")), () => false);
    expect(after[blockSlotId("g1")], "no folded shortcut was taken").not.toContain("D");
  });

  it("is a no-op with no fold state at all — a surface that does not fold is untouched", () => {
    const after = projectDrop(folded, drop("D", blockSlotId("g1")));
    expect(after[blockSlotId("g1")]).not.toContain("D");
  });
});
