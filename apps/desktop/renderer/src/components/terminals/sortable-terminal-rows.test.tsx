import type { ReactNode } from "react";
import { render } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

/**
 * The dnd-kit `onDragEnd` GLUE of `<SortableTerminalRows>` (task `01KXBNX51EPQ69MVKYG4W960HD`,
 * follow-up finding `01KXCCYT26ZFAJJ5M9XRV5HF4Z`). The reorder GATE drives `reorderTerminals`
 * through the e2e seam and the persistence unit starts at `reorderTerminals` — so the glue itself
 * (`move(ids, event)` → `sameOrder` guard → callback) was exercised by NOTHING. The WebDriver
 * pointer sensor is undrivable in the Electron/CDP harness, but the `onDragEnd` computation is pure
 * JS: we mock dnd-kit's `<DragDropProvider>` to a pass-through that CAPTURES the `onDragEnd` the
 * component wires, render the REAL component, then fire a SYNTHETIC dnd-kit `dragend` event through
 * it. `@dnd-kit/helpers`'s `move` and the `sameOrder` guard stay REAL — only the (untestable)
 * pointer machinery is stubbed, so a regression in `move()`, the guard, or the callback wiring
 * (group + reordered ids) is caught.
 */

/**
 * The minimal shape of the dnd-kit `dragend` event the handler + `move` read: a top-level
 * `canceled` (the handler's early-out) plus an `operation` carrying the dragged `source` and the
 * drop `target` (what `move` resolves the new index order from).
 *
 * `source.manager` IS PART OF THAT MINIMUM NOW. The list stopped sorting a flat array of ids and
 * sorts the TWO-LEVEL record `group-drag.ts` describes (`{ __list__: [...], "block:g1": [...] }`),
 * because that is what makes "the row entered / left a block" a position rather than a code path.
 * `@dnd-kit/helpers`' `move` takes a different branch for a record: before deciding anything it
 * reads `source.manager.dragOperation` for the pointer position (it needs it to tell "dropped
 * ABOVE the target" from "below" when the two live in different slots), and returns the items
 * UNCHANGED when there is no manager at all. So the double carries one — an inert one, with the
 * position at the origin: the same-slot re-orders below never consult it, and pinning a pointer
 * position here would be pinning dnd-kit's geometry rather than this component's glue.
 */
interface FakeDragEndEvent {
  canceled: boolean;
  operation: {
    canceled: boolean;
    source: { id: string; manager?: unknown } | null;
    /** `element` + `position` are what gesture 5's BAND is measured from — see `dropOnMiddleOf`. */
    target: { id: string; element?: Element } | null;
    position?: { current: { x: number; y: number } };
  };
}

/** The inert `manager` the record branch of `move` dereferences — see {@link FakeDragEndEvent}. */
const MANAGER = { dragOperation: { shape: null, position: { current: { x: 0, y: 0 } } } };

// Captured lazily when the mocked provider renders; `vi.hoisted` so the holder exists by the time
// the mock factory (hoisted above imports) runs.
const dnd = vi.hoisted(() => ({
  onDragEnd: null as null | ((event: unknown) => void),
  onDragOver: null as null | ((event: unknown) => void),
}));

vi.mock("@dnd-kit/react", () => ({
  DragDropProvider: ({
    children,
    onDragEnd,
    onDragOver,
  }: {
    children: ReactNode;
    onDragEnd: (event: unknown) => void;
    onDragOver: (event: unknown) => void;
  }) => {
    dnd.onDragEnd = onDragEnd;
    // `onDragOver` IS PART OF THE MINIMUM NOW. The drop commits THE PENDANT THAT WAS ON SCREEN
    // rather than re-deriving it from the dragend event (so a release can never mean something
    // other than what the ring said), which means a drag has to be walked here too: hover, then
    // release. Every drop below therefore fires the two in that order.
    dnd.onDragOver = onDragOver;
    return <>{children}</>;
  },
}));

// `useSortable` needs the real DragDropManager context (absent under the stubbed provider) and a
// live DOM node; the rows only read `{ ref, isDragging }`, so a no-op is enough to render.
vi.mock("@dnd-kit/react/sortable", () => ({
  useSortable: () => ({ ref: () => {}, isDragging: false }),
}));

import type { PaneGroup } from "@/components/panes/pane-groups";
import { createPaneLayout, splitPane, type PaneNode } from "@/components/panes/pane-tree";
import { SortableTerminalRows } from "./sortable-terminal-rows";
import { TerminalGroupsProvider } from "./use-terminal-groups";
import { TerminalReorderProvider } from "./use-terminal-reorder";

/**
 * Walk one drop through the component: HOVER the target (which is what computes the pendant), then
 * RELEASE. Both halves matter — the release commits what the hover put on screen.
 */
function fireDragEnd(event: FakeDragEndEvent): void {
  if (!dnd.onDragEnd || !dnd.onDragOver) {
    throw new Error("the drag handlers were not captured — the provider never rendered");
  }
  dnd.onDragOver(event);
  dnd.onDragEnd(event);
}

/**
 * A `dragend` that drags `sourceId` onto `targetId` (both live), the drop `move` reorders on.
 *
 * It carries NO target box and NO drag point, which is what makes it an EDGE drop: the band of
 * gesture 5 is a DOM reading, so `readMergeTarget` answers `null` without one and the four position
 * gestures are read exactly as they always were. {@link dropOnMiddleOf} is the fifth.
 */
function drop(sourceId: string, targetId: string): FakeDragEndEvent {
  return {
    canceled: false,
    operation: {
      canceled: false,
      source: { id: sourceId, manager: MANAGER },
      target: { id: targetId },
    },
  };
}

/**
 * A `dragend` released on the MIDDLE BAND of `targetId` — gesture 5, the birth of a folder out of
 * two lines that already exist (human review 01M1313GN2ESW0BCM8S1G0ZY2P).
 */
function dropOnMiddleOf(sourceId: string, targetId: string): FakeDragEndEvent {
  const height = 32;
  return {
    canceled: false,
    operation: {
      canceled: false,
      source: { id: sourceId, manager: MANAGER },
      target: {
        id: targetId,
        element: { getBoundingClientRect: () => ({ top: 0, height }) } as unknown as Element,
      },
      position: { current: { x: 0, y: height / 2 } },
    },
  };
}

describe("SortableTerminalRows — onDragEnd glue (dnd-kit)", () => {
  const reorderTerminals = vi.fn();

  beforeEach(() => {
    reorderTerminals.mockClear();
    dnd.onDragEnd = null;
    dnd.onDragOver = null;
  });

  function renderRows(group: string | null) {
    return render(
      <TerminalReorderProvider reorderTerminals={reorderTerminals} orderedIds={["A", "B", "C"]}>
        <SortableTerminalRows
          terminals={[{ id: "A" }, { id: "B" }, { id: "C" }]}
          group={group}
          empty="none"
          renderRow={(t) => <span>{t.id}</span>}
        />
      </TerminalReorderProvider>,
    );
  }

  it("a WORKSPACE drag that CHANGES the order commits reorderTerminals(group, reordered ids)", () => {
    renderRows("ws1");
    // Drag A onto B → move([A,B,C], A→B) = [B,A,C]; the order changed → the callback fires with it.
    fireDragEnd(drop("A", "B"));
    expect(reorderTerminals).toHaveBeenCalledTimes(1);
    expect(reorderTerminals).toHaveBeenCalledWith("ws1", ["B", "A", "C"]);
  });

  it("reorders the LOOSE group (group=null) with the moved order", () => {
    renderRows(null);
    // Drag C onto A → move([A,B,C], C→A) = [C,A,B].
    fireDragEnd(drop("C", "A"));
    expect(reorderTerminals).toHaveBeenCalledTimes(1);
    expect(reorderTerminals).toHaveBeenCalledWith(null, ["C", "A", "B"]);
  });

  it("a NO-OP drop (source === target ⇒ sameOrder) never calls reorderTerminals (guard)", () => {
    renderRows("ws1");
    // Dropping A onto itself yields the SAME order → the `sameOrder` guard skips the commit.
    fireDragEnd(drop("A", "A"));
    expect(reorderTerminals).not.toHaveBeenCalled();
  });

  it("a CANCELED drag is ignored (no reorder even if source/target differ)", () => {
    renderRows("ws1");
    fireDragEnd({
      canceled: true,
      operation: { canceled: true, source: { id: "A", manager: MANAGER }, target: { id: "C" } },
    });
    expect(reorderTerminals).not.toHaveBeenCalled();
  });
});

/**
 * THE SAME GLUE, WITH A BLOCK IN THE LIST — the half the flat-array version could not express.
 *
 * The list is two-level now, so `onDragEnd` has three more verdicts to reach than "re-order":
 * a row dropped INSIDE a block joins the group, a member dropped OUT of it leaves, and a drop that
 * stays inside re-orders the members. Which one happened is `dropIntent`'s call (pinned pure in
 * `group-drag.test.ts`); what is pinned HERE is that the component routes it to the right mutator
 * of `useTerminalGroups` AND still persists the flat list order alongside it.
 */
describe("SortableTerminalRows — the group gestures reach the model", () => {
  const reorderTerminals = vi.fn();
  const leaveGroup = vi.fn();
  const insertIntoGroup = vi.fn();
  const reorderGroupMembers = vi.fn();

  beforeEach(() => {
    for (const spy of [reorderTerminals, leaveGroup, insertIntoGroup, reorderGroupMembers]) {
      spy.mockClear();
    }
    dnd.onDragEnd = null;
    dnd.onDragOver = null;
  });

  /** g1 = [A, B] as a block, C bare — the smallest list that has both a block and a bare row. */
  function renderWithGroup(canInsert = true) {
    const g1: PaneGroup = {
      id: "g1",
      name: null,
      color: null,
      tree: splitPane(createPaneLayout("A", "A-pane"), {
        paneId: "A-pane",
        direction: "row",
        terminalId: "B",
        newPaneId: "B-pane",
      }) as PaneNode,
    };
    return render(
      <TerminalGroupsProvider
        groups={[g1]}
        leaveGroup={leaveGroup}
        reorderGroupMembers={reorderGroupMembers}
        insertIntoGroup={insertIntoGroup}
        canInsertIntoGroup={() => canInsert}
        dissolveGroup={() => {}}
        renameGroup={() => {}}
        recolorGroup={() => {}}
      >
        <TerminalReorderProvider reorderTerminals={reorderTerminals} orderedIds={["A", "B", "C"]}>
          <SortableTerminalRows
            terminals={[{ id: "A" }, { id: "B" }, { id: "C" }]}
            group="ws1"
            empty="none"
            renderRow={(t) => <span>{t.id}</span>}
          />
        </TerminalReorderProvider>
      </TerminalGroupsProvider>,
    );
  }

  it("a bare row dropped ON a member ENTERS the group, at the position it landed on", () => {
    renderWithGroup();
    // C (root) onto A (a member of g1) → C crosses into the block's slot at A's index.
    fireDragEnd(drop("C", "A"));
    expect(insertIntoGroup).toHaveBeenCalledWith("g1", "C", 0);
    // …and the list order it produced is persisted in the same breath.
    expect(reorderTerminals).toHaveBeenCalledWith("ws1", ["C", "A", "B"]);
    expect(leaveGroup).not.toHaveBeenCalled();
  });

  it("…unless the 180×120px floor refuses it — nothing is written at all", () => {
    renderWithGroup(false);
    fireDragEnd(drop("C", "A"));
    expect(insertIntoGroup).not.toHaveBeenCalled();
    expect(reorderTerminals).not.toHaveBeenCalled();
  });

  it("a member dropped ON a bare row LEAVES its group", () => {
    renderWithGroup();
    // B (member of g1) onto C (root) → B lands in the bare list and quits the group.
    fireDragEnd(drop("B", "C"));
    expect(leaveGroup).toHaveBeenCalledWith("B");
    expect(insertIntoGroup).not.toHaveBeenCalled();
  });

  it("a member dropped ON its own partner RE-ORDERS the members, membership untouched", () => {
    renderWithGroup();
    fireDragEnd(drop("B", "A"));
    expect(reorderGroupMembers).toHaveBeenCalledWith("g1", ["B", "A"]);
    expect(leaveGroup).not.toHaveBeenCalled();
    expect(insertIntoGroup).not.toHaveBeenCalled();
  });
});

/**
 * GESTURE 5 IN THE SIDEBAR — grouping terminals that ALREADY EXIST (human review
 * 01M1313GN2ESW0BCM8S1G0ZY2P).
 *
 * Verbatim: « si je veux créer un groupe avec les terminaux déjà existant sans avoir de groupe
 * préalable comment ça se passe ? ». It could not be done at all: `createGroupFromSplit` was the
 * ONE birth of a terminal group and it only fires when a SOLO terminal is split, so two terminals
 * that were already there had no gesture to join them.
 *
 * WHAT IS PINNED HERE is the routing and the two refusals. The reading of the drop is pure and lives
 * in `group-drag.test.ts`; the real pointer walk is in `group-drag.browser.test.tsx`.
 */
describe("SortableTerminalRows — a group is born from two lines that already exist", () => {
  const reorderTerminals = vi.fn();
  const createGroupWith = vi.fn();

  beforeEach(() => {
    reorderTerminals.mockClear();
    createGroupWith.mockClear();
    dnd.onDragEnd = null;
    dnd.onDragOver = null;
  });

  /** Three SOLO rows and nothing else — the list the gesture was missing from. */
  function renderSolos(
    props: { createGroupWith?: typeof createGroupWith | null; canCreateGroup?: boolean } = {},
  ) {
    return render(
      <TerminalGroupsProvider
        groups={[]}
        leaveGroup={() => {}}
        createGroupWith={
          props.createGroupWith === undefined ? createGroupWith : props.createGroupWith
        }
        canCreateGroup={() => props.canCreateGroup ?? true}
        dissolveGroup={() => {}}
        renameGroup={() => {}}
        recolorGroup={() => {}}
      >
        <TerminalReorderProvider reorderTerminals={reorderTerminals} orderedIds={["A", "B", "C"]}>
          <SortableTerminalRows
            terminals={[{ id: "A" }, { id: "B" }, { id: "C" }]}
            group="ws1"
            empty="none"
            renderRow={(t) => <span>{t.id}</span>}
          />
        </TerminalReorderProvider>
      </TerminalGroupsProvider>,
    );
  }

  it("dropping a row on the MIDDLE of another founds a group of exactly those two", () => {
    renderSolos();
    fireDragEnd(dropOnMiddleOf("A", "C"));
    // BOTH members are named by the gesture: the held row and the line it landed on. Nothing else
    // was taken along — which is the whole objection to the dotted slot this replaces.
    expect(createGroupWith).toHaveBeenCalledWith("A", "C");
    // …and the pair is made contiguous in the list, the partner first (the order the tree reads).
    expect(reorderTerminals).toHaveBeenCalledWith("ws1", ["B", "C", "A"]);
  });

  it("…while the same drop on that row's EDGE is still a plain re-order", () => {
    renderSolos();
    fireDragEnd(drop("A", "C"));
    expect(createGroupWith).not.toHaveBeenCalled();
    expect(reorderTerminals).toHaveBeenCalledWith("ws1", ["B", "C", "A"]);
  });

  it("a surface that offers NO birth writes nothing — not even a re-order it did not ask for", () => {
    renderSolos({ createGroupWith: null });
    fireDragEnd(dropOnMiddleOf("A", "C"));
    expect(createGroupWith).not.toHaveBeenCalled();
    expect(
      reorderTerminals,
      "the order is committed with the birth, never on its own",
    ).not.toHaveBeenCalled();
  });
});
