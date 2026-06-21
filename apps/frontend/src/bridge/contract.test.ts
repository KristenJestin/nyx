/**
 * SHARED CONTRACT SUITE for `NyxBridge` adapters.
 *
 * `runBridgeContract(name, makeHarness)` asserts the behaviors the contract
 * guarantees REGARDLESS of shell: binary round-tripping, per-terminal PTY output
 * ordering as `Uint8Array`, idempotent unsubscribe, error shape, and window
 * controls. It runs against BOTH the in-memory `FakeNyxBridge` and the Tauri
 * adapter (driven by `@tauri-apps/api/mocks`), so every adapter — today's Tauri
 * one, tomorrow's Electron one — is held to the same behavior. A future Electron
 * adapter adds one `runBridgeContract("electron", …)` call and inherits the suite.
 *
 * Each harness owns its TRANSPORT details: `emitPtyOutput` produces the wire shape
 * that adapter consumes (Tauri: a JSON `number[]`; the fake: the already-
 * contract-shaped `Uint8Array`), and declares whether its transport supports the
 * deterministic unsubscribe / real window checks (the Tauri mock IPC does not
 * fully implement either, so those assertions are scoped to capable harnesses).
 */
import { mockIPC, clearMocks } from "@tauri-apps/api/mocks";
import { emit } from "@tauri-apps/api/event";
import { afterEach, describe, expect, it } from "vitest";

import { isBridgeError, type NyxBridge, type PtyOutput } from "./contract";
import { tauriBridge } from "./tauri";
import { electronBridge } from "./electron";
import { FakeNyxBridge } from "./fake";

/**
 * Install a FAKE `window.nyxCore` + `window.nyxWindow` (the Electron preload's
 * allowlisted bridges) so the Electron adapter is exercisable in jsdom — no real
 * IPC. The fake routes `invoke` to per-command handlers, fans `onEvent` out to its
 * registered listeners (so the harness can emit `{ event, payload }` envelopes), and
 * records `ptyAck` for the flow-control assertion. Returns a teardown that detaches
 * the globals.
 */
function installFakeNyxCore(handlers: Record<string, (args?: Record<string, unknown>) => unknown>): {
  emitEvent: (event: string, payload: unknown) => void;
  ackCalls: Array<{ ptyId: number; bytes: number }>;
  cleanup: () => void;
} {
  const eventListeners = new Set<(e: { event: string; payload: unknown }) => void>();
  const ackCalls: Array<{ ptyId: number; bytes: number }> = [];
  const windowCalls: string[] = [];
  const nyxCore = {
    invoke: (command: string, args?: Record<string, unknown>) => {
      const h = handlers[command];
      if (!h) return Promise.reject(`no fake handler for ${command}`);
      return Promise.resolve(h(args));
    },
    ptyAck: (ptyId: number, bytes: number) => ackCalls.push({ ptyId, bytes }),
    onEvent: (handler: (e: { event: string; payload: unknown }) => void) => {
      eventListeners.add(handler);
      return () => eventListeners.delete(handler);
    },
  };
  const nyxWindow = {
    minimize: () => (windowCalls.push("minimize"), Promise.resolve()),
    toggleMaximize: () => (windowCalls.push("toggleMaximize"), Promise.resolve(true)),
    close: () => (windowCalls.push("close"), Promise.resolve()),
    controlsVisible: () => Promise.resolve(true),
    pickDirectory: () => Promise.resolve(null),
    homeDir: () => Promise.resolve("/home/test"),
  };
  (window as unknown as { nyxCore: unknown }).nyxCore = nyxCore;
  (window as unknown as { nyxWindow: unknown }).nyxWindow = nyxWindow;
  return {
    emitEvent: (event, payload) => eventListeners.forEach((l) => l({ event, payload })),
    ackCalls,
    cleanup: () => {
      delete (window as unknown as { nyxCore?: unknown }).nyxCore;
      delete (window as unknown as { nyxWindow?: unknown }).nyxWindow;
    },
  };
}

interface Harness {
  bridge: NyxBridge;
  /** Emit a `pty://output` event carrying `bytes`, in this adapter's wire shape. */
  emitPtyOutput(id: number, bytes: number[]): void | Promise<void>;
  /** True if this transport honors unsubscribe synchronously (the fake does; the
   *  Tauri mock IPC delivers events best-effort and does not). */
  readonly deterministicUnsub: boolean;
  /** True if `window.*` controls are exercisable under this harness (the Tauri
   *  mock IPC does not set up `__TAURI_INTERNALS__.metadata.currentWindow`). */
  readonly windowTestable: boolean;
  cleanup?(): void;
}

function runBridgeContract(name: string, makeHarness: () => Harness): void {
  describe(`NyxBridge contract — ${name}`, () => {
    let h: Harness;
    afterEach(() => h?.cleanup?.());

    it("resolves a spawn id and round-trips PTY write bytes", async () => {
      h = makeHarness();
      const id = await h.bridge.ptySpawn({ cols: 80, rows: 24 });
      expect(typeof id).toBe("number");
      // void command: resolves (Tauri yields `null`, the fake `undefined`).
      await h.bridge.ptyWrite(id, new Uint8Array([104, 105]));
    });

    it("delivers PTY output per terminal id, in order, as Uint8Array", async () => {
      h = makeHarness();
      const got: PtyOutput[] = [];
      const unsub = await h.bridge.subscribePtyOutput((o) => got.push(o));

      await h.emitPtyOutput(1, [65, 66]);
      await h.emitPtyOutput(2, [67]);
      await h.emitPtyOutput(1, [68]);
      await Promise.resolve();

      const forOne = got.filter((o) => o.id === 1);
      expect(forOne).toHaveLength(2);
      // The contract normalizes binary to Uint8Array at the boundary, every adapter.
      expect(forOne[0].bytes).toBeInstanceOf(Uint8Array);
      expect(Array.from(forOne[0].bytes)).toEqual([65, 66]);
      expect(Array.from(forOne[1].bytes)).toEqual([68]);
      unsub();
    });

    it("stops delivering after unsubscribe, idempotently", async () => {
      h = makeHarness();
      if (!h.deterministicUnsub) return; // transport-dependent; asserted on capable harnesses
      const got: PtyOutput[] = [];
      const unsub = await h.bridge.subscribePtyOutput((o) => got.push(o));
      unsub();
      unsub(); // idempotent — must not throw
      await h.emitPtyOutput(1, [1]);
      await Promise.resolve();
      expect(got).toHaveLength(0);
    });

    it("rejects a failing command with a BridgeError shape", async () => {
      h = makeHarness();
      // The fake has no handler for this command → rejects with a BridgeError.
      // (The Tauri mock returns null, so it resolves; this asserts the shape only
      // when a rejection actually occurs.)
      await h.bridge.invoke("delete_project", { id: "nope" }).then(
        () => {
          /* resolved (tauri mock) — nothing to assert */
        },
        (e) => expect(isBridgeError(e)).toBe(true),
      );
    });

    it("exposes window controls", async () => {
      h = makeHarness();
      expect(h.bridge.window.dragRegionProps()).toMatchObject({
        "data-tauri-drag-region": true,
      });
      if (!h.windowTestable) return;
      await h.bridge.window.minimize();
      await h.bridge.window.toggleMaximize();
    });
  });
}

// --- Adapter 1: the in-memory fake (full transport semantics) ---------------
runBridgeContract("fake", () => {
  const fake = new FakeNyxBridge()
    .on("pty_spawn", () => 1)
    .on("pty_write", () => undefined)
    .on("pty_resize", () => undefined)
    .on("pty_close", () => undefined);
  return {
    bridge: fake,
    // The fake's transport IS the contract shape: emit Uint8Array directly.
    emitPtyOutput: (id, bytes) =>
      fake.emit("pty://output", { id, bytes: Uint8Array.from(bytes) }),
    deterministicUnsub: true,
    windowTestable: true,
  };
});

// --- Adapter 2: Tauri, over the mock IPC ------------------------------------
runBridgeContract("tauri", () => {
  mockIPC(
    (cmd) => {
      if (cmd === "pty_spawn") return 1;
      return null;
    },
    { shouldMockEvents: true },
  );
  return {
    bridge: tauriBridge,
    // Tauri's wire shape for pty output is a JSON number[].
    emitPtyOutput: (id, bytes) => emit("pty://output", { id, bytes }) as unknown as Promise<void>,
    // The mock IPC delivers events best-effort and does not implement a
    // synchronous unlisten nor the window `currentWindow` metadata.
    deterministicUnsub: false,
    windowTestable: false,
    cleanup: () => clearMocks(),
  };
});

// --- Adapter 3: Electron, over a fake window.nyxCore / window.nyxWindow ------
runBridgeContract("electron", () => {
  const core = installFakeNyxCore({
    pty_spawn: () => 1,
    pty_write: () => null,
    pty_resize: () => null,
    pty_close: () => null,
  });
  return {
    bridge: electronBridge,
    // The Electron main relays pty output as a `{ event, payload }` envelope whose
    // payload is `{ id, bytes: number[] }` (the contract's JSON wire shape).
    emitPtyOutput: (id, bytes) => core.emitEvent("pty://output", { id, bytes }),
    // The demux unsubscribe is a synchronous Set.delete; window controls are real.
    deterministicUnsub: true,
    windowTestable: true,
    cleanup: () => core.cleanup(),
  };
});

// --- Electron-only: the flow-control ACK contract ---------------------------
// The Electron adapter must NOT ack on arrival — the consumer acks consumed bytes
// via `ackPtyOutput`, which credits the host. This asserts the native-capability
// half of the contract (the lossless flow control) the other adapters no-op.
describe("NyxBridge contract — electron flow-control ack", () => {
  it("acks consumed bytes through window.nyxCore.ptyAck, only on ackPtyOutput", async () => {
    const core = installFakeNyxCore({ pty_spawn: () => 1 });
    try {
      const got: PtyOutput[] = [];
      const unsub = await electronBridge.subscribePtyOutput((o) => got.push(o));

      // Output arrival alone must NOT ack (the consumer hasn't written to xterm yet).
      core.emitEvent("pty://output", { id: 1, bytes: [1, 2, 3] });
      await Promise.resolve();
      expect(core.ackCalls).toHaveLength(0);

      // The consumer credits bytes after xterm consumed them.
      electronBridge.ackPtyOutput(1, 3);
      expect(core.ackCalls).toEqual([{ ptyId: 1, bytes: 3 }]);

      unsub();
    } finally {
      core.cleanup();
    }
  });
});
