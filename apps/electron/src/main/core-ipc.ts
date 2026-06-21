/**
 * MAIN-side relay between the renderer's allowlisted `nyx:core/*` IPC and the
 * dedicated core-host (phase 3, task #8). It is the ONLY bridge the renderer's
 * nyxBridge Electron adapter talks to; it never reaches the host directly.
 *
 * Three jobs:
 *   1. `invoke(command, args)` → a typed `HostRequest` → the host's result. The
 *      renderer speaks the CONTRACT command names (`pty_spawn`, `pty_write`, …); we
 *      translate each to a host-protocol request and return the result in the shape
 *      the contract expects (e.g. `pty_spawn` → the live pty id).
 *   2. `pty-ack` (fire-and-forget) → the host's flow-control credit. The renderer
 *      sends it after `xterm.write` completes; we forward it WITHOUT a reply so the
 *      write path never blocks (the credit loop that bounds memory — annexe §E).
 *   3. host EVENTS → the renderer: every `HostEventPayload` is translated into the
 *      contract's `{ event, payload }` envelope (`pty://output`, `pty://exit`, the
 *      terminal cwd/exec-state, the `*://changed` invalidations) and pushed on the
 *      single `nyx:core/event` channel for the renderer to demux.
 *
 * ORDERING: per-terminal output order is preserved end to end — the host emits
 * `pty-output` in stream order on one channel, Node IPC is ordered, and a single
 * `webContents.send` per event keeps that order to the renderer. Bytes ride as a
 * JSON `number[]` (the contract's wire shape; the adapter normalizes to `Uint8Array`).
 */
import { BrowserWindow, ipcMain, type IpcMainInvokeEvent } from "electron";

import type { CoreHost } from "./core-host";
import { CORE_CHANNELS, type CoreEventEnvelope } from "../shared/ipc";
import { windowControlsVisible } from "./env";
import type {
  HostEventPayload,
  HostRequestPayload,
  PtySpawnResult,
} from "../shared/host-protocol";

/**
 * Contract commands answered LOCALLY in main rather than round-tripped to the core-host —
 * because they have no host-side authority at all (a true N/A), NOT because the host cannot
 * service them.
 *
 * `terminal_info` / `auto_attach_terminal` / `register_terminal_pty` are NO LONGER here:
 * the review found they had been stubbed to benign defaults, which silently KILLED three
 * real features (the sidebar auto-label / shell-suffix, the cwd auto-attach, and the MCP
 * record↔live-PTY liveness binding). They now round-trip to the host's `core-command`
 * router, which serves the REAL nyx-core logic (`nyx_core::proc` for the live cwd/foreground
 * on Linux — `null`/`null` on Windows WITHOUT erroring, so no spam; `decide_attachment` for
 * auto-attach; the synchronous liveness registry for the binding).
 *
 * `agent_close_warnings` is NO LONGER here either: the review found it had been stubbed to
 * `[]`, which silently KILLED the close-warning feature (PRD-5 #6) — the window always closed
 * without the dialog, dropping live agent sessions. Its claimed justification ("a live-runtime
 * scan with no core-host authority") was FALSE: the Tauri impl is a PURE DB read, the same
 * nature as `agent_active_sessions`. It now round-trips to the host's `core-command` router,
 * which serves the REAL `nyx_core::db::close_warning_candidates` + `should_warn_on_close`
 * policy (see `DbCommandTask`), so the renderer gets the real candidates and the dialog fires.
 *
 * Nothing genuinely N/A remains over this transport, so the map is empty (the mechanism is
 * kept for the next true N/A command).
 */
const LOCAL_FALLBACKS: Record<string, () => unknown> = {};

/**
 * Is this a BENIGN core-host rejection raised because the host is shutting down (or
 * already down) at app close? When `stop()` sends `shutdown`, the host exits cleanly
 * (code=0) and `onExit → failAllPending` REJECTS every still-in-flight `invoke` (the
 * renderer keeps polling busy-state / close-warnings during the close). Those rejects
 * are expected teardown noise, NOT real failures — without this guard they escape the
 * `ipcMain.handle` and Electron logs "Error occurred in handler for 'nyx:core/invoke'".
 *
 * We treat a reject as benign when EITHER the host has entered its shutdown lifecycle
 * (`currentState` ∈ {stopping, stopped}) OR the message matches the shutdown-time
 * rejection shapes raised by `core-host.ts`:
 *   - `core-host exited (code=…, signal=…)`  — `failAllPending` on the clean exit
 *   - `core-host is stopping|stopped; new requests are refused` — the `request` guard
 *   - `core-host is not running (state=…)`    — the child is already gone
 * A real error from a READY host (a genuine command failure) does NOT match either
 * arm and still propagates, so the handler keeps surfacing true faults.
 */
function isBenignShutdownReject(coreHost: CoreHost, err: unknown): boolean {
  const state = coreHost.currentState;
  if (state === "stopping" || state === "stopped") return true;
  const message = err instanceof Error ? err.message : String(err);
  return (
    message.includes("core-host exited") ||
    message.includes("new requests are refused") ||
    message.includes("core-host is not running")
  );
}

/**
 * Map a contract command name + args onto a typed host request payload. The PTY surface
 * maps to its dedicated typed requests (the host owns the live pty registry + the
 * lossless flow control); EVERY OTHER contract command — the full DB-backed +
 * managed-command-runtime long tail — maps to a single `core-command` request carrying
 * the contract name + the args serialized to JSON, which the host routes to the right
 * authority (the napi DB dispatcher off the Node loop, or the live command runner).
 *
 * The allowlist is the closed contract `BackendCommand` union: a command the host can't
 * service yet (a PTY-state introspection, a not-yet-ported integration) is NOT a silent
 * no-op here — it rides the same typed seam and the host returns a readable error that
 * surfaces as the adapter's `command` BridgeError. This is the foundational parity gap
 * the review found: phase 3 wired ONLY the PTY surface, so the DB-backed commands never
 * traversed renderer→preload→main→host→DB; now the whole surface does.
 */
function toHostRequest(command: string, args: Record<string, unknown> = {}): HostRequestPayload {
  switch (command) {
    case "pty_spawn":
      return {
        kind: "pty-spawn",
        cols: Number(args.cols),
        rows: Number(args.rows),
        cwd: args.cwd as string | undefined,
        terminalId: args.terminalId as string | undefined,
      };
    case "pty_write":
      return {
        kind: "pty-write",
        ptyId: Number(args.id),
        // The contract passes bytes as number[]; encode to base64 for the host wire.
        dataB64: Buffer.from((args.data as number[]) ?? []).toString("base64"),
      };
    case "pty_resize":
      return {
        kind: "pty-resize",
        ptyId: Number(args.id),
        cols: Number(args.cols),
        rows: Number(args.rows),
      };
    case "pty_close":
      return { kind: "pty-close", ptyId: Number(args.id) };
    default:
      // The non-PTY surface: forward the contract command name + JSON args to the host,
      // which routes it (DB dispatcher vs managed-command runner) and replies with the
      // result already in the contract shape. JSON keeps the wire shape identical to the
      // Tauri `invoke` the front already speaks, so no per-command relay code is needed.
      return { kind: "core-command", command, argsJson: JSON.stringify(args ?? {}) };
  }
}

/**
 * Translate a host event into the contract event envelope the renderer demuxes.
 * Returns `null` for events with no renderer-facing mapping (e.g. `ready`/`fatal`,
 * which drive main's own lifecycle and are surfaced separately).
 */
function toEnvelope(payload: HostEventPayload): CoreEventEnvelope | null {
  switch (payload.kind) {
    case "pty-output":
      return {
        event: "pty://output",
        // Decode base64 → number[] (the contract's JSON wire shape). The renderer's
        // adapter normalizes to Uint8Array AND acks `bytes` after xterm.write.
        payload: { id: payload.ptyId, bytes: Array.from(Buffer.from(payload.dataB64, "base64")) },
      };
    case "pty-exit":
      return { event: "pty://exit", payload: { id: payload.ptyId, code: payload.code } };
    case "pty-cwd":
      // Surfaced for the portable cwd source (auto-attach). Keyed by both ids so a
      // record-less terminal still carries its live pty id.
      return {
        event: "pty://cwd",
        payload: { id: payload.ptyId, terminalId: payload.terminalId, cwd: payload.cwd },
      };
    case "pty-exec-state":
      // The contract's `terminal://exec-state` is keyed by the PERSISTENT terminal id
      // (the host only emits this AFTER persisting). The renderer's raw subscriber
      // (`use-terminals.ts`) destructures the SNAKE_CASE Tauri wire shape
      // (`terminal_id`, `exit_code`, `unread`), so we emit that exact shape for
      // cross-shell parity — NOT the camelCase typed-method shape.
      return {
        event: "terminal://exec-state",
        payload: {
          terminal_id: payload.terminalId,
          state: payload.state,
          exit_code: payload.exitCode,
          unread: payload.unread,
          updated_at: payload.updatedAt,
        },
      };
    case "pty-busy-state":
      // The OS-derived running-dot signal (PRD-5 #1), keyed by the PERSISTENT id. The
      // renderer's raw subscriber reads the snake_case `terminal_id` + `busy`.
      return {
        event: "terminal://busy-state",
        payload: { terminal_id: payload.terminalId, busy: payload.busy },
      };
    case "command-state":
      // A managed-command run-state / ack / output-cleared transition, mapped to the
      // matching Tauri event name. The renderer filters on `instanceId` (camelCase,
      // load-bearing — same as the PTY events), so we emit that exact shape for parity.
      switch (payload.event) {
        case "ack":
          return { event: "command://ack", payload: { instanceId: payload.instanceId } };
        case "output-cleared":
          return {
            event: "command://output-cleared",
            payload: { instanceId: payload.instanceId },
          };
        default:
          return {
            event: "command://state",
            payload: {
              instanceId: payload.instanceId,
              state: payload.state,
              code: payload.exitCode,
            },
          };
      }
    case "command-output":
      // Coalesced managed-command output for one instance (parity with the Tauri
      // `command://output`). Decode base64 → number[] (the contract's JSON wire shape);
      // the renderer's output panel filters on `instanceId` and writes the bytes.
      return {
        event: "command://output",
        payload: {
          instanceId: payload.instanceId,
          bytes: Array.from(Buffer.from(payload.dataB64, "base64")),
        },
      };
    case "changed":
      // The coarse "collection changed → re-fetch" invalidations.
      return { event: `${payload.topic}://changed`, payload: undefined };
    case "ready":
    case "fatal":
      // Lifecycle events — handled by main's CoreHost.onState/onEvent, not relayed
      // to the renderer as a contract event.
      return null;
  }
}

/**
 * Register the core IPC relay. Call once after `app` is ready. `coreHost` is the
 * live host manager; events are pushed to whatever window the `getWindow` resolver
 * returns at emit time (so a window recreated on macOS `activate` still receives
 * events).
 */
export function registerCoreIpc(coreHost: CoreHost, getWindow: () => BrowserWindow | null): void {
  // 1. invoke(command, args) → host request → result.
  ipcMain.handle(
    CORE_CHANNELS.invoke,
    async (_event: IpcMainInvokeEvent, command: string, args?: Record<string, unknown>) => {
      // `window_controls_visible` is a WINDOW-chrome concern, not a core-host command:
      // answer it from the same OS-env check the `nyxWindow.controlsVisible` bridge uses
      // (parity with the Tauri command), so it never bounces off the DB dispatcher.
      if (command === "window_controls_visible") return windowControlsVisible();
      // Commands the host can't service over this transport yet, whose renderer call-site
      // fails open: answer locally with the benign default so the dev console isn't
      // flooded with a child_process stack on every (per-second) poll. See LOCAL_FALLBACKS.
      const fallback = LOCAL_FALLBACKS[command];
      if (fallback) return fallback();

      const req = toHostRequest(command, args);
      let result: unknown;
      try {
        result = await coreHost.request<unknown>(req);
      } catch (err) {
        // At app close the host shuts down (code=0) and rejects any invoke still in
        // flight; swallow that BENIGN teardown reject so Electron does not log
        // "Error occurred in handler for 'nyx:core/invoke'". The renderer call-sites
        // already fail open during close (e.g. fetchCloseWarnings catches → []). A real
        // error from a ready host is NOT benign and still propagates to the renderer.
        if (isBenignShutdownReject(coreHost, err)) return undefined;
        throw err;
      }
      // Shape the result the way the contract command expects.
      if (command === "pty_spawn") return (result as PtySpawnResult).ptyId;
      return result;
    },
  );

  // 2. pty-ack (fire-and-forget) → host flow-control credit. `notify` registers no
  //    pending entry and awaits no reply, so a high-rate ack stream never blocks
  //    xterm.write nor grows main's pending map.
  ipcMain.on(CORE_CHANNELS.ptyAck, (_event, ptyId: number, bytes: number) => {
    coreHost.notify({ kind: "pty-ack", ptyId, bytes });
  });

  // 3. host events → renderer. Translate + push on the single event channel,
  //    preserving per-terminal order (one send per event, Node IPC is ordered).
  coreHost.onEvent((payload) => {
    const envelope = toEnvelope(payload);
    if (!envelope) return;
    const win = getWindow();
    if (win && !win.isDestroyed()) {
      win.webContents.send(CORE_CHANNELS.event, envelope);
    }
  });
}
