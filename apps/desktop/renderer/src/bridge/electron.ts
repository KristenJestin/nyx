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
import { errorText } from "./error-text";

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
  // Optional so a preload predating the close-guard (or a bare test double) type-checks;
  // the adapter falls back to `pagehide` / no-ops when they are absent.
  onCloseRequested?(handler: () => void): () => void;
  confirmClose?(): Promise<void>;
  controlsVisible(): Promise<boolean>;
  pickDirectory(title?: string): Promise<string | null>;
  homeDir(): Promise<string | null>;
  // Optional for the same reason as the close-guard pair above: a preload predating
  // the project-identity picker (or a bare test double) must still type-check.
  pickProjectImage?(projectId: string): Promise<string | null>;
  discardProjectImage?(projectId: string): Promise<void>;
  pruneProjectImages?(liveProjectIds: string[]): Promise<number>;
}

/** Read the injected bridge objects. They exist whenever this adapter is selected
 *  (the env detector only picks Electron when `window.nyxCore` is present). */
function coreApi(): NyxCoreApi {
  return (window as unknown as { nyxCore: NyxCoreApi }).nyxCore;
}
function windowApi(): NyxWindowApi {
  return (window as unknown as { nyxWindow: NyxWindowApi }).nyxWindow;
}

/**
 * The channel `window.nyxCore.invoke` rides (mirror of `CORE_CHANNELS.invoke` in
 * `apps/desktop/src/shared/ipc.ts`, declared locally like {@link CoreEventEnvelope} so the frontend
 * does not import from the electron app package). It is not used to CALL anything here — the
 * preload owns the channel — only to reconstruct, exactly, the wrapper Electron builds from it.
 */
const CORE_INVOKE_CHANNEL = "nyx:core/invoke";

/**
 * The wrapper `ipcRenderer.invoke` puts around EVERY rejection: when a main-process
 * `ipcMain.handle` throws, the renderer side does not re-raise that error — it throws a NEW one
 * whose message is `` `Error invoking remote method '${channel}': ${error}` ``. The trailing
 * interpolation stringifies the deserialized error, which is where the second `Error: ` comes from.
 * Both halves are fixed strings, so both can be removed EXACTLY rather than guessed at.
 */
const IPC_WRAP_PREFIX = `Error invoking remote method '${CORE_INVOKE_CHANNEL}': `;

/**
 * The same wrapper, for ANY channel. Electron builds it identically whichever `ipcMain.handle`
 * threw, so the two fixed halves — the lead-in and the `': ` that closes the channel name — are
 * still removed exactly; only the channel between them is left unconstrained. Needed because the
 * WINDOW channels (`nyx:window/…`) also surface refusals a human reads: the picture picker throws
 * a written sentence, and without this the settings modal would print
 * `Error invoking remote method 'nyx:window/pick-project-image': Error: <sentence>`.
 */
const IPC_WRAP_ANY_CHANNEL = /^Error invoking remote method '[^']*': /;
/** `Error.prototype.toString()` on the main-side error, i.e. `name + ": "` with the default name. */
const STRINGIFIED_ERROR_PREFIX = "Error: ";

/** A non-null object, without asserting anything about its fields. */
function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

/** The `message` a value carries, when it carries one as a string — a SHAPE test, not a class one. */
function messageField(value: unknown): string | null {
  return isRecord(value) && typeof value.message === "string" ? value.message : null;
}

/**
 * The failure BEHIND an `ipcRenderer.invoke` rejection — the backend's own refusal, without the
 * transport's wrapping (second human review, finding F6).
 *
 * # What was on screen
 *
 * A duplicate link showed, in the block's `role="alert"`:
 * `Error invoking remote method 'nyx:core/invoke': Error: url '…' is already attached to this Spec`.
 * The sentence the Rust authority wrote with care was last, behind a channel name and two stacked
 * `Error:`. Nothing downstream was at fault: `errorText` renders a message VERBATIM, which is its
 * whole point — it was handed a message that had already been wrapped.
 *
 * # Why HERE and nowhere else
 *
 * This is the ONE place a native failure becomes the `BridgeError` the app will show
 * ({@link toBridgeError}). Peeling in a hook or a component would put a copy of this knowledge in
 * every surface, free to drift — the defect `error-text.ts` exists to prevent. Peeling here fixes
 * every command at once, which is the point of having a single conversion.
 *
 * # Two branches, in order of preference, and a refusal to guess
 *
 * (a) Electron may attach the ORIGINAL, deserialized error as `cause`. When it does, that object
 *     carries the authority's sentence with no wrapping at all — it is handed on as-is and NO
 *     string is touched. Which Electron builds do this cannot be settled from source (the
 *     renderer-side `invoke` lives in the binary's V8 snapshot), so the branch is WRITTEN rather
 *     than assumed, and costs nothing when it is not taken.
 * (b) Otherwise the two fixed prefixes above are removed — reconstructed from the channel name the
 *     code already knows, not matched by an approximate pattern.
 *
 * If the message does NOT start with that exact wrapper, the value is returned UNTOUCHED: a
 * failure that never crossed this channel (a timeout, an abort, a rejection raised in-process)
 * keeps its own words, and no speculative slice can ever eat a legitimate sentence.
 *
 * # It reads a SHAPE, and it does not read a message to show it
 *
 * The inspection is structural — a `message` field of type string — and never a prototype test:
 * an error that crossed an IPC boundary may well have lost its prototype (`error-text.ts` lists
 * that case), and the guard-rail `error-text.guard.test.ts` refuses class tests in every surface
 * for that exact reason. Note also what this function does NOT do: it never decides what to
 * DISPLAY. It picks WHICH value the one reader — {@link errorText} — is then handed.
 */
function unwrapCoreInvokeRejection(e: unknown): unknown {
  const wrapped = messageField(e);
  if (wrapped === null || !wrapped.startsWith(IPC_WRAP_PREFIX)) return e;
  // (a) The original failure, when this build attached it.
  const cause = isRecord(e) ? e.cause : undefined;
  if ((messageField(cause) ?? "").trim().length > 0) return cause;
  // (b) The exact wrapper, removed.
  const inner = wrapped.slice(IPC_WRAP_PREFIX.length);
  return inner.startsWith(STRINGIFIED_ERROR_PREFIX)
    ? inner.slice(STRINGIFIED_ERROR_PREFIX.length)
    : inner;
}

/**
 * {@link unwrapCoreInvokeRejection} for a rejection that crossed ANY channel — used where a
 * window-channel failure is rethrown to a surface that will DISPLAY it. Same two-step reading:
 * the attached cause when the build provides one, else the wrapper removed from the message.
 */
function unwrapInvokeRejection(e: unknown): unknown {
  const wrapped = messageField(e);
  if (wrapped === null || !IPC_WRAP_ANY_CHANNEL.test(wrapped)) return e;
  const cause = isRecord(e) ? e.cause : undefined;
  if ((messageField(cause) ?? "").trim().length > 0) return cause;
  const inner = wrapped.replace(IPC_WRAP_ANY_CHANNEL, "");
  return inner.startsWith(STRINGIFIED_ERROR_PREFIX)
    ? new Error(inner.slice(STRINGIFIED_ERROR_PREFIX.length))
    : new Error(inner);
}

/** Build a {@link BridgeError} from a caught IPC/command failure. */
function toBridgeError(kind: BridgeError["kind"], cause: unknown, source?: string): BridgeError {
  // The SAME reading every surface uses (finding `01KYVT6HVA260DTAEREQA4DFJN`) — this is where a
  // native failure becomes the message the whole app will show, so it may not have a private one.
  // What it reads is the failure BEHIND the IPC wrapper; `cause` still carries the raw value, which
  // the contract keeps for logging and never for control flow.
  return {
    kind,
    message: errorText(unwrapCoreInvokeRejection(cause), "bridge call failed"),
    source,
    cause,
  };
}

/**
 * Run a promise under the request options: a timeout (reject `timeout`) and/or an
 * abort signal (reject `canceled`). With neither, returns the promise unchanged.
 * Identical semantics to the Tauri adapter's `withRequestOptions`.
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
  // No `dragRegionProps` here: the drag region is CSS, not IPC. `<ChromeBar>` carries the
  // `drag-region` utility (`-webkit-app-region: drag`, globals.css) and Chromium moves the
  // frameless window from it on its own — see the note in contract.ts.
  async onCloseRequested(handler: () => void): Promise<Unsubscribe> {
    // The OS close button / Alt-F4 / quit routes through the MAIN process, which
    // WITHHOLDS the real close and relays the `nyx:window/close-requested` push. Prefer
    // that real signal (fires BEFORE teardown, so the close-warning guard can actually
    // show its dialog and the caller then proceeds via `confirmClose`). Fall back to the
    // DOM `pagehide` only when the preload predates the channel (older shell / tests),
    // where all a handler can do is a best-effort flush. Idempotent unsubscribe.
    let done = false;
    const api = windowApi();
    if (typeof api.onCloseRequested === "function") {
      const un = api.onCloseRequested(() => handler());
      return () => {
        if (done) return;
        done = true;
        un();
      };
    }
    const onHide = () => handler();
    window.addEventListener("pagehide", onHide);
    return () => {
      if (done) return;
      done = true;
      window.removeEventListener("pagehide", onHide);
    };
  },
  confirmClose: () => {
    const api = windowApi();
    // No confirm-close channel (a preload predating it) means no close-WITHHOLD guard
    // either — the two ship together. So degrade to a plain close instead of resolving
    // into the void: the chrome bar's X routes through here, and a silent no-op would
    // leave the user with a window that cannot be closed at all.
    return (api.confirmClose ? api.confirmClose() : api.close()).catch((e) => {
      throw toBridgeError("ipc", e, "window.confirmClose");
    });
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
    const un = demux.subscribe<{ id: number; bytes: number[] | Uint8Array }>("pty://output", (w) =>
      listener({ id: w.id, bytes: bytesIn(w.bytes) }),
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
      demux.subscribe<{ id: number; code: number | null }>("pty://exit", (w) =>
        listener({ id: w.id, code: w.code }),
      ),
    );
  },

  subscribeCommandOutput(listener: Listener<CommandOutput>): Promise<Unsubscribe> {
    return Promise.resolve(
      demux.subscribe<{ id: string; bytes: number[] | Uint8Array }>("command://output", (w) =>
        listener({ id: w.id, bytes: bytesIn(w.bytes) }),
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
      demux.subscribe<{ id: string; busy: boolean }>("terminal://busy-state", (w) =>
        listener({ id: w.id, busy: w.busy }),
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

  async pickProjectImage(projectId: string): Promise<string | null> {
    // NOT swallowed, unlike `pickDirectory` above: a refusal here (extension, size)
    // is a sentence the human needs to read, and turning it into `null` would spell
    // it exactly like pressing Cancel. Only a preload that predates the picker
    // degrades silently — there is nothing to say about it.
    const api = windowApi();
    if (!api.pickProjectImage) {
      // A preload predating the picker. NOT `null`: that is how Cancel is spelled, and a button
      // that silently does nothing is worse than one that says why.
      throw new Error("This build cannot pick a project picture — restart nyx to pick one up.");
    }
    try {
      return (await api.pickProjectImage(projectId)) ?? null;
    } catch (e) {
      // The refusal written in the main process, WITHOUT Electron's `Error invoking remote
      // method '…'` wrapper: this message goes straight into the alert under the button.
      throw unwrapInvokeRejection(e);
    }
  },

  async discardProjectImage(projectId: string): Promise<void> {
    // Best-effort by design: the row is the truth, the file is a cache of it. A
    // failed delete leaves an orphan that the NEXT gesture on this project takes —
    // adopting a picture and removing the identity both work off the directory
    // listing, never off a remembered name, so the retry is automatic. NOT the boot
    // sweep: it spares every file a LIVE project could claim, so it only collects
    // this one once the project itself is gone. Either way, a locked file must never
    // block the human from removing the identity.
    try {
      await windowApi().discardProjectImage?.(projectId);
    } catch {
      /* the next gesture on this project takes it */
    }
  },

  async pruneProjectImages(liveProjectIds: string[]): Promise<number> {
    try {
      return (await windowApi().pruneProjectImages?.(liveProjectIds)) ?? 0;
    } catch {
      return 0;
    }
  },

  paths,
};
