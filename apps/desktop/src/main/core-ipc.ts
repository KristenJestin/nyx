/**
 * MAIN-side relay between the renderer's allowlisted `nyx:core/*` IPC and the supervised
 * `nyx-sidecar` (Phase 3, G0). It is the ONLY bridge the renderer's nyxBridge Electron
 * adapter talks to; the renderer never reaches the sidecar (nor any Node) directly.
 *
 * Three jobs:
 *   1. `invoke(command, args)` → the Sidecar maps the contract command name to a typed pipe
 *      request and returns the result in the shape the contract expects.
 *   2. `pty-ack` (fire-and-forget) → the sidecar's flow-control credit. The renderer sends it
 *      after `xterm.write` completes; we forward it WITHOUT a reply so the write path never
 *      blocks (the credit loop that bounds memory — PDR-0007 / annexe §E).
 *   3. sidecar EVENTS → the renderer: every event is mapped (in the Sidecar) to the
 *      contract's `{ event, payload }` envelope and pushed on the single `nyx:core/event`
 *      channel for the renderer to demux.
 *
 * ORDERING: per-terminal output order is preserved end to end — the sidecar emits
 * `terminal_output` in stream order on one pipe, Node IPC is ordered, and a single
 * `webContents.send` per event keeps that order to the renderer.
 *
 * This REPLACES the transitional core-host relay: the runtime authority is now the Rust
 * sidecar over the MessagePack pipe (no `nyx-napi`, no Node `core-host`).
 */
import { BrowserWindow, ipcMain, type IpcMainInvokeEvent } from "electron";

import { CORE_CHANNELS } from "../shared/ipc";
import type { Sidecar } from "./sidecar";

/**
 * Is this a BENIGN sidecar rejection raised because it is shutting down (or already down) at
 * app close? When `stop()` runs, the sidecar exits cleanly and `onExit` REJECTS every
 * still-in-flight `invoke` (the renderer keeps polling busy-state / close-warnings during the
 * close). Those rejects are expected teardown noise, not real failures — without this guard
 * they escape the `ipcMain.handle` and Electron logs an error. A real error from a READY
 * sidecar (a genuine command failure) does NOT match and still propagates.
 */
function isBenignShutdownReject(sidecar: Sidecar, err: unknown): boolean {
  const state = sidecar.currentState;
  if (state === "stopping" || state === "stopped") return true;
  const message = err instanceof Error ? err.message : String(err);
  return (
    message.includes("sidecar exited") ||
    message.includes("new requests are refused") ||
    message.includes("sidecar is not running")
  );
}

/**
 * Register the core IPC relay. Call once after `app` is ready. `sidecar` is the live manager;
 * events are pushed to whatever window the `getWindow` resolver returns at emit time (so a
 * window recreated on macOS `activate` still receives them).
 */
export function registerCoreIpc(sidecar: Sidecar, getWindow: () => BrowserWindow | null): void {
  // 1. invoke(command, args) → the Sidecar's contract dispatch → result.
  ipcMain.handle(
    CORE_CHANNELS.invoke,
    async (_event: IpcMainInvokeEvent, command: string, args?: Record<string, unknown>) => {
      try {
        return await sidecar.invoke(command, args ?? {});
      } catch (err) {
        // At app close the sidecar shuts down and rejects any invoke still in flight; swallow
        // that BENIGN teardown reject so Electron does not log a handler error. The renderer
        // call-sites already fail open during close. A real error from a ready sidecar is NOT
        // benign and still propagates.
        if (isBenignShutdownReject(sidecar, err)) return undefined;
        throw err;
      }
    },
  );

  // 2. pty-ack (fire-and-forget) → sidecar flow-control credit. `ackPty` registers no pending
  //    entry and awaits no reply, so a high-rate ack stream never blocks xterm.write.
  ipcMain.on(CORE_CHANNELS.ptyAck, (_event, ptyId: number, bytes: number) => {
    sidecar.ackPty(ptyId, bytes);
  });

  // 3. sidecar events → renderer. The Sidecar already maps each event to the contract
  //    envelope; push it on the single event channel, preserving per-terminal order (one send
  //    per event, the pipe + Node IPC are ordered).
  sidecar.onEvent((envelope) => {
    const win = getWindow();
    if (win && !win.isDestroyed()) {
      win.webContents.send(CORE_CHANNELS.event, envelope);
    }
  });
}
