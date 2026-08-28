import { act, render, waitFor } from "@testing-library/react";
import { mockIPC } from "@/bridge/test-harness";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { toast } from "@/components/ui/toast";
import { __resetTerminalStore } from "@/components/terminal/terminal-store";
import type { TerminalRecord } from "@/components/sidebar/use-terminals";
import { savePaneCollection } from "./pane-layout-persistence";
import {
  EMPTY_COLLECTION,
  orderGroupMembers,
  type PaneCollection,
} from "@/components/panes/pane-groups";
import { movePaneToEdge, paneLeaves, type PaneNode } from "@/components/panes/pane-tree";
import { PaneTreeHost } from "./pane-tree-host";
import { RuntimeSelectionProvider } from "./runtime-selection";
import { saveLastTerminalId } from "./ui-persistence";
import { usePaneLayout, type PaneLayoutController, type SplitWorkspace } from "./use-pane-layout";
import type { WorkspaceRuntimeTerminal } from "@/components/runtime/use-workspace-runtime";

// The REFUSAL surface: a move under the floor must SAY so (the split's own rule).
vi.mock("@/components/ui/toast", () => ({
  toast: { error: vi.fn(), success: vi.fn(), info: vi.fn() },
}));

/**
 * THE VIEW'S RESTRUCTURE GESTURE — the unit half of review 01M0CY7R5Z7KV5R49FQYXH9QRN.
 *
 * What jsdom CAN prove, it proves here: the pure tree operation (`movePaneToEdge` — four edges,
 * perpendicular direction, 50/50, the moved leaf keeping its very object), the controller's floor
 * arbitration (`canMovePane` / `movePane` on an INJECTED host size), the zero-creation guarantee
 * (`createTerminal` never called), the focus following the moved pane, and the coexistence with
 * the sidebar's permutation (`withMemberOrder`, untouched). The GEOMETRIC half — real rectangles,
 * `elementFromPoint`, PTY sizes after the gesture — lives in `pane-move.browser.test.tsx`.
 */

/** `ta | tb` — two panes side by side, stable ids so every assertion can name them. */
const TREE: PaneNode = {
  kind: "split",
  id: "s-root",
  direction: "row",
  ratio: 0.5,
  children: [
    { kind: "leaf", id: "p-a", terminalId: "ta" },
    { kind: "leaf", id: "p-b", terminalId: "tb" },
  ],
};

describe("movePaneToEdge (the pure tree operation)", () => {
  // A TWO-PANE tree compacts to ONE leaf when the dragged pane detaches, so the RESULT is the new
  // split itself — the assertion surface for direction / ratio / side.
  it("LEFT edge: the moved leaf becomes the FIRST child of a new row split at 50/50", () => {
    const next = movePaneToEdge(TREE, "p-b", "p-a", "left");
    expect(next).not.toBeNull();
    expect(next).toMatchObject({ kind: "split", direction: "row", ratio: 0.5 });
    expect(paneLeaves(next as PaneNode).map((l) => l.terminalId)).toEqual(["tb", "ta"]);
  });

  it("RIGHT edge: same row split, the moved leaf SECOND", () => {
    const next = movePaneToEdge(TREE, "p-b", "p-a", "right") as PaneNode;
    expect(next).toMatchObject({ kind: "split", direction: "row", ratio: 0.5 });
    expect(paneLeaves(next).map((l) => l.terminalId)).toEqual(["ta", "tb"]);
  });

  it("UP edge: the split is PERPENDICULAR to the horizontal one — a column, moved leaf first", () => {
    const next = movePaneToEdge(TREE, "p-b", "p-a", "up") as PaneNode;
    expect(next).toMatchObject({ kind: "split", direction: "column", ratio: 0.5 });
    expect(paneLeaves(next).map((l) => l.terminalId)).toEqual(["tb", "ta"]);
  });

  it("DOWN edge: column split, moved leaf second", () => {
    const next = movePaneToEdge(TREE, "p-b", "p-a", "down") as PaneNode;
    expect(next).toMatchObject({ kind: "split", direction: "column", ratio: 0.5 });
    expect(paneLeaves(next).map((l) => l.terminalId)).toEqual(["ta", "tb"]);
  });

  it("INSIDE a nested tree, only the target's slot changes — the rest of the tree keeps its nodes", () => {
    // row[ col[ta / tb] , tc ]
    const nested: PaneNode = {
      kind: "split",
      id: "s-root",
      direction: "row",
      ratio: 0.5,
      children: [
        {
          kind: "split",
          id: "s-left",
          direction: "column",
          ratio: 0.4,
          children: [
            { kind: "leaf", id: "p-a", terminalId: "ta" },
            { kind: "leaf", id: "p-b", terminalId: "tb" },
          ],
        },
        { kind: "leaf", id: "p-c", terminalId: "tc" },
      ],
    };
    const next = movePaneToEdge(nested, "p-c", "p-a", "left") as Extract<
      PaneNode,
      { kind: "split" }
    >;
    // tc detaches (the root row compacts onto the left column), then splits p-a's slot.
    expect(next.direction).toBe("column");
    expect(next.id).toBe("s-left");
    expect(next.ratio).toBe(0.4);
    const [top, bottom] = next.children;
    expect(top).toMatchObject({ kind: "split", direction: "row", ratio: 0.5 });
    expect(paneLeaves(top).map((l) => l.terminalId)).toEqual(["tc", "ta"]);
    expect(bottom).toMatchObject({ kind: "leaf", id: "p-b", terminalId: "tb" });
    expect(paneLeaves(next).map((l) => l.terminalId)).toEqual(["tc", "ta", "tb"]);
  });

  it("the moved leaf keeps its very OBJECT — same pane id, same terminalId (no recreation)", () => {
    const next = movePaneToEdge(TREE, "p-b", "p-a", "left") as PaneNode;
    const moved = paneLeaves(next).find((l) => l.terminalId === "tb");
    expect(moved).toBe(TREE.children[1]);
    expect(moved?.id).toBe("p-b");
  });

  it("REFUSES: same pane twice, unknown ids, a one-leaf root — and never touches the input", () => {
    expect(movePaneToEdge(TREE, "p-a", "p-a", "left")).toBeNull();
    expect(movePaneToEdge(TREE, "p-zz", "p-a", "left")).toBeNull();
    expect(movePaneToEdge(TREE, "p-b", "p-zz", "left")).toBeNull();
    expect(
      movePaneToEdge({ kind: "leaf", id: "p-a", terminalId: "ta" }, "p-a", "p-a", "up"),
    ).toBeNull();
    // The input tree is untouched by every refusal AND by every successful move.
    expect(paneLeaves(TREE).map((l) => [l.id, l.terminalId])).toEqual([
      ["p-a", "ta"],
      ["p-b", "tb"],
    ]);
  });
});

/* ────────────────────────────── THE CONTROLLER ────────────────────────────── */

function terminal(id: string): WorkspaceRuntimeTerminal {
  return {
    id,
    cwd: `/repo/${id}`,
    label: null,
    workspace_id: "ws-root",
    status: "alive",
    exec_state: "idle",
    exec_state_unread: false,
    sessions: [],
  };
}

const WORKSPACES: SplitWorkspace[] = [{ id: "ws-root", path: "/repo", is_root: true }];

let controller: PaneLayoutController;
let selectedId: string | null;

function Harness({
  terminals,
  createTerminal,
  measureHost,
}: {
  terminals: WorkspaceRuntimeTerminal[];
  createTerminal: (workspaceId: string, path: string) => Promise<string>;
  measureHost: () => { width: number; height: number } | null;
}) {
  controller = usePaneLayout({
    projectId: "p-move",
    terminals,
    workspaces: WORKSPACES,
    createTerminal,
    measureHost,
  });
  const selection = useRuntimeSelectionSafe();
  selectedId = selection;
  return <div data-visible={controller.visibleTerminalIds.join(",")} />;
}

// Tiny indirection so the harness stays a function component (no hooks outside).
import { useRuntimeSelection } from "./runtime-selection";
function useRuntimeSelectionSafe(): string | null {
  const { selected } = useRuntimeSelection();
  return selected?.kind === "terminal" ? selected.id : null;
}

function renderMoveHarness(
  measureHost: () => { width: number; height: number } | null,
  createTerminal = vi.fn(async () => "never-created"),
) {
  function Wrapper() {
    return (
      <RuntimeSelectionProvider>
        <Harness
          terminals={[terminal("ta"), terminal("tb")]}
          createTerminal={createTerminal}
          measureHost={measureHost}
        />
      </RuntimeSelectionProvider>
    );
  }
  render(<Wrapper />);
  return { createTerminal };
}

/** Seed the project with the ONE group holding TREE, remembered on `ta` so the restore shows it. */
function seedGroup(): void {
  savePaneCollection("p-move", {
    groups: [{ id: "g1", tree: TREE, name: null, color: null }],
  });
  saveLastTerminalId("p-move", "ta");
}

const groupTree = (): PaneNode => controller.groups[0].tree;

beforeEach(() => {
  window.localStorage.clear();
  vi.mocked(toast.error).mockClear();
});

afterEach(() => {
  window.localStorage.clear();
  __resetTerminalStore();
});

describe("usePaneLayout.movePane / canMovePane", () => {
  it("a VALID drop restructures the tree, focuses the moved pane — and creates NO terminal", async () => {
    seedGroup();
    const { createTerminal } = renderMoveHarness(
      () => ({ width: 800, height: 700 }),
      vi.fn(async () => "never-created"),
    );
    await waitFor(() => expect(controller.focusedPaneId).toBe("p-a"));

    act(() => controller.movePane("p-b", "p-a", "left"));

    expect(paneLeaves(groupTree()).map((l) => l.terminalId)).toEqual(["tb", "ta"]);
    expect(controller.focusedPaneId, "the focus follows the moved pane").toBe("p-b");
    expect(selectedId, "the sidebar follows too").toBe("tb");
    expect(createTerminal, "a move NEVER creates a terminal").not.toHaveBeenCalled();
  });

  it("the floor REFUSES a drop that would seat a pane under 180×120px — tree untouched, message shown", async () => {
    seedGroup();
    // 200px of height: an UP/DOWN drop seats two 100px-tall panes — under the absolute floor.
    const { createTerminal } = renderMoveHarness(() => ({ width: 800, height: 200 }));
    await waitFor(() => expect(controller.focusedPaneId).toBe("p-a"));
    const before = groupTree();

    expect(controller.canMovePane("p-b", "p-a", "up")).toBe(false);
    act(() => controller.movePane("p-b", "p-a", "up"));

    expect(groupTree(), "the tree is unchanged, by reference").toBe(before);
    expect(toast.error, "the refusal is announced, never silent").toHaveBeenCalledTimes(1);
    expect(createTerminal).not.toHaveBeenCalled();

    // The SAME gesture on the OTHER axis fits (400px halves ≥ 180px): the refusal is geometric.
    expect(controller.canMovePane("p-b", "p-a", "left")).toBe(true);
  });

  it("an UNMEASURED host cannot refuse — the floor is a floor, not a gate", async () => {
    seedGroup();
    renderMoveHarness(() => null);
    await waitFor(() => expect(controller.focusedPaneId).toBe("p-a"));
    expect(controller.canMovePane("p-b", "p-a", "left")).toBe(true);
  });

  it("coexists with the SIDEBAR permutation: after a view move, withMemberOrder still permutes occupants without touching structure or ratios", async () => {
    seedGroup();
    renderMoveHarness(() => ({ width: 800, height: 700 }));
    await waitFor(() => expect(controller.focusedPaneId).toBe("p-a"));

    act(() => controller.movePane("p-b", "p-a", "left"));
    const afterMove = groupTree();
    const shapeOf = (node: PaneNode) =>
      JSON.stringify(node, (key, value) => (key === "terminalId" ? "<occupant>" : value));

    act(() => controller.reorderGroupMembers("g1", ["ta", "tb"]));

    // STRUCTURE frozen: identical node ids, directions and ratios — only WHICH terminal sits in
    // which rectangle changed. That is `withMemberOrder`'s contract, unchanged by this feature.
    expect(shapeOf(groupTree())).toBe(shapeOf(afterMove));
    expect(paneLeaves(groupTree()).map((l) => l.terminalId)).toEqual(["ta", "tb"]);

    // …and the pure mutator itself still answers exactly as before: the tree comes back BY
    // REFERENCE when the asked order already holds, and a foreign id / wrong length is refused.
    const collection: PaneCollection = {
      groups: [{ id: "g1", tree: afterMove, name: null, color: null }],
    };
    expect(orderGroupMembers(collection, "g1", ["tb", "ta"])).toBe(collection);
    expect(orderGroupMembers(collection, "g1", ["ta", "zz"])).toBe(collection);
  });
});

/* ─────────────── COMMANDS ARE NOT AFFECTED (criterion 7, structural half) ───────────────
 *
 * The drag affordance exists ONLY where the handlers are wired. `<PaneTreeHost>` without
 * `onMovePane` / `canMovePane` renders no grip at all — which is exactly how the COMMAND groups'
 * surface stays out of scope: `RuntimePaneHost` passes the handlers only to the terminals' host,
 * while a selected service renders through `<CommandPaneHost>`, whose disposition is DERIVED from
 * the settings' ordered member list (`autoLayoutTree`) and receives nothing to drag with.
 */

function record(id: string): TerminalRecord {
  return {
    id,
    cwd: `/repo/${id}`,
    label: null,
    scrollback: "",
    status: "alive",
    order_index: 0,
    created_at: 0,
    updated_at: 0,
    closed_at: null,
  };
}

/** The same two-pane shape, over the records' own terminal ids (`t1 | t2`). */
const UI_TREE: PaneNode = {
  kind: "split",
  id: "s-root",
  direction: "row",
  ratio: 0.5,
  children: [
    { kind: "leaf", id: "p-a", terminalId: "t1" },
    { kind: "leaf", id: "p-b", terminalId: "t2" },
  ],
};

describe("the drag affordance is opt-in per surface", () => {
  beforeEach(() => {
    let nextPtyId = 100;
    mockIPC(
      (cmd, args) => {
        const a = (args ?? {}) as Record<string, unknown>;
        if (cmd === "pty_spawn") return nextPtyId++;
        if (cmd === "pty_close") return null;
        void cmd;
        void a;
        return null;
      },
      { shouldMockEvents: true },
    );
  });

  afterEach(() => {
    __resetTerminalStore();
  });

  it("no handlers ⇒ no grip, and a pointer sweep shows no indicator", async () => {
    const { container } = render(
      <PaneTreeHost
        terminals={[record("t1"), record("t2")]}
        layout={UI_TREE}
        focusedPaneId="p-a"
        onFocusPane={() => {}}
        onSplitPane={() => {}}
        onClosePane={() => {}}
        onResizeSplit={() => {}}
      />,
    );
    await waitFor(() =>
      expect(container.querySelectorAll("[data-visible='true']")).toHaveLength(2),
    );
    expect(container.querySelectorAll("[data-pane-grip]"), "no prise, no lie").toHaveLength(0);
    // Even a synthetic pointer sequence over another pane's edge band produces nothing.
    const host = container.querySelector("[data-pane-host]") as HTMLElement;
    const target = container.querySelector("[data-terminal-id='t2']") as HTMLElement;
    const box = target.getBoundingClientRect();
    host.dispatchEvent(
      new PointerEvent("pointermove", {
        bubbles: true,
        clientX: box.left + box.width * 0.1,
        clientY: box.top + box.height / 2,
      }),
    );
    expect(container.querySelectorAll("[data-pane-drop-indicator]")).toHaveLength(0);
  });

  it("handlers wired but ONE pane ⇒ still no grip (nothing to restructure)", async () => {
    const { container } = render(
      <PaneTreeHost
        terminals={[record("t1")]}
        layout={{ kind: "leaf", id: "p-a", terminalId: "t1" }}
        focusedPaneId="p-a"
        onFocusPane={() => {}}
        onSplitPane={() => {}}
        onClosePane={() => {}}
        onMovePane={() => {}}
        canMovePane={() => true}
      />,
    );
    await waitFor(() =>
      expect(container.querySelectorAll("[data-visible='true']")).toHaveLength(1),
    );
    expect(container.querySelectorAll("[data-pane-grip]")).toHaveLength(0);
  });
});

/** Referenced so the empty-collection import earns its keep in the seeding helpers above. */
void EMPTY_COLLECTION;
