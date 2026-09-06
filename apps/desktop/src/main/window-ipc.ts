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
import { copyFileSync, existsSync, mkdirSync, readdirSync, rmSync, statSync } from "node:fs";

import { WINDOW_CHANNELS } from "../shared/ipc";
import { windowControlsVisible } from "./env";
import {
  adoptProjectImage,
  discardProjectImage,
  PROJECT_IMAGE_EXTENSIONS,
  projectImagesDir,
  pruneProjectImages,
  type ImageFs,
} from "./project-image";
import { resolveDataDir } from "./sidecar";
import { confirmWindowClose } from "./window";

/**
 * The two ways the picture handlers reach the outside world, taken as ARGUMENTS rather than
 * reached for — the same `ImageFs` / `resolveDataDirFrom` reflex `project-image.ts` states in its
 * own header. A spec can then drive the registered handler end to end without a real filesystem
 * and without a real userData dir, instead of replacing `node:fs` for the whole test process
 * (which is global under bun's `mock.module`, and would hand an imaginary filesystem to any
 * neighbouring spec that read the real one).
 */
export interface ProjectImageDeps {
  /** The slice of `node:fs` the copies are made with. */
  readonly fs: ImageFs;
  /** Where the copies live — resolved LATE, per call, so nothing pins a path at import time. */
  readonly imagesDir: () => string;
}

/**
 * The real filesystem and this install's real `userData/project-icons` — the production wiring.
 *
 * Exported for the spec that pins it: every production call site is `registerWindowIpc()` with no
 * argument, so this object IS the path the app copies to, and the default parameter is the only
 * thing standing between it and the handlers. Nothing else imports it.
 */
export const realProjectImageDeps: ProjectImageDeps = {
  fs: { mkdirSync, readdirSync, statSync, copyFileSync, rmSync, existsSync },
  // Through `resolveDataDir`, NOT `app.getPath("userData")` directly: that is the ONE answer to
  // "where does this run keep its data", and it honours the `NYX_DATA_DIR` override the e2e
  // harness pins PER SPEC. Reading `userData` here instead would put the SQLite database under
  // the pin and the pictures outside it — two data roots for one run, and specs sharing a
  // picture directory they each believe they own.
  imagesDir: () => projectImagesDir(resolveDataDir()),
};

/** Resolve the BrowserWindow that sent an IPC message (or undefined if gone). */
function senderWindow(event: IpcMainInvokeEvent): BrowserWindow | null {
  return BrowserWindow.fromWebContents(event.sender);
}

/**
 * Register the window-control IPC handlers. Call once, after `app` is ready and
 * before/at window creation. Idempotency: `ipcMain.handle` throws if a channel is
 * double-registered, so this is called exactly once from the main entrypoint.
 *
 * `deps` defaults to the real filesystem and the real userData dir, so every production call site
 * stays `registerWindowIpc()`; only a spec ever passes anything.
 */
export function registerWindowIpc(deps: ProjectImageDeps = realProjectImageDeps): void {
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

  // The renderer's close-guard says "go ahead" (no live agent session, or the user
  // confirmed): proceed with the real close, bypassing the `close` withhold guard.
  ipcMain.handle(WINDOW_CHANNELS.confirmClose, (event) => {
    const win = senderWindow(event);
    if (win) confirmWindowClose(win);
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

  // THE PROJECT PICTURE. Same shape as the folder picker above — the dialog is a
  // main-process concern — plus the COPY that makes the choice durable: what the
  // renderer gets back, and what lands in `projects.image_path`, is a file inside the
  // app's own data dir, so moving or deleting the original changes nothing.
  //
  // A REFUSAL THROWS. `ipcMain.handle` turns it into a rejection carrying the message,
  // which is what lets the settings modal show the real reason in line under the button
  // that produced it — a `role="alert"` node, not a toast (`project-identity.tsx`);
  // resolving `null` would make "your 40 MB scan was refused" indistinguishable from
  // "you pressed Cancel". Cancelling is the only `null`.
  ipcMain.handle(
    WINDOW_CHANNELS.pickProjectImage,
    async (event, projectId: string): Promise<string | null> => {
      const win = senderWindow(event);
      const opts: Electron.OpenDialogOptions = {
        properties: ["openFile"],
        title: "Choose a project picture",
        filters: [{ name: "Images", extensions: [...PROJECT_IMAGE_EXTENSIONS] }],
      };
      const result = win
        ? await dialog.showOpenDialog(win, opts)
        : await dialog.showOpenDialog(opts);
      if (result.canceled || result.filePaths.length === 0) return null;
      return adoptProjectImage(deps.fs, deps.imagesDir(), projectId, result.filePaths[0]!);
    },
  );

  ipcMain.handle(
    WINDOW_CHANNELS.discardProjectImage,
    (_event, projectId: string, keep?: string): void => {
      discardProjectImage(deps.fs, deps.imagesDir(), projectId, keep);
    },
  );

  // The boot sweep. The EMPTY-LIST refusal lives in `pruneProjectImages` itself rather
  // than here, so it holds for every caller — this handler is deliberately a thin
  // relay of a rule written once.
  ipcMain.handle(
    WINDOW_CHANNELS.pruneProjectImages,
    (_event, liveProjectIds: string[]): number =>
      pruneProjectImages(deps.fs, deps.imagesDir(), liveProjectIds ?? []).length,
  );
}
