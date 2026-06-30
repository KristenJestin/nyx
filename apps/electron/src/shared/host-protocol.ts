/**
 * The typed wire protocol between the Electron MAIN process and the dedicated
 * core-host (the `ELECTRON_RUN_AS_NODE` Node process that owns `nyx-napi` and the
 * PTY). Both ends import this module so the message shapes — and the request⇄reply
 * correlation — are a single compile-time contract.
 *
 * Transport: Node's built-in IPC channel (`process.send` / `process.on('message')`),
 * enabled by the `'ipc'` stdio slot. The protocol itself is transport-agnostic — it
 * is just the message union below.
 *
 * Two directions, two kinds:
 *   - **request / response** (`HostRequest` → `HostResponse`) — correlated by a
 *     monotonic `id`. Main asks, host replies with the SAME id (ok+result | error).
 *   - **events** (`HostEvent`) — host→main, UNcorrelated, fire-and-forget. This is
 *     the `EventSink` frontier on the wire: `pty-output`, `pty-exit`, `pty-cwd`,
 *     `pty-exec-state`, `changed`, and the host's own lifecycle signals
 *     (`ready`, `fatal`).
 *
 * Phase 3 scope: the FULL interactive-PTY surface — spawn/write/resize/close keyed
 * by a live pty id, the ordered output/exit/cwd/exec-state events, and the LOSSLESS
 * FLOW-CONTROL ack (`pty-ack`, renderer→main→host) that credits each terminal's
 * backlog so the host can pause/resume the Rust reader. The DB + command + MCP
 * surface stays phase 5.
 */

// ---------------------------------------------------------------------------
// Requests (main → host), each correlated by `id`.
// ---------------------------------------------------------------------------

/** Liveness + ABI probe: the host replies with the loaded nyx-core/napi versions. */
export interface PingRequest {
  kind: "ping";
}

/**
 * Spawn an interactive shell PTY. The host allocates the live `ptyId` (the napi
 * `NyxPty.id()`), starts streaming `pty-output` for it, and returns the id. The
 * renderer keys its xterm by this id (exactly like the Tauri `pty_spawn`).
 */
export interface PtySpawnRequest {
  kind: "pty-spawn";
  cols: number;
  rows: number;
  /** Working dir for the shell; omitted → the host's cwd. */
  cwd?: string;
  /** The persistent terminal RECORD id (SQLite `terminals.id`), for exec-state. */
  terminalId?: string;
}

/** Write bytes (base64) to a live PTY. */
export interface PtyWriteRequest {
  kind: "pty-write";
  ptyId: number;
  /** base64-encoded bytes (JSON-safe binary). */
  dataB64: string;
}

/** Resize a live PTY. */
export interface PtyResizeRequest {
  kind: "pty-resize";
  ptyId: number;
  cols: number;
  rows: number;
}

/** Close (kill) a live PTY. */
export interface PtyCloseRequest {
  kind: "pty-close";
  ptyId: number;
}

/**
 * FLOW-CONTROL ACK (renderer→main→host). The renderer reports that `bytes` of a
 * PTY's output have been consumed by xterm (acknowledged after `xterm.write`'s
 * completion callback). The host subtracts this from the terminal's unacked backlog
 * and resumes the Rust reader once it drops below the low-water mark — the credit
 * loop that bounds memory and keeps the stream lossless (PRD / annexe §E).
 *
 * Sent as a fire-and-forget request (no response needed); it is NOT correlated to a
 * reply so it never blocks the renderer's write path.
 */
export interface PtyAckRequest {
  kind: "pty-ack";
  ptyId: number;
  /** Number of output bytes xterm has now consumed for this PTY. */
  bytes: number;
}

/**
 * A NON-PTY backend command (PRD-5 review — the full nyxBridge request surface end to
 * end). The renderer's Electron adapter speaks the contract `BackendCommand` names
 * (`list_terminals`, `create_terminal`, `list_projects`, `command_start`, …); main
 * forwards each as this single typed request, and the host routes it to the right
 * authority:
 *   - DB-backed commands → `NyxCore.dbCommand(command, argsJson)` (an AsyncTask off the
 *     Node loop) → the JSON result string the host parses back to the contract shape;
 *   - the managed-command RUNTIME (`command_start`/`stop`/`relaunch`/`output`/
 *     `acknowledge`) → the live `CommandManager` runner that owns the off-screen PTYs.
 *
 * `argsJson` is the contract args serialized to JSON (`"{}"` when none) — JSON keeps the
 * wire shape identical to the Tauri `invoke` the front already speaks, so no per-command
 * host code is needed. The reply is a {@link CoreCommandResult}.
 */
export interface CoreCommandRequest {
  kind: "core-command";
  /** The contract command name (a `BackendCommand`). */
  command: string;
  /** The contract args, serialized to JSON (`"{}"` for the no-arg commands). */
  argsJson: string;
}

/** Begin an ordered shutdown (snapshot, stop PTYs/commands, then exit). */
export interface ShutdownRequest {
  kind: "shutdown";
}

/**
 * TEST-ONLY: force the host to crash (abrupt non-zero exit, no ordered shutdown), so
 * the lifecycle's crash-detection / single-restart / degraded policy can be
 * exercised deterministically. Guarded by `NYX_HOST_ALLOW_CRASH=1` in the host; a
 * production host ignores it. Never sent by main in normal operation.
 */
export interface CrashRequest {
  kind: "__crash";
}

/** The discriminated union of every request payload. */
export type HostRequestPayload =
  | PingRequest
  | PtySpawnRequest
  | PtyWriteRequest
  | PtyResizeRequest
  | PtyCloseRequest
  | PtyAckRequest
  | CoreCommandRequest
  | ShutdownRequest
  | CrashRequest;

/** A request envelope: payload + a correlation id the response echoes. */
export interface HostRequest {
  type: "req";
  id: number;
  payload: HostRequestPayload;
}

// ---------------------------------------------------------------------------
// Responses (host → main), echoing the request `id`.
// ---------------------------------------------------------------------------

/** A successful reply: `result` is request-kind-specific (or null). */
export interface HostResponseOk {
  type: "res";
  id: number;
  ok: true;
  result: unknown;
}

/** A failed reply: a readable `error` string (never an infinite hang). */
export interface HostResponseErr {
  type: "res";
  id: number;
  ok: false;
  error: string;
}

export type HostResponse = HostResponseOk | HostResponseErr;

/** The result shape of a `pty-spawn` (the live id the renderer keys its xterm by). */
export interface PtySpawnResult {
  ptyId: number;
}

/**
 * The result of a {@link CoreCommandRequest}: the command's value, ALREADY in the
 * contract shape (the host parsed the napi JSON-string result, or built the runtime
 * status). Typed as `unknown` because the value is command-specific (a `Terminal[]`, a
 * `Project[]`, a `CommandStatus` string, `null`, …); main returns it verbatim to the
 * renderer's `invoke`, which casts it to the caller's expected `R`.
 */
export type CoreCommandResult = unknown;

/** The result shape of a `ping` (the ABI/liveness proof main logs). */
export interface PingResult {
  /** nyx-core version (from the napi `version()` — proves the `.node` loaded). */
  coreVersion: string;
  /** Electron version embedding this host's Node (e.g. "42.4.1"). */
  electron: string;
  /** Node version embedded by that Electron. */
  node: string;
  /** Node ABI (`process.versions.modules`) the `.node` was matched against. */
  abi: string;
  /** True iff this process is pure Node (no Chromium renderer/browser runtime). */
  nodePure: boolean;
  /** The resolved data dir (proves the AppPaths frontier honored userData/NYX_DATA_DIR). */
  dataDir: string;
  /** The resolved resource dir, if any (unpacked-resources frontier). */
  resourceDir: string | null;
}

// ---------------------------------------------------------------------------
// Events (host → main), uncorrelated.
// ---------------------------------------------------------------------------

/** Host finished boot and is ready to serve (carries the same proof as PingResult). */
export interface ReadyEvent {
  kind: "ready";
  info: PingResult;
}

/**
 * A chunk of interactive-terminal output (EventSink.pty_output on the wire).
 * ORDERED per `ptyId`: the host emits these in exact stream order, capped at 64 KiB
 * each. Carries the byte length so main/renderer can credit the flow-control loop
 * without decoding base64 first.
 */
export interface PtyOutputEvent {
  kind: "pty-output";
  ptyId: number;
  dataB64: string;
  /** Decoded byte length of this chunk (for the flow-control accounting). */
  bytes: number;
}

/** The interactive terminal exited (EventSink.pty_exit on the wire). */
export interface PtyExitEvent {
  kind: "pty-exit";
  ptyId: number;
  code: number | null;
}

/** A decoded OSC 7 cwd for a live PTY (portable cwd source). */
export interface PtyCwdEvent {
  kind: "pty-cwd";
  ptyId: number;
  terminalId: string | null;
  cwd: string;
}

/**
 * A PERSISTED exec-state transition for a terminal RECORD (PRD-5 task #1). Unlike the
 * raw OSC 133 callback, this is emitted AFTER the host has written the transition to
 * the DB, so it carries the persisted `unread` flag + the stamped `updatedAt` — parity
 * with the Tauri `persist_and_emit_exec_state`. Keyed by the PERSISTENT `terminalId`.
 */
export interface PtyExecStateEvent {
  kind: "pty-exec-state";
  terminalId: string;
  /** `idle` | `running` | `success` | `error` (the DB CHECK vocabulary). */
  state: string;
  exitCode: number | null;
  /** Whether this is an UNREAD settled notification (the persisted flag). */
  unread: boolean;
  /** Epoch-ms of the transition (the persisted `exec_state_updated_at`). */
  updatedAt: number;
}

/**
 * An OS-derived BUSY/idle TRANSITION for a terminal RECORD (PRD-5 task #1, decision
 * 1-B). Emitted by the host's busy-state poll loop on a CHANGE only (never every
 * tick), keyed by the PERSISTENT `terminalId`. `busy` is the kernel-truthful
 * "a command is running in the foreground" bit — the AUTHORITY for the running dot,
 * independent of OSC 133.
 */
export interface PtyBusyStateEvent {
  kind: "pty-busy-state";
  terminalId: string;
  busy: boolean;
}

/**
 * A per-terminal process-tree resource reading for a terminal RECORD (FEEDBACK #28).
 * Emitted by the host's stats poll loop on a visible CHANGE, keyed by the PERSISTENT
 * `terminalId`. `cpuPct` is the summed CPU% of the shell + all descendants (per single
 * core, so it can exceed 100); `memBytes` is the summed RSS in bytes. Cross-platform via
 * `sysinfo` (Linux/macOS/Windows).
 */
export interface PtyStatsEvent {
  kind: "pty-stats";
  terminalId: string;
  cpuPct: number;
  memBytes: number;
}

/**
 * A managed-command run-state / ack / output-cleared transition (parity with the Tauri
 * `command://state` / `command://ack` / `command://output-cleared` events). The host
 * forwards the runner's `CommandStateEvent`; main maps `kind` to the matching renderer
 * event name, keyed by the persistent `instanceId` (`command_instances.id`).
 */
export interface CommandStateHostEvent {
  kind: "command-state";
  /** `"state"` | `"ack"` | `"output-cleared"`. */
  event: string;
  instanceId: string;
  /** `idle|running|success|error` for a `"state"` event; empty otherwise. */
  state: string;
  exitCode: number | null;
}

/**
 * A coalesced managed-command output chunk (parity with the Tauri `command://output`).
 * Bytes are base64-encoded for JSON transport (decoded back to a Buffer by main),
 * keyed by `instanceId`.
 */
export interface CommandOutputHostEvent {
  kind: "command-output";
  instanceId: string;
  dataB64: string;
}

/** A coarse "X changed → re-fetch" invalidation (EventSink.changed on the wire). */
export interface ChangedEvent {
  kind: "changed";
  topic: "terminals" | "workspaces" | "commands" | "agent-sessions";
}

/** The host hit an unrecoverable error (e.g. the `.node` failed to load). */
export interface FatalEvent {
  kind: "fatal";
  error: string;
}

export type HostEventPayload =
  | ReadyEvent
  | PtyOutputEvent
  | PtyExitEvent
  | PtyCwdEvent
  | PtyExecStateEvent
  | PtyBusyStateEvent
  | PtyStatsEvent
  | CommandStateHostEvent
  | CommandOutputHostEvent
  | ChangedEvent
  | FatalEvent;

/** An event envelope (no id — fire-and-forget). */
export interface HostEvent {
  type: "evt";
  payload: HostEventPayload;
}

/** Any message crossing the channel, in either direction. */
export type HostMessage = HostRequest | HostResponse | HostEvent;

/** The boot parameters main passes to the host (resolved in full-Electron main). */
export interface HostBootConfig {
  /** Resolved writable data dir (`userData`, honoring `NYX_DATA_DIR`). */
  dataDir: string;
  /** Resolved read-only resource dir (unpacked, outside asar), or null in dev/bare. */
  resourceDir: string | null;
}

// ---------------------------------------------------------------------------
// Flow-control constants (PRD frozen decision / annexe §E) — shared so the host
// (which enforces them) and any test/inspection agree on the exact thresholds.
// ---------------------------------------------------------------------------

/** Max output bytes per emitted chunk. A coalesced flood chunk is split into pieces
 * no larger than this before crossing the IPC, so the backlog is bounded to
 * `HIGH_WATER + CHUNK_BYTES` and one event never carries a multi-MiB payload. */
export const CHUNK_BYTES = 64 * 1024;

/** High-water mark: at or above this many UNACKED output bytes for a terminal, the
 * host pauses that terminal's Rust reader (OS backpressure to the child). */
export const HIGH_WATER = 512 * 1024;

/** Low-water mark: once the unacked backlog drops below this, the host resumes the
 * reader. The hysteresis gap (HIGH→LOW) avoids pause/resume thrashing under flood. */
export const LOW_WATER = 128 * 1024;
