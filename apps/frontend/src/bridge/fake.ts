/**
 * `FakeNyxBridge` — an in-memory {@link NyxBridge} for tests. Implements the SAME
 * contract as the real adapters, so component tests drive it instead of mocking
 * `@tauri-apps/*`, and the shared contract suite (`./contract.test.ts`) runs the
 * same behavioral assertions against it AND every real adapter.
 *
 * It records invokes, lets a test push events to subscribers, and honors the
 * unsubscribe / binary / error guarantees the contract documents.
 */
import type {
  BackendCommand,
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

type Handler = (args?: Record<string, unknown>) => unknown;

export class FakeNyxBridge implements NyxBridge {
  /** Per-command stub responses; a missing command rejects with a `command` error. */
  private readonly handlers = new Map<BackendCommand, Handler>();
  /**
   * Optional CATCH-ALL handler consulted when no per-command stub matches — the
   * drop-in for the Tauri mock IPC's single `mockIPC((cmd,args)=>…)` callback, so a
   * migrated test installs ONE backend for every command. `on(...)` per-command
   * stubs still win (they are checked first). Throwing rejects with a BridgeError.
   */
  private catchAll: ((command: BackendCommand, args?: Record<string, unknown>) => unknown) | null =
    null;
  /** Recorded invocations, in order, for assertions. */
  readonly calls: Array<{ command: BackendCommand; args?: Record<string, unknown> }> = [];
  /** Live subscribers keyed by the logical channel. */
  private readonly subs = new Map<string, Set<Listener<unknown>>>();

  // --- Test controls ------------------------------------------------------
  /** Stub a command's response (value or thrown error). */
  on(command: BackendCommand, handler: Handler): this {
    this.handlers.set(command, handler);
    return this;
  }
  /** Install the catch-all backend (the `mockIPC` drop-in). Returns `this`. */
  onAnyCommand(
    handler: (command: BackendCommand, args?: Record<string, unknown>) => unknown,
  ): this {
    this.catchAll = handler;
    return this;
  }
  /**
   * Reset all mutable state IN PLACE — handlers, catch-all, recorded calls/window
   * calls, close-request handlers, and live subscribers — so a test-harness
   * `clearMocks()` returns the SAME instance to a pristine state (the mocked
   * `nyxBridge` reference the code captured stays valid).
   */
  reset(): void {
    this.handlers.clear();
    this.catchAll = null;
    this.calls.length = 0;
    this.windowCalls.length = 0;
    this.closeRequestedHandlers.length = 0;
    this.ackCalls.length = 0;
    this.subs.clear();
    this.pickedDirectory = null;
    this.homeDirValue = "/home/test";
  }
  /** Push an event to every live subscriber of `channel`. */
  emit(channel: string, payload: unknown): void {
    this.subs.get(channel)?.forEach((l) => l(payload));
  }
  /** Number of live subscribers on a channel (to assert unsubscribe worked). */
  subscriberCount(channel: string): number {
    return this.subs.get(channel)?.size ?? 0;
  }

  private addSub<T>(channel: string, listener: Listener<T>): Promise<Unsubscribe> {
    let set = this.subs.get(channel);
    if (!set) {
      set = new Set();
      this.subs.set(channel, set);
    }
    const l = listener as Listener<unknown>;
    set.add(l);
    let done = false;
    return Promise.resolve(() => {
      if (done) return;
      done = true;
      set!.delete(l);
    });
  }

  // --- NyxBridge ----------------------------------------------------------
  invoke<R>(
    command: BackendCommand,
    args?: Record<string, unknown>,
    _opts?: RequestOptions,
  ): Promise<R> {
    this.calls.push({ command, args });
    // Per-command stub wins; otherwise the catch-all (the `mockIPC` drop-in).
    const h = this.handlers.get(command);
    try {
      if (h) return Promise.resolve(h(args) as R);
      if (this.catchAll) return Promise.resolve(this.catchAll(command, args) as R);
    } catch (e) {
      return Promise.reject({ kind: "command" as const, message: String(e), source: command });
    }
    return Promise.reject({
      kind: "command" as const,
      message: `no fake handler for ${command}`,
      source: command,
    });
  }

  subscribe<T>(event: string, listener: Listener<T>): Promise<Unsubscribe> {
    return this.addSub(event, listener);
  }

  ptySpawn(opts: PtySpawnOptions): Promise<number> {
    return this.invoke<number>("pty_spawn", { ...opts });
  }
  ptyWrite(id: number, data: Uint8Array): Promise<void> {
    return this.invoke<void>("pty_write", { id, data: Array.from(data) });
  }
  ptyResize(id: number, cols: number, rows: number): Promise<void> {
    return this.invoke<void>("pty_resize", { id, cols, rows });
  }
  ptyClose(id: number): Promise<void> {
    return this.invoke<void>("pty_close", { id });
  }
  subscribePtyOutput(listener: Listener<PtyOutput>) {
    return this.addSub("pty://output", listener);
  }
  subscribePtyExit(listener: Listener<PtyExit>) {
    return this.addSub("pty://exit", listener);
  }
  /** Recorded flow-control acks (so a test can assert the consumer credits bytes
   *  only after xterm consumes them). The fake has no real backpressure. */
  readonly ackCalls: Array<{ id: number; bytes: number }> = [];
  ackPtyOutput(id: number, bytes: number): void {
    this.ackCalls.push({ id, bytes });
  }
  subscribeCommandOutput(listener: Listener<CommandOutput>) {
    return this.addSub("command://output", listener);
  }
  subscribeCommandState(listener: Listener<CommandState>) {
    return this.addSub("command://state", listener);
  }
  subscribeCommandAck(listener: Listener<CommandAck>) {
    return this.addSub("command://ack", listener);
  }
  subscribeTerminalBusyState(listener: Listener<TerminalBusyState>) {
    return this.addSub("terminal://busy-state", listener);
  }
  subscribeTerminalExecState(listener: Listener<TerminalExecState>) {
    return this.addSub("terminal://exec-state", listener);
  }

  readonly windowCalls: string[] = [];
  readonly window: WindowControls = {
    minimize: () => {
      this.windowCalls.push("minimize");
      return Promise.resolve();
    },
    toggleMaximize: () => {
      this.windowCalls.push("toggleMaximize");
      return Promise.resolve();
    },
    close: () => {
      this.windowCalls.push("close");
      return Promise.resolve();
    },
    dragRegionProps: () => ({ "data-tauri-drag-region": true }),
    onCloseRequested: (handler: () => void) => {
      this.closeRequestedHandlers.push(handler);
      return Promise.resolve(() => {
        const i = this.closeRequestedHandlers.indexOf(handler);
        if (i >= 0) this.closeRequestedHandlers.splice(i, 1);
      });
    },
  };
  /** Test control: invoke to simulate the OS close-requested signal. */
  readonly closeRequestedHandlers: Array<() => void> = [];
  triggerCloseRequested(): void {
    this.closeRequestedHandlers.forEach((h) => h());
  }

  pickedDirectory: string | null = null;
  pickDirectory(_title?: string): Promise<string | null> {
    return Promise.resolve(this.pickedDirectory);
  }

  homeDirValue: string | null = "/home/test";
  readonly paths = {
    homeDir: () => Promise.resolve(this.homeDirValue),
  };
}
