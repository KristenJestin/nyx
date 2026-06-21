/**
 * Main-process handlers for the allowlisted `nyx:window/*` IPC channels.
 *
 * Each `ipcMain.handle` is the request/response counterpart of a `nyxWindow.*` call
 * the preload exposes. Handlers resolve the window from the event's `WebContents`
 * (`BrowserWindow.fromWebContents`) rather than capturing a window reference, so the
 * same handlers serve correctly even though phase 2 has a single window.
 *
 * Registering on `ipcMain` (a global) once at boot is the documented pattern; the
 * allowlist (channel constants in ../shared/ipc) guarantees only these four channels
 * exist — the renderer physically cannot reach an unregistered channel.
 */
import { app, BrowserWindow, dialog, ipcMain, type IpcMainInvokeEvent } from "electron";
import { homedir } from "node:os";

import { WINDOW_CHANNELS } from "../shared/ipc";
import { windowControlsVisible } from "./env";

/** Resolve the BrowserWindow that sent an IPC message (or undefined if gone). */
function senderWindow(event: IpcMainInvokeEvent): BrowserWindow | null {
  return BrowserWindow.fromWebContents(event.sender);
}

/**
 * Register the window-control IPC handlers. Call once, after `app` is ready and
 * before/at window creation. Idempotency: `ipcMain.handle` throws if a channel is
 * double-registered, so this is called exactly once from the main entrypoint.
 */
export function registerWindowIpc(): void {
  ipcMain.handle(WINDOW_CHANNELS.minimize, (event) => {
    senderWindow(event)?.minimize();
  });

  ipcMain.handle(WINDOW_CHANNELS.toggleMaximize, (event): boolean => {
    const win = senderWindow(event);
    if (!win) return false;
    if (win.isMaximized()) {
      win.unmaximize();
      return false;
    }
    win.maximize();
    return true;
  });

  ipcMain.handle(WINDOW_CHANNELS.close, (event) => {
    senderWindow(event)?.close();
  });

  // Resolved from the OS env in the main process (the renderer can't read it).
  ipcMain.handle(WINDOW_CHANNELS.controlsVisible, (): boolean => windowControlsVisible());

  // Native folder picker (main-process concern; the sandboxed renderer can't open a
  // dialog). Single selection, directories only — mirrors the Tauri plugin-dialog.
  ipcMain.handle(
    WINDOW_CHANNELS.pickDirectory,
    async (event, title?: string): Promise<string | null> => {
      const win = senderWindow(event);
      const opts: Electron.OpenDialogOptions = { properties: ["openDirectory"], title };
      const result = win
        ? await dialog.showOpenDialog(win, opts)
        : await dialog.showOpenDialog(opts);
      if (result.canceled || result.filePaths.length === 0) return null;
      return result.filePaths[0];
    },
  );

  // Home directory (main-process concern; the renderer has no Node). `app.getPath`
  // throws if 'home' is somehow unavailable, so fall back to os.homedir().
  ipcMain.handle(WINDOW_CHANNELS.homeDir, (): string | null => {
    try {
      return app.getPath("home") || homedir() || null;
    } catch {
      return homedir() || null;
    }
  });
}
