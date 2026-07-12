import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { emit, mockIPC } from "@/bridge/test-harness";
import { chooseSpawnDimensions } from "./xterm-instance";
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
