/**
 * Preload — the ONE allowlisted bridge between the hardened renderer and the main
 * process. With `contextIsolation: true` + `nodeIntegration: false` + `sandbox:
 * true`, the renderer has NO `require`, NO `ipcRenderer`, NO Node globals. The only
 * thing it can touch is the `window.nyxWindow` object this script installs via
 * `contextBridge.exposeInMainWorld` — a closed, typed allowlist (see ../shared/ipc).
 *
 * Each method is a thin `ipcRenderer.invoke` of a fixed `nyx:window/*` channel
 * string. The renderer can NOT pass an arbitrary channel name: the channel constants
 * are baked in here, so there is no string the page could supply to reach an
 * unintended `ipcMain` handler. This is the official contextBridge pattern from
 * electronjs.org/docs/latest/tutorial/security (exposeInMainWorld + ipcRenderer).
 */
import { contextBridge, ipcRenderer, type IpcRendererEvent } from "electron";

import {
  CORE_CHANNELS,
  WINDOW_CHANNELS,
  type CoreEventEnvelope,
  type NyxCoreApi,
  type NyxWindowApi,
} from "../shared/ipc";

/** The allowlisted window-control API handed to the renderer. */
const nyxWindow: NyxWindowApi = {
  minimize: () => ipcRenderer.invoke(WINDOW_CHANNELS.minimize) as Promise<void>,
  toggleMaximize: () =>
    ipcRenderer.invoke(WINDOW_CHANNELS.toggleMaximize) as Promise<boolean>,
  close: () => ipcRenderer.invoke(WINDOW_CHANNELS.close) as Promise<void>,
  controlsVisible: () =>
    ipcRenderer.invoke(WINDOW_CHANNELS.controlsVisible) as Promise<boolean>,
  pickDirectory: (title) =>
    ipcRenderer.invoke(WINDOW_CHANNELS.pickDirectory, title) as Promise<string | null>,
  homeDir: () => ipcRenderer.invoke(WINDOW_CHANNELS.homeDir) as Promise<string | null>,
};

/**
 * The allowlisted CORE bridge (phase 3). Every PTY / backend interaction the
 * renderer makes rides these three fixed `nyx:core/*` channels — the renderer can
 * NOT name an arbitrary IPC channel: the constants are baked in here, so a page
 * cannot reach an unintended `ipcMain` handler. `invoke` round-trips a host request;
 * `ptyAck` is fire-and-forget (no reply, so it never blocks xterm.write); `onEvent`
 * subscribes to the single relayed host-event channel and demuxes by name.
 */
const nyxCore: NyxCoreApi = {
  invoke: (command, args) =>
    ipcRenderer.invoke(CORE_CHANNELS.invoke, command, args) as Promise<unknown>,
  ptyAck: (ptyId, bytes) => ipcRenderer.send(CORE_CHANNELS.ptyAck, ptyId, bytes),
  onEvent: (handler) => {
    // Wrap so the page-supplied handler never sees the raw `IpcRendererEvent`
    // (which exposes `sender`); it receives only the demuxable envelope.
    const listener = (_e: IpcRendererEvent, envelope: CoreEventEnvelope) => handler(envelope);
    ipcRenderer.on(CORE_CHANNELS.event, listener);
    return () => ipcRenderer.removeListener(CORE_CHANNELS.event, listener);
  },
};

// Expose under single namespaced globals. `exposeInMainWorld` deep-freezes each
// object across the isolation boundary, so the page can call these methods but can
// neither mutate them nor reach `ipcRenderer` behind them.
contextBridge.exposeInMainWorld("nyxWindow", nyxWindow);
contextBridge.exposeInMainWorld("nyxCore", nyxCore);
