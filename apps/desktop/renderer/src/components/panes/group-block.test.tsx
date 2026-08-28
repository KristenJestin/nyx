import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { GroupBlock } from "./group-block";
import { groupListBlocks } from "./group-list-blocks";
import type { PaneGroup } from "./pane-groups";
import { createPaneLayout, splitPane, type PaneNode } from "./pane-tree";

function tree(a: string, b: string): PaneNode {
  return splitPane(createPaneLayout(a, `p-${a}`), {
    paneId: `p-${a}`,
    direction: "row",
    terminalId: b,
    newPaneId: `p-${b}`,
  }) as PaneNode;
}

function group(id: string, members: [string, string], extra: Partial<PaneGroup> = {}): PaneGroup {
  return { id, tree: tree(members[0], members[1]), name: null, color: null, ...extra };
}

/**
 * ONE LIST, WITH BLOCKS IN IT (review 01M0ABR6V501D0SPR2BYBVZC4A, major). A terminal joining a
 * group must not change SECTION — only appearance — so the partition below is the only re-ordering
 * that happens, and it is the minimum one: a group takes the position of its first member.
 */
describe("groupListBlocks", () => {
  const idOf = (item: { id: string }) => item.id;
  const items = ["a", "b", "c", "d", "e"].map((id) => ({ id }));

  it("keeps every row in ONE list — a solo row is a solo row, in place", () => {
    const blocks = groupListBlocks(items, idOf, () => null);
    expect(blocks.map((b) => (b.kind === "solo" ? b.item.id : "?"))).toEqual([
      "a",
      "b",
      "c",
      "d",
      "e",
    ]);
  });

  it("pulls a group's SCATTERED members contiguous, at the position of the FIRST of them", () => {
    const g = group("g1", ["a", "d"]);
    const blocks = groupListBlocks(items, idOf, (id) => (id === "a" || id === "d" ? g : null));
    expect(
      blocks.map((b) => (b.kind === "solo" ? b.item.id : b.items.map((i) => i.id).join("+"))),
    ).toEqual(["a+d", "b", "c", "e"]);
    // …and the rest of the list keeps its relative order.
    const solos = blocks.filter((b) => b.kind === "solo");
    expect(solos.map((b) => (b as { item: { id: string } }).item.id)).toEqual(["b", "c", "e"]);
  });

  it("never lists a row twice, and never loses one", () => {
    const g1 = group("g1", ["b", "e"]);
    const g2 = group("g2", ["c", "d"]);
    const of = (id: string) =>
      id === "b" || id === "e" ? g1 : id === "c" || id === "d" ? g2 : null;
    const blocks = groupListBlocks(items, idOf, of);
    const flat = blocks.flatMap((b) => (b.kind === "solo" ? [b.item.id] : b.items.map(idOf)));
    expect(flat.sort()).toEqual(["a", "b", "c", "d", "e"]);
    expect(new Set(flat).size).toBe(flat.length);
  });

  it("emits a block per group, in first-member order", () => {
    const g1 = group("g1", ["d", "e"]);
    const g2 = group("g2", ["b", "c"]);
    const of = (id: string) =>
      id === "d" || id === "e" ? g1 : id === "b" || id === "c" ? g2 : null;
    const blocks = groupListBlocks(items, idOf, of);
    expect(
      blocks.filter((b) => b.kind === "group").map((b) => (b as { group: PaneGroup }).group.id),
    ).toEqual(["g2", "g1"]);
  });

  it("keeps a block inside ONE list — a member living elsewhere is not pulled across", () => {
    const g = group("g1", ["a", "b"]);
    // `zz` is not in this list at all (it is another workspace's row); `a` and `b` are.
    const blocks = groupListBlocks(items, idOf, (id) =>
      id === "a" || id === "b" || id === "zz" ? g : null,
    );
    const groupBlock = blocks.find((b) => b.kind === "group") as {
      items: readonly { id: string }[];
    };
    expect(groupBlock.items.map(idOf)).toEqual(["a", "b"]);
  });

  /**
   * A GROUP OF ONE IS NOT A BLOCK (review 01M0ASVP67FM5J7AEJ85N5T2GN). The list is rendered per
   * WORKSPACE and "Join <group>" carries no workspace condition, so a group really can leave a
   * single member in a section — and the block drawn around it said "this terminal is alone in a
   * frame", the one shape the rule reserves for the bare row.
   */
  it("draws NO block around a lone member whose group lives mostly in another list", () => {
    const g = group("g1", ["a", "zz"]);
    const blocks = groupListBlocks(items, idOf, (id) => (id === "a" || id === "zz" ? g : null));
    expect(blocks.some((b) => b.kind === "group")).toBe(false);
    // …and the row is still THERE, in its place, as a bare row — nothing is dropped.
    expect(blocks.map((b) => (b.kind === "solo" ? idOf(b.item) : "BLOCK"))).toEqual(
      items.map(idOf),
    );
  });

  it("still draws the block in the section that DOES hold two members", () => {
    const g = group("g1", ["a", "c"]);
    const blocks = groupListBlocks(items, idOf, (id) =>
      id === "a" || id === "c" || id === "zz" ? g : null,
    );
    const groupBlock = blocks.find((b) => b.kind === "group") as {
      items: readonly { id: string }[];
    };
    expect(groupBlock, "two members here ⇒ a block here").toBeTruthy();
    expect(groupBlock.items.map(idOf)).toEqual(["a", "c"]);
  });
});

describe("GroupBlock", () => {
  /**
   * THE COST IS ASSUMED (review 01M0BPP79090KAQ9ZAFJ23ZRTX). The head used to be the NAME and its
   * absence rendered nothing at all — no placeholder, no reserved height. It is now always there,
   * because the group needs a permanent object to be grabbed and settled by, and the user chose
   * that option against the thickened border (no height, but a 4px target) and the head on hover
   * (density kept, but the list moves under the pointer).
   */
  it("ALWAYS opens on a head — an unnamed group has one too", () => {
    const { container } = render(
      <GroupBlock groupId="g1" color={null} name={null}>
        <span>row</span>
      </GroupBlock>,
    );
    const block = container.querySelector("[data-group-block]") as HTMLElement;
    expect(block.firstElementChild?.getAttribute("data-group-head")).toBe("g1");
    expect(block.textContent).toContain("row");
  });

  it("treats a blank name as NO name — the head shows nothing, not spaces", () => {
    const { container } = render(
      <GroupBlock groupId="g1" color={null} name="   ">
        <span>row</span>
      </GroupBlock>,
    );
    const head = container.querySelector("[data-group-head]") as HTMLElement;
    expect(head.dataset.groupHeadLabel).toBe("unnamed");
    expect(head.textContent, "no name → no words (review 01M0CY3F075ACP4TFBK1KJ49X8)").toBe("");
  });

  it("puts the name INSIDE the block, at its head", () => {
    const { container } = render(
      <GroupBlock groupId="g1" color={4} name="build">
        <span>row</span>
      </GroupBlock>,
    );
    const block = container.querySelector("[data-group-block]") as HTMLElement;
    const head = screen.getByText("build");
    expect(block.contains(head)).toBe(true);
    expect(block.firstElementChild?.contains(head)).toBe(true);
  });

  it("carries the group's colour through ONE custom property, and the neutral by default", () => {
    const { container, rerender } = render(
      <GroupBlock groupId="g1" color={null} name={null}>
        <span>row</span>
      </GroupBlock>,
    );
    const read = () => container.querySelector("[data-group-block]") as HTMLElement;
    expect(read().dataset.groupColor).toBe("none");
    expect(read().getAttribute("style")).toContain("var(--group-0)");
    rerender(
      <GroupBlock groupId="g1" color={3} name={null}>
        <span>row</span>
      </GroupBlock>,
    );
    expect(read().dataset.groupColor).toBe("3");
    expect(read().getAttribute("style")).toContain("var(--group-3)");
  });

  /**
   * THE FOLDER IS DRAWN ON EVERY BLOCK, AND IT SAYS WHICH GESTURES IT OFFERS (human review
   * 01M12MW1DV1V455EWBMV048TE6, superseding review 01M0BSNTEN5N2GC9QQBASJYHHB's "no grip where
   * nothing drags").
   *
   * The grip could only ever be a handle, so on a block that does not travel it was a promise
   * nothing kept and it had to be removed entirely — which left the command heads with no glyph at
   * all. A FOLDER is not a promise of anything: it is the node itself, it is what folds, and it is
   * what a group without a name still has. So it is drawn on both families.
   *
   * What DOES vary is what it offers, and that is asserted on the two things a user reads: the
   * accessible name (which states the gestures) and the cursor (grab where the block travels,
   * pointer where it only folds).
   */
  it("draws a FOLDER on every block — a handle where the block travels, a toggle where it does not", () => {
    const { container, rerender } = render(
      <GroupBlock groupId="g1" color={null} name="build" open onToggle={() => {}}>
        <span>row</span>
      </GroupBlock>,
    );
    const folderOf = () => container.querySelector("[data-group-folder='g1']") as HTMLElement;

    // NO `handleRef` — the command surfaces. The folder is a toggle and says only that.
    expect(folderOf(), "a folder even where nothing drags").toBeTruthy();
    expect(folderOf().tagName, "a real button, hence a tab stop").toBe("BUTTON");
    expect(folderOf().getAttribute("aria-hidden"), "announced, never hidden").toBeNull();
    expect(folderOf().getAttribute("aria-label")).toBe("Toggle group build");
    const bareCursor = folderOf().className;
    expect(bareCursor).toContain("cursor-pointer");

    // WITH a `handleRef` — the terminals' sidebar. The block travels, and the KEYBOARD drag still
    // starts here, which is what the accessible name still says.
    rerender(
      <GroupBlock
        groupId="g1"
        color={null}
        name="build"
        open
        onToggle={() => {}}
        handleRef={() => {}}
      >
        <span>row</span>
      </GroupBlock>,
    );
    expect(folderOf().getAttribute("aria-label")).toBe("Toggle and reorder group build");

    // …AND THE CURSOR DOES NOT DIVERGE BETWEEN THE TWO (human review 01M140TP2F7X0DGMRC0RQQFHAF).
    // « sur l'icon dossier y a toujours le mouse drag, là à dégager ». The glyph carried
    // `cursor-grab` wherever the block travelled — a leftover from the days when it WAS the handle.
    // Since the grip became the whole head, a grab cursor on the glyph alone designates a zone that
    // has no special hold on the gesture, and it contradicts what the glyph does: it folds. Read as
    // an EQUALITY between the families rather than as a literal, so the day someone retunes a
    // toggle's cursor both families still follow it together.
    expect(folderOf().className, "one cursor, both families").toBe(bareCursor);
    expect(folderOf().className, "and it is a toggle's, not a grip's").not.toContain("cursor-grab");
  });

  /**
   * THE FOLDER OPENS AND CLOSES, AND THE WHOLE BAND IS THE TARGET.
   *
   * Folding is what makes the head worth its 32 px: a closed group REPLACES its members instead of
   * adding to them. Two things are frozen here — that a closed block really unmounts its body (not
   * merely hides it: a hidden body would keep answering queries and keep its dots alive), and that
   * one press produces exactly ONE fold. That second one is not pedantry: the folder button sits
   * INSIDE the clickable band, so a handler on both would toggle twice and the group would never
   * appear to move.
   */
  it("folds: a closed block renders no body, and one press on the folder folds exactly once", () => {
    let folds = 0;
    const { container } = render(
      <GroupBlock groupId="g1" color={null} name="build" open onToggle={() => (folds += 1)}>
        <span>row</span>
      </GroupBlock>,
    );
    const block = () => container.querySelector("[data-group-block='g1']") as HTMLElement;
    expect(block().dataset.groupOpen).toBe("true");
    expect(block().textContent).toContain("row");

    // The folder is inside the band; the band carries the handler. One press, one fold.
    (container.querySelector("[data-group-folder='g1']") as HTMLElement).click();
    expect(folds, "the press bubbled to the band and folded once, not twice").toBe(1);
    // …and pressing the band anywhere else folds too — a wide target for no extra pixel.
    (container.querySelector("[data-group-head='g1']") as HTMLElement).click();
    expect(folds).toBe(2);

    // A block that mounts CLOSED renders no body at all. Asserted on a fresh mount rather than on
    // the transition out: the shared `<CollapsibleSection>` keeps the body alive while it animates
    // its height back to zero (that is what makes the fold read as a fold), so the unmount lands a
    // frame later. What is frozen here is the end state — nothing hidden, nothing left answering.
    const closed = render(
      <GroupBlock groupId="g2" color={null} name="build" open={false} onToggle={() => {}}>
        <span>row</span>
      </GroupBlock>,
    );
    const closedBlock = closed.container.querySelector("[data-group-block='g2']") as HTMLElement;
    expect(closedBlock.dataset.groupOpen).toBe("false");
    expect(closedBlock.textContent, "the body is UNMOUNTED, not hidden").not.toContain("row");
  });

  /**
   * THE ROLLUP DOT IS THE CLOSED FOLDER'S JOB, AND ONLY THE CLOSED ONE'S — the workspace
   * accordion's rule, applied to the block that can now hide its members' dots.
   */
  it("carries the rollup on the folder while CLOSED and erases it while OPEN", () => {
    // Each case is its own MOUNT rather than a rerender: the shared dot fades OUT through
    // `<AnimatePresence>` (that is what keeps a state change from popping on screen), so an
    // unmount lands a frame after the prop changes. What is frozen here is the END state.
    const dotOf = (id: string, props: { open: boolean; rollup?: "error" | "idle" }) => {
      const { container } = render(
        <GroupBlock groupId={id} color={null} name="build" {...props}>
          <span>row</span>
        </GroupBlock>,
      );
      const folder = container.querySelector(`[data-group-folder='${id}']`) as HTMLElement;
      return folder.querySelector("[data-state]");
    };

    const closed = dotOf("closed", { open: false, rollup: "error" });
    expect(closed, "closed ⇒ the aggregate shows").toBeTruthy();
    expect(closed?.getAttribute("data-state"), "and it is the aggregate it was given").toBe(
      "error",
    );

    expect(
      dotOf("open", { open: true, rollup: "error" }),
      "open ⇒ the rows carry their own dots, the aggregate would be a duplicate",
    ).toBeNull();

    // A QUIET aggregate draws nothing even folded — `isQuietState`, inherited from the shared dot
    // rather than re-tested per family. A folder with nothing to say is a plain folder.
    expect(
      dotOf("quiet", { open: false, rollup: "idle" }),
      "an idle aggregate is not a grey spot, it is nothing",
    ).toBeNull();

    // And a block with NO state at all carries no pastille either — a modal's group has no runtime.
    expect(dotOf("stateless", { open: false }), "no state ⇒ no pastille").toBeNull();
  });

  /**
   * THE HEAD CARRIES A STATE, NEVER A NUMBER (human review 01M12MW1DV1V455EWBMV048TE6). The block
   * offers no slot for a counter at all any more — the only opening left before the label is the
   * folder, and the only thing it can hold is a dot.
   */
  it("offers no counter slot in the head — a `summary` is a hover, not a pixel of the band", () => {
    const { container } = render(
      <GroupBlock groupId="g1" color={null} name="build" open summary="2 of 3 running">
        <span>row</span>
      </GroupBlock>,
    );
    const head = container.querySelector("[data-group-head='g1']") as HTMLElement;
    expect(head.textContent, "the band shows the name and nothing else").toBe("build");
  });

  it("uses design tokens only — no raw colour anywhere in what it renders", () => {
    const { container } = render(
      <GroupBlock groupId="g1" color={5} name="deploy">
        <span>row</span>
      </GroupBlock>,
    );
    expect(container.innerHTML).not.toMatch(/#[0-9a-fA-F]{3,8}\b/);
    expect(container.innerHTML).not.toMatch(/\brgba?\(/);
  });
});
