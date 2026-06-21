/**
 * MAIN-side manager of the dedicated core-host (the mirror, on the Electron side, of
 * how the Tauri adapter owns the core in-process).
 *
 * Spawns the core-host as a Node-pure child of the Electron binary
 * (`ELECTRON_RUN_AS_NODE=1`, NOT `utilityProcess` — PRD frozen decision), wires a
 * correlated request/response + event channel over Node's built-in IPC
 * (`child.send` / `child.on('message')`, enabled by the `'ipc'` stdio slot), and
 * resolves the two `AppPaths` the host needs (which the host can't resolve itself —
 * `app` is unavailable under `ELECTRON_RUN_AS_NODE`).
 *
 * Lifecycle (task #25): a bounded boot HANDSHAKE (ready/degraded/fatal — never an
 * infinite load), single-shot crash RESTART gated on whether work was active,
 * ordered SHUTDOWN that blocks new requests, and FORCED cleanup so no orphan host /
 * PTY survives. State changes are observable (`onState`) so main can surface a
 * degraded/fatal host to the renderer.
 */
import { spawn, type ChildProcess } from "node:child_process";
import path from "node:path";
import { app } from "electron";

import type {
  HostBootConfig,
  HostEventPayload,
  HostMessage,
  HostRequestPayload,
  HostResponse,
  PingResult,
} from "../shared/host-protocol";

/** The Electron binary path — re-execed as Node for the host. `process.execPath` IS
 * the Electron binary in both dev and packaged builds. */
function electronBinary(): string {
  return process.execPath;
}

/**
 * Resolve the host entry script as a REAL on-disk path. Bundled to
 * `dist/core-host/index.js` next to the compiled main
 * (`dist/main/core-host.js` → `../core-host/index.js`). In a packaged build the
 * core-host is `asarUnpack`ed (electron-builder), so we redirect an `app.asar` path
 * to its `app.asar.unpacked` sibling — the host is then spawned from a real file
 * (not from inside the archive), removing any asar-spawn ambiguity at the gate.
 */
function hostEntry(): string {
  let entry = path.join(__dirname, "..", "core-host", "index.js");
  if (entry.includes(`app.asar${path.sep}`) && !entry.includes("app.asar.unpacked")) {
    entry = entry.replace(`app.asar${path.sep}`, `app.asar.unpacked${path.sep}`);
  }
  return entry;
}

/**
 * Resolve the writable data dir for the host: `NYX_DATA_DIR` override (the e2e
 * harness pins it) else Electron's `userData`. Mirrors the Tauri `resolve_data_dir`.
 */
export function resolveDataDir(): string {
  const override = process.env.NYX_DATA_DIR;
  if (override && override.length > 0) return override;
  return app.getPath("userData");
}

/**
 * Resolve the read-only resource dir (the unpacked, outside-asar resources). In a
 * packaged build electron-builder packs our app into `app.asar` and unpacks the
 * `asarUnpack` globs (native module, core-host, and `dist/resources/**`) to
 * `resources/app.asar.unpacked`, preserving the `dist/` prefix. So the unpacked
 * resource ROOT is `…/app.asar.unpacked/dist` — this is the directory whose
 * `resources/claude-plugin` subpath nyx-core's `resolve_bundled_plugin_dir`
 * resolves (it appends `resources/claude-plugin`, matching the Tauri resource layout
 * where the bundled plugin sits at `<resource_dir>/resources/claude-plugin`).
 * `process.resourcesPath` is the packaged `resources/` dir. In dev there is no
 * packaged resource dir → null (matches the Tauri `None` in a bare run; nyx-core then
 * falls back to its source tree). The host re-points an `app.asar` path to
 * `app.asar.unpacked` itself.
 */
export function resolveResourceDir(): string | null {
  if (!app.isPackaged) return null;
  return path.join(process.resourcesPath, "app.asar.unpacked", "dist");
}

/** A pending correlated request awaiting its reply. */
interface Pending {
  resolve: (value: unknown) => void;
  reject: (err: Error) => void;
}

/** Listener for host→main events (the EventSink frontier, surfaced to main). */
export type HostEventListener = (event: HostEventPayload) => void;

/**
 * The observable host lifecycle state.
 *   - `starting`  — spawned, boot handshake in flight.
 *   - `ready`     — boot handshake succeeded; serving.
 *   - `degraded`  — an unexpected exit happened that we did NOT auto-restart (work
 *                   was active, or the single restart was already used). Surfaced to
 *                   the renderer; NOT an infinite load.
 *   - `fatal`     — boot failed (timeout or a `.node`/boot error). Readable, terminal.
 *   - `stopping`  — an ordered shutdown is in progress; new requests are refused.
 *   - `stopped`   — fully down (clean shutdown or after teardown).
 */
export type HostState = "idle" | "starting" | "ready" | "degraded" | "fatal" | "stopping" | "stopped";

/** A state-change observation (with a readable reason for degraded/fatal). */
export interface HostStateChange {
  state: HostState;
  reason?: string;
}

export type HostStateListener = (change: HostStateChange) => void;

/** How long the boot handshake may take before we declare `fatal` (never hang).
 * Overridable via `NYX_HOST_BOOT_TIMEOUT_MS` (operational knob + lets tests shorten
 * the wait); defaults to 10s. */
function bootTimeoutMs(): number {
  const raw = Number(process.env.NYX_HOST_BOOT_TIMEOUT_MS);
  return Number.isFinite(raw) && raw > 0 ? raw : 10_000;
}

export class CoreHost {
  private child: ChildProcess | null = null;
  private nextId = 1;
  private readonly pending = new Map<number, Pending>();
  private readonly listeners = new Set<HostEventListener>();
  private readonly stateListeners = new Set<HostStateListener>();

  private state: HostState = "idle";
  private stateReason: string | undefined;
  /** Latched true while WE initiated the shutdown (so its exit is not a crash). */
  private intentionalStop = false;
  /** The single automatic restart budget (a crash with no active work spends it). */
  private restartBudget = 1;
  /** Count of live PTYs / managed commands — "active work" for the restart gate. */
  private activeWork = 0;
  /** Resolver for the in-flight boot handshake, settled by `ready`/timeout/fatal. */
  private bootSettle: { resolve: () => void; reject: (e: Error) => void } | null = null;
  private bootTimer: NodeJS.Timeout | null = null;

  /** Subscribe to host events (ready/pty-output/pty-exit/changed/fatal). */
  onEvent(listener: HostEventListener): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  /** Subscribe to lifecycle state changes (main surfaces these to the renderer). */
  onState(listener: HostStateListener): () => void {
    this.stateListeners.add(listener);
    return () => this.stateListeners.delete(listener);
  }

  /** The current lifecycle state. */
  get currentState(): HostState {
    return this.state;
  }

  /** The readable reason for the current state (set for degraded/fatal), if any. */
  get currentStateReason(): string | undefined {
    return this.stateReason;
  }

  /** Whether the host child is currently alive (spawned and not exited). */
  get alive(): boolean {
    return this.child !== null;
  }

  /** The live child's PID (for the smoke / lifecycle to assert identity). */
  get pid(): number | undefined {
    return this.child?.pid;
  }

  /** Mark a unit of active work started/stopped (a PTY/managed command). Used by the
   * crash-restart gate: a crash WITH active work must not be silently restarted. */
  markWorkStarted(): void {
    this.activeWork += 1;
  }
  markWorkStopped(): void {
    this.activeWork = Math.max(0, this.activeWork - 1);
  }
  get hasActiveWork(): boolean {
    return this.activeWork > 0;
  }

  private setState(state: HostState, reason?: string): void {
    this.state = state;
    this.stateReason = reason;
    for (const l of this.stateListeners) l({ state, reason });
  }

  /**
   * Spawn the host AND await its boot handshake, BOUNDED by the boot timeout.
   * Resolves once the host emits `ready`; rejects (state `fatal`) on a boot timeout
   * or a `fatal` event (e.g. the `.node` failed to load). Never hangs — the timeout
   * guarantees a readable terminal state instead of an infinite load.
   */
  start(): Promise<void> {
    if (this.child) throw new Error("core-host already started");
    this.intentionalStop = false;
    this.setState("starting");
    this.spawnChild();

    const timeout = bootTimeoutMs();
    return new Promise<void>((resolve, reject) => {
      this.bootSettle = { resolve, reject };
      this.bootTimer = setTimeout(() => {
        // Boot handshake timed out → fatal + kill the half-booted child.
        const reason = `boot handshake timed out after ${timeout}ms`;
        this.setState("fatal", reason);
        this.intentionalStop = true;
        this.child?.kill();
        this.settleBoot(new Error(reason));
      }, timeout);
    });
  }

  /** Low-level spawn (used by `start` and the single crash-restart). */
  private spawnChild(): void {
    const config: HostBootConfig = {
      dataDir: resolveDataDir(),
      resourceDir: resolveResourceDir(),
    };
    this.child = spawn(electronBinary(), [hostEntry()], {
      env: {
        ...process.env,
        ELECTRON_RUN_AS_NODE: "1",
        NYX_HOST_CONFIG: JSON.stringify(config),
      },
      stdio: ["inherit", "inherit", "inherit", "ipc"],
    });

    this.child.on("message", (msg: HostMessage) => this.onMessage(msg));
    this.child.on("exit", (code, signal) => this.onExit(code, signal));
    this.child.on("error", (err) => {
      // Spawn-level failure (e.g. binary missing): a readable fatal, not a hang.
      this.setState("fatal", `core-host spawn error: ${err.message}`);
      this.failAllPending(`core-host spawn error: ${err.message}`);
      this.settleBoot(err);
    });
  }

  private settleBoot(err?: Error): void {
    if (this.bootTimer) {
      clearTimeout(this.bootTimer);
      this.bootTimer = null;
    }
    const s = this.bootSettle;
    this.bootSettle = null;
    if (!s) return;
    if (err) s.reject(err);
    else s.resolve();
  }

  private onMessage(msg: HostMessage): void {
    if (!msg || typeof msg !== "object") return;
    if (msg.type === "res") {
      this.settle(msg);
    } else if (msg.type === "evt") {
      const p = msg.payload;
      // Drive the boot handshake from the host's own lifecycle events.
      if (p.kind === "ready") {
        this.setState("ready");
        this.settleBoot();
      } else if (p.kind === "fatal") {
        // A boot/load fatal from the host → readable fatal state (never a hang).
        this.setState("fatal", p.error);
        this.settleBoot(new Error(p.error));
      }
      for (const l of this.listeners) l(p);
    }
  }

  private settle(res: HostResponse): void {
    const p = this.pending.get(res.id);
    if (!p) return;
    this.pending.delete(res.id);
    if (res.ok) p.resolve(res.result);
    else p.reject(new Error(res.error));
  }

  /**
   * Child exit handler — the crux of the crash policy. An exit we did NOT initiate is
   * a CRASH. We restart AT MOST ONCE, and ONLY if no work was active; otherwise we go
   * `degraded` (surfaced to the renderer) and do NOT blindly restart.
   */
  private onExit(code: number | null, signal: NodeJS.Signals | null): void {
    this.child = null;
    this.failAllPending(`core-host exited (code=${code}, signal=${signal})`);

    if (this.intentionalStop) {
      // We asked it to stop — terminal clean state.
      this.setState("stopped");
      return;
    }

    // Unexpected exit = crash.
    const detail = `core-host crashed (code=${code}, signal=${signal})`;
    if (!this.hasActiveWork && this.restartBudget > 0) {
      // Crash with NO active work → spend the single restart budget.
      this.restartBudget -= 1;
      this.setState("starting", `${detail} — restarting once`);
      this.spawnChild();
      // Re-arm the boot timeout for the restart so it can't hang either.
      this.bootTimer = setTimeout(() => {
        const reason = `boot handshake timed out after restart`;
        this.setState("fatal", reason);
        this.intentionalStop = true;
        this.child?.kill();
      }, bootTimeoutMs());
    } else {
      // Crash WITH active work, or the restart budget is spent → degraded, no blind
      // restart (the renderer is told; the user decides).
      const why = this.hasActiveWork
        ? `${detail} while work was active — not auto-restarted`
        : `${detail} — restart budget exhausted`;
      this.setState("degraded", why);
    }
  }

  private failAllPending(reason: string): void {
    for (const [, p] of this.pending) p.reject(new Error(reason));
    this.pending.clear();
  }

  /** Send a correlated request; resolves with the host's `result` or rejects. New
   * requests are REFUSED once a shutdown is in progress (or the host isn't ready) —
   * EXCEPT the `shutdown` request itself, which `stop()` issues via the internal
   * `send` to drive the ordered teardown. */
  request<T = unknown>(payload: HostRequestPayload, timeoutMs = 5000): Promise<T> {
    if (this.state === "stopping" || this.state === "stopped") {
      return Promise.reject(new Error(`core-host is ${this.state}; new requests are refused`));
    }
    if (!this.child) {
      return Promise.reject(new Error(`core-host is not running (state=${this.state})`));
    }
    return this.send<T>(payload, timeoutMs);
  }

  /** The unguarded correlated send (the wire mechanics). `request` adds the
   * state guard on top; `stop` uses this directly for the shutdown handshake. */
  private send<T = unknown>(payload: HostRequestPayload, timeoutMs: number): Promise<T> {
    if (!this.child) {
      return Promise.reject(new Error(`core-host is not running (state=${this.state})`));
    }
    const id = this.nextId++;
    return new Promise<T>((resolve, reject) => {
      const timer = setTimeout(() => {
        if (this.pending.delete(id)) reject(new Error(`request ${payload.kind} timed out`));
      }, timeoutMs);
      this.pending.set(id, {
        resolve: (v) => {
          clearTimeout(timer);
          resolve(v as T);
        },
        reject: (e) => {
          clearTimeout(timer);
          reject(e);
        },
      });
      this.child!.send({ type: "req", id, payload });
    });
  }

  /**
   * Fire-and-forget send: deliver a request payload the host does NOT reply to (the
   * flow-control `pty-ack`). It registers NO pending entry — so a high-rate ack
   * stream never grows the pending map or waits on a 5s timeout — and is dropped if
   * the host is not currently serving (a missed ack just means a slightly later
   * resume, never a lost byte: the data sits in the kernel buffer regardless).
   */
  notify(payload: HostRequestPayload): void {
    if (this.state !== "ready" || !this.child) return;
    // `id` is required by the envelope but irrelevant (no reply is correlated).
    this.child.send({ type: "req", id: this.nextId++, payload });
  }

  /** Correlated liveness/ABI probe → the host's proof bundle. */
  ping(): Promise<PingResult> {
    return this.request<PingResult>({ kind: "ping" });
  }

  /**
   * Ordered shutdown. Transitions to `stopping` (which REFUSES new requests), asks
   * the host to shut down in order (snapshot → stop PTY/commands/MCP/DB on the host
   * side), then FORCE-kills if it lingers past `timeoutMs` so no orphan host/PTY
   * survives. Idempotent: a second call (e.g. close-request then destroy) is a no-op.
   */
  async stop(timeoutMs = 3000): Promise<void> {
    if (this.state === "stopping" || this.state === "stopped" || this.state === "idle") {
      return;
    }
    const child = this.child;
    this.intentionalStop = true;
    this.setState("stopping");
    if (!child) {
      this.setState("stopped");
      return;
    }
    const exited = new Promise<void>((resolve) => child.once("exit", () => resolve()));
    try {
      // The shutdown handshake itself bypasses the `stopping` request-guard (it IS
      // the shutdown) via the internal `send`. Drives the host's ordered teardown
      // (snapshot → stop PTY/commands/MCP/DB). Ignore failure — we kill below.
      await this.send({ kind: "shutdown" }, timeoutMs);
    } catch {
      // ignore — forced kill below covers an unresponsive/crashed host.
    }
    const timer = setTimeout(() => {
      if (this.child) child.kill(); // forced cleanup — no orphan survives.
    }, timeoutMs);
    await exited;
    clearTimeout(timer);
    this.setState("stopped");
  }
}
