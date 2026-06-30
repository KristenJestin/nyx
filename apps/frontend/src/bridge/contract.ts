/**
 * nyxBridge — the SINGLE frontend↔shell frontier.
 *
 * Every interaction the React app has with its host shell (today Tauri; tomorrow
 * Electron) goes through this typed contract. No component imports `@tauri-apps/*`
 * directly: they depend on `NyxBridge`, and a shell-specific adapter
 * (`./tauri.ts`, later `./electron.ts`) implements it. A test fake (`./fake.ts`)
 * implements the same surface so component tests never touch a real IPC layer.
 *
 * This file is the CONTRACT only — interfaces, payload types, error shape, and the
 * documented guarantees (unsubscribe, error serialization, timeouts, per-terminal
 * PTY ordering). It contains no runtime wiring.
 *
 * Scope was derived by inventorying every `@tauri-apps/api` and
 * `@tauri-apps/plugin-*` call-site under `apps/frontend/src` — see
 * `./INVENTORY.md` for the call-site → method/event map.
 *
 * `tauri-plugin-opener`: had NO frontend call-site (no `@tauri-apps/plugin-opener`
 * import, no `openUrl`/`openPath`/`revealItemInDir` anywhere in `src`). Confirmed
 * UNUSED → deliberately OUT of the contract AND removed from the Tauri shell (plugin
 * registration in `lib.rs`, the `tauri-plugin-opener` Cargo dependency, and the
 * `opener:default` capability are all gone). If an "open external URL/path"
 * capability is ever needed, add an `openExternal(target: string)` method here and
 * re-register the plugin.
 */

// ---------------------------------------------------------------------------
// Errors, ordering & timeouts (cross-cutting guarantees)
// ---------------------------------------------------------------------------

/**
 * The serialized error every bridge method rejects with. A shell maps its native
 * failure (a Tauri `invoke` rejection string, an Electron IPC error) into this
 * stable shape so callers never branch on a shell-specific error type.
 *
 * `kind` partitions the failure space so callers can react structurally:
 * - `ipc`      — the transport itself failed (no shell, channel closed, teardown
 *                race). Almost always swallowed by the caller (fail-open).
 * - `command`  — the backend command ran and returned an error (a `Result::Err`
 *                surfaced from Rust); `message` is the backend's error string.
 * - `timeout`  — the call exceeded {@link RequestOptions.timeoutMs} (see below).
 * - `canceled` — the call was aborted via {@link RequestOptions.signal}.
 */
export interface BridgeError {
  readonly kind: "ipc" | "command" | "timeout" | "canceled";
  /** Human-readable message (the backend error string for `command`). */
  readonly message: string;
  /** The command/event name in flight when it failed, when known. */
  readonly source?: string;
  /** The original thrown value, for logging/debugging. Never relied on for control flow. */
  readonly cause?: unknown;
}

/** Type guard: is an unknown caught value a {@link BridgeError}? */
export function isBridgeError(e: unknown): e is BridgeError {
  return (
    typeof e === "object" &&
    e !== null &&
    "kind" in e &&
    "message" in e &&
    typeof (e as BridgeError).message === "string"
  );
}

/** Per-request knobs. All optional; a shell applies sane defaults. */
export interface RequestOptions {
  /**
   * Reject with a `timeout` {@link BridgeError} after this many ms. Default:
   * shell-defined (the Tauri adapter uses no timeout — matching today's bare
   * `invoke` — except where a method documents one). `0`/omitted = no timeout.
   */
  readonly timeoutMs?: number;
  /** Abort the in-flight call; rejects with a `canceled` {@link BridgeError}. */
  readonly signal?: AbortSignal;
}

/**
 * Returned by every `subscribe*` method. Call it to stop receiving events and
 * release the shell-side listener. Idempotent: calling it twice is a safe no-op.
 * MUST be called on teardown to avoid a leaked listener (the Tauri adapter wraps
 * `@tauri-apps/api/event`'s `UnlistenFn`; the Electron adapter removes its IPC
 * handler). May be async under the hood but is exposed as fire-and-forget — the
 * shell swallows unsubscribe failures.
 */
export type Unsubscribe = () => void;

/** A subscription callback. Invoked once per event, in arrival order. */
export type Listener<T> = (payload: T) => void;

// ---------------------------------------------------------------------------
// PTY (interactive terminal) — binary, ordered per terminal
// ---------------------------------------------------------------------------

/**
 * Output chunk for one interactive PTY. `bytes` is RAW terminal output, carried
 * as a byte array (the Tauri adapter receives a JSON `number[]` and normalizes to
 * `Uint8Array`; an Electron adapter passes a `Uint8Array`/`Buffer` straight
 * through). The contract type is `Uint8Array` — adapters own the wire encoding.
 *
 * ORDERING GUARANTEE: for a given `id`, `subscribePtyOutput` callbacks fire in the
 * exact byte order the PTY produced — no reordering, no coalescing that crosses a
 * chunk boundary out of order. Chunks for DIFFERENT ids may interleave freely.
 */
export interface PtyOutput {
  /** The live PTY id (from {@link NyxBridge.ptySpawn}). */
  readonly id: number;
  /** Raw output bytes, in order. */
  readonly bytes: Uint8Array;
}

/** The interactive PTY exited. */
export interface PtyExit {
  readonly id: number;
  /** Natural exit code, or `null` when killed/unknown. */
  readonly code: number | null;
}

/** Options for {@link NyxBridge.ptySpawn}. */
export interface PtySpawnOptions {
  /** Working dir for the shell; `undefined` → backend default (nyx's cwd). */
  readonly cwd?: string;
  readonly cols: number;
  readonly rows: number;
  /**
   * The persistent terminal RECORD id (SQLite `terminals.id`) this session binds
   * to, so the backend maps live `pty_id → record id` for exec-state. `undefined`
   * for a record-less standalone terminal.
   */
  readonly terminalId?: string;
}

// ---------------------------------------------------------------------------
// Command / event payload types reused across the contract
// ---------------------------------------------------------------------------

/** `command://output` — coalesced output for a managed-command instance. */
export interface CommandOutput {
  readonly id: string;
  readonly bytes: Uint8Array;
}

/** `command://state` — a managed-command run-state transition. */
export interface CommandState {
  readonly id: string;
  readonly state: string;
  readonly exitCode: number | null;
}

/** `command://ack` — a managed-command "unseen result" flag was cleared. */
export interface CommandAck {
  readonly id: string;
}

/** `terminal://busy-state` — OS-derived busy/idle transition for a terminal. */
export interface TerminalBusyState {
  readonly id: string;
  readonly busy: boolean;
}

/** `terminal://exec-state` — OSC 133 / exec-state badge transition. */
export interface TerminalExecState {
  readonly id: string;
  readonly state: string;
  readonly exitCode: number | null;
}

// ---------------------------------------------------------------------------
// The bridge surface, grouped by capability
// ---------------------------------------------------------------------------

/**
 * The single typed frontier. A shell adapter implements ALL of it; a component
 * depends only on the slice it needs. Method names mirror the underlying backend
 * command where it clarifies intent. Every request method may reject with a
 * {@link BridgeError}; callers that fail-open (window controls, scrollback
 * persistence) simply `.catch()` it.
 */
export interface NyxBridge {
  /** Low-level escape hatch: invoke a backend command by name. Prefer the typed
   *  methods below; this exists so the inventory's long tail of one-off commands
   *  (project/workspace/command CRUD) is covered without 40 bespoke signatures.
   *  Every typed method is implementable in terms of this. */
  invoke<R>(
    command: BackendCommand,
    args?: Record<string, unknown>,
    opts?: RequestOptions,
  ): Promise<R>;

  /**
   * Low-level subscription escape hatch: deliver the RAW payload of `event` to
   * `listener`, returning an idempotent {@link Unsubscribe}. Mirrors a bare
   * `listen(event, e => listener(e.payload))`. The typed `subscribe*` methods below
   * are the curated surface (they normalize binary + field names); this exists so a
   * call-site that needs the exact backend payload shape (e.g. a per-instance filter
   * on the wire field name) migrates faithfully without re-deriving its type. */
  subscribe<T>(event: BackendEvent, listener: Listener<T>): Promise<Unsubscribe>;

  // --- Interactive PTY (binary, ordered) ---------------------------------
  ptySpawn(opts: PtySpawnOptions): Promise<number>;
  ptyWrite(id: number, data: Uint8Array): Promise<void>;
  ptyResize(id: number, cols: number, rows: number): Promise<void>;
  ptyClose(id: number): Promise<void>;
  /** Subscribe to ordered output for ALL terminals; filter by `id` in the callback. */
  subscribePtyOutput(listener: Listener<PtyOutput>): Promise<Unsubscribe>;
  subscribePtyExit(listener: Listener<PtyExit>): Promise<Unsubscribe>;
  /**
   * FLOW-CONTROL ACKNOWLEDGEMENT. The consumer calls this for `bytes` of a PTY's
   * output once the RENDERER has actually CONSUMED them — i.e. from `xterm.write`'s
   * completion callback, NOT when the chunk merely arrived. The Electron adapter
   * credits the per-terminal backlog so the host resumes the paused Rust reader
   * below the low-water mark (the lossless backpressure loop — PRD / annexe §E). On
   * shells with no flow control (Tauri/the fake) it is a no-op, so the single PTY
   * consumer (`use-pty`) calls it unconditionally and stays shell-agnostic.
   */
  ackPtyOutput(id: number, bytes: number): void;

  // --- Managed commands (services) ---------------------------------------
  subscribeCommandOutput(listener: Listener<CommandOutput>): Promise<Unsubscribe>;
  subscribeCommandState(listener: Listener<CommandState>): Promise<Unsubscribe>;
  subscribeCommandAck(listener: Listener<CommandAck>): Promise<Unsubscribe>;

  // --- Terminal exec/busy state ------------------------------------------
  subscribeTerminalBusyState(listener: Listener<TerminalBusyState>): Promise<Unsubscribe>;
  subscribeTerminalExecState(listener: Listener<TerminalExecState>): Promise<Unsubscribe>;

  // --- Window (frameless chrome) -----------------------------------------
  readonly window: WindowControls;

  // --- Native folder picker ----------------------------------------------
  /** Open the OS folder picker; resolve the chosen absolute path, or `null` if
   *  cancelled. Single selection, directories only. */
  pickDirectory(title?: string): Promise<string | null>;

  // --- Paths --------------------------------------------------------------
  readonly paths: AppPathsBridge;
}

/** The frameless-window control surface (min / toggle-maximize / close + drag). */
export interface WindowControls {
  minimize(): Promise<void>;
  toggleMaximize(): Promise<void>;
  /** Close the window. The CLOSE-INTERCEPTION policy (agent-session warnings) is a
   *  caller concern: the UI first asks the backend via `agent_close_warnings`
   *  (see {@link NyxBridge.invoke}) and only calls `close()` when clear / confirmed. */
  close(): Promise<void>;
  /** Props to spread on the draggable chrome element so the shell can move the
   *  window by it. The Tauri adapter returns `{ "data-tauri-drag-region": true }`;
   *  an Electron adapter returns the CSS `-webkit-app-region: drag` equivalent. */
  dragRegionProps(): Record<string, unknown>;
  /** Subscribe to the OS "window close requested" signal (the user clicked the OS
   *  close button / Alt-F4). The handler runs BEFORE the window is destroyed — used
   *  to flush pending scrollback. Returns an idempotent {@link Unsubscribe}. Resolves
   *  with a no-op unsubscribe when the shell has no such hook (e.g. under tests). */
  onCloseRequested(handler: () => void): Promise<Unsubscribe>;
}

/** Filesystem paths the front asks the shell to resolve. */
export interface AppPathsBridge {
  /** The user's home directory, or `null` if it can't be resolved (caller falls
   *  back to `"."`). Mirrors `@tauri-apps/api/path`'s `homeDir`. */
  homeDir(): Promise<string | null>;
}

/**
 * The closed set of backend command names the front invokes today — every
 * `invoke("...")` call-site under `apps/frontend/src`. A string-literal union so a
 * typo or a dropped command is a compile error, and so a shell adapter can map
 * each name to its transport exhaustively.
 */
export type BackendCommand =
  // pty
  | "pty_spawn"
  | "pty_write"
  | "pty_resize"
  | "pty_close"
  | "register_terminal_pty"
  | "terminal_info"
  // terminals
  | "create_terminal"
  | "list_terminals"
  | "attach_terminal"
  | "auto_attach_terminal"
  | "close_terminal"
  | "set_active"
  | "rename"
  | "reorder"
  | "terminal_exec_mark_read"
  | "persist_scrollback"
  // FEEDBACK #32: persist a terminal's live cwd into its record so a relaunch
  // re-spawns at the LAST directory, not the stale spawn-time cwd.
  | "set_terminal_cwd"
  // projects / workspaces
  | "list_projects"
  | "create_project"
  | "update_project"
  | "delete_project"
  | "projects_reorder"
  | "set_project_collapsed"
  | "set_project_resume_agent_sessions"
  | "list_workspaces"
  | "create_workspace"
  | "rename_workspace"
  | "workspace_delete"
  | "set_workspace_collapsed"
  // managed commands
  | "command_list"
  | "command_create"
  | "command_update"
  | "command_delete"
  | "command_start"
  | "command_stop"
  | "command_relaunch"
  | "command_acknowledge"
  | "command_output"
  | "command_instance_list"
  | "command_import_scripts"
  | "command_import_create"
  | "command_source_refresh"
  | "command_resync_source"
  | "command_unlink_source"
  // agents
  | "agent_active_sessions"
  | "agent_close_warnings"
  // The RUNTIME agent-activity map (the live dot): which terminals are working/waiting
  // or carry a "response ready" notification. NEVER persisted — an in-memory read off the
  // same store the Claude per-turn hooks write. Re-pulled on `agent-sessions://changed`.
  | "agent_activity_snapshot"
  // Clear the focus-aware "response ready" notification for a viewed terminal (the
  // activity analogue of `terminal_exec_mark_read`). `{ terminalId }`. Idempotent.
  | "agent_mark_ready_read"
  // integrations
  | "integration_list"
  | "integration_install"
  | "integration_remove"
  // misc
  | "window_controls_visible";

/**
 * The closed set of backend EVENT channels the front subscribes to — every
 * `listen("...")` call-site. Adapters map each to their transport; the typed
 * `subscribe*` methods above wrap the common ones.
 */
export type BackendEvent =
  | "pty://output"
  | "pty://exit"
  | "command://output"
  | "command://output-cleared"
  | "command://state"
  | "command://ack"
  | "terminal://busy-state"
  | "terminal://exec-state"
  // Per-terminal CPU%/RAM live readings (FEEDBACK #28).
  | "terminal://stats"
  // Coarse "this collection changed → re-fetch it" invalidations (the
  // ChangedTopic axis). The components subscribe to these by their channel string.
  | "commands://changed"
  | "workspaces://changed"
  | "terminals://changed"
  | "agent-sessions://changed";
