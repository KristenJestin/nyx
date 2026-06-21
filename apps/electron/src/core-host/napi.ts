/**
 * Loads the `nyx-napi` native addon — and ONLY here, in the dedicated core-host.
 *
 * The `.node` is NEVER required in the Electron main or renderer (PRD frozen
 * decision: a PTY fork from the Chromium main process SIGSEGVs — POC §B.1/§J). This
 * module is reachable only from the `ELECTRON_RUN_AS_NODE` host entry, which is why
 * the load lives in `core-host/`, not `main/`.
 *
 * ### Locating the addon (dev + packaged-unpacked-asar)
 *
 * The build stages the napi-rs OFFICIAL loader (`index.js`) + the platform-suffixed
 * `.node` into `dist/native/` (see `scripts/copy-napi.cjs`). We `require` that
 * loader so napi-rs resolves the correct per-platform `.node` suffix.
 *
 * In a PACKAGED build the addon is unpacked OUTSIDE the asar (electron-builder
 * `asarUnpack` of the native-module glob), so the on-disk path contains
 * `app.asar.unpacked` instead of `app.asar`. Node's `require` does this rewrite
 * transparently, but a
 * native `.node` genuinely cannot be mmap'd from inside the archive — so we resolve
 * the loader path and, if it sits under `app.asar`, redirect to
 * `app.asar.unpacked`. This makes the host find the addon both unpacked-in-place
 * (dev) and unpacked-from-asar (packaged), satisfying the done-criterion.
 */
import { createRequire } from "node:module";
import path from "node:path";
import fs from "node:fs";

/**
 * The PTY callbacks. The addon builds each `ThreadsafeFunction` with
 * `ErrorStrategy::Fatal`, so the value is the ONLY argument (NO leading `err`). We
 * type them variadic and read the LAST argument as the payload, so this stays
 * correct whether the binding delivers `(value)` or `(err, value)` — exactly as the
 * `verify-abi` harness does.
 */
export type PtyDataCallback = (...args: unknown[]) => void;
export type PtyExitCallback = (...args: unknown[]) => void;
export type PtyCwdCallback = (...args: unknown[]) => void;
export type PtyExecStateCallback = (...args: unknown[]) => void;

/** A decoded OSC 133 exec-state transition the addon delivers (see nyx-napi). */
export interface ExecStateEvent {
  /** `"success"` (exit 0) or `"error"` (non-zero / unknown). */
  state: string;
  /** Exit code when the `D` carried one; absent otherwise (still `error`). */
  exitCode?: number;
}

/**
 * The full phase-3 addon surface the host uses. Constructor mirrors the napi
 * `NyxPty::new`: (cols, rows, cwd?, terminalId?, onData, onExit, onCwd, onExecState).
 */
export interface NyxPtyCtor {
  new (
    cols: number,
    rows: number,
    cwd: string | null,
    terminalId: string | null,
    onData: PtyDataCallback,
    onExit: PtyExitCallback,
    onCwd: PtyCwdCallback,
    onExecState: PtyExecStateCallback,
  ): NyxPtyInstance;
}

export interface NyxPtyInstance {
  write(data: Buffer): void;
  resize(cols: number, rows: number): void;
  kill(): void;
  /** Lossless flow control: pause/resume the Rust reader (OS backpressure). */
  setPaused(paused: boolean): void;
  id(): number;
  /**
   * The OS-derived BUSY bit (PRD-5 task #1): `true` when a command runs in the
   * foreground, `false` at an idle prompt, `null` when undeterminable (non-Unix /
   * master closed). Read by the busy-state poll loop.
   */
  busy(): boolean | null;
  /**
   * The LIVE auto-label introspection of this terminal (the `terminal_info` backend):
   * `{ cwd, foreground }` read straight from the kernel (Linux `/proc`, keyed by the
   * shell pid + foreground pgid). Both `null` on Windows (no `/proc`) WITHOUT erroring —
   * a clean degradation. Polled on a bounded ~1s cadence by the auto-label loop.
   */
  terminalInfo(): { cwd: string | null; foreground: string | null };
}

/**
 * A terminal record the restore flow reads (PRD-5 task #5). Mirrors the napi
 * `TerminalRow` (camelCase) — the subset of the DB row the host needs to re-open a
 * terminal and re-evaluate its exec-state at boot.
 */
export interface TerminalRow {
  id: string;
  status: string;
  cwd: string;
  label: string | null;
  orderIndex: number;
  workspaceId: string | null;
  execState: string;
  execExitCode: number | null;
  execStateUnread: boolean;
  execStateUpdatedAt: number;
}

/** Result of persisting an exec-state transition (PRD-5 task #1). */
export interface ExecStatePersist {
  /** Whether a row was actually updated (the terminal id exists). */
  updated: boolean;
  /** The stamped `exec_state_updated_at` (epoch-ms); 0 when `updated` is false. */
  updatedAt: number;
}

/** A parked agent-session resume (PRD-5 task #5) — injected at the terminal's first
 * respawn. Mirrors the napi `ResumePark`. */
export interface ResumePark {
  terminalId: string;
  command: string;
  sessionId: string;
  uncertain: boolean;
}

/**
 * A managed-command runtime event the runner delivers on the Node loop (mirrors the
 * napi `CommandStateEvent`). The host maps `kind` to the matching Tauri event name so
 * the renderer's command band behaves identically:
 *   - `"state"`          → `command://state`  (a run-state transition)
 *   - `"ack"`            → `command://ack`    (the unread flag was cleared)
 *   - `"output-cleared"` → `command://output-cleared` (the captured buffer was wiped)
 */
export interface CommandStateEvent {
  kind: string;
  instanceId: string;
  /** `idle|running|success|error` for a `"state"` event; empty otherwise. */
  state: string;
  /** Natural exit code on a `success`/`error` finish; absent otherwise. */
  exitCode?: number;
}

/** A coalesced command-output chunk (mirrors the napi `CommandOutputEvent`). */
export interface CommandOutputEvent {
  instanceId: string;
  /** Raw output bytes (the renderer's xterm renders them as-is). */
  bytes: Buffer;
}

/** Callback shapes for the command runner — `ErrorStrategy::Fatal`, value-only. */
export type CommandStateCallback = (...args: unknown[]) => void;
export type CommandOutputCallback = (...args: unknown[]) => void;

/** A coarse `changed` invalidation an MCP mutating tool produced (mirrors the napi
 * `McpChangedEvent`): the renderer re-pulls the named collection. */
export interface McpChangedEvent {
  /** `terminals` | `workspaces` | `commands` | `agent-sessions`. */
  topic: string;
}

/** A live-PTY operation an MCP terminal tool queues to the host's PTY manager (mirrors
 * the napi `McpTerminalOp`): the half nyx-core cannot do (it owns the records, not the
 * live PTY). */
export interface McpTerminalOp {
  /** `park` (a create_terminal opening command) | `send` (a send_to_terminal write) |
   * `close` (kill the terminal's PTY). */
  op: string;
  /** The terminal RECORD id the op targets. */
  terminalId: string;
  /** The command line for `park`/`send`; empty for `close`. */
  command: string;
}

/** MCP dispatcher callback shapes — `ErrorStrategy::Fatal`, value-only. */
export type McpChangedCallback = (...args: unknown[]) => void;
export type McpTerminalOpCallback = (...args: unknown[]) => void;

/** The status of a command instance after a lifecycle call (mirrors `CommandStatus`). */
export interface CommandStatus {
  instanceId: string;
  state: string;
  running: boolean;
  exitCode: number | null;
  unread: boolean;
  wasRunning: boolean;
  restarted: boolean;
}

/**
 * The managed-command runner the host owns (mirrors the napi `NyxCommandRunner`).
 * Built ONCE at boot via `NyxCore.createCommandRunner`; drives the off-screen command
 * PTYs, persists to the shared pool, and is the SAME runner the MCP runtime tools route
 * onto. Parity with the Tauri `ManagedCommandRunner`.
 */
export interface NyxCommandRunnerInstance {
  /** Start (idempotent on a running instance). Resolves cmd+cwd from the DB first. */
  start(instanceId: string): CommandStatus;
  /** Stop (tree-kill). Idempotent on a non-running instance. */
  stop(instanceId: string): CommandStatus;
  /** Relaunch (the explicit restart; never two live processes). */
  relaunch(instanceId: string): CommandStatus;
  /** Read the instance's captured output (live tail while running, else persisted). */
  getOutput(instanceId: string): string;
  /** The live run status (no mutation). */
  status(instanceId: string): CommandStatus;
  /** Whether the instance has a live running process. */
  isRunning(instanceId: string): boolean;
  /** Acknowledge a finished one-shot's unseen result (clear `unread`, emit
   * `command://ack`); never touches the factual outcome. Returns the factual state. */
  acknowledge(instanceId: string): string;
  /** BOOT RESTORE: relaunch the snapshotted-running instances; returns the ids. */
  restoreOnBoot(): string[];
  /** SHUTDOWN SNAPSHOT: persist `was_running_on_shutdown` for every instance. */
  snapshotOnShutdown(): void;
  /** Latch the shutdown so snapshot+reap run exactly once. */
  beginShutdown(): boolean;
  /** Hard-kill every running instance's process tree (no orphans past shutdown). */
  killAllRunning(): void;
}

/**
 * The shared-core handle the host owns over nyx-core's DB pool + MCP server (PRD-5
 * tasks #2/#3/#5). Constructed once at boot (`new NyxCore(dataDir)`); every DB method
 * returns a Promise (a napi `AsyncTask` run on the libuv worker pool — NEVER the Node
 * loop), and the MCP server shares the SAME r2d2 pool.
 */
export interface NyxCoreInstance {
  /** Persist a terminal exec-state transition off the Node loop (OSC 133 result). */
  setExecState(
    terminalId: string,
    state: string,
    exitCode: number | null,
    unread: boolean,
  ): Promise<ExecStatePersist>;
  /** Read a terminal record (or null), e.g. to settle a stale `running` on exit. */
  getTerminal(terminalId: string): Promise<TerminalRow | null>;
  /** List every terminal in sidebar order (the boot restore read). */
  listTerminals(): Promise<TerminalRow[]>;
  /** Create a new alive terminal record (so restore can re-open it). */
  createTerminal(cwd: string, label: string | null): Promise<TerminalRow>;
  /**
   * Publish the record↔live-PTY join into the SYNCHRONOUS liveness registry (the
   * `register_terminal_pty` command — the Electron mirror of the Tauri `TerminalPtyMap`).
   * After this, the MCP terminal tools resolve the record to a LIVE shell synchronously
   * (`send_to_terminal` writes instead of `invalid_state`; `list_terminals` reports
   * `live: true`). Idempotent. Synchronous (a cheap map insert, never the Node loop).
   */
  registerTerminalPty(recordId: string, ptyId: number): void;
  /**
   * Retract the record↔live-PTY join (a null-pty register + the PTY-exit cleanup): the
   * record's shell is no longer live, so the MCP tools fall back to `invalid_state` /
   * `live: false`. Idempotent. Synchronous.
   */
  unregisterTerminalPty(recordId: string): void;
  /**
   * Run the backend auto-attach for a terminal RECORD given its live `cwd` (the
   * `auto_attach_terminal` command). The shared nyx-core resolver applies the hybrid
   * auto/manual rule + persists the decided binding; resolves to `{ workspaceId, changed }`.
   * Off the Node loop (AsyncTask).
   */
  autoAttachTerminal(
    terminalId: string,
    cwd: string | null,
  ): Promise<{ workspaceId: string | null; changed: boolean }>;
  /** Settle every phantom `running` terminal down to idle (boot normalization). */
  normalizePhantomTerminals(): Promise<number>;
  /** Boot agent-session resume scan: returns the `claude --resume` parks to inject. */
  resumeScanOnBoot(): Promise<ResumePark[]>;
  /** Mark a session `resume_failed` (a parked resume could not be injected). */
  markResumeFailed(sessionId: string): Promise<void>;
  /** TEST/PROOF: a deliberately slow DB query (does NOT block the loop). */
  dbSlowQuery(delayMs: number): Promise<number>;
  /**
   * The GENERIC, allowlisted DB-command dispatcher (PRD-5 review — full nyxBridge
   * surface end-to-end). Forward a contract `BackendCommand` NAME + a JSON args STRING;
   * resolves the JSON RESULT STRING (the host `JSON.parse`s it back to the contract
   * shape). Runs off the Node loop (AsyncTask). Rejects with a readable error for an
   * unknown command, bad args, or a command not available over this transport. The
   * managed-command RUNTIME (`command_start`/…) is NOT routed here — the host sends
   * those to the live `CommandManager`.
   */
  dbCommand(command: string, argsJson: string): Promise<string>;
  /**
   * Start the MCP server on the shared pool (idempotent); returns the bound port. The
   * dispatcher serves the FULL advertised surface (PRD-5 review #68). `onChanged`
   * receives a coarse `changed` invalidation after a mutating tool (so the renderer
   * re-pulls), and `onTerminalOp` receives the live-PTY operations the interactive
   * terminal tools queue to the host's PTY manager (the half nyx-core cannot do — it
   * owns the records, not the live PTY). Both are fire-and-forget on the Node loop.
   */
  mcpStart(onChanged?: McpChangedCallback, onTerminalOp?: McpTerminalOpCallback): number;
  /** The bound MCP port, or 0 if not started. */
  mcpPort(): number;
  /** Whether the MCP server has started. */
  mcpIsStarted(): boolean;
  /**
   * Boot reconciliation (PRD-5 #4): re-template/re-register installed providers'
   * plugins (never install on boot). Runs DETACHED + best-effort; the `claude` CLI it
   * shells out to is wall-clock bounded (a hung CLI cannot freeze the host).
   */
  mcpReconcile(dataDir: string, resourceDir: string | null): void;
  /**
   * Build the managed-command runner over the Node event callbacks + the SHARED pool,
   * stash it so the MCP runtime command tools route onto it, and return the handle the
   * host owns. Built ONCE at boot, BEFORE `mcpStart`, so the runtime tools are live
   * from the first MCP request (parity with the Tauri `manage_command_runner`).
   */
  createCommandRunner(
    onState: CommandStateCallback,
    onOutput: CommandOutputCallback,
  ): NyxCommandRunnerInstance;
}

export interface NyxCoreCtor {
  new (dataDir: string): NyxCoreInstance;
}

/** The runner is built via `NyxCore.createCommandRunner`, not a bare ctor, so the
 * addon need not expose `NyxCommandRunner` as a top-level constructor (it is exported
 * for its TYPE only). The host obtains instances through the core handle. */
export interface NyxNapi {
  version(): string;
  NyxPty: NyxPtyCtor;
  NyxCore: NyxCoreCtor;
}

/**
 * Resolve the path to the staged napi-rs loader (`dist/native/index.js`), correcting
 * an `app.asar` path to its `app.asar.unpacked` sibling so the native `.node` loads
 * from outside the archive in a packaged build.
 */
export function resolveNativeLoaderPath(): string {
  // dist/core-host/index.js → ../native/index.js
  let loader = path.join(__dirname, "..", "native", "index.js");
  // Packaged: redirect ...app.asar... → ...app.asar.unpacked... (the .node lives there).
  if (loader.includes(`app.asar${path.sep}`) && !loader.includes("app.asar.unpacked")) {
    loader = loader.replace(`app.asar${path.sep}`, `app.asar.unpacked${path.sep}`);
  }
  return loader;
}

/**
 * Load the addon. Throws a READABLE error (never a silent hang) if the loader/.node
 * is missing or fails the ABI match — the lifecycle turns that into a `fatal` state
 * the renderer can show (task #25).
 */
export function loadNapi(): NyxNapi {
  const loader = resolveNativeLoaderPath();
  if (!fs.existsSync(loader)) {
    throw new Error(
      `nyx-napi loader not found at ${loader} — was \`copy:napi\` run (and the .node built)?`,
    );
  }
  // Use a require bound to THIS file so the relative resolution + asar rewrite hold.
  const req = createRequire(__filename);
  const addon = req(loader) as NyxNapi;
  if (
    typeof addon.version !== "function" ||
    typeof addon.NyxPty !== "function" ||
    typeof addon.NyxCore !== "function"
  ) {
    throw new Error(`nyx-napi loaded from ${loader} but is missing version()/NyxPty/NyxCore`);
  }
  return addon;
}
