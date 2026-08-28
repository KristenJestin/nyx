import { useEffect, useRef } from "react";
import { render } from "@testing-library/react";
import { page } from "vitest/browser";
import { afterEach, beforeEach, expect, it } from "vitest";

// The REAL Tailwind utilities: the panes are sized by `h-full`, so without this stylesheet the
// tiling would have no boxes and "the VISIBLE panes" would be a claim about nothing.
import "@/globals.css";

import { nyxBridge } from "@/bridge";
import type { FakeNyxBridge } from "@/bridge/fake";
import { __resetTerminalStore } from "@/components/terminal/terminal-store";
import type { TerminalRecord } from "@/components/sidebar/use-terminals";
import { __resetIdleUnloadSeam, type IdleUnloadSweep } from "./idle-unload-seam";
import { PaneTreeHost } from "./pane-tree-host";
import { RuntimeSelectionProvider } from "./runtime-selection";
import { useIdleUnload } from "./use-idle-unload";
import { usePaneLayout, type PaneLayoutController, type SplitWorkspace } from "./use-pane-layout";
import type { WorkspaceRuntimeTerminal } from "@/components/runtime/use-workspace-runtime";

/**
 * REAL-DRIVE READ OF THE IDLE-UNLOAD SEAM (review finding F4, criterion 6).
 *
 * jsdom can prove the wiring; it cannot prove that a REAL renderer, with real panes really laid
 * out side by side, publishes `window.__nyxIdleUnload` and reports the whole visible set through
 * it. That is the observation the review says is missing from the product, so it is taken here in
 * headless Chromium, exactly the way a piloted session would take it: read the global, print what
 * it says.
 *
 * THE BACKEND DOUBLE, in browser mode: `@/bridge/test-harness`'s `vi.mock("@/bridge")` does NOT
 * apply here (vitest only hoists `vi.mock` written IN the test file). It is not needed either:
 * without the Electron preload's `window.nyxCore`, `@/bridge` already resolves to a live
 * `FakeNyxBridge` — so the double IS `nyxBridge`, and we drive it directly.
 */
const backend = nyxBridge as unknown as FakeNyxBridge;

/** Every `unload_idle_sessions` the backend really received, in order. */
const received: { thresholdSecs: number; visited: string[] }[] = [];

interface SeamWindow {
  __nyxIdleUnload?: () => IdleUnloadSweep | null;
}
const readSeam = () => (window as unknown as SeamWindow).__nyxIdleUnload?.() ?? null;

beforeEach(async () => {
  await page.viewport(1400, 900);
  received.length = 0;
  __resetIdleUnloadSeam();
  backend.reset();
  let nextPtyId = 100;
  backend.onAnyCommand((cmd, args) => {
    if (cmd === "pty_spawn") return nextPtyId++;
    if (cmd === "list_terminals") return [];
    if (cmd === "terminal_info") return { cwd: null, foreground: null };
    if (cmd === "unload_idle_sessions") {
      received.push({
        thresholdSecs: Number(args?.thresholdSecs),
        visited: [...((args?.visited as string[]) ?? [])],
      });
      return [];
    }
    return null;
  });
});

afterEach(() => {
  __resetTerminalStore();
  document.body.replaceChildren();
});

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

function runtime(id: string): WorkspaceRuntimeTerminal {
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

async function waitFor(pred: () => boolean, timeoutMs = 8000): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (pred()) return true;
    await new Promise((r) => setTimeout(r, 25));
  }
  return pred();
}

let controller: PaneLayoutController;

/**
 * The Terminals page's own wiring, minus the page: the tiling owns which terminals are VISIBLE,
 * and that very set — not the selection — is what feeds the idle-unload sweep.
 */
function Page({ terminals }: { terminals: WorkspaceRuntimeTerminal[] }) {
  // A STABLE counter: a per-render one would hand the same id to the second split, and the tree's
  // one-terminal-one-pane invariant would refuse it — the tiling would silently stop at two panes.
  const nextId = useRef(1);
  controller = usePaneLayout({
    projectId: "seam-p1",
    terminals,
    workspaces: WORKSPACES,
    createTerminal: async () => `t${++nextId.current}`,
  });
  useIdleUnload(controller.visibleTerminalIds, { intervalMs: 40, thresholdSecs: 900 });
  // Split twice, once the tiling is live — the real transition, through the real guard.
  useEffect(() => {
    if (controller.focusedPaneId == null) return;
    if (controller.visibleTerminalIds.length >= 3) return;
    controller.splitPane(
      controller.focusedPaneId,
      controller.visibleTerminalIds.length === 1 ? "row" : "column",
    );
  }, [controller.focusedPaneId, controller.visibleTerminalIds.length]);
  return (
    <PaneTreeHost
      terminals={terminals.map((t) => record(t.id))}
      layout={controller.layout}
      focusedPaneId={controller.focusedPaneId}
      onFocusPane={controller.focusPane}
      onSplitPane={controller.splitPane}
      onClosePane={controller.closePane}
      onResizeSplit={controller.resizeSplit}
    />
  );
}

it("F4: a REAL renderer publishes the seam, and it reports every VISIBLE pane", async () => {
  const host = document.createElement("div");
  host.style.cssText = "position:fixed;top:0;left:0;width:1200px;height:700px;";
  document.body.appendChild(host);

  // Three live terminals, tiled: the split path creates t2 and t3, and the page re-renders with
  // all three in the runtime list — which is what a real project looks like after two splits.
  const { rerender } = render(
    <RuntimeSelectionProvider>
      <Page terminals={[runtime("t1")]} />
    </RuntimeSelectionProvider>,
    { container: host },
  );
  await waitFor(() => controller.visibleTerminalIds.length >= 2);
  rerender(
    <RuntimeSelectionProvider>
      <Page terminals={[runtime("t1"), runtime("t2"), runtime("t3")]} />
    </RuntimeSelectionProvider>,
  );
  expect(
    await waitFor(() => controller.visibleTerminalIds.length === 3),
    "three panes are tiled",
  ).toBe(true);

  // The panes are REALLY on screen, each with a box — "visible" is a measurement here, not a flag.
  const tiled = Array.from(
    host.querySelectorAll<HTMLElement>('[data-terminal-id][data-visible="true"]'),
  );
  expect(tiled.length, "three visible pane slots").toBe(3);
  for (const pane of tiled) {
    expect(
      pane.getBoundingClientRect().width,
      `${pane.dataset.terminalId} has a box`,
    ).toBeGreaterThan(0);
  }

  // NOW READ THE SEAM — the whole point of the finding: from outside the app, with no access to
  // the bridge, no patch on `nyxCore`, no instrumentation of the preload.
  expect(
    await waitFor(() => (readSeam()?.visited.length ?? 0) === 3),
    "the seam reports a three-terminal sweep",
  ).toBe(true);
  const sweep = readSeam() as IdleUnloadSweep;

  // OBSERVED (headless Chromium, 1200×700 host, three tiled panes), read off the global:
  //   {"at":1786974899511,"thresholdSecs":900,"visited":["t1","t2","t3"]}   focused = "t3"
  // — the focused pane is ONE of the three; the other two are in the protected set all the same.
  expect(sweep.thresholdSecs, "the threshold the sidecar was handed").toBe(900);
  expect(sweep.at, "a real timestamp").toBeGreaterThan(0);
  expect([...sweep.visited].sort()).toEqual(["t1", "t2", "t3"]);
  // …and it is the tiling's own visible set, not the selection.
  expect([...sweep.visited].sort()).toEqual([...controller.visibleTerminalIds].sort());
  const focused = controller.focusedTerminalId as string;
  expect(sweep.visited).toContain(focused);
  expect(
    sweep.visited.filter((id) => id !== focused),
    "the two UNFOCUSED panes are protected too",
  ).toHaveLength(2);

  // The seam is a MIRROR, not a story: the backend really received those very arguments…
  const last = received[received.length - 1] as { thresholdSecs: number; visited: string[] };
  expect(last.thresholdSecs).toBe(sweep.thresholdSecs);
  expect([...last.visited].sort()).toEqual([...sweep.visited].sort());

  // …and its timestamp ADVANCES with the sweeps, so a reader can tell a live tick from a stale one.
  const before = sweep.at;
  expect(
    await waitFor(() => (readSeam() as IdleUnloadSweep).at > before),
    "the seam follows the next tick",
  ).toBe(true);
});

it("F4: the bridge is untouched — the seam reads, it does not open anything", async () => {
  const host = document.createElement("div");
  host.style.cssText = "position:fixed;top:0;left:0;width:1200px;height:700px;";
  document.body.appendChild(host);
  render(
    <RuntimeSelectionProvider>
      <Page terminals={[runtime("t1")]} />
    </RuntimeSelectionProvider>,
    { container: host },
  );
  expect(await waitFor(() => readSeam() != null), "the seam is published").toBe(true);

  // The published surface is EXACTLY one read-only function: no setter, no bridge handle, no way
  // to make the app do anything it was not already doing.
  const published = (window as unknown as SeamWindow).__nyxIdleUnload as () => unknown;
  expect(typeof published).toBe("function");
  expect(published.length, "it takes no arguments").toBe(0);
  const swept = received.length;
  for (let i = 0; i < 5; i++) published();
  expect(received.length, "reading the seam issues no command").toBe(swept);
});
