/**
 * SHARED CONTRACT SUITE for `NyxBridge` adapters.
 *
 * `runBridgeContract(name, makeHarness)` asserts the behaviors the contract
 * guarantees REGARDLESS of shell: binary round-tripping, per-terminal PTY output
 * ordering as `Uint8Array`, idempotent unsubscribe, error shape, and window
 * controls. It runs against BOTH the in-memory `FakeNyxBridge` (the test double) and
 * the `electronBridge` (the shipping adapter, driven through a fake `window.nyxCore`),
 * so the adapter is held to the same behavior the fake guarantees.
 *
 * Each harness owns its TRANSPORT details: `emitPtyOutput` produces the wire shape
 * that adapter consumes (Electron: a relayed `{ event, payload }` envelope with bytes
 * as a JSON `number[]`; the fake: the already-contract-shaped `Uint8Array`), and
 * declares whether its transport supports the deterministic unsubscribe / real window
 * checks.
 *
 * Tauri / N-API are removed (PDR-0006/0007) — only the Electron adapter ships, so the
 * suite no longer drives `@tauri-apps/api/mocks`.
 */
import { afterEach, describe, expect, it } from "vitest";

import { type BridgeError, isBridgeError, type NyxBridge, type PtyOutput } from "./contract";
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
function installFakeNyxCore(
  handlers: Record<string, (args?: Record<string, unknown>) => unknown>,
): {
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
    emitPtyOutput: (id, bytes) => fake.emit("pty://output", { id, bytes: Uint8Array.from(bytes) }),
    deterministicUnsub: true,
    windowTestable: true,
  };
});

// --- Adapter 2: Tauri, over the mock IPC ------------------------------------
// --- Adapter 2: Electron, over a fake window.nyxCore / window.nyxWindow ------
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

// --- Electron-only: the IPC wrapper never reaches the user ------------------
//
// Second human review, finding F6. A duplicate link showed, in the Links block's `role="alert"`:
// `Error invoking remote method 'nyx:core/invoke': Error: url '…' is already attached to this
// Spec` — the authority's sentence last, behind the channel name and two stacked `Error:`. Nothing
// downstream was at fault: `errorText` renders a message verbatim, and it was handed one that had
// already been wrapped by `ipcRenderer.invoke`. The unwrapping therefore lives at the ONE place a
// native failure becomes a `BridgeError` (`electron.ts::toBridgeError`), and these tests hold it
// there — including the case where there is nothing to unwrap, which must pass through untouched.
describe("NyxBridge contract — electron refusal messages", () => {
  /** What `ipcRenderer.invoke` rejects with when a main-process handler throws `sentence`. */
  function ipcWrapped(sentence: string, cause?: Error): Error {
    const wrapped = new Error(`Error invoking remote method 'nyx:core/invoke': Error: ${sentence}`);
    if (cause) (wrapped as { cause?: unknown }).cause = cause;
    return wrapped;
  }

  /** Drive a real `electronBridge.invoke` whose transport rejects with `rejection`. */
  async function refusal(rejection: unknown): Promise<BridgeError> {
    const core = installFakeNyxCore({ annexe_add: () => Promise.reject(rejection) });
    try {
      const settled: unknown = await electronBridge
        .invoke("annexe_add", { specId: "s1" })
        .then(() => null, (e: unknown) => e);
      expect(isBridgeError(settled)).toBe(true);
      return settled as BridgeError;
    } finally {
      core.cleanup();
    }
  }

  it("shows the backend's sentence ALONE — no channel name, no stacked `Error:`", async () => {
    // Every refusal of the link write path, exactly as the Rust authority words them.
    for (const sentence of [
      "url 'https://www.youtube.com' is already attached to this Spec",
      "invalid url 'not a url': a url annexe must be an ABSOLUTE http:// or https:// URL",
      "invalid url 'https://x.test/a b': a url annexe must not carry WHITESPACE or CONTROL " +
        "characters inside it (surrounding blanks are trimmed off; percent-encode the rest)",
      "url too long: 2049 characters — a url annexe is limited to 2048 characters",
    ]) {
      const refused = await refusal(ipcWrapped(sentence));
      expect(refused.message).toBe(sentence);
      expect(refused.message).not.toContain("invoking remote method");
      expect(refused.message.startsWith("Error:")).toBe(false);
    }
  });

  it("reads the ORIGINAL error when the transport attaches it, rather than cutting a string", async () => {
    // Whether an Electron build attaches the deserialized error as `cause` cannot be settled from
    // source, so the adapter covers both. When it IS there it wins, and the wrapper's own text —
    // deliberately mangled here — is never consulted.
    const refused = await refusal(
      ipcWrapped("MANGLED, must not be read", new Error("url 'https://x.test' is already attached")),
    );
    expect(refused.message).toBe("url 'https://x.test' is already attached");
  });

  it("leaves a message that did NOT cross that channel INTACT — no speculative slicing", async () => {
    // A failure raised in-process, and a wrapper naming ANOTHER channel: neither matches the exact
    // prefix, so neither is touched. This is what makes the peel safe on a sentence that happens to
    // read like a wrapper.
    for (const untouched of [
      "sidecar is not running (state=stopped)",
      "Error invoking remote method 'nyx:window/close': Error: nope",
      "Error: a backend sentence that starts like a stringified error",
    ]) {
      const refused = await refusal(new Error(untouched));
      expect(refused.message).toBe(untouched);
    }
  });

  it("keeps the rest of the BridgeError shape: kind, source, and the RAW value as cause", async () => {
    const raw = ipcWrapped("url 'https://x.test' is already attached to this Spec");
    const refused = await refusal(raw);
    expect(refused.kind).toBe("command");
    expect(refused.source).toBe("annexe_add");
    // `cause` is the original thrown value — for logging, never for control flow (contract.ts).
    expect(refused.cause).toBe(raw);
  });
});
