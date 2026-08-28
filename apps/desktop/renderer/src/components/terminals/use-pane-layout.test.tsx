import { act, render, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { toast } from "@/components/ui/toast";
import { loadPaneCollection, savePaneCollection } from "./pane-layout-persistence";
import {
  EMPTY_COLLECTION,
  groupMemberIds,
  type PaneCollection,
} from "@/components/panes/pane-groups";
import {
  createPaneLayout,
  findPane,
  MAX_SPLIT_RATIO,
  MAX_STORED_RATIO,
  MIN_SPLIT_RATIO,
  MIN_STORED_RATIO,
  paneCount,
  paneLeaves,
  paneTerminalIds,
  splitPane,
  type PaneNode,
  type PaneSplit,
} from "@/components/panes/pane-tree";
import {
  MIN_PANE_HEIGHT_PX,
  MIN_PANE_WIDTH_PX,
  paneRects,
  paneSeparators,
  type HostSize,
} from "@/components/panes/pane-geometry";
import { RuntimeSelectionProvider, useRuntimeSelection } from "./runtime-selection";
import { saveLastTerminalId } from "./ui-persistence";
import {
  displayTree,
  resolveSplitWorkspace,
  showTerminal,
  usePaneLayout,
  withFocus,
  type PaneLayoutController,
  type SplitWorkspace,
} from "./use-pane-layout";
import type { WorkspaceRuntimeTerminal } from "@/components/runtime/use-workspace-runtime";

// The REFUSAL surface: a split with no workspace must SAY so, not fail silently.
vi.mock("@/components/ui/toast", () => ({
  toast: { error: vi.fn(), success: vi.fn(), info: vi.fn() },
}));

function terminal(id: string, workspaceId: string | null): WorkspaceRuntimeTerminal {
  return {
    id,
    cwd: `/repo/${id}`,
    label: null,
    workspace_id: workspaceId,
    status: "alive",
    exec_state: "idle",
    exec_state_unread: false,
    sessions: [],
  };
}

const WORKSPACES: SplitWorkspace[] = [
  { id: "ws-root", path: "/repo", is_root: true },
  { id: "ws-feat", path: "/repo-feat", is_root: false },
];

/** The live controller of the last render, plus the live selection (and its setter). */
let controller: PaneLayoutController;
let selectedId: string | null;
/** The sidebar's own gesture: what a row click calls. */
let selectRuntime: (selection: { kind: "terminal"; id: string }) => void;

function Harness({
  projectId = "p1",
  terminals,
  workspaces = WORKSPACES,
  createTerminal = creator(),
  // jsdom has NO layout engine — every box is 0×0 — so the absolute pane floor cannot measure
  // anything and never refuses. Tests that exercise it hand the real host size in.
  measureHost = () => null,
  // WHICH workspaces answered the runtime read. Omitted ⇒ no coverage claim, i.e. the flat
  // terminal list is taken as exhaustive — what every test written before the partial-snapshot
  // fix assumed, so their bodies are untouched.
  resolvedWorkspaceIds,
}: {
  projectId?: string;
  terminals: WorkspaceRuntimeTerminal[];
  workspaces?: SplitWorkspace[];
  createTerminal?: (workspaceId: string, path: string) => Promise<string>;
  measureHost?: () => HostSize | null;
  resolvedWorkspaceIds?: ReadonlySet<string> | null;
}) {
  controller = usePaneLayout({
    projectId,
    terminals,
    workspaces,
    createTerminal,
    measureHost,
    resolvedWorkspaceIds,
  });
  const { selected, select } = useRuntimeSelection();
  selectedId = selected?.kind === "terminal" ? selected.id : null;
  selectRuntime = select;
  return <div data-visible={controller.visibleTerminalIds.join(",")} />;
}

function renderHook(props: Parameters<typeof Harness>[0]) {
  return render(
    <RuntimeSelectionProvider>
      <Harness {...props} />
    </RuntimeSelectionProvider>,
  );
}

/**
 * Seed a project with ONE group holding `tree` — the v2 shape a project with a tiling has.
 * A project whose terminals are all solo simply has NO group, which is `null` here.
 */
function savePaneLayout(projectId: string, tree: PaneNode | null): void {
  savePaneCollection(
    projectId,
    tree == null ? EMPTY_COLLECTION : { groups: [{ id: "g1", tree, name: null, color: null }] },
  );
}
/** The tree of the ONE stored group, or `null` when the project has no group at all. */
function loadPaneLayout(projectId: string): PaneNode | null {
  return loadPaneCollection(projectId)?.groups[0]?.tree ?? null;
}
/** The whole stored collection (for the assertions that are about the GROUPS, not one tree). */
function loadCollection(projectId: string): PaneCollection | null {
  return loadPaneCollection(projectId);
}

/** The default `createTerminal`: hands back a fresh record id and records the call. */
function creator(id = "new-term") {
  return vi.fn(async (_workspaceId: string, _path: string) => id);
}

beforeEach(() => {
  window.localStorage.clear();
  vi.mocked(toast.error).mockClear();
});

afterEach(() => {
  window.localStorage.clear();
});

describe("resolveSplitWorkspace", () => {
  it("uses the focused terminal's OWN workspace", () => {
    expect(resolveSplitWorkspace(terminal("t1", "ws-feat"), WORKSPACES)).toEqual({
      id: "ws-feat",
      path: "/repo-feat",
    });
  });

  it("falls back to the project ROOT workspace for a loose or STALE binding", () => {
    const root = { id: "ws-root", path: "/repo" };
    expect(resolveSplitWorkspace(terminal("t1", null), WORKSPACES)).toEqual(root);
    expect(resolveSplitWorkspace(terminal("t1", "deleted-ws"), WORKSPACES)).toEqual(root);
    expect(resolveSplitWorkspace(null, WORKSPACES)).toEqual(root);
  });

  it("falls back to the FIRST workspace when none is flagged root", () => {
    const noRoot: SplitWorkspace[] = [{ id: "ws-a", path: "/a", is_root: false }];
    expect(resolveSplitWorkspace(terminal("t1", null), noRoot)).toEqual({ id: "ws-a", path: "/a" });
  });

  it("returns null when the project has NO workspace at all", () => {
    expect(resolveSplitWorkspace(terminal("t1", "ws-root"), [])).toBeNull();
  });
});

describe("withFocus", () => {
  const tree = splitPane(createPaneLayout("t1", "p1"), {
    paneId: "p1",
    direction: "row",
    terminalId: "t2",
    newPaneId: "p2",
  }) as PaneNode;
  const collection: PaneCollection = { groups: [{ id: "g1", tree, name: null, color: null }] };
  const shown = { kind: "group", groupId: "g1" } as const;

  it("keeps a live pane, else falls back to the first, else nothing", () => {
    expect(withFocus(collection, shown, "p2").focusedPaneId).toBe("p2");
    expect(withFocus(collection, shown, "gone").focusedPaneId).toBe("p1");
    expect(withFocus(collection, null, "p1")).toEqual({
      collection,
      display: null,
      focusedPaneId: null,
    });
    // A display naming a group that no longer exists is the same as no display at all.
    expect(withFocus(EMPTY_COLLECTION, shown, "p1").focusedPaneId).toBeNull();
  });
});

/**
 * THE ONE NAVIGATION RULE (review 01M0ABR6V501D0SPR2BYBVZC4A, critical). Every entry point — the
 * sidebar click, the deep-link, the restore — resolves through `showTerminal`, so none of them
 * can invent a different behaviour.
 */
describe("showTerminal", () => {
  const tree = splitPane(createPaneLayout("t1", "p1"), {
    paneId: "p1",
    direction: "row",
    terminalId: "t2",
    newPaneId: "p2",
  }) as PaneNode;
  const collection: PaneCollection = { groups: [{ id: "g1", tree, name: null, color: null }] };

  it("shows a MEMBER's whole group and focuses THAT member", () => {
    expect(showTerminal(collection, "t2")).toEqual({
      display: { kind: "group", groupId: "g1" },
      focusedPaneId: "p2",
    });
    expect(
      paneTerminalIds(displayTree(collection, showTerminal(collection, "t2").display)),
    ).toEqual(["t1", "t2"]);
  });

  it("shows a SOLO terminal ALONE, and leaves every group untouched BY REFERENCE", () => {
    const shown = showTerminal(collection, "t9");
    expect(shown.display.kind).toBe("solo");
    const painted = displayTree(collection, shown.display) as PaneNode;
    expect(paneTerminalIds(painted)).toEqual(["t9"]);
    expect(painted.kind).toBe("leaf");
    // The group's tree is the SAME object — showing a solo terminal is not a layout mutation.
    expect(collection.groups[0].tree).toBe(tree);
  });

  it("REUSES the solo node when the same terminal is shown again (a stable pane id)", () => {
    const first = showTerminal(collection, "t9");
    const again = showTerminal(collection, "t9", first.display);
    expect(again.display).toBe(first.display);
    expect(again.focusedPaneId).toBe(first.focusedPaneId);
  });
});

describe("usePaneLayout — restore", () => {
  it("restores the project's persisted tiling once the terminals are live", async () => {
    savePaneLayout(
      "p1",
      splitPane(createPaneLayout("t1", "p1"), {
        paneId: "p1",
        direction: "row",
        terminalId: "t2",
        newPaneId: "p2",
      }) as PaneNode,
    );
    renderHook({ terminals: [terminal("t1", "ws-root"), terminal("t2", "ws-root")] });
    await waitFor(() => expect(controller.visibleTerminalIds).toEqual(["t1", "t2"]));
    expect(controller.focusedPaneId).toBe("p1");
  });

  it("falls back to a single pane on the remembered terminal when nothing is persisted", async () => {
    saveLastTerminalId("p1", "t2");
    renderHook({ terminals: [terminal("t1", "ws-root"), terminal("t2", "ws-root")] });
    await waitFor(() => expect(controller.visibleTerminalIds).toEqual(["t2"]));
  });

  it("does not persist an empty layout over the stored one before the restore", async () => {
    savePaneLayout(
      "p1",
      splitPane(createPaneLayout("t1", "p1"), {
        paneId: "p1",
        direction: "row",
        terminalId: "t2",
        newPaneId: "p2",
      }) as PaneNode,
    );
    // Mount with NO live terminal: the restore has nothing to reconcile against and must leave the
    // stored tree alone (a wipe here would lose the tiling on every slow snapshot).
    renderHook({ terminals: [] });
    await act(async () => {
      await Promise.resolve();
    });
    expect(window.localStorage.getItem("nyx.ui.paneLayout:p1")).toContain('"terminalId":"t1"');
  });

  it("does not wipe the stored tiling when the runtime snapshot goes EMPTY *after* the restore", async () => {
    // The sidecar going away is INDISTINGUISHABLE from "no terminals" at the page level:
    // `useProjectRuntime` silently omits every rejected `workspace_runtime_snapshot`, so the flat
    // terminal list falls to [] while the terminals are still running. Reconciling against that
    // absence used to prune every leaf, collapse the tree to null and `removeItem` the project's
    // layout — and the restore never replays, so the tiling was lost for good.
    const twoPanes = splitPane(createPaneLayout("t1", "p1"), {
      paneId: "p1",
      direction: "row",
      terminalId: "t2",
      newPaneId: "p2",
    }) as PaneNode;
    savePaneLayout("p1", twoPanes);
    const live = [terminal("t1", "ws-root"), terminal("t2", "ws-root")];

    const { rerender } = renderHook({ terminals: live });
    await waitFor(() => expect(controller.visibleTerminalIds).toEqual(["t1", "t2"]));

    // BLACKOUT: every workspace snapshot fails at once.
    rerender(
      <RuntimeSelectionProvider>
        <Harness terminals={[]} />
      </RuntimeSelectionProvider>,
    );
    await act(async () => {
      await Promise.resolve();
    });
    expect(loadPaneLayout("p1")).toEqual(twoPanes);
    expect(paneCount(controller.layout as PaneNode)).toBe(2);

    // …and the terminals come back: the same two panes, still persisted, no single-pane rebuild.
    rerender(
      <RuntimeSelectionProvider>
        <Harness terminals={live} />
      </RuntimeSelectionProvider>,
    );
    await act(async () => {
      await Promise.resolve();
    });
    expect(controller.visibleTerminalIds).toEqual(["t1", "t2"]);
    expect(loadPaneLayout("p1")).toEqual(twoPanes);
    expect(paneCount(loadPaneLayout("p1") as PaneNode)).toBe(2);
  });

  /**
   * A PARTIAL snapshot is the same absence of signal as a blank one, one notch finer.
   * `use-project-runtime.ts:66` drops a REJECTED `workspace_runtime_snapshot` BY DESIGN ("a single
   * workspace failure must not blank the others"), so one failing workspace leaves the flat list
   * INCOMPLETE, never empty — the `liveIds.length === 0` guard never fires. Reconciling against
   * that half-truth pruned the silent workspace's leaves, dissolved the group and `removeItem`-ed
   * the key; the restore being one-shot, the tiling was gone for good. Reachable from the backend:
   * `db_command.rs:936` answers `unknown workspace {id}` for ONE workspace while the others serve.
   */
  describe("a PARTIAL runtime snapshot never prunes the workspaces that stayed silent", () => {
    const twoPanes = splitPane(createPaneLayout("t1", "p1"), {
      paneId: "p1",
      direction: "row",
      terminalId: "t2",
      newPaneId: "p2",
    }) as PaneNode;
    const BOTH = new Set(["ws-root", "ws-feat"]);

    function remount(
      terminals: WorkspaceRuntimeTerminal[],
      resolvedWorkspaceIds: ReadonlySet<string>,
      rerender: ReturnType<typeof renderHook>["rerender"],
    ) {
      rerender(
        <RuntimeSelectionProvider>
          <Harness terminals={terminals} resolvedWorkspaceIds={resolvedWorkspaceIds} />
        </RuntimeSelectionProvider>,
      );
    }

    it("C1 — a workspace that fails ONCE after the restore keeps its group, in memory and in localStorage", async () => {
      savePaneLayout("p1", twoPanes);
      const t1 = terminal("t1", "ws-root");
      const t2 = terminal("t2", "ws-root");
      const t3 = terminal("t3", "ws-feat");

      const { rerender } = renderHook({
        terminals: [t1, t2, t3],
        resolvedWorkspaceIds: BOTH,
      });
      await waitFor(() => expect(controller.groups.map(groupMemberIds)).toEqual([["t1", "t2"]]));

      // ws-root REJECTS: t1/t2 leave the flat list, ws-feat still answers with t3.
      remount([t3], new Set(["ws-feat"]), rerender);
      await act(async () => {
        await Promise.resolve();
      });
      // The RAW key, first: the loss is an `removeItem`, and it is immediate.
      expect(window.localStorage.getItem("nyx.ui.paneLayout:p1")).not.toBeNull();
      expect(controller.groups.map(groupMemberIds)).toEqual([["t1", "t2"]]);
      expect(loadPaneLayout("p1")).toEqual(twoPanes);

      // …and ws-root recovers: the same group, still persisted, never rebuilt.
      remount([t1, t2, t3], BOTH, rerender);
      await act(async () => {
        await Promise.resolve();
      });
      expect(controller.groups.map(groupMemberIds)).toEqual([["t1", "t2"]]);
      expect(loadPaneLayout("p1")).toEqual(twoPanes);
    });

    it("C2 — a FIRST snapshot that is already partial restores the whole stored group", async () => {
      savePaneLayout("p1", twoPanes);
      const t1 = terminal("t1", "ws-root");
      // t2 lives in ws-feat, whose snapshot rejected on the very first read.
      const t2 = terminal("t2", "ws-feat");

      const { rerender } = renderHook({
        terminals: [t1],
        resolvedWorkspaceIds: new Set(["ws-root"]),
      });
      await waitFor(() => expect(controller.groups.map(groupMemberIds)).toEqual([["t1", "t2"]]));
      expect(loadPaneLayout("p1")).toEqual(twoPanes);

      // ws-feat answers next. The restore is one-shot: a group dropped above would never return.
      remount([t1, t2], BOTH, rerender);
      await act(async () => {
        await Promise.resolve();
      });
      expect(controller.groups.map(groupMemberIds)).toEqual([["t1", "t2"]]);
      expect(loadPaneLayout("p1")).toEqual(twoPanes);
    });

    it("still prunes a terminal whose OWN workspace answered, while another stays silent", async () => {
      // The protection is per-WORKSPACE, not a blanket freeze: ws-root answered and no longer
      // carries t2, so t2 really is gone — its leaf goes and the one-member group dissolves,
      // exactly as on a complete snapshot.
      savePaneLayout("p1", twoPanes);
      const t1 = terminal("t1", "ws-root");
      const t2 = terminal("t2", "ws-root");
      const t3 = terminal("t3", "ws-feat");

      const { rerender } = renderHook({ terminals: [t1, t2, t3], resolvedWorkspaceIds: BOTH });
      await waitFor(() => expect(controller.groups.map(groupMemberIds)).toEqual([["t1", "t2"]]));

      remount([t1], new Set(["ws-root"]), rerender);
      await act(async () => {
        await Promise.resolve();
      });
      expect(controller.groups).toEqual([]);
      expect(loadPaneLayout("p1")).toBeNull();
    });
  });
});

/**
 * WHY THESE FOUR LIVE IN JSDOM AND NOT IN CHROMIUM (review finding, and the answer written down).
 *
 * The PRD's testing decisions name the BROWSER for the split's resolution rules — right workspace,
 * atomic success, refusal without an empty pane, root fallback. They are covered here instead, on
 * purpose: all four are PURE RESOLUTION (`resolveSplitWorkspace` plus the ORDER of the calls —
 * create the terminal first, add the leaf only on success). Not one pixel enters the decision, so
 * a layout engine cannot change any of their outcomes, and jsdom states them in milliseconds where
 * Chromium would only add a rendering it does not exercise.
 *
 * The item of that list that does NOT reduce to resolution — navigation / reload, where a whole
 * tree comes back and every pane must be MEASURED before its PTY opens — is covered in Chromium
 * (`pane-tiling-gate.browser.test.tsx`, the two RESTORE specs), because there the pixels ARE the
 * subject.
 */
describe("usePaneLayout — split", () => {
  it("creates the terminal in the FOCUSED pane's workspace, then adds and focuses its pane", async () => {
    const createTerminal = creator("t2");
    renderHook({ terminals: [terminal("t1", "ws-feat")], createTerminal });
    await waitFor(() => expect(controller.visibleTerminalIds).toEqual(["t1"]));
    const paneId = controller.focusedPaneId as string;

    await act(async () => {
      controller.splitPane(paneId, "row");
    });

    // The terminal was created in the pane's OWN workspace…
    expect(createTerminal).toHaveBeenCalledWith("ws-feat", "/repo-feat");
    // …and the leaf appeared WITH it, focused, next to the original pane.
    await waitFor(() => expect(controller.visibleTerminalIds).toEqual(["t1", "t2"]));
    expect(paneCount(controller.layout as PaneNode)).toBe(2);
    const rects = paneRects(controller.layout as PaneNode);
    expect(rects.map((r) => r.rect.width)).toEqual([50, 50]);
    expect(controller.focusedTerminalId).toBe("t2");
    expect(selectedId).toBe("t2");
  });

  it("splits DOWN into a column", async () => {
    renderHook({ terminals: [terminal("t1", "ws-root")], createTerminal: creator("t2") });
    await waitFor(() => expect(controller.focusedPaneId).not.toBeNull());
    await act(async () => {
      controller.splitPane(controller.focusedPaneId as string, "column");
    });
    await waitFor(() => expect(controller.visibleTerminalIds).toHaveLength(2));
    expect(paneRects(controller.layout as PaneNode).map((r) => r.rect.height)).toEqual([50, 50]);
  });

  it("uses the ROOT workspace when the focused terminal's binding is stale", async () => {
    const createTerminal = creator("t2");
    renderHook({ terminals: [terminal("t1", "deleted-ws")], createTerminal });
    await waitFor(() => expect(controller.focusedPaneId).not.toBeNull());
    await act(async () => {
      controller.splitPane(controller.focusedPaneId as string, "row");
    });
    expect(createTerminal).toHaveBeenCalledWith("ws-root", "/repo");
    await waitFor(() => expect(controller.visibleTerminalIds).toHaveLength(2));
  });

  it("REFUSES explicitly (and creates nothing) when the project has no workspace", async () => {
    const createTerminal = creator("t2");
    renderHook({ terminals: [terminal("t1", null)], workspaces: [], createTerminal });
    await waitFor(() => expect(controller.focusedPaneId).not.toBeNull());
    const before = controller.layout;

    await act(async () => {
      controller.splitPane(controller.focusedPaneId as string, "row");
    });

    expect(createTerminal).not.toHaveBeenCalled();
    expect(toast.error).toHaveBeenCalledWith(expect.stringContaining("workspace"));
    // The layout is untouched — no empty pane, no half-split.
    expect(controller.layout).toBe(before);
    expect(controller.visibleTerminalIds).toEqual(["t1"]);
  });

  it("leaves the tree UNCHANGED when the terminal creation fails, and SHOWS the sidecar's reason", async () => {
    // The bridge rejects with a BridgeError OBJECT, never an `Error`: the toast must carry ITS
    // sentence (via `errorText`), not a generic line that hides why the spawn was refused.
    const createTerminal = vi.fn(async () => {
      throw {
        kind: "command",
        message: "workspace /repo no longer exists",
        source: "new_terminal",
      };
    });
    renderHook({ terminals: [terminal("t1", "ws-root")], createTerminal });
    await waitFor(() => expect(controller.focusedPaneId).not.toBeNull());
    const before = controller.layout;

    await act(async () => {
      controller.splitPane(controller.focusedPaneId as string, "row");
    });

    expect(toast.error).toHaveBeenCalledWith(
      expect.stringContaining("workspace /repo no longer exists"),
    );
    expect(controller.layout).toBe(before);
    expect(controller.visibleTerminalIds).toEqual(["t1"]);
    expect(paneCount(controller.layout as PaneNode)).toBe(1);
  });

  it("PERSISTS the split so a remount (navigation away and back) restores it", async () => {
    const { unmount } = renderHook({
      terminals: [terminal("t1", "ws-root")],
      createTerminal: creator("t2"),
    });
    await waitFor(() => expect(controller.focusedPaneId).not.toBeNull());
    await act(async () => {
      controller.splitPane(controller.focusedPaneId as string, "row");
    });
    await waitFor(() => expect(controller.visibleTerminalIds).toEqual(["t1", "t2"]));
    unmount();

    renderHook({
      terminals: [terminal("t1", "ws-root"), terminal("t2", "ws-root")],
      createTerminal: creator("t3"),
    });
    await waitFor(() => expect(controller.visibleTerminalIds).toEqual(["t1", "t2"]));
  });
});

describe("usePaneLayout — the ABSOLUTE pane floor refuses a split (finding F1)", () => {
  /** A host too narrow to hold two panes at the floor: 2 × 180 = 360 > 300. */
  const TOO_NARROW: HostSize = { width: 2 * MIN_PANE_WIDTH_PX - 60, height: 800 };
  /** A host too short to hold two panes at the floor: 2 × 120 = 240 > 200. */
  const TOO_SHORT: HostSize = { width: 1200, height: 2 * MIN_PANE_HEIGHT_PX - 40 };

  it("emits NO create_terminal and leaves the tree untouched when the pane is too narrow", async () => {
    const createTerminal = creator("t2");
    renderHook({
      terminals: [terminal("t1", "ws-root")],
      createTerminal,
      measureHost: () => TOO_NARROW,
    });
    await waitFor(() => expect(controller.focusedPaneId).not.toBeNull());
    const before = controller.layout;

    await act(async () => {
      controller.splitPane(controller.focusedPaneId as string, "row");
    });

    // NOTHING reached the backend: the refusal happens BEFORE the create path is entered, so no
    // terminal is spawned and none has to be cleaned up.
    expect(createTerminal).not.toHaveBeenCalled();
    // …and the tiling is the very same object — no leaf added, no ratio touched.
    expect(controller.layout).toBe(before);
    expect(controller.visibleTerminalIds).toEqual(["t1"]);
    expect(paneCount(controller.layout as PaneNode)).toBe(1);
  });

  it("SAYS SO: the refusal is a visible message naming the floor, never a silent no-op", async () => {
    renderHook({
      terminals: [terminal("t1", "ws-root")],
      createTerminal: creator("t2"),
      measureHost: () => TOO_NARROW,
    });
    await waitFor(() => expect(controller.focusedPaneId).not.toBeNull());
    await act(async () => {
      controller.splitPane(controller.focusedPaneId as string, "row");
    });
    expect(toast.error).toHaveBeenCalledWith(expect.stringContaining(`${MIN_PANE_WIDTH_PX}px`));

    // The other axis has its own message and its own threshold.
    vi.mocked(toast.error).mockClear();
    renderHook({
      terminals: [terminal("t1", "ws-root")],
      createTerminal: creator("t2"),
      measureHost: () => TOO_SHORT,
    });
    await waitFor(() => expect(controller.focusedPaneId).not.toBeNull());
    await act(async () => {
      controller.splitPane(controller.focusedPaneId as string, "column");
    });
    expect(toast.error).toHaveBeenCalledWith(expect.stringContaining(`${MIN_PANE_HEIGHT_PX}px`));
  });

  it("refuses only the AXIS being cut — the roomy one still splits", async () => {
    const createTerminal = creator("t2");
    renderHook({
      terminals: [terminal("t1", "ws-root")],
      createTerminal,
      // 300px wide (too narrow for a row cut) but 800px tall (plenty for a column cut).
      measureHost: () => TOO_NARROW,
    });
    await waitFor(() => expect(controller.focusedPaneId).not.toBeNull());

    await act(async () => {
      controller.splitPane(controller.focusedPaneId as string, "column");
    });
    expect(createTerminal).toHaveBeenCalledTimes(1);
    await waitFor(() => expect(controller.visibleTerminalIds).toEqual(["t1", "t2"]));
  });

  it("refuses the split that would produce the reported 18px panes, at the third cut", async () => {
    // The user's 906×600 main area. 906 → 453/453 ✓, 453 → 226/226 ✓, 226 → 113/113 ✗.
    const reported: HostSize = { width: 906, height: 600 };
    let created = 0;
    const createTerminal = vi.fn(async () => `t${++created + 1}`);
    renderHook({
      terminals: [terminal("t1", "ws-root")],
      createTerminal,
      measureHost: () => reported,
    });
    await waitFor(() => expect(controller.focusedPaneId).not.toBeNull());

    for (let i = 0; i < 4; i++) {
      await act(async () => {
        controller.splitPane(controller.focusedPaneId as string, "row");
      });
      await waitFor(() => expect(controller.layout).not.toBeNull());
    }

    // Only TWO of the four attempts got through: the tiling stops at three panes, and the
    // narrowest of them is 226px — not the 18px the relative clamp allowed.
    expect(createTerminal).toHaveBeenCalledTimes(2);
    expect(paneCount(controller.layout as PaneNode)).toBe(3);
    const widths = paneRects(controller.layout as PaneNode).map(
      (p) => (reported.width * p.rect.width) / 100,
    );
    expect(Math.min(...widths)).toBeGreaterThanOrEqual(MIN_PANE_WIDTH_PX);
  });
});

describe("usePaneLayout — close, reconcile, selection", () => {
  async function twoPaneSetup() {
    renderHook({ terminals: [terminal("t1", "ws-root")], createTerminal: creator("t2") });
    await waitFor(() => expect(controller.focusedPaneId).not.toBeNull());
    await act(async () => {
      controller.splitPane(controller.focusedPaneId as string, "row");
    });
    await waitFor(() => expect(controller.visibleTerminalIds).toEqual(["t1", "t2"]));
  }

  it("closing a pane compacts the tree and moves the focus to the sibling", async () => {
    await twoPaneSetup();
    const closed = controller.focusedPaneId as string;
    await act(async () => {
      controller.closePane(closed);
    });
    expect(controller.visibleTerminalIds).toEqual(["t1"]);
    expect(controller.focusedTerminalId).toBe("t1");
    expect(selectedId).toBe("t1");
  });

  it("closing the pane of a SOLO terminal is INERT — the terminal has no group to leave", async () => {
    // The arbitrage written in `closePane`: a solo terminal has exactly one pane and closing it
    // would leave the frame empty while the terminal is still alive in the sidebar. Closing the
    // TERMINAL is a different, existing action (the row's ×, Ctrl+W).
    renderHook({ terminals: [terminal("t1", "ws-root")], createTerminal: creator() });
    await waitFor(() => expect(controller.focusedPaneId).not.toBeNull());
    const before = controller.layout;
    await act(async () => {
      controller.closePane(controller.focusedPaneId as string);
    });
    expect(controller.visibleTerminalIds).toEqual(["t1"]);
    expect(controller.layout).toBe(before);
  });

  it("closing a pane of a PAIR dissolves the group and shows the SURVIVOR alone", async () => {
    await twoPaneSetup();
    const closed = controller.focusedPaneId as string;
    await act(async () => {
      controller.closePane(closed);
    });
    expect(controller.groups).toEqual([]);
    expect(controller.showingGroupId).toBeNull();
    expect(controller.visibleTerminalIds).toEqual(["t1"]);
    // The closed pane's terminal is STILL ALIVE — it merely became solo.
    expect(loadCollection("p1")).toBeNull();
  });

  it("drops the pane of a terminal that really disappeared and compacts", async () => {
    const { rerender } = renderHook({
      terminals: [terminal("t1", "ws-root")],
      createTerminal: creator("t2"),
    });
    await waitFor(() => expect(controller.focusedPaneId).not.toBeNull());
    await act(async () => {
      controller.splitPane(controller.focusedPaneId as string, "row");
    });
    await waitFor(() => expect(controller.visibleTerminalIds).toEqual(["t1", "t2"]));

    // The snapshot comes back WITHOUT t1 (closed elsewhere / by an agent over MCP).
    rerender(
      <RuntimeSelectionProvider>
        <Harness terminals={[terminal("t2", "ws-root")]} createTerminal={creator("t3")} />
      </RuntimeSelectionProvider>,
    );
    await waitFor(() => expect(controller.visibleTerminalIds).toEqual(["t2"]));
    expect(paneCount(controller.layout as PaneNode)).toBe(1);
  });

  it("falls back to a single VALID pane when the whole live set was replaced", async () => {
    const { rerender } = renderHook({
      terminals: [terminal("t1", "ws-root")],
      createTerminal: creator("t2"),
    });
    await waitFor(() => expect(controller.focusedPaneId).not.toBeNull());
    const stalePaneId = controller.focusedPaneId as string;

    // The TOTAL-stale reconcile, the one path that builds a tree outside the restore: t1 was closed
    // elsewhere (agent over MCP / another window) and `useTerminals` recreated a default terminal,
    // so the snapshot names t2 and t2 ONLY. Pruning leaves NOTHING (`pruneStaleTerminals` returns
    // `null`) and the layout must be rebuilt on the first live terminal — never left `null`, never
    // left focused on a pane id that died with the previous tree.
    rerender(
      <RuntimeSelectionProvider>
        <Harness terminals={[terminal("t2", "ws-root")]} createTerminal={creator("t3")} />
      </RuntimeSelectionProvider>,
    );

    await waitFor(() => expect(controller.visibleTerminalIds).toEqual(["t2"]));
    expect(controller.layout).not.toBeNull();
    const layout = controller.layout as PaneNode;
    expect(paneCount(layout)).toBe(1);
    expect(controller.focusedPaneId).not.toBeNull();
    expect(controller.focusedPaneId).not.toBe(stalePaneId);
    // The focus points at a leaf that REALLY exists in the rebuilt tree, holding the live terminal.
    expect(findPane(layout, controller.focusedPaneId as string)).not.toBeNull();
    expect(controller.focusedTerminalId).toBe("t2");
    // t2 is SOLO — it belongs to no group, so there is no group to store (solo membership is the
    // absence of a record, not a record of absence).
    expect(loadCollection("p1")).toBeNull();
  });

  it("mirrors the sidebar selection: a VISIBLE terminal only takes the focus", async () => {
    await twoPaneSetup();
    const firstPane = paneRects(controller.layout as PaneNode)[0]?.paneId as string;
    await act(async () => {
      controller.focusPane(firstPane);
    });
    expect(controller.focusedTerminalId).toBe("t1");
    expect(selectedId).toBe("t1");
    // Both panes are still on screen — focusing is not showing.
    expect(controller.visibleTerminalIds).toEqual(["t1", "t2"]);
  });

  /** A restored two-pane tiling (t1 | t2) over a project that ALSO has an off-screen t3. */
  async function twoPanesPlusOffscreen() {
    savePaneLayout(
      "p1",
      splitPane(createPaneLayout("t1", "p1"), {
        paneId: "p1",
        direction: "row",
        terminalId: "t2",
        newPaneId: "p2",
      }) as PaneNode,
    );
    renderHook({
      terminals: [terminal("t1", "ws-root"), terminal("t2", "ws-root"), terminal("t3", "ws-root")],
    });
    await waitFor(() => expect(controller.visibleTerminalIds).toEqual(["t1", "t2"]));
    expect(controller.focusedPaneId).toBe("p1");
  }

  /**
   * THE EXIT FROM THE TILING (review 01M0ABR6V501D0SPR2BYBVZC4A, critical). This used to assert the
   * opposite: selecting an off-screen terminal SUBSTITUTED it into the focused pane
   * (`setPaneTerminal`), which is exactly what left the user with no way back to one terminal
   * filling the frame. It now shows the terminal ALONE and the group is left strictly alone.
   */
  it("shows a SOLO terminal ALONE, and does not touch the group (same tree BY REFERENCE)", async () => {
    await twoPanesPlusOffscreen();
    const groupTree = controller.groups[0].tree;

    await act(async () => {
      selectRuntime({ kind: "terminal", id: "t3" });
    });

    await waitFor(() => expect(controller.visibleTerminalIds).toEqual(["t3"]));
    expect(paneCount(controller.layout as PaneNode)).toBe(1);
    expect(controller.focusedTerminalId).toBe("t3");
    expect(controller.showingGroupId).toBeNull();
    // THE GROUP IS UNCHANGED — not rebuilt, not re-keyed: the very same object.
    expect(controller.groups).toHaveLength(1);
    expect(controller.groups[0].tree).toBe(groupTree);
    expect(paneTerminalIds(controller.groups[0].tree)).toEqual(["t1", "t2"]);
    // …and no terminal was substituted anywhere.
    expect(loadPaneLayout("p1")).toEqual(groupTree);
  });

  it("shows a MEMBER's whole group back, focused on THAT member", async () => {
    await twoPanesPlusOffscreen();
    await act(async () => {
      selectRuntime({ kind: "terminal", id: "t3" });
    });
    await waitFor(() => expect(controller.visibleTerminalIds).toEqual(["t3"]));

    // Back to a member: the WHOLE group returns, and the focus is on the member that was clicked.
    await act(async () => {
      selectRuntime({ kind: "terminal", id: "t2" });
    });
    await waitFor(() => expect(controller.visibleTerminalIds).toEqual(["t1", "t2"]));
    expect(controller.focusedPaneId).toBe("p2");
    expect(controller.focusedTerminalId).toBe("t2");
    expect(controller.showingGroupId).toBe(controller.groups[0].id);
  });

  it("ignores a selection the runtime snapshot has NOT brought in yet", async () => {
    await twoPanesPlusOffscreen();
    const before = controller.layout;

    await act(async () => {
      selectRuntime({ kind: "terminal", id: "not-live-yet" });
    });

    expect(controller.layout).toBe(before);
    expect(controller.visibleTerminalIds).toEqual(["t1", "t2"]);
  });

  it("exposes EVERY visible terminal (the idle-unload's visited set), not just the focused one", async () => {
    await twoPaneSetup();
    expect(controller.visibleTerminalIds).toEqual(["t1", "t2"]);
    expect(controller.visibleTerminalIds).toContain(controller.focusedTerminalId as string);
    expect(controller.visibleTerminalIds.length).toBeGreaterThan(1);
  });
});

describe("usePaneLayout — resizeSplit", () => {
  /** A restored `t1 | t2` tiling under project `p1`, and the id of its one split. */
  async function twoPanes(ratio = 0.5): Promise<string> {
    savePaneLayout(
      "p1",
      splitPane(createPaneLayout("t1", "p1"), {
        paneId: "p1",
        direction: "row",
        terminalId: "t2",
        newPaneId: "p2",
        ratio,
      }) as PaneNode,
    );
    renderHook({ terminals: [terminal("t1", "ws-root"), terminal("t2", "ws-root")] });
    await waitFor(() => expect(controller.visibleTerminalIds).toEqual(["t1", "t2"]));
    return (controller.layout as PaneSplit).id;
  }

  /** What is written in `localStorage` right now (`null` when the entry is absent/invalid). */
  function persistedRatio(): number | null {
    const stored = loadPaneLayout("p1");
    return stored == null || stored.kind !== "split" ? null : stored.ratio;
  }

  it("applies a PREVIEW live but does not write it — the layout still moves", async () => {
    const splitId = await twoPanes();
    await act(async () => {
      controller.resizeSplit(splitId, 0.7, "preview");
    });
    // The tree (hence the panes' boxes, hence the ResizeObserver → FitAddon → pty_resize chain)
    // followed the pointer…
    expect(paneRects(controller.layout as PaneNode).map((p) => p.rect.width)).toEqual([70, 30]);
    expect(paneSeparators(controller.layout as PaneNode)[0]?.ratio).toBe(0.7);
    // …but nothing was persisted: a drag must not write once per frame.
    expect(persistedRatio()).toBe(0.5);
  });

  it("writes the ratio when the interaction ENDS", async () => {
    const splitId = await twoPanes();
    await act(async () => {
      controller.resizeSplit(splitId, 0.62, "preview");
      controller.resizeSplit(splitId, 0.68, "preview");
    });
    expect(persistedRatio()).toBe(0.5);

    await act(async () => {
      controller.resizeSplit(splitId, 0.68, "commit");
    });
    // The commit persists even though that last frame did not move the tree.
    expect(persistedRatio()).toBe(0.68);
  });

  /**
   * The band clamped HERE is the STORABLE one, not the aesthetic 15..85: a seam may legitimately
   * park outside 15..85 when the px floor demands it (user arbitrage — see `pane-tree`'s two
   * bands). The 15..85 band is enforced one layer up, by `separatorRatioBounds`, which every drag
   * and every key goes through before reaching this hook; what this asserts is the LAST resort —
   * a nonsense number cannot collapse a side to nothing.
   */
  it("CLAMPS whatever it is handed — a ratio can never leave the storable band", async () => {
    const splitId = await twoPanes();
    await act(async () => {
      controller.resizeSplit(splitId, 5, "commit");
    });
    expect(persistedRatio()).toBe(MAX_STORED_RATIO);
    await act(async () => {
      controller.resizeSplit(splitId, -3, "commit");
    });
    expect(persistedRatio()).toBe(MIN_STORED_RATIO);
    // …and the aesthetic band is still INSIDE it, so nothing about 15..85 was widened away.
    expect(MIN_STORED_RATIO).toBeLessThan(MIN_SPLIT_RATIO);
    expect(MAX_STORED_RATIO).toBeGreaterThan(MAX_SPLIT_RATIO);
  });

  it("is a pure LAYOUT edit: no pane is added, dropped or refocused", async () => {
    const splitId = await twoPanes();
    const focused = controller.focusedPaneId;
    await act(async () => {
      controller.resizeSplit(splitId, 0.3, "commit");
    });
    expect(controller.visibleTerminalIds).toEqual(["t1", "t2"]);
    expect(paneCount(controller.layout as PaneNode)).toBe(2);
    expect(controller.focusedPaneId).toBe(focused);
  });

  it("ignores an unknown split id", async () => {
    await twoPanes();
    const before = controller.layout;
    await act(async () => {
      controller.resizeSplit("split-gone", 0.2, "commit");
    });
    expect(controller.layout).toBe(before);
  });

  it("does not mute the NEXT write when a drag is abandoned without a commit", async () => {
    const splitId = await twoPanes();
    await act(async () => {
      controller.resizeSplit(splitId, 0.75, "preview");
    });
    expect(persistedRatio()).toBe(0.5);
    // A different transition after the abandoned drag persists normally (the preview flag lives
    // in the state it describes, so it cannot leak onto an unrelated one).
    await act(async () => {
      controller.closePane(controller.focusedPaneId as string);
    });
    // The pair dissolved: the project has no group left, and THAT is what got written.
    expect(loadCollection("p1")).toBeNull();
  });
});

/**
 * The MODEL half of navigation / reload: which tree comes back, with which ratios and which pane
 * focused. Its PIXEL half — every restored pane measured before its PTY opens, and the separators'
 * `aria-value*` derived from the real host, in a window that may be SMALLER than the one that
 * persisted the layout — cannot be stated here (jsdom has no layout engine and no useful rAF) and
 * is covered in Chromium by the two RESTORE specs of `pane-tiling-gate.browser.test.tsx`.
 */
describe("usePaneLayout — restore of tree, ratios and focus", () => {
  /** Build `t1 | (t2 / t3)` with explicit, non-default ratios — the shape a restore must return. */
  function nestedLayout(): PaneNode {
    const row = splitPane(createPaneLayout("t1", "p1"), {
      paneId: "p1",
      direction: "row",
      terminalId: "t2",
      newPaneId: "p2",
      ratio: 0.35,
    }) as PaneNode;
    return splitPane(row, {
      paneId: "p2",
      direction: "column",
      terminalId: "t3",
      newPaneId: "p3",
      ratio: 0.75,
    }) as PaneNode;
  }

  const THREE = [terminal("t1", "ws-root"), terminal("t2", "ws-root"), terminal("t3", "ws-root")];

  it("RELOAD: the tree, every ratio and the focused pane come back", async () => {
    // The live session: a nested tiling, resized, with the focus on the LAST pane.
    savePaneLayout("p1", nestedLayout());
    saveLastTerminalId("p1", "t3");
    const { unmount } = renderHook({ terminals: THREE });
    await waitFor(() => expect(controller.visibleTerminalIds).toEqual(["t1", "t2", "t3"]));
    const splitId = (controller.layout as PaneSplit).id;
    await act(async () => {
      controller.resizeSplit(splitId, 0.28, "commit");
    });
    const before = paneRects(controller.layout as PaneNode);
    expect(controller.focusedPaneId).toBe("p3");
    // A RELOAD: the whole renderer goes and comes back on the same `localStorage`.
    unmount();

    renderHook({ terminals: THREE });
    await waitFor(() => expect(controller.visibleTerminalIds).toEqual(["t1", "t2", "t3"]));
    // Same panes, same ids, same geometry to the percent — ratios included, nested ones too.
    expect(paneRects(controller.layout as PaneNode)).toEqual(before);
    // …and the focus landed back on a pane that EXISTS, the one showing the remembered terminal.
    expect(controller.focusedPaneId).toBe("p3");
    expect(controller.focusedTerminalId).toBe("t3");
  });

  it("PROJECT ROUND TRIP: each project keeps its own tree and ratios", async () => {
    savePaneLayout("p1", nestedLayout());
    savePaneLayout("p2", null); // the other project's terminal is SOLO — it has no group
    const { unmount } = renderHook({ projectId: "p1", terminals: THREE });
    await waitFor(() => expect(controller.visibleTerminalIds).toEqual(["t1", "t2", "t3"]));
    const splitId = (controller.layout as PaneSplit).id;
    await act(async () => {
      controller.resizeSplit(splitId, 0.22, "commit");
    });
    const projectOne = paneRects(controller.layout as PaneNode);
    unmount();

    // AWAY: another project, its own single-pane tiling — no leak in either direction.
    const away = renderHook({ projectId: "p2", terminals: [terminal("t9", "ws-root")] });
    await waitFor(() => expect(controller.visibleTerminalIds).toEqual(["t9"]));
    expect(paneCount(controller.layout as PaneNode)).toBe(1);
    away.unmount();

    // BACK: the first project's tiling is exactly as it was left.
    renderHook({ projectId: "p1", terminals: THREE });
    await waitFor(() => expect(controller.visibleTerminalIds).toEqual(["t1", "t2", "t3"]));
    expect(paneRects(controller.layout as PaneNode)).toEqual(projectOne);
    expect(controller.focusedPaneId).not.toBeNull();
  });

  it("falls back to a VALID focus when the remembered terminal is no longer tiled", async () => {
    savePaneLayout("p1", nestedLayout());
    // The hint names a terminal that is NOT in the restored tree (it was closed while away).
    saveLastTerminalId("p1", "t404");
    renderHook({ terminals: THREE });
    await waitFor(() => expect(controller.visibleTerminalIds).toEqual(["t1", "t2", "t3"]));
    // Never an invalid focus: it degrades to the first pane rather than pointing at nothing.
    expect(controller.focusedPaneId).toBe("p1");
    expect(controller.focusedTerminalId).toBe("t1");
  });
});

/**
 * THE MEASURED HOST REACHING THE RESTORE — the one link of the restore chain nothing attested
 * (review round 4). `restorePaneLayout`'s `host` argument defaults to `null`, so the CORRECT call
 * and the DEGRADED one are a single word apart and both compile; the compaction itself is well
 * covered, but only through tests that hand the host in BY HAND. Replacing
 * `measureHostRef.current()` with `null` in `use-pane-layout.ts` used to leave the whole suite
 * green — i.e. the feature this round added could be switched off in production in silence.
 */
describe("usePaneLayout — the MEASURED host is what the restore judges with", () => {
  /** A persisted seam at the top of the STORABLE band: 0.98 — accepted by the shape guard. */
  const corrupted = (): PaneNode => ({
    kind: "split",
    id: "s1",
    direction: "row",
    ratio: MAX_STORED_RATIO,
    children: [
      { kind: "leaf", id: "p1", terminalId: "t1" },
      { kind: "leaf", id: "p2", terminalId: "t2" },
    ],
  });
  const TWO = [terminal("t1", "ws-root"), terminal("t2", "ws-root")];
  const HOST: HostSize = { width: 720, height: 900 };
  const widths = (root: PaneNode, host: HostSize) =>
    paneRects(root).map((p) => (host.width * p.rect.width) / 100);

  it("restores 0.98 as a 540 / 180px tiling — the hook's OWN measurement, not one handed in", async () => {
    savePaneLayout("p1", corrupted());
    renderHook({ terminals: TWO, measureHost: () => HOST });
    await waitFor(() => expect(controller.visibleTerminalIds).toEqual(["t1", "t2"]));
    // 705.60 / 14.40px is what the restore returns when the measurement never reaches it.
    expect(widths(controller.layout as PaneNode, HOST)).toEqual([540, 180]);
    expect(Math.min(...widths(controller.layout as PaneNode, HOST))).toBeGreaterThanOrEqual(
      MIN_PANE_WIDTH_PX,
    );
  });

  it("and an UNMEASURABLE host still restores exactly what it did before the px pass existed", async () => {
    // jsdom has no layout engine and the tiling may not be on screen yet: `measureHost` answers
    // `null`, there is nothing to judge with, and the entry comes back untouched. That is the
    // written engagement of `pane-layout-persistence.ts` and (1) must not break it.
    savePaneLayout("p1", corrupted());
    renderHook({ terminals: TWO });
    await waitFor(() => expect(controller.visibleTerminalIds).toEqual(["t1", "t2"]));
    expect(widths(controller.layout as PaneNode, HOST)).toEqual([705.6, 14.4]);
  });
});

/**
 * THE GROUP TRANSITIONS AT THE HOOK LEVEL — the model's rules as the page really drives them
 * (review 01M0ABR6V501D0SPR2BYBVZC4A, critical).
 */
describe("usePaneLayout — groups", () => {
  const TWO = [terminal("t1", "ws-root"), terminal("t2", "ws-root")];

  /** A creator handing back a FRESH id per call (t3, t4, …) — a split can then follow a split. */
  function sequence() {
    let n = 3;
    return vi.fn(async (_workspaceId: string, _path: string) => `t${n++}`);
  }

  /** The displayed tree's leaves as `[paneId, terminalId]` — which rectangle holds which terminal. */
  const paneRectangles = (): [string, string][] =>
    paneLeaves(controller.layout as PaneNode).map((leaf) => [leaf.id, leaf.terminalId]);

  /** A project showing the solo terminal `t1`, with `t2` live but solo too. */
  async function solo(createTerminal = sequence()) {
    renderHook({ terminals: TWO, createTerminal });
    await waitFor(() => expect(controller.visibleTerminalIds).toEqual(["t1"]));
    expect(controller.groups).toEqual([]);
  }

  it("a split from a SOLO terminal creates a GROUP OF TWO", async () => {
    await solo();
    await act(async () => {
      controller.splitPane(controller.focusedPaneId as string, "row");
    });
    await waitFor(() => expect(controller.visibleTerminalIds).toEqual(["t1", "t3"]));
    expect(controller.groups).toHaveLength(1);
    expect(paneTerminalIds(controller.groups[0].tree)).toEqual(["t1", "t3"]);
    expect(controller.showingGroupId).toBe(controller.groups[0].id);
    // Born grey and nameless — nothing is asked at creation.
    expect([controller.groups[0].name, controller.groups[0].color]).toEqual([null, null]);
    // …and the OTHER terminal is untouched: still solo.
    expect(paneTerminalIds(controller.groups[0].tree)).not.toContain("t2");
  });

  it("a split from a MEMBER adds a leaf to the EXISTING group — never a second one", async () => {
    await solo();
    await act(async () => {
      controller.splitPane(controller.focusedPaneId as string, "row");
    });
    await waitFor(() => expect(controller.groups).toHaveLength(1));
    const groupId = controller.groups[0].id;

    // A SECOND split, this time from inside the group.
    await act(async () => {
      controller.splitPane(controller.focusedPaneId as string, "column");
    });
    await waitFor(() => expect(controller.visibleTerminalIds).toHaveLength(3));
    expect(controller.groups).toHaveLength(1);
    expect(controller.groups[0].id).toBe(groupId);
    expect(paneCount(controller.layout as PaneNode)).toBe(3);
    expect(paneTerminalIds(controller.groups[0].tree)).toEqual(["t1", "t3", "t4"]);
  });

  it("LEAVING a group compacts it, makes the terminal solo, and SHOWS it alone", async () => {
    await solo();
    await act(async () => {
      controller.splitPane(controller.focusedPaneId as string, "row");
    });
    await waitFor(() => expect(controller.groups).toHaveLength(1));
    await act(async () => {
      controller.splitPane(controller.focusedPaneId as string, "column");
    });
    await waitFor(() => expect(controller.visibleTerminalIds).toHaveLength(3));

    await act(async () => {
      controller.leaveGroup("t1");
    });
    expect(controller.visibleTerminalIds).toEqual(["t1"]);
    expect(controller.showingGroupId).toBeNull();
    expect(controller.groups).toHaveLength(1);
    expect(paneTerminalIds(controller.groups[0].tree)).not.toContain("t1");
    expect(selectedId).toBe("t1");
  });

  it("leaving the SECOND-TO-LAST member DISSOLVES the group", async () => {
    await solo();
    await act(async () => {
      controller.splitPane(controller.focusedPaneId as string, "row");
    });
    await waitFor(() => expect(controller.groups).toHaveLength(1));
    await act(async () => {
      controller.leaveGroup("t1");
    });
    expect(controller.groups).toEqual([]);
    expect(controller.visibleTerminalIds).toEqual(["t1"]);
  });

  it("JOINING puts a solo terminal into a group, at the automatic position", async () => {
    await solo();
    await act(async () => {
      controller.splitPane(controller.focusedPaneId as string, "row");
    });
    await waitFor(() => expect(controller.groups).toHaveLength(1));
    const groupId = controller.groups[0].id;

    // JOINING at the automatic position is the END index of `insertIntoGroup` — the controller's
    // separate `joinGroup` is gone with the row menu that was its only caller.
    await act(async () => {
      controller.insertIntoGroup(groupId, "t2", 2);
    });
    expect(controller.groups).toHaveLength(1);
    expect(paneTerminalIds(controller.groups[0].tree)).toEqual(["t1", "t3", "t2"]);
    expect(controller.showingGroupId).toBe(groupId);
    expect(controller.focusedTerminalId).toBe("t2");
  });

  /**
   * THE FOCUS FOLLOWS THE TERMINAL THROUGH A SIDEBAR PERMUTATION (review finding
   * 01M11WR6JTPGAP4ABK0RWXB6XK).
   *
   * `withMemberOrder` permutes the OCCUPANTS and FREEZES the pane ids, so a `focusedPaneId`
   * re-conducted verbatim would hand the keyboard (and the focus ring, and the Spec surcouche) to
   * whatever terminal the permutation moved into that rectangle, while the sidebar kept naming the
   * previous one. The three surfaces must answer with the SAME terminal after the gesture as
   * before it — this is what the two pre-existing permutation specs (`pane-move.test.tsx`,
   * `pane-move.browser.test.tsx`) never asserted.
   */
  it("PERMUTING members re-pins the focus on the terminal, not on the rectangle", async () => {
    await solo();
    await act(async () => {
      controller.splitPane(controller.focusedPaneId as string, "row");
    });
    await waitFor(() => expect(controller.groups).toHaveLength(1));
    await act(async () => {
      controller.splitPane(controller.focusedPaneId as string, "column");
    });
    await waitFor(() => expect(controller.visibleTerminalIds).toHaveLength(3));
    const groupId = controller.groups[0].id;
    expect(groupMemberIds(controller.groups[0])).toEqual(["t1", "t3", "t4"]);

    // Put the focus on the FIRST member through the sidebar's own gesture, so the three surfaces
    // start out in agreement (ring on t1's rectangle, keyboard on t1, sidebar naming t1).
    act(() => selectRuntime({ kind: "terminal", id: "t1" }));
    await waitFor(() => expect(controller.focusedTerminalId).toBe("t1"));
    const rectangles = paneRectangles();
    const focusedRectangle = controller.focusedPaneId as string;

    // The permutation the sidebar drag commits: t1 goes last, t4 takes the first rectangle.
    act(() => controller.reorderGroupMembers(groupId, ["t4", "t3", "t1"]));

    // STRUCTURE FROZEN — the same pane ids in the same reading order, only the occupants moved.
    expect(paneRectangles().map(([id]) => id)).toEqual(rectangles.map(([id]) => id));
    expect(paneRectangles().map(([, t]) => t)).toEqual(["t4", "t3", "t1"]);
    // …so the rectangle that had the focus is now t4's, and the focus is NOT there any more.
    expect(controller.focusedPaneId).not.toBe(focusedRectangle);
    // THE THREE SURFACES STILL AGREE, on the very terminal that had the focus before the gesture.
    expect(controller.focusedTerminalId, "the keyboard stayed with t1").toBe("t1");
    expect(
      findPane(controller.layout as PaneNode, controller.focusedPaneId as string)?.terminalId,
      "the ring is on the rectangle that now hosts t1",
    ).toBe("t1");
    expect(selectedId, "and the sidebar names the same terminal").toBe("t1");
  });

  it("DISSOLVING makes every member solo and keeps the focused one on screen", async () => {
    await solo();
    await act(async () => {
      controller.splitPane(controller.focusedPaneId as string, "row");
    });
    await waitFor(() => expect(controller.groups).toHaveLength(1));
    const shown = controller.focusedTerminalId as string;
    await act(async () => {
      controller.dissolveGroup(controller.groups[0].id);
    });
    expect(controller.groups).toEqual([]);
    expect(controller.visibleTerminalIds).toEqual([shown]);
  });

  it("PERSISTS a group's name and colour, and brings them back on reload", async () => {
    await solo();
    await act(async () => {
      controller.splitPane(controller.focusedPaneId as string, "row");
    });
    await waitFor(() => expect(controller.groups).toHaveLength(1));
    const groupId = controller.groups[0].id;
    await act(async () => {
      controller.renameGroup(groupId, "build");
      controller.recolorGroup(groupId, 4);
    });
    expect(loadCollection("p1")?.groups[0]).toMatchObject({ name: "build", color: 4 });

    // Clearing either one leaves the group alive.
    await act(async () => {
      controller.renameGroup(groupId, "");
    });
    expect(controller.groups[0].name).toBeNull();
    expect(controller.groups[0].color).toBe(4);
    await act(async () => {
      controller.recolorGroup(groupId, null);
    });
    expect(controller.groups).toHaveLength(1);
    expect(loadCollection("p1")?.groups[0]).toMatchObject({ name: null, color: null });
  });
});

/**
 * A GROUP BORN GREY AND NAMELESS IS FULLY USABLE (review 01M0ABR6V501D0SPR2BYBVZC4A, major).
 * Nothing is asked at creation, and nothing has to be filled in for the group to work.
 */
describe("usePaneLayout — an UNNAMED, UNCOLOURED group works exactly like a named one", () => {
  const TWO = [terminal("t1", "ws-root"), terminal("t2", "ws-root")];

  function sequence() {
    let n = 3;
    return vi.fn(async (_workspaceId: string, _path: string) => `t${n++}`);
  }

  it("displays, navigates, splits and releases without a name or a colour", async () => {
    renderHook({ terminals: TWO, createTerminal: sequence() });
    await waitFor(() => expect(controller.visibleTerminalIds).toEqual(["t1"]));
    await act(async () => {
      controller.splitPane(controller.focusedPaneId as string, "row");
    });
    await waitFor(() => expect(controller.groups).toHaveLength(1));
    expect([controller.groups[0].name, controller.groups[0].color]).toEqual([null, null]);

    // DISPLAY — the group is on screen with both members.
    expect(controller.visibleTerminalIds).toEqual(["t1", "t3"]);
    // NAVIGATION — selecting a member focuses it; selecting the solo one shows it alone.
    await act(async () => {
      selectRuntime({ kind: "terminal", id: "t1" });
    });
    expect(controller.focusedTerminalId).toBe("t1");
    await act(async () => {
      selectRuntime({ kind: "terminal", id: "t2" });
    });
    await waitFor(() => expect(controller.visibleTerminalIds).toEqual(["t2"]));
    // …and back to the group by picking any MEMBER — t1 is live in the snapshot, t3 is the one
    // the split created and the harness has not re-pushed yet.
    await act(async () => {
      selectRuntime({ kind: "terminal", id: "t1" });
    });
    await waitFor(() => expect(controller.visibleTerminalIds).toEqual(["t1", "t3"]));

    // SPLIT — inside the unnamed group, like any other.
    await act(async () => {
      controller.splitPane(controller.focusedPaneId as string, "column");
    });
    await waitFor(() => expect(controller.visibleTerminalIds).toHaveLength(3));
    // LEAVE — and it dissolves by the same rule when it drops below two.
    await act(async () => {
      controller.leaveGroup("t1");
    });
    expect(controller.groups).toHaveLength(1);
  });

  it("carries its NAME and COLOUR across a RELAUNCH", async () => {
    const { unmount } = renderHook({ terminals: TWO, createTerminal: sequence() });
    await waitFor(() => expect(controller.focusedPaneId).not.toBeNull());
    await act(async () => {
      controller.splitPane(controller.focusedPaneId as string, "row");
    });
    await waitFor(() => expect(controller.groups).toHaveLength(1));
    const groupId = controller.groups[0].id;
    await act(async () => {
      controller.renameGroup(groupId, "build");
      controller.recolorGroup(groupId, 4);
    });
    saveLastTerminalId("p1", "t1");
    unmount();

    // A RELAUNCH: the renderer goes and comes back on the same `localStorage`.
    renderHook({
      terminals: [...TWO, terminal("t3", "ws-root")],
      createTerminal: sequence(),
    });
    await waitFor(() => expect(controller.groups).toHaveLength(1));
    expect(controller.groups[0].id).toBe(groupId);
    expect([controller.groups[0].name, controller.groups[0].color]).toEqual(["build", 4]);
    expect(paneTerminalIds(controller.groups[0].tree)).toEqual(["t1", "t3"]);
  });
});
