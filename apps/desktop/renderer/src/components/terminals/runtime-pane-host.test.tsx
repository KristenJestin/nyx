import { render, screen, waitFor } from "@testing-library/react";
import { bridgeFake, mockIPC } from "@/bridge/test-harness";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { __resetTerminalStore } from "@/components/terminal/terminal-store";
import { CommandGroupsProvider } from "@/components/command/command-groups-context";
import { commandGroupTree } from "@/components/command/command-group-tree";
import type {
  CommandGroup,
  CommandGroupsController,
} from "@/components/command/use-command-groups";
import { RuntimePaneHost } from "./runtime-pane-host";
import { paneRects, paneRectStyle } from "@/components/panes/pane-geometry";
import { createPaneLayout, splitPane, type PaneNode } from "@/components/panes/pane-tree";
import { RuntimeSelectionProvider, type RuntimeSelection } from "./runtime-selection";
import type { PaneLayoutController } from "./use-pane-layout";
import type {
  WorkspaceRuntimeService,
  WorkspaceRuntimeTerminal,
} from "@/components/runtime/use-workspace-runtime";

function service(overrides: Partial<WorkspaceRuntimeService> = {}): WorkspaceRuntimeService {
  return {
    id: "s1",
    command_id: "c1",
    workspace_id: "ws1",
    name: "preview",
    command: "npm run preview",
    subfolder: null,
    description: null,
    run_state: "idle",
    exit_code: 0,
    unread: false,
    ...overrides,
  };
}

function renderHost(services: WorkspaceRuntimeService[], selection: RuntimeSelection) {
  return render(
    <RuntimeSelectionProvider initialSelection={selection}>
      <RuntimePaneHost terminals={[]} services={services} />
    </RuntimeSelectionProvider>,
  );
}

describe("RuntimePaneHost — managed-command empty state", () => {
  beforeEach(() => {
    // The output panel rehydrates via `command_output`; keep it inert for the running case.
    mockIPC((cmd) => (cmd === "command_output" ? "" : undefined));
  });

  it("shows the MINIMAL empty state (a discrete state label, not a card/black pane) when idle", () => {
    renderHost([service({ run_state: "idle", exit_code: 0 })], { kind: "service", id: "s1" });
    // The minimal empty state = just the run-state word; the rejected card's copy is gone.
    expect(screen.getByText("idle")).toBeTruthy();
    expect(screen.queryByText(/npm run preview/)).toBeNull();
    expect(screen.queryByText(/Last run/)).toBeNull();
    expect(screen.queryByText("Output appears here when the command runs.")).toBeNull();
  });

  it("shows the live output pane (no idle empty state) once the service is running", () => {
    renderHost([service({ run_state: "running", exit_code: null })], { kind: "service", id: "s1" });
    // A running service streams output — the idle state label is NOT rendered.
    expect(screen.queryByText("idle")).toBeNull();
  });
});

/* ─────────── THE TILING, AS PRODUCTION WIRES IT ─────────── */

function terminal(id: string): WorkspaceRuntimeTerminal {
  return {
    id,
    cwd: `/repo/${id}`,
    label: null,
    workspace_id: "ws1",
    status: "alive",
    exec_state: "idle",
    exec_state_unread: false,
    sessions: [],
  };
}

/**
 * A tiling of `ids`, ONE leaf each: `ids[i]` sits in `p{i+1}`. Two ids give the historical
 * `t1 | t2` pair; more of them alternate row/column exactly as successive splits would, which is
 * what lets a spec scale the PANE COUNT without a second fixture.
 */
function tilingOf(ids: readonly string[]): PaneNode {
  let tree: PaneNode = createPaneLayout(ids[0], "p1");
  ids.slice(1).forEach((id, i) => {
    tree = splitPane(tree, {
      paneId: `p${i + 1}`,
      direction: i % 2 === 0 ? "row" : "column",
      terminalId: id,
      newPaneId: `p${i + 2}`,
    }) as PaneNode;
  });
  return tree;
}

/**
 * A CONTROLLER STUB — the tiling state the page owns, handed in as production hands it. The
 * behaviour of `usePaneLayout` itself is pinned in `use-pane-layout.test.tsx`; what is under test
 * here is what `<RuntimePaneHost>` DOES with a controller.
 */
function controller(
  focusedPaneId: string,
  ids: readonly string[] = ["t1", "t2"],
): PaneLayoutController {
  const noop = () => {};
  return {
    layout: tilingOf(ids),
    focusedPaneId,
    focusedTerminalId: ids[Number(focusedPaneId.slice(1)) - 1] ?? null,
    groups: [],
    showingGroupId: null,
    showGroup: noop,
    visibleTerminalIds: ids,
    focusPane: noop,
    splitPane: noop,
    closePane: noop,
    resizeSplit: noop,
    focusNeighborPane: noop,
    closeFocusedPane: noop,
    leaveGroup: noop,
    reorderGroupMembers: noop,
    insertIntoGroup: noop,
    canInsertIntoGroup: () => true,
    createGroupWith: noop,
    canCreateGroup: () => true,
    movePane: noop,
    canMovePane: () => true,
    dissolveGroup: noop,
    renameGroup: noop,
    recolorGroup: noop,
  };
}

describe("RuntimePaneHost — the tiling is the terminal surface", () => {
  beforeEach(() => {
    mockIPC((cmd) => {
      if (cmd === "pty_spawn") return 7;
      if (cmd === "terminal_specs") return { session: null, specs: [] };
      return null;
    });
  });

  afterEach(() => {
    __resetTerminalStore();
    vi.restoreAllMocks();
  });

  function renderTiling(focusedPaneId: string, selection: RuntimeSelection) {
    return render(
      <RuntimeSelectionProvider initialSelection={selection}>
        <RuntimePaneHost
          terminals={[terminal("t1"), terminal("t2")]}
          services={[]}
          tiling={controller(focusedPaneId)}
        />
      </RuntimeSelectionProvider>,
    );
  }

  it("mounts the TILING host — both terminals visible, exactly one focused", async () => {
    const { container } = renderTiling("p2", { kind: "terminal", id: "t2" });
    await waitFor(() => expect(container.querySelector("[data-pane-host]")).not.toBeNull());
    const visible = container.querySelectorAll('[data-visible="true"]');
    expect(visible, "both leaves are on screen at once — this is the whole point").toHaveLength(2);
    const focused = container.querySelectorAll('[data-focused="true"]');
    expect(focused, "and exactly ONE holds the keyboard").toHaveLength(1);
    expect((focused[0] as HTMLElement).dataset.terminalId).toBe("t2");
  });

  /**
   * THE SURCOUCHE IS PER TERMINAL, THE RESOLVER IS ONE (review finding
   * 01M11WF11C0H8G2MG515FBT6RD). Every VISIBLE terminal is resolved — that is what makes a
   * languette stay readable on a pane the keyboard has left — while the LIVE COST stays what a
   * single overlay used to pay: one `specs://changed` and one `agent-sessions://changed`
   * subscription for the whole tiling, whatever the pane count. This is the criterion the finding
   * asks to be proven by measurement rather than by reading.
   */
  it("resolves EVERY visible terminal through ONE live resolver", async () => {
    const fake = bridgeFake();
    const { container } = renderTiling("p2", { kind: "terminal", id: "t2" });
    await waitFor(() => expect(container.querySelector("[data-pane-host]")).not.toBeNull());

    await waitFor(() =>
      expect(
        new Set(
          fake.calls.filter((c) => c.command === "terminal_specs").map((c) => c.args?.terminalId),
        ),
      ).toEqual(new Set(["t1", "t2"])),
    );
    // …and the SUBSCRIPTIONS did not multiply with the panes: ONE per topic, live, for both.
    expect(fake.subscriberCount("specs://changed"), "one resolver, not one per pane").toBe(1);
    expect(fake.subscriberCount("agent-sessions://changed")).toBe(1);
  });

  it("the resolver does NOT multiply with the panes — four panes, still ONE subscription each", async () => {
    const fake = bridgeFake();
    const ids = ["t1", "t2", "t3", "t4"];
    const { container } = render(
      <RuntimeSelectionProvider initialSelection={{ kind: "terminal", id: "t1" }}>
        <RuntimePaneHost
          terminals={ids.map(terminal)}
          services={[]}
          tiling={controller("p1", ids)}
        />
      </RuntimeSelectionProvider>,
    );
    await waitFor(() =>
      expect(
        container.querySelectorAll('[data-visible="true"]'),
        "four panes on screen",
      ).toHaveLength(4),
    );
    await waitFor(() =>
      expect(
        new Set(
          fake.calls.filter((c) => c.command === "terminal_specs").map((c) => c.args?.terminalId),
        ),
        "all four terminals resolved",
      ).toEqual(new Set(ids)),
    );
    // THE COUNT THE PRD PINS: the live subscriptions are a property of the RESOLVER, not of the
    // pane count. Doubling the panes leaves both at one.
    expect(fake.subscriberCount("specs://changed")).toBe(1);
    expect(fake.subscriberCount("agent-sessions://changed")).toBe(1);
  });

  it("takes the resolver away when a SERVICE is the selected surface (still one at most)", async () => {
    const fake = bridgeFake();
    const { container } = renderTiling("p2", { kind: "service", id: "s1" });
    await waitFor(() => expect(container.querySelector("[data-pane-host]")).not.toBeNull());
    // The tiling stays mounted (its terminals keep their PTYs) but is hidden, and the surcouche of
    // a terminal nobody is looking at has nothing to say — exactly as before the tiling existed.
    expect(container.querySelector('[data-terminal-id="t2"]')?.closest(".hidden")).not.toBeNull();
    expect(fake.calls.filter((c) => c.command === "terminal_specs")).toHaveLength(0);
    // Not "one idle resolver" — NONE: the provider is handed an empty id list and opens nothing.
    expect(fake.subscriberCount("specs://changed")).toBe(0);
    expect(fake.subscriberCount("agent-sessions://changed")).toBe(0);
  });

  it("falls back to the placeholder when there is no tiling at all", () => {
    render(
      <RuntimeSelectionProvider initialSelection={null}>
        <RuntimePaneHost terminals={[terminal("t1")]} services={[]} />
      </RuntimeSelectionProvider>,
    );
    expect(screen.getByText("No terminals to show.")).toBeTruthy();
  });
});

/* ─────────── A COMMAND GROUP, ON THE MAIN SURFACE ─────────── */

/**
 * THE MOUNT, NOT THE COMPONENT (review finding 01M0ZN7JSN5MPJ484VF5NFED8G). `<CommandPaneHost>`
 * was written, tested and reachable from nowhere: `<RuntimePaneHost>` rendered `CommandOutputPanel`
 * for the selected service and never asked whether it belonged to a group, so a command group never
 * appeared in the app. These tests drive the REAL main surface with the REAL provider, and compare
 * the rendered rectangles to `commandGroupTree` — the disposition the settings screen composes.
 */
describe("RuntimePaneHost — a selected service that belongs to a GROUP", () => {
  beforeEach(() => {
    mockIPC((cmd) => (cmd === "command_output" ? "" : undefined));
  });

  function groupsController(groups: CommandGroup[]): CommandGroupsController {
    return {
      groups,
      groupOf: (commandId) => groups.find((g) => g.command_ids.includes(commandId)) ?? null,
      createGroup: async () => {},
      addToGroup: async () => {},
      removeFromGroup: async () => {},
      dissolveGroup: async () => {},
      renameGroup: async () => {},
      recolorGroup: async () => {},
      setLayout: async () => {},
      setShape: async () => {},
      reorderCommands: async () => {},
      reload: async () => {},
    };
  }

  function renderGrouped(groups: CommandGroup[], services: WorkspaceRuntimeService[], id: string) {
    return render(
      <CommandGroupsProvider controller={groupsController(groups)}>
        <RuntimeSelectionProvider initialSelection={{ kind: "service", id }}>
          <RuntimePaneHost terminals={[]} services={services} />
        </RuntimeSelectionProvider>
      </CommandGroupsProvider>,
    );
  }

  const members = ["c1", "c2", "c3"];
  const grouped: CommandGroup = {
    id: "g1",
    project_id: "p1",
    name: null,
    color: null,
    layout_shape: "auto",
    command_ids: members,
  };
  const running = members.map((commandId, i) =>
    service({
      id: `s${i + 1}`,
      command_id: commandId,
      name: commandId,
      run_state: "running",
      exit_code: null,
    }),
  );

  it("renders the WHOLE group, with the rectangles commandGroupTree derives", () => {
    const { container } = renderGrouped([grouped], running, "s1");
    const host = container.querySelector("[data-command-pane-host]");
    expect(host).not.toBeNull();

    // The panes on screen, in DOM order, with their geometry read off the inline styles.
    const drawn = Array.from(container.querySelectorAll<HTMLElement>("[data-command-pane-id]")).map(
      (pane) => [
        pane.dataset.commandId,
        pane.style.left,
        pane.style.top,
        pane.style.width,
        pane.style.height,
      ],
    );
    // The reference: the SAME tree the settings screen's vignettes draw, resolved through the
    // shared `paneRects`/`paneRectStyle` pair — not a second layout engine.
    const expected = paneRects(commandGroupTree(members)!).map((placement) => {
      const style = paneRectStyle(placement.rect);
      return [
        placement.terminalId,
        String(style.left),
        String(style.top),
        String(style.width),
        String(style.height),
      ];
    });
    expect(drawn).toEqual(expected);
    // Three members ⇒ 2 side by side with the 3rd underneath the second (finding #90 criterion 4).
    expect(expected.map((e) => e[0])).toEqual(["c1", "c2", "c3"]);
    expect(expected[0].slice(1)).toEqual(["0%", "0%", "50%", "100%"]);
  });

  it("picks the group from ANY member — the group is the surface, not the clicked row", () => {
    const { container } = renderGrouped([grouped], running, "s3");
    expect(container.querySelectorAll("[data-command-pane-id]")).toHaveLength(3);
  });

  it("keeps the single output pane for a SOLO command", () => {
    const { container } = renderGrouped([], running, "s1");
    expect(container.querySelector("[data-command-pane-host]")).toBeNull();
  });

  it("never borrows another workspace's instance for a member", () => {
    const elsewhere = running.map((s) =>
      s.command_id === "c3" ? { ...s, workspace_id: "ws2" } : s,
    );
    const { container } = renderGrouped([grouped], elsewhere, "s1");
    const ids = Array.from(container.querySelectorAll<HTMLElement>("[data-command-pane-id]")).map(
      (pane) => pane.dataset.commandId,
    );
    expect(ids).toEqual(["c1", "c2"]);
  });
});
