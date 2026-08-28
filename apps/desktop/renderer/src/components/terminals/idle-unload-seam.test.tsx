import { act, render, waitFor } from "@testing-library/react";
import { bridgeFake } from "@/bridge/test-harness";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  __idleUnloadSeamHolders,
  __resetIdleUnloadSeam,
  claimIdleUnloadSeam,
  type IdleUnloadSweep,
} from "./idle-unload-seam";
import { RuntimeSelectionProvider } from "./runtime-selection";
import { useIdleUnload } from "./use-idle-unload";
import { usePaneLayout, type PaneLayoutController, type SplitWorkspace } from "./use-pane-layout";
import type { WorkspaceRuntimeTerminal } from "@/components/runtime/use-workspace-runtime";

/**
 * THE IDLE-UNLOAD READ SEAM (`window.__nyxIdleUnload`) — review finding F4.
 *
 * Two things are pinned here. The seam's OWN contract (published on the `__nyxDeck` pattern,
 * refcounted, reporting the last sweep's timestamp / threshold / visited set), and the reason it
 * exists: the PRD's rule that the terminals PROTECTED from the sweep are every VISIBLE pane —
 * not the focused one — is a claim about the arguments of a fire-and-forget invoke, and this is
 * the only place it can be read from.
 */

// `usePaneLayout` speaks to the user through the toast on a refused split; nothing here refuses,
// but the module must not reach a real toast host in jsdom.
vi.mock("@/components/ui/toast", () => ({
  toast: { error: vi.fn(), success: vi.fn(), info: vi.fn() },
}));

interface SeamWindow {
  __nyxIdleUnload?: () => IdleUnloadSweep | null;
}

const seam = () => (window as unknown as SeamWindow).__nyxIdleUnload;
/** The last sweep as a REAL-DRIVE reader sees it: through the global, never through the module. */
const lastSweep = () => seam()?.() ?? null;

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

beforeEach(() => {
  window.localStorage.clear();
  __resetIdleUnloadSeam();
});

afterEach(() => {
  window.localStorage.clear();
  expect(__idleUnloadSeamHolders(), "every test releases its claims").toBe(0);
  expect(seam(), "the global leaves with its last holder").toBeUndefined();
});

describe("claimIdleUnloadSeam (the ownership rule, straight off deck-seam)", () => {
  it("publishes on the FIRST claim and removes only on the LAST release", () => {
    expect(seam()).toBeUndefined();
    const releaseA = claimIdleUnloadSeam();
    const published = seam();
    expect(published).toBeDefined();

    // A second holder does NOT republish — the same live reader stays in place…
    const releaseB = claimIdleUnloadSeam();
    expect(seam()).toBe(published);

    // …and the first release leaves the seam standing for the remaining holder.
    releaseA();
    expect(seam()).toBe(published);
    // Releasing twice is a no-op — it must not drop someone else's claim.
    releaseA();
    expect(seam()).toBe(published);

    releaseB();
    expect(seam()).toBeUndefined();
  });

  it("reports NO sweep before the first one has run", () => {
    const release = claimIdleUnloadSeam();
    expect(lastSweep()).toBeNull();
    release();
  });
});

describe("useIdleUnload publishes the seam and records every sweep", () => {
  it("reflects the LAST sweep: the timestamp advances, threshold and visited match the invoke", async () => {
    const fake = bridgeFake();
    fake.on("unload_idle_sessions", () => []);

    const { rerender, unmount } = render(<Sweeper visited={["t-a", "t-b"]} />);

    await waitFor(() => expect(lastSweep(), "a sweep was recorded").not.toBeNull());
    const first = lastSweep() as IdleUnloadSweep;
    expect(first.thresholdSecs).toBe(900);
    expect(first.visited).toEqual(["t-a", "t-b"]);
    // …and it is EXACTLY what the backend received, argument for argument.
    const invoked = fake.calls.filter((c) => c.command === "unload_idle_sessions");
    expect(invoked.length).toBeGreaterThan(0);
    expect(invoked[invoked.length - 1]?.args).toEqual({
      thresholdSecs: first.thresholdSecs,
      visited: first.visited,
    });

    // The focus moves: the NEXT sweep must carry the new set and a LATER timestamp — a seam that
    // froze on the first sweep would be worse than none.
    rerender(<Sweeper visited={["t-c"]} />);
    await waitFor(() =>
      expect((lastSweep() as IdleUnloadSweep).visited, "the seam followed the focus").toEqual([
        "t-c",
      ]),
    );
    const second = lastSweep() as IdleUnloadSweep;
    expect(second.at).toBeGreaterThanOrEqual(first.at);
    expect(second.visited).toEqual(["t-c"]);

    unmount();
  });

  it("COPIES the visited set — a later focus change cannot rewrite a recorded sweep", async () => {
    const fake = bridgeFake();
    fake.on("unload_idle_sessions", () => []);
    const live = ["t-a"];
    const { unmount } = render(<Sweeper visited={live} />);
    await waitFor(() => expect(lastSweep()).not.toBeNull());
    const recorded = lastSweep() as IdleUnloadSweep;
    live.push("t-mutated");
    expect(recorded.visited).toEqual(["t-a"]);
    unmount();
  });

  it("keeps the seam alive while ANY host sweeps, and takes it away with the last one", async () => {
    const fake = bridgeFake();
    fake.on("unload_idle_sessions", () => []);
    const one = render(<Sweeper visited={["t-a"]} />);
    const two = render(<Sweeper visited={["t-b"]} />);
    await waitFor(() => expect(__idleUnloadSeamHolders()).toBe(2));

    one.unmount();
    expect(seam(), "the surviving host still publishes it").toBeDefined();
    two.unmount();
    expect(seam()).toBeUndefined();
  });

  it("is INERT: nothing in the application reads it, and reading it changes nothing", async () => {
    const fake = bridgeFake();
    fake.on("unload_idle_sessions", () => []);
    const { unmount } = render(<Sweeper visited={["t-a"]} />);
    await waitFor(() => expect(lastSweep()).not.toBeNull());

    const before = fake.calls.filter((c) => c.command === "unload_idle_sessions").length;
    for (let i = 0; i < 5; i++) lastSweep();
    expect(
      fake.calls.filter((c) => c.command === "unload_idle_sessions").length,
      "reading the seam issues no command",
    ).toBe(before);
    unmount();
  });
});

/** The hook under test, on a fast cadence so a sweep lands inside a `waitFor`. */
function Sweeper({ visited }: { visited: readonly string[] }) {
  useIdleUnload(visited, { intervalMs: 20 });
  return null;
}

/* ─────────── THE PRD'S CENTRAL RULE, READ THROUGH THE SEAM ─────────── */

/** The live controller of the last render — the page's own wiring, minus the page. */
let controller: PaneLayoutController;

/**
 * The Terminals page's EXACT wiring between the tiling and the sweep (`terminals-page.tsx`): the
 * visited set handed to `useIdleUnload` is `visibleTerminalIds` — every leaf of the pane tree —
 * and NOT the focused pane's terminal.
 */
function TilingSweeper({
  terminals,
  createTerminal,
}: {
  terminals: WorkspaceRuntimeTerminal[];
  createTerminal: (workspaceId: string, path: string) => Promise<string>;
}) {
  controller = usePaneLayout({
    projectId: "p1",
    terminals,
    workspaces: WORKSPACES,
    createTerminal,
    // jsdom has no layout engine, so the absolute pane floor cannot measure anything: an
    // unmeasured host never refuses a split (see `canSplitPane`).
    measureHost: () => null,
  });
  useIdleUnload(controller.visibleTerminalIds, { intervalMs: 20 });
  return null;
}

describe("the visited set is EVERY VISIBLE PANE — proved through the seam", () => {
  it("carries all three tiled terminals, not just the focused one", async () => {
    const fake = bridgeFake();
    fake.on("unload_idle_sessions", () => []);
    let created = 1;
    const createTerminal = vi.fn(async () => `t${++created}`);

    const { unmount } = render(
      <RuntimeSelectionProvider>
        <TilingSweeper terminals={[terminal("t1")]} createTerminal={createTerminal} />
      </RuntimeSelectionProvider>,
    );
    await waitFor(() => expect(controller.focusedPaneId).not.toBeNull());

    // Tile up to THREE panes — the shape the PRD is about (several shells side by side).
    await act(async () => {
      controller.splitPane(controller.focusedPaneId as string, "row");
    });
    await waitFor(() => expect(controller.visibleTerminalIds).toHaveLength(2));
    await act(async () => {
      controller.splitPane(controller.focusedPaneId as string, "column");
    });
    await waitFor(() => expect(controller.visibleTerminalIds).toHaveLength(3));

    // ONE pane holds the focus…
    const focused = controller.focusedTerminalId as string;
    expect(focused).not.toBeNull();

    // …yet the sweep the sidecar receives declares ALL THREE as visited. Read through the SEAM —
    // the only vantage point a piloted session has on this call.
    await waitFor(() =>
      expect(lastSweep()?.visited, "the seam saw a three-terminal sweep").toHaveLength(3),
    );
    const sweep = lastSweep() as IdleUnloadSweep;
    expect([...sweep.visited].sort()).toEqual([...controller.visibleTerminalIds].sort());
    expect(sweep.visited).toContain(focused);
    // THE POINT: the two panes that do NOT hold the focus are protected too. A "focused only"
    // implementation would have sent exactly one id.
    expect(sweep.visited.filter((id) => id !== focused)).toHaveLength(2);
    expect(sweep.visited.length).toBeGreaterThan(1);

    unmount();
  });
});
