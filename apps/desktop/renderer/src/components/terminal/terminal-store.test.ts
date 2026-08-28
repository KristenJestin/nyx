import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { emit, mockIPC } from "@/bridge/test-harness";
import { awaitMeasuredBox, chooseSpawnDimensions } from "./xterm-instance";
import {
  __resetTerminalStore,
  attachTerminal,
  detachTerminal,
  disposeTerminal,
  ensureTerminal,
  getTerminal,
  reconcileTerminals,
  terminalStore,
} from "./terminal-store";

/**
 * The MODULE-LEVEL STORE contract (jsdom): xterm instances live in a module-level
 * Map, OUTSIDE React / the route lifecycle. Navigation DETACHES an instance's DOM
 * node (never disposes), the PTY subscription lives IN the instance so output keeps
 * flowing into the buffer while detached, and a reattach is lossless. The instance
 * is DISPOSED (PTY closed) ONLY on a real close (dispose / reconcile).
 *
 * xterm renders to a canvas jsdom can't paint (getContext is stubbed to null in
 * vitest.setup), so we assert the BUFFER (the source of truth), not pixels.
 */

const FIRST_PTY_ID = 100;
let spawnedIds: number[] = [];
let closes: number[] = [];

/** Poll until `pred()` holds or we time out; xterm parses writes asynchronously. */
async function waitFor(pred: () => boolean, timeoutMs = 1000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (pred()) return;
    await new Promise((r) => setTimeout(r, 5));
  }
  expect(pred(), "waitFor predicate timed out").toBe(true);
}

/** Wait until the instance's spawn has resolved (its PTY id is known). */
async function waitForSpawn(id: string): Promise<void> {
  await waitFor(() => getTerminal(id)?.pty != null);
}

describe("terminal-store (module-level keep-alive)", () => {
  beforeEach(() => {
    spawnedIds = [];
    closes = [];
    mockIPC((cmd, args) => {
      if (cmd === "pty_spawn") {
        const id = FIRST_PTY_ID + spawnedIds.length;
        spawnedIds.push(id);
        return id;
      }
      if (cmd === "pty_close") {
        closes.push((args?.id as number) ?? -1);
      }
      return null;
    });
  });

  afterEach(() => {
    __resetTerminalStore();
  });

  it("holds instances in a module-level Map and publishes ids reactively", async () => {
    ensureTerminal({ recordId: "a" });
    ensureTerminal({ recordId: "b" });
    expect(getTerminal("a")).toBeDefined();
    expect(getTerminal("b")).toBeDefined();
    expect(terminalStore.state.ids).toEqual(["a", "b"]);
    await waitForSpawn("a");
  });

  it("ensureTerminal is idempotent — a second call never respawns", async () => {
    const first = ensureTerminal({ recordId: "a" });
    await waitForSpawn("a");
    const spawnsAfterFirst = spawnedIds.length;

    const again = ensureTerminal({ recordId: "a" });
    expect(again).toBe(first);
    // Give any (erroneous) second spawn a chance to fire, then assert none did.
    await new Promise((r) => setTimeout(r, 20));
    expect(spawnedIds.length).toBe(spawnsAfterFirst);
  });

  it("attach MOVES the same node; detach removes it WITHOUT disposing", async () => {
    const instance = ensureTerminal({ recordId: "a" });
    await waitForSpawn("a");

    const c1 = document.createElement("div");
    document.body.appendChild(c1);
    attachTerminal("a", c1);
    expect(instance.el.parentElement).toBe(c1);
    // The focusable helper textarea lives INSIDE the node, so focus survives moves.
    expect(instance.el.querySelector(".xterm-helper-textarea")).not.toBeNull();

    // DETACH (navigation): node removed, but the instance stays in the store and
    // the PTY is NOT closed.
    detachTerminal("a");
    expect(instance.el.parentElement).toBeNull();
    expect(getTerminal("a")).toBe(instance);
    expect(closes).toHaveLength(0);

    // REATTACH into a DIFFERENT container (the return route's pane): SAME node.
    const c2 = document.createElement("div");
    document.body.appendChild(c2);
    attachTerminal("a", c2);
    expect(instance.el.parentElement).toBe(c2);
    expect(instance.el.querySelector(".xterm-helper-textarea")).not.toBeNull();
  });

  it("PTY output written WHILE DETACHED lands in the buffer, present on reattach", async () => {
    const instance = ensureTerminal({ recordId: "a" });
    await waitForSpawn("a");
    const ptyId = instance.pty as number;

    const c1 = document.createElement("div");
    document.body.appendChild(c1);
    attachTerminal("a", c1);

    // Output while ATTACHED.
    await emit("pty://output", {
      id: ptyId,
      bytes: Array.from(new TextEncoder().encode("BEFORE_NAV_7f3a\r\n")),
    });
    await waitFor(() => instance.readScreenText().includes("BEFORE_NAV_7f3a"));

    // DETACH — as if the user navigated away from the Terminals page.
    detachTerminal("a");
    expect(instance.el.parentElement).toBeNull();

    // Output arrives WHILE DETACHED — the subscription lives in the instance, not
    // the (unmounted) React pane, so it must still reach the buffer.
    await emit("pty://output", {
      id: ptyId,
      bytes: Array.from(new TextEncoder().encode("WHILE_DETACHED_zz9\r\n")),
    });
    await waitFor(() => instance.readScreenText().includes("WHILE_DETACHED_zz9"));
    // Never closed the PTY across the navigation.
    expect(closes).toHaveLength(0);

    // REATTACH (return to the page): the SAME instance, buffer intact — both the
    // pre-nav AND the while-detached output are present.
    const c2 = document.createElement("div");
    document.body.appendChild(c2);
    attachTerminal("a", c2);
    expect(getTerminal("a")).toBe(instance);
    const text = instance.readScreenText();
    expect(text).toContain("BEFORE_NAV_7f3a");
    expect(text).toContain("WHILE_DETACHED_zz9");
  });

  it("survives repeated detach/reattach cycles losslessly (buffer intact, never respawned)", async () => {
    const instance = ensureTerminal({ recordId: "a" });
    await waitForSpawn("a");
    const ptyId = instance.pty as number;

    const c1 = document.createElement("div");
    document.body.appendChild(c1);
    attachTerminal("a", c1);
    await emit("pty://output", {
      id: ptyId,
      bytes: Array.from(new TextEncoder().encode("SESSION_CONTENT_v1m\r\n")),
    });
    await waitFor(() => instance.readScreenText().includes("SESSION_CONTENT_v1m"));

    // 10 detach→reattach cycles (navigation churn).
    for (let i = 0; i < 10; i++) {
      detachTerminal("a");
      const c = document.createElement("div");
      document.body.appendChild(c);
      attachTerminal("a", c);
    }

    // Same instance + buffer survived; never closed the PTY, never respawned.
    expect(getTerminal("a")).toBe(instance);
    expect(instance.readScreenText()).toContain("SESSION_CONTENT_v1m");
    expect(closes).toHaveLength(0);
    expect(spawnedIds).toHaveLength(1);
  });

  it("disposeTerminal is the ONLY path that closes the PTY + drops the instance", async () => {
    ensureTerminal({ recordId: "a" });
    await waitForSpawn("a");
    expect(closes).toHaveLength(0);

    disposeTerminal("a");
    expect(getTerminal("a")).toBeUndefined();
    expect(terminalStore.state.ids).not.toContain("a");
    expect(closes).toHaveLength(1);
  });

  it("reconcileTerminals disposes instances whose record is no longer live", async () => {
    ensureTerminal({ recordId: "a" });
    ensureTerminal({ recordId: "b" });
    ensureTerminal({ recordId: "c" });
    await waitForSpawn("a");
    await waitForSpawn("b");
    await waitForSpawn("c");

    // Only a + c remain live (b was closed elsewhere / by an agent over MCP).
    reconcileTerminals(["a", "c"]);
    expect(getTerminal("a")).toBeDefined();
    expect(getTerminal("b")).toBeUndefined();
    expect(getTerminal("c")).toBeDefined();
    // b's PTY was closed exactly once.
    expect(closes).toHaveLength(1);
  });

  it("reconcileTerminals against the GLOBAL set keeps a terminal absent from the current deck", async () => {
    // Two terminals live in DIFFERENT projects: `pA` (project A, the deck on screen) and `pB`
    // (project B, whose pane is not mounted right now). Both records are still ALIVE, so the
    // GLOBAL live set is [pA, pB].
    ensureTerminal({ recordId: "pA" });
    ensureTerminal({ recordId: "pB" });
    await waitForSpawn("pA");
    await waitForSpawn("pB");

    // Reconciling against the GLOBAL set (both projects) disposes NEITHER — the deck currently
    // showing only pA (project A) must NOT cause pB (project B) to be closed. This is the fix: the
    // reconciliation runs against `list_terminals` (all projects), never the per-project deck.
    reconcileTerminals(["pA", "pB"]);
    expect(getTerminal("pA")).toBeDefined();
    expect(getTerminal("pB")).toBeDefined();
    expect(closes).toHaveLength(0);

    // Only once pB is REALLY closed (dropped from the global set) is it disposed.
    reconcileTerminals(["pA"]);
    expect(getTerminal("pA")).toBeDefined();
    expect(getTerminal("pB")).toBeUndefined();
    expect(closes).toHaveLength(1);
  });
});

describe("chooseSpawnDimensions (spawn-size gate)", () => {
  it("uses the fit when the element has a real box", () => {
    const el = { clientWidth: 640, clientHeight: 360 };
    const dims = chooseSpawnDimensions(el, () => ({ cols: 200, rows: 50 }), { cols: 80, rows: 24 });
    expect(dims).toEqual({ cols: 200, rows: 50 });
  });

  it("ignores the degenerate fit and falls back when the pane is hidden (0×0)", () => {
    const el = { clientWidth: 0, clientHeight: 0 };
    const dims = chooseSpawnDimensions(el, () => ({ cols: 8, rows: 24 }), { cols: 80, rows: 24 });
    expect(dims).toEqual({ cols: 80, rows: 24 });
  });

  it("falls back when the element is absent (not yet opened)", () => {
    const dims = chooseSpawnDimensions(null, () => ({ cols: 8, rows: 24 }), { cols: 80, rows: 24 });
    expect(dims).toEqual({ cols: 80, rows: 24 });
  });

  it("falls back per-axis when a visible fit returns undefined", () => {
    const el = { clientWidth: 100, clientHeight: 100 };
    const dims = chooseSpawnDimensions(el, () => undefined, { cols: 80, rows: 24 });
    expect(dims).toEqual({ cols: 80, rows: 24 });
  });
});

/**
 * THE OTHER HALF OF THE SPAWN-SIZE GATE — the wait, with its clock injected.
 *
 * `chooseSpawnDimensions` above judges ONE instant and is right to be pessimistic. A pane BORN BY
 * A SPLIT, however, is mounted in the same commit that creates its rectangle: read at that instant
 * its box is 0×0, and the shell would start at 80×24 inside a pane that is nothing like 80×24.
 * `awaitMeasuredBox` gives that pane its few MACROTASKS — and gives a genuinely hidden one nothing
 * but a bounded, cheap refusal.
 *
 * The tick clock is injected, so what is pinned here is the POLICY (immediate / within budget /
 * exhausted), never a real layout engine. The OUTCOME in a real browser — one grid per pane, never
 * 80×24 — is pinned in `../terminals/pane-tree-host.browser.test.tsx`.
 */
describe("awaitMeasuredBox (a split pane never spawns at the 80×24 fallback)", () => {
  /** A TICK clock (macrotasks, not frames) that runs the queued callbacks on demand, counting
   *  how many it served. */
  function ticks() {
    const queue: Array<() => void> = [];
    let served = 0;
    return {
      get served() {
        return served;
      },
      scheduleTick: (cb: () => void) => {
        queue.push(cb);
      },
      /** Run every callback queued so far (a callback may queue the next tick). */
      async flush(times = 1) {
        for (let i = 0; i < times; i++) {
          const due = queue.splice(0, queue.length);
          for (const cb of due) {
            served += 1;
            cb();
          }
          await Promise.resolve();
        }
      },
    };
  }

  it("resolves at once — WITHOUT burning a tick — when the box is already measurable", async () => {
    const clock = ticks();
    const el = { clientWidth: 640, clientHeight: 360 };
    await expect(
      awaitMeasuredBox(el, { ticks: 5, scheduleTick: clock.scheduleTick }),
    ).resolves.toBe(true);
    expect(clock.served, "a laid-out pane must not delay its own spawn").toBe(0);
  });

  it("WAITS for the pane the split just created, and reports the box once it lands", async () => {
    const clock = ticks();
    // The split's pane: mounted with its rectangle, but not laid out yet.
    const el = { clientWidth: 0, clientHeight: 0 };
    const measured = awaitMeasuredBox(el, { ticks: 5, scheduleTick: clock.scheduleTick });
    await clock.flush();
    // Still nothing on the first tick — the wait must not have given up.
    el.clientWidth = 600;
    el.clientHeight = 350;
    await clock.flush(2);
    await expect(measured).resolves.toBe(true);
  });

  it("gives up after its BUDGET on a pane that never gets a box (display:none)", async () => {
    const clock = ticks();
    const el = { clientWidth: 0, clientHeight: 0 };
    const measured = awaitMeasuredBox(el, { ticks: 3, scheduleTick: clock.scheduleTick });
    await clock.flush(6);
    await expect(measured).resolves.toBe(false);
    // Bounded: a hidden pane costs a couple of ticks, never a stall.
    expect(clock.served).toBeLessThanOrEqual(4);
  });

  it("gives up on an absent element instead of waiting forever", async () => {
    const clock = ticks();
    const measured = awaitMeasuredBox(null, { ticks: 2, scheduleTick: clock.scheduleTick });
    await clock.flush(4);
    await expect(measured).resolves.toBe(false);
  });

  it("and the verdict it enables is the real one: the measured box wins over the fallback", async () => {
    const clock = ticks();
    const el = { clientWidth: 0, clientHeight: 0 };
    const measured = awaitMeasuredBox(el, { ticks: 5, scheduleTick: clock.scheduleTick });
    el.clientWidth = 600;
    el.clientHeight = 350;
    await clock.flush(2);
    expect(await measured).toBe(true);
    // What the spawn does with it — the split pane's own grid, not 80×24.
    expect(
      chooseSpawnDimensions(el, () => ({ cols: 66, rows: 20 }), { cols: 80, rows: 24 }),
    ).toEqual({ cols: 66, rows: 20 });
  });
});
