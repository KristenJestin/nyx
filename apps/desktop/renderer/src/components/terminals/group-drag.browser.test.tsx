import { useState } from "react";
import { SquareTerminalIcon } from "lucide-react";
import { render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { page, userEvent } from "vitest/browser";

// The REAL utilities and tokens: this spec drives a REAL drag, so the list has the real geometry
// dnd-kit measures its collisions against.
import "@/globals.css";

import { canSplitPane, type HostSize } from "@/components/panes/pane-geometry";
import {
  createGroupFromSplit,
  detachTerminal,
  findGroup,
  groupInsertAnchor,
  groupMemberIds,
  groupOfTerminal,
  insertIntoGroup,
  orderGroupMembers,
  type PaneCollection,
} from "@/components/panes/pane-groups";
import {
  createPaneLayout,
  paneLeaves,
  splitPane,
  type PaneNode,
} from "@/components/panes/pane-tree";
import { RuntimeRow } from "./runtime-row";
import { RuntimeSelectionProvider } from "./runtime-selection";
import { SortableTerminalRows } from "./sortable-terminal-rows";
import { TerminalGroupsProvider } from "./use-terminal-groups";
import { TerminalReorderProvider } from "./use-terminal-reorder";

/**
 * THE FOUR GESTURES, EXERCISED FOR REAL (review 01M0BPP79090KAQ9ZAFJ23ZRTX, criteria 8 and 10).
 *
 * The unit spec drives the component's handlers with synthetic dnd-kit events; here NOTHING is
 * stubbed. Real Chromium, real layout, the real sensors: the pointer gestures go through
 * `pointerdown` / `pointermove` / `pointerup` and the keyboard gestures through Space + arrows +
 * Space, on a list holding TWO groups and TWO bare rows.
 *
 * Every gesture is driven the way a user drives it: MOVE UNTIL THE INDICATOR SAYS WHAT WILL HAPPEN,
 * then release. That is not a convenience — it is the claim the design makes ("le « pendant » porte
 * la conception"), so a test that had to guess a pixel distance instead would be testing the wrong
 * thing.
 *
 * The keyboard half is the CONDITION under which the row's menu was allowed to lose "Leave group"
 * and "Join group N": if a gesture were unreachable without a pointer, removing those entries would
 * take a capability away instead of adding one.
 */

let host: HTMLElement;
let mounted: { unmount: () => void } | null = null;

beforeEach(async () => {
  await page.viewport(1100, 900);
  // THE FOLD IS REMEMBERED (`use-group-folds` → `ui-persistence`), and two walks here fold a block
  // on purpose — a press without movement is a click, and the click folds. Without this, one test's
  // fold is the next one's starting state and its members are simply not in the DOM.
  localStorage.clear();
  host = document.createElement("div");
  host.style.width = "320px";
  host.className = "bg-background";
  document.body.appendChild(host);
});

afterEach(() => {
  // Unmount BEFORE detaching the host: React must be the one to tear its own tree down.
  mounted?.unmount();
  mounted = null;
  host.remove();
});

/** Grow a tree by splitting its LAST leaf, the way a group grows one member at a time. */
function grow(tree: PaneNode, terminalId: string): PaneNode {
  const leaves = paneLeaves(tree);
  return splitPane(tree, {
    paneId: leaves[leaves.length - 1].id,
    direction: leaves.length % 2 === 1 ? "row" : "column",
    terminalId,
    newPaneId: `${terminalId}-pane`,
  }) as PaneNode;
}

function groupOf(id: string, members: readonly string[]) {
  let tree: PaneNode = createPaneLayout(members[0], `${members[0]}-pane`);
  for (const member of members.slice(1)) tree = grow(tree, member);
  return { id, tree, name: null, color: null };
}

/** A roomy tiling host: the 180×120px floor refuses nothing here. */
const ROOMY: HostSize = { width: 1600, height: 1200 };

/** A host too narrow to hold two panes side by side — the floor refuses a BIRTH in it. */
const CRAMPED: HostSize = { width: 200, height: 1200 };

/** The list over a LIVE collection, wired exactly as `use-pane-layout` wires the real one. */
function renderList(initial: PaneCollection, ids: readonly string[], hostSize: HostSize = ROOMY) {
  const live = { collection: initial, order: ids as readonly string[] };
  /** Can a group be BORN in this host? The split's own question, on a one-leaf probe. */
  const canBirth = () =>
    canSplitPane(createPaneLayout("probe", "probe-pane"), "probe-pane", "row", hostSize);

  function Harness() {
    const [collection, setCollection] = useState(initial);
    const [order, setOrder] = useState<readonly string[]>(ids);
    live.collection = collection;
    live.order = order;
    return (
      <RuntimeSelectionProvider initialSelection={null}>
        <TerminalGroupsProvider
          groups={collection.groups}
          leaveGroup={(terminalId) => setCollection((c) => detachTerminal(c, terminalId))}
          reorderGroupMembers={(groupId, memberIds) =>
            setCollection((c) => orderGroupMembers(c, groupId, memberIds))
          }
          insertIntoGroup={(groupId, terminalId, index) =>
            setCollection((c) => insertIntoGroup(c, groupId, terminalId, index)?.collection ?? c)
          }
          canInsertIntoGroup={(groupId, index) => {
            const group = findGroup(live.collection, groupId);
            if (group == null) return false;
            const anchor = groupInsertAnchor(group, index);
            return canSplitPane(group.tree, anchor.paneId, anchor.direction, hostSize);
          }}
          // GESTURE 5, wired the way `use-pane-layout` wires it: the SAME single birth
          // (`createGroupFromSplit`) after detaching both ends, the SAME floor question a split
          // asks — a group of two IS the frame split once — and the same guard AT THE COMMIT, so a
          // refusal really leaves the model untouched instead of only looking refused.
          createGroupWith={(terminalId, partnerId) => {
            if (!canBirth()) return;
            setCollection((c) => {
              const detached = detachTerminal(detachTerminal(c, terminalId), partnerId);
              const born = createGroupFromSplit(detached, {
                terminalId: partnerId,
                newTerminalId: terminalId,
                direction: "row",
              });
              return born?.collection ?? c;
            });
          }}
          canCreateGroup={canBirth}
          dissolveGroup={() => {}}
          renameGroup={() => {}}
          recolorGroup={() => {}}
        >
          <TerminalReorderProvider
            reorderTerminals={(_group, next) => setOrder(next)}
            orderedIds={order}
          >
            <SortableTerminalRows
              terminals={ids.map((id) => ({ id }))}
              group="ws1"
              empty="none"
              renderRow={(t) => (
                // THE REAL ROW, NOT A STAND-IN (review 01M0BZ6Y5H6YJB4327B01EWCTT). This harness
                // used to render its own `<button data-drag-activator>` here, so the eight gestures
                // below — the four at the keyboard included — were proven on a double that declared
                // the marker by hand. `runtime-row.tsx` could then lose `data-drag-activator` and
                // this file stayed green while every real row went keyboard-mute. It is the real
                // `<RuntimeRow>` now: the button Space is pressed on is the one the component
                // renders, and the marker is read from the component instead of being restated.
                // `data-row` rides on the LABEL (a `ReactNode`), which keeps the harness's
                // `[data-row]` lookups — and the insertion-line readings built on them — unchanged.
                <RuntimeRow
                  selection={{ kind: "terminal", id: t.id }}
                  icon={<SquareTerminalIcon className="size-4" />}
                  label={<span data-row={t.id}>{t.id}</span>}
                  state="idle"
                />
              )}
            />
          </TerminalReorderProvider>
        </TerminalGroupsProvider>
      </RuntimeSelectionProvider>
    );
  }

  mounted = render(<Harness />, { container: host });

  /**
   * What the insertion line sits BEFORE right now: a row id, a group id, or `null`.
   *
   * The trait is an OVERLAY inside its own item's positioning wrapper (review
   * 01M0CY3F075ACP4TFBK1KJ49X8): it no longer sits in the flow before the item. Its EDGE carries
   * the direction — `top` = "the row falls before this item", `bottom` = "after it" — so only a
   * `top` trait answers this question, and the item is the wrapper's own row/block.
   */
  const lineBefore = (): string | null => {
    const line = host.querySelector("[data-drop-insertion]") as HTMLElement | null;
    if (line == null || line.dataset.dropInsertionEdge !== "top") return null;
    const wrapper = line.parentElement;
    const block = wrapper?.querySelector(":scope > [data-group-block]");
    if (block != null) return block.getAttribute("data-group-block");
    return wrapper?.querySelector("[data-row]")?.getAttribute("data-row") ?? null;
  };

  return {
    live,
    lineBefore,
    row: (id: string) =>
      host.querySelector(`[data-row='${id}']`)?.closest("[data-terminal-row]") as HTMLElement,
    /**
     * The block's FOLDER — the handle since human review 01M12MW1DV1V455EWBMV048TE6, where the grip
     * became a folder that both moves the block and folds it. Nothing about the GESTURE moved with
     * it: the sensor, the 5px activation and the four intents are untouched, which is exactly what
     * the eight walks below re-prove through the new element.
     */
    folder: (groupId: string) =>
      host.querySelector(`[data-group-folder="${groupId}"]`) as HTMLElement,
    /**
     * The block's HEAD — the handle since human review 01M1312FSV6MAR0P63SEG2RF6C. « c'est toute la
     * ligne qui permet de drag and drop le truc »: the grip left the folder for the whole line, the
     * way a terminal row is grabbed anywhere on its surface. The folder above is still a tab stop
     * and still the keyboard entry, which the walks below prove separately.
     */
    head: (groupId: string) => host.querySelector(`[data-group-head='${groupId}']`) as HTMLElement,
    block: (groupId: string) =>
      host.querySelector(`[data-group-block='${groupId}']`) as HTMLElement | null,
    armed: (groupId: string) =>
      host.querySelector(`[data-group-block='${groupId}']`)?.hasAttribute("data-drop-armed") ===
      true,
    /**
     * "I AM PULLING THIS ROW OUT" — the signal the design actually chose, read whole.
     *
     * `!armed("g1")` is NOT that signal (review 01M0BSNTEN5N2GC9QQBASJYHHB, critical): "g1 is no
     * longer lit" is equally true when the row has walked straight into the NEIGHBOURING block, and
     * a leave-test that stops there is measuring gesture 3 while claiming gesture 2. The state that
     * means "the drop lands OUTSIDE every group" is the conjunction: NO block anywhere is armed,
     * AND the insertion line is drawn in the bare list rather than inside a block's own column. A
     * refused block (`data-drop-refused`, never armed) is caught by the second half.
     */
    outsideEveryBlock: () => {
      if (host.querySelector("[data-drop-armed]") != null) return false;
      const line = host.querySelector("[data-drop-insertion]");
      return line != null && line.closest("[data-group-block]") == null;
    },
    members: (groupId: string) => {
      const group = findGroup(live.collection, groupId);
      return group == null ? null : groupMemberIds(group);
    },
    /** The id of the group holding `terminalId` in the LIVE collection — `null` when it is SOLO. */
    groupIdOf: (terminalId: string) => groupOfTerminal(live.collection, terminalId)?.id ?? null,
    /**
     * "THE DROP WOULD MAKE A FOLDER OF THIS LINE" — gesture 5's pendant, read off the row's own
     * wrapper. `refused` is its twin: the floor forbids the birth, and the line says so instead of
     * lighting up. They are never both, which is asserted where it matters.
     */
    mergingWith: (id: string, refused = false) => {
      const marked = host.querySelector(
        refused ? "[data-drop-merge-refused]" : "[data-drop-merge]",
      );
      return marked != null && marked.querySelector(`[data-row='${id}']`) != null;
    },
  };
}

/** Let the browser paint — dnd-kit schedules its moves and measurements on animation frames. */
function frames(count = 3): Promise<void> {
  return new Promise((resolve) => {
    let left = count;
    const tick = () => (left-- <= 0 ? resolve() : requestAnimationFrame(tick));
    requestAnimationFrame(tick);
  });
}

function pointer(type: string, x: number, y: number): PointerEvent {
  return new PointerEvent(type, {
    bubbles: true,
    cancelable: true,
    composed: true,
    pointerId: 1,
    pointerType: "mouse",
    isPrimary: true,
    button: 0,
    buttons: type === "pointerup" ? 0 : 1,
    clientX: x,
    clientY: y,
  });
}

/**
 * A REAL pointer drag: press on `from`, walk in `direction` (−1 up, +1 down) until the list SAYS the
 * drop will do what we want, then release. Returns whether the condition was ever reached — a drag
 * that never got its indicator is a failed gesture, and the test says so rather than asserting on an
 * outcome nobody was promised.
 */
async function dragUntil(
  from: Element,
  direction: -1 | 1,
  until: () => boolean,
  maxSteps = 40,
): Promise<boolean> {
  const box = from.getBoundingClientRect();
  const x = box.left + box.width / 2;
  let y = box.top + box.height / 2;
  from.dispatchEvent(pointer("pointerdown", x, y));
  await frames(2);
  let reached = false;
  for (let i = 0; i < maxSteps && !reached; i += 1) {
    y += direction * 6;
    document.dispatchEvent(pointer("pointermove", x, y));
    await frames(2);
    // The 5px activation means the first step or two are not a drag yet.
    reached = i > 1 && until();
  }
  document.dispatchEvent(pointer("pointerup", x, y));
  await frames(8);
  return reached;
}

function key(target: Element | Document, code: string): void {
  const init = {
    bubbles: true,
    cancelable: true,
    composed: true,
    code,
    key: code === "Space" ? " " : code,
  };
  target.dispatchEvent(new KeyboardEvent("keydown", init));
  target.dispatchEvent(new KeyboardEvent("keyup", init));
}

/**
 * A REAL keyboard drag: Space on the focused element picks it up, arrows walk it, Space drops it.
 * `until` is checked AFTER each arrow — never before the first — so a row already sitting where the
 * predicate is happy is still actually moved.
 */
async function keyboardDragUntil(
  handle: HTMLElement,
  arrow: "ArrowUp" | "ArrowDown",
  until: () => boolean,
  max = 8,
): Promise<boolean> {
  handle.focus();
  key(handle, "Space");
  await frames(3);
  let reached = false;
  for (let i = 0; i < max && !reached; i += 1) {
    key(document, arrow);
    await frames(3);
    reached = until();
  }
  key(document, "Space");
  await frames(8);
  return reached;
}

/** Two groups and two bare rows — the list the criterion asks for. */
const twoGroupsTwoSolos = (): PaneCollection => ({
  groups: [groupOf("g1", ["A", "B", "C"]), groupOf("g2", ["E", "F"])],
});
const IDS = ["A", "B", "C", "D", "E", "F", "G"];

describe("The four gestures at the real POINTER (criterion 10)", () => {
  it("1 — RE-ORDER inside the block: C walked up until the line sits above A", async () => {
    const list = renderList(twoGroupsTwoSolos(), IDS);
    expect(list.members("g1"), "before").toEqual(["A", "B", "C"]);

    const reached = await dragUntil(
      list.row("C"),
      -1,
      () => list.armed("g1") && list.lineBefore() === "A",
    );

    expect(reached, "the block stayed armed and the line reached the top of it").toBe(true);
    expect(list.members("g1"), "after").toEqual(["C", "A", "B"]);
    expect(
      paneLeaves(findGroup(list.live.collection, "g1")!.tree).map((l) => l.terminalId),
      "the panes follow the members",
    ).toEqual(["C", "A", "B"]);
    expect(list.live.collection.groups, "still two groups").toHaveLength(2);
  });

  it("2 — LEAVE the block: C walked down until the drop lands OUTSIDE every block", async () => {
    const list = renderList(twoGroupsTwoSolos(), IDS);
    expect(list.members("g1"), "before").toEqual(["A", "B", "C"]);
    expect(list.groupIdOf("C"), "before").toBe("g1");

    const reached = await dragUntil(list.row("C"), 1, () => list.outsideEveryBlock());

    expect(reached, "no block is armed and the line sits in the BARE list").toBe(true);
    expect(list.members("g1"), "after").toEqual(["A", "B"]);
    expect(list.block("g1"), "the block survives with two").toBeTruthy();
    // THE ASSERTION THE GESTURE IS ABOUT: C is a SOLO terminal again. Without it the test passes
    // just as happily when C has merely moved into the neighbouring block — which is gesture 3.
    expect(list.groupIdOf("C"), "C is solo again").toBeNull();
    expect(list.members("g2"), "and no other block gained it").toEqual(["E", "F"]);
  });

  it("3 — ENTER a block: the bare row D walked up until g1 ARMS", async () => {
    const list = renderList(twoGroupsTwoSolos(), IDS);
    const before = paneLeaves(findGroup(list.live.collection, "g1")!.tree).length;
    expect(list.members("g1"), "before").toEqual(["A", "B", "C"]);

    const reached = await dragUntil(list.row("D"), -1, () => list.armed("g1"));

    expect(reached, "the block armed — the drop lands inside").toBe(true);
    expect(list.members("g1"), "after").toContain("D");
    expect(list.members("g1")).toHaveLength(4);
    expect(paneLeaves(findGroup(list.live.collection, "g1")!.tree)).toHaveLength(before + 1);
  });

  it("4 — MOVE the whole block by its folder: g2 walked up past the bare row D", async () => {
    const list = renderList(twoGroupsTwoSolos(), IDS);
    const treeBefore = findGroup(list.live.collection, "g2")!.tree;
    const collectionBefore = list.live.collection;
    expect(list.live.order, "before").toEqual(IDS);

    const reached = await dragUntil(list.folder("g2"), -1, () => {
      const before = list.lineBefore();
      return before === "D" || before === "g1";
    });

    expect(reached, "the line reached a slot above the block's own").toBe(true);
    expect(list.live.order, "after").not.toEqual(IDS);
    expect(
      list.live.order.join("").includes("EF"),
      `E and F stayed together: ${list.live.order}`,
    ).toBe(true);
    // NOTHING else moved: the very same collection object, hence the very same pane tree.
    expect(list.members("g2")).toEqual(["E", "F"]);
    expect(findGroup(list.live.collection, "g2")!.tree, "the tree, by reference").toBe(treeBefore);
    expect(list.live.collection).toBe(collectionBefore);
  });
});

/**
 * THE GRIP IS THE WHOLE HEAD (human review 01M1312FSV6MAR0P63SEG2RF6C).
 *
 * Gesture 4 was already proved above THROUGH THE FOLDER, at the pointer and at the keyboard, and
 * those two walks are the non-regression: widening the grip must not narrow it. What is added here
 * is the part that was impossible before — a press taken on the head somewhere the folder is NOT —
 * and the other half of the same claim: the SAME surface still folds when the press does not move,
 * with no arbitrage written anywhere, only the sensor's 5 px activation.
 */
describe("The grip is the whole head, and it still folds", () => {
  it("4 — MOVE the whole block from a point that is NOT the folder", async () => {
    const list = renderList(twoGroupsTwoSolos(), IDS);
    const head = list.head("g2");
    const headBox = head.getBoundingClientRect();
    const folderBox = list.folder("g2").getBoundingClientRect();
    // WHERE THE PRESS LANDS is the whole point of this walk: `dragUntil` presses the element's own
    // centre, and that centre has to be clear of the folder for the walk to prove anything at all.
    expect(
      headBox.left + headBox.width / 2,
      "the press is taken outside the folder's box",
    ).toBeGreaterThan(folderBox.right);

    const treeBefore = findGroup(list.live.collection, "g2")!.tree;
    const reached = await dragUntil(head, -1, () => {
      const before = list.lineBefore();
      return before === "D" || before === "g1";
    });

    expect(reached, "the line reached a slot above the block's own").toBe(true);
    expect(list.live.order, "the block travelled").not.toEqual(IDS);
    expect(
      list.live.order.join("").includes("EF"),
      `E and F stayed together: ${list.live.order}`,
    ).toBe(true);
    // Gesture 4 is a pure LIST re-order: no group is touched and no tree is rebuilt.
    expect(list.members("g2"), "its members kept their order").toEqual(["E", "F"]);
    expect(findGroup(list.live.collection, "g2")!.tree, "its tree, by reference").toBe(treeBefore);
  });

  it("…and a press on that same surface that never MOVED folds instead of dragging", async () => {
    const list = renderList(twoGroupsTwoSolos(), IDS);
    const head = list.head("g1");
    const collectionBefore = list.live.collection;
    expect(list.block("g1")!.dataset.groupOpen, "open to begin with").toBe("true");

    // A REAL click — the browser's own down/up at one point, under the 5 px the sensor arms at.
    await userEvent.click(head);
    await frames(4);

    expect(list.block("g1")!.dataset.groupOpen, "the click folded it").toBe("false");
    // …and NOTHING was dragged: same order, same collection object, hence the same trees.
    expect(list.live.order, "no re-order happened").toEqual(IDS);
    expect(list.live.collection, "the collection was not touched").toBe(collectionBefore);
  });
});

/**
 * GESTURE 5 — A FOLDER BORN OF TWO LINES THAT ALREADY EXIST (human review 01M1313GN2ESW0BCM8S1G0ZY2P).
 *
 * « si je veux créer un groupe avec les terminaux déjà existant sans avoir de groupe préalable
 * comment ça se passe ? » — it could not be done. `createGroupFromSplit` was the one birth of a
 * terminal group and it only fires when a SOLO terminal is SPLIT, which creates a terminal. Two
 * terminals that were already there had no gesture to join them.
 *
 * A root line is three bands now: its edges insert before/after it, its MIDDLE founds a folder of it
 * and the line being held. This is the real walk — real Chromium, real sensors, real layout — so
 * what is proved is that the band is REACHABLE, not just computable.
 */
describe("Gesture 5 at the real POINTER — the middle band founds a folder", () => {
  it("D walked onto the MIDDLE of G: a group of exactly those two, and no terminal created", async () => {
    const list = renderList(twoGroupsTwoSolos(), IDS);
    expect(list.groupIdOf("D"), "D is solo before").toBeNull();
    expect(list.groupIdOf("G"), "G is solo before").toBeNull();
    const before = list.live.collection.groups.length;

    const reached = await dragUntil(list.row("D"), 1, () => list.mergingWith("G"));

    expect(reached, "the target line armed — the release will found a folder").toBe(true);
    expect(list.live.collection.groups, "one more group").toHaveLength(before + 1);
    const born = list.live.collection.groups.find((g) => g.id !== "g1" && g.id !== "g2");
    expect(born, "and it is a NEW one, not one of the two that were there").toBeTruthy();
    // BOTH members are the ones the gesture named — the line under the pointer keeps its place and
    // the held one joins it. Nothing else was taken along.
    expect(groupMemberIds(born!)).toEqual(["G", "D"]);
    // NO TERMINAL WAS CREATED: the two leaves ARE the two terminals that already existed.
    expect(paneLeaves(born!.tree)).toHaveLength(2);
    expect(list.members("g1"), "the other groups are untouched").toEqual(["A", "B", "C"]);
    expect(list.members("g2")).toEqual(["E", "F"]);
  });

  it("…and the 180×120px floor REFUSES the birth in a host too narrow, founding nothing", async () => {
    const list = renderList(twoGroupsTwoSolos(), IDS, CRAMPED);
    const before = list.live.collection.groups.length;

    // The refusal is read DURING the walk, which is also where "never both at once" can be checked
    // at all: after the release both pendants are gone.
    let bothAtOnce = false;
    const reached = await dragUntil(list.row("D"), 1, () => {
      if (list.mergingWith("G") && list.mergingWith("G", true)) bothAtOnce = true;
      return list.mergingWith("G", true);
    });

    expect(reached, "the target line wears the REFUSAL — the habitual destructive pendant").toBe(
      true,
    );
    expect(bothAtOnce, "a line never says 'come in' while turning the drop away").toBe(false);
    // AND IT REALLY IS A REFUSAL, not just a colour: the model is untouched after the release.
    expect(list.live.collection.groups).toHaveLength(before);
    expect(list.groupIdOf("D"), "D is still solo").toBeNull();
    expect(list.groupIdOf("G"), "G is still solo").toBeNull();
  });
});

/**
 * A BLOCK NEVER ARMS ANOTHER BLOCK (human review 01M140TQC52BT95TK60DD5TPYH).
 *
 * « petit beug visuel quand je drag and drop un dossier, l'interface me montre que je peux le drag
 * dans un autre dossier, dans la réalité si je le lance dedans il le fait pas ». Groups do not nest,
 * and the commit knew it — `dropIntent` short-circuits a dragged block to a plain list re-order —
 * but the PENDANT read `move()`'s projection, and `move()` resolves a member row under the pointer
 * as a child of ITS block's slot. Walking a block over an OPEN block's members therefore armed the
 * target: the ring promised an entry the release could not perform.
 *
 * THE FOUR COMBINATIONS ARE WALKED, NOT READ. The finding was reported for block-on-block, and the
 * same divergence could live in any of the neighbouring readings — so each is driven at the real
 * pointer and SAMPLED AT EVERY STEP, because a pendant that lies for three frames in the middle of a
 * walk is exactly the defect and would be invisible to an assertion taken at the end.
 */
describe("A block never arms another block — the four combinations, measured", () => {
  /**
   * Walk `from` a fixed number of steps and sample the pendant after EVERY move. Unlike
   * {@link dragUntil} it has no stopping condition: what is being proved is a NEGATIVE, so the walk
   * must cross the whole target rather than halt at the first frame that satisfies something.
   */
  async function dragSampling(
    from: Element,
    direction: -1 | 1,
    sample: () => void,
    steps = 30,
  ): Promise<void> {
    const box = from.getBoundingClientRect();
    const x = box.left + box.width / 2;
    let y = box.top + box.height / 2;
    from.dispatchEvent(pointer("pointerdown", x, y));
    await frames(2);
    for (let i = 0; i < steps; i += 1) {
      y += direction * 6;
      document.dispatchEvent(pointer("pointermove", x, y));
      await frames(2);
      if (i > 1) sample();
    }
    document.dispatchEvent(pointer("pointerup", x, y));
    await frames(8);
  }

  /** Every block wearing a drag pendant right now — armed OR refused, both are lies here. */
  const pendants = () =>
    [...host.querySelectorAll("[data-drop-armed], [data-drop-refused]")].map(
      (el) => el.getAttribute("data-group-block") ?? "?",
    );

  /** The lines the list says a folder would be born with — never available to a block. */
  const merging = () =>
    host.querySelectorAll("[data-drop-merge], [data-drop-merge-refused]").length;

  it("BLOCK on BLOCK, and on its MEMBER lines: no ring, ever — and the release re-orders the list", async () => {
    const list = renderList(twoGroupsTwoSolos(), IDS);
    // g1 is OPEN, so its three member rows are in the DOM and are what `move()` used to resolve the
    // drop against. Walking g2 UP crosses D, then C, B, A, then g1's head — the whole block.
    expect(list.block("g1")!.dataset.groupOpen, "g1 is open — its members are collidable").toBe(
      "true",
    );
    const seen: string[] = [];
    let merges = 0;
    await dragSampling(list.head("g2"), -1, () => {
      seen.push(...pendants());
      merges += merging();
    });

    expect(seen, "no block ever lit up while a block was in the air").toEqual([]);
    expect(merges, "and no line ever offered to found a folder with a block").toBe(0);
    // WHAT THE RELEASE DID is the other half: the announced re-order really happened, and the list
    // is still every terminal exactly once — the nested projection used to drop g1's members out of
    // the flat order entirely and put the literal `block:g1` string in their place.
    expect([...list.live.order].sort(), "every terminal, exactly once").toEqual([...IDS].sort());
    expect(list.members("g1"), "g1 kept its members").toEqual(["A", "B", "C"]);
    expect(list.members("g2"), "g2 travelled whole").toEqual(["E", "F"]);
    expect(list.live.collection.groups, "and no group was born or lost").toHaveLength(2);
  });

  it("BLOCK on a ROOT ROW: the insertion trait, and still no ring", async () => {
    const list = renderList(twoGroupsTwoSolos(), IDS);
    const seen: string[] = [];
    let sawTrait = false;
    // g2 walked DOWN over the bare row G — a root line, whose middle band founds folders for rows.
    await dragSampling(
      list.head("g2"),
      1,
      () => {
        seen.push(...pendants());
        if (host.querySelector("[data-drop-insertion]") != null) sawTrait = true;
      },
      12,
    );

    expect(seen, "a root row cannot arm a block either").toEqual([]);
    expect(sawTrait, "what it shows instead is WHERE the block lands").toBe(true);
    expect([...list.live.order].sort(), "every terminal, exactly once").toEqual([...IDS].sort());
    expect(list.members("g2"), "the group travelled whole").toEqual(["E", "F"]);
  });

  it("ROW on a BLOCK still arms — the pendant was not silenced, only made true", async () => {
    const list = renderList(twoGroupsTwoSolos(), IDS);
    const reached = await dragUntil(list.row("D"), -1, () => list.armed("g1"));
    expect(reached, "a ROW entering a block is the one case that must light up").toBe(true);
    expect(list.members("g1")).toContain("D");
  });
});

describe("The four gestures at the KEYBOARD (criterion 8)", () => {
  it("1 — RE-ORDER inside the block, no pointer", async () => {
    const list = renderList(twoGroupsTwoSolos(), IDS);
    const reached = await keyboardDragUntil(
      list.row("A").querySelector("button") as HTMLElement,
      "ArrowDown",
      () => list.armed("g1") && list.lineBefore() !== "A",
    );
    expect(reached, "the block stayed armed — the row is still inside it").toBe(true);
    expect(list.members("g1"), "the members re-ordered").not.toEqual(["A", "B", "C"]);
    expect(new Set(list.members("g1")), "membership untouched").toEqual(new Set(["A", "B", "C"]));
  });

  it("2 — LEAVE the block, no pointer", async () => {
    const list = renderList(twoGroupsTwoSolos(), IDS);
    // MORE ARROWS THAN THE OTHER THREE WALKS, and the reason is gesture 5. The keyboard sensor steps
    // the drag by 10 px, so walking a row down now SWEEPS THROUGH the middle band of each root line
    // it passes — where the reading is "found a folder with this one", not "insert here". That is
    // the gesture being reachable without a pointer rather than a hazard: the ring says so, no trait
    // is drawn, and one more arrow moves on to the edge band. The walk simply has to be allowed to
    // take that extra step, exactly as a hand would.
    const reached = await keyboardDragUntil(
      list.row("C").querySelector("button") as HTMLElement,
      "ArrowDown",
      () => list.block("g1") != null && list.outsideEveryBlock(),
      16,
    );
    expect(reached, "no block is armed and the line sits in the BARE list").toBe(true);
    expect(list.members("g1"), "C left the group").toEqual(["A", "B"]);
    // Same reinforcement as the pointer twin: "left g1" and "became solo" are two different facts,
    // and only the second one is the gesture.
    expect(list.groupIdOf("C"), "C is solo again").toBeNull();
    expect(list.members("g2"), "and no other block gained it").toEqual(["E", "F"]);
  });

  it("3 — ENTER a block, no pointer", async () => {
    const list = renderList(twoGroupsTwoSolos(), IDS);
    const reached = await keyboardDragUntil(
      list.row("D").querySelector("button") as HTMLElement,
      "ArrowUp",
      () => list.armed("g1"),
    );
    expect(reached, "the block armed — the drop lands inside").toBe(true);
    expect(list.members("g1"), "D joined").toContain("D");
  });

  it("4 — MOVE the whole block, no pointer: Space on its folder, arrows, Space", async () => {
    const list = renderList(twoGroupsTwoSolos(), IDS);
    const treeBefore = findGroup(list.live.collection, "g2")!.tree;

    const reached = await keyboardDragUntil(list.folder("g2"), "ArrowUp", () => {
      const before = list.lineBefore();
      return before === "D" || before === "g1";
    });

    expect(reached, "the line reached a slot above the block's own").toBe(true);
    expect(list.live.order, "the block travelled").not.toEqual(IDS);
    expect(list.members("g2"), "its members kept their order").toEqual(["E", "F"]);
    expect(findGroup(list.live.collection, "g2")!.tree, "its tree, by reference").toBe(treeBefore);
  });
});

/**
 * A FOLDED BLOCK IS STILL A DROP TARGET (human review 01M12MW1DV1V455EWBMV048TE6 — the one thing the
 * finding asks to be verified on the `group-drag.ts` side, and it was NOT true when it was asked).
 *
 * Folding removes the members from the DOM, and entering a group has always happened through a
 * MEMBER ROW under the pointer: `move()` resolves a block as a PARENT only by finding one of its
 * children, never by the block itself (which is also a child of the root list). Measured before the
 * fix: walking a bare row over a folded block armed nothing and drew the insertion line BESIDE it,
 * on every one of forty steps — "drop a terminal into a folded group" silently became "re-order the
 * list", which looks exactly like nothing happening.
 *
 * `projectDrop` is the answer, and this is the walk that proves it, the way a user does it: fold the
 * group by pressing its head, then walk a bare row onto it until it ARMS, and release. Nothing is
 * stubbed — same sensors, same slots, same `dropIntent`.
 */
describe("A folded block still takes a drop (criterion: group-drag.ts)", () => {
  it("3 — ENTER a CLOSED block: D walked up until the folded g1 ARMS", async () => {
    const list = renderList(twoGroupsTwoSolos(), IDS);
    expect(list.members("g1"), "before").toEqual(["A", "B", "C"]);

    // Fold it: the whole head band is the toggle, and a press without movement is a click.
    (host.querySelector("[data-group-head='g1']") as HTMLElement).click();
    // The body animates its height back to zero before it unmounts (the shared collapsible), so the
    // members leave the DOM a few frames after the press rather than with it.
    for (let i = 0; i < 60 && host.querySelector("[data-row='A']") != null; i += 1) {
      await frames(2);
    }
    expect((list.block("g1") as HTMLElement).dataset.groupOpen, "the block really folded").toBe(
      "false",
    );
    expect(host.querySelector("[data-row='A']"), "its members are unmounted with it").toBeNull();

    const reached = await dragUntil(list.row("D"), -1, () => list.armed("g1"));

    expect(reached, "the folded block armed — the drop lands inside it").toBe(true);
    expect(list.members("g1"), "and D really joined").toContain("D");
    expect(list.members("g1")).toHaveLength(4);
  });
});
