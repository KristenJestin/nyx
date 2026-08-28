import { useCallback, useMemo, useState } from "react";
import { act, fireEvent, render } from "@testing-library/react";
import { afterEach, beforeEach, expect, it } from "vitest";

// The REAL Tailwind utilities. LOAD-BEARING: the pane slot and `<Terminal>`'s container are sized
// by `h-full` / `p-2.5`, so without this stylesheet a terminal's box is its own CONTENT height and
// every geometry / PTY-size assertion below would measure the test's markup, not the app's layout.
import "@/globals.css";

import { nyxBridge } from "@/bridge";
import type { FakeNyxBridge } from "@/bridge/fake";
import { Terminal } from "@/components/terminal/terminal";
import { __resetTerminalStore, getTerminal } from "@/components/terminal/terminal-store";
import type { TerminalRecord } from "@/components/sidebar/use-terminals";
import { MIN_PANE_WIDTH_PX } from "@/components/panes/pane-geometry";
import { savePaneCollection } from "./pane-layout-persistence";

/**
 * Seed a project with ONE group holding `tree` — the v2 persisted shape (a project's layout is a
 * COLLECTION of groups since the groups model; a solo terminal has no group at all).
 */
function savePaneLayout(projectId: string, tree: PaneNode): void {
  savePaneCollection(projectId, { groups: [{ id: "g1", tree, name: null, color: null }] });
}
import { PaneTreeHost } from "./pane-tree-host";
import {
  findPaneByTerminal,
  paneCount,
  paneLeaves,
  type PaneNode,
} from "@/components/panes/pane-tree";
import { useTerminalShortcuts } from "@/components/sidebar/use-terminal-shortcuts";
import { RuntimeSelectionProvider, useRuntimeSelection } from "./runtime-selection";
import { saveLastTerminalId } from "./ui-persistence";
import { usePaneLayout, type PaneLayoutController, type SplitWorkspace } from "./use-pane-layout";
import type { WorkspaceRuntimeTerminal } from "@/components/runtime/use-workspace-runtime";

/**
 * THE TILING GATE — four LIVE terminals, one of them hosting an AGENT SESSION, in a real Chromium.
 *
 * WHAT "GATE" MEANS HERE, AND WHAT IT DOES NOT. This file is NOT the CDP gate the repo's Golden
 * Rule asks for (README.md:55-59, docs/product/technical-architecture.md:100-106: a capability is
 * "delivered" only once it has been OBSERVED WORKING IN THE REAL APP — Electron + sidecar + a real
 * PTY/shell, driven over CDP, as cockpit v2.2 / review v2.5 / terminal-blocks each were). It runs
 * in vitest BROWSER MODE against the {@link FakeNyxBridge} double: real Chromium and real layout,
 * but NO Electron, NO sidecar and NO real PTY — `pty_spawn` / `pty_write` / `pty_resize` are the
 * in-memory counters set up below, and the "agent session" is a fixture object. So it proves the
 * tiling's geometry, lifecycle and wiring; it cannot prove the capability against a real shell.
 * WHERE THE REST GOES: observing the tiling in the real app is still to be done, and it is OUTSIDE
 * this PRD — its testing decisions plan browser tests and name no e2e spec. It is handed back as a
 * separate item rather than smuggled in here.
 *
 * This is the acceptance surface of the pane tiling, and it is deliberately run in BROWSER MODE:
 * jsdom has no layout engine, so "four terminals really are alive, sized, typed into and resized
 * side by side" is unprovable there — every box is 0×0 and every PTY size a fiction.
 *
 * What it holds the product to:
 *  1. four tiled terminals keep INPUT, OUTPUT, FOCUS and RESIZE, with no crash and no lost
 *     instance (the same xterm object, the same PTY, one DOM attachment, throughout);
 *  2. an AGENT session is driven from its pane EXACTLY as the same terminal is outside any tiling —
 *     there is no agent leaf kind, no agent branch, nothing for the tiling to get wrong;
 *  3. a terminal closed EXTERNALLY (another window, an agent over MCP) loses its pane and the tree
 *     COMPACTS, leaving the other three untouched;
 *  4. a DOUBLE `pty_spawn` for one `terminal_id` is caught — the cardinal sin of a host that
 *     remounts its panes. Every test asserts it, and one test proves the counter is not blind.
 *
 * THE BACKEND DOUBLE. `@/bridge/test-harness`'s `vi.mock("@/bridge")` does NOT apply in browser
 * mode — vitest only hoists `vi.mock` written IN the test file — so `mockIPC` would install a
 * handler on a fake nothing imports. It is not needed either: without the Electron preload's
 * `window.nyxCore`, `@/bridge` already resolves to a live {@link FakeNyxBridge}, so the double IS
 * `nyxBridge` and we drive it directly.
 */
const backend = nyxBridge as unknown as FakeNyxBridge;

/** A PTY grid — what `pty_spawn` / `pty_resize` carry. */
interface PtySize {
  cols: number;
  rows: number;
}

/** How many `pty_spawn` calls each terminal id got. Anything but 1 is a defect. */
const spawnCounts = new Map<string, number>();
/**
 * The size each terminal's PTY was SPAWNED at — the FIRST `pty_spawn`, keyed by terminal id. The
 * first one is the one that matters: a later `pty_resize` cannot un-print a prompt (or an agent's
 * first turn) already wrapped at the wrong width.
 */
const spawnSizes = new Map<string, PtySize>();
/** Which terminal owns each live PTY id (a `pty_write`/`pty_resize` only carries the PTY id). */
const ptyOwners = new Map<number, string>();
/** The live PTY id of each terminal. */
const ptyIds = new Map<string, number>();
/** Everything each terminal's keystrokes put on the wire, decoded, in order. */
const writes = new Map<string, string>();
/** The LAST `pty_resize` each terminal received. */
const resizes = new Map<string, PtySize>();
/** Every `pty_close` — a pane closing (or a layout compacting) must NOT appear here. */
const closes: number[] = [];

const decoder = new TextDecoder();
const encoder = new TextEncoder();

beforeEach(() => {
  spawnCounts.clear();
  spawnSizes.clear();
  ptyOwners.clear();
  ptyIds.clear();
  writes.clear();
  resizes.clear();
  closes.length = 0;
  window.localStorage.clear();
  backend.reset();
  let nextPtyId = 200;
  backend.onAnyCommand((cmd, args) => {
    if (cmd === "pty_spawn") {
      const terminalId = typeof args?.terminalId === "string" ? args.terminalId : "";
      spawnCounts.set(terminalId, (spawnCounts.get(terminalId) ?? 0) + 1);
      if (terminalId !== "" && !spawnSizes.has(terminalId)) {
        spawnSizes.set(terminalId, { cols: Number(args?.cols), rows: Number(args?.rows) });
      }
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
    if (cmd === "pty_close") {
      closes.push(Number(args?.id));
      return null;
    }
    if (cmd === "list_terminals") return [];
    if (cmd === "terminal_info") return { cwd: null, foreground: null };
    return null;
  });
});

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

/**
 * The runner's viewport, which every host below has to FIT INSIDE.
 *
 * LOAD-BEARING, and the subtle part of this file: xterm pauses a terminal's rendering while its
 * screen is outside the viewport (an IntersectionObserver inside the renderer). A host laid out
 * past the right or bottom edge would keep its buffers perfectly correct while painting NOTHING,
 * and every "the pane shows it" assertion would fail for a reason that has nothing to do with the
 * tiling. The vitest browser test frame is NARROW (a phone-sized viewport), so the tiling host is
 * sized from it rather than from a comfortable-looking constant.
 */
const VIEWPORT = { width: 414, height: 896 };
/** The tiling host: four panes of ~200×175 — small, but a real, paintable, typeable grid each. */
const TILING = { width: 400, height: 700 };

/** A fixed-size, on-screen host — the app's main area. */
function mountHost(width: number, height: number, top = 0): HTMLElement {
  expect(width, "a host wider than the viewport would stop painting").toBeLessThanOrEqual(
    Math.min(VIEWPORT.width, window.innerWidth),
  );
  expect(top + height, "a host taller than the viewport would stop painting").toBeLessThanOrEqual(
    Math.min(VIEWPORT.height, window.innerHeight),
  );
  const host = document.createElement("div");
  host.style.cssText = `position:fixed;top:${top}px;left:0;width:${width}px;height:${height}px`;
  document.body.appendChild(host);
  return host;
}

const WORKSPACES: SplitWorkspace[] = [{ id: "ws-root", path: "/repo", is_root: true }];

/**
 * A runtime terminal as the snapshot serves it. `agent` gives it an AGENT SESSION — which changes
 * NOTHING about how it is tiled: the tree has no agent leaf kind, an agent is a property of the
 * SESSION hosted by the terminal, and this test is here to prove the tiling never learns otherwise.
 */
function runtimeTerminal(id: string, agent = false): WorkspaceRuntimeTerminal {
  return {
    id,
    cwd: "/repo",
    label: null,
    workspace_id: "ws-root",
    status: "alive",
    exec_state: "idle",
    exec_state_unread: false,
    detected_agent_kind: agent ? "claude_code" : null,
    sessions: agent
      ? [
          {
            id: `sess-${id}`,
            terminal_id: id,
            workspace_id: "ws-root",
            agent_kind: "claude_code",
            external_session_id: "ext-1",
            state: "active",
          },
        ]
      : [],
  };
}

/** The handles the test drives the mounted page through. */
interface Gate {
  readonly controller: PaneLayoutController;
  /** Drop a terminal from the live snapshot — an EXTERNAL close (other window, agent over MCP). */
  readonly dropTerminal: (id: string) => void;
  /** The SIDEBAR gesture: select a terminal by id (what a row click does). */
  readonly select: (terminalId: string) => void;
}

let gate: Gate;
/** The ids the harness' `createTerminal` will hand out, in order. */
let creations: string[] = [];

/**
 * The Terminals page's tiling, minus the sidebar: the REAL `use-pane-layout` (restore, split,
 * close, reconcile, resize, persistence) driving the REAL `<PaneTreeHost>`. Splitting here creates
 * a terminal through the same injected `createTerminal` seam the page wires to the sidebar's "+",
 * so the four panes below are built the way a user builds them.
 */
function TilingPage({ initial }: { initial: readonly WorkspaceRuntimeTerminal[] }) {
  const [terminals, setTerminals] = useState<readonly WorkspaceRuntimeTerminal[]>(initial);

  const createTerminal = useCallback(async (_workspaceId: string, _path: string) => {
    const id = creations.shift() as string;
    setTerminals((current) => [...current, runtimeTerminal(id)]);
    return id;
  }, []);

  const controller = usePaneLayout({
    projectId: "gate",
    terminals,
    workspaces: WORKSPACES,
    createTerminal,
  });

  // The REAL chord registrations — the same hook the page wires, so Alt+arrows and Mod+Shift+W
  // are exercised through the very path the product uses (and `isTerminalNavChord` is what makes
  // xterm yield them). The CRUD chords are inert here: this harness owns no terminal lifecycle.
  useTerminalShortcuts({
    onNew: () => {},
    onClose: () => {},
    onNext: () => {},
    onPrev: () => {},
    onSplitRight: () =>
      controller.focusedPaneId && controller.splitPane(controller.focusedPaneId, "row"),
    onSplitDown: () =>
      controller.focusedPaneId && controller.splitPane(controller.focusedPaneId, "column"),
    onFocusLeft: () => controller.focusNeighborPane("left"),
    onFocusRight: () => controller.focusNeighborPane("right"),
    onFocusUp: () => controller.focusNeighborPane("up"),
    onFocusDown: () => controller.focusNeighborPane("down"),
    onClosePane: () => controller.closeFocusedPane(),
  });

  const { select } = useRuntimeSelection();
  gate = {
    controller,
    dropTerminal: (id) => setTerminals((current) => current.filter((t) => t.id !== id)),
    select: (terminalId) => select({ kind: "terminal", id: terminalId }),
  };

  // The same snake_case → `TerminalRecord` projection `<RuntimePaneHost>` does.
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
    />
  );
}

/**
 * Mount the page on ONE terminal (`agent`, hosting an agent session) and SPLIT it three times into
 * the nested four-pane tiling `[agent / t4] | [t2 / t3]` — a row split with a column split on each
 * side, so both axes and both nesting levels are under test.
 */
async function mountFourPanes(host: HTMLElement): Promise<void> {
  creations = ["t2", "t3", "t4"];
  render(
    <RuntimeSelectionProvider>
      <TilingPage initial={[runtimeTerminal("agent", true)]} />
    </RuntimeSelectionProvider>,
    { container: host },
  );
  expect(await waitFor(() => gate.controller.focusedPaneId != null), "the first pane").toBe(true);

  const paneOf = (terminalId: string) =>
    findPaneByTerminal(gate.controller.layout as PaneNode, terminalId)?.id as string;

  await act(async () => {
    gate.controller.splitPane(paneOf("agent"), "row");
  });
  expect(await waitFor(() => paneCount(gate.controller.layout as PaneNode) === 2)).toBe(true);
  await act(async () => {
    gate.controller.splitPane(paneOf("t2"), "column");
  });
  expect(await waitFor(() => paneCount(gate.controller.layout as PaneNode) === 3)).toBe(true);
  await act(async () => {
    gate.controller.splitPane(paneOf("agent"), "column");
  });
  expect(await waitFor(() => paneCount(gate.controller.layout as PaneNode) === 4)).toBe(true);

  // Every pane's shell really started (four distinct PTYs, none of them a duplicate).
  expect(
    await waitFor(() => ALL.every((id) => spawnCounts.get(id) === 1)),
    "four PTYs spawned, one each",
  ).toBe(true);

  // …and NOT ONE of the three panes BORN FROM A SPLIT started its shell at the 80×24 fallback: a
  // pane the tree gained mid-session is measured before its PTY opens, exactly like one present at
  // mount. Their spawn boxes are the ones the tree had AT THEIR OWN SPLIT (t2 was a full-height
  // half until t3 halved it), so the box-for-box match is asserted in the single-split test below,
  // where the geometry stays put.
  for (const id of ["t2", "t3", "t4"]) {
    const size = spawnSizes.get(id) as PtySize;
    expect(`${size.cols}×${size.rows}`, `${id} spawned at the fallback`).not.toBe("80×24");
    // Not a coincidence either: a half of this 400px host cannot hold 80 columns.
    expect(size.cols, `${id} spawned at its pane's width`).toBeLessThan(80);
  }
}

const ALL = ["agent", "t2", "t3", "t4"] as const;

/** The pane slot showing `terminalId`. */
function paneEl(host: HTMLElement, terminalId: string): HTMLElement {
  return host.querySelector(`[data-terminal-id="${terminalId}"]`) as HTMLElement;
}

/** Feed `data` as if typed, through the SAME `__nyxDeckInput` seam the e2e specs use. */
function typeInto(terminalId: string, data: string): void {
  (window as unknown as { __nyxDeckInput: Record<string, (d: string) => void> }).__nyxDeckInput[
    terminalId
  ](data);
}

/** Read a terminal's buffer through the SAME `__nyxDeck` seam the e2e specs use. */
function screenText(terminalId: string): string {
  return (window as unknown as { __nyxDeck: Record<string, () => string> }).__nyxDeck[terminalId]();
}

/** Push bytes as if the shell printed them. */
function emitOutput(terminalId: string, text: string): void {
  backend.emit("pty://output", {
    id: ptyIds.get(terminalId) as number,
    bytes: Array.from(encoder.encode(text)),
  });
}

/** A PointerEvent as a real device delivers it. */
function pointerEvent(type: string, x: number, y: number, buttons: number): PointerEvent {
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

/** Drag `separator` from its centre to `(x, y)`. */
function dragSeparator(separator: HTMLElement, x: number, y: number): void {
  const box = separator.getBoundingClientRect();
  const from = { x: box.left + box.width / 2, y: box.top + box.height / 2 };
  fireEvent(separator, pointerEvent("pointerdown", from.x, from.y, 1));
  fireEvent(separator, pointerEvent("pointermove", x, y, 1));
  fireEvent(separator, pointerEvent("pointerup", x, y, 0));
}

/** The live grid of every terminal, keyed by record id. */
function grids(): Map<string, PtySize> {
  const out = new Map<string, PtySize>();
  for (const id of ALL) {
    const term = getTerminal(id)?.terminal;
    if (term) out.set(id, { cols: term.cols, rows: term.rows });
  }
  return out;
}

/** Every visible terminal has told its PTY the grid xterm is actually painting. */
function ptySizesAgree(ids: readonly string[]): boolean {
  return ids.every((id) => {
    const term = getTerminal(id)?.terminal;
    const sent = resizes.get(id);
    return term != null && sent != null && sent.cols === term.cols && sent.rows === term.rows;
  });
}

it("a pane BORN FROM A SPLIT spawns its PTY at its own box, never the 80×24 fallback", async () => {
  // The half of the "never 80×24" claim a STATIC layout cannot make: there, every pane exists
  // before its `<Terminal>` mounts. Here the pane does not exist at all until `splitPane` creates
  // its terminal — which is the ordering that could hand `pty_spawn` an unmeasured 0×0 box and
  // start a shell (or a resumed agent turn) wrapped at the wrong width, frozen in the scrollback.
  const host = mountHost(TILING.width, TILING.height);
  creations = ["born"];
  render(
    <RuntimeSelectionProvider>
      <TilingPage initial={[runtimeTerminal("seed")]} />
    </RuntimeSelectionProvider>,
    { container: host },
  );
  expect(await waitFor(() => gate.controller.focusedPaneId != null), "the first pane").toBe(true);
  expect(await waitFor(() => spawnSizes.has("seed")), "the seed terminal spawned").toBe(true);

  // A REAL split, through the page's own path: create the terminal, THEN add the leaf.
  await act(async () => {
    gate.controller.splitPane(gate.controller.focusedPaneId as string, "row");
  });
  expect(await waitFor(() => paneCount(gate.controller.layout as PaneNode) === 2)).toBe(true);
  expect(await waitFor(() => spawnSizes.has("born")), "the split's terminal spawned").toBe(true);

  const pane = paneEl(host, "born");
  const box = pane.getBoundingClientRect();
  const size = spawnSizes.get("born") as PtySize;

  // NOT the fallback — and not by luck: half of this 400px host cannot hold 80 columns.
  expect(`${size.cols}×${size.rows}`, "the split pane spawned at the fallback").not.toBe("80×24");
  expect(size.cols).toBeLessThan(80);
  expect(box.width, "the new pane really is the half").toBeCloseTo(TILING.width / 2, 0);

  // It IS this pane's grid: xterm laid out exactly `rows` rows, and the painted screen fits inside
  // the pane box while covering it up to the container padding (`p-2.5` = 10px a side), xterm's
  // scrollbar and a partial trailing cell.
  expect(
    pane.querySelectorAll(".xterm-rows > div").length,
    "the new pane renders its spawned row count",
  ).toBe(size.rows);
  const screen = (pane.querySelector(".xterm-screen") as HTMLElement).getBoundingClientRect();
  const cellWidth = screen.width / size.cols;
  const cellHeight = screen.height / size.rows;
  expect(cellWidth, "the new pane has a measured cell").toBeGreaterThan(0);
  expect(screen.width, "the grid fits the new pane").toBeLessThanOrEqual(box.width);
  expect(screen.height, "the grid fits the new pane").toBeLessThanOrEqual(box.height);
  expect(box.width - screen.width, "the grid spans the new pane").toBeLessThanOrEqual(
    20 + 20 + cellWidth,
  );
  expect(box.height - screen.height, "the grid spans the new pane").toBeLessThanOrEqual(
    20 + cellHeight,
  );

  // The spawn size needed NO correction: the live terminal still shows that very grid, on one PTY.
  expect(getTerminal("born")?.terminal.cols).toBe(size.cols);
  expect(getTerminal("born")?.terminal.rows).toBe(size.rows);
  expect(spawnCounts.get("born"), "the new pane owns exactly one PTY").toBe(1);
});

it("four LIVE terminals keep input, output, focus and successive resizes — no crash, no lost instance", async () => {
  const host = mountHost(TILING.width, TILING.height);
  await mountFourPanes(host);

  // The instances as they are RIGHT AFTER the tiling is built. "No lost instance" is asserted
  // against these very objects: a remount would produce different ones.
  const instances = new Map(ALL.map((id) => [id, getTerminal(id)]));
  const nodes = new Map(ALL.map((id) => [id, getTerminal(id)?.el]));
  for (const id of ALL) expect(instances.get(id), `${id} is live`).toBeDefined();

  // ── INPUT: each pane's keystrokes reach ITS OWN PTY, and nobody else's ────────────────────
  for (const id of ALL) typeInto(id, `echo ${id}\r`);
  expect(
    await waitFor(() => ALL.every((id) => (writes.get(id) ?? "").includes(`echo ${id}\r`))),
    "every pane's input reached its own PTY",
  ).toBe(true);
  for (const id of ALL) {
    const others = ALL.filter((other) => other !== id);
    for (const other of others) {
      expect(writes.get(id), `${id} received only its own keystrokes`).not.toContain(
        `echo ${other}`,
      );
    }
  }

  // ── OUTPUT: what each shell prints lands in ITS pane, all four at once ────────────────────
  for (const id of ALL) emitOutput(id, `hello from ${id}\r\n`);
  expect(
    await waitFor(() => ALL.every((id) => screenText(id).includes(`hello from ${id}`))),
    "every pane received its own output",
  ).toBe(true);
  // …and it is really PAINTED in that pane's own grid, not just sitting in a buffer. (xterm's DOM
  // renderer emits NO-BREAK SPACEs, so the painted text is normalised before comparing.)
  const paintedRows = (id: string) =>
    ((paneEl(host, id).querySelector(".xterm-rows") as HTMLElement).textContent ?? "").replace(
      /\u00a0/g,
      " ",
    );
  expect(
    await waitFor(() => ALL.every((id) => paintedRows(id).includes(`hello from ${id}`))),
    "every pane painted its own output",
  ).toBe(true);

  // ── FOCUS: exactly ONE pane owns the keyboard, and clicking another moves it ──────────────
  for (const id of ALL) {
    fireEvent.mouseDown(paneEl(host, id));
    expect(
      await waitFor(() => paneEl(host, id).getAttribute("data-focused") === "true"),
      `${id} took the focus`,
    ).toBe(true);
    expect(host.querySelectorAll('[data-focused="true"]').length, "one focused pane").toBe(1);
    expect(
      await waitFor(() => paneEl(host, id).contains(document.activeElement)),
      `${id} took the keyboard`,
    ).toBe(true);
  }

  // ── RESIZE, SUCCESSIVELY: the outer seam, then a nested one ───────────────────────────────
  const separators = () => Array.from(host.querySelectorAll<HTMLElement>("[data-separator-id]"));
  expect(separators().length, "one separator per split").toBe(3);

  // FIRST resize: the OUTER seam, dragged right — the left column (agent / t4) widens, the right
  // one (t2 / t3) narrows, and all four PTYs are told about it.
  const before = grids();
  const outer = separators().find((s) => s.dataset.direction === "row") as HTMLElement;
  dragSeparator(outer, TILING.width * 0.7, TILING.height / 2);
  expect(
    await waitFor(
      () => ptySizesAgree(ALL) && grids().get("agent")?.cols !== before.get("agent")?.cols,
    ),
    "the outer drag re-sized every PTY",
  ).toBe(true);
  expect((grids().get("agent") as PtySize).cols, "the left column widened").toBeGreaterThan(
    (before.get("agent") as PtySize).cols,
  );
  expect((grids().get("t2") as PtySize).cols, "the right column narrowed").toBeLessThan(
    (before.get("t2") as PtySize).cols,
  );

  // SECOND resize, on top of the first: a NESTED seam, on the other axis. It moves only the two
  // panes of its own sub-box — which is the whole point of the tree being nested.
  const afterOuter = grids();
  const nested = separators().find((s) => s.dataset.direction === "column") as HTMLElement;
  dragSeparator(nested, nested.getBoundingClientRect().left + 4, TILING.height * 0.25);
  expect(
    await waitFor(
      () =>
        ptySizesAgree(ALL) && ALL.some((id) => grids().get(id)?.rows !== afterOuter.get(id)?.rows),
    ),
    "the nested drag re-sized the PTYs on its own axis",
  ).toBe(true);

  // ── AND THROUGH ALL OF IT: the same instance, the same node, ONE PTY, still interactive ───
  for (const id of ALL) {
    expect(getTerminal(id), `${id} was never re-created`).toBe(instances.get(id));
    expect(getTerminal(id)?.el, `${id} kept its persistent node`).toBe(nodes.get(id));
    expect(
      host.querySelectorAll(`[data-terminal-node="${id}"]`).length,
      `${id} is attached once`,
    ).toBe(1);
    // THE CARDINAL SIN: a second `pty_spawn` for one terminal_id (a host that remounts its panes).
    expect(spawnCounts.get(id), `${id} owns exactly one PTY`).toBe(1);
  }
  expect(closes, "no PTY was closed by a layout edit").toEqual([]);

  // Still typeable after the resizes — the panes are live, not a frozen painting.
  for (const id of ALL) typeInto(id, "still-alive\r");
  expect(
    await waitFor(() => ALL.every((id) => (writes.get(id) ?? "").includes("still-alive"))),
    "every pane is still interactive after the resizes",
  ).toBe(true);
});

/**
 * What "interactive" means for a terminal, measured the SAME way wherever it is mounted. Comparing
 * two of these is the parity claim: an agent's terminal behaves identically inside the tiling and
 * outside it.
 */
interface Interaction {
  /** The bytes its keystrokes put on the wire. */
  wrote: string;
  /** Whether what the agent printed reached its buffer. */
  echoed: boolean;
  /** Whether it takes the keyboard when asked. */
  focused: boolean;
  /** How many PTYs it owns. */
  ptys: number;
}

/** Drive `recordId` exactly as a user drives an agent, and report what happened. */
async function driveAgent(recordId: string, container: HTMLElement): Promise<Interaction> {
  typeInto(recordId, "/model opus\r");
  await waitFor(() => (writes.get(recordId) ?? "").includes("/model opus\r"));
  emitOutput(recordId, "\x1b[32m● Claude Code\x1b[0m ready\r\n");
  const echoed = await waitFor(() => screenText(recordId).includes("Claude Code"));
  getTerminal(recordId)?.focus();
  const focused = await waitFor(() => container.contains(document.activeElement));
  return {
    wrote: writes.get(recordId) ?? "",
    echoed,
    focused,
    ptys: spawnCounts.get(recordId) ?? 0,
  };
}

it("an AGENT session is driven from its pane exactly as the same terminal outside any tiling", async () => {
  // THE TILING: four panes, the first one hosting the agent session.
  const host = mountHost(TILING.width, 600);
  await mountFourPanes(host);

  // THE CONTROL — "hors tiling": the SAME `<Terminal>`, in its own box, with no pane host, no
  // tree, no separator around it. Mounted BELOW the tiling so both stay inside the viewport (an
  // off-screen terminal stops painting, see `VIEWPORT`).
  const solo = mountHost(TILING.width, 250, 620);
  render(<Terminal recordId="solo" cwd="/repo" active={false} />, { container: solo });
  expect(await waitFor(() => spawnCounts.get("solo") === 1), "the control spawned").toBe(true);

  const control = await driveAgent("solo", solo);
  const tiled = await driveAgent("agent", paneEl(host, "agent"));

  // PARITY, fact by fact: same bytes on the wire, same output in the buffer, same keyboard, one
  // PTY each. The tiling adds nothing and takes nothing away.
  expect(tiled).toEqual(control);

  // …and STRUCTURALLY there is nothing for it to get wrong: the agent's leaf is an ORDINARY pane
  // leaf — `kind`, `id`, `terminalId`, and not one field more. No agent leaf kind exists.
  const leaf = findPaneByTerminal(gate.controller.layout as PaneNode, "agent");
  expect(leaf).not.toBeNull();
  expect(Object.keys(leaf as object).sort()).toEqual(["id", "kind", "terminalId"]);
  expect(leaf?.kind).toBe("leaf");
  // The agent pane is one leaf among four, laid out and sized like any other.
  expect(paneEl(host, "agent").getBoundingClientRect().width).toBeCloseTo(TILING.width / 2, 0);
  expect(paneCount(gate.controller.layout as PaneNode)).toBe(4);
});

it("a terminal closed EXTERNALLY loses its pane and the tree compacts — the other three untouched", async () => {
  const host = mountHost(TILING.width, TILING.height);
  await mountFourPanes(host);

  const survivors = ["agent", "t2", "t4"] as const;
  const instances = new Map(survivors.map((id) => [id, getTerminal(id)]));
  for (const id of survivors) emitOutput(id, `keep ${id}\r\n`);
  expect(await waitFor(() => survivors.every((id) => screenText(id).includes(`keep ${id}`)))).toBe(
    true,
  );

  // t3 disappears from the runtime snapshot — closed in another window, or by an agent over MCP.
  await act(async () => {
    gate.dropTerminal("t3");
  });
  expect(
    await waitFor(() => paneCount(gate.controller.layout as PaneNode) === 3),
    "the tree compacted onto three panes",
  ).toBe(true);

  // COMPACTED, not holed: t3's parent split collapsed onto its sibling, so three leaves need two
  // splits and the three panes still TILE the host exactly.
  const layout = gate.controller.layout as PaneNode;
  expect(
    paneLeaves(layout)
      .map((l) => l.terminalId)
      .sort(),
  ).toEqual([...survivors].sort());
  expect(host.querySelectorAll('[data-visible="true"]').length).toBe(3);
  const boxes = survivors.map((id) => paneEl(host, id).getBoundingClientRect());
  for (let i = 0; i < boxes.length; i++) {
    for (let j = i + 1; j < boxes.length; j++) {
      const a = boxes[i] as DOMRect;
      const b = boxes[j] as DOMRect;
      expect(
        a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom,
        "two panes must not overlap",
      ).toBe(false);
    }
  }
  const hostBox = host.getBoundingClientRect();
  expect(
    boxes.reduce((sum, box) => sum + box.width * box.height, 0),
    "the three panes still cover the host — the freed space went to the sibling",
  ).toBeGreaterThan(hostBox.width * hostBox.height * 0.98);

  // THE OTHER THREE ARE UNTOUCHED: same instance, same buffer, still one PTY each, no `pty_close`,
  // still typeable — and each was re-fitted to its new box.
  for (const id of survivors) {
    expect(getTerminal(id), `${id} kept its instance`).toBe(instances.get(id));
    expect(screenText(id), `${id} kept its buffer`).toContain(`keep ${id}`);
    expect(spawnCounts.get(id), `${id} still owns exactly one PTY`).toBe(1);
    expect(host.querySelectorAll(`[data-terminal-node="${id}"]`).length).toBe(1);
  }
  // A pane leaving the tiling is a LAYOUT edit: not one PTY was closed by it.
  expect(closes).toEqual([]);
  expect(await waitFor(() => ptySizesAgree(survivors)), "the survivors were re-sized").toBe(true);

  for (const id of survivors) typeInto(id, `after-${id}\r`);
  expect(
    await waitFor(() => survivors.every((id) => (writes.get(id) ?? "").includes(`after-${id}`))),
    "the survivors are still interactive",
  ).toBe(true);

  // And the focus never points at the pane that is gone.
  const focused = gate.controller.focusedPaneId as string;
  expect(paneLeaves(layout).some((leaf) => leaf.id === focused)).toBe(true);
});

it("the gate SEES a second PTY for one terminal_id — the counter is not blind", async () => {
  const host = mountHost(TILING.width, 300);
  render(<Terminal recordId="twice" cwd="/repo" active={false} />, { container: host });
  expect(await waitFor(() => spawnCounts.get("twice") === 1)).toBe(true);

  // Force exactly the defect the other tests assert against: the store loses the instance, so the
  // next mount BUILDS A NEW ONE and spawns a SECOND PTY for the same terminal_id.
  __resetTerminalStore();
  const second = mountHost(TILING.width, 300, 320);
  render(<Terminal recordId="twice" cwd="/repo" active={false} />, { container: second });

  // If this counter could not tell one spawn from two, `spawnCounts.get(id) === 1` would be a
  // tautology everywhere above. It can.
  expect(await waitFor(() => spawnCounts.get("twice") === 2), "a double spawn is visible").toBe(
    true,
  );
});

/* ─────────────────────── RESTORE — a whole tiling reborn from `localStorage` ───────────────────────
 *
 * The PRD's testing decisions list "navigation / reload" among the BROWSER tests, and it was the
 * one item covered in jsdom only (review finding). It is the ONE path where an ENTIRE tree —
 * several panes, nested ratios, a focused pane — comes back at once, and three things it must get
 * right are UNPROVABLE without a layout engine:
 *  - each restored pane gets a REAL box, so its PTY spawns at that box and not at the 80×24
 *    fallback — the OUTCOME, which jsdom (no layout engine) cannot show at all. The frame WAIT
 *    `awaitMeasuredBox` performs is NOT what this proves: that is pinned with an injected clock in
 *    `../terminal/terminal-store.test.ts` ("awaitMeasuredBox (a split pane never spawns at the
 *    80×24 fallback)"). Measured (review round 3): making `awaitMeasuredBox` return `false` at once
 *    leaves this spec GREEN — a restore in real Chromium measures a box on the first read;
 *  - the restored separators' `aria-value*` come from the MEASURED host, so in jsdom they always
 *    fall back to the relative band and the nominal regime is never exercised;
 *  - the window can be SMALLER than the one that persisted the layout — the arbitrated case.
 *
 * The persisted fixture below is deliberately NOT a set of halves: 52 / 42 / 63 % on both axes, so
 * a restore that quietly re-balanced (or dropped) a ratio could not pass by coincidence. Its ids
 * are stated too — the restored DOM is addressed BY THEM, which is what proves the tree on screen
 * IS the persisted one rather than a fresh tiling of the same shape.
 */
const RESTORED_TREE: PaneNode = {
  kind: "split",
  id: "s-root",
  direction: "row",
  ratio: 0.52,
  children: [
    {
      kind: "split",
      id: "s-left",
      direction: "column",
      ratio: 0.42,
      children: [
        { kind: "leaf", id: "p-a", terminalId: "ta" },
        { kind: "leaf", id: "p-b", terminalId: "tb" },
      ],
    },
    {
      kind: "split",
      id: "s-right",
      direction: "column",
      ratio: 0.63,
      children: [
        { kind: "leaf", id: "p-c", terminalId: "tc" },
        { kind: "leaf", id: "p-d", terminalId: "td" },
      ],
    },
  ],
};
const RESTORED = ["ta", "tb", "tc", "td"] as const;

/** Write the fixture where the app reads it, then mount the page FRESH on the same four terminals. */
async function mountRestored(host: HTMLElement, focusHint: string): Promise<void> {
  savePaneLayout("gate", RESTORED_TREE);
  saveLastTerminalId("gate", focusHint);
  creations = [];
  render(
    <RuntimeSelectionProvider>
      <TilingPage initial={RESTORED.map((id) => runtimeTerminal(id))} />
    </RuntimeSelectionProvider>,
    { container: host },
  );
  expect(
    await waitFor(() => host.querySelectorAll('[data-visible="true"]').length === 4),
    "the four persisted panes came back",
  ).toBe(true);
  expect(
    await waitFor(() => RESTORED.every((id) => spawnSizes.has(id))),
    "every restored pane spawned its PTY",
  ).toBe(true);
}

/** The rendered box of the pane showing `terminalId`. */
function paneBox(host: HTMLElement, terminalId: string): DOMRect {
  return paneEl(host, terminalId).getBoundingClientRect();
}

/** A restored separator, addressed by the SPLIT ID that was persisted. */
function seamEl(host: HTMLElement, splitId: string): HTMLElement {
  return host.querySelector(`[data-separator-id="${splitId}"]`) as HTMLElement;
}

it("RESTORE: a persisted 4-pane tiling comes back measured, focused, and spawned at its own boxes", async () => {
  const host = mountHost(TILING.width, TILING.height);
  await mountRestored(host, "tc");

  // (a) EVERY PANE FOUND ITS PIXELS — the persisted ratios, resolved against the real host.
  // 400 × 700 ⇒ 52 % of the width and 42 / 63 % of the height, to the pixel.
  const expected: Record<string, { w: number; h: number }> = {
    ta: { w: TILING.width * 0.52, h: TILING.height * 0.42 },
    tb: { w: TILING.width * 0.52, h: TILING.height * 0.58 },
    tc: { w: TILING.width * 0.48, h: TILING.height * 0.63 },
    td: { w: TILING.width * 0.48, h: TILING.height * 0.37 },
  };
  for (const id of RESTORED) {
    const box = paneBox(host, id);
    const want = expected[id] as { w: number; h: number };
    expect(box.width, `${id} restored width`).toBeCloseTo(want.w, 0);
    expect(box.height, `${id} restored height`).toBeCloseTo(want.h, 0);
  }
  // …and the panes really TILE the host: no gap, no overlap, whatever the nesting.
  expect(paneBox(host, "ta").right, "the seam is where the ratio puts it").toBeCloseTo(
    paneBox(host, "tc").left,
    0,
  );
  expect(paneBox(host, "ta").bottom).toBeCloseTo(paneBox(host, "tb").top, 0);

  // (b) THE FOCUS came back too — on the pane showing the remembered terminal, not the first one.
  expect(
    paneEl(host, "tc").getAttribute("data-focused"),
    "the remembered terminal's pane takes the focus",
  ).toBe("true");
  expect(host.querySelectorAll('[data-focused="true"]').length, "exactly one focused pane").toBe(1);
  expect(paneEl(host, "ta").getAttribute("data-focused")).toBe("false");

  // (c) EVERY PTY STARTED AT ITS OWN PANE'S SIZE — the point of the whole restore path. A shell
  // (or a resumed agent turn) wrapped at 80×24 and then resized keeps the wrong wrap in its
  // scrollback forever, so the FIRST spawn is what is measured.
  for (const id of RESTORED) {
    const size = spawnSizes.get(id) as PtySize;
    expect(`${size.cols}×${size.rows}`, `${id} spawned at the fallback`).not.toBe("80×24");
    // Not luck: no pane of this 400px host can hold 80 columns.
    expect(size.cols, `${id} spawned at its pane's width`).toBeLessThan(80);
    const live = getTerminal(id)?.terminal;
    expect(live, `${id} is a live xterm`).not.toBeNull();
    expect({ cols: live?.cols, rows: live?.rows }, `${id} spawned at the grid it paints`).toEqual({
      cols: size.cols,
      rows: size.rows,
    });
  }
  // …and they are PER-PANE, not one size handed to everyone: the taller half of each column has
  // strictly more rows than the shorter one, exactly as the persisted 42 / 63 % ask.
  const rows = (id: string) => (spawnSizes.get(id) as PtySize).rows;
  expect(rows("tb"), "tb owns 58 % of the height, ta 42 %").toBeGreaterThan(rows("ta"));
  expect(rows("tc"), "tc owns 63 % of the height, td 37 %").toBeGreaterThan(rows("td"));

  // …and the restored separators announce the course the MEASURED host really offers: 180px of
  // 400 = 45 %, so 45..55 — the px clamp, not the constant 15 / 85 — around the persisted 52 %.
  const root = seamEl(host, "s-root");
  expect(root.getAttribute("aria-valuenow"), "the persisted ratio survived the restore").toBe("52");
  expect([root.getAttribute("aria-valuemin"), root.getAttribute("aria-valuemax")]).toEqual([
    "45",
    "55",
  ]);
});

it("RESTORE into a SMALLER window: the starved seam hands 15..85 back, the others stay px-clamped", async () => {
  // The arbitrated case: the layout was persisted in a comfortable window and comes back in one
  // that can no longer hold it. 340px cannot seat two 180px panes side by side, so the ROOT seam
  // is genuinely starved; the height (400px) still seats two 120px panes, so the two COLUMN seams
  // are in the nominal regime. Both regimes, in one restored tree.
  const host = mountHost(340, 400);
  await mountRestored(host, "ta");

  const announced = (splitId: string) => {
    const seam = seamEl(host, splitId);
    return [seam.getAttribute("aria-valuemin"), seam.getAttribute("aria-valuemax")];
  };
  expect(
    await waitFor(() => announced("s-root")[0] === "15"),
    `the starved root seam announces ${announced("s-root").join(" / ")}`,
  ).toBe(true);
  // STARVED — 180 + 180 > 340: no ratio seats both sides, so the relative course comes back rather
  // than two equal bounds that would read as "this widget cannot move".
  expect(announced("s-root"), "the starved seam hands the relative course back").toEqual([
    "15",
    "85",
  ]);
  expect(paneBox(host, "ta").width, "…because both sides really are under the floor").toBeLessThan(
    MIN_PANE_WIDTH_PX,
  );

  // NOMINAL — the same restore, on the axis that still fits: 120px of 400 = 30 %, so 30..70. The
  // px clamp, measured, on a tree that was never re-balanced (the persisted 42 % is still there).
  expect(announced("s-left"), "the left column seam is still px-clamped").toEqual(["30", "70"]);
  expect(announced("s-right"), "the right column seam is still px-clamped").toEqual(["30", "70"]);
  expect(seamEl(host, "s-left").getAttribute("aria-valuenow")).toBe("42");

  // And the restore still spawned every PTY at its own (smaller) box — never the fallback.
  for (const id of RESTORED) {
    const size = spawnSizes.get(id) as PtySize;
    expect(`${size.cols}×${size.rows}`, `${id} spawned at the fallback`).not.toBe("80×24");
    expect(size.cols, `${id} spawned at its pane's width`).toBeLessThan(80);
  }
});

/* ─────────── RESTORE — A COLLECTION: two groups plus a solo terminal, in real Chromium ───────────
 *
 * Review 01M0ABR6V501D0SPR2BYBVZC4A (critical). The model is no longer one tree per project but a
 * COLLECTION of groups plus solo terminals, and the point of the change is that a project holding
 * BOTH must come back exactly as it was left — including the part that has no tree at all.
 *
 * Chromium, not jsdom: what "identical" means here is the panes' real boxes (the persisted ratios
 * resolved against the measured host) and each restored pane's PTY spawning at its own box.
 */
const GROUP_A: PaneNode = {
  kind: "split",
  id: "ga-root",
  direction: "column",
  ratio: 0.37,
  children: [
    { kind: "leaf", id: "pa-1", terminalId: "ga1" },
    { kind: "leaf", id: "pa-2", terminalId: "ga2" },
  ],
};
const GROUP_B: PaneNode = {
  kind: "split",
  id: "gb-root",
  direction: "column",
  ratio: 0.71,
  children: [
    { kind: "leaf", id: "pb-1", terminalId: "gb1" },
    { kind: "leaf", id: "pb-2", terminalId: "gb2" },
  ],
};
const COLLECTION_IDS = ["ga1", "ga2", "gb1", "gb2", "alone"] as const;

/** Persist the two groups (the solo terminal is stored NOWHERE — that is what solo means). */
function seedCollection(): void {
  savePaneCollection("gate", {
    groups: [
      { id: "ga", tree: GROUP_A, name: "build", color: 4 },
      { id: "gb", tree: GROUP_B, name: null, color: null },
    ],
  });
}

it("RESTORE (collection): two groups and a solo terminal come back identical after a relaunch", async () => {
  const host = mountHost(TILING.width, TILING.height);
  seedCollection();
  // The remembered terminal is a member of the FIRST group — so the app reopens on that group.
  saveLastTerminalId("gate", "ga2");
  creations = [];
  const first = render(
    <RuntimeSelectionProvider>
      <TilingPage initial={COLLECTION_IDS.map((id) => runtimeTerminal(id))} />
    </RuntimeSelectionProvider>,
    { container: host },
  );
  expect(
    await waitFor(() => host.querySelectorAll('[data-visible="true"]').length === 2),
    "the remembered group came back with its two panes",
  ).toBe(true);

  // BOTH groups survived the restore, with their labels; the solo terminal is in neither.
  expect(gate.controller.groups.map((g) => [g.id, g.name, g.color])).toEqual([
    ["ga", "build", 4],
    ["gb", null, null],
  ]);
  expect(
    gate.controller.groups.flatMap((g) => paneLeaves(g.tree).map((l) => l.terminalId)),
  ).toEqual(["ga1", "ga2", "gb1", "gb2"]);

  // THE PIXELS: 37 % of the height for the first group's top pane, to the pixel — the persisted
  // ratio resolved against the REAL host, which is the half jsdom cannot state.
  const measured = (terminalId: string) => paneBox(host, terminalId);
  expect(measured("ga1").height).toBeCloseTo(TILING.height * 0.37, 0);
  expect(measured("ga2").height).toBeCloseTo(TILING.height * 0.63, 0);
  expect(measured("ga1").bottom).toBeCloseTo(measured("ga2").top, 0);
  expect(measured("ga1").width).toBeCloseTo(TILING.width, 0);
  const before = {
    ga1: measured("ga1").height,
    ga2: measured("ga2").height,
  };
  // …and each restored pane spawned its PTY at its OWN box, never the 80×24 fallback.
  expect(await waitFor(() => spawnSizes.has("ga1") && spawnSizes.has("ga2"))).toBe(true);
  expect((spawnSizes.get("ga1") as { cols: number }).cols).toBeGreaterThan(0);

  // THE SOLO TERMINAL: selecting it shows it ALONE, filling the frame — the exit from the tiling.
  await act(async () => {
    gate.select("alone");
  });
  expect(
    await waitFor(() => host.querySelectorAll('[data-visible="true"]').length === 1),
    "the solo terminal fills the frame on its own",
  ).toBe(true);
  expect(paneBox(host, "alone").width).toBeCloseTo(TILING.width, 0);
  expect(paneBox(host, "alone").height).toBeCloseTo(TILING.height, 0);
  // …and showing it changed NO group.
  expect(gate.controller.groups).toHaveLength(2);

  // A RELAUNCH: the renderer goes and comes back on the same `localStorage`.
  first.unmount();
  host.remove();
  const relaunched = mountHost(TILING.width, TILING.height);
  creations = [];
  render(
    <RuntimeSelectionProvider>
      <TilingPage initial={COLLECTION_IDS.map((id) => runtimeTerminal(id))} />
    </RuntimeSelectionProvider>,
    { container: relaunched },
  );
  expect(
    await waitFor(() => relaunched.querySelectorAll('[data-visible="true"]').length === 2),
    "the tiling came back after the relaunch",
  ).toBe(true);
  expect(gate.controller.groups.map((g) => [g.id, g.name, g.color])).toEqual([
    ["ga", "build", 4],
    ["gb", null, null],
  ]);
  // IDENTICAL, to the pixel.
  expect(paneBox(relaunched, "ga1").height).toBeCloseTo(before.ga1, 0);
  expect(paneBox(relaunched, "ga2").height).toBeCloseTo(before.ga2, 0);
});

/**
 * THE PANE CHORDS, EXERCISED IN REAL CHROMIUM (review 01M0ABR6V501D0SPR2BYBVZC4A, minor).
 *
 * jsdom can say the handler ran; only a browser can say the chord survives a real `keydown` through
 * the real hotkey manager while a real xterm holds the keyboard — which is precisely the case that
 * was broken (TanStack skips an Alt chord by default when an input is focused, and the focused
 * pane's terminal keeps a textarea focused at all times).
 */
it("CHORDS: Alt+arrows walk a four-pane tiling and Mod+Shift+W closes the focused pane", async () => {
  const host = mountHost(TILING.width, TILING.height);
  // The four panes of the fixture: ta / tb stacked on the left, tc / td on the right.
  savePaneLayout("gate", RESTORED_TREE);
  saveLastTerminalId("gate", "ta");
  creations = [];
  render(
    <RuntimeSelectionProvider>
      <TilingPage initial={RESTORED.map((id) => runtimeTerminal(id))} />
    </RuntimeSelectionProvider>,
    { container: host },
  );
  expect(await waitFor(() => host.querySelectorAll('[data-visible="true"]').length === 4)).toBe(
    true,
  );

  /** Which terminal the tiling reports as focused. */
  const focused = () =>
    (host.querySelector('[data-visible="true"][data-focused="true"]') as HTMLElement | null)
      ?.dataset.terminalId ?? null;
  const chord = (init: KeyboardEventInit) =>
    document.dispatchEvent(new KeyboardEvent("keydown", { bubbles: true, ...init }));
  const alt = (key: string) => chord({ key, altKey: true });

  expect(await waitFor(() => focused() === "ta")).toBe(true);

  // DOWN — ta (top-left) → tb (bottom-left).
  alt("ArrowDown");
  expect(await waitFor(() => focused() === "tb"), "Alt+↓ reached tb").toBe(true);
  // RIGHT — tb (bottom-left) → td (bottom-right), the pane whose left edge it touches.
  alt("ArrowRight");
  expect(await waitFor(() => focused() === "td"), "Alt+→ reached td").toBe(true);
  // UP — td → tc.
  alt("ArrowUp");
  expect(await waitFor(() => focused() === "tc"), "Alt+↑ reached tc").toBe(true);
  // LEFT — tc → ta.
  alt("ArrowLeft");
  expect(await waitFor(() => focused() === "ta"), "Alt+← reached ta").toBe(true);
  // AGAINST THE EDGE — nothing to the left of ta, and nothing above it: the focus stays put.
  alt("ArrowLeft");
  alt("ArrowUp");
  await new Promise((r) => setTimeout(r, 60));
  expect(focused(), "no wrap, no jump to the far side").toBe("ta");

  // CLOSE THE FOCUSED PANE — the tree compacts to three panes…
  chord({ key: "w", ctrlKey: true, shiftKey: true });
  expect(
    await waitFor(() => host.querySelectorAll('[data-visible="true"]').length === 3),
    "Mod+Shift+W closed one pane",
  ).toBe(true);
  // …and TA IS STILL ALIVE: closing a pane is a layout edit, so its terminal keeps its instance
  // (it is merely no longer tiled — it became solo).
  expect(getTerminal("ta")).not.toBeNull();
  expect(paneLeaves(gate.controller.layout as PaneNode).map((l) => l.terminalId)).not.toContain(
    "ta",
  );
  // The SPLIT chords still work, unchanged: Mod+Shift+↓ adds a pane back. (↓ and not →: at
  // 400px wide, halving a column would put both halves under the 180px floor and the split would
  // be REFUSED — which is the audited behaviour, not a chord failure.)
  creations = ["te"];
  chord({ key: "ArrowDown", ctrlKey: true, shiftKey: true });
  expect(
    await waitFor(() => host.querySelectorAll('[data-visible="true"]').length === 4),
    "Mod+Shift+↓ still splits",
  ).toBe(true);
});
