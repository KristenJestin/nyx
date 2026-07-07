import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { mockIPC } from "@/bridge/test-harness";
import { FakeTerminalCore } from "./fake-core";
import { setCoreFactory, type CoreFactory } from "./ghostty-core";
import {
  __resetTerminalStore,
  attachTerminal,
  destroyTerminal,
  detachTerminal,
  ensureTerminal,
  getTerminal,
  reconcileTerminals,
  terminalStore,
} from "./terminal-store";

/**
 * The NO-HOST STORE contract (jsdom): instances live in a module-level Map,
 * outside React/route lifecycle; navigation DETACHES their DOM node (never
 * destroys), and reattach is lossless. The ghostty WASM cannot load under jsdom,
 * so we inject a {@link FakeTerminalCore} via the core-factory seam.
 */

let cores: FakeTerminalCore[] = [];
let restoreFactory: CoreFactory | null = null;
let closes: number[] = [];

async function waitForInstance(id: string): Promise<void> {
  // `ensureTerminal` fires an async init; wait until the wterm host is built.
  await new Promise((r) => setTimeout(r, 0));
  await new Promise((r) => setTimeout(r, 0));
  const inst = getTerminal(id);
  if (!inst || !inst.el.querySelector(".term-grid")) {
    await new Promise((r) => setTimeout(r, 5));
  }
}

describe("terminal-store (no-host detach/reattach)", () => {
  beforeEach(() => {
    cores = [];
    closes = [];
    mockIPC((cmd, args) => {
      if (cmd === "pty_spawn") return 100 + cores.length;
      if (cmd === "pty_close") {
        closes.push((args?.id as number) ?? -1);
      }
      return null;
    });
    restoreFactory = setCoreFactory(async () => {
      const core = new FakeTerminalCore();
      cores.push(core);
      return core;
    });
  });

  afterEach(() => {
    __resetTerminalStore();
    if (restoreFactory) setCoreFactory(restoreFactory);
  });

  it("holds instances in a module-level Map and publishes ids reactively", async () => {
    ensureTerminal({ recordId: "a" });
    ensureTerminal({ recordId: "b" });
    expect(getTerminal("a")).toBeDefined();
    expect(getTerminal("b")).toBeDefined();
    expect(terminalStore.state.ids).toEqual(["a", "b"]);
    await waitForInstance("a");
  });

  it("ensureTerminal is idempotent — a second call never respawns", async () => {
    const first = ensureTerminal({ recordId: "a" });
    await waitForInstance("a");
    const spawnsAfterFirst = cores.length;
    const again = ensureTerminal({ recordId: "a" });
    expect(again).toBe(first);
    await new Promise((r) => setTimeout(r, 10));
    expect(cores.length).toBe(spawnsAfterFirst); // no second core built
  });

  it("attach moves the SAME node; detach removes it WITHOUT destroying", async () => {
    const instance = ensureTerminal({ recordId: "a" });
    await waitForInstance("a");

    const c1 = document.createElement("div");
    document.body.appendChild(c1);
    attachTerminal("a", c1);
    expect(instance.el.parentElement).toBe(c1);
    // The focusable input lives INSIDE the node, so focus survives moves.
    expect(instance.el.querySelector("textarea")).not.toBeNull();

    // DETACH (navigation): node removed, but the instance stays in the store and
    // the PTY is NOT closed.
    detachTerminal("a");
    expect(instance.el.parentElement).toBeNull();
    expect(getTerminal("a")).toBe(instance);
    expect(closes).toHaveLength(0);

    // REATTACH into a DIFFERENT container (the return route's pane): same node.
    const c2 = document.createElement("div");
    document.body.appendChild(c2);
    attachTerminal("a", c2);
    expect(instance.el.parentElement).toBe(c2);
    expect(instance.el.querySelector("textarea")).not.toBeNull();
  });

  it("survives repeated detach/reattach cycles losslessly (buffer + alt-screen intact)", async () => {
    const instance = ensureTerminal({ recordId: "a" });
    await waitForInstance("a");
    const c1 = document.createElement("div");
    document.body.appendChild(c1);
    attachTerminal("a", c1);

    // Enter alt-screen (a TUI) and write content.
    const core = cores[0];
    core.writeString("\x1b[?1049h"); // enter alt-screen
    core.writeString("VIM-SESSION-CONTENT");

    // 10 detach→reattach cycles (navigation churn).
    for (let i = 0; i < 10; i++) {
      detachTerminal("a");
      const c = document.createElement("div");
      document.body.appendChild(c);
      attachTerminal("a", c);
    }

    // The instance, its core state (alt-screen + content) and node all survived;
    // never closed the PTY, never rebuilt the core.
    expect(getTerminal("a")).toBe(instance);
    expect(instance.usingAltScreen()).toBe(true);
    expect(core.written).toContain("VIM-SESSION-CONTENT");
    expect(closes).toHaveLength(0);
    expect(cores).toHaveLength(1); // core never rebuilt across cycles
  });

  it("detach CUTS the app-driven ResizeObserver; reattach re-arms it losslessly", async () => {
    // Track live (connected) ResizeObservers so we can prove detach disconnects
    // the one `observeResize` armed — the JSDoc contract (#24). The vitest.setup
    // stub is a no-op; here we count observe/disconnect on THIS test's instances.
    let liveObservers = 0;
    class TrackingResizeObserver {
      observe(): void {
        liveObservers++;
      }
      unobserve(): void {}
      disconnect(): void {
        if (liveObservers > 0) liveObservers--;
      }
    }
    const prevRO = globalThis.ResizeObserver;
    globalThis.ResizeObserver = TrackingResizeObserver as unknown as typeof ResizeObserver;
    try {
      const instance = ensureTerminal({ recordId: "a" });
      await waitForInstance("a");

      const c1 = document.createElement("div");
      document.body.appendChild(c1);
      attachTerminal("a", c1); // arms observeResize → one live observer
      expect(liveObservers, "attach arms the app-driven observer").toBe(1);

      // Enter alt-screen + write content so we can prove state survives the cut.
      const core = cores[0];
      core.writeString("\x1b[?1049h");
      core.writeString("OBSERVER-CUT-CONTENT");

      // DETACH: the observer is CUT (no stale observer lingers on the old parent),
      // the node is removed, but the instance + its core state stay alive.
      detachTerminal("a");
      expect(liveObservers, "detach disconnects the observer (JSDoc #24)").toBe(0);
      expect(instance.el.parentElement).toBeNull();
      expect(getTerminal("a")).toBe(instance);
      expect(closes).toHaveLength(0);

      // REATTACH: the observer is RE-ARMED (exactly one live, not doubled) and the
      // state is lossless (same instance, alt-screen + content intact).
      const c2 = document.createElement("div");
      document.body.appendChild(c2);
      attachTerminal("a", c2);
      expect(liveObservers, "reattach re-arms exactly one observer").toBe(1);
      expect(getTerminal("a")).toBe(instance);
      expect(instance.usingAltScreen()).toBe(true);
      expect(core.written).toContain("OBSERVER-CUT-CONTENT");
    } finally {
      globalThis.ResizeObserver = prevRO;
    }
  });

  it("destroyTerminal is the ONLY path that closes the PTY + drops the instance", async () => {
    ensureTerminal({ recordId: "a" });
    await waitForInstance("a");
    expect(closes).toHaveLength(0);

    destroyTerminal("a");
    expect(getTerminal("a")).toBeUndefined();
    expect(terminalStore.state.ids).not.toContain("a");
    expect(closes).toHaveLength(1);
  });

  it("reconcileTerminals disposes instances whose record is no longer live", async () => {
    ensureTerminal({ recordId: "a" });
    ensureTerminal({ recordId: "b" });
    ensureTerminal({ recordId: "c" });
    await waitForInstance("a");

    // Only a + c remain live (b was closed elsewhere / by an agent).
    reconcileTerminals(["a", "c"]);
    expect(getTerminal("a")).toBeDefined();
    expect(getTerminal("b")).toBeUndefined();
    expect(getTerminal("c")).toBeDefined();
    // b's PTY was closed exactly once.
    expect(closes).toHaveLength(1);
  });
});
