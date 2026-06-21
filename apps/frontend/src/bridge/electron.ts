/**
 * `nyxBridge.electron` — the Electron implementation of the {@link NyxBridge}
 * contract (phase 3, task #10).
 *
 * It speaks ONLY the allowlisted `window.nyxCore` + `window.nyxWindow` objects the
 * Electron preload installs (see `apps/electron/src/preload`, `apps/electron/src/shared/ipc.ts`).
 * The renderer runs with `contextIsolation: true` + `nodeIntegration: false` +
 * `sandbox: true`, so it has NO `require`, NO `ipcRenderer`, NO Node — these two
 * deep-frozen bridge objects are the entire surface. This module therefore imports
 * NO `@tauri-apps/*` (that import is confined to `./tauri.ts`); it is symmetric to
 * the Tauri adapter but over the Electron IPC.
 *
 * Responsibilities the contract assigns to the adapter:
 *  - map an `invoke` failure into the serialized {@link BridgeError};
 *  - DEMUX the single relayed host-event channel (`window.nyxCore.onEvent`) by the
 *    event name in the `{ event, payload }` envelope, into the typed `subscribe*`
 *    streams, and normalize binary payloads (`number[]` ↔ `Uint8Array`);
 *  - drive the LOSSLESS FLOW-CONTROL ack: after a `pty://output` chunk is handed to
 *    its listener (the renderer's xterm.write path), credit the bytes back to the
 *    host via `window.nyxCore.ptyAck` so the per-terminal backlog drains and the
 *    Rust reader resumes (PRD / annexe §E);
 *  - wrap the window controls + apply request timeouts / abort signals.
 */
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

/**
 * The shape of the relayed host-event envelope main pushes on the single core-event
 * channel (mirror of `apps/electron/src/shared/ipc.ts` `CoreEventEnvelope`). Declared
 * locally so the frontend does not import from the electron app package.
 */
interface CoreEventEnvelope {
  event: string;
  payload: unknown;
}

/** The `window.nyxCore` bridge the preload installs (mirror of `NyxCoreApi`). */
interface NyxCoreApi {
  invoke(command: string, args?: Record<string, unknown>): Promise<unknown>;
  ptyAck(ptyId: number, bytes: number): void;
  onEvent(handler: (envelope: CoreEventEnvelope) => void): () => void;
}

/** The `window.nyxWindow` bridge the preload installs (mirror of `NyxWindowApi`). */
interface NyxWindowApi {
  minimize(): Promise<void>;
  toggleMaximize(): Promise<boolean>;
  close(): Promise<void>;
  controlsVisible(): Promise<boolean>;
  pickDirectory(title?: string): Promise<string | null>;
  homeDir(): Promise<string | null>;
}

/** Read the injected bridge objects. They exist whenever this adapter is selected
 *  (the env detector only picks Electron when `window.nyxCore` is present). */
function coreApi(): NyxCoreApi {
  return (window as unknown as { nyxCore: NyxCoreApi }).nyxCore;
}
function windowApi(): NyxWindowApi {
  return (window as unknown as { nyxWindow: NyxWindowApi }).nyxWindow;
}

/** Build a {@link BridgeError} from a caught IPC/command failure. */
function toBridgeError(
  kind: BridgeError["kind"],
  cause: unknown,
  source?: string,
): BridgeError {
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
 * Identical semantics to the Tauri adapter's `withRequestOptions`.
 */
function withRequestOptions<R>(
  p: Promise<R>,
  source: string,
  opts?: RequestOptions,
): Promise<R> {
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
    const onAbort = () =>
      done(() => reject(toBridgeError("canceled", "aborted", source)));
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

/** Invoke a backend command over `nyxCore.invoke`, mapping failures to {@link BridgeError}. */
function invoke<R>(
  command: BackendCommand,
  args?: Record<string, unknown>,
  opts?: RequestOptions,
): Promise<R> {
  const call = coreApi()
    .invoke(command, args)
    .catch((e) => {
      throw toBridgeError("command", e, command);
    }) as Promise<R>;
  return withRequestOptions(call, command, opts);
}

// ---------------------------------------------------------------------------
// Event demux: one host-event channel → per-channel listener sets.
// ---------------------------------------------------------------------------

/**
 * A single shared demultiplexer over `window.nyxCore.onEvent`. The Electron main
 * relays EVERY host event on one channel as `{ event, payload }`; we register ONE
 * underlying listener and fan each envelope out to the per-channel listener sets the
 * `subscribe*` methods register here. This keeps exactly one IPC listener regardless
 * of how many terminals/components subscribe, and gives a synchronous, idempotent
 * unsubscribe (just a `Set.delete`).
 */
class EventDemux {
  private readonly byChannel = new Map<string, Set<Listener<unknown>>>();
  private detach: (() => void) | null = null;

  private ensureAttached(): void {
    if (this.detach) return;
    this.detach = coreApi().onEvent((envelope) => {
      const set = this.byChannel.get(envelope.event);
      if (!set || set.size === 0) return;
      // Copy to a snapshot so a listener that unsubscribes mid-dispatch does not
      // mutate the set we are iterating.
      for (const l of Array.from(set)) l(envelope.payload);
    });
  }

  subscribe<T>(channel: string, listener: Listener<T>): Unsubscribe {
    this.ensureAttached();
    let set = this.byChannel.get(channel);
    if (!set) {
      set = new Set();
      this.byChannel.set(channel, set);
    }
    const l = listener as Listener<unknown>;
    set.add(l);
    let done = false;
    return () => {
      if (done) return;
      done = true;
      set!.delete(l);
    };
  }
}

const demux = new EventDemux();

/** `number[]` (JSON wire) | `Uint8Array` → `Uint8Array`. */
function bytesIn(raw: number[] | Uint8Array): Uint8Array {
  return raw instanceof Uint8Array ? raw : Uint8Array.from(raw);
}

// ---------------------------------------------------------------------------
// Window controls (over window.nyxWindow).
// ---------------------------------------------------------------------------

const windowControls: WindowControls = {
  minimize: () =>
    windowApi()
      .minimize()
      .catch((e) => {
        throw toBridgeError("ipc", e, "window.minimize");
      }),
  toggleMaximize: () =>
    windowApi()
      .toggleMaximize()
      .then(() => undefined)
      .catch((e) => {
        throw toBridgeError("ipc", e, "window.toggleMaximize");
      }),
  close: () =>
    windowApi()
      .close()
      .catch((e) => {
        throw toBridgeError("ipc", e, "window.close");
      }),
  // Electron drags the window from any element carrying this attribute via a CSS
  // `-webkit-app-region: drag` rule the renderer ships (so the SAME chrome markup
  // works on both shells — the rule maps `[data-tauri-drag-region]` to a drag region
  // under Electron). Returning the same key keeps the chrome component shell-agnostic.
  dragRegionProps: () => ({ "data-tauri-drag-region": true }),
  async onCloseRequested(handler: () => void): Promise<Unsubscribe> {
    // The OS close button routes through the main process, which (phase 3+) relays a
    // close-requested signal. Until that channel exists, the renderer-side
    // scrollback flush is driven by the standard `beforeunload`/`pagehide` lifecycle;
    // we subscribe to `pagehide` so a window teardown still flushes. Idempotent.
    let done = false;
    const onHide = () => handler();
    window.addEventListener("pagehide", onHide);
    return () => {
      if (done) return;
      done = true;
      window.removeEventListener("pagehide", onHide);
    };
  },
};

// ---------------------------------------------------------------------------
// Paths.
// ---------------------------------------------------------------------------

const paths: AppPathsBridge = {
  async homeDir() {
    try {
      // Resolved in the MAIN process (the renderer has no Node) via the window bridge.
      const home = await windowApi().homeDir();
      return home || null;
    } catch {
      return null;
    }
  },
};

// ---------------------------------------------------------------------------
// The concrete Electron bridge.
// ---------------------------------------------------------------------------

export const electronBridge: NyxBridge = {
  invoke,

  subscribe<T>(event: BackendEvent, listener: Listener<T>): Promise<Unsubscribe> {
    // Raw passthrough: deliver the relayed payload unchanged for this channel.
    return Promise.resolve(demux.subscribe<T>(event, listener));
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
    // The contract carries bytes as Uint8Array; the main relay encodes to base64 for
    // the host, so we pass a plain number[] (the JSON wire shape).
    return invoke<void>("pty_write", { id, data: Array.from(data) });
  },
  ptyResize(id, cols, rows) {
    return invoke<void>("pty_resize", { id, cols, rows });
  },
  ptyClose(id) {
    return invoke<void>("pty_close", { id });
  },

  subscribePtyOutput(listener: Listener<PtyOutput>): Promise<Unsubscribe> {
    // Normalize bytes to Uint8Array and deliver. We do NOT ack here — the credit must
    // reflect what xterm actually CONSUMED, so the consumer (`use-pty`) acks from
    // `xterm.write`'s completion callback via `ackPtyOutput` below.
    const un = demux.subscribe<{ id: number; bytes: number[] | Uint8Array }>(
      "pty://output",
      (w) => listener({ id: w.id, bytes: bytesIn(w.bytes) }),
    );
    return Promise.resolve(un);
  },
  ackPtyOutput(id: number, bytes: number): void {
    // Credit the bytes the renderer (xterm) has now consumed back to the host so the
    // per-terminal backlog drains and the paused Rust reader resumes below the
    // low-water mark. Fire-and-forget — it must never block the write path.
    try {
      coreApi().ptyAck(id, bytes);
    } catch {
      // A missed ack only delays a resume slightly — never loses a byte (the data
      // sits in the kernel PTY buffer). Swallow teardown races.
    }
  },
  subscribePtyExit(listener: Listener<PtyExit>): Promise<Unsubscribe> {
    return Promise.resolve(
      demux.subscribe<{ id: number; code: number | null }>(
        "pty://exit",
        (w) => listener({ id: w.id, code: w.code }),
      ),
    );
  },

  subscribeCommandOutput(listener: Listener<CommandOutput>): Promise<Unsubscribe> {
    return Promise.resolve(
      demux.subscribe<{ id: string; bytes: number[] | Uint8Array }>(
        "command://output",
        (w) => listener({ id: w.id, bytes: bytesIn(w.bytes) }),
      ),
    );
  },
  subscribeCommandState(listener: Listener<CommandState>): Promise<Unsubscribe> {
    return Promise.resolve(
      demux.subscribe<{ id: string; state: string; exitCode: number | null }>(
        "command://state",
        (w) => listener({ id: w.id, state: w.state, exitCode: w.exitCode ?? null }),
      ),
    );
  },
  subscribeCommandAck(listener: Listener<CommandAck>): Promise<Unsubscribe> {
    return Promise.resolve(
      demux.subscribe<{ id: string }>("command://ack", (w) => listener({ id: w.id })),
    );
  },

  subscribeTerminalBusyState(listener: Listener<TerminalBusyState>): Promise<Unsubscribe> {
    return Promise.resolve(
      demux.subscribe<{ id: string; busy: boolean }>(
        "terminal://busy-state",
        (w) => listener({ id: w.id, busy: w.busy }),
      ),
    );
  },
  subscribeTerminalExecState(listener: Listener<TerminalExecState>): Promise<Unsubscribe> {
    return Promise.resolve(
      demux.subscribe<{ id: string; state: string; exitCode: number | null }>(
        "terminal://exec-state",
        (w) => listener({ id: w.id, state: w.state, exitCode: w.exitCode ?? null }),
      ),
    );
  },

  window: windowControls,

  async pickDirectory(title?: string): Promise<string | null> {
    // The OS folder picker lives in the MAIN process (the sandboxed renderer cannot
    // open a native dialog directly); exposed through the window bridge.
    try {
      return (await windowApi().pickDirectory(title)) ?? null;
    } catch {
      return null;
    }
  },

  paths,
};
