/**
 * `nyxBridge.tauri` — the Tauri implementation of the {@link NyxBridge} contract.
 *
 * This is the ONLY production module allowed to import `@tauri-apps/*`. Every other
 * component depends on the contract and receives a bridge instance (today this
 * Tauri one). Swapping in the Electron adapter (phase 3) changes only which module
 * `index.ts` selects — no component changes.
 *
 * Responsibilities the contract assigns to the adapter:
 *  - map Tauri `invoke`/`listen` failures into the serialized {@link BridgeError};
 *  - normalize binary payloads (`number[]` ↔ `Uint8Array`) at the boundary;
 *  - wrap Tauri's `UnlistenFn` in an idempotent {@link Unsubscribe};
 *  - apply request timeouts / abort signals.
 */
import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import { listen as tauriListen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

import type {
  AppPathsBridge,
  BackendCommand,
  BackendEvent,
  BridgeError,
  CommandAck,
  CommandOutput,
  CommandState,
  Listener,
  NyxBridge,
  PtyExit,
  PtyOutput,
  PtySpawnOptions,
  RequestOptions,
  TerminalBusyState,
  TerminalExecState,
  Unsubscribe,
  WindowControls,
} from "./contract";

/** Build a {@link BridgeError} from a caught Tauri/IPC failure. */
function toBridgeError(kind: BridgeError["kind"], cause: unknown, source?: string): BridgeError {
  const message =
    cause instanceof Error
      ? cause.message
      : typeof cause === "string"
        ? cause
        : "bridge call failed";
  return { kind, message, source, cause };
}

/**
 * Run a promise under the request options: a timeout (reject `timeout`) and/or an
 * abort signal (reject `canceled`). With neither, returns the promise unchanged.
 */
function withRequestOptions<R>(p: Promise<R>, source: string, opts?: RequestOptions): Promise<R> {
  if (!opts || (!opts.timeoutMs && !opts.signal)) return p;
  return new Promise<R>((resolve, reject) => {
    let settled = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const done = (fn: () => void) => {
      if (settled) return;
      settled = true;
      if (timer) clearTimeout(timer);
      if (opts.signal) opts.signal.removeEventListener("abort", onAbort);
      fn();
    };
    const onAbort = () => done(() => reject(toBridgeError("canceled", "aborted", source)));
    if (opts.signal) {
      if (opts.signal.aborted) return onAbort();
      opts.signal.addEventListener("abort", onAbort);
    }
    if (opts.timeoutMs && opts.timeoutMs > 0) {
      timer = setTimeout(
        () => done(() => reject(toBridgeError("timeout", `>${opts.timeoutMs}ms`, source))),
        opts.timeoutMs,
      );
    }
    p.then(
      (v) => done(() => resolve(v)),
      (e) => done(() => reject(toBridgeError("command", e, source))),
    );
  });
}

/** Invoke a backend command, mapping failures to {@link BridgeError}. */
function invoke<R>(
  command: BackendCommand,
  args?: Record<string, unknown>,
  opts?: RequestOptions,
): Promise<R> {
  const call = tauriInvoke<R>(command, args).catch((e) => {
    throw toBridgeError("command", e, command);
  });
  return withRequestOptions(call, command, opts);
}

/**
 * Subscribe to a backend event, mapping each Tauri event to the contract payload
 * via `map`, and returning an idempotent {@link Unsubscribe}. Because `listen` is
 * async, an unsubscribe issued before it resolves is honored once it does (we latch
 * an `unlistened` flag and call the resolved `UnlistenFn` immediately).
 */
function subscribe<TWire, T>(
  event: BackendEvent,
  map: (wire: TWire) => T,
  listener: Listener<T>,
): Promise<Unsubscribe> {
  let unlistened = false;
  let unlistenFn: (() => void) | undefined;
  const ready = tauriListen<TWire>(event, (e) => {
    listener(map(e.payload));
  })
    .then((fn) => {
      unlistenFn = fn;
      if (unlistened) fn(); // unsubscribed before listen resolved
    })
    // Swallow a teardown race: under a test that clears the mock IPC, the pending
    // `listen` round-trip can reject (`transformCallback` undefined) AFTER the test
    // — an expected, harmless straggler, not a real failure.
    .catch(() => {});
  // The returned Unsubscribe is sync + idempotent.
  void ready;
  return Promise.resolve(() => {
    if (unlistened) return;
    unlistened = true;
    try {
      unlistenFn?.();
    } catch {
      // ignore teardown race
    }
  });
}

/** `number[]` (Tauri JSON wire) → `Uint8Array`. */
function bytesIn(raw: number[] | Uint8Array): Uint8Array {
  return raw instanceof Uint8Array ? raw : Uint8Array.from(raw);
}
/** `Uint8Array` → `number[]` for the Tauri JSON wire. */
function bytesOut(data: Uint8Array): number[] {
  return Array.from(data);
}

const windowControls: WindowControls = {
  minimize: () =>
    getCurrentWindow()
      .minimize()
      .catch((e) => {
        throw toBridgeError("ipc", e, "window.minimize");
      }),
  toggleMaximize: () =>
    getCurrentWindow()
      .toggleMaximize()
      .catch((e) => {
        throw toBridgeError("ipc", e, "window.toggleMaximize");
      }),
  close: () =>
    getCurrentWindow()
      .close()
      .catch((e) => {
        throw toBridgeError("ipc", e, "window.close");
      }),
  // Tauri moves the window by any element carrying this attribute.
  dragRegionProps: () => ({ "data-tauri-drag-region": true }),
  async onCloseRequested(handler: () => void): Promise<Unsubscribe> {
    let unlistened = false;
    let un: (() => void) | undefined;
    try {
      un = await getCurrentWindow().onCloseRequested(() => handler());
      if (unlistened) un();
    } catch {
      // Not running under a Tauri window (tests) — nothing to hook.
    }
    return () => {
      if (unlistened) return;
      unlistened = true;
      try {
        un?.();
      } catch {
        // ignore teardown race
      }
    };
  },
};

const paths: AppPathsBridge = {
  async homeDir() {
    try {
      const { homeDir } = await import("@tauri-apps/api/path");
      return (await homeDir()) || null;
    } catch {
      return null;
    }
  },
};

/** The concrete Tauri bridge. */
export const tauriBridge: NyxBridge = {
  invoke,

  subscribe<T>(event: BackendEvent, listener: Listener<T>): Promise<Unsubscribe> {
    // Raw passthrough: deliver the backend payload unchanged (mirrors a bare
    // `listen(event, e => listener(e.payload))`).
    return subscribe<T, T>(event, (w) => w, listener);
  },

  ptySpawn(opts: PtySpawnOptions): Promise<number> {
    return invoke<number>("pty_spawn", {
      cwd: opts.cwd,
      cols: opts.cols,
      rows: opts.rows,
      terminalId: opts.terminalId,
    });
  },
  ptyWrite(id, data) {
    return invoke<void>("pty_write", { id, data: bytesOut(data) });
  },
  ptyResize(id, cols, rows) {
    return invoke<void>("pty_resize", { id, cols, rows });
  },
  ptyClose(id) {
    return invoke<void>("pty_close", { id });
  },
  subscribePtyOutput(listener: Listener<PtyOutput>) {
    return subscribe<{ id: number; bytes: number[] }, PtyOutput>(
      "pty://output",
      (w) => ({ id: w.id, bytes: bytesIn(w.bytes) }),
      listener,
    );
  },
  subscribePtyExit(listener: Listener<PtyExit>) {
    return subscribe<{ id: number; code: number | null }, PtyExit>(
      "pty://exit",
      (w) => ({ id: w.id, code: w.code }),
      listener,
    );
  },
  // Tauri's PTY transport applies its own coalescing/throttling on the backend and
  // has no renderer-driven credit loop, so the flow-control ack is a no-op here. The
  // single PTY consumer calls it unconditionally; only the Electron adapter acts on it.
  ackPtyOutput(_id: number, _bytes: number): void {},

  subscribeCommandOutput(listener: Listener<CommandOutput>) {
    return subscribe<{ id: string; bytes: number[] }, CommandOutput>(
      "command://output",
      (w) => ({ id: w.id, bytes: bytesIn(w.bytes) }),
      listener,
    );
  },
  subscribeCommandState(listener: Listener<CommandState>) {
    return subscribe<{ id: string; state: string; exitCode: number | null }, CommandState>(
      "command://state",
      (w) => ({ id: w.id, state: w.state, exitCode: w.exitCode ?? null }),
      listener,
    );
  },
  subscribeCommandAck(listener: Listener<CommandAck>) {
    return subscribe<{ id: string }, CommandAck>("command://ack", (w) => ({ id: w.id }), listener);
  },

  subscribeTerminalBusyState(listener: Listener<TerminalBusyState>) {
    return subscribe<{ id: string; busy: boolean }, TerminalBusyState>(
      "terminal://busy-state",
      (w) => ({ id: w.id, busy: w.busy }),
      listener,
    );
  },
  subscribeTerminalExecState(listener: Listener<TerminalExecState>) {
    return subscribe<{ id: string; state: string; exitCode: number | null }, TerminalExecState>(
      "terminal://exec-state",
      (w) => ({ id: w.id, state: w.state, exitCode: w.exitCode ?? null }),
      listener,
    );
  },

  window: windowControls,

  async pickDirectory(title?: string): Promise<string | null> {
    const { open } = await import("@tauri-apps/plugin-dialog");
    const selected = await open({ directory: true, multiple: false, title });
    if (Array.isArray(selected)) return selected[0] ?? null;
    return selected ?? null;
  },

  paths,
};
