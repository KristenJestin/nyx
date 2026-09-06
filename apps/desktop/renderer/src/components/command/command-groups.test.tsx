import { readFileSync } from "node:fs";
import { join } from "node:path";
import { useState, type ReactNode } from "react";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

vi.mock("@/components/ui/toast", () => ({
  toast: { error: vi.fn(), success: vi.fn(), info: vi.fn() },
}));

/**
 * DRIVING THE DRAG — the same seam `sortable-terminal-rows.test.tsx` uses, and for the same reason.
 *
 * The gestures of this screen are drops now, and a real pointer drag is undrivable under jsdom (no
 * layout, so dnd-kit's collision detection has nothing to measure). What is testable is the GLUE:
 * `<DragDropProvider>` is stubbed to a pass-through that CAPTURES the `onDragEnd` the component
 * wires, the REAL component renders, and a synthetic dnd-kit `dragend` goes through it —
 * `@dnd-kit/helpers`' `move`, `dropIntent` and the commit order all stay real. The pure reading of a
 * drop is pinned next door in `group-drag.test.ts`; what is pinned here is that the screen routes
 * each verdict to the right controller call.
 */
interface FakeDragEndEvent {
  canceled: boolean;
  operation: {
    canceled: boolean;
    source: { id: string; manager?: unknown } | null;
    /** `element` and `position` are what gesture 5's BAND is measured from — see `mergeOnto`. */
    target: { id: string; element?: Element } | null;
    position?: { current: { x: number; y: number } };
  };
}

/** The inert `manager` the RECORD branch of `move` dereferences for the pointer position. */
const MANAGER = { dragOperation: { shape: null, position: { current: { x: 0, y: 0 } } } };

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
    // The drop commits THE PENDANT THAT WAS ON SCREEN rather than re-deriving it from the dragend
    // event, so a drag has to be WALKED here too: hover, then release.
    dnd.onDragOver = onDragOver;
    return <>{children}</>;
  },
}));

// `useSortable` needs the real DragDropManager context (absent under the stubbed provider) and a
// live DOM node; the entries only read `{ ref, handleRef, isDragging }`, so no-ops are enough.
vi.mock("@dnd-kit/react/sortable", () => ({
  useSortable: () => ({ ref: () => {}, handleRef: () => {}, isDragging: false }),
}));

import { CommandCatalog } from "./command-catalog";
import { CommandGroupDetail } from "./command-group-detail";
import { commandGroupTree, type CommandRow } from "./command-group-tree";
import { NO_SELECTION, type CommandsSelection } from "./commands-selection";
import { CommandPaneHost } from "./command-pane-host";
import type { CommandGroup, CommandGroupsController } from "./use-command-groups";

import { COMMAND_GROUP_SHAPES } from "@/components/panes/pane-groups";
import { layoutFitsFloor, paneRects, MIN_PANE_WIDTH_PX } from "@/components/panes/pane-geometry";
import type { WorkspaceRuntimeService } from "@/components/runtime/use-workspace-runtime";

/**
 * Walk one drop through the screen: HOVER the target (which is what computes the pendant), then
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
 * A `dragend` that drags `sourceId` onto `targetId` — the drop `move` re-parents on.
 *
 * It carries NO target element and NO pointer position, which is exactly what makes it an EDGE drop:
 * `readMergeTarget` cannot measure a band without both, so it answers `null` and the four
 * position-based gestures are read as they always were. {@link mergeOnto} is the fifth.
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
 * A `dragend` released on the MIDDLE BAND of `targetId` — gesture 5 (human review
 * 01M1313GN2ESW0BCM8S1G0ZY2P): the two lines become a folder.
 *
 * The band is read from the DOM, so the fake supplies the two readings it needs: a target box and
 * the drag's live point. `ratio` is where in that box the point sits — 0.5 is dead centre, and the
 * edge values below prove the SAME drop on the SAME target is a plain re-order there.
 */
function mergeOnto(sourceId: string, targetId: string, ratio = 0.5): FakeDragEndEvent {
  const height = 32;
  const element = {
    getBoundingClientRect: () => ({ top: 0, height }) as DOMRect,
  } as unknown as Element;
  return {
    canceled: false,
    operation: {
      canceled: false,
      source: { id: sourceId, manager: MANAGER },
      target: { id: targetId, element },
      position: { current: { x: 0, y: ratio * height } },
    },
  };
}

const SRC = join(process.cwd(), "src");

/** The commands of a project, as the settings screen lists them. */
const COMMANDS = [
  { id: "c1", name: "check", command: "bun run check" },
  { id: "c2", name: "build", command: "bun run build" },
  { id: "c3", name: "lint", command: "bun run lint" },
  { id: "c4", name: "test", command: "bun run test" },
];

/**
 * A controller over an in-memory group list, driven by the SAME rules the backend enforces
 * (at most one group per command, dissolution below two members, the ordered member list IS the
 * disposition) — so this exercises the screen, and the Rust suite exercises the transitions.
 */
function fakeController(initial: CommandGroup[] = []): CommandGroupsController & {
  state: () => CommandGroup[];
} {
  let groups = initial;
  const dissolveShort = () => {
    groups = groups.filter((g) => g.command_ids.length >= 2);
  };
  const detach = (commandId: string) => {
    groups = groups.map((g) =>
      g.command_ids.includes(commandId)
        ? { ...g, command_ids: g.command_ids.filter((id) => id !== commandId) }
        : g,
    );
    dissolveShort();
  };
  const controller: CommandGroupsController & { state: () => CommandGroup[] } = {
    get groups() {
      return groups;
    },
    state: () => groups,
    groupOf: (commandId) => groups.find((g) => g.command_ids.includes(commandId)) ?? null,
    createGroup: async (commandIds) => {
      for (const id of commandIds) detach(id);
      groups = [
        ...groups,
        {
          id: `g${groups.length + 1}`,
          project_id: "p1",
          name: null,
          color: null,
          layout_shape: "auto",
          command_ids: [...commandIds],
        },
      ];
    },
    addToGroup: async (groupId, commandId) => {
      detach(commandId);
      groups = groups.map((g) =>
        g.id === groupId ? { ...g, command_ids: [...g.command_ids, commandId] } : g,
      );
    },
    removeFromGroup: async (commandId) => detach(commandId),
    dissolveGroup: async (groupId) => {
      groups = groups.filter((g) => g.id !== groupId);
    },
    renameGroup: async (groupId, name) => {
      groups = groups.map((g) => (g.id === groupId ? { ...g, name } : g));
    },
    recolorGroup: async (groupId, color) => {
      groups = groups.map((g) => (g.id === groupId ? { ...g, color } : g));
    },
    setLayout: async (groupId, commandIds) => {
      groups = groups.map((g) => (g.id === groupId ? { ...g, command_ids: [...commandIds] } : g));
    },
    setShape: async (groupId, shape) => {
      groups = groups.map((g) => (g.id === groupId ? { ...g, layout_shape: shape } : g));
    },
    reorderCommands: async () => {},
    reload: async () => {},
  };
  return controller;
}

/**
 * The screen driven by REAL React state, so a mutation repaints exactly as it does in the app.
 * `onState` publishes the live group list to the assertions.
 */
function Screen({
  initial,
  onState,
  onOrder,
  onDelete,
  commands = COMMANDS,
}: {
  initial: CommandGroup[];
  onState: (groups: CommandGroup[]) => void;
  /** Every list order the screen persisted, in the order it persisted them. */
  onOrder?: (commandIds: readonly string[]) => void;
  /** Every command the catalogue's trailing trash asked to delete. */
  onDelete?: (commandId: string) => void;
  commands?: readonly CommandRow[];
}) {
  const [groups, setGroups] = useState<CommandGroup[]>(initial);
  const [selection, setSelection] = useState<CommandsSelection>(NO_SELECTION);
  onState(groups);
  const dissolveShort = (list: CommandGroup[]) => list.filter((g) => g.command_ids.length >= 2);
  const detach = (list: CommandGroup[], commandId: string) =>
    dissolveShort(
      list.map((g) =>
        g.command_ids.includes(commandId)
          ? { ...g, command_ids: g.command_ids.filter((id) => id !== commandId) }
          : g,
      ),
    );
  const controller: CommandGroupsController = {
    groups,
    groupOf: (commandId) => groups.find((g) => g.command_ids.includes(commandId)) ?? null,
    createGroup: async (commandIds) =>
      setGroups((list) => {
        let next = list;
        for (const id of commandIds) next = detach(next, id);
        return [
          ...next,
          {
            id: `g${next.length + 1}`,
            project_id: "p1",
            name: null,
            color: null,
            layout_shape: "auto",
            command_ids: [...commandIds],
          },
        ];
      }),
    addToGroup: async (groupId, commandId) =>
      setGroups((list) =>
        detach(list, commandId).map((g) =>
          g.id === groupId ? { ...g, command_ids: [...g.command_ids, commandId] } : g,
        ),
      ),
    removeFromGroup: async (commandId) => setGroups((list) => detach(list, commandId)),
    dissolveGroup: async (groupId) => setGroups((list) => list.filter((g) => g.id !== groupId)),
    renameGroup: async (groupId, name) =>
      setGroups((list) => list.map((g) => (g.id === groupId ? { ...g, name } : g))),
    recolorGroup: async (groupId, color) =>
      setGroups((list) => list.map((g) => (g.id === groupId ? { ...g, color } : g))),
    setLayout: async (groupId, commandIds) =>
      setGroups((list) =>
        list.map((g) => (g.id === groupId ? { ...g, command_ids: [...commandIds] } : g)),
      ),
    setShape: async (groupId, shape) =>
      setGroups((list) => list.map((g) => (g.id === groupId ? { ...g, layout_shape: shape } : g))),
    reorderCommands: async (commandIds) => onOrder?.(commandIds),
    reload: async () => {},
  };
  // THE SCREEN IS A MASTER/DETAIL since finding 01M132XFZ5KY8QMJ79HZZSBV3J: the catalogue on the
  // left never moves, and the panel on the right shows what is selected. The harness composes the
  // two exactly as `<CommandsSection>` does, minus the IPC-backed template list, so every gesture
  // below is played against the real components.
  const selected = selection.kind === "group" ? groups.find((g) => g.id === selection.id) : null;
  const membersOf = (group: CommandGroup) =>
    commands
      .filter((c) => group.command_ids.includes(c.id))
      .sort((a, b) => group.command_ids.indexOf(a.id) - group.command_ids.indexOf(b.id));
  return (
    <>
      <CommandCatalog
        commands={commands}
        controller={controller}
        selection={selection}
        onSelect={setSelection}
        onDelete={(command) => onDelete?.(command.id)}
        importableCount={null}
      />
      {selected == null ? null : (
        <CommandGroupDetail
          group={selected}
          members={membersOf(selected)}
          onRename={(name) => void controller.renameGroup(selected.id, name)}
          onRecolor={(color) => void controller.recolorGroup(selected.id, color)}
          onPickShape={(shape) => void controller.setShape(selected.id, shape)}
          onDissolve={() => {
            void controller.dissolveGroup(selected.id);
            setSelection(NO_SELECTION);
          }}
        />
      )}
    </>
  );
}

/** Select a folder — the head's click, which is what opens its detail panel. */
function openGroup(container: HTMLElement, groupId = "g1"): void {
  const head = container.querySelector(`[data-group-head='${groupId}']`);
  if (head == null) throw new Error(`no head for group ${groupId}`);
  fireEvent.click(head);
}

/** Render the screen and expose the live group list plus every list order it persisted. */
function renderScreen(initial: CommandGroup[], commands: readonly CommandRow[] = COMMANDS) {
  let live: CommandGroup[] = initial;
  const orders: string[][] = [];
  const deletions: string[] = [];
  const result = render(
    <Screen
      initial={initial}
      onState={(g) => (live = g)}
      onOrder={(ids) => orders.push([...ids])}
      onDelete={(id) => deletions.push(id)}
      commands={commands}
    />,
  );
  return { ...result, state: () => live, orders: () => orders, deletions: () => deletions };
}

function group(command_ids: string[], overrides: Partial<CommandGroup> = {}): CommandGroup {
  return {
    id: "g1",
    project_id: "p1",
    name: null,
    color: null,
    layout_shape: "auto",
    command_ids,
    ...overrides,
  };
}

function service(commandId: string, overrides: Partial<WorkspaceRuntimeService> = {}) {
  return {
    id: `${commandId}-inst`,
    command_id: commandId,
    workspace_id: "ws1",
    name: commandId,
    command: `run ${commandId}`,
    run_state: "idle",
    exit_code: null,
    unread: false,
    ...overrides,
  } as WorkspaceRuntimeService;
}

/**
 * THE AUTOMATIC DISPOSITION (review 01M0ABR6V501D0SPR2BYBVZC4A, major). The panes follow the
 * ordered member list, through the SAME `autoLayoutTree` the terminals' automatic placement uses.
 */
describe("the commands group layout", () => {
  const boxes = (ids: string[]) =>
    paneRects(commandGroupTree(ids)!).map((p) => [
      p.terminalId,
      p.rect.left,
      p.rect.top,
      p.rect.width,
      p.rect.height,
    ]);

  it("2 — the second goes to the RIGHT", () => {
    expect(boxes(["c1", "c2"])).toEqual([
      ["c1", 0, 0, 50, 100],
      ["c2", 50, 0, 50, 100],
    ]);
  });

  it("3 — the third goes UNDERNEATH", () => {
    expect(boxes(["c1", "c2", "c3"])).toEqual([
      ["c1", 0, 0, 50, 100],
      ["c2", 50, 0, 50, 50],
      ["c3", 50, 50, 50, 50],
    ]);
  });

  it("4 — a QUADRANT", () => {
    expect(boxes(["c1", "c2", "c3", "c4"])).toEqual([
      ["c1", 0, 0, 50, 50],
      ["c3", 0, 50, 50, 50],
      ["c2", 50, 0, 50, 50],
      ["c4", 50, 50, 50, 50],
    ]);
  });

  it("REUSES the terminals' engine — same module, same floor", () => {
    // The layout is the SHARED `pane-geometry` / `pane-groups` pair, not a second engine.
    const source = readFileSync(
      join(SRC, "components", "command", "command-pane-host.tsx"),
      "utf8",
    );
    expect(source).toContain('from "@/components/panes/pane-geometry"');
    expect(
      readFileSync(join(SRC, "components", "command", "command-group-tree.ts"), "utf8"),
    ).toContain("shapeTree");

    // …and the floor is the SAME 180×120px: a four-pane layout does not fit a 300px-wide host.
    const four = commandGroupTree(["c1", "c2", "c3", "c4"])!;
    expect(layoutFitsFloor(four, { width: 300, height: 900 })).toBe(false);
    expect(layoutFitsFloor(four, { width: 800, height: 900 })).toBe(true);
    // An UNMEASURED host cannot refuse anything (the rule `canSplitPane` follows).
    expect(layoutFitsFloor(four, null)).toBe(true);
    // The narrowest pane of the refused layout is under the floor, measurably.
    const narrowest = Math.min(...paneRects(four).map((p) => (300 * p.rect.width) / 100));
    expect(narrowest).toBeLessThan(MIN_PANE_WIDTH_PX);
  });
});

describe("CommandCatalog — composition happens HERE, and it happens at the DRAG", () => {
  /**
   * A host of a known size, so the 180×120px floor has something to measure. jsdom gives every
   * element a zero box, and an UNMEASURABLE host cannot refuse anything (the rule `canSplitPane`
   * follows) — so without this the floor is never exercised at all.
   */
  function mountHost(width: number, height: number): () => void {
    const host = document.createElement("div");
    host.setAttribute("data-command-pane-host", "true");
    host.getBoundingClientRect = () =>
      ({
        width,
        height,
        top: 0,
        left: 0,
        right: width,
        bottom: height,
        x: 0,
        y: 0,
        toJSON: () => ({}),
      }) as DOMRect;
    document.body.appendChild(host);
    return () => host.remove();
  }

  /**
   * THE WHOLE LINE IS THE GRIP (finding 01M132XFZ5KY8QMJ79HZZSBV3J) — the shape a terminal row has
   * always had (`sortable-terminal-rows.tsx` hands dnd-kit the element and no handle). The gripped
   * dots this replaces were a separate 24×24 target standing exactly where the family glyph goes,
   * on a list whose every other family is grabbed anywhere on its surface.
   */
  it("draws a LINE, not a gripped row: a select band and a trailing delete, nothing else", () => {
    const live = renderScreen([group(["c1", "c2"])]);
    // No grip anywhere — neither the button nor the marker it carried.
    expect(live.container.querySelector("[data-command-grip]")).toBeNull();
    expect(screen.queryByLabelText("Move check")).toBeNull();
    // Every row is a SidebarLine with a select band and its own delete.
    for (const id of ["c1", "c2", "c3"]) {
      const row = live.container.querySelector(`[data-command-row='${id}']`) as HTMLElement;
      expect(row, id).not.toBeNull();
      expect(row.querySelector("[data-command-select]"), `${id}: a select band`).not.toBeNull();
    }
    expect(screen.getByLabelText("Delete check")).toBeTruthy();
    // The two controls the user threw out are gone — by name AND by the attribute they carried.
    expect(screen.queryByLabelText(/to a group$/)).toBeNull();
    expect(screen.queryByLabelText(/from its group$/)).toBeNull();
    expect(live.container.querySelector("[data-group-action]")).toBeNull();
    expect(screen.queryByText(/automatic layout/)).toBeNull();
  });

  /**
   * THE LINE SELECTS, AND THE FOLDER'S HEAD SELECTS ITS FOLDER — the master/detail's whole hinge.
   * Nothing arbitrates between selecting and dragging: the shared sensor arms past 5 px, so a press
   * that never moved is a click (`reorder-dnd.ts`).
   */
  it("a click on a line SELECTS it, and the trailing delete never selects what it deletes", () => {
    const live = renderScreen([group(["c1", "c2"])]);
    const row = () => live.container.querySelector("[data-command-row='c3']") as HTMLElement;
    expect(row().dataset.selected).toBe("false");
    fireEvent.click(row().querySelector("[data-command-select]")!);
    expect(row().dataset.selected).toBe("true");

    // The trash is DRAG-INERT (dnd-kit reads the attribute, a React stopPropagation cannot) and it
    // does not move the selection to the row it is about to remove.
    const trash = screen.getByLabelText("Delete check");
    expect(trash.getAttribute("data-drag-inert")).toBe("");
    fireEvent.click(trash);
    expect(live.deletions()).toEqual(["c1"]);
    expect(row().dataset.selected, "the selection did not follow the trash").toBe("true");
  });

  /** THE COUNT IS WRITTEN ONCE — it used to be in the tab AND in the footer. */
  it("writes the command count exactly once, in the catalogue's head", () => {
    const live = renderScreen([group(["c1", "c2"])]);
    const counts = live.container.querySelectorAll("[data-command-count]");
    expect(counts).toHaveLength(1);
    expect(counts[0].textContent).toBe("4");
  });

  it("ENTERS a group by dropping a row on a member, AT the position it landed on", async () => {
    const live = renderScreen([group(["c1", "c2"])]);
    // INSERTION, not append: the drop landed on the first member, so the row takes its place.
    fireDragEnd(drop("c3", "c1"));
    await waitFor(() => expect(live.state()[0].command_ids).toEqual(["c3", "c1", "c2"]));
  });

  it("LEAVES a group by dropping a member outside its block", async () => {
    const live = renderScreen([group(["c1", "c2", "c3"])]);
    fireDragEnd(drop("c3", "c4"));
    await waitFor(() => expect(live.state()[0].command_ids).toEqual(["c1", "c2"]));
  });

  it("DISSOLVES when the drop takes the group below two members", async () => {
    const live = renderScreen([group(["c1", "c2"])]);
    fireDragEnd(drop("c1", "c3"));
    await waitFor(() => expect(live.state()).toEqual([]));
    // …and both commands are still listed, now solo (no block left at all).
    expect(live.container.querySelectorAll("[data-group-block]")).toHaveLength(0);
    expect(live.container.querySelector('[data-command-row="c1"]')).not.toBeNull();
    expect(live.container.querySelector('[data-command-row="c2"]')).not.toBeNull();
  });

  it("RE-ORDERS the members when the drop stays inside the block", async () => {
    const live = renderScreen([group(["c1", "c2", "c3"])]);
    fireDragEnd(drop("c2", "c1"));
    await waitFor(() => expect(live.state()[0].command_ids).toEqual(["c2", "c1", "c3"]));
  });

  /**
   * THE FIFTH GESTURE, ON THE LINE ITSELF (human review 01M1313GN2ESW0BCM8S1G0ZY2P).
   *
   * It used to be a dotted slot at the bottom of the list. Three things were wrong with it and only
   * the third is about pixels: it held permanent room for an occasional gesture, in a list that
   * scrolls; it could not say WHO joins whom, so it picked the second member itself
   * (`soloIds.find(…)`) and took a command along without announcing it; and with no partner to be
   * found it answered nothing at all — the one place on the screen where a completed gesture was a
   * silent no-op.
   *
   * A root line is THREE BANDS now: its two edges insert before/after it, its middle makes a folder
   * of it and the line being held. The partner is the line under the pointer, so nothing is guessed
   * and nothing can be unanswerable.
   */
  it("CREATES a group out of the DROPPED line and the line it was dropped ON", async () => {
    const live = renderScreen([]);
    fireDragEnd(mergeOnto("c1", "c3"));
    await waitFor(() => expect(live.state()).toHaveLength(1));
    // c3 was the target and keeps its place; c1 is the one that travelled. NOTHING ELSE joined —
    // the slot this replaces would have paired c1 with c2 without ever saying so.
    expect(live.state()[0].command_ids).toEqual(["c3", "c1"]);
  });

  it("…and the SAME drop on the same line's EDGE is a plain re-order, not a birth", async () => {
    const live = renderScreen([]);
    fireDragEnd(mergeOnto("c1", "c3", 0.05));
    await waitFor(() => expect(live.orders()).toHaveLength(1));
    expect(live.state(), "no group was founded on an edge band").toEqual([]);
    fireDragEnd(mergeOnto("c1", "c3", 0.95));
    expect(live.state(), "…nor on the other one").toEqual([]);
  });

  /**
   * THE LIST ORDER IS PERSISTED WITH THE MEMBERSHIP (finding 01M11XTRXVVPZ9XMDW5ARWKWHA). Nothing
   * could write it before `command_reorder` existed: the list kept creation order, so a command
   * dragged out of a group sprang back to its "natural" position instead of staying where it fell.
   */
  it("PERSISTS the list order alongside the membership — a dropped row stays where it fell", async () => {
    const live = renderScreen([group(["c1", "c2"])]);
    fireDragEnd(drop("c4", "c3"));
    await waitFor(() => expect(live.orders()).toHaveLength(1));
    expect(live.orders()[0]).toEqual(["c1", "c2", "c4", "c3"]);
    // A pure list re-order touches no membership.
    expect(live.state()[0].command_ids).toEqual(["c1", "c2"]);
  });

  it("a CANCELED drag writes nothing at all", () => {
    const live = renderScreen([group(["c1", "c2"])]);
    fireDragEnd({
      canceled: true,
      operation: { canceled: true, source: { id: "c3", manager: MANAGER }, target: { id: "c1" } },
    });
    expect(live.state()[0].command_ids).toEqual(["c1", "c2"]);
    expect(live.orders()).toEqual([]);
  });

  it("draws the group as a grey unnamed BLOCK — the SAME component as the terminals' list", () => {
    const { container } = renderScreen([group(["c1", "c2"])]);
    const block = container.querySelector("[data-group-block]") as HTMLElement;
    expect(block).not.toBeNull();
    expect(block.dataset.groupColor).toBe("none");
    expect(block.getAttribute("style")).toContain("var(--identity-0)");
    // Exactly its members are inside it; the solo commands are not.
    const inside = Array.from(block.querySelectorAll("[data-command-row]")).map(
      (row) => (row as HTMLElement).dataset.commandRow,
    );
    expect(inside).toEqual(["c1", "c2"]);
    // THE SHARED COMPONENT, cited: this screen imports the terminals' GroupBlock and GroupPanel —
    // there is no commands-only copy of either.
    const source = readFileSync(join(SRC, "components", "command", "command-catalog.tsx"), "utf8");
    expect(source).toContain('from "@/components/panes/group-block"');
    // …and NOT a second block of its own: the sidebar's folder is the only one there is.
    expect(source).not.toMatch(/function w*GroupHead/);
  });

  /**
   * ONE DRAG STACK, AND IT MOVED HOUSE (findings 01M11XTRXVVPZ9XMDW5ARWKWHA and
   * 01M0ZMHM9GSC39SCN2DBRQ8VX3).
   *
   * The screen used to carry a NATIVE HTML5 drag on the vignettes — the only `draggable` in the
   * renderer, next to a depot whose drag stack is `@dnd-kit`, configured ONCE so that no surface
   * "reintroduces a competing drag config". That is settled by reuse rather than by removal now: the
   * screen mounts the SHARED sensors and the SHARED wiring. And those modules had to MOVE for it —
   * they lived in `components/terminals/`, and importing that directory from `components/command/`
   * would re-open the import cycle the earlier finding closed.
   */
  describe("the gesture is the terminals' gesture, from the neutral module", () => {
    const source = () =>
      readFileSync(join(SRC, "components", "command", "command-catalog.tsx"), "utf8");

    it("mounts the ONE dnd-kit config, and no native drag anywhere", () => {
      const live = renderScreen([group(["c1", "c2", "c3"])]);
      expect(live.container.querySelector("[draggable='true']")).toBeNull();
      for (const el of live.container.querySelectorAll("*")) {
        expect(el.getAttribute("draggable")).toBeNull();
      }
      // JSX shapes only — the module's prose still EXPLAINS the history, and must be free to.
      expect(source()).not.toMatch(/\bdraggable=/);
      expect(source()).not.toMatch(/\bonDrop=/);
      expect(source()).toContain("sensors={reorderSensors}");
    });

    it("imports the drag from components/panes, never from components/terminals", () => {
      const imports = source()
        .split("\n")
        .filter((line) => /^\s*(import |} from |\s+)/.test(line) && line.includes('from "'));
      const joined = imports.join("\n");
      expect(joined).toContain('from "@/components/panes/group-drag"');
      expect(joined).toContain('from "@/components/panes/reorder-dnd"');
      expect(joined).toContain('from "@/components/panes/sortable-entry"');
      // THE CYCLE STAYS CLOSED (finding 01M0ZMHM9GSC39SCN2DBRQ8VX3): not one import of this screen
      // reaches into the terminals' directory. The prose above may still name it — it has to, to
      // explain why the modules moved.
      expect(joined).not.toContain("components/terminals");
    });

    it("the keyboard reaches the same gestures — the sensor opts out of preventActivation", () => {
      // The row's handle is a real `<button>`, hence a tab stop, and the SHARED sensor set is what
      // lets a Space on it start a drag (dnd-kit's default guard cancels a gesture whose keydown
      // does not land on the draggable element itself).
      const live = renderScreen([group(["c1", "c2"])]);
      // The tab stop is the LINE'S OWN select band — a real `<button>`, exactly as a terminal
      // row's is, which is what makes the gestures reachable without a pointer.
      const band = live.container.querySelector("[data-command-select='c1']") as HTMLElement;
      expect(band.tagName).toBe("BUTTON");
      const sensors = readFileSync(join(SRC, "components", "panes", "reorder-dnd.ts"), "utf8");
      expect(sensors).toContain("KeyboardSensor.configure");
      // BOTH sensors opt out of dnd-kit's default "the event must land on the draggable itself"
      // guard, and both do it through the SAME expression. It stopped being a bare `() => false`
      // when a control inside a draggable head had to be able to say "never me" (human review
      // 01M1312FSV6MAR0P63SEG2RF6C) — but there is still exactly one rule, written once and read
      // twice, which is what this counts.
      expect(
        sensors.match(/preventActivation: \(event\) => startsOnInertControl\(event\)/g),
      ).toHaveLength(2);
    });
  });

  /**
   * THE DISPOSITION CARD IS A PREVIEW, NOT A HANDLE (finding 01M11XTRXVVPZ9XMDW5ARWKWHA).
   *
   * Verbatim: the two-click swap was « nul on comprend pas », because nothing said you were between
   * the clicks nor what the second would do. The cells are now exactly what they always looked like
   * — a faithful drawing of the tiling — and re-arranging is dragging a row, which can INSERT at a
   * chosen position where the two clicks could only SWAP a pair. Nothing was lost in the trade.
   */
  describe("the disposition card is a READ-ONLY preview", () => {
    it("draws one cell per pane, in disposition order, and not one of them is a control", () => {
      const live = renderScreen([group(["c1", "c2", "c3"])]);
      // THE EDITOR IS NOT IN THE BLOCK ANY MORE (finding 01M132XFZ5KY8QMJ79HZZSBV3J): it was three
      // levels down and ~110px per group. It is one click away, in the folder's detail panel.
      expect(live.container.querySelector("[data-layout-cell]")).toBeNull();
      openGroup(live.container);
      const cells = Array.from(live.container.querySelectorAll<HTMLElement>("[data-layout-cell]"));
      expect(cells.map((cell) => cell.dataset.layoutCell)).toEqual(["c1", "c2", "c3"]);
      for (const cell of cells) {
        expect(cell.tagName).toBe("DIV");
        expect(cell.getAttribute("aria-pressed")).toBeNull();
        expect(cell.onclick).toBeNull();
      }
      // The pick-then-pick vocabulary is gone with the gesture.
      expect(screen.queryByLabelText(/in the layout$/)).toBeNull();
      expect(screen.queryByLabelText(/^Swap /)).toBeNull();
      expect(screen.queryByLabelText(/^Cancel moving /)).toBeNull();
    });

    it("follows the re-arrangement: the cells move when the members do", async () => {
      const live = renderScreen([group(["c1", "c2", "c3"])]);
      openGroup(live.container);
      fireDragEnd(drop("c2", "c1"));
      await waitFor(() =>
        expect(
          Array.from(live.container.querySelectorAll<HTMLElement>("[data-layout-cell]")).map(
            (cell) => cell.dataset.layoutCell,
          ),
        ).toEqual(["c2", "c1", "c3"]),
      );
    });
  });

  /**
   * THE SHAPE IS CHOSEN, NOT DERIVED (finding 01M11XTRXVVPZ9XMDW5ARWKWHA). It used to be deduced
   * from the member count and nothing else — two side by side, three in an L, four in a quadrant —
   * so « 4 en colonne » and « 1 grand + 3 petits » simply did not exist.
   */
  describe("the tiling SHAPE is chosen from a closed vocabulary", () => {
    it("offers the six named shapes, `auto` selected by default", () => {
      const live = renderScreen([group(["c1", "c2", "c3"])]);
      openGroup(live.container);
      const thumbs = Array.from(
        live.container.querySelectorAll<HTMLElement>("[data-layout-shape]"),
      );
      expect(thumbs.map((t) => t.dataset.layoutShape)).toEqual([...COMMAND_GROUP_SHAPES]);
      expect(thumbs.filter((t) => t.getAttribute("aria-pressed") === "true")).toHaveLength(1);
      expect(
        thumbs.find((t) => t.getAttribute("aria-pressed") === "true")?.dataset.layoutShape,
      ).toBe("auto");
    });

    it("picking one PERSISTS it and re-derives the preview through it", async () => {
      const live = renderScreen([group(["c1", "c2", "c3"])]);
      openGroup(live.container);
      fireEvent.click(screen.getByLabelText("Layout column"));
      await waitFor(() => expect(live.state()[0].layout_shape).toBe("column"));
      // A column of three is THIRDS, not 50/25/25: the named shapes strike 1/n bands.
      expect(
        paneRects(commandGroupTree(["c1", "c2", "c3"], "column")!).map((p) =>
          Math.round(p.rect.height),
        ),
      ).toEqual([33, 33, 33]);
    });

    it("a shape the 180×120px floor refuses is STRUCK THROUGH, not hidden", () => {
      // 760×420 with four members: `column` gives 105px-high panes (under the 120px floor) while
      // `row` gives 190px-wide ones (over the 180px one) — so exactly one of the two is refused.
      const unmount = mountHost(760, 420);
      try {
        const live = renderScreen([group(["c1", "c2", "c3", "c4"])]);
        openGroup(live.container);
        const thumb = (shape: string) =>
          live.container.querySelector<HTMLButtonElement>(`[data-layout-shape="${shape}"]`)!;
        // SHOWN, and refused — not missing.
        expect(thumb("column")).not.toBeNull();
        expect(thumb("column").dataset.layoutShapeRefused).toBe("true");
        expect(thumb("column").disabled).toBe(true);
        expect(thumb("row").dataset.layoutShapeRefused).toBeUndefined();
        expect(thumb("row").disabled).toBe(false);
      } finally {
        unmount();
      }
    });
  });

  /**
   * THE NAME AND THE COLOUR MOVED INTO THE FOLDER'S OWN PANEL (finding 01M132XFZ5KY8QMJ79HZZSBV3J)
   * — they used to be a SECOND DIALOG opened over the modal from the head's menu trigger, which is
   * the same duplication the two lists were, one level up. The vocabulary is still the shared one
   * (`panes/group-swatches.tsx`, mounted by `group-panel.tsx` too), so the two cannot drift.
   */
  it("names and colours the group from its own detail panel — and the second dialog is gone", async () => {
    const live = renderScreen([group(["c1", "c2"])]);
    expect(screen.queryByLabelText("Group options"), "no second editor").toBeNull();
    openGroup(live.container);
    const field = (await screen.findByLabelText("Group name (optional)")) as HTMLInputElement;
    expect(field.value).toBe("");
    expect(screen.getByLabelText("No colour").getAttribute("aria-pressed")).toBe("true");

    fireEvent.change(field, { target: { value: "build" } });
    fireEvent.blur(field);
    await waitFor(() => expect(live.state()[0].name).toBe("build"));
    fireEvent.click(screen.getByLabelText("Colour 4"));
    await waitFor(() => expect(live.state()[0].color).toBe(4));
  });
});

/**
 * SYMMETRY WITH THE AGENT (review finding 01M0AS6Q6KEE8AJFGCXDJB2SNQ, major — and its second round,
 * 01M11XTRXVVPZ9XMDW5ARWKWHA).
 *
 * The rule the human review set: "rien que l'agent puisse faire et que tu ne puisses pas défaire à
 * la main, ni l'inverse". The first round lifted the screen to the agent's level with a menu that
 * NAMED its target. This round replaces the menu with a drag that POINTS at it — the target is
 * designated instead of listed — and adds the eighth action the agent gained at the same time: the
 * tiling SHAPE. Nothing left the hand in the trade: the drop can insert a member at a chosen
 * position, where the menu could only append.
 */
describe("every operation exists on BOTH sides", () => {
  /** Five commands so two groups can coexist AND a solo one is still left to move around. */
  const FIVE: CommandRow[] = [...COMMANDS, { id: "c5", name: "fmt", command: "bun run fmt" }];
  const TWO_GROUPS = [group(["c1", "c2"], { id: "g1" }), group(["c3", "c4"], { id: "g2" })];

  /**
   * The agent's side is read from the tool descriptor itself (`mcp.rs`, the `action` enum of
   * `manage_command_group`) rather than restated here, so the day a ninth action is added this test
   * goes red instead of drifting. The screen's side is not a list of names either: each entry
   * PERFORMS the gesture on the real component and asserts the state it produced.
   */
  const MCP_ACTIONS = (() => {
    const source = readFileSync(
      join(process.cwd(), "..", "..", "..", "crates", "nyx-core", "src", "mcp.rs"),
      "utf8",
    );
    const at = source.indexOf('"name": MANAGE_COMMAND_GROUP_TOOL');
    const enumMatch = /"enum": \[([^\]]*)\]/.exec(source.slice(at));
    return (enumMatch?.[1] ?? "")
      .split(",")
      .map((piece) => piece.trim().replace(/"/g, ""))
      .filter((piece) => piece.length > 0);
  })();

  /** One HAND gesture per action, each ending on the state the agent's action would produce. */
  const GESTURES: Record<string, () => Promise<void>> = {
    create: async () => {
      const live = renderScreen([], FIVE);
      fireDragEnd(mergeOnto("c1", "c2"));
      await waitFor(() => expect(live.state()[0].command_ids).toEqual(["c2", "c1"]));
    },
    add: async () => {
      const live = renderScreen(TWO_GROUPS, FIVE);
      fireDragEnd(drop("c5", "c1"));
      await waitFor(() => expect(live.state()[0].command_ids).toEqual(["c5", "c1", "c2"]));
      // …and the SECOND group was not touched: the drop designated its target, it did not guess.
      expect(live.state()[1].command_ids).toEqual(["c3", "c4"]);
    },
    remove: async () => {
      const live = renderScreen([group(["c1", "c2", "c3"], { id: "g1" })], FIVE);
      fireDragEnd(drop("c3", "c5"));
      await waitFor(() => expect(live.state()[0].command_ids).toEqual(["c1", "c2"]));
    },
    dissolve: async () => {
      const live = renderScreen([group(["c1", "c2"], { id: "g1" })], FIVE);
      openGroup(live.container);
      fireEvent.click(await screen.findByRole("button", { name: "Ungroup all" }));
      await waitFor(() => expect(live.state()).toEqual([]));
    },
    name: async () => {
      const live = renderScreen([group(["c1", "c2"], { id: "g1" })], FIVE);
      openGroup(live.container);
      const field = (await screen.findByLabelText("Group name (optional)")) as HTMLInputElement;
      fireEvent.change(field, { target: { value: "checks" } });
      fireEvent.blur(field);
      await waitFor(() => expect(live.state()[0].name).toBe("checks"));
    },
    color: async () => {
      const live = renderScreen([group(["c1", "c2"], { id: "g1" })], FIVE);
      openGroup(live.container);
      fireEvent.click(await screen.findByLabelText("Colour 3"));
      await waitFor(() => expect(live.state()[0].color).toBe(3));
    },
    layout: async () => {
      const live = renderScreen([group(["c1", "c2", "c3"], { id: "g1" })], FIVE);
      fireDragEnd(drop("c2", "c1"));
      await waitFor(() => expect(live.state()[0].command_ids).toEqual(["c2", "c1", "c3"]));
    },
    shape: async () => {
      const live = renderScreen([group(["c1", "c2"], { id: "g1" })], FIVE);
      openGroup(live.container);
      fireEvent.click(screen.getByLabelText("Layout main left"));
      await waitFor(() => expect(live.state()[0].layout_shape).toBe("main-left"));
    },
  };

  it("the two lists are the same, name for name", () => {
    expect(MCP_ACTIONS.length).toBeGreaterThan(0);
    expect(Object.keys(GESTURES).sort()).toEqual([...MCP_ACTIONS].sort());
  });

  it.each(Object.keys(GESTURES))("the screen really performs '%s'", async (action) => {
    await GESTURES[action]();
  });
});

describe("CommandPaneHost — the rendered view offers NO composition", () => {
  const services = [service("c1"), service("c2"), service("c3")];

  it("lays the members out side by side, in disposition order", () => {
    const { container } = render(
      <CommandPaneHost group={group(["c1", "c2", "c3"])} services={services} />,
    );
    const panes = Array.from(container.querySelectorAll("[data-command-pane-id]")).map((pane) => {
      const el = pane as HTMLElement;
      return [el.dataset.commandId, el.style.left, el.style.top, el.style.width, el.style.height];
    });
    expect(panes).toEqual([
      ["c1", "0%", "0%", "50%", "100%"],
      ["c2", "50%", "0%", "50%", "50%"],
      ["c3", "50%", "50%", "50%", "50%"],
    ]);
  });

  it("renders NO split control and NO GRABBABLE separator — nothing a pointer could re-arrange", () => {
    const { container } = render(
      <CommandPaneHost group={group(["c1", "c2", "c3"])} services={services} />,
    );
    // The terminals' host renders these; the commands' one must not.
    expect(container.querySelector('[aria-label="Split pane right"]')).toBeNull();
    expect(container.querySelector('[aria-label="Split pane down"]')).toBeNull();
    expect(container.querySelector('[aria-label="Close pane"]')).toBeNull();
    expect(container.querySelector("[data-separator-id]")).toBeNull();
    expect(container.querySelector('[role="separator"]')).toBeNull();
    // …and no pane is draggable either.
    expect(container.querySelector("[draggable='true']")).toBeNull();
    for (const pane of container.querySelectorAll("[data-command-pane-id]")) {
      expect((pane as HTMLElement).onpointerdown).toBeNull();
      expect((pane as HTMLElement).onmousedown).toBeNull();
    }
  });

  /**
   * THE SEAM IS DRAWN, AND ONLY DRAWN (review 01M0AT7KJV4G3C6GQNB36AV3N4, orchestrator arbitrage).
   * The delimitation is kept — two adjacent command panes must be told apart exactly like two
   * terminals — and the MANIPULATION is dropped, because "no pointer gesture changes the layout"
   * and "a draggable separator" cannot both be true. What the seam must NOT have is everything a
   * user could read as an invitation: a role, a tab stop, a resize cursor, a pointer handler.
   */
  describe("the seam between two command panes", () => {
    it("is DRAWN: one inert hairline per split, positioned on the boundary", () => {
      const { container } = render(
        <CommandPaneHost group={group(["c1", "c2", "c3"])} services={services} />,
      );
      const seams = Array.from(container.querySelectorAll<HTMLElement>("[data-command-seam]"));
      // Three panes ⇒ two splits ⇒ two seams, on the boundaries the layout puts them.
      expect(seams).toHaveLength(2);
      expect(seams.map((seam) => seam.dataset.direction)).toEqual(["row", "column"]);
      expect(seams[0].style.left).toBe("calc(50% - 0.5px)");
      expect(seams[1].style.top).toBe("calc(50% - 0.5px)");
      // …and it is painted with the terminals' resting trait token, not a colour of its own.
      expect(seams[0].className).toContain("bg-ring/85");
    });

    it("is NOT interactive: no role, no tab stop, no resize cursor, no pointer handler", () => {
      const { container } = render(
        <CommandPaneHost group={group(["c1", "c2", "c3"])} services={services} />,
      );
      // Swept over the WHOLE view, not just the seams: the criterion is that nothing in the
      // commands' render offers a resize.
      for (const el of container.querySelectorAll<HTMLElement>("*")) {
        expect(el.getAttribute("role")).not.toBe("separator");
        expect(el.getAttribute("tabindex")).toBeNull();
        expect(el.className).not.toMatch(/cursor-(col|row)-resize/);
        expect(el.onpointerdown).toBeNull();
        expect(el.onmousedown).toBeNull();
        expect(el.onpointermove).toBeNull();
      }
      // The seams themselves are out of the hit-testing and out of the a11y tree entirely.
      for (const seam of container.querySelectorAll<HTMLElement>("[data-command-seam]")) {
        expect(seam.className).toContain("pointer-events-none");
        expect(seam.getAttribute("aria-hidden")).toBe("true");
      }
    });

    it("does not move under a pointer gesture — the rectangles are identical before and after", () => {
      const { container } = render(
        <CommandPaneHost group={group(["c1", "c2", "c3"])} services={services} />,
      );
      const rects = () =>
        Array.from(container.querySelectorAll<HTMLElement>("[data-command-pane-id]")).map(
          (pane) => [
            pane.dataset.commandId,
            pane.style.left,
            pane.style.top,
            pane.style.width,
            pane.style.height,
          ],
        );
      const before = rects();
      const seam = container.querySelector<HTMLElement>("[data-command-seam]") as HTMLElement;
      // A full press → drag → release across the seam, plus the mouse equivalent.
      fireEvent.pointerDown(seam, { clientX: 450, clientY: 300, pointerId: 1 });
      fireEvent.pointerMove(seam, { clientX: 700, clientY: 300, pointerId: 1 });
      fireEvent.pointerUp(seam, { clientX: 700, clientY: 300, pointerId: 1 });
      fireEvent.mouseDown(seam, { clientX: 450, clientY: 300 });
      fireEvent.mouseMove(seam, { clientX: 700, clientY: 300 });
      fireEvent.mouseUp(seam, { clientX: 700, clientY: 300 });
      expect(rects()).toEqual(before);
    });
  });

  /**
   * A FAILED PANE NAMES ITSELF (review 01M0ASWCH977S4J4XCC0B8N89N). `commandRunPhaseLabel` could
   * already say "Finished with an error — exit N"; the only production caller passed no options and
   * was mounted for `idle` alone, so three of the five phases had no display path at all — and in a
   * group of commands side by side, the user saw raw outputs with no way to tell which one failed.
   */
  describe("each pane names its run phase", () => {
    const failed = service("c1", { run_state: "error", exit_code: 2 });
    const ok = service("c2", { run_state: "success", exit_code: 0 });

    it("names the state AND the exit code of a command that finished in error", () => {
      render(<CommandPaneHost group={group(["c1", "c2"])} services={[failed, ok]} />);
      expect(screen.getByText("Finished with an error — exit 2")).toBeTruthy();
      // …and the neighbour says its own thing, so the two are told apart.
      expect(screen.getByText("Finished successfully")).toBeTruthy();
    });

    it("gives running / succeeded / failed a real display path, each pane on its own", () => {
      const { container } = render(
        <CommandPaneHost
          group={group(["c1", "c2", "c3"])}
          services={[failed, ok, service("c3", { run_state: "running" })]}
        />,
      );
      const phases = Array.from(
        container.querySelectorAll<HTMLElement>("[data-command-pane-phase]"),
      ).map((el) => el.dataset.commandPanePhase);
      expect(phases).toEqual(["failed", "succeeded", "running"]);
      expect(screen.getByText("Running…")).toBeTruthy();
    });

    it("keeps the idle empty state, and hands IT the exit code too", () => {
      // `idle` still renders `<CommandRunEmpty>` — unchanged — but its label call now carries the
      // exit code, so no phase can reach a label that silently drops half its sentence.
      render(
        <CommandPaneHost
          group={group(["c1", "c2"])}
          services={[service("c1", { run_state: "idle" }), ok]}
        />,
      );
      expect(screen.getByText(/press Start to run it/i)).toBeTruthy();
    });
  });

  it("skips a member with no instance in THIS workspace, and shows one member full frame", () => {
    const { container } = render(
      <CommandPaneHost group={group(["c1", "c2", "c3"])} services={[service("c1")]} />,
    );
    // One live member is not a tiling: it fills the frame, with no pane boxes at all.
    expect(container.querySelectorAll("[data-command-pane-id]")).toHaveLength(0);
    expect(container.querySelector("[data-command-pane-host]")).not.toBeNull();
  });
});

/**
 * A COMMAND and a TERMINAL can never share a group. The two models live over DISJOINT id spaces
 * and disjoint stores — the terminals' collection is keyed on terminal ids in `localStorage`, the
 * commands' one on `managed_commands` ids in the DB — so there is no shape in which they mix.
 * (The backend refusal is pinned in Rust: `a_terminal_can_never_join_a_command_group`.)
 */
describe("commands and terminals never share a group", () => {
  it("the two models have no common store and no common member type", () => {
    const commandGroups = readFileSync(
      join(SRC, "components", "command", "use-command-groups.ts"),
      "utf8",
    );
    const terminalGroups = readFileSync(join(SRC, "components", "panes", "pane-groups.ts"), "utf8");
    // The commands' grouping never touches the terminals' store…
    expect(commandGroups).not.toContain("pane-layout-persistence");
    expect(commandGroups).not.toContain("window.localStorage");
    expect(commandGroups).not.toContain("savePaneCollection");
    // …and the terminals' model never names a command.
    expect(terminalGroups).not.toContain("command_id");
    expect(terminalGroups).not.toContain("managed_command");
  });

  it("a terminal id is simply not a member of any command group", () => {
    const controller = fakeController([group(["c1", "c2"])]);
    expect(controller.groupOf("terminal-42")).toBeNull();
  });
});
