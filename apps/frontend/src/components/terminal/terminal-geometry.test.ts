import type { FitAddon } from "@xterm/addon-fit";
import type { Terminal as XTerm } from "@xterm/xterm";
import { describe, expect, it, vi } from "vitest";

import { reconcileTerminalGeometry } from "./terminal-geometry";

/**
 * A fake xterm + fit + element + side-effect spies, all recording into ONE
 * ordered call log so the test can assert the LOAD-BEARING pipeline order
 * (fit → resyncSize → clearWebglAtlas → refresh → scrollToBottom). jsdom has no
 * real layout / WebGL, which is exactly why the pipeline is extracted as a pure
 * function: it is fully exercisable here without a painted canvas.
 */
function makeHarness(box: { w: number; h: number }) {
  const order: string[] = [];
  const instance = {
    rows: 24,
    refresh: vi.fn((start: number, end: number) => {
      order.push(`refresh(${start},${end})`);
    }),
    scrollToBottom: vi.fn(() => {
      order.push("scrollToBottom");
    }),
  } as unknown as XTerm;
  const fitAddon = {
    fit: vi.fn(() => {
      order.push("fit");
    }),
  } as unknown as FitAddon;
  const resyncSize = vi.fn(() => {
    order.push("resyncSize");
  });
  const clearWebglAtlas = vi.fn(() => {
    order.push("clearWebglAtlas");
  });
  // A bare object stands in for the DOM element; only clientWidth/Height are read.
  const element = { clientWidth: box.w, clientHeight: box.h } as unknown as HTMLElement;
  return { order, instance, fitAddon, resyncSize, clearWebglAtlas, element };
}

describe("reconcileTerminalGeometry", () => {
  it("runs the pipeline in the load-bearing order when the pane has a real box", () => {
    const h = makeHarness({ w: 800, h: 600 });
    const ran = reconcileTerminalGeometry({
      instance: h.instance,
      element: h.element,
      fitAddon: h.fitAddon,
      resyncSize: h.resyncSize,
      clearWebglAtlas: h.clearWebglAtlas,
    });

    expect(ran).toBe(true);
    // The order is the WHOLE point: fit measures, the PTY is resized (SIGWINCH),
    // THEN the atlas is rebuilt and the screen repainted against the new metrics.
    expect(h.order).toEqual([
      "fit",
      "resyncSize",
      "clearWebglAtlas",
      "refresh(0,23)",
      "scrollToBottom",
    ]);
  });

  it("ALWAYS pushes a PTY resize (SIGWINCH) — even when the size did not change (#20/#23)", () => {
    // The reappear-at-same-size case (#20) and a resumed pty stuck at spawn size
    // (#23) both rely on resyncSize firing unconditionally: it is the only thing
    // that delivers a SIGWINCH to make the TUI redraw. It must NOT be gated on a
    // detected size delta.
    const h = makeHarness({ w: 640, h: 480 });
    reconcileTerminalGeometry({
      instance: h.instance,
      element: h.element,
      fitAddon: h.fitAddon,
      resyncSize: h.resyncSize,
      clearWebglAtlas: h.clearWebglAtlas,
    });
    expect(h.resyncSize).toHaveBeenCalledTimes(1);
  });

  it("rebuilds the WebGL atlas AND repaints (clearAtlas before refresh)", () => {
    const h = makeHarness({ w: 100, h: 100 });
    reconcileTerminalGeometry({
      instance: h.instance,
      element: h.element,
      fitAddon: h.fitAddon,
      resyncSize: h.resyncSize,
      clearWebglAtlas: h.clearWebglAtlas,
    });
    expect(h.clearWebglAtlas).toHaveBeenCalledTimes(1);
    expect(h.instance.refresh).toHaveBeenCalledWith(0, 23);
    // clearAtlas must come BEFORE refresh, or the repaint draws the stale atlas.
    expect(h.order.indexOf("clearWebglAtlas")).toBeLessThan(h.order.indexOf("refresh(0,23)"));
  });

  it("is a NO-OP for a hidden (0×0) pane — never bakes a bogus geometry", () => {
    // The core of the #20/#23 fix: while display:none the element is 0×0; fitting
    // / atlas-building there is what corrupts the render. The pipeline must skip.
    const h = makeHarness({ w: 0, h: 0 });
    const ran = reconcileTerminalGeometry({
      instance: h.instance,
      element: h.element,
      fitAddon: h.fitAddon,
      resyncSize: h.resyncSize,
      clearWebglAtlas: h.clearWebglAtlas,
    });
    expect(ran).toBe(false);
    expect(h.order).toEqual([]);
    expect(h.fitAddon.fit).not.toHaveBeenCalled();
    expect(h.resyncSize).not.toHaveBeenCalled();
    expect(h.clearWebglAtlas).not.toHaveBeenCalled();
  });

  it("skips a pane with zero WIDTH only (one collapsed axis is still hidden)", () => {
    const h = makeHarness({ w: 0, h: 600 });
    const ran = reconcileTerminalGeometry({
      instance: h.instance,
      element: h.element,
      fitAddon: h.fitAddon,
      resyncSize: h.resyncSize,
      clearWebglAtlas: h.clearWebglAtlas,
    });
    expect(ran).toBe(false);
  });

  it("is inert with a null instance or null element", () => {
    const h = makeHarness({ w: 800, h: 600 });
    expect(
      reconcileTerminalGeometry({
        instance: null,
        element: h.element,
        fitAddon: h.fitAddon,
        resyncSize: h.resyncSize,
        clearWebglAtlas: h.clearWebglAtlas,
      }),
    ).toBe(false);
    expect(
      reconcileTerminalGeometry({
        instance: h.instance,
        element: null,
        fitAddon: h.fitAddon,
        resyncSize: h.resyncSize,
        clearWebglAtlas: h.clearWebglAtlas,
      }),
    ).toBe(false);
    expect(h.resyncSize).not.toHaveBeenCalled();
  });

  it("does not throw when refresh/scrollToBottom throw (detached instance)", () => {
    const h = makeHarness({ w: 800, h: 600 });
    (h.instance.refresh as unknown as ReturnType<typeof vi.fn>).mockImplementation(() => {
      throw new Error("detached");
    });
    (h.instance.scrollToBottom as unknown as ReturnType<typeof vi.fn>).mockImplementation(() => {
      throw new Error("detached");
    });
    expect(() =>
      reconcileTerminalGeometry({
        instance: h.instance,
        element: h.element,
        fitAddon: h.fitAddon,
        resyncSize: h.resyncSize,
        clearWebglAtlas: h.clearWebglAtlas,
      }),
    ).not.toThrow();
    // The earlier steps still ran (fit + the SIGWINCH-bearing resync + atlas).
    expect(h.resyncSize).toHaveBeenCalledTimes(1);
    expect(h.clearWebglAtlas).toHaveBeenCalledTimes(1);
  });
});
