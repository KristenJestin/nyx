import { StrictMode, useCallback, useMemo, useState } from "react";
import { act, fireEvent, render } from "@testing-library/react";
import { afterEach, beforeEach, expect, it } from "vitest";
import { page } from "vitest/browser";

// The REAL Tailwind utilities — the panes' boxes (and therefore every rectangle this file
// measures) come from the app's own layout classes, not from the test's markup.
import "@/globals.css";

import { nyxBridge } from "@/bridge";
import type { FakeNyxBridge } from "@/bridge/fake";
import type { TerminalRecord } from "@/components/sidebar/use-terminals";
import { __resetTerminalStore, getTerminal } from "@/components/terminal/terminal-store";
import { savePaneCollection } from "./pane-layout-persistence";
import { PaneTreeHost } from "./pane-tree-host";
import { paneLeaves, type PaneEdge, type PaneNode } from "@/components/panes/pane-tree";
import { RuntimeSelectionProvider, useRuntimeSelection } from "./runtime-selection";
import { saveLastTerminalId } from "./ui-persistence";
import { usePaneLayout, type PaneLayoutController, type SplitWorkspace } from "./use-pane-layout";
import type { WorkspaceRuntimeTerminal } from "@/components/runtime/use-workspace-runtime";

/**
 * THE VIEW'S RESTRUCTURE GESTURE, IN REAL CHROMIUM (review 01M0CY7R5Z7KV5R49FQYXH9QRN).
 *
 * The unit spec proves the tree arithmetic; THIS file proves what only a layout engine can:
 * a REAL pointer drag by the pane's grip lands on the hovered EDGE of the target pane, the drop
 * indicator is a VISIBLE rectangle over half the target (reachable by `elementFromPoint`), no PTY
 * is ever spawned twice, and the moved terminal is re-fitted to its new box through the existing
 * ResizeObserver → FitAddon → `pty_resize` pipeline. The floor refusals are measured in pixels.
 *
 * Same standing as `pane-tiling-gate.browser.test.tsx`: real Chromium, real layout, but the
 * backend is the {@link FakeNyxBridge} double (no Electron, no sidecar, no real PTY).
 */
const backend = nyxBridge as unknown as FakeNyxBridge;

/** A PTY grid — what `pty_spawn` / `pty_resize` carry. */
interface PtySize {
  cols: number;
  rows: number;
}

const spawnCounts = new Map<string, number>();
const ptyIds = new Map<string, number>();
const writes = new Map<string, string>();
const resizes = new Map<string, PtySize>();
const decoder = new TextDecoder();
const encoder = new TextEncoder();

beforeEach(() => {
  spawnCounts.clear();
  ptyIds.clear();
  writes.clear();
  resizes.clear();
  window.localStorage.clear();
  backend.reset();
  let nextPtyId = 300;
  backend.onAnyCommand((cmd, args) => {
    if (cmd === "pty_spawn") {
      const terminalId = typeof args?.terminalId === "string" ? args.terminalId : "";
      spawnCounts.set(terminalId, (spawnCounts.get(terminalId) ?? 0) + 1);
      const ptyId = nextPtyId++;
      ptyOwners.set(ptyId, terminalId);
      ptyIds.set(terminalId, ptyId);
      return ptyId;
    }
    if (cmd === "pty_write") {
      const owner = ptyOwners.get(Number(args?.id));
      const bytes = Array.isArray(args?.data) ? (args.data as number[]) : [];
      if (owner != null) {
        writes.set(owner, (writes.get(owner) ?? "") + decoder.decode(Uint8Array.from(bytes)));
      }
      return null;
    }
    if (cmd === "pty_resize") {
      const owner = ptyOwners.get(Number(args?.id));
      if (owner != null) resizes.set(owner, { cols: Number(args?.cols), rows: Number(args?.rows) });
      return null;
    }
    if (cmd === "list_terminals") return [];
    if (cmd === "terminal_info") return { cwd: null, foreground: null };
    return null;
  });
});

const ptyOwners = new Map<number, string>();

afterEach(() => {
  __resetTerminalStore();
  document.body.replaceChildren();
  window.localStorage.clear();
});

async function waitFor(predicate: () => boolean, timeoutMs = 5000): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (predicate()) return true;
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
  return predicate();
}

/** The runner's viewport — every host must FIT INSIDE it or xterm stops painting (see the gate). */
const VIEWPORT = { width: 414, height: 896 };

function mountHost(width: number, height: number): HTMLElement {
  expect(width).toBeLessThanOrEqual(Math.min(VIEWPORT.width, window.innerWidth));
  expect(height).toBeLessThanOrEqual(Math.min(VIEWPORT.height, window.innerHeight));
  const host = document.createElement("div");
  host.style.cssText = `position:fixed;top:0;left:0;width:${width}px;height:${height}px`;
  document.body.appendChild(host);
  return host;
}

const WORKSPACES: SplitWorkspace[] = [{ id: "ws-root", path: "/repo", is_root: true }];

function runtimeTerminal(id: string): WorkspaceRuntimeTerminal {
  return {
    id,
    cwd: "/repo",
    label: null,
    workspace_id: "ws-root",
    status: "alive",
    exec_state: "idle",
    exec_state_unread: false,
    sessions: [],
  };
}

let controller: PaneLayoutController;
/** The ids the harness' `createTerminal` handed out — a move must leave it EMPTY. */
let creations: string[];

function MovePage({ initial }: { initial: readonly WorkspaceRuntimeTerminal[] }) {
  const [terminals, setTerminals] = useState(initial);
  const createTerminal = useCallback(async (_ws: string, _path: string) => {
    const id = `created-${creations.length}`;
    creations.push(id);
    setTerminals((current) => [...current, runtimeTerminal(id)]);
    return id;
  }, []);
  controller = usePaneLayout({
    projectId: "p-move-browser",
    terminals,
    workspaces: WORKSPACES,
    createTerminal,
  });
  const { select } = useRuntimeSelection();
  void select;
  const records = useMemo<TerminalRecord[]>(
    () =>
      terminals.map((t) => ({
        id: t.id,
        cwd: t.cwd,
        label: t.label,
        scrollback: "",
        status: t.status,
        order_index: 0,
        created_at: 0,
        updated_at: 0,
        closed_at: null,
      })),
    [terminals],
  );
  return (
    <PaneTreeHost
      terminals={records}
      layout={controller.layout}
      focusedPaneId={controller.focusedPaneId}
      onFocusPane={controller.focusPane}
      onSplitPane={controller.splitPane}
      onClosePane={controller.closePane}
      onResizeSplit={controller.resizeSplit}
      onMovePane={controller.movePane}
      canMovePane={controller.canMovePane}
    />
  );
}

/**
 * `ta | tb` side by side at 50/50, stable pane ids — persisted where the restore reads it, so the
 * mounted page comes back as THAT tiling (the same way a user's session does).
 */
function seedRow(): PaneNode {
  const tree: PaneNode = {
    kind: "split",
    id: "s-root",
    direction: "row",
    ratio: 0.5,
    children: [
      { kind: "leaf", id: "p-a", terminalId: "ta" },
      { kind: "leaf", id: "p-b", terminalId: "tb" },
    ],
  };
  savePaneCollection("p-move-browser", {
    groups: [{ id: "g1", tree, name: null, color: null }],
  });
  saveLastTerminalId("p-move-browser", "ta");
  return tree;
}

async function mountTiling(width: number, height: number): Promise<HTMLElement> {
  seedRow();
  creations = [];
  const host = mountHost(width, height);
  host.dataset.testid = "pane-move-host";
  render(
    <RuntimeSelectionProvider>
      <MovePage initial={[runtimeTerminal("ta"), runtimeTerminal("tb")]} />
    </RuntimeSelectionProvider>,
    { container: host },
  );
  expect(
    await waitFor(() => host.querySelectorAll("[data-visible='true']").length === 2),
    "both panes came back",
  ).toBe(true);
  expect(await waitFor(() => ["ta", "tb"].every((id) => spawnCounts.get(id) === 1))).toBe(true);
  return host;
}

function paneEl(host: HTMLElement, terminalId: string): HTMLElement {
  return host.querySelector(`[data-terminal-id="${terminalId}"]`) as HTMLElement;
}

function gripEl(host: HTMLElement, paneId: string): HTMLElement {
  return host.querySelector(`[data-pane-grip="${paneId}"]`) as HTMLElement;
}

/** The EDGE BAND — the "I am about to cut here" vocabulary. */
function indicator(host: HTMLElement): HTMLElement | null {
  return host.querySelector("[data-pane-drop-indicator]");
}

/**
 * The PERMUTATION vocabulary (review finding 01M11WFPFZGAC68VGXJSG6QNMK) — the two whole panes
 * outlined because they are about to exchange places rather than be cut.
 */
function swapZones(host: HTMLElement): HTMLElement[] {
  return Array.from(host.querySelectorAll<HTMLElement>("[data-pane-swap-indicator]"));
}

/** ANY drop announcement, whichever vocabulary it speaks — `null` when the hover shows nothing. */
function dropSignal(host: HTMLElement): HTMLElement | null {
  return indicator(host) ?? swapZones(host)[0] ?? null;
}

/** A PointerEvent as a real device delivers it. */
function ptr(type: string, x: number, y: number, buttons: number): PointerEvent {
  return new PointerEvent(type, {
    pointerId: 1,
    pointerType: "mouse",
    isPrimary: true,
    button: 0,
    buttons,
    clientX: x,
    clientY: y,
    bubbles: true,
    cancelable: true,
  });
}

/** Let the browser settle (React commit + paint + the indicator's layout). */
function frames(count = 3): Promise<void> {
  return new Promise((resolve) => {
    let left = count;
    const tick = () => (left-- <= 0 ? resolve() : requestAnimationFrame(tick));
    requestAnimationFrame(tick);
  });
}

/**
 * A REAL pointer drag of `paneId`'s grip towards an absolute point, IN STEPS — press, walk, hold.
 * Returns a `release` the caller fires after asserting whatever the HOLD must show (the indicator).
 */
async function grabAndMoveTo(
  grip: HTMLElement,
  to: { x: number; y: number },
  steps = 6,
): Promise<{ release: () => Promise<void>; cursor: { x: number; y: number } }> {
  const box = grip.getBoundingClientRect();
  let x = box.left + box.width / 2;
  let y = box.top + box.height / 2;
  fireEvent(grip, ptr("pointerdown", x, y, 1));
  await frames(2);
  for (let i = 1; i <= steps; i++) {
    x += (to.x - x) / (steps - i + 1);
    y += (to.y - y) / (steps - i + 1);
    fireEvent(grip, ptr("pointermove", x, y, 1));
    await frames(1);
  }
  fireEvent(grip, ptr("pointermove", to.x, to.y, 1));
  await frames(2);
  return {
    cursor: { x: to.x, y: to.y },
    release: async () => {
      fireEvent(grip, ptr("pointerup", to.x, to.y, 0));
      await frames(4);
    },
  };
}

/** The point INSIDE `target`'s outer edge band on `edge` (a 12% inset keeps it off the corner). */
function edgePoint(target: DOMRect, edge: PaneEdge): { x: number; y: number } {
  const cx = target.left + target.width / 2;
  const cy = target.top + target.height / 2;
  switch (edge) {
    case "left":
      return { x: target.left + target.width * 0.12, y: cy };
    case "right":
      return { x: target.right - target.width * 0.12, y: cy };
    case "up":
      return { x: cx, y: target.top + target.height * 0.12 };
    case "down":
      return { x: cx, y: target.bottom - target.height * 0.12 };
  }
}

it("criterion 1 — a LEFT-edge drop splits as a row, neighbour at 50/50, and creates NOTHING", async () => {
  const host = await mountTiling(400, 700);
  const before = new Map(spawnCounts);
  const instances = new Map(["ta", "tb"].map((id) => [id, getTerminal(id)]));

  const drag = await grabAndMoveTo(
    gripEl(host, "p-b"),
    edgePoint(paneEl(host, "ta").getBoundingClientRect(), "left"),
  );
  await drag.release();

  // THE TREE: tb detached, ta's slot split row-wise, tb FIRST (it landed on the left), 50/50.
  const layout = controller.layout as Extract<PaneNode, { kind: "split" }>;
  expect(layout.kind).toBe("split");
  expect(layout.direction).toBe("row");
  expect(layout.ratio).toBe(0.5);
  expect(paneLeaves(layout).map((l) => l.terminalId)).toEqual(["tb", "ta"]);
  expect(paneLeaves(layout).map((l) => l.id)).toEqual(["p-b", "p-a"]);

  // NO CREATION, NO LOSS: not one new PTY, not one new terminal record, same store instances.
  expect(creations, "createTerminal was never called").toEqual([]);
  for (const id of ["ta", "tb"]) {
    expect(spawnCounts.get(id), `${id} still owns exactly one PTY`).toBe(before.get(id));
    expect(getTerminal(id), `${id} kept its instance`).toBe(instances.get(id));
    expect(host.querySelectorAll(`[data-terminal-node="${id}"]`).length, "one attachment").toBe(1);
  }
});

it("criterion 2 — all FOUR edges: the split direction is PERPENDICULAR to the hovered edge", async () => {
  const cases: readonly {
    readonly edge: PaneEdge;
    readonly direction: "row" | "column";
    readonly order: readonly string[];
  }[] = [
    { edge: "left", direction: "row", order: ["tb", "ta"] },
    { edge: "right", direction: "row", order: ["ta", "tb"] },
    { edge: "up", direction: "column", order: ["tb", "ta"] },
    { edge: "down", direction: "column", order: ["ta", "tb"] },
  ];
  for (const { edge, direction, order } of cases) {
    const host = await mountTiling(400, 700);
    const drag = await grabAndMoveTo(
      gripEl(host, "p-b"),
      edgePoint(paneEl(host, "ta").getBoundingClientRect(), edge),
    );
    await drag.release();

    const layout = controller.layout as Extract<PaneNode, { kind: "split" }>;
    expect(layout.direction, `${edge}: perpendicular direction`).toBe(direction);
    expect(layout.ratio, `${edge}: 50/50`).toBe(0.5);
    expect(
      paneLeaves(layout).map((l) => l.terminalId),
      `${edge}: which side`,
    ).toEqual(order);
    expect(creations, `${edge}: nothing created`).toEqual([]);
  }
});

it("criterion 3 — the floor REFUSES: no indicator during an invalid hover, tree untouched on drop", async () => {
  // 220px of height: an UP drop would seat two 110px-tall panes — under the 120px floor.
  const host = await mountTiling(400, 220);
  const before = controller.groups[0].tree;
  const spawnsBefore = new Map(spawnCounts);

  // INVALID hover: the up band of ta shows NOTHING.
  const bad = await grabAndMoveTo(
    gripEl(host, "p-b"),
    edgePoint(paneEl(host, "ta").getBoundingClientRect(), "up"),
  );
  await frames(2);
  expect(dropSignal(host), "an invalid hover never announces itself").toBeNull();
  await bad.release();

  expect(controller.groups[0].tree, "the tree is unchanged, by reference").toBe(before);
  expect(controller.layout).toBe(before);
  for (const id of ["ta", "tb"]) expect(spawnCounts.get(id)).toBe(spawnsBefore.get(id));

  // CONTRAST, same host: the LEFT band fits (200px halves ≥ 180px) and DOES announce itself —
  // the refusal above was geometric, not a broken affordance.
  const good = await grabAndMoveTo(
    gripEl(host, "p-b"),
    edgePoint(paneEl(host, "ta").getBoundingClientRect(), "left"),
  );
  await frames(2);
  expect(dropSignal(host), "the valid hover shows the zone").not.toBeNull();
  await good.release();
  expect(paneLeaves(controller.layout as PaneNode).map((l) => l.terminalId)).toEqual(["tb", "ta"]);
});

it("criterion 4 — the indicator is a VISIBLE rectangle over the hovered HALF, hit by elementFromPoint", async () => {
  // FOUR PANES, deliberately: this criterion is about the CUT vocabulary, and a two-pane drop is a
  // permutation, which now draws something else (see the swap specs below). Dropping the top-left
  // quadrant onto a quadrant of the bottom row really does restructure, so the band is the honest
  // announcement and its geometry is what is under test.
  const { host } = await mountSpiedHost(false);
  const targetBox = paneEl(host, "td").getBoundingClientRect();

  const drag = await grabAndMoveTo(gripEl(host, "q-a"), edgePoint(targetBox, "left"));
  await frames(2);

  const zone = indicator(host) as HTMLElement;
  expect(zone, "the indicator exists during the gesture").not.toBeNull();
  expect(zone.getAttribute("data-pane-drop-indicator")).toBe("left");
  expect(zone.getAttribute("data-drop-target-pane")).toBe("q-d");
  expect(swapZones(host), "a real restructure never speaks the swap vocabulary").toHaveLength(0);

  // GEOMETRY, not attributes: non-zero, ON the left half of the TARGET's box, nowhere else.
  const box = zone.getBoundingClientRect();
  const message = `indicator{${box.left.toFixed(1)},${box.top.toFixed(1)} ${box.width.toFixed(1)}×${box.height.toFixed(1)}} vs td{${targetBox.left.toFixed(1)},${targetBox.top.toFixed(1)} ${targetBox.width.toFixed(1)}×${targetBox.height.toFixed(1)}}`;
  expect(box.width, message).toBeGreaterThan(0);
  expect(box.height, message).toBeGreaterThan(0);
  expect(box.left, message).toBeGreaterThanOrEqual(targetBox.left - 1);
  expect(box.right, message).toBeCloseTo(targetBox.left + targetBox.width / 2, 0);
  expect(box.top, message).toBeCloseTo(targetBox.top, 0);
  expect(box.height, message).toBeCloseTo(targetBox.height, 0);

  // REACHABLE: the element answering at the zone's centre IS the indicator (or lives inside it).
  const centre = document.elementFromPoint(box.left + box.width / 2, box.top + box.height / 2);
  expect(centre, "elementFromPoint reaches the indicator").not.toBeNull();
  expect(zone.contains(centre) || centre === zone, "not behind another element").toBe(true);

  // …and the MIDDLE of the target is NOT a drop zone: no edge band, no indicator at all.
  const mid = { x: targetBox.left + targetBox.width / 2, y: targetBox.top + targetBox.height / 2 };
  fireEvent(gripEl(host, "q-a"), ptr("pointermove", mid.x, mid.y, 1));
  await frames(2);
  expect(dropSignal(host), "the centre of a pane is not an edge").toBeNull();

  await drag.release();
});

/* ─────────── THE PERMUTATION VOCABULARY (review finding 01M11WFPFZGAC68VGXJSG6QNMK) ───────────
 *
 * « il doit capter juste quand c'est du swap parce que ce truc d'afficher au dessus d'un autre
 * terminal c'est ok, sauf quand ça doit swap ». At two panes, dropping A on the far edge of B
 * detaches A, leaves B alone and re-inserts A on the other side: nothing is cut, the two terminals
 * EXCHANGE places. The band announced a split that never happens.
 *
 * The recognition is a DRY RUN of `movePaneToEdge` compared to the tree on screen — never a case on
 * the pane count — so these two specs pin BOTH families on the same gesture: two panes ⇒ swap,
 * four panes ⇒ band (criterion 4 above).
 */
it("a drop that PERMUTES shows the two panes exchanging, not an edge band", async () => {
  const host = await mountTiling(400, 700);
  const taBox = paneEl(host, "ta").getBoundingClientRect();
  const tbBox = paneEl(host, "tb").getBoundingClientRect();

  const drag = await grabAndMoveTo(gripEl(host, "p-b"), edgePoint(taBox, "left"));
  await frames(2);

  expect(indicator(host), "no edge band: nothing is being cut here").toBeNull();
  const zones = swapZones(host);
  expect(zones, "BOTH panes are named — they are the ones exchanging").toHaveLength(2);
  expect(new Set(zones.map((z) => z.getAttribute("data-pane-swap-indicator")))).toEqual(
    new Set(["p-a", "p-b"]),
  );

  // GEOMETRY: each zone covers its pane WHOLE (not a half), which is what "these two exchange"
  // has to look like next to a band that covers exactly half of one.
  for (const [paneId, paneBox] of [
    ["p-a", taBox],
    ["p-b", tbBox],
  ] as const) {
    const zone = zones.find((z) => z.getAttribute("data-pane-swap-indicator") === paneId);
    const box = (zone as HTMLElement).getBoundingClientRect();
    expect(box.left, `${paneId} left`).toBeCloseTo(paneBox.left, 0);
    expect(box.right, `${paneId} right`).toBeCloseTo(paneBox.right, 0);
    expect(box.top, `${paneId} top`).toBeCloseTo(paneBox.top, 0);
    expect(box.bottom, `${paneId} bottom`).toBeCloseTo(paneBox.bottom, 0);
  }

  // …and the drop still does exactly what it always did: the mutation is untouched.
  await drag.release();
  expect(paneLeaves(controller.layout as PaneNode).map((l) => l.terminalId)).toEqual(["tb", "ta"]);
});

it("criterion 9 — captures: the indicator DURING the gesture, the new disposition AFTER the drop", async () => {
  const host = await mountTiling(400, 700);
  const taBox = paneEl(host, "ta").getBoundingClientRect();
  const drag = await grabAndMoveTo(gripEl(host, "p-b"), edgePoint(taBox, "left"));
  await frames(3);

  // Two panes ⇒ this drop PERMUTES, so what is lit is the swap vocabulary, not an edge band.
  expect(swapZones(host), "holding the gesture with both panes lit").toHaveLength(2);
  await expect(page.getByTestId("pane-move-host")).toMatchScreenshot(
    "during-the-gesture-swap-of-both-panes",
  );
  await drag.release();
  await frames(3);
  await expect(page.getByTestId("pane-move-host")).toMatchScreenshot(
    "after-the-drop-tb-left-of-ta-50-50",
  );

  expect(paneLeaves(controller.layout as PaneNode).map((l) => l.terminalId)).toEqual(["tb", "ta"]);
});

it("criterion 5 — the moved terminal stays FUNCTIONAL: input, output, one PTY, one node, re-fitted", async () => {
  const host = await mountTiling(400, 700);
  const instance = getTerminal("tb");
  const drag = await grabAndMoveTo(
    gripEl(host, "p-b"),
    edgePoint(paneEl(host, "ta").getBoundingClientRect(), "left"),
  );
  await drag.release();
  expect(paneLeaves(controller.layout as PaneNode).map((l) => l.terminalId)).toEqual(["tb", "ta"]);

  // INPUT: keystrokes still reach ITS OWN PTY — through the SAME `__nyxDeckInput` seam the
  // e2e specs and the tiling gate use.
  const deckInput = (window as unknown as Record<string, Record<string, (d: string) => void>>)
    .__nyxDeckInput;
  deckInput.tb("echo moved\r");
  expect(
    await waitFor(() => (writes.get("tb") ?? "").includes("echo moved\r")),
    "input reaches the same PTY",
  ).toBe(true);
  expect(getTerminal("tb"), "the very same store instance through the gesture").toBe(instance);

  // OUTPUT: what the shell prints lands in the buffer AND is painted in the NEW pane.
  backend.emit("pty://output", {
    id: ptyIds.get("tb") as number,
    bytes: Array.from(encoder.encode("still-alive\r\n")),
  });
  const deck = (window as unknown as { __nyxDeck: Record<string, () => string> }).__nyxDeck;
  expect(await waitFor(() => deck.tb().includes("still-alive"))).toBe(true);

  // ONE of everything: one PTY, one DOM attachment, no respawn.
  expect(spawnCounts.get("tb"), "exactly one PTY through the gesture").toBe(1);
  expect(host.querySelectorAll('[data-terminal-node="tb"]').length, "attached once").toBe(1);

  // SIZE: the PTY was told about the NEW box through the existing pipeline — never 0×0, never a
  // frozen 80×24, and consistent with the grid xterm is actually painting.
  const pane = paneEl(host, "tb");
  expect(
    await waitFor(() => {
      const sent = resizes.get("tb");
      const live = getTerminal("tb")?.terminal;
      return (
        sent != null &&
        live != null &&
        sent.cols === live.cols &&
        sent.rows === live.rows &&
        live.cols > 0 &&
        live.rows > 0
      );
    }),
    "the PTY size agrees with the live grid",
  ).toBe(true);
  const live = getTerminal("tb")?.terminal as { cols: number; rows: number };
  expect(`${live.cols}×${live.rows}`).not.toBe("80×24");
  const box = pane.getBoundingClientRect();
  expect(box.width, "the moved pane has a real box").toBeGreaterThan(100);
  const screen = pane.querySelector(".xterm-screen") as HTMLElement;
  expect(screen.getBoundingClientRect().width, "and paints into it").toBeLessThanOrEqual(box.width);
});

it("criterion 6 — the sidebar STILL permutes: reorderGroupMembers swaps occupants, structure frozen", async () => {
  const host = await mountTiling(400, 700);
  const drag = await grabAndMoveTo(
    gripEl(host, "p-b"),
    edgePoint(paneEl(host, "ta").getBoundingClientRect(), "left"),
  );
  await drag.release();
  const afterMove = controller.groups[0].tree;
  const shapeOf = (node: PaneNode) =>
    JSON.stringify(node, (_key, value) =>
      (typeof value === "string" && value === "tb") || value === "ta" ? "<occupant>" : value,
    );

  act(() => controller.reorderGroupMembers("g1", ["ta", "tb"]));

  expect(shapeOf(controller.groups[0].tree), "structure, ratios and ids frozen").toBe(
    shapeOf(afterMove),
  );
  expect(paneLeaves(controller.groups[0].tree).map((l) => l.terminalId)).toEqual(["ta", "tb"]);
  // The DOM followed: ta now sits in the left slot.
  expect(
    (host.querySelector('[data-visible="true"]') as HTMLElement).getAttribute("data-terminal-id"),
  ).toBe("ta");

  // …AND THE FOCUS FOLLOWED THE TERMINAL, not the rectangle (review finding
  // 01M11WR6JTPGAP4ABK0RWXB6XK). `withMemberOrder` freezes the pane ids, so a focus re-conducted
  // verbatim would have handed the keyboard — and `<Terminal active>`, and the Spec surcouche —
  // to `ta`, which the user never touched. It is `tb` that had the focus before the permutation.
  expect(controller.focusedTerminalId, "the keyboard stayed with tb").toBe("tb");
  expect(
    (host.querySelector('[data-focused="true"]') as HTMLElement).getAttribute("data-terminal-id"),
    "and the ring is drawn on tb's new rectangle",
  ).toBe("tb");
});

/* ─────────── THE COMMITTED TARGET vs THE HOVERED ONE (finding 01M11WFPFZGAC68VGXJSG6QNMK) ───────────
 *
 * The finding's second lead: `endPaneDrag` calls `onMovePane` from INSIDE two nested `setState`
 * updaters, i.e. a side effect in the RENDER phase. React is free to run an updater more than once
 * (StrictMode does it on every commit, and `main.tsx` wraps the whole app in one), so the suspicion
 * is that the drop commits something other than what the indicator promised. These two specs
 * MEASURE it rather than reason about it: a real pointer drag onto a NAMED pane's NAMED edge, in a
 * four-pane tiling where the target is unambiguous, with the handler spied.
 */
/** `ta|tb` over `tc|td` — four quadrants with stable pane ids. */
function fourPaneTree(): PaneNode {
  return {
    kind: "split",
    id: "q-root",
    direction: "column",
    ratio: 0.5,
    children: [
      {
        kind: "split",
        id: "q-top",
        direction: "row",
        ratio: 0.5,
        children: [
          { kind: "leaf", id: "q-a", terminalId: "ta" },
          { kind: "leaf", id: "q-b", terminalId: "tb" },
        ],
      },
      {
        kind: "split",
        id: "q-bottom",
        direction: "row",
        ratio: 0.5,
        children: [
          { kind: "leaf", id: "q-c", terminalId: "tc" },
          { kind: "leaf", id: "q-d", terminalId: "td" },
        ],
      },
    ],
  };
}

/** Mount the RAW host with a spied `onMovePane`, optionally under `<StrictMode>` as the app is. */
async function mountSpiedHost(strict: boolean): Promise<{
  host: HTMLElement;
  moves: Array<{ dragged: string; target: string; edge: PaneEdge }>;
}> {
  const moves: Array<{ dragged: string; target: string; edge: PaneEdge }> = [];
  const host = mountHost(414, 800);
  const tree = fourPaneTree();
  const records: TerminalRecord[] = ["ta", "tb", "tc", "td"].map((id) => ({
    id,
    cwd: `/repo/${id}`,
    label: null,
    scrollback: "",
    status: "alive",
    order_index: 0,
    created_at: 0,
    updated_at: 0,
    closed_at: null,
  }));
  const ui = (
    <PaneTreeHost
      terminals={records}
      layout={tree}
      focusedPaneId="q-a"
      onFocusPane={() => {}}
      onSplitPane={() => {}}
      onClosePane={() => {}}
      onMovePane={(dragged, target, edge) => moves.push({ dragged, target, edge })}
      canMovePane={() => true}
    />
  );
  render(strict ? <StrictMode>{ui}</StrictMode> : ui, { container: host });
  expect(await waitFor(() => host.querySelectorAll("[data-visible='true']").length === 4)).toBe(
    true,
  );
  return { host, moves };
}

it("the COMMITTED target is the HOVERED one — the drop never lands somewhere else", async () => {
  const { host, moves } = await mountSpiedHost(false);
  // Drag the TOP-LEFT pane onto the RIGHT band of the BOTTOM-RIGHT one: four panes, so the target
  // and the edge are both unambiguous — a wrong commit cannot hide behind a forced answer.
  const target = paneEl(host, "td").getBoundingClientRect();
  const point = edgePoint(target, "right");
  const drag = await grabAndMoveTo(gripEl(host, "q-a"), point);

  // WHAT THE USER IS PROMISED, read off the DOM at the instant of the release.
  const shown = indicator(host);
  expect(shown, "an indicator is showing").not.toBeNull();
  const promised = {
    target: shown?.getAttribute("data-drop-target-pane"),
    edge: shown?.getAttribute("data-pane-drop-indicator"),
  };
  expect(promised).toEqual({ target: "q-d", edge: "right" });

  await drag.release();

  // WHAT IS COMMITTED.
  expect(moves, "exactly one commit").toHaveLength(1);
  expect(moves[0]).toEqual({ dragged: "q-a", target: "q-d", edge: "right" });
});

it("under <StrictMode> — as the app itself runs — the drop still commits EXACTLY ONCE", async () => {
  const { host, moves } = await mountSpiedHost(true);
  const drag = await grabAndMoveTo(
    gripEl(host, "q-a"),
    edgePoint(paneEl(host, "td").getBoundingClientRect(), "right"),
  );
  await drag.release();
  // `onMovePane` is called from inside a state updater, which StrictMode double-invokes. A second
  // call would apply the restructure twice — the tree the user gets would not be the one the
  // indicator drew.
  expect(moves, "one gesture, one commit").toHaveLength(1);
  expect(moves[0]).toEqual({ dragged: "q-a", target: "q-d", edge: "right" });
});
