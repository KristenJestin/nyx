/**
 * The TYPED, ALLOWLISTED contract between the Electron main process and the
 * renderer (exposed through the preload `contextBridge`). This is the ONLY surface
 * the renderer can touch — `contextIsolation: true` + `nodeIntegration: false` mean
 * the renderer has no `require`, no `ipcRenderer`, no Node globals; it sees only the
 * `window.nyxWindow` object the preload builds from this allowlist.
 *
 * Keeping the channel names in one shared module (imported by BOTH the main handlers
 * and the preload bridge) makes the allowlist a compile-time contract: a renderer
 * call to an unlisted channel is impossible because the preload never wires one, and
 * a main `ipcMain.handle` for an unlisted channel is a type error here.
 *
 * Phase 2 scope: only the FRAMELESS WINDOW CONTROLS (minimize / maximize-toggle /
 * close + the `NYX_WINDOW_CONTROLS` visibility probe the front already reads). The
 * full `nyxBridge` adapter (invoke/listen over the core-host IPC) is phase 3 (task
 * #10); it will extend this allowlist, not replace it.
 */

/**
 * The window-control channels. `Invoke` channels are request/response
 * (`ipcRenderer.invoke` ↔ `ipcMain.handle`); they return a value to the renderer.
 * Named with a stable `nyx:window/*` prefix so the allowlist is greppable and can
 * never collide with a future core-host channel (`nyx:core/*`).
 */
export const WINDOW_CHANNELS = {
  /** Minimize the main window. */
  minimize: "nyx:window/minimize",
  /** Toggle maximize ⇄ restore. Returns the resulting `isMaximized` state. */
  toggleMaximize: "nyx:window/toggle-maximize",
  /** Close the main window (honors the renderer-side close-warning flow upstream). */
  close: "nyx:window/close",
  /**
   * Resolve whether the custom window controls should render, from the OS env
   * `NYX_WINDOW_CONTROLS` (`"0"` hides; unset/any other value = visible). The
   * renderer cannot read `process.env`, so the main process is the only place the
   * raw (non-Vite-prefixed) value reaches it — mirrors the Tauri
   * `window_controls_visible` command the front already calls.
   */
  controlsVisible: "nyx:window/controls-visible",
  /**
   * Open the OS folder picker (directories only, single selection) and resolve the
   * chosen absolute path or `null` if cancelled. A MAIN-process concern (the
   * sandboxed renderer cannot open a native dialog), mirroring the Tauri
   * `plugin-dialog` `open({ directory:true })` the front uses for `pickDirectory`.
   */
  pickDirectory: "nyx:window/pick-directory",
  /**
   * Resolve the user's home directory (or `null`). A MAIN-process concern
   * (`app.getPath('home')`), mirroring the Tauri `@tauri-apps/api/path` `homeDir`.
   */
  homeDir: "nyx:window/home-dir",
} as const;

/** Union of every allowlisted window channel string. */
export type WindowChannel = (typeof WINDOW_CHANNELS)[keyof typeof WINDOW_CHANNELS];

/**
 * The CORE channels — the renderer↔core-host bridge (phase 3, task #8/#10). The
 * renderer never reaches the core-host directly: it speaks these fixed
 * `nyx:core/*` channels to MAIN, which relays to the host over the typed
 * host-protocol. Kept generic on purpose so the phase-3 PTY surface AND the phase-5
 * command/DB long tail ride the SAME allowlisted seam — the renderer's nyxBridge
 * adapter maps each logical command/event name onto these, it does not get a new
 * channel per backend command.
 */
export const CORE_CHANNELS = {
  /** Request/response: `invoke(name, args)` → a host request → its result. Covers
   *  every `pty_*` (and, phase 5, the command/DB long tail). `ipcRenderer.invoke`. */
  invoke: "nyx:core/invoke",
  /** Fire-and-forget flow-control ack (renderer→main→host). `ipcRenderer.send` — it
   *  must NOT round-trip a reply, so it never blocks the renderer's xterm.write path. */
  ptyAck: "nyx:core/pty-ack",
  /** Host→renderer EVENT push (`pty://output`, `pty://exit`, exec/cwd/changed). Main
   *  forwards every host event on this single channel; the renderer demuxes by the
   *  event name in the payload. `webContents.send` ↔ `ipcRenderer.on`. */
  event: "nyx:core/event",
} as const;

/** Union of every allowlisted core channel string. */
export type CoreChannel = (typeof CORE_CHANNELS)[keyof typeof CORE_CHANNELS];

/**
 * The logical EVENT a relayed host event maps to, in the contract's vocabulary
 * (`pty://output`, etc.). Main translates each `HostEventPayload` into one of these
 * `{ event, payload }` envelopes before pushing it on `CORE_CHANNELS.event`, so the
 * renderer's adapter demuxes by a stable name without importing the host protocol.
 */
export interface CoreEventEnvelope {
  /** The contract event channel name (mirrors `BackendEvent`). */
  event: string;
  /** The event payload, already in the contract's shape (binary as number[]). */
  payload: unknown;
}

/** The renderer-facing core bridge the preload installs on `window.nyxCore`. */
export interface NyxCoreApi {
  /** Invoke a backend command by name; resolves with its result or rejects with a
   *  readable error string. The adapter maps `pty_spawn`/`pty_write`/… onto this. */
  invoke(command: string, args?: Record<string, unknown>): Promise<unknown>;
  /** Fire-and-forget flow-control credit (after xterm.write). Never awaited. */
  ptyAck(ptyId: number, bytes: number): void;
  /** Subscribe to relayed host events; returns an unsubscribe. The handler gets the
   *  `{ event, payload }` envelope and filters by `event` name. */
  onEvent(handler: (envelope: CoreEventEnvelope) => void): () => void;
}

/**
 * The shape the preload exposes on `window.nyxWindow`. The renderer (and the phase-3
 * Electron `nyxBridge` adapter) program against THIS interface, never against
 * `ipcRenderer` directly.
 */
export interface NyxWindowApi {
  /** Minimize the window. */
  minimize(): Promise<void>;
  /** Toggle maximize ⇄ restore; resolves to the resulting maximized state. */
  toggleMaximize(): Promise<boolean>;
  /** Close the window. */
  close(): Promise<void>;
  /** Whether the custom controls should be shown (see `NYX_WINDOW_CONTROLS`). */
  controlsVisible(): Promise<boolean>;
  /** Open the OS folder picker; resolve the chosen path, or `null` if cancelled. */
  pickDirectory(title?: string): Promise<string | null>;
  /** Resolve the user's home directory, or `null`. */
  homeDir(): Promise<string | null>;
}

declare global {
  // The allowlisted bridge objects the preload installs. Typed here so the renderer
  // code (the phase-3 nyxBridge Electron adapter) gets full IntelliSense and the
  // preload stays in sync.
  interface Window {
    nyxWindow: NyxWindowApi;
    nyxCore: NyxCoreApi;
  }
}
