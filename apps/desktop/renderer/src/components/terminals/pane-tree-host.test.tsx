import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { mockIPC } from "@/bridge/test-harness";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { __resetTerminalStore, getTerminal } from "@/components/terminal/terminal-store";
import type { TerminalRecord } from "@/components/sidebar/use-terminals";
import { PaneTreeHost } from "./pane-tree-host";
import {
  MIN_PANE_WIDTH_PX,
  SEPARATOR_KEY_STEP,
  SEPARATOR_THICKNESS_PX,
} from "@/components/panes/pane-geometry";
import {
  closePane,
  createPaneLayout,
  MAX_SPLIT_RATIO,
  MIN_SPLIT_RATIO,
  splitPane,
  type PaneNode,
} from "@/components/panes/pane-tree";

interface IpcRecorder {
  spawns: string[];
  closes: number[];
}

function installIpc(): IpcRecorder {
  const rec: IpcRecorder = { spawns: [], closes: [] };
  let nextPtyId = 100;
  mockIPC(
    (cmd, args) => {
      const a = (args ?? {}) as Record<string, unknown>;
      if (cmd === "pty_spawn") {
        rec.spawns.push(String(a.terminalId));
        return nextPtyId++;
      }
      if (cmd === "pty_close") rec.closes.push((a.id as number) ?? -1);
      return null;
    },
    { shouldMockEvents: true },
  );
  return rec;
}

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

const records = [record("t1"), record("t2"), record("t3")];

/** `t1 | t2` — two visible panes, `t3` mounted but not tiled. */
function twoPanes(): PaneNode {
  return splitPane(createPaneLayout("t1", "p1"), {
    paneId: "p1",
    direction: "row",
    terminalId: "t2",
    newPaneId: "p2",
  }) as PaneNode;
}

function panes(container: HTMLElement, selector = "[data-terminal-id]") {
  return Array.from(container.querySelectorAll<HTMLElement>(selector));
}

/** The pane slots BY terminal id — the ELEMENTS, so their identity can be compared later. */
function slotsById(container: HTMLElement): Map<string, HTMLElement> {
  return new Map(panes(container).map((el) => [String(el.getAttribute("data-terminal-id")), el]));
}

/**
 * Assert that NO pane slot was rebuilt between `before` and now.
 *
 * "The host re-lays out without REMOUNTING" is the property every no-second-spawn claim rests on,
 * and it is invisible to every other reading in this file: a remounted pane has the same markup,
 * the same store instance (`ensureTerminal` is idempotent) and — for that same reason — the same
 * PTY count. Only the DOM NODE changes: React discards the old element and builds a new one. So
 * node identity is the only thing that can pin it, and without it a host whose slot key varies
 * with the pane id — one that tears every terminal down and back up on each split, the very
 * arrangement in which the store's idempotence is all that stands between the app and a second
 * PTY — passes this whole family untouched.
 */
function expectSameSlots(before: Map<string, HTMLElement>, container: HTMLElement): void {
  const now = slotsById(container);
  for (const [id, el] of before) {
    expect(now.get(id), `${id}'s pane slot was REMOUNTED (a new DOM node)`).toBe(el);
  }
}

/**
 * `pty_spawn` calls COUNTED PER `terminal_id` — the only reading that can SEE a duplicate.
 *
 * `new Set(spawns).size === 3` cannot, by construction: de-duplicating the ids and then counting
 * them erases exactly what is being looked for. Three records that spawned four PTYs
 * (`t1 t2 t3 t3`) still collapse to a set of three, so that assertion holds on the very defect it
 * claims to guard against. The browser gate counts per id for this reason
 * (`pane-tiling-gate.browser.test.tsx`); this is the same reading in the tier CI actually runs.
 */
function spawnsPerTerminal(ipc: IpcRecorder): Map<string, number> {
  const counts = new Map<string, number>();
  for (const id of ipc.spawns) counts.set(id, (counts.get(id) ?? 0) + 1);
  return counts;
}

/** How long the spawn stream must stay SILENT before its count is taken as final. */
const SPAWN_QUIET_MS = 250;

/**
 * WAIT FOR THE SPAWNS TO STOP COMING, then count them per `terminal_id`.
 *
 * WHY A PLAIN `waitFor` IS NOT ENOUGH. `startPty()` is ASYNC and sits behind `awaitMeasuredBox`
 * — a budget of MACROTASKS, spent in full under jsdom where no box is ever measurable — and then
 * behind `rehydrateScreen()`. A duplicate `pty_spawn` therefore lands SEVERAL turns of the event
 * loop after the mount that caused it. `waitFor(() => expect(spawns).toHaveLength(3))` resolves on
 * the FIRST sight of three: a LOWER bound that a fourth spawn one macrotask later slips straight
 * past — and so does any `expect(spawns).toHaveLength(3)` written just after such a `waitFor`.
 *
 * This resolves only once NO new spawn has arrived for {@link SPAWN_QUIET_MS}, so the count taken
 * from it is the FINAL one rather than the earliest one.
 */
async function settledSpawns(ipc: IpcRecorder): Promise<Map<string, number>> {
  let seen = -1;
  let lastChange = Date.now();
  await waitFor(
    () => {
      if (ipc.spawns.length !== seen) {
        seen = ipc.spawns.length;
        lastChange = Date.now();
      }
      const quiet = Date.now() - lastChange;
      if (quiet < SPAWN_QUIET_MS) {
        throw new Error(`spawns still arriving ([${ipc.spawns.join(", ")}], quiet ${quiet}ms)`);
      }
    },
    { interval: 10, timeout: 10_000 },
  );
  return spawnsPerTerminal(ipc);
}

/** EXACTLY ONE PTY per given `terminal_id`, and not one PTY for anybody else. */
function expectOnePtyEach(counts: Map<string, number>, ids: string[]): void {
  for (const id of ids) expect(counts.get(id), `${id} owns exactly one PTY`).toBe(1);
  expect([...counts.keys()].sort(), "a terminal spawned that was never asked to").toEqual(
    [...ids].sort(),
  );
}

describe("<PaneTreeHost>", () => {
  beforeEach(() => {
    installIpc();
  });

  afterEach(() => {
    __resetTerminalStore();
  });

  it("mounts one slot per record and lays out ONLY the tree's leaves", async () => {
    const { container } = render(
      <PaneTreeHost
        terminals={records}
        layout={twoPanes()}
        focusedPaneId="p1"
        onFocusPane={() => {}}
        onSplitPane={() => {}}
        onClosePane={() => {}}
      />,
    );
    await waitFor(() => expect(panes(container)).toHaveLength(3));

    const byId = new Map(panes(container).map((el) => [el.getAttribute("data-terminal-id"), el]));
    // VISIBLE = every leaf, laid out side by side from the tree's ratios.
    expect(byId.get("t1")?.getAttribute("data-visible")).toBe("true");
    expect(byId.get("t1")?.style.left).toBe("0%");
    expect(byId.get("t1")?.style.width).toBe("50%");
    expect(byId.get("t2")?.getAttribute("data-visible")).toBe("true");
    expect(byId.get("t2")?.style.left).toBe("50%");
    // NOT in the tree: mounted (instance + PTY alive) but boxless.
    expect(byId.get("t3")?.getAttribute("data-visible")).toBe("false");
    expect(byId.get("t3")?.style.display).toBe("none");
  });

  it("marks exactly ONE pane focused, distinct from the visible set", async () => {
    const { container } = render(
      <PaneTreeHost
        terminals={records}
        layout={twoPanes()}
        focusedPaneId="p2"
        onFocusPane={() => {}}
        onSplitPane={() => {}}
        onClosePane={() => {}}
      />,
    );
    await waitFor(() => expect(panes(container)).toHaveLength(3));
    const focused = panes(container).filter((el) => el.getAttribute("data-focused") === "true");
    const visible = panes(container).filter((el) => el.getAttribute("data-visible") === "true");
    expect(visible).toHaveLength(2);
    expect(focused).toHaveLength(1);
    expect(focused[0]?.getAttribute("data-terminal-id")).toBe("t2");
  });

  it("gives every visible terminal ONE store instance, ONE PTY and ONE DOM attachment", async () => {
    const ipc = installIpc();
    const { container } = render(
      <PaneTreeHost
        terminals={records}
        layout={twoPanes()}
        focusedPaneId="p1"
        onFocusPane={() => {}}
        onSplitPane={() => {}}
        onClosePane={() => {}}
      />,
    );
    // ONE PTY per record, counted PER `terminal_id` and read only once the spawn stream has gone
    // silent: a duplicate is a SECOND entry under an EXISTING id — precisely what a set of ids
    // cannot show, and what a `waitFor` stopping at the first sight of three cannot wait for.
    expectOnePtyEach(await settledSpawns(ipc), ["t1", "t2", "t3"]);
    for (const id of ["t1", "t2", "t3"]) {
      expect(getTerminal(id)).toBeDefined();
      // The instance's persistent node is attached EXACTLY once, inside its own pane slot.
      const nodes = container.querySelectorAll(`[data-terminal-node="${id}"]`);
      expect(nodes).toHaveLength(1);
      expect(nodes[0]?.closest("[data-terminal-id]")?.getAttribute("data-terminal-id")).toBe(id);
    }
  });

  it("THE COUNTER IS NOT BLIND: a second PTY for one terminal_id is visible to these tests", async () => {
    // WHY THIS EXISTS. Every test above (and the phase-2 gate) rests on "one `pty_spawn` per
    // terminal_id" — the cardinal sin of a host that remounts its panes. So this forces a REAL
    // double spawn and puts it back through `expectOnePtyEach`, THE ASSERTION THE OTHER TESTS
    // MAKE: what has to be shown is that the ASSERTION fails, not that the recorder holds two
    // entries — the recorder was never in doubt, the reading was. The browser gate carries the
    // same control, but browser mode needs a Chromium that CI's unit job does not have; this is
    // the twin that runs wherever the unit suite does.
    const ipc = installIpc();
    const one = [record("t1")];
    const first = render(
      <PaneTreeHost
        terminals={one}
        layout={createPaneLayout("t1", "p1")}
        focusedPaneId="p1"
        onFocusPane={() => {}}
        onSplitPane={() => {}}
        onClosePane={() => {}}
      />,
    );
    expectOnePtyEach(await settledSpawns(ipc), ["t1"]);

    // Force EXACTLY the defect the other tests assert against: the store loses the instance, so
    // the next mount BUILDS A NEW ONE and opens a SECOND PTY for the same terminal_id.
    first.unmount();
    __resetTerminalStore();
    render(
      <PaneTreeHost
        terminals={one}
        layout={createPaneLayout("t1", "p1")}
        focusedPaneId="p1"
        onFocusPane={() => {}}
        onSplitPane={() => {}}
        onClosePane={() => {}}
      />,
    );

    const counts = await settledSpawns(ipc);
    expect(counts.get("t1"), "one terminal, two PTYs").toBe(2);
    // THE POINT: the assertion the rest of this file makes REJECTS this run.
    expect(
      () => expectOnePtyEach(counts, ["t1"]),
      "the one-PTY-per-terminal assertion must reject a double spawn",
    ).toThrow();
    // …whereas the reading this test used to make — de-duplicate, then count — reports a perfectly
    // healthy ONE. That is why it is gone: it is blind to the only defect it existed to catch.
    expect(new Set(ipc.spawns).size, "the de-duplicated reading sees nothing wrong").toBe(1);
  });

  it("a SPLIT re-lays out without remounting (no second spawn, still one attachment)", async () => {
    const ipc = installIpc();
    const layout = twoPanes();
    const { container, rerender } = render(
      <PaneTreeHost
        terminals={records}
        layout={layout}
        focusedPaneId="p1"
        onFocusPane={() => {}}
        onSplitPane={() => {}}
        onClosePane={() => {}}
      />,
    );
    expectOnePtyEach(await settledSpawns(ipc), ["t1", "t2", "t3"]);
    const before = slotsById(container);

    const split = splitPane(layout, {
      paneId: "p2",
      direction: "column",
      terminalId: "t3",
      newPaneId: "p3",
    }) as PaneNode;
    rerender(
      <PaneTreeHost
        terminals={records}
        layout={split}
        focusedPaneId="p3"
        onFocusPane={() => {}}
        onSplitPane={() => {}}
        onClosePane={() => {}}
      />,
    );

    await waitFor(() =>
      expect(container.querySelector('[data-terminal-id="t3"]')?.getAttribute("data-visible")).toBe(
        "true",
      ),
    );
    // No remount ⇒ the very same slot ELEMENTS, no new instance, no new PTY, still a single DOM
    // attachment each. The spawn count is read once the stream has settled, so a respawn triggered
    // by the split has had every macrotask it needs to show up.
    expectSameSlots(before, container);
    expectOnePtyEach(await settledSpawns(ipc), ["t1", "t2", "t3"]);
    for (const id of ["t1", "t2", "t3"]) {
      expect(container.querySelectorAll(`[data-terminal-node="${id}"]`)).toHaveLength(1);
    }
  });

  it("CLOSING a pane (the layout shrinks) never closes the terminal", async () => {
    const ipc = installIpc();
    const layout = twoPanes();
    const { container, rerender } = render(
      <PaneTreeHost
        terminals={records}
        layout={layout}
        focusedPaneId="p2"
        onFocusPane={() => {}}
        onSplitPane={() => {}}
        onClosePane={() => {}}
      />,
    );
    expectOnePtyEach(await settledSpawns(ipc), ["t1", "t2", "t3"]);
    const before = slotsById(container);

    rerender(
      <PaneTreeHost
        terminals={records}
        layout={closePane(layout, "p2")}
        focusedPaneId="p1"
        onFocusPane={() => {}}
        onSplitPane={() => {}}
        onClosePane={() => {}}
      />,
    );
    await waitFor(() =>
      expect(container.querySelector('[data-terminal-id="t2"]')?.getAttribute("data-visible")).toBe(
        "false",
      ),
    );
    // The pane is gone from the tiling; the PTY, the instance and the record are untouched — and
    // no slot was rebuilt, so leaving the tree costs the departing terminal nothing either.
    expectSameSlots(before, container);
    expectOnePtyEach(await settledSpawns(ipc), ["t1", "t2", "t3"]);
    expect(ipc.closes).toHaveLength(0);
    expect(getTerminal("t2")).toBeDefined();
    expect(container.querySelector('[data-terminal-id="t2"]')).not.toBeNull();
  });

  it("wires the per-pane split / close controls", async () => {
    const onSplitPane = vi.fn();
    const onClosePane = vi.fn();
    render(
      <PaneTreeHost
        terminals={records}
        layout={twoPanes()}
        focusedPaneId="p1"
        onFocusPane={() => {}}
        onSplitPane={onSplitPane}
        onClosePane={onClosePane}
      />,
    );
    const right = await screen.findAllByLabelText("Split pane right");
    const down = await screen.findAllByLabelText("Split pane down");
    const close = await screen.findAllByLabelText("Close pane");
    // One control cluster per VISIBLE pane (two panes here).
    expect(right).toHaveLength(2);

    fireEvent.click(right[0] as HTMLElement);
    fireEvent.click(down[1] as HTMLElement);
    fireEvent.click(close[1] as HTMLElement);
    expect(onSplitPane).toHaveBeenNthCalledWith(1, "p1", "row");
    expect(onSplitPane).toHaveBeenNthCalledWith(2, "p2", "column");
    expect(onClosePane).toHaveBeenCalledWith("p2");
  });

  it("hides the close control when a single pane is left (the last pane is kept)", async () => {
    render(
      <PaneTreeHost
        terminals={[records[0] as TerminalRecord]}
        layout={createPaneLayout("t1", "p1")}
        focusedPaneId="p1"
        onFocusPane={() => {}}
        onSplitPane={() => {}}
        onClosePane={() => {}}
      />,
    );
    expect(await screen.findByLabelText("Split pane right")).toBeTruthy();
    expect(screen.queryByLabelText("Close pane")).toBeNull();
  });

  it("focuses the pane the pointer presses", async () => {
    const onFocusPane = vi.fn();
    const { container } = render(
      <PaneTreeHost
        terminals={records}
        layout={twoPanes()}
        focusedPaneId="p1"
        onFocusPane={onFocusPane}
        onSplitPane={() => {}}
        onClosePane={() => {}}
      />,
    );
    await waitFor(() => expect(panes(container)).toHaveLength(3));
    fireEvent.mouseDown(container.querySelector('[data-terminal-id="t2"]') as HTMLElement);
    expect(onFocusPane).toHaveBeenCalledWith("p2");
  });

  it("renders NO separator when the host is read-only (no resize callback)", async () => {
    const { container } = render(
      <PaneTreeHost
        terminals={records}
        layout={twoPanes()}
        focusedPaneId="p1"
        onFocusPane={() => {}}
        onSplitPane={() => {}}
        onClosePane={() => {}}
      />,
    );
    await waitFor(() => expect(panes(container)).toHaveLength(3));
    // An inert focusable splitter would announce a resize a keyboard user cannot perform.
    expect(container.querySelectorAll("[data-separator-id]")).toHaveLength(0);
  });

  /**
   * EVERY VISIBLE PANE CARRIES ITS OWN OVERLAY (review finding 01M11WF11C0H8G2MG515FBT6RD).
   *
   * The host used to call the renderer for the FOCUSED pane only, so a terminal working a Spec
   * showed nothing the moment the keyboard moved on. The surcouche belongs to the terminal, so the
   * call is per VISIBLE pane — and each result is anchored in ITS OWN pane, never in a neighbour
   * and never over the main area. A terminal that is not in the tiling gets nothing at all.
   */
  it("renders ONE overlay per VISIBLE pane, each anchored inside its OWN pane", async () => {
    const { container } = render(
      <PaneTreeHost
        terminals={records}
        layout={twoPanes()}
        focusedPaneId="p2"
        onFocusPane={() => {}}
        onSplitPane={() => {}}
        onClosePane={() => {}}
        renderPaneOverlay={(terminalId) => <div data-overlay-for={terminalId} />}
      />,
    );
    await waitFor(() => expect(panes(container)).toHaveLength(3));
    const overlays = Array.from(container.querySelectorAll("[data-overlay-for]"));
    // BOTH visible terminals, focused or not — `t3` is mounted but not tiled, so it gets none.
    expect(overlays.map((el) => el.getAttribute("data-overlay-for"))).toEqual(["t1", "t2"]);
    // Anchored INSIDE its own pane (that pane's positioning block), not over the whole main area
    // and not in the neighbour's box.
    for (const overlay of overlays) {
      expect(overlay.closest("[data-terminal-id]")?.getAttribute("data-terminal-id")).toBe(
        overlay.getAttribute("data-overlay-for"),
      );
    }
    // Focusing the OTHER pane makes none of them disappear.
    expect(container.querySelectorAll('[data-focused="true"]')).toHaveLength(1);
  });
});

/**
 * THE SEPARATORS — the resize affordance on each split's seam.
 *
 * jsdom has no layout engine and no pointer capture, so the two are FAKED here (a stubbed host
 * box, a spied `setPointerCapture`) to assert the WIRING: which ratio each gesture asks for, in
 * which phase, and that the press really requests capture and suppresses the native selection.
 * That the resulting boxes and PTY sizes are right is asserted in the browser spec, where boxes
 * are real.
 */
describe("<PaneTreeHost> — separators", () => {
  /** The host box the drag maths is done against (jsdom reports 0×0 for everything). */
  const HOST_BOX = { left: 0, top: 0, width: 1000, height: 500 };

  let captured: number[];

  beforeEach(() => {
    installIpc();
    captured = [];
    // jsdom implements neither of these; the component only ever calls them on a real pointer.
    Element.prototype.setPointerCapture = function setPointerCapture(pointerId: number) {
      captured.push(pointerId);
    };
    Element.prototype.releasePointerCapture = () => {};
  });

  /** Render a `t1 | t2` tiling with a live resize callback, and stub the host's measured box. */
  function renderResizable() {
    const onResizeSplit = vi.fn();
    const { container } = render(
      <PaneTreeHost
        terminals={records}
        layout={twoPanes()}
        focusedPaneId="p1"
        onFocusPane={() => {}}
        onSplitPane={() => {}}
        onClosePane={() => {}}
        onResizeSplit={onResizeSplit}
      />,
    );
    const host = container.querySelector('[data-pane-host="true"]') as HTMLElement;
    host.getBoundingClientRect = () =>
      ({ ...HOST_BOX, right: 1000, bottom: 500, x: 0, y: 0, toJSON: () => ({}) }) as DOMRect;
    const separator = container.querySelector("[data-separator-id]") as HTMLElement;
    return { container, host, separator, onResizeSplit };
  }

  it("exposes ONE ARIA window splitter per split, with its bounds and current value", async () => {
    const { container } = renderResizable();
    await waitFor(() => expect(panes(container)).toHaveLength(3));

    const separators = Array.from(container.querySelectorAll<HTMLElement>("[data-separator-id]"));
    // One split ⇒ one separator, focusable and announced with the band it may move in.
    expect(separators).toHaveLength(1);
    const separator = separators[0] as HTMLElement;
    expect(separator.getAttribute("role")).toBe("separator");
    expect(separator.tabIndex).toBe(0);
    // A `row` split lays its panes SIDE BY SIDE ⇒ the bar itself is vertical.
    expect(separator.getAttribute("aria-orientation")).toBe("vertical");
    expect(separator.getAttribute("aria-valuenow")).toBe("50");
    expect(separator.getAttribute("aria-valuemin")).toBe(String(Math.round(MIN_SPLIT_RATIO * 100)));
    expect(separator.getAttribute("aria-valuemax")).toBe(String(Math.round(MAX_SPLIT_RATIO * 100)));
  });

  it("AFFORDANCE: the seam is PAINTED at rest, and hover / focus / drag each step above it", () => {
    const { separator } = renderResizable();
    const atRest = separator.className;

    // AT REST: the 8px band stays transparent (it must not sit as a grey slab over the terminals)
    // but a HAIRLINE is painted inside it — so two adjacent panes show their seam, and the handle
    // can be found, with NO interaction at all. That resting paint is the whole finding.
    expect(atRest).toContain("before:content-['']");
    // `/85`, not the original `/50`: measured in real Chromium on the app's dark palette, `/50`
    // composited to a 1.88:1 contrast against the terminals' background — painted, but under the
    // WCAG 1.4.11 non-text floor. `/85` is the first step that clears 3:1 (finding F3, whose own
    // measurement lives in the browser spec).
    expect(atRest).toContain("before:bg-ring/85");
    expect(atRest).toContain("bg-transparent");
    // THREE DISTINCT interaction steps above the resting state, each stronger than the last.
    expect(atRest).toContain("hover:bg-ring/40");
    expect(atRest).toContain("hover:before:bg-ring/95");
    expect(atRest).toContain("focus-visible:bg-ring/60");
    expect(atRest).toContain("focus-visible:before:bg-ring");

    // The GRAB BAND is untouched: the trait is 1px of PAINT inside the SAME 8px hit area the drag
    // rides on (`separatorStyle`'s inline box) — a thinner line must never mean a thinner target.
    expect(separator.style.width).toBe(`${SEPARATOR_THICKNESS_PX}px`);
    expect(atRest).toContain("before:w-px");

    // …and the DRAG state is a fourth, distinct one.
    fireEvent.pointerDown(separator, { pointerId: 7, button: 0, clientX: 500, clientY: 250 });
    expect(separator.className).toContain("bg-ring/70");
    expect(separator.className).not.toContain("bg-transparent");
  });

  it("KEYBOARD: an arrow steps the ratio and commits it straight away", async () => {
    const { separator, onResizeSplit } = renderResizable();
    const splitId = separator.getAttribute("data-separator-id") as string;

    fireEvent.keyDown(separator, { key: "ArrowRight" });
    // A key press IS the whole interaction, so it lands as a COMMIT (the persisted phase).
    expect(onResizeSplit).toHaveBeenCalledWith(splitId, 0.5 + SEPARATOR_KEY_STEP, "commit");

    // Home / End jump to the END OF THE COURSE — which on this 1000px host is the ABSOLUTE floor
    // (180px a side ⇒ 18% / 82%), not the relative 15 / 85: the two are cumulative and the
    // tighter one wins. `MIN_SPLIT_RATIO` would let this pane sit at 150px.
    fireEvent.keyDown(separator, { key: "Home" });
    expect(onResizeSplit).toHaveBeenLastCalledWith(
      splitId,
      MIN_PANE_WIDTH_PX / HOST_BOX.width,
      "commit",
    );
    fireEvent.keyDown(separator, { key: "End" });
    expect(onResizeSplit).toHaveBeenLastCalledWith(
      splitId,
      1 - MIN_PANE_WIDTH_PX / HOST_BOX.width,
      "commit",
    );
    expect(MIN_PANE_WIDTH_PX / HOST_BOX.width).toBeGreaterThan(MIN_SPLIT_RATIO);
  });

  it("KEYBOARD: an unrelated key is left to the page", () => {
    const { separator, onResizeSplit } = renderResizable();
    fireEvent.keyDown(separator, { key: "Tab" });
    fireEvent.keyDown(separator, { key: "a" });
    expect(onResizeSplit).not.toHaveBeenCalled();
  });

  it("DRAG: previews while moving and commits ONCE on release, always within the bounds", () => {
    const { host, separator, onResizeSplit } = renderResizable();
    const splitId = separator.getAttribute("data-separator-id") as string;

    const dispatched = fireEvent.pointerDown(separator, {
      pointerId: 7,
      button: 0,
      clientX: 500,
      clientY: 250,
    });
    // THE PRESS IS `preventDefault`-ED, and that is the MECHANISM the criterion rests on: a native
    // text selection starts on the press itself, i.e. BEFORE the `select-none` below can exist (it
    // only appears in the render that follows this handler). `select-none` keeps a selection from
    // being PAINTED while the pointer sweeps the terminals; cancelling the default action is what
    // keeps one from ever STARTING. `fireEvent` returns `false` exactly when the default was
    // prevented, so this asserts the call rather than the comment claiming it.
    expect(dispatched, "the press must cancel the native selection/drag").toBe(false);
    // POINTER CAPTURE is taken on the press, so the drag survives flying over a terminal.
    expect(captured).toEqual([7]);
    // …and the host suppresses text selection for the whole gesture (no highlighted terminal).
    expect(host.getAttribute("data-resizing")).toBe(splitId);
    expect(host.className).toContain("select-none");

    fireEvent.pointerMove(separator, { pointerId: 7, clientX: 700, clientY: 250 });
    expect(onResizeSplit).toHaveBeenLastCalledWith(splitId, 0.7, "preview");
    // Past the edge the PREVIEW is already clamped — the pane never collapses mid-drag, and the
    // stop is the ABSOLUTE floor in pixels (180px of this 1000px host = 82%), not the constant 85%.
    fireEvent.pointerMove(separator, { pointerId: 7, clientX: 5000, clientY: 250 });
    expect(onResizeSplit).toHaveBeenLastCalledWith(
      splitId,
      1 - MIN_PANE_WIDTH_PX / HOST_BOX.width,
      "preview",
    );

    fireEvent.pointerUp(separator, { pointerId: 7, clientX: 600, clientY: 250 });
    expect(onResizeSplit).toHaveBeenLastCalledWith(splitId, 0.6, "commit");
    // EXACTLY ONE commit: the `lostpointercapture` that follows a release must not write twice.
    fireEvent.lostPointerCapture(separator, { pointerId: 7 });
    expect(onResizeSplit.mock.calls.filter((call) => call[2] === "commit")).toHaveLength(1);
    expect(host.getAttribute("data-resizing")).toBeNull();
  });

  it("DRAG: a foreign pointer cannot move or end someone else's drag", () => {
    const { separator, onResizeSplit } = renderResizable();
    fireEvent.pointerDown(separator, { pointerId: 7, button: 0, clientX: 500, clientY: 250 });
    fireEvent.pointerMove(separator, { pointerId: 9, clientX: 800, clientY: 250 });
    fireEvent.pointerUp(separator, { pointerId: 9, clientX: 800, clientY: 250 });
    expect(onResizeSplit).not.toHaveBeenCalled();
  });

  it("DRAG: a cancelled gesture still commits, at the last previewed ratio", () => {
    const { separator, onResizeSplit } = renderResizable();
    const splitId = separator.getAttribute("data-separator-id") as string;
    fireEvent.pointerDown(separator, { pointerId: 7, button: 0, clientX: 500, clientY: 250 });
    fireEvent.pointerMove(separator, { pointerId: 7, clientX: 650, clientY: 250 });
    fireEvent.pointerCancel(separator, { pointerId: 7 });
    // The interaction ENDED — an aborted drag must not leave the layout unpersisted.
    expect(onResizeSplit).toHaveBeenLastCalledWith(splitId, 0.65, "commit");
  });

  it("positions a NESTED separator on its own sub-box", async () => {
    const nested = splitPane(twoPanes(), {
      paneId: "p2",
      direction: "column",
      terminalId: "t3",
      newPaneId: "p3",
    }) as PaneNode;
    const { container } = render(
      <PaneTreeHost
        terminals={records}
        layout={nested}
        focusedPaneId="p1"
        onFocusPane={() => {}}
        onSplitPane={() => {}}
        onClosePane={() => {}}
        onResizeSplit={() => {}}
      />,
    );
    await waitFor(() => expect(container.querySelectorAll("[data-separator-id]")).toHaveLength(2));
    const [outer, inner] = Array.from(
      container.querySelectorAll<HTMLElement>("[data-separator-id]"),
    );
    // The outer bar spans the whole host on the vertical seam…
    expect((outer as HTMLElement).getAttribute("data-direction")).toBe("row");
    expect((outer as HTMLElement).style.height).toBe("100%");
    // …the inner one only spans the right-hand half, on the horizontal seam.
    expect((inner as HTMLElement).getAttribute("data-direction")).toBe("column");
    expect((inner as HTMLElement).style.left).toBe("50%");
    expect((inner as HTMLElement).style.width).toBe("50%");
  });
});
